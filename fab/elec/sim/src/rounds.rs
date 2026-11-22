//! Rounds (ESIMRT S5): a live run as blocks that advance together in rounds of simulated time,
//! a stage at a time, each stage's blocks in parallel on helper cores.
//!
//! - A planner ([`Planner`]: S4's partition, or any other) cuts the whole circuit into blocks,
//!   each its own `Solver` with the global unknowns it solves ([`Block::own`]). `LiveSim` gives
//!   each a `Stepper` and runs them here. A planner that returns none keeps the whole circuit on
//!   one stepper, as before: that path is unchanged.
//! - A round ends on the round grid (`LiveSpec::round` samples from t = 0), or earlier at a timed
//!   command, the end of a `Step` or a firmware stop, never at a time the wall clock chose.
//!   Block 0, the control block, steps with the attached process on the live worker. The later
//!   stages run to its round end in order, or run earlier rounds on the helpers while it steps;
//!   a stage the plan marks unpipelined (`RoundPlan::unpipelined`) runs its round right after the
//!   stage before it, so what it exports back there is one round late.
//! - A stage's blocks are shared, heaviest first by measured cost ([`Share`]): the worker posts
//!   joiner jobs to the helpers ([`Helpers`]: the host's task pool), no more than the stage's
//!   cost over its largest block's, and each joiner and the worker take one block at a time until
//!   none is left. A block is taken through one atomic word; it waits in its cell and goes back
//!   there when run. A helper on a slow core then holds one block, not a share of the round, and
//!   the worker waits only for blocks a helper is running, so a pool that is busy or has no
//!   threads costs the parallelism, never progress.
//! - Every block's accepted points are recorded through the round and merged after it, in block
//!   order, into one stream of whole-circuit points, which the sinks, the trace and the interval
//!   statistics take as they take a single stepper's. At a point of one block the others' values
//!   are linear between their own points: what a view draws between points, never fed back.
//! - Couplings ([`Coupling`]): a value one block solves and others read through boundary sources.
//!   Each export keeps a window of accepted points covering the oldest round and lag still due.
//!   Before a block runs a round, each boundary source gets the points it can read (`Solver::feed`).
//!   A `Same` coupling reads this round's points from an earlier stage. A `Round` coupling reads
//!   one round length back, or further for a back coupling in the pipeline.
//! - Determinism: a block's round is a function of its state at the round's start and the tables
//!   it is fed, which are fixed before its stage runs. Who runs a block, the helper count and
//!   timing never change a value; `LiveSim::set_pack_shuffle` shares the blocks in a random order
//!   to prove it.

use crate::analysis::partition::Land;
use crate::analysis::tran::{Accepted, Restarts, Stepper};
use crate::circuit::SimCircuit;
use crate::dev::boundary::Table;
use crate::livesim::Taps;
use crate::newton::{SimDiag, Solver, Stats};
use crate::stamp::UnkKind;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

/// One block of a plan: its name, its stage, and the unknowns it solves as (local, global),
/// ground in neither. Stage 0 is block 0's alone: the control block, which holds every pad an
/// attached process reads or drives and every watch.
#[derive(Clone, Debug)]
pub struct Block {
    pub name: String,
    pub stage: u32,
    pub own: Vec<(u32, u32)>,
}

/// One block's work in a run in rounds (`LiveSim::block_work`).
#[derive(Clone, Debug)]
pub struct BlockWork {
    pub name: String,
    pub stage: u32,
    /// Its solver's unknowns, boundary branches included.
    pub unknowns: usize,
    /// Its solver's counts since the rounds began.
    pub stats: Stats,
    /// Its stepper's history restarts by cause, over its stepper's life.
    pub restarts: Restarts,
    /// Its smoothed wall a round, in ns. Block 0's cost is unmeasured, fixed at 1e3 ns per own unknown.
    pub cost_ns: f64,
    /// Wall in the first step of its rounds, in ns.
    pub first_ns: u64,
    /// First steps taken.
    pub first_n: u64,
    /// Wall in the steps after the first, in ns.
    pub later_ns: u64,
    /// Steps taken after the first.
    pub later_n: u64,
    /// Wall in its rounds on the worker, in ns.
    pub worker_ns: u64,
    /// Rounds it ran on the worker.
    pub worker_rounds: u64,
    /// Wall in its rounds on helpers, in ns.
    pub helper_ns: u64,
    /// Rounds it ran on helpers.
    pub helper_rounds: u64,
}

/// Where a run in rounds spent its wall time, in nanoseconds over the run.
#[derive(Clone, Debug, Default)]
pub struct RoundSplit {
    /// Wall in block 0's steps, including collecting its pin hits.
    pub ctl_ns: u64,
    /// Steps block 0 was asked to take.
    pub ctl_steps: u64,
    /// Wall checking, starting and installing plans at round starts.
    pub round_start_ns: u64,
    /// Wall exporting the blocks' points into their windows.
    pub export_ns: u64,
    /// Wall feeding the stages after block 0.
    pub feed_ns: u64,
    /// Wall ordering each stage's blocks and posting its joiners.
    pub pack_ns: u64,
    /// Wall running the blocks the worker takes of a stage run in its turn (`run_stage`).
    pub own_ns: u64,
    /// Wall running the blocks the worker takes of the stages posted behind block 0.
    pub claimed_ns: u64,
    /// Wall waiting for the blocks helpers are running and putting the blocks back.
    pub wait_ns: u64,
    /// The rest of the stages' wall, including checking failures.
    pub stage_rest_ns: u64,
    /// Wall in the worker's own blocks by stage, in run order after block 0.
    pub stage_own_ns: Vec<u64>,
    /// Wall waiting for helpers by stage, in the same order.
    pub stage_wait_ns: Vec<u64>,
    /// Total delay from posting a joiner until it takes its first block.
    pub wake_ns: u64,
    /// Longest delay from posting a joiner until it takes its first block.
    pub wake_max_ns: u64,
    /// Posted joiners that took a block.
    pub wake_n: u64,
    /// Wall running blocks on helpers, summed across helpers.
    pub helper_run_ns: u64,
    /// Wall finding and assembling the next merged point.
    pub next_point_ns: u64,
    /// Wall delivering merged points to the sink, trace and interval statistics.
    pub put_ns: u64,
    /// Wall recording the trace in `put` during an inline merge.
    pub put_trace_ns: u64,
    /// Wall updating interval statistics in `put` during an inline merge.
    pub put_interval_ns: u64,
    /// Wall closing the merge after its last point.
    pub end_merge_ns: u64,
    /// Wall opening rounds: the sinks' heard nets, block 0's feed, and the stages behind it fed,
    /// packed and posted.
    pub open_ns: u64,
    /// Of `open_ns`: asking the sinks what they hear, and the stages behind block 0 fed, packed
    /// and posted (`post_behind`), and of that the posts to the helpers alone.
    pub open_heard_ns: u64,
    pub open_behind_ns: u64,
    pub open_post_ns: u64,
    /// Wall posting merges to the helpers.
    pub post_merge_ns: u64,
    /// Wall collecting a posted merge, including running it when the worker claimed it.
    pub merge_wait_ns: u64,
    /// Wall running posted merges on either the helpers or the worker.
    pub merge_job_ns: u64,
    /// Posted merges completed.
    pub merge_jobs: u64,
    /// Posted merges claimed by the worker.
    pub merge_claimed: u64,
    /// Wall catching up the stages and merging their remaining points.
    pub flush_ns: u64,
    /// Times the stages were caught up and their points merged.
    pub flushes: u64,
    /// Points delivered by the merge.
    pub merged_points: u64,
    /// Merged points less than 5 ns after the preceding point in their round.
    pub close_points: u64,
    /// Close points that share an owning block with their predecessor.
    pub close_same_block: u64,
    /// Close points by the first owning block of the preceding and current points, in order.
    pub close_pairs: BTreeMap<(u32, u32), u64>,
    /// Close points in disjoint gap buckets with upper bounds of 1 ps, 100 ps and 5 ns.
    pub close_gap: [u64; 3],
    /// The shadow rounds (`Shadows`): a late-reading block's rounds run again with what it read
    /// late taken on time, those whose block failed to step it again, the wall taking
    /// their blocks as their rounds began (on the core that runs the block) and running them again
    /// (on the helpers).
    pub shadows: u64,
    pub shadow_failed: u64,
    pub shadow_take_ns: u64,
    /// The rounds whose late term at a hub port passed `HUB_SCREEN` (`Hub`), a port's round
    /// counted once.
    pub hub_onsets: u64,
    pub shadow_run_ns: u64,
    /// Per figure (`SHADOW_FIGURES`): its largest, where (the block and its round's end), and how
    /// many fell in each decade from 1e-6 (below it in the first, 1 and over in the last).
    pub shadow_worst: [f64; 5],
    pub shadow_at: [String; 5],
    pub shadow_hist: [[u64; 8]; 5],
    /// The lowest bar where a late-reading block ends is held to (`Rounds::shadow_bar`): past its
    /// own however shifted, the run refuses the cut the block reads late at (`end_shift`,
    /// `LiveSim::shadow_taken`).
    pub shadow_bar: f64,
    /// The largest "own end" figure judged, in its block's bar, and where: how near the run came to
    /// refusing a cut for its end. The figure judged is the unshifted one, or for a round within
    /// its bar once shifted to an own edge (`EndShift::Shifted`) the shifted one. The place says
    /// the unshifted figure and the shift beside it, and the verdict: what it refused for, or what
    /// held its shifts.
    pub shadow_nearest: f64,
    pub shadow_nearest_at: String,
    /// The shadow rounds that ended past their bar unshifted, by kind: refused (past it however
    /// shifted, no shift taken, or their own crossings apart), shifted within it and held by the
    /// reader's drift and smear, and shifted within it with those refusing.
    pub shadow_past: [u64; 3],
    /// The shadow rounds' ends as shifts (`end_shift`, Fable's A-next): rounds that end within
    /// their bar with a shift and still (none), past it but within it shifted (an edge apart, the
    /// run refuses nothing), and past it however shifted (it refuses the cut); and rounds whose
    /// own crossings disagree (it refuses the cut).
    pub shift_plain: u64,
    pub shift_still: u64,
    pub shift_found: u64,
    pub shift_over: u64,
    pub shift_parity: u64,
    /// The shifts by size in decades from 1e-12 s (below it in the first, 1e-5 s and over in the
    /// last), and per reader by its block's name their sums (`ShiftSums`: the shifts of its rounds
    /// with own crossings, over all its rounds' time).
    pub shift_hist: [u64; 8],
    pub shift_by: BTreeMap<String, ShiftSums>,
    /// The shadow rounds owed a checker (`Shadows`) and let go: a reader's sampling round replaced
    /// by its newer one, burst rounds stale (`CHAIN_STALE` rounds past their own) or thinned
    /// (every second one past `SHADOW_BURST` a reader), and those still owed when their plan ended.
    /// The run is never held up for its checks.
    pub shadow_replaced: u64,
    pub shadow_stale: u64,
    pub shadow_thinned: u64,
    pub shadow_lost: u64,
    /// The shadow rounds the worker ran itself, for a reader with no finished check for
    /// `SHADOW_FLOOR` sampling turns.
    pub shadow_floor: u64,
    /// The chain trees not owed and dropped with no room for them as they were ready.
    pub chain_skipped: u64,
    /// The check jobs the pool could start at the latest round start (`Checks::room`).
    pub checkers: u64,
    /// The chain shadow (`Shadows`, `RoundPlan::chains`): the imports a born jump split in, a round
    /// each, the trees run again and their members, the trees a member failed in, the wall taking
    /// their blocks (copies shared with a shadow round included) and running them, and the trees
    /// whose path a round-late read cut.
    pub chain_jumps: u64,
    pub chain_trees: u64,
    pub chain_members: u64,
    pub chain_failed: u64,
    pub chain_take_ns: u64,
    pub chain_run_ns: u64,
    /// Of them, the trees taken with a born export on its first jumps after its tree's plan
    /// (`CHAIN_FIRST`), and their wall taking and running them.
    pub chain_first: u64,
    pub chain_first_ns: u64,
    pub chain_cut_late: u64,
    /// Trees whose own held screen fired, and how often each named block fired it.
    pub chain_escalated: u64,
    pub chain_escalated_by: BTreeMap<String, u64>,
    /// Members of escalated trees with no born table to shift.
    pub chain_unswept: u64,
    /// Owed trees carried past their first posting attempt, and owed trees dropped stale.
    pub chain_owed_carried: u64,
    pub chain_owed_stale: u64,
    /// The members' own rounds in the run (their smoothed wall as they were taken), beside the
    /// wall running them again.
    pub chain_own_ns: u64,
    /// The trees the plans that went in kept from the plan before (their counts carried on) and
    /// began.
    pub chain_kept: u64,
    pub chain_began: u64,
    /// Per figure (`SHADOW_FIGURES`), over the members: its largest and where.
    pub chain_worst: [f64; 5],
    pub chain_at: [String; 5],
    /// The signed mean over a member's round of the difference on one of its own voltage nodes,
    /// the round as it ran less the round again (volts; Fable's chain-shadow answer: what the split
    /// leaves on a node, a switch's on-time bias), the largest in size and where.
    pub chain_signed: f64,
    pub chain_signed_at: String,
    /// The trees with a member past its screen at the round's end, followed through the later window
    /// (`Tree::run`, Fable's H1/H2 answer): those with a member's own end at the window's end past
    /// the bar (counted, never acting), those with a member's "own held" past it (held: convicted
    /// on this jump) and those with none (healed); the largest own held over the bar, and where,
    /// on which node, with that member's own end at the window's end over the bar.
    pub chain_followed: u64,
    pub chain_later_over: u64,
    pub chain_held_over: u64,
    pub chain_healed: u64,
    /// The chain shadow's act (`Rounds::chain_act`, Fable's H1/H2 answer, Q3): the born exports a
    /// held tree of one convicted (its chain lands), the exports a held union marked (those whose
    /// chain holds a member held past the bar: each one's next jumps are taken alone until it is
    /// convicted or heals on `CHAIN_FIRST` in a row), and the marks healed so.
    pub chain_convicted: u64,
    pub chain_marked: u64,
    pub chain_unmarked: u64,
    pub chain_held_nearest: f64,
    pub chain_held_nearest_at: String,
    /// The largest "own end" of a member in its tree's bar (`Rounds::chain_bar`), and where; the
    /// trees past it (count only).
    pub chain_nearest: f64,
    pub chain_nearest_at: String,
    pub chain_over: u64,
}

/// A value a block exported in rounds that is not a number or is past what the circuit can hold
/// (`Rounds::hold`): the first such, its block, name, time and value, and where that block's
/// round began (the run goes back to a snapshot at or before it).
#[derive(Clone, Debug)]
pub(crate) struct Breach {
    pub(crate) block: usize,
    /// The coupling whose value broke, exported by `block`.
    pub(crate) coupling: u32,
    pub(crate) name: String,
    pub(crate) t: f64,
    pub(crate) v: f64,
    pub(crate) from: f64,
}

/// When the blocks that read a coupling read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lag {
    /// In the same round: the exporter is in an earlier stage.
    Same,
    /// One round length late, or more for a back coupling in the pipeline (`RoundPlan::lag_rounds`).
    Round,
}

/// A stage's place in the pipeline: its stage less the stages at or below it that run right after
/// the stage before them (`RoundPlan::unpipelined`). Block 0 is alone in place 0.
pub(crate) fn slot_of(stage: u32, unpipelined: &[u32]) -> u32 {
    stage - unpipelined.iter().filter(|&&s| s <= stage).count() as u32
}

/// How many rounds late a reader in pipeline place `to` reads a lagged coupling from place `from`
/// (`RoundPlan::slot`; without unpipelined stages a place is its stage).
pub(crate) fn lag_rounds(from: u32, to: u32, pipeline: bool) -> u64 {
    if pipeline { u64::from(from.saturating_sub(to)) + 1 } else { 1 }
}

/// Where block 0 finished round `r`. Round zero ends at the plan's starting time.
fn round_end(ends: &VecDeque<(u64, f64)>, start: f64, r: u64) -> f64 {
    if r == 0 {
        return start;
    }
    ends.iter().rev().find(|e| e.0 == r).map(|e| e.1).expect("a late read's round is kept")
}

/// A value one block solves and others read: the exporter and its local unknown (a node voltage,
/// or a boundary source's branch current), the boundary sources that read it by block and
/// instance name, when they read it, and per reader the cut it reads it across (`cuts`: reader
/// and cut, in `to`'s order; a reader across none, an exact edge, is not listed), with the
/// largest of those cuts' rho. Readers of one value can each take it across a cut of their own:
/// the metal bank's oscillators each read its summing nodes across the cut at their own output.
#[derive(Clone, Debug)]
pub struct Coupling {
    pub from: (u32, u32),
    pub to: Vec<(u32, String)>,
    pub lag: Lag,
    pub cuts: Vec<(u32, u32)>,
    pub rho: f64,
}

impl Coupling {
    /// The cut reader block `k` reads it across, if any. O(readers).
    pub fn cut_of(&self, k: u32) -> Option<u32> {
        self.cuts.iter().find(|c| c.0 == k).map(|c| c.1)
    }
}

/// How far apart a step may put a block's own crossings, as a share of their spacing, when it
/// steps on its own bound: an oscillator in it drifts flat with the square of that share
/// (measured on the tone box: about 0.6 cents at a thirteenth, 0.16 at a 27th, at 48 and 96 kHz
/// alike), and at a fiftieth it keeps its pitch as stepping on the sample period does. Crossings
/// tens of milliseconds apart (a key's press and release) leave it its longest bound.
pub(crate) const CROSSING_SHARE: f64 = 0.02;

/// A block's own step bound in Fast (`LiveSpec::own_bound`): the sample period, and the longest
/// bound: the most sample periods, a power of two dividing the round, within Fast's
/// resolution and the bandwidth step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OwnBound {
    pub(crate) period: f64,
    pub(crate) longest: f64,
}

impl OwnBound {
    /// None when no bound longer than the period fits.
    pub(crate) fn new(period: f64, within: f64, round_samples: u32) -> Option<OwnBound> {
        let mut k = 1u32;
        while round_samples % (2 * k) == 0 && f64::from(2 * k) * period <= within {
            k *= 2;
        }
        (k > 1).then(|| OwnBound { period, longest: f64::from(k) * period })
    }
}

/// A plan: its blocks in stage order, block 0 the control block, and their couplings. Their
/// solvers come beside it, in block order.
#[derive(Clone, Debug, Default)]
pub struct RoundPlan {
    pub blocks: Vec<Block>,
    pub couplings: Vec<Coupling>,
    /// Global unknowns several blocks each solve a share of, (global, [(block, local)]): the
    /// branch current of a rail source copied into every block that uses it. The merge shows
    /// their sum. A `Block::own` names one of them (any), as every global unknown is owned once.
    pub sums: Vec<(u32, Vec<(u32, u32)>)>,
    /// Per element cut in two: each (block, name) of its copy (`<name>#ghost`) on the cut's far
    /// side. An edit of the element is an edit of every copy, at the same instant.
    pub ghosts: BTreeMap<String, Vec<(u32, String)>>,
    /// The switches whose round map passed in both their positions (`plan_blocks`): an edit that
    /// moves only their controls keeps the proof.
    pub switches: Vec<String>,
    /// What the plan's round map keeps to prove it again after an edit (`plan_blocks`); None from
    /// a planner that proves nothing, whose edits on a loop leave rounds at once.
    pub proof: Option<Arc<crate::analysis::round::RoundProof>>,
    /// Per coupling, what the plan promises its late read gets wrong at most, as its proof holds
    /// it (`PartOpts::hub_tol` for a return in the hub form, else `cut_tol`); empty from a planner
    /// that promises nothing, whose shadow rounds only count (`Rounds::shadow_bar`).
    pub late_tol: Vec<f64>,
    /// The plan's bar for a return in the hub form (`PartOpts::hub_tol`), as its proof holds it: no
    /// reader's shadow bar is looser (`Rounds::shadow_bar`). None from a planner that promises
    /// nothing.
    pub hub_tol: Option<f64>,
    /// The stages that run in their round right after the stage before them, not behind it in the
    /// pipeline (`plan_blocks`: a cut read as late as the pipeline reads it fails, and passes read
    /// one round late). Each takes the place in the pipeline of the stage before (`slot`), so what
    /// a later stage of that place exports back to an earlier one is one round late. Ascending,
    /// each after stage 1.
    pub unpipelined: Vec<u32>,
    /// The nodes its cuts are at, by name (`plan_blocks`): in force, the next plan keeps them
    /// inside the keep band (`Planner::plan_from`).
    pub cuts: Vec<String>,
    /// Per cut a coupling names (`Coupling::cut`), its node (`plan_blocks`): what a run refuses
    /// when a value read across it leaves what the circuit can hold (`Rounds::blame`).
    pub cut_nodes: BTreeMap<u32, String>,
    /// The imports that land, as (coupling, reader block), ascending (`plan_blocks`, by the plan-time
    /// test of each import, A-K1): their jumps reach the reader as they are, never split onto its
    /// grid (`Table::split`), and stay instants it lands on when it is on its own bound. Empty from a
    /// planner that tests none: every import split.
    pub lands: Vec<(u32, u32)>,
    /// The chain shadow's trees (`plan_blocks`, `Chain`): one per export a jump is born on that
    /// some reader splits. Empty from a planner that tests none.
    pub chains: Vec<Chain>,
    /// The holder's ports (`HolderPort`, `plan_blocks`): one per return in the hub form, in its
    /// consumer's block. Empty from a planner that makes none.
    pub ports: Vec<HolderPort>,
}

/// A consumer's stand-in for the block that holds a node it returns its current into
/// (`BoundaryKind::Port`, Fable's Q3 answer): fed its own return as the holder reads it, the same
/// table at the same delay (`Rounds::feed`), so the node moves at once by what the holder will do a
/// round later. Its impedance is the holder's driving-point impedance at the node, fitted over the
/// map's points (`r + rb / (1 + s tau) + s l`); all zero where none fits, and the port holds the
/// node as the holder exported it.
///
/// It stands on the consumer's side only, for its own draw. The holder still reads every
/// consumer's draw a round late, so what reads the node inside the holder's block hears its
/// consumers as late as before, and one consumer hears another's draw a round late through the
/// node (directly, or through a third block that reads the node in the same round).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HolderPort {
    /// The consumer's block and the port's boundary there (`<node>#port<k>`, `k` the holder).
    pub block: u32,
    pub name: String,
    /// The return's coupling, read by the holder alone.
    pub coupling: u32,
    pub r: f64,
    pub rb: f64,
    pub tau: f64,
    pub l: f64,
}

/// An export a jump is born on that some reader splits, and the tree its jumps reach (the relay
/// rule's paths, `charge_hold`): what the chain shadow runs again with the export landed in every
/// reader that splits it, each member fed the shadow's export of the one before (Fable's relay
/// answer, Q2; one tree per export, its chain-shadow answer, Q2). The transfers only pick the
/// members; a path step read a round late ends the tree there.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chain {
    /// The coupling the jumps are born on, and its export's name (`Land::born`).
    pub born: u32,
    pub name: String,
    /// The blocks that read it split (the tree's roots), ascending: the order they are fed in, the
    /// first the export's head (`Rounds::chain_round`).
    pub roots: Vec<u32>,
    /// The tree's pairs (coupling, reader block), by the reader's stage, then reader and coupling.
    pub pairs: Vec<(u32, u32)>,
    /// Each pair's coupling label, in `pairs` order. A held reader names its targets from these.
    pub labels: Vec<String>,
    /// Its blocks in stage order (the order they are fed in).
    pub members: Vec<u32>,
    /// The path steps out of the tree read a round late: where it was cut.
    pub cut_late: u32,
}

impl RoundPlan {
    /// A stage's place in the pipeline (`slot_of` with its `unpipelined`).
    pub fn slot(&self, stage: u32) -> u32 {
        slot_of(stage, &self.unpipelined)
    }

    /// How many rounds late block `to` reads a lagged coupling from block `from`: `lag_rounds` on
    /// their places in the pipeline (`slot`).
    pub(crate) fn lag_rounds(&self, from: usize, to: usize, pipeline: bool) -> u64 {
        lag_rounds(self.slot(self.blocks[from].stage), self.slot(self.blocks[to].stage), pipeline)
    }

    /// Whether `other` cuts the circuit as this one does: the same blocks in order, at the same
    /// stages, solving the same unknowns, the same couplings (their rho aside), the same sums, the
    /// same ghosts and the same stages unpipelined.
    pub fn same_cut(&self, other: &RoundPlan) -> bool {
        self.blocks.len() == other.blocks.len()
            && self.blocks.iter().zip(&other.blocks).all(|(a, b)| a.name == b.name && a.stage == b.stage && a.own == b.own)
            && self.couplings.len() == other.couplings.len()
            && self.couplings.iter().zip(&other.couplings).all(|(a, b)| a.from == b.from && a.to == b.to && a.lag == b.lag && a.cuts == b.cuts)
            && self.sums == other.sums
            && self.ghosts == other.ghosts
            && self.unpipelined == other.unpipelined
    }

    /// Sets a device's parameter in block `k`'s solver `s`: the device where the block has it, and
    /// its ghost where the block holds its copy (`ghosts`). True when either took.
    pub fn set_param(&self, k: usize, s: &mut Solver, device: &str, key: &str, value: f64) -> bool {
        let mut took = s.set_param(device, key, value);
        for (_, ghost) in self.ghosts.get(device).into_iter().flatten().filter(|g| g.0 as usize == k) {
            took |= s.set_param(ghost, key, value);
        }
        took
    }
}

/// A warm the run keeps while it wants the plan made ahead (`Planner::warm`): let go of before it
/// is back, its job stops at its next unit.
pub struct Warm {
    done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Warm {
    /// The job sets `done` with `Release` when it finishes and checks `stop` between units.
    pub fn new(done: Arc<AtomicBool>, stop: Arc<AtomicBool>) -> Warm {
        Warm { done, stop }
    }

    /// Whether the job has finished.
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

impl Drop for Warm {
    fn drop(&mut self) {
        if !self.done() {
            self.stop.store(true, Ordering::Release);
        }
    }
}

/// What cuts a run into blocks.
pub trait Planner: Send + PlannerClone {
    /// The plan for the run's circuit as lowered (`ckt`) at the state `whole` holds at time `t`
    /// (its `x` and device states, every edit made), with each block's solver seeded from it.
    /// None runs the whole circuit on one stepper.
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag>;

    /// The plan as `plan` makes it, knowing the plan in force (None on one stepper): a planner with
    /// a keep rule keeps that plan's cuts while they stay inside its band. By default `plan`.
    fn plan_from(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64, in_force: Option<&RoundPlan>) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let _ = in_force;
        self.plan(ckt, whole, t)
    }

    /// Never cut at `node` again: a value read late across it left what the circuit can hold
    /// (`LiveSim`'s growth rule). By default nothing; the run then declines a plan that reads
    /// late there (`Rounds::reads_late_at`).
    fn refuse_cut(&mut self, node: &str) {
        let _ = node;
    }

    /// From the next plan on, land the born pairs and the same-round paths to the coupling a
    /// held member read. Splitting the born jump left that member past its bar. By default nothing.
    fn land_chain(&mut self, land: &Land) {
        let _ = land;
    }

    /// Makes ahead, off the caller's thread, what a plan of the board as it is now pays first
    /// (`BlockPlanner`: its plan at `t` in the key's states, on its pool: `round::warm_plan`), so
    /// rounds asked for later find it made. A run that keeps its planner aside warms when its
    /// first plan would be due. The run keeps the handle while it wants the warm. Dropping it
    /// before the job finishes stops the job at its next unit.
    /// Returns `None` when no job is posted, as by default.
    fn warm(&self, ckt: &Arc<SimCircuit>, whole: &Solver, t: f64) -> Option<Warm> {
        let _ = (ckt, whole, t);
        None
    }

    /// Whether a start at `whole` can begin on a cut carried from a plan of the same circuit
    /// (`carried`, at its part values or others) instead of waiting for its own plan: a cut is
    /// kept for its circuit. By default no.
    fn can_carry(&self, ckt: &SimCircuit, whole: &Solver) -> bool {
        let _ = (ckt, whole);
        false
    }

    /// That carried cut, built on `ckt` as `whole` holds it at `t` and proved there
    /// (`round::build_carried`), with the chains it lands by name: from `from`, the carry a run
    /// made again holds on its timeline (`LiveSim::follow`), else the one kept for the circuit.
    /// None with none to carry, a refusal saying why it was not taken. By default none.
    fn carried(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64, from: Option<&Arc<crate::analysis::round::Carry>>) -> Option<Result<CarriedPlan, CarryRefused>> {
        let _ = (ckt, whole, t, from);
        None
    }
}

/// A start's plan on the cut carried for its circuit (`Planner::carried`): the plan and its blocks'
/// solvers, the chains its source's planner had landed by name, which the run learns with it (a
/// landing is the safe side at any part values; refusals are not carried), and the carry it was
/// built from, which the run's timeline holds (`LiveSim::carried_cut`).
pub struct CarriedPlan {
    pub plan: RoundPlan,
    pub solvers: Vec<Solver>,
    pub landed: Vec<Land>,
    pub carry: Arc<crate::analysis::round::Carry>,
}

/// A carried cut not taken (`Planner::carried`): the carry tried, which the run's timeline holds as
/// it does one taken, and why.
pub struct CarryRefused {
    pub carry: Arc<crate::analysis::round::Carry>,
    pub why: String,
}

/// A planner's copy behind its box: a snapshot of a run keeps its planner's state, and a plan on
/// its way is made by a fork of it. Every planner is `Clone` (derive it).
pub trait PlannerClone {
    fn clone_box(&self) -> Box<dyn Planner>;
}

impl<T: Planner + Clone + 'static> PlannerClone for T {
    fn clone_box(&self) -> Box<dyn Planner> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn Planner> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Posts a job to a helper core: the host's task pool (`Spawn::job`), never a thread of its own.
pub type Post = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

/// Where a run in rounds puts its packs, and how many helpers it may keep busy beside the worker.
#[derive(Clone)]
pub struct Helpers {
    pub post: Post,
    pub count: usize,
}

/// Where a run in rounds checks itself (`LiveSim::set_checks`): its shadow rounds and chain trees
/// go to `helpers` (the host's background jobs, below the run's packs), or, under a host's budget,
/// ahead of its plan work there (`ahead`, `LiveSim::set_checks_ahead`). The checks ready at a
/// round start are owed in the run's own lists (`Shadows`) and handed over as the pool can start
/// them (`room`), split over that many jobs and run back to back (`Rounds::checks_post`). With no
/// budget the room is `helpers.count` less, when `beside_packs` (the checks and the packs share
/// one pool), the packs and merge the round before posted at once, less the check jobs posted and
/// not yet ended. The count is shared by every plan the run goes through.
#[derive(Clone)]
pub struct Checks {
    pub helpers: Helpers,
    pub beside_packs: bool,
    pub ahead: Option<CheckPost>,
    out: Arc<AtomicUsize>,
}

/// A host's budget for a run's checks (`Checks::ahead`): `post` queues a check job ahead of every
/// plan's, and `room` is how many it can start now (its free slots, or one to wait when none of the
/// checks' waits). O(1) each.
#[derive(Clone)]
pub struct CheckPost {
    pub post: Post,
    pub room: Arc<dyn Fn() -> usize + Send + Sync>,
}

impl Checks {
    pub fn new(helpers: Helpers, beside_packs: bool) -> Checks {
        Checks { helpers, beside_packs, ahead: None, out: Arc::default() }
    }

    /// The check jobs the pool can start now beside `packs` posted at once. O(1).
    fn room(&self, packs: usize) -> usize {
        match &self.ahead {
            Some(a) => (a.room)(),
            None => self.helpers.count.saturating_sub(if self.beside_packs { packs } else { 0 }).saturating_sub(self.out.load(Ordering::Acquire)),
        }
    }

    /// One check job posted, counted until it ends or is dropped. O(1).
    fn post(&self, job: Box<dyn FnOnce() + Send>) {
        struct Out(Arc<AtomicUsize>);
        impl Drop for Out {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::AcqRel);
            }
        }
        self.out.fetch_add(1, Ordering::AcqRel);
        let out = Out(self.out.clone());
        let job: Box<dyn FnOnce() + Send> = Box::new(move || {
            let _out = out;
            job();
        });
        match &self.ahead {
            Some(a) => (a.post)(job),
            None => (self.helpers.post)(job),
        }
    }
}

impl std::fmt::Debug for Helpers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Helpers({})", self.count)
    }
}

/// A unit of work `fan_out` shares out.
pub type Unit<T> = Box<dyn FnOnce() -> T + Send>;

/// Why `fan_out` returned no results: a unit panicked, or the job was stopped (`with_stop`).
#[derive(Debug)]
pub enum Fanned {
    Panicked,
    Stopped,
}

impl Fanned {
    /// The diagnostic for `what` (its words before " panicked" in a message).
    pub fn diag(self, what: &str) -> SimDiag {
        match self {
            Self::Panicked => SimDiag::setup(format!("{what} panicked")),
            Self::Stopped => SimDiag::setup(format!("{what}: stopped, the run left it")),
        }
    }
}

thread_local! {
    static CURRENT_STOP: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static CURRENT_BEHIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with what it posts marked as behind work or not (`behind`): work nothing waits for, a
/// plan behind a carried start that already plays. `fan_out` keeps the mark for its units' turns on
/// other cores, so a host's pool can tell every unit of such a plan from awaited work.
pub fn with_behind<R>(behind: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_BEHIND.set(self.0);
        }
    }
    let _restore = Restore(CURRENT_BEHIND.replace(behind));
    f()
}

/// Whether the job on this thread is behind work (`with_behind`). O(1).
pub fn behind() -> bool {
    CURRENT_BEHIND.get()
}

/// Runs `f` with the job's stop flag in place for its calls to `fan_out`.
pub fn with_stop<R>(stop: Arc<AtomicBool>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_STOP.set(self.0.take());
        }
    }
    let _restore = Restore(CURRENT_STOP.replace(Some(stop)));
    f()
}

/// Whether the run has stopped the job on this thread.
pub fn stopped() -> bool {
    CURRENT_STOP.with_borrow(|stop| stop.as_ref().is_some_and(|s| s.load(Ordering::Acquire)))
}

/// What `fan_out`'s units share: each taken once, by the next index.
struct Fan<T> {
    units: Vec<Mutex<Option<Unit<T>>>>,
    next: std::sync::atomic::AtomicUsize,
    stop: Option<Arc<AtomicBool>>,
    /// Whether its units are behind work (`with_behind`), for their turns on other cores.
    behind: bool,
    tx: mpsc::Sender<(usize, Result<T, Fanned>)>,
}

impl<T: Send + 'static> Fan<T> {
    /// Takes the next unit and reports its result or that it was stopped. False when none is left.
    fn run_next(&self) -> bool {
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        if i >= self.units.len() {
            return false;
        }
        let unit = self.units[i].lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(unit) = unit {
            let got = if self.stop.as_ref().is_some_and(|s| s.load(Ordering::Acquire)) {
                drop(unit);
                Err(Fanned::Stopped)
            } else {
                catch_unwind(AssertUnwindSafe(unit)).map_err(|_| Fanned::Panicked)
            };
            let _ = self.tx.send((i, got));
        }
        true
    }

    /// A core's turn: one unit, then the next turn posted while units are left, so the pool takes
    /// its other work (a round's packs) between units.
    fn turn(fan: Arc<Fan<T>>, post: Post) {
        with_behind(fan.behind, || {
            if fan.run_next() && fan.next.load(Ordering::Relaxed) < fan.units.len() {
                let again = post.clone();
                post(Box::new(move || Fan::turn(fan, again)));
            }
        })
    }
}

/// `units` run on the calling thread and on up to `pool.count` of the pool's cores at once, their
/// results in their order. Each core runs a unit at a time and posts its next turn after it; the
/// caller takes units as the cores do and then waits only for those a core is running, so a busy
/// pool or none costs the parallelism, never progress. A unit that panics reports `Fanned::Panicked`.
/// Units taken after the job is stopped end unrun and report `Fanned::Stopped`. The results never
/// depend on which core ran what.
pub fn fan_out<T: Send + 'static>(units: Vec<Unit<T>>, pool: Option<&Helpers>) -> Result<Vec<T>, Fanned> {
    let n = units.len();
    let (tx, rx) = mpsc::channel();
    let stop = CURRENT_STOP.with_borrow(Clone::clone);
    let fan = Arc::new(Fan { units: units.into_iter().map(|u| Mutex::new(Some(u))).collect(), next: Default::default(), stop, behind: behind(), tx });
    if let Some(h) = pool {
        for _ in 0..h.count.min(n.saturating_sub(1)) {
            let (f, post) = (fan.clone(), h.post.clone());
            (h.post)(Box::new(move || Fan::turn(f, post)));
        }
    }
    while fan.run_next() {}
    drop(fan);
    let mut out: Vec<Option<T>> = (0..n).map(|_| None).collect();
    for _ in 0..n {
        let (i, got) = rx.recv().map_err(|_| Fanned::Panicked)?;
        out[i] = Some(got?);
    }
    out.into_iter().collect::<Option<_>>().ok_or(Fanned::Panicked)
}

/// Whether two blocks' solvers are the same block: the same unknowns and the same instances (by
/// kind and name, in order). O(unknowns + instances).
/// What makes a block the same block (`Rounds::runs_cut`, `Rounds::same_blocks`): its solver's
/// unknowns and its instances, by name and in order, each bank with its kind. Kept beside the
/// block, so it is read while the block is on a helper.
#[derive(Clone)]
struct BlockSig {
    unknowns: Vec<String>,
    banks: Vec<(&'static str, Vec<String>)>,
}

impl BlockSig {
    fn of(s: &Solver) -> BlockSig {
        BlockSig {
            unknowns: s.unknowns.iter().map(|u| u.name.clone()).collect(),
            banks: s.banks.iter().map(|b| (b.kind(), (0..b.len()).map(|i| b.inst_name(i).to_string()).collect())).collect(),
        }
    }

    /// Whether `s` has these unknowns and instances. O(unknowns + instances).
    fn matches(&self, s: &Solver) -> bool {
        self.unknowns.len() == s.unknowns.len()
            && self.unknowns.iter().zip(&s.unknowns).all(|(u, v)| *u == v.name)
            && self.banks.len() == s.banks.len()
            && self.banks.iter().zip(&s.banks).all(|((kind, names), q)| *kind == q.kind() && names.len() == q.len() && names.iter().enumerate().all(|(i, n)| n == q.inst_name(i)))
    }
}

/// `s`'s unknowns and device states from `whole`, matched by name; returns the map (local,
/// global) of the unknowns both have. O(unknowns + instances).
pub fn seed_from(s: &mut Solver, whole: &Solver) -> Vec<(u32, u32)> {
    let mut own = Vec::new();
    for l in 1..s.unknowns.len() {
        if let Some(g) = whole.unknown(&s.unknowns[l].name) {
            s.x[l] = whole.x[g as usize];
            own.push((l as u32, g));
        }
    }
    copy_states(whole, s);
    own
}

/// Each instance's device state from `from` into the instance of the same name in `to`.
fn copy_states(from: &Solver, to: &mut Solver) {
    for bank in &from.banks {
        for i in 0..bank.len() {
            if let Some((b, k)) = to.find_device(bank.inst_name(i)) {
                let state = bank.state(i);
                if !state.is_empty() {
                    to.banks[b].set_state(k, &state);
                }
            }
        }
    }
}

/// A plan the rounds can run: solvers beside its blocks, block 0 alone at stage 0 and the stages
/// ascending, every global unknown solved by exactly one block, the stages unpipelined its own.
pub(crate) fn check(plan: &RoundPlan, solvers: &[Solver], whole: &Solver) -> Result<(), SimDiag> {
    let fail = |why: String| Err(SimDiag::setup(format!("round plan: {why}")));
    if plan.blocks.is_empty() || plan.blocks.len() != solvers.len() {
        return fail(format!("{} blocks, {} solvers", plan.blocks.len(), solvers.len()));
    }
    let mut owner = vec![u32::MAX; whole.unknowns.len()];
    for (k, (b, s)) in plan.blocks.iter().zip(solvers).enumerate() {
        if (k == 0) != (b.stage == 0) || (k > 0 && b.stage < plan.blocks[k - 1].stage) {
            return fail(format!("block {k} ({}) at stage {}: block 0 alone is stage 0, the stages ascend", b.name, b.stage));
        }
        for &(l, g) in &b.own {
            if l == 0 || l as usize >= s.unknowns.len() || g == 0 || g as usize >= owner.len() {
                return fail(format!("block {k} ({}) maps {l} to {g}", b.name));
            }
            if owner[g as usize] != u32::MAX {
                return fail(format!("{} is solved by blocks {} and {k}", whole.unknowns[g as usize].name, owner[g as usize]));
            }
            owner[g as usize] = k as u32;
        }
    }
    if let Some(g) = (1..owner.len()).find(|&g| owner[g] == u32::MAX) {
        return fail(format!("no block solves {}", whole.unknowns[g].name));
    }
    if plan.unpipelined.windows(2).any(|w| w[0] >= w[1]) || plan.unpipelined.iter().any(|&s| s < 2 || !plan.blocks.iter().any(|b| b.stage == s)) {
        return fail(format!("unpipelined stages {:?}: each a stage of the plan after stage 1, ascending", plan.unpipelined));
    }
    for (g, shares) in &plan.sums {
        for &(b, l) in shares {
            if *g as usize >= whole.unknowns.len() || b as usize >= solvers.len() || l == 0 || l as usize >= solvers[b as usize].unknowns.len() {
                return fail(format!("a share of unknown {g} is unknown {l} of block {b}"));
            }
        }
    }
    for (device, copies) in &plan.ghosts {
        if let Some((b, ghost)) = copies.iter().find(|(b, g)| solvers.get(*b as usize).is_none_or(|s| s.find_device(g).is_none())) {
            return fail(format!("{device}'s copy {ghost} is not in block {b}"));
        }
    }
    for (c, k) in plan.couplings.iter().enumerate() {
        let (b, l) = (k.from.0 as usize, k.from.1 as usize);
        if b >= solvers.len() || l == 0 || l >= solvers[b].unknowns.len() {
            return fail(format!("coupling {c} exports unknown {l} of block {b}"));
        }
        for (to, name) in &k.to {
            let to = *to as usize;
            if to >= solvers.len() || to == b || solvers[to].find_device(name).is_none() {
                return fail(format!("coupling {c} feeds {name} in block {to}"));
            }
            if k.lag == Lag::Same && plan.blocks[to].stage <= plan.blocks[b].stage {
                return fail(format!("coupling {c} is read in the same round by block {to}, a stage not after its exporter's"));
            }
        }
    }
    Ok(())
}

/// What running rounds hand the rounds of a plan that cuts the run anew (`Rounds::into_parts`,
/// `Rounds::new`): the round count with block 0's recent round ends, so late reads go on cutting
/// where they did, and each coupling's history by its exporter (block, local unknown), for the
/// couplings of the blocks the plan keeps; and per chain tree its born export by its exporter, its
/// roots and members, and its counts (`Shadows::seen`), which a tree the plan keeps carries on.
pub(crate) struct Carry {
    ctl_done: u64,
    ends: VecDeque<(u64, f64)>,
    start: f64,
    windows: Vec<((u32, u32), Window)>,
    chains: Vec<((u32, u32), Vec<u32>, Vec<u32>, ChainJumps)>,
}

/// Sets holder port `p`'s impedance in its block's solver `s` where the solver has it otherwise
/// (`HolderPort`); true when it changed.
fn port_into(s: &mut Solver, p: &HolderPort) -> bool {
    let Some((bank, i)) = s.find_device(&p.name) else { return false };
    let z = [("r", p.r), ("rb", p.rb), ("tau", p.tau), ("l", p.l)];
    if z.iter().all(|&(key, v)| s.banks[bank].param(i, key) == Some(v)) {
        return false;
    }
    for (key, v) in z {
        s.set_param(&p.name, key, v);
    }
    true
}

/// What a read of a coupling's window depends on beyond the reader (`Read::table`): the pipeline,
/// the round's length, block 0's recorded ends of round and the rounds' start.
struct Read<'a> {
    pipeline: bool,
    round: f64,
    ends: &'a VecDeque<(u64, f64)>,
    start: f64,
}

impl Read<'_> {
    /// What a reader of window `w` takes for round `m` from `t0` to `end` at `delay`: its points
    /// (through the exporter's end of the round read, in the pipeline), on the reader's `grid` its
    /// jumps split unless the read `lands`, and with `crossings_as_data` the exporter's crossings
    /// plain points (`Rounds::feed`); and how many jumps it split. A holder's port takes its
    /// holder's table this way, so the two read the same. O(the points read).
    #[allow(clippy::too_many_arguments)]
    fn table(&self, w: &Window, t0: f64, end: f64, m: u64, delay: f64, grid: f64, lands: bool, crossings_as_data: bool) -> (Table, u32) {
        let mut points = if self.pipeline {
            // In the pipeline an exporter may have run further: a read takes its points through its
            // own end of the round read, this round's for a `Same` read, and for a read `l` rounds
            // late block 0's recorded end of round m - l (which also covers shortened rounds and
            // rounding).
            let lag = (delay / self.round).round() as u64;
            let cut = if lag == 0 { end } else { round_end(self.ends, self.start, m.saturating_sub(lag)) };
            w.table_to(t0 - delay - grid, cut)
        } else {
            w.table(t0 - delay - grid)
        };
        let mut jumps = 0;
        if let Some((fed, n)) = points.split(grid, delay, t0, None, end, end).filter(|_| !lands) {
            jumps = n;
            points = fed;
        }
        if crossings_as_data && !lands && !points.edges.is_empty() {
            let edges = std::mem::take(&mut points.edges);
            points.events.retain(|i| edges.binary_search(i).is_err());
        }
        (points, jumps)
    }
}

/// An export's accepted points by time: from the last at or before what a read can still ask for
/// to the newest.
#[derive(Clone, Default)]
struct Window {
    t: Vec<f64>,
    v: Vec<f64>,
    events: Vec<u32>,
    /// The events that are crossings of the exporting block.
    edges: Vec<u32>,
    /// Points marked as events by the point after them (`mark_newest`), ascending.
    jumps: Vec<u32>,
    /// Edge points whose jump ran over a source edge shorter than the resolution, and its end
    /// (`Accepted::edge_end`), where the export does not relay it (`pass_on`): a read that lands
    /// the jump takes it (`landed_to`). Ascending.
    ends: Vec<(u32, f64)>,
    /// The latest time it was trimmed at (`trim`): no read may start before it.
    trimmed: Option<f64>,
}

impl Window {
    /// The newest point is an event (where a value may jump), marked by the next point: a read
    /// whose points end at it never sees the mark (`table_range`).
    fn mark_newest(&mut self) {
        let n = self.t.len() as u32;
        if n > 0 && self.jumps.last() != Some(&(n - 1)) {
            self.jumps.push(n - 1);
        }
    }

    fn push(&mut self, t: f64, v: f64, event: bool, edge: bool) {
        if event {
            self.events.push(self.t.len() as u32);
        }
        if event && edge {
            self.edges.push(self.t.len() as u32);
        }
        self.t.push(t);
        self.v.push(v);
    }

    /// The first point a read at `t` or later needs: the last at or before `t`.
    fn first(&self, t: f64) -> usize {
        self.t.partition_point(|&at| at <= t).saturating_sub(1)
    }

    /// `first` for a read from `t`, which the trim must have left whole.
    fn read_from(&self, t: f64) -> usize {
        debug_assert!(self.trimmed.is_none_or(|k| t >= k - 1e-9), "a read from {t} starts before the window's trim at {:?}", self.trimmed);
        self.first(t)
    }

    /// Drops what no read at `t` or later needs.
    fn trim(&mut self, t: f64) {
        self.trimmed = Some(self.trimmed.map_or(t, |k| k.max(t)));
        let k = self.first(t);
        if k > 0 {
            self.t.drain(..k);
            self.v.drain(..k);
            self.events.retain(|&e| e as usize >= k);
            self.events.iter_mut().for_each(|e| *e -= k as u32);
            self.edges.retain(|&e| e as usize >= k);
            self.edges.iter_mut().for_each(|e| *e -= k as u32);
            self.jumps.retain(|&e| e as usize >= k);
            self.jumps.iter_mut().for_each(|e| *e -= k as u32);
            self.ends.retain(|e| e.0 as usize >= k);
            self.ends.iter_mut().for_each(|e| e.0 -= k as u32);
        }
    }

    /// The points a reader can use through the exporter's round end.
    fn table_to(&self, t: f64, cut: f64) -> Table {
        let k = self.read_from(t);
        let e = self.t.partition_point(|&at| at <= cut).max(k + 1).min(self.t.len());
        self.table_range(k, e)
    }

    /// The window from the last point at or before `t`, as it stands: what a chain tree's shadow
    /// points follow (`Open::before`). O(its points).
    fn since(&self, t: f64) -> Window {
        let k = self.read_from(t);
        let from = |p: &[u32]| p.iter().filter(|&&i| i as usize >= k).map(|&i| i - k as u32).collect();
        let ends = self.ends.iter().filter(|e| e.0 as usize >= k).map(|&(i, end)| (i - k as u32, end)).collect();
        Window { t: self.t[k..].to_vec(), v: self.v[k..].to_vec(), events: from(&self.events), edges: from(&self.edges), jumps: from(&self.jumps), ends, trimmed: self.trimmed }
    }

    /// `table_to` as a read that lands the window's jumps takes it: after each edge point the end
    /// of the source edge its jump ran over (`ends`), at its level, as an export that relays the
    /// jump passes it on (`pass_on`). O(points).
    fn landed_to(&self, t: f64, cut: f64) -> Table {
        let k = self.read_from(t);
        let e = self.t.partition_point(|&at| at <= cut).max(k + 1).min(self.t.len());
        let mut table = self.table_range(k, e);
        for &(i, end) in self.ends.iter().rev() {
            let i = i as usize;
            if i < k || i + 1 >= e {
                continue;
            }
            let at = i - k + 1;
            table.t.insert(at, end);
            table.v.insert(at, table.v[at - 1]);
            for x in table.events.iter_mut().chain(table.edges.iter_mut()) {
                if *x as usize >= at {
                    *x += 1;
                }
            }
        }
        table
    }

    /// The points a read from `t` on takes.
    fn table(&self, t: f64) -> Table {
        self.table_range(self.read_from(t), self.t.len())
    }

    /// Points `k..e` as a table. A point's mark by the point after it counts only with that point
    /// in the table: what a read sees of its last point is fixed when the exporter reaches it, never
    /// by how far it has run since. A marked point is an event and no edge.
    fn table_range(&self, k: usize, e: usize) -> Table {
        fn within(points: &[u32], k: usize, to: usize) -> impl Iterator<Item = u32> + '_ {
            points.iter().copied().filter(move |&i| (k..to).contains(&(i as usize)))
        }
        let jumps: Vec<u32> = within(&self.jumps, k, e.saturating_sub(1)).collect();
        let mut events: Vec<u32> = within(&self.events, k, e).chain(jumps.iter().copied()).collect();
        events.sort_unstable();
        events.dedup();
        let edges = within(&self.edges, k, e).filter(|i| jumps.binary_search(i).is_err()).map(|i| i - k as u32).collect();
        Table { t: self.t[k..e].to_vec(), v: self.v[k..e].to_vec(), events: events.into_iter().map(|i| i - k as u32).collect(), edges }
    }
}

/// A late-reading block's rounds taken to run again (`Shadows`): one in this many, each reader in
/// its turn (A-S5: about 2 % of the readers' work).
const SHADOW_EVERY: u64 = 50;

/// After a plan goes in, an edit or a press (`Rounds::shadow_burst`), every reader's every round
/// runs again for this many rounds: where a cut is most likely to go wrong.
const SHADOW_BURST: u64 = 32;

/// A reader with no finished check for this many of its sampling turns (`SHADOW_EVERY` of its
/// rounds each) has its oldest owed round run by the worker (Fable's D): the run slows by one
/// shadow round, about 0.1 ms, at most once a reader in that many rounds, and is never left
/// unchecked however short of checkers it runs.
const SHADOW_FLOOR: u64 = 10;

/// A hub port's round is marked screened (`Hub::screened`) when its largest late term exceeds
/// this share of the circuit's largest source.
const HUB_SCREEN: f64 = 1e-3;

/// The shadow rounds' figures, in `RoundSplit::shadow_worst` order: what the block exports, its
/// difference's mean over the round and its largest, each against that value's range since the
/// rounds began; its own voltages, the same two against the circuit's largest source; and where
/// its own voltages end, against that source.
pub const SHADOW_FIGURES: [&str; 5] = ["export mean", "export largest", "own mean", "own largest", "own end"];

/// How far a late-reading block's round, run again with what it read late on time, may end from
/// where it ran to (the "own end" figure, against the circuit's largest source), in what the plan
/// promises its late reads get wrong (`Rounds::shadow_bar`). Measured with the shadow rounds
/// counting: drum16 pressed, 9.2e-4 at most; the tone box with its plans judged at its present
/// state alone (75 % flat), its key's block reading an ordinary value late (then promised 1e-3) 2.2e-2
/// in the round of the press. A supply or a pad read back a round late is promised the hub's bar
/// (0.1): the tone box's supply ends 1.7e-3 off, a keypad's row 2.6e-2 where its key is released.
/// A reader of returns is held to the hub's bar itself (`RoundPlan::hub_tol`), not ten times it. The
/// other figures go past the bar in normal play where a flip moves by a sample: they count.
pub const SHADOW_ACT: f64 = 10.0;

/// The judge's version: what the growth rule, the shadow rounds and the chain shadow refuse and
/// land by. A host's runs remember their refusals and landings under it (`states::memory_key`),
/// so a changed judge forgets them: raise it with any change to what refuses or lands.
pub const JUDGE: u32 = 1;

/// The "own end" figure's place in `SHADOW_FIGURES`.
const OWN_END: usize = 4;

/// A shadow round that refuses its block's cut (`Rounds::shadow_over`): its block, by place and
/// name, its round's end, and why.
#[derive(Clone, Debug)]
pub(crate) struct ShadowOver {
    pub(crate) block: usize,
    pub(crate) name: String,
    pub(crate) end: f64,
    pub(crate) why: OverWhy,
}

/// Why a shadow round refuses its block's cut (`ShadowOver`).
#[derive(Clone, Debug)]
pub(crate) enum OverWhy {
    /// Its block ends past its bar however shifted within its step bound: its own end figure and
    /// the bar.
    End(f64, f64),
    /// Its own crossings disagree: their counts as it ran and again.
    Crossings(u32, u32),
    /// Its block's shifts since its plan went in (`ShiftSums`) drift or smear past what the plan
    /// promises (`Rounds::shift_bar`), clear of their own scatter: which ("drift" or "smear"), the
    /// figure, the bar, and the rounds with a shift it is taken from.
    Shifts(&'static str, f64, f64, u64),
}

/// How far past its jump round a chain tree's member past its bar is followed (Fable's H1/H2
/// answer): whole rounds of at least this many sample periods, the later window its "own held"
/// figure is taken over.
pub const CHAIN_LONG: f64 = 32.0;

/// A member is followed at an own end over bar / CHAIN_SCREEN. Its own held over that same
/// screen calls for an offset sweep of the tree.
const CHAIN_SCREEN: f64 = 8.0;
/// The equally spaced offsets in the reader step containing the born jump.
const CHAIN_OFFSETS: usize = 8;

/// A born export's first rounds with a jump split after its tree's plan went in, each of which
/// the chain shadow runs again. After them an unmarked export is sampled every `SHADOW_EVERY`
/// jumping rounds (`Rounds::chain_round`).
const CHAIN_FIRST: u64 = 8;

/// A tree still waiting for its members or for a checker goes stale this many rounds after
/// the end of its later window.
const CHAIN_STALE: u64 = 64;

/// A block's round as it ran (`Shadows`): its points' times, its exports point by point (in
/// `Run::exports` order) and its own values point by point (in `Run::own` order).
/// Its own crossings (`CROSSING`): how many, and the time of the last (minus infinity for none).
#[derive(Clone)]
struct Ran {
    t: Vec<f64>,
    exp: Vec<f64>,
    vals: Vec<f64>,
    crossings: u32,
    last_crossing: f64,
}

impl Ran {
    /// A block's round as it ran, from its record. O(points).
    fn of(rec: &Rec) -> Ran {
        let (crossings, last_crossing) = crossings(rec);
        Ran { t: rec.t.clone(), exp: rec.exp.clone(), vals: rec.vals.clone(), crossings, last_crossing }
    }

    fn record(&self) -> EndRecord<'_> {
        EndRecord { t: &self.t, vals: &self.vals, crossings: self.crossings, last_crossing: self.last_crossing }
    }
}

/// A record's own crossings (`CROSSING`): how many, and the time of the last (minus infinity for
/// none). O(points).
fn crossings(rec: &Rec) -> (u32, f64) {
    rec.flags.iter().zip(&rec.t).filter(|(f, _)| **f & CROSSING != 0).fold((0, f64::NEG_INFINITY), |(n, _), (_, &t)| (n + 1, t))
}

/// A late-reading block's round taken to run again: the block as the round began, fed, with the
/// wall taking it (on the core that ran it: `Run::shadow_take`; block 0 as it is fed), and the
/// round as it ran once it has. One copy serves the shadow round and every chain tree that takes
/// the block's round.
struct Open {
    b: u32,
    m: u64,
    t0: f64,
    /// What its feed read with (`Rounds::feed`): the sample grid of a split and whether its
    /// imports' crossings were data.
    grid: f64,
    crossings_as_data: bool,
    run: Option<(Box<Run>, u64)>,
    ran: Option<Ran>,
    /// Whether its shadow round wants it (and as a burst round), and how many chain trees still do.
    late: bool,
    burst: bool,
    trees: u32,
    /// Per export a tree passes on (coupling), its window as the round began (`Rounds::export`):
    /// what the tree's shadow points follow.
    before: Vec<(u32, Window)>,
}

impl Open {
    /// The block's copy and its round for one user: the last takes them, the others clone (the
    /// clone's wall in its place of the take's).
    fn take_for(&mut self, last: bool) -> (Option<(Box<Run>, u64)>, Option<Ran>) {
        if last {
            return (self.run.take(), self.ran.take());
        }
        let at = Instant::now();
        let run = self.run.as_ref().map(|(r, _)| r.clone());
        (run.map(|r| (r, at.elapsed().as_nanos() as u64)), self.ran.clone())
    }
}

/// A chain tree's member, sent with the tree (`Tree`): the block as its round began, what it reads
/// along the tree (each source's name, its delay, and the born import's table landed or the
/// window of a coupling the tree passes on, read from a time), the exports it passes on (the
/// window's place in `Tree::windows`, the export's in `Run::exports`), its round as it ran and its
/// exports' ranges. Its grid and crossings-as-data setting come from its actual feed.
struct Member {
    b: u32,
    run: Box<Run>,
    grid: f64,
    crossings_as_data: bool,
    feeds: Vec<(String, f64, Feed)>,
    outs: Vec<(usize, usize)>,
    ran: Ran,
    ranges: Vec<(f64, f64)>,
    later: Vec<Later>,
}

/// A round of the later window a chain tree's member past its screen is followed through
/// (`Member::later`, `Shadows::follow`): its end, the tables the run fed the block in it, and the
/// round as it ran (shared with every tree that follows it).
struct Later {
    end: f64,
    feeds: Vec<(String, f64, Arc<Table>)>,
    ran: Arc<Ran>,
}

/// What a followed or swept member left over the later window (`follow`, Fable's
/// H1/H2 answer): its "own held" (of its own voltage nodes, the difference as it ran less again
/// integrated with its sign over the window, over the window's length and `peak`, the largest in
/// size), the node that is on, and its own end at the window's end (`OWN_END`).
struct Held {
    held: f64,
    node: String,
    end: f64,
    /// The winning grid offset, or the jump's own offset.
    offset: Option<usize>,
}

/// What a member reads along its tree (`Member::feeds`).
enum Feed {
    Table(Arc<Table>),
    Window(usize, f64),
}

/// One chain round's tree run again, sent to a helper (`Shadows`, `RoundPlan::chains`): the born
/// exports in it (`ks`), its members in stage order, the exports landed into their roots, each
/// member fed the shadow points of the ones before (each coupling's window as the round began,
/// `windows`, which the members' shadow rounds pass their points on to as `Rounds::export` does,
/// the jumps relayed), from `t0` to `end`.
struct Tree {
    ks: Vec<u32>,
    first: bool,
    owed_until: Option<u64>,
    t0: f64,
    end: f64,
    peak: f64,
    bar: f64,
    edges: bool,
    members: Vec<Member>,
    windows: Vec<(u32, Window)>,
}

/// What a chain tree found (`Tree::run`): its bar, its members' figures (`SHADOW_FIGURES`) and
/// signed mean (`figures`), none for a member that failed to step its round again (and the members
/// after it, which it feeds). A followed or swept member keeps its worst held reading. A failed
/// arm leaves a valid reading available for conviction and prevents the tree from healing a mark.
struct TreeFound {
    ks: Vec<u32>,
    first: bool,
    end: f64,
    bar: f64,
    members: Vec<MemberReading>,
    followed: bool,
    failed: bool,
    sweep_clear: bool,
    escalated_by: Vec<u32>,
    unswept: u64,
    own_ns: u64,
    ns: u64,
}

struct MemberReading {
    b: u32,
    figures: Option<([f64; 5], f64)>,
    held: Option<Held>,
}

impl Tree {
    /// The tree's rounds again in stage order. The checker keeps each starting state for the
    /// two arms at each offset in case a member's held reading calls for them.
    fn run(self) -> TreeFound {
        let at = Instant::now();
        let Tree { ks, first, t0, end, peak, bar, edges, mut members, windows, .. } = self;
        let starts: Vec<_> = members.iter().map(|mb| mb.run.clone()).collect();
        let mut passed = windows.clone();
        let mut found = Vec::with_capacity(members.len());
        let (mut failed, mut own) = (false, 0.0);
        for mb in &mut members {
            if failed {
                found.push(MemberReading { b: mb.b, figures: None, held: None });
                continue;
            }
            let run = &mut mb.run;
            own += run.cost;
            for (name, delay, feed) in &mb.feeds {
                let table = match feed {
                    Feed::Table(t) => t.clone(),
                    Feed::Window(w, from) => Arc::new(passed[*w].1.table_to(*from, end)),
                };
                run.s.feed(name, table, *delay);
            }
            if !mb.feeds.is_empty() {
                run.st.breaks_changed(&run.s);
            }
            let exp0: Vec<f64> = run.exports.iter().map(|&l| run.s.x[l as usize]).collect();
            let own0: Vec<f64> = run.own.iter().map(|&(l, _)| run.s.x[l as usize]).collect();
            run.round(end, true);
            let figures = run.failed.is_none().then(|| figures(run, t0, end, &exp0, &own0, &mb.ran, &mb.ranges, peak)).filter(|f| f.0.iter().all(|x| !x.is_nan()));
            failed = figures.is_none();
            let n = run.exports.len();
            for &(w, j) in &mb.outs {
                pass_on(&mut passed[w].1, &run.rec, n, j, true, edges, |_, _| {});
            }
            found.push(MemberReading { b: mb.b, figures, held: None });
        }
        let screen = bar / CHAIN_SCREEN;
        let mut followed = false;
        let mut escalated_by = Vec::new();
        for (mb, reading) in members.iter_mut().zip(&mut found) {
            if reading.figures.as_ref().is_some_and(|f| f.0[OWN_END] > screen) {
                followed = true;
                reading.held = follow(mb, end, peak);
                failed |= reading.held.is_none();
                if reading.held.as_ref().is_some_and(|h| h.held > screen) {
                    escalated_by.push(mb.b);
                }
            }
        }
        let (mut unswept, mut sweep_clear) = (0, true);
        if !escalated_by.is_empty() {
            for ((mb, start), reading) in members.iter().zip(&starts).zip(&mut found) {
                if !mb.feeds.iter().any(|(_, _, f)| matches!(f, Feed::Table(_))) {
                    unswept += 1;
                    continue;
                }
                let Some(jump) = mb.born_jump(t0, end).filter(|_| mb.grid > 0.0) else {
                    failed = true;
                    continue;
                };
                let base = (jump / mb.grid).floor() * mb.grid;
                for k in 0..CHAIN_OFFSETS {
                    let delta = base + (k as f64 / CHAIN_OFFSETS as f64) * mb.grid - jump;
                    match mb.offset(start, &windows, t0, end, peak, delta) {
                        Some(mut held) => {
                            sweep_clear &= held.held < bar;
                            if reading.held.as_ref().is_none_or(|own| held.held > own.held) {
                                held.offset = Some(k);
                                reading.held = Some(held);
                            }
                        }
                        None => failed = true,
                    }
                }
            }
            sweep_clear &= unswept < members.len() as u64;
        }
        TreeFound { ks, first, end, bar, members: found, followed, failed, sweep_clear, escalated_by, unswept,
            own_ns: own as u64, ns: at.elapsed().as_nanos() as u64 }
    }
}

impl Member {
    /// The born jump in reader time, taking a crossing before a plain event. A next-float
    /// jump can lack an event mark, and the last point exported can be a jump at the round end.
    fn born_jump(&self, t0: f64, end: f64) -> Option<f64> {
        self.feeds.iter().find_map(|(_, delay, feed)| {
            let Feed::Table(table) = feed else { return None };
            let mut marked = table.edges.iter().chain(&table.events).map(|&i| table.t[i as usize] + delay);
            marked.clone().find(|&t| t >= t0 && t < end)
                .or_else(|| table.t.windows(2).filter(|ts| ts[1] <= ts[0].next_up())
                    .map(|ts| ts[0] + delay).find(|&t| t >= t0 && t <= end))
                .or_else(|| marked.find(|&t| t == end))
        })
    }

    /// The split and landed readings from the same real starting state, the born feeds moved
    /// together by `delta`. Both arms see the same window feeds and the run's later feeds.
    fn offset(&self, start: &Run, windows: &[(u32, Window)], t0: f64, end: f64, peak: f64, delta: f64) -> Option<Held> {
        let (mut split_feeds, mut landed_feeds) = (Vec::new(), Vec::new());
        for (name, delay, feed) in &self.feeds {
            let (split, landed) = match feed {
                Feed::Table(t) => {
                    let mut shifted = t.as_ref().clone();
                    for at in &mut shifted.t { *at += delta; }
                    let landed = Arc::new(shifted);
                    let mut split = landed.split(self.grid, *delay, t0, None, end, end)
                        .map_or_else(|| landed.clone(), |(table, _)| Arc::new(table));
                    if self.crossings_as_data && !split.edges.is_empty() {
                        let table = Arc::make_mut(&mut split);
                        let edges = std::mem::take(&mut table.edges);
                        table.events.retain(|i| edges.binary_search(i).is_err());
                    }
                    (split, landed)
                }
                Feed::Window(w, from) => {
                    let table = Arc::new(windows[*w].1.table_to(*from, end));
                    (table.clone(), table)
                }
            };
            split_feeds.push((name.clone(), *delay, split));
            landed_feeds.push((name.clone(), *delay, landed));
        }
        let (split, landed) = self.records(start, end, &split_feeds).zip(self.records(start, end, &landed_feeds))?;
        let span = (self.later.last()?.end - end).max(f64::MIN_POSITIVE);
        let no = start.own.len();
        let (mut winner, mut later_end) = (None, 0.0_f64);
        for (i, &(local, _)) in start.own.iter().enumerate() {
            let u = local as usize;
            if start.s.unknowns[u].kind != UnkKind::Voltage { continue; }
            let last = |rec: &Rec, v0| if rec.t.is_empty() { v0 } else { rec.vals[(rec.t.len() - 1) * no + i] };
            let v0 = start.s.x[u];
            let (mut vs, mut vl) = (last(&split[0], v0), last(&landed[0], v0));
            let (mut from, mut net) = (end, 0.0);
            for ((s, l), lr) in split[1..].iter().zip(&landed[1..]).zip(&self.later) {
                net += differ(from, (vs, vl), &s.t, &|p| s.vals[p * no + i], &l.t, &|p| l.vals[p * no + i]).2;
                (vs, vl, from) = (last(s, vs), last(l, vl), lr.end);
            }
            later_end = later_end.max((vs - vl).abs() / peak);
            let held = net.abs() / span / peak;
            if held.is_nan() { return None; }
            if winner.is_none_or(|(best, _)| held >= best) {
                winner = Some((held, u));
            }
        }
        winner.map(|(held, u)| Held { held, node: start.s.unknowns[u].name.clone(), end: later_end, offset: None })
    }

    /// One sweep arm's jump round and later rounds, starting from the checker's saved copy.
    fn records(&self, start: &Run, end: f64, first: &[(String, f64, Arc<Table>)]) -> Option<Vec<Rec>> {
        let mut run = start.clone();
        let mut records = Vec::with_capacity(1 + self.later.len());
        for (end, feeds) in std::iter::once((end, first)).chain(self.later.iter().map(|lr| (lr.end, lr.feeds.as_slice()))) {
            for (name, delay, table) in feeds {
                run.s.feed(name, table.clone(), *delay);
            }
            if !feeds.is_empty() { run.st.breaks_changed(&run.s); }
            run.round(end, true);
            if run.failed.is_some() { return None; }
            records.push(std::mem::take(&mut run.rec));
        }
        Some(records)
    }
}

/// A member past the screen at its round's end (`end`) run on through its later window
/// (`Member::later`), each round fed the tables the run fed it: its `Held`, none when it fails a
/// round. Each side's later round starts from where that side ended the one before.
/// O(the window's steps + points).
fn follow(mb: &mut Member, end: f64, peak: f64) -> Option<Held> {
    let run = &mut mb.run;
    let no = run.own.len();
    let last = |ran: &Ran, i: usize, v0: f64| if ran.t.is_empty() { v0 } else { ran.vals[(ran.t.len() - 1) * no + i] };
    let (mut nets, mut ends, mut from, mut prev) = (vec![0.0f64; no], vec![0.0f64; no], end, &mb.ran);
    for lr in &mb.later {
        run.rec = Rec::default();
        for (name, delay, table) in &lr.feeds {
            run.s.feed(name, table.clone(), *delay);
        }
        if !lr.feeds.is_empty() {
            run.st.breaks_changed(&run.s);
        }
        let own0: Vec<f64> = run.own.iter().map(|&(l, _)| run.s.x[l as usize]).collect();
        run.round(lr.end, true);
        if run.failed.is_some() {
            return None;
        }
        let rec = &run.rec;
        for (i, &v0) in own0.iter().enumerate() {
            if run.s.unknowns[run.own[i].0 as usize].kind != UnkKind::Voltage {
                continue;
            }
            let (_, _, net) = differ(from, (last(prev, i, v0), v0), &lr.ran.t, &|p| lr.ran.vals[p * no + i], &rec.t, &|p| rec.vals[p * no + i]);
            nets[i] += net;
            let again = if rec.t.is_empty() { v0 } else { rec.vals[(rec.t.len() - 1) * no + i] };
            ends[i] = (last(&lr.ran, i, v0) - again).abs() / peak;
        }
        (from, prev) = (lr.end, &*lr.ran);
    }
    let span = (from - end).max(f64::MIN_POSITIVE);
    let (i, net) = nets.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
    let held = net.abs() / span / peak;
    (!held.is_nan()).then(|| Held { held, node: run.s.unknowns[run.own[i].0 as usize].name.clone(), end: ends.iter().copied().fold(0.0, f64::max), offset: None })
}

/// A block's round again against the round as it ran (`Shadow::run`, `Tree::run`), both from the
/// values it began with (`exp0`, `own0`): the `SHADOW_FIGURES`, and of its own voltage nodes the
/// signed mean over the round of the difference (as it ran less again, volts) largest in size.
/// O(both rounds' points).
#[allow(clippy::too_many_arguments)]
fn figures(run: &Run, t0: f64, end: f64, exp0: &[f64], own0: &[f64], ran: &Ran, ranges: &[(f64, f64)], peak: f64) -> ([f64; 5], f64) {
    let (span, rec) = ((end - t0).max(f64::MIN_POSITIVE), &run.rec);
    let n = exp0.len();
    let (mut exp_mean, mut exp_most) = (0.0f64, 0.0f64);
    for (j, &v0) in exp0.iter().enumerate() {
        let (area, most, _) = differ(t0, (v0, v0), &ran.t, &|p| ran.exp[p * n + j], &rec.t, &|p| rec.exp[p * n + j]);
        let range = ranges.get(j).map_or(0.0, |r| r.1 - r.0);
        let scale = if range > 0.0 { range } else { peak };
        (exp_mean, exp_most) = (exp_mean.max(area / span / scale), exp_most.max(most / scale));
    }
    let no = own0.len();
    let (mut own_mean, mut own_most, mut own_end, mut signed) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (i, &v0) in own0.iter().enumerate() {
        if run.s.unknowns[run.own[i].0 as usize].kind != UnkKind::Voltage {
            continue;
        }
        let (area, most, net) = differ(t0, (v0, v0), &ran.t, &|p| ran.vals[p * no + i], &rec.t, &|p| rec.vals[p * no + i]);
        let last = |t: &[f64], vals: &[f64]| if t.is_empty() { v0 } else { vals[(t.len() - 1) * no + i] };
        (own_mean, own_most) = (own_mean.max(area / span / peak), own_most.max(most / peak));
        own_end = own_end.max((last(&ran.t, &ran.vals) - last(&rec.t, &rec.vals)).abs() / peak);
        if (net / span).abs() > signed.abs() {
            signed = net / span;
        }
    }
    ([exp_mean, exp_most, own_mean, own_most, own_end], signed)
}

/// One record of a block's round for `end_shift`: its points' times, its own values point by point
/// (`Run::own` order), and its own crossings (`CROSSING`): how many, and the time of the last
/// (minus infinity for none).
#[derive(Clone, Copy, Debug)]
pub struct EndRecord<'a> {
    pub t: &'a [f64],
    pub vals: &'a [f64],
    pub crossings: u32,
    pub last_crossing: f64,
}

/// Where a block's round run again ends against the round as it ran, as a shift in time (Fable's
/// A-next): a value read a round late moves the instant a block's own edge falls by a little, and
/// an edge that falls just before the round's end in one run and just after it in the other leaves
/// the two a whole swing apart there, though they differ by that little.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EndShift {
    /// The ends within the bar, and the shift (s) their difference makes along the round again's
    /// last step (its projection on that step's slope): > 0 where the round as it ran is ahead.
    /// None where the block's own voltages are still there, or the projection is longer than the
    /// step bound.
    Plain(Option<f64>),
    /// The ends past the bar, but within it with one run taken that much earlier (s): > 0 where the
    /// round as it ran is ahead, < 0 where it is behind, never longer than the step bound, and
    /// only to an edge: the run taken earlier has an own crossing within the step bound of the end.
    /// Then the end figure once shifted (of the peak, as the bar): within the bar.
    Shifted(f64, f64),
    /// The ends past the bar however shifted within the step bound.
    Over,
}

/// A reader's shifts (`EndShift`) over its shadow rounds (`RoundSplit::shift_by`, `Shadows::sums`):
/// how many, their sum, the sum of their sizes and of their squares, the length of every round
/// taken summed (with a shift or not), and the largest. A round's shift is taken only where the
/// block has own crossings in it, as it ran or again (main's settled step 4): a still block's end
/// projected on a slope of next to nothing is a level read as a time, not its edges moved.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShiftSums {
    pub n: u64,
    pub sum: f64,
    pub abs: f64,
    pub sq: f64,
    pub span: f64,
    pub most: f64,
}

impl ShiftSums {
    /// One round's shift `tau` (s) over its length `span`.
    pub fn add(&mut self, tau: f64, span: f64) {
        self.n += 1;
        self.sum += tau;
        self.abs += tau.abs();
        self.sq += tau * tau;
        self.span += span;
        self.most = self.most.max(tau.abs());
    }

    /// A round of length `span` taken without a shift: its time counts, so a block whose edges
    /// are a round or more apart is not judged as if it had one every round.
    pub fn pass(&mut self, span: f64) {
        self.span += span;
    }

    /// The shifts' mean over the time they were taken in: what lateness does to the block's
    /// frequency, as a share.
    pub fn drift(&self) -> f64 {
        self.sum.abs() / self.span
    }

    /// Their sizes' mean over that time: what it does to the block's waveform, in the block's swing.
    pub fn smear(&self) -> f64 {
        self.abs / self.span
    }

    /// Whether the drift stands clear of the shifts' own scatter: their sum past three times the
    /// root of their squares' sum.
    pub fn drifts(&self) -> bool {
        self.sum.abs() > 3.0 * self.sq.sqrt()
    }

    /// Whether the smear stands clear of the same scatter: the sizes' sum past three times the root
    /// of their squares' sum, so a few large shifts alone (at least ten like-sized ones) make no
    /// smear.
    pub fn smears(&self) -> bool {
        self.abs > 3.0 * self.sq.sqrt()
    }
}

/// A block's round run again (`b`) against the round as it ran (`a`), both from its own values
/// `own0` at `t0` to `end`, each linear between its points (Fable's A-next): over its own values
/// `volt` marks (its voltages), how its ends shift (`EndShift`) against `bar` of `peak`, by at most
/// `tau_bar` (the block's step bound: the solver places no edge finer than its step, so a shift
/// within it is what the step allows), and only to an edge (a level apart, however smooth the
/// waveform it rides on, stays a level); and whether their own crossings agree: the same count, or
/// one apart where the run with the more has its last within `tau_bar` of the end (the edge one run
/// has just before the end and the other just after). A value not a number is past every bar.
/// O(the points in the last `tau_bar` x own values).
#[allow(clippy::too_many_arguments)]
pub fn end_shift(t0: f64, end: f64, own0: &[f64], volt: &[bool], a: EndRecord, b: EndRecord, peak: f64, bar: f64, tau_bar: f64) -> (EndShift, bool) {
    let n = own0.len();
    let last = |r: &EndRecord, i: usize| if r.t.is_empty() { own0[i] } else { r.vals[(r.t.len() - 1) * n + i] };
    let more = if a.crossings >= b.crossings { &a } else { &b };
    let parity = a.crossings == b.crossings || (a.crossings.abs_diff(b.crossings) == 1 && more.last_crossing >= end - tau_bar);
    let plain = (0..n).filter(|&i| volt[i]).map(|i| (last(&a, i) - last(&b, i)).abs() / peak).fold(0.0f64, |m, x| if x > m || x.is_nan() { x } else { m });
    if plain <= bar {
        // b's last step that takes time, from its point before (`t0`'s values before its first)
        let at = |p: usize| if p == 0 { (t0, own0) } else { (b.t[p - 1], &b.vals[(p - 1) * n..p * n]) };
        let k = b.t.len();
        let (te, ve) = at(k);
        let Some((tp, vp)) = (0..k).rev().map(at).find(|&(t, _)| t < te) else { return (EndShift::Plain(None), parity) };
        let (mut ds, mut ss) = (0.0, 0.0);
        for i in (0..n).filter(|&i| volt[i]) {
            let s = (ve[i] - vp[i]) / (te - tp);
            ds += (last(&a, i) - last(&b, i)) * s;
            ss += s * s;
        }
        let tau = ds / ss;
        return (EndShift::Plain((ss > 0.0 && tau.abs() <= tau_bar).then_some(tau)), parity);
    }
    let (tol, span) = (bar * peak, tau_bar.min(end - t0));
    let edged = |r: &EndRecord| r.last_crossing >= end - tau_bar;
    let behind = edged(&b).then(|| shift_back(t0, end, own0, volt, &b, &|i| last(&a, i), tol, span)).flatten();
    let ahead = edged(&a).then(|| shift_back(t0, end, own0, volt, &a, &|i| last(&b, i), tol, span)).flatten();
    // the end figure once shifted: record `r`'s own values `u` before the end against `c`'s end
    let shifted = |r: &EndRecord, c: &EndRecord, u: f64| {
        (0..n).filter(|&i| volt[i]).map(|i| (value_at(t0, own0, r, i, end - u) - last(c, i)).abs() / peak).fold(0.0f64, f64::max)
    };
    let shift = match (behind, ahead) {
        (Some(u), Some(v)) if v < u => EndShift::Shifted(v, shifted(&a, &b, v)),
        (Some(u), _) => EndShift::Shifted(-u, shifted(&b, &a, u)),
        (None, Some(v)) => EndShift::Shifted(v, shifted(&a, &b, v)),
        (None, None) => EndShift::Over,
    };
    (shift, parity)
}

/// Record `r`'s own value `i` at `t` (in `[t0, end]`): linear between its points, `own0` at `t0`
/// its first, held past its last. O(log its points).
fn value_at(t0: f64, own0: &[f64], r: &EndRecord, i: usize, t: f64) -> f64 {
    let n = own0.len();
    let k = r.t.partition_point(|&x| x <= t);
    let (tl, vl) = if k == 0 { (t0, own0[i]) } else { (r.t[k - 1], r.vals[(k - 1) * n + i]) };
    match r.t.get(k) {
        Some(&th) => vl + (r.vals[k * n + i] - vl) * (t - tl) / (th - tl),
        None => vl,
    }
}

/// The least `u` in `[0, span]` where each of record `r`'s own values `volt` marks, `u` before `end`,
/// is within `tol` of the other run's end (`c`): `r` linear between its points, `own0` at `t0` its
/// first, held past its last. Per step each value gives an interval of times; their intersection's
/// latest, in the latest step that has one. O(r's points in the last `span` x own values).
#[allow(clippy::too_many_arguments)]
fn shift_back(t0: f64, end: f64, own0: &[f64], volt: &[bool], r: &EndRecord, c: &dyn Fn(usize) -> f64, tol: f64, span: f64) -> Option<f64> {
    let n = own0.len();
    let at = |p: usize| if p == 0 { (t0, own0) } else { (r.t[p - 1], &r.vals[(p - 1) * n..p * n]) };
    let from = end - span;
    // the steps from the latest back: past the last point the value held to the end
    let k = r.t.len();
    let mut steps = std::iter::once((at(k), (end, at(k).1))).chain((1..=k).rev().map(|p| (at(p - 1), at(p))));
    steps.find_map(|((tl, vl), (th, vh))| {
        if th < from || th <= tl {
            return None;
        }
        let (mut lo, mut hi) = (tl.max(from), th.min(end));
        for i in (0..n).filter(|&i| volt[i]) {
            let (d0, d1) = (vl[i] - c(i), vh[i] - c(i));
            if !(d0.is_finite() && d1.is_finite()) {
                return None;
            }
            let m = (d1 - d0) / (th - tl);
            if m == 0.0 {
                if d0.abs() > tol {
                    return None;
                }
                continue;
            }
            let (x, y) = (tl + (-tol - d0) / m, tl + (tol - d0) / m);
            (lo, hi) = (lo.max(x.min(y)), hi.min(x.max(y)));
            if lo > hi {
                return None;
            }
        }
        Some(end - hi)
    })
}

/// A block's round's points of its export `j` (of `n`) onto that export's window, as the readers
/// take them: a restart's point marks the one before it (the importers land on its instant, as on
/// an edge: past it a value may jump, not follow the slope before it); an event stays one, and a
/// point the block landed on an imported event alone (`RELAYED`) is one where the export `relays`
/// it and the value moves to the next point. With `edges`, a crossing or a relayed switching instant
/// is an edge; a relayed corner is a plain event. Where the export relays, the end of the source
/// edge a jump ran over follows its edge point at that point's level,
/// so a block that lands the jump takes its first step to there (`Bank::jump_step`). `each` sees
/// each recorded point's time and value before it is appended. O(points).
fn pass_on(w: &mut Window, rec: &Rec, n: usize, j: usize, relays: bool, edges: bool, mut each: impl FnMut(f64, f64)) {
    let mut ends = rec.ends.iter().peekable();
    for (p, &t) in rec.t.iter().enumerate() {
        let v = rec.exp[p * n + j];
        if rec.flags[p] & JUMP != 0 {
            w.mark_newest();
        }
        let relayed = relays && rec.flags[p] & RELAYED != 0 && rec.exp.get((p + 1) * n + j).is_none_or(|&next| next != v);
        let event = rec.flags[p] & EVENT != 0 || relayed;
        let edge = edges && (rec.flags[p] & CROSSING != 0 || relayed && rec.flags[p] & RELAYED_EDGE != 0);
        each(t, v);
        w.push(t, v, event, edge);
        if let Some(&(_, end)) = ends.next_if(|&&(q, _)| q as usize == p) {
            if end > t && rec.t.get(p + 1).is_some_and(|&next| end < next) {
                if relays {
                    w.push(end, v, false, false);
                } else {
                    w.ends.push(((w.t.len() - 1) as u32, end));
                }
            }
        }
    }
}

/// What a shadow round found (`Shadow::run`): its block, its round's start and end, its figures
/// (`SHADOW_FIGURES`) and its end's shift (`end_shift`), none when the block failed to step the
/// round again, its own crossings as it ran and again, and its wall.
struct Found {
    b: u32,
    t0: f64,
    end: f64,
    figures: Option<[f64; 5]>,
    shift: Option<(EndShift, bool)>,
    crossings: (u32, u32),
    ns: u64,
}

/// The shadow rounds (A-S5). After a block that reads a value a round late has run
/// a round, and every value it read late has been exported for that round on time, the round runs
/// again on a helper from the block as it began, the late values read on time, and what it exports
/// and where it ends are compared with the round it ran: what lateness did to that block in that
/// round, through its own solver, flips included. Where the block ends past its bar
/// (`Rounds::shadow_bar`) however shifted within its step bound, or its own crossings disagree
/// (`end_shift`), or its shifts since its plan went in drift or smear past what the plan promises
/// (`sums`, `Rounds::shift_bar`), the run refuses the cut it reads late at
/// (`LiveSim::shadow_taken`); the other figures count. A reader's round is taken one in
/// `SHADOW_EVERY`, each reader in its turn, and every round for `SHADOW_BURST` rounds after a plan
/// goes in, an edit or a press. The block 0 of a run with an attached process steps with the
/// process and is not taken (`shadow_ctl`). From the run's values alone; the run never waits for
/// one, and but for a round past its bar they change nothing it does (`RoundSplit::shadows`).
struct Shadows {
    /// The blocks that read a value a round late, in block order.
    readers: Vec<u32>,
    /// Per reader (in `readers` order), its shifts since its plan went in (Fable's A-next step 4):
    /// a plan these rounds keep keeps them.
    sums: Vec<ShiftSums>,
    /// Whether block 0 is taken: no process attached.
    ctl: bool,
    /// Every reader's every round through this one (block 0's numbering).
    burst_to: u64,
    open: Vec<Open>,
    tx: mpsc::Sender<Option<Found>>,
    rx: mpsc::Receiver<Option<Found>>,
    /// Per coupling, its values' range since the rounds began: an export's figure is taken
    /// against it.
    range: Vec<(f64, f64)>,
    /// The first shadow round past its bar since the run last took one (`Rounds::shadow_over`).
    over: Option<ShadowOver>,
    /// The chain shadow (`RoundPlan::chains`, `Tree`): the imports a born jump split in not yet
    /// counted (`RoundSplit::chain_jumps`); per born export its rounds with a jump split since its
    /// tree's plan went in and the count its next periodic check is due at (`Rounds::chain_round`); the chain
    /// rounds being taken; the trees back from the helpers; and the trees the plan kept from the
    /// one before and began, not yet counted (`RoundSplit::chain_kept`).
    jumps: u64,
    seen: Vec<ChainJumps>,
    taking: Vec<ChainRound>,
    /// Ready trees on first jumps or taken for a marked export alone, in the order they first
    /// waited for a checker.
    owed: Vec<Tree>,
    /// The shadow rounds ready and owed a checker, per reader (in `readers` order, Fable's Q-checks
    /// answer): its burst rounds oldest first, until stale (`CHAIN_STALE` rounds past their own)
    /// and thinned evenly past `SHADOW_BURST`; its newest sampling round, which replaces an older
    /// one; and block 0's round count at its last finished check (`SHADOW_FLOOR`).
    owed_burst: Vec<VecDeque<Shadow>>,
    owed_sampling: Vec<Option<Shadow>>,
    checked: Vec<u64>,
    tree_tx: mpsc::Sender<Option<TreeFound>>,
    tree_rx: mpsc::Receiver<Option<TreeFound>>,
    fresh: (u64, u64),
    follow: Vec<Follow>,
    /// The act (`Rounds::chain_act`): per born export, its mark (the jumps it has healed on in a
    /// row since a held union marked it), and the convictions pending for the run. Each records
    /// the chain index, the held member's block and what that member held.
    marks: Vec<Option<u64>>,
    convicted: Vec<(u32, u32, String)>,
}

/// A block's read of a hub node it returns its current into (`HolderPort`), as fed for its round
/// (`Rounds::feed`). It records the node's local unknown, and the table and delay the node's
/// boundary reads. Its late term at a point is the node less that value, what its port adds for
/// its own draw (`Port`'s `r e + w + l de/dt`). As the block steps (`Run::step`), `most` keeps
/// the round's largest late term, read at the table from `at` on (the round's points come in
/// time order).
#[derive(Clone)]
struct HubRead {
    node: u32,
    table: Arc<Table>,
    delay: f64,
    at: usize,
    most: f64,
}

impl HubRead {
    /// Each read's late term at the accepted point at `t` (the block's unknowns `x`) kept in its
    /// `most`. Out of line: a round asks for it rarely (`Rounds::hub_screen`), and the step's own
    /// loop keeps its code as it was without it. O(reads), each table read on from `at`.
    #[inline(never)]
    fn screen(hub: &mut [HubRead], x: &[f64], t: f64) {
        for h in hub {
            let e = x[h.node as usize] - h.table.value_from(t - h.delay, &mut h.at);
            h.most = h.most.max(e.abs());
        }
    }
}

/// A holder port's place in the recording (`Hub`). Its consumer's block reads the node through
/// the boundary `<node>#in`; the port identifies the node's local unknown there.
#[derive(Clone)]
struct HubPort {
    block: u32,
    input: String,
    node: u32,
}

/// The hub ports, and the port rounds whose late terms passed the screen, as each block exports
/// (`Rounds::export`): one figure a read a round, its largest late term, kept by the pack that
/// steps it, in the rounds screened (`Rounds::hub_screen`: every round, else those the merge
/// gate's take will ask for). Only their count is read so far (`RoundSplit::hub_onsets`); the
/// take adds the per-port marks and the readers' crossings with the trigger that reads them.
#[derive(Clone)]
struct Hub {
    ports: Vec<HubPort>,
    /// The port rounds past the screen since the split last took them (`Rounds::shadows_post`).
    onsets: u64,
}

impl Hub {
    /// The plan's holder ports (`RoundPlan::ports`) as the blocks read them.
    fn new(ports: &[HolderPort], runs: &[Run]) -> Hub {
        let ports = ports.iter().filter_map(|p| {
            let (node, _holder) = p.name.rsplit_once("#port")?;
            let local = runs.get(p.block as usize)?.s.unknown(node)?;
            Some(HubPort { block: p.block, input: format!("{node}#in"), node: local as u32 })
        }).collect();
        Hub { ports, onsets: 0 }
    }

    /// Block `block`'s read of its input `name` screened, when it is a hub port's (`Run::hub`).
    /// Cold and out of line, as each hub site is: rounds screen when asked (`Rounds::hub_screen`),
    /// and the worker's path keeps its code as it was without them. O(ports).
    #[cold]
    #[inline(never)]
    fn read(&self, reads: &mut Vec<HubRead>, block: usize, name: &str, table: &Arc<Table>, delay: f64) {
        if let Some(p) = self.ports.iter().find(|p| p.block as usize == block && p.input == name) {
            reads.push(HubRead { node: p.node, table: table.clone(), delay, at: 0, most: 0.0 });
        }
    }

    /// A screened round's reads past the screen (`HUB_SCREEN` of the run's peak, from its `hold`)
    /// counted. Cold, as `read`. O(reads).
    #[cold]
    #[inline(never)]
    fn tally(&mut self, reads: &[HubRead], hold: f64) {
        let peak = if hold.is_finite() { hold / crate::livesim::HOLD } else { 1.0 };
        self.onsets += reads.iter().filter(|h| h.most > HUB_SCREEN * peak).count() as u64;
    }
}

/// A born export's count of rounds with a jump split and the count its next periodic check is
/// due at, carried with a kept chain.
#[derive(Clone, Default)]
struct ChainJumps {
    seen: u64,
    next: u64,
}

/// A chain round being taken (`Shadows::taking`, Fable's chain-shadow answer, Q2): its round,
/// start and end, the born exports that joined it (`RoundPlan::chains`), whether one of them is
/// on its first jumps, and the later rounds its members' rounds are kept for (`Shadows::follow`,
/// `Rounds::long_rounds`). One tree: each block of the round run again once. A marked export's
/// round is taken alone (`Shadows::marks`), beside the union's.
struct ChainRound {
    m: u64,
    t0: f64,
    end: f64,
    ks: Vec<u32>,
    first: bool,
    long: u32,
    alone: bool,
}

/// Block `r`'s followed round `m`, once it has run (`Rounds::trees_post`). O(follows).
fn followed(follow: &[Follow], r: u32, m: u64) -> Option<&Follow> {
    follow.iter().find(|f| f.b == r && f.m == m && f.ran.is_some())
}

/// What a block's feed takes from the shadows (`Rounds::feed_takes`): whether its round is run
/// again, and whether a long chain tree follows it.
#[derive(Clone, Copy)]
struct FeedTakes {
    shadow: bool,
    follows: bool,
}

/// A chain round's member's later round, kept in case the tree follows it (`Shadows::follow`,
/// `Member::later`): the block, the round, its end, the tables the run fed it and its round as it
/// ran.
struct Follow {
    b: u32,
    m: u64,
    end: f64,
    feeds: Vec<(String, f64, Arc<Table>)>,
    ran: Option<Arc<Ran>>,
}

impl Shadows {
    fn new(readers: Vec<u32>, range: Vec<(f64, f64)>, chains: usize) -> Shadows {
        let (tx, rx) = mpsc::channel();
        let (tree_tx, tree_rx) = mpsc::channel();
        let n = readers.len();
        Shadows { sums: vec![ShiftSums::default(); n], readers, ctl: false, burst_to: 0, open: Vec::new(), tx, rx, range, over: None, jumps: 0, seen: vec![ChainJumps::default(); chains], taking: Vec::new(), owed: Vec::new(),
            owed_burst: (0..n).map(|_| VecDeque::new()).collect(), owed_sampling: (0..n).map(|_| None).collect(), checked: vec![0; n], tree_tx, tree_rx, fresh: (0, chains as u64), follow: Vec::new(),
            marks: vec![None; chains], convicted: Vec::new() }
    }
}

/// A copy of the rounds (a snapshot) takes the readers, their shifts' sums, ranges and the born
/// exports' counts and marks, never a round or a tree taken, one on its way, one past its bar or an
/// export convicted: a run back at the snapshot takes its rounds again.
impl Clone for Shadows {
    fn clone(&self) -> Self {
        Shadows { sums: self.sums.clone(), ctl: self.ctl, burst_to: self.burst_to, seen: self.seen.clone(), marks: self.marks.clone(), checked: self.checked.clone(), ..Shadows::new(self.readers.clone(), self.range.clone(), 0) }
    }
}

/// One shadow round, sent to a helper: the block as its round began, the late values on time for
/// its boundary sources, the round as it ran, its exports' ranges and the circuit's largest source.
struct Shadow {
    b: u32,
    m: u64,
    burst: bool,
    t0: f64,
    run: Box<Run>,
    feeds: Vec<(String, Arc<Table>)>,
    ran: Ran,
    ranges: Vec<(f64, f64)>,
    peak: f64,
    bar: f64,
}

impl Shadow {
    /// The round again, its late values on time, and its figures and its end's shift
    /// (`end_shift`, against its bar, by its step bound) against the round as it ran.
    /// O(the round's steps + points).
    fn run(self) -> Found {
        let at = Instant::now();
        let Shadow { b, t0, mut run, feeds, ran, ranges, peak, bar, .. } = self;
        let tau_bar = run.st.max_step;
        let end = ran.t.last().copied().unwrap_or(t0);
        for (name, table) in &feeds {
            run.s.feed(name, table.clone(), 0.0);
        }
        if !feeds.is_empty() {
            run.st.breaks_changed(&run.s);
        }
        let exp0: Vec<f64> = run.exports.iter().map(|&l| run.s.x[l as usize]).collect();
        let own0: Vec<f64> = run.own.iter().map(|&(l, _)| run.s.x[l as usize]).collect();
        run.round(end, true);
        let figures = run.failed.is_none().then(|| figures(&run, t0, end, &exp0, &own0, &ran, &ranges, peak).0).filter(|f| f.iter().all(|x| !x.is_nan()));
        let (again, last_crossing) = crossings(&run.rec);
        let shift = figures.is_some().then(|| {
            let volt: Vec<bool> = run.own.iter().map(|&(l, _)| run.s.unknowns[l as usize].kind == UnkKind::Voltage).collect();
            let rec = EndRecord { t: &run.rec.t, vals: &run.rec.vals, crossings: again, last_crossing };
            end_shift(t0, end, &own0, &volt, ran.record(), rec, peak, bar, tau_bar)
        });
        Found { b, t0, end, figures, shift, crossings: (ran.crossings, again), ns: at.elapsed().as_nanos() as u64 }
    }
}

/// Two runs of one value over a round from `t0`, `a` from `v0.0` and `b` from `v0.1`, each linear
/// between its own points (times `ta` and `tb`, values by point): their difference's size integrated over the
/// round (trapezoids on the union of their points), at its largest, and the difference (`a` less
/// `b`) integrated. O(points).
fn differ(t0: f64, v0: (f64, f64), ta: &[f64], va: &dyn Fn(usize) -> f64, tb: &[f64], vb: &dyn Fn(usize) -> f64) -> (f64, f64, f64) {
    // the value at `t`, `k` its first point at or after `t`
    let at = |ts: &[f64], v: &dyn Fn(usize) -> f64, v0: f64, k: usize, t: f64| {
        if k >= ts.len() {
            return if ts.is_empty() { v0 } else { v(ts.len() - 1) };
        }
        let (tp, vp) = if k == 0 { (t0, v0) } else { (ts[k - 1], v(k - 1)) };
        if ts[k] > tp { vp + (v(k) - vp) * (t - tp) / (ts[k] - tp) } else { v(k) }
    };
    let (mut i, mut j) = (0, 0);
    let (mut prev_t, mut prev_d, mut prev_s, mut area, mut most, mut net) = (t0, (v0.0 - v0.1).abs(), v0.0 - v0.1, 0.0, (v0.0 - v0.1).abs(), 0.0);
    while i < ta.len() || j < tb.len() {
        let t = ta.get(i).copied().unwrap_or(f64::INFINITY).min(tb.get(j).copied().unwrap_or(f64::INFINITY));
        let s = at(ta, va, v0.0, i, t) - at(tb, vb, v0.1, j, t);
        let d = s.abs();
        area += 0.5 * (d + prev_d) * (t - prev_t);
        net += 0.5 * (s + prev_s) * (t - prev_t);
        most = most.max(d);
        (prev_t, prev_d, prev_s) = (t, d, s);
        i += usize::from(ta.get(i) == Some(&t));
        j += usize::from(tb.get(j) == Some(&t));
    }
    (area, most, net)
}

/// A device's unknowns in `s`, none when it is not there.
fn unks_of(s: &Solver, name: &str) -> Vec<u32> {
    let mut out = Vec::new();
    if let Some((b, i)) = s.find_device(name) {
        s.banks[b].inst_unks(i, &mut out);
    }
    out
}

/// Per unknown of a block's solver `s`: whether its part of the block (the unknowns its devices
/// join, each device all its terminals: a switch its control too, though it reads it through its
/// state, unless the plan proved it in both positions, `switches`) holds both a boundary the block
/// reads (`reads`) and an unknown it exports. Only a device there can change the block's response
/// from what it reads to what it exports, so only its edit can change an entry of the plan's round
/// map; an edit elsewhere keeps the map's proof.
fn on_loop(s: &Solver, reads: &[&str], exports: &[u32], switches: &[String]) -> Vec<bool> {
    fn find(root: &mut [u32], mut u: u32) -> u32 {
        while root[u as usize] != u {
            root[u as usize] = root[root[u as usize] as usize];
            u = root[u as usize];
        }
        u
    }
    let n = s.x.len();
    let mut root: Vec<u32> = (0..n as u32).collect();
    let mut unks = Vec::new();
    for bank in &s.banks {
        for i in 0..bank.len() {
            unks.clear();
            bank.inst_unks(i, &mut unks);
            if switches.iter().any(|w| w == bank.inst_name(i)) {
                unks.truncate(bank.terminals());
            }
            let mut joined = unks.iter().copied().filter(|&u| u != 0);
            let Some(first) = joined.next() else { continue };
            for u in joined {
                let (a, b) = (find(&mut root, first), find(&mut root, u));
                root[a as usize] = b;
            }
        }
    }
    let (mut read, mut export) = (vec![false; n], vec![false; n]);
    for &name in reads {
        for u in unks_of(s, name).into_iter().filter(|&u| u != 0) {
            read[find(&mut root, u) as usize] = true;
        }
    }
    for &u in exports {
        export[find(&mut root, u) as usize] = true;
    }
    (0..n as u32).map(|u| {
        let r = find(&mut root, u) as usize;
        read[r] && export[r]
    }).collect()
}

/// The switches (`Bank::hold_position`): those a person sets, and the circuit's own. One a person
/// sets reads its control off nodes that only constant independent voltage sources drive (a key's
/// or a button's press, a value an edit sets) and that nothing else reads but switches' controls;
/// a switch the circuit's own signal drives (an oscillator's, a comparator's) is the circuit's.
pub(crate) fn switches(ckt: &SimCircuit, s: &Solver) -> (Vec<String>, Vec<String>) {
    let set: Vec<&str> = ckt.devices.v.iter().filter(|v| matches!(v.src.wave, crate::circuit::Wave::None)).map(|v| v.name.as_str()).collect();
    // per instance whether it takes positions, on a copy; per unknown what touches it, and where
    let mut probe = s.banks.clone();
    let switch: Vec<Vec<bool>> = probe.iter_mut().map(|bank| (0..bank.len()).map(|i| bank.hold_position(i, Some(true))).collect()).collect();
    let mut touches: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); s.x.len()];
    let mut unks = Vec::new();
    for (b, bank) in s.banks.iter().enumerate() {
        for i in 0..bank.len() {
            unks.clear();
            bank.inst_unks(i, &mut unks);
            for (j, &u) in unks.iter().enumerate().filter(|e| *e.1 != 0) {
                touches[u as usize].push((b, i, j));
            }
        }
    }
    let read = |b: usize, i: usize, j: usize| switch[b][i] && j >= s.banks[b].terminals();
    let set_by = |b: usize, i: usize, j: usize| j < s.banks[b].terminals() && set.contains(&s.banks[b].inst_name(i));
    let (mut out, mut own) = (Vec::new(), Vec::new());
    for (b, bank) in s.banks.iter().enumerate() {
        for i in (0..bank.len()).filter(|&i| switch[b][i]) {
            unks.clear();
            bank.inst_unks(i, &mut unks);
            let control: Vec<u32> = unks[bank.terminals().min(unks.len())..].iter().copied().filter(|&u| u != 0).collect();
            let user = !control.is_empty() && control.iter().all(|&u| {
                let t = &touches[u as usize];
                t.iter().all(|&(b, i, j)| read(b, i, j) || set_by(b, i, j)) && t.iter().any(|&(b, i, j)| set_by(b, i, j))
            });
            if user {
                out.push(bank.inst_name(i).to_string());
            } else {
                own.push(bank.inst_name(i).to_string());
            }
        }
    }
    (out, own)
}

/// A copy of `s` with the switches `which` held closed (`Some(true)`), open, or each in the position
/// other than its present one (`None`, `Bank::hold_position`); None when `s` holds none of them.
pub(crate) fn hold_switches(s: &Solver, which: &[String], closed: Option<bool>) -> Option<Solver> {
    let mut held = s.clone();
    let mut any = false;
    for w in which {
        if let Some((b, i)) = held.find_device(w) {
            any |= held.banks[b].hold_position(i, closed);
        }
    }
    any.then_some(held)
}

/// `s` with the switches `which` each in its other position, stepped from its state at `t` for
/// `span`: what a flip moves faster than that settles where it takes it (a pressed key's row onto
/// its column). None when `s` holds none of them or a step fails.
pub(crate) fn flipped(s: &Solver, which: &[String], t: f64, span: f64) -> Option<Solver> {
    stepped(hold_switches(s, which, None)?, t, span)
}

/// `f` stepped from its state at `t` for `span`, as a change just made to it moves it (a switch
/// flipped, a pad written). None when a step fails.
pub(crate) fn stepped(mut f: Solver, t: f64, span: f64) -> Option<Solver> {
    let mut st = Stepper::resume(&mut f, t, span);
    while st.t() < t + span {
        st.step(&mut f, t + span, &mut |_: &Solver, _: &Accepted| {}).ok()?;
    }
    Some(f)
}

/// Whether a landing at `t` was only on an imported event: a boundary source's breakpoint and no
/// breakpoint of the block's own there. Such a point is not exported as an event: its importers
/// would land on it and, through a round-lagged coupling back, return it a round later, and again
/// every round after, each edge a train that never ends (on drum16 every block's stepping grew
/// with the run). Interpolation reads the point like any other. A crossing has no breakpoint and
/// stays an event. A coupling the plan lands a jump through (`RELAYED`) passes a relayed switching
/// instant on as an edge and a relayed corner as a plain event.
fn relayed(s: &Solver, t: f64) -> bool {
    let before = t.next_down();
    let (mut fed, mut own) = (false, false);
    for b in &s.banks {
        if b.next_break(before) == t {
            if b.kind() == "boundary" {
                fed = true;
            } else {
                own = true;
            }
        }
    }
    fed && !own
}

const EVENT: u8 = 1;
const COARSE: u8 = 2;
/// The first point after a restart (`Stepper::restart`: a pin write, an edit): a value may jump
/// between the restart's instant and it.
const JUMP: u8 = 4;
/// The point is a crossing of the block's own.
const CROSSING: u8 = 8;
/// The point was a landing on an imported event alone (`relayed`): an event only in the windows of
/// the couplings the plan lands a jump through (`RoundPlan::lands`), and there only where the
/// value moves to the next point. A relayed switching instant passes on as an edge (`RELAYED_EDGE`),
/// a relayed corner as a plain event. The plan lands no such chain that closes a loop (`charge_hold`).
const RELAYED: u8 = 16;
/// The point landed on imported switching instants alone: where the export relays it, it passes
/// on as an edge so its readers restart as at a switching instant.
const RELAYED_EDGE: u8 = 32;

/// A block's recorded points.
#[derive(Clone, Default)]
struct Rec {
    t: Vec<f64>,
    flags: Vec<u8>,
    /// Per point its own unknowns' values (`Run::own` order), its pins' currents and its exports.
    vals: Vec<f64>,
    pin_i: Vec<f64>,
    exp: Vec<f64>,
    /// Its edge points that carry the end of a source edge their jump ran over (`Accepted::edge_end`):
    /// the point and the end, in point order.
    ends: Vec<(u32, f64)>,
    /// This buffer records pin currents.
    pins: bool,
}

/// One block's buffers in export order and the point the front buffer starts from.
#[derive(Clone)]
struct Merge {
    own: Vec<u32>,
    set: usize,
    pin_ids: Vec<u32>,
    queue: VecDeque<Rec>,
    spares: Vec<Rec>,
    t0: f64,
    vals0: Vec<f64>,
    pins0: Vec<f64>,
    pins: bool,
    /// The next point in the front buffer.
    cur: usize,
}

impl Merge {
    fn next_time(&self) -> Option<f64> {
        self.queue.front()?.t.get(self.cur).copied()
    }

    /// Recycles exhausted front buffers, leaving at least `keep` of the queued buffers in place.
    fn recycle(&mut self, keep: usize) {
        while self.queue.len() > keep && self.cur == self.queue[0].t.len() {
            let mut rec = self.queue.pop_front().expect("the front buffer is exhausted");
            let (k, p) = (self.own.len(), self.pin_ids.len());
            let cur = self.cur;
            self.t0 = rec.t[cur - 1];
            self.vals0.copy_from_slice(&rec.vals[(cur - 1) * k..cur * k]);
            if self.pins {
                self.pins0.copy_from_slice(&rec.pin_i[(cur - 1) * p..cur * p]);
            }
            rec.t.clear();
            rec.flags.clear();
            rec.vals.clear();
            rec.pin_i.clear();
            rec.exp.clear();
            rec.ends.clear();
            self.spares.push(rec);
            self.cur = 0;
        }
    }
}

#[derive(Clone)]
struct MergeSide {
    /// The merge's records in block order.
    merge: Vec<Merge>,
    sums: Vec<u32>,
    /// The preceding merged point and its owning blocks in order, empty between rounds.
    prev_point: f64,
    point_owners: Vec<u32>,
    close_points: u64,
    close_gap: [u64; 3],
    close_same_block: u64,
    close_pairs: BTreeMap<(u32, u32), u64>,
    /// The merge goes no further than this (`Rounds::mark_keep`): a kept snapshot's round end.
    stop: f64,
}

/// Whole-circuit points from one posted merge, in delivery order.
#[derive(Clone, Default)]
pub(crate) struct MergeRows {
    pub(crate) rounds_id: u64,
    pub(crate) t: Vec<f64>,
    pub(crate) flags: Vec<u8>,
    pub(crate) x: Vec<f64>,
    pub(crate) pin_i: Vec<f64>,
}

impl MergeRows {
    pub(crate) fn clear(&mut self) {
        self.t.clear();
        self.flags.clear();
        self.x.clear();
        self.pin_i.clear();
    }
}

struct MergeJob {
    side: Box<MergeSide>,
    x: Vec<f64>,
    pin_i: Vec<f64>,
    rows: MergeRows,
    run_ns: u64,
}

impl MergeJob {
    fn run(&mut self) {
        let at = Instant::now();
        while let Some((t, flags)) = self.side.next_point(&mut self.x, &mut self.pin_i) {
            self.rows.t.push(t);
            self.rows.flags.push(flags);
            self.rows.x.extend_from_slice(&self.x);
            self.rows.pin_i.extend_from_slice(&self.pin_i);
        }
        self.side.end_merge();
        self.run_ns = at.elapsed().as_nanos() as u64;
    }

    fn lost() -> SimDiag {
        SimDiag::setup("a round's merge was lost on a helper core (it panicked)".into())
    }
}

struct MergeSlot {
    claimed: AtomicBool,
    job: Mutex<Option<MergeJob>>,
}

impl MergeSlot {
    fn claim(&self) -> Option<MergeJob> {
        if self.claimed.swap(true, Ordering::AcqRel) {
            return None;
        }
        self.job.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

struct MergeFlight {
    slot: Arc<MergeSlot>,
    rx: mpsc::Receiver<Option<MergeJob>>,
}

/// A block running: its solver beside its stepper, its map, its pins and its record.
#[derive(Clone)]
pub(crate) struct Run {
    pub(crate) s: Solver,
    pub(crate) st: Stepper,
    pub(crate) own: Vec<(u32, u32)>,
    /// The first `set` of `own` are its alone; the rest are shares of a sum (`RoundPlan::sums`).
    set: usize,
    taps: Taps,
    /// The plan's pins with a terminal in this block.
    pins: Vec<u32>,
    pin_scratch: Vec<f64>,
    /// The local unknowns other blocks read, by its exports' order (`Rounds::exports`).
    exports: Vec<u32>,
    rec: Rec,
    /// Its wall a round, ns, smoothed: what orders it among its stage's (`Rounds::order`).
    cost: f64,
    first_ns: u64,
    first_n: u64,
    later_ns: u64,
    later_n: u64,
    worker_ns: u64,
    worker_rounds: u64,
    helper_ns: u64,
    helper_rounds: u64,
    /// `Stepper::restarts.change` at its newest point.
    changes: u64,
    /// A change between steps that is not a restart (a pin write with a ramp): its next point is
    /// recorded as after a restart.
    pub(crate) kink: bool,
    /// An edit reached it: its history is cut before it steps on.
    pub(crate) edited: bool,
    /// The outcome of its last round.
    failed: Option<SimDiag>,
    /// Its next round is to run again (`Shadows`): it takes itself as the round begins, on the
    /// core that runs it, into `shadow` with the wall that took.
    shadow_take: bool,
    shadow: Option<(Box<Run>, u64)>,
    /// Its reads of hub nodes it returns its current into, as fed for its round (`HubRead`).
    hub: Vec<HubRead>,
}

impl Run {
    pub(crate) fn new(s: Solver, st: Stepper, own: Vec<(u32, u32)>, taps: Taps, pins: usize) -> Run {
        let ids: Vec<u32> = taps.pins.iter().enumerate().filter(|(_, &(_, n))| n > 0).map(|(p, _)| p as u32).collect();
        let cost = 1e3 * own.len() as f64;
        let (changes, set) = (st.restarts.change, own.len());
        Run {
            s, st, own, set, taps, pins: ids, pin_scratch: vec![0.0; pins], exports: Vec::new(), rec: Rec::default(), cost,
            first_ns: 0, first_n: 0, later_ns: 0, later_n: 0, worker_ns: 0, worker_rounds: 0, helper_ns: 0, helper_rounds: 0,
            changes, kink: false, edited: false, failed: None, shadow_take: false, shadow: None, hub: Vec::new(),
        }
    }

    /// A running block a new plan keeps (`Rounds::same_blocks`): its solver and stepper whole (its
    /// history, factored and compiled LU and warm start), and what an edit left it to do; the rest
    /// as a new block's, for the new plan's own, taps and pins.
    pub(crate) fn kept(old: Run, own: Vec<(u32, u32)>, taps: Taps, pins: usize) -> Run {
        let Run { s, st, kink, edited, .. } = old;
        Run { kink, edited, ..Run::new(s, st, own, taps, pins) }
    }

    /// One `Stepper::step` (at most to `t_end`), its points recorded; with hub reads (fed when
    /// asked, `Rounds::hub_screen`) through the copy that screens them at each point.
    pub(crate) fn step(&mut self, t_end: f64) -> Result<(), SimDiag> {
        if !self.hub.is_empty() {
            return self.step_screened(t_end);
        }
        self.step_with::<false>(t_end)
    }

    /// `step` with the hub reads screened, cold and out of line (`Hub::read`).
    #[cold]
    #[inline(never)]
    fn step_screened(&mut self, t_end: f64) -> Result<(), SimDiag> {
        self.step_with::<true>(t_end)
    }

    /// One `Stepper::step`, its points recorded and, with `SCREEN`, the hub reads screened at each.
    fn step_with<const SCREEN: bool>(&mut self, t_end: f64) -> Result<(), SimDiag> {
        let Run { s, st, own, taps, pins, pin_scratch, exports, rec, changes, kink, hub, .. } = self;
        let with_pins = rec.pins;
        let now = st.restarts.change;
        let crossings = st.restarts.crossing;
        let stepped = st.step(s, t_end, &mut |sv: &Solver, a: &Accepted| {
            let jump = (std::mem::replace(changes, now) != now) | std::mem::take(kink);
            rec.t.push(a.t);
            if a.edge_end.is_finite() {
                rec.ends.push(((rec.t.len() - 1) as u32, a.edge_end));
            }
            let fed = a.event && relayed(sv, a.t);
            let fed_edge = fed && crate::analysis::tran::edge_only(sv, a.t);
            rec.flags.push((u8::from(a.event && !fed) * EVENT) | (u8::from(fed) * RELAYED) | (u8::from(fed_edge) * RELAYED_EDGE) | (u8::from(a.coarse) * COARSE) | (u8::from(jump) * JUMP));
            rec.vals.extend(own.iter().map(|&(l, _)| sv.x[l as usize]));
            rec.exp.extend(exports.iter().map(|&l| sv.x[l as usize]));
            if SCREEN {
                HubRead::screen(hub, &sv.x, a.t);
            }
            if with_pins {
                taps.currents(sv, a.xdot, a.t, pin_scratch);
                rec.pin_i.extend(pins.iter().map(|&p| pin_scratch[p as usize]));
            }
        });
        if st.restarts.crossing != crossings {
            if let Some(f) = rec.flags.last_mut() {
                *f |= CROSSING;
            }
        }
        stepped
    }

    /// The round to `end`, timed into its cost; taken first, to run again, when it is to be
    /// (`shadow_take`).
    fn round(&mut self, end: f64, helper: bool) {
        if std::mem::take(&mut self.shadow_take) {
            let at = Instant::now();
            let mut copy = Box::new(self.clone());
            (copy.rec, copy.shadow) = (Rec::default(), None);
            self.shadow = Some((copy, at.elapsed().as_nanos() as u64));
        }
        let start = Instant::now();
        let (mut steps, mut first_ns) = (0, 0);
        while self.failed.is_none() && self.st.t() < end {
            if let Err(d) = self.step(end) {
                self.failed = Some(d);
            }
            steps += 1;
            if steps == 1 {
                first_ns = start.elapsed().as_nanos() as u64;
            }
        }
        let wall = start.elapsed().as_nanos();
        self.cost = 0.75 * self.cost + 0.25 * wall as f64;
        let ns = wall as u64;
        if steps > 0 {
            self.first_n += 1;
            self.first_ns += first_ns;
        }
        if steps > 1 {
            self.later_n += steps - 1;
            self.later_ns += ns.saturating_sub(first_ns);
        }
        if helper {
            self.helper_rounds += 1;
            self.helper_ns += ns;
        } else {
            self.worker_rounds += 1;
            self.worker_ns += ns;
        }
    }

    /// Its pins' currents at its newest point.
    fn currents_now(&mut self) -> &[f64] {
        let Run { s, st, taps, pin_scratch, .. } = self;
        let newest = st.hist.newest();
        taps.currents(s, &newest.xdot, newest.t, pin_scratch);
        pin_scratch
    }
}

/// The stages and rounds posted at the open round's start, and their blocks to collect at its
/// close: shared with `joiners` jobs posted, the last block done signalled on `rx`.
struct Flight {
    ran: Vec<(usize, u64)>,
    share: Arc<Share>,
    joiners: usize,
    rx: mpsc::Receiver<()>,
}

/// A copy of the rounds is made between rounds, when all posted work is back.
struct InFlight<T>(Option<T>);

impl<T> Clone for InFlight<T> {
    fn clone(&self) -> Self {
        debug_assert!(self.0.is_none(), "rounds are copied between rounds");
        InFlight(None)
    }
}

/// A block shared for its round: its number, its run, and the end of its round (blocks of several
/// stages posted together can end their rounds apart).
type Shared = (u32, Box<Run>, f64);

/// A flight's blocks, heaviest first, shared by its joiners (jobs on the helpers) and the worker.
/// Each takes one block at a time until none is left: a joiner from the heavy end, the worker from
/// the light end while joiners run the stages behind block 0 (`Rounds::land`), or from the heavy
/// end in a stage run in its turn, where it starts first (`Rounds::run_stage`). The untaken blocks
/// are a range in one atomic word; a block waits in its cell, is taken out to run and goes back
/// when run. The joiner that finishes the last block signals the flight's channel, so the worker
/// waits once at most, and only for blocks a helper is running.
struct Share {
    cells: Vec<Mutex<Option<Shared>>>,
    /// The untaken blocks: the first's cell in the low half, one past the last's in the high.
    ends: AtomicU64,
    /// Blocks run (or lost to a panic on a helper).
    done: AtomicUsize,
    /// Joiners that took a block.
    joined: AtomicU64,
    /// Of those: from post to first block, summed and the most, and their blocks' wall.
    wake_ns: AtomicU64,
    wake_max_ns: AtomicU64,
    run_ns: AtomicU64,
}

impl Share {
    fn new(blocks: Vec<Shared>) -> Arc<Share> {
        let n = blocks.len() as u64;
        Arc::new(Share {
            cells: blocks.into_iter().map(|b| Mutex::new(Some(b))).collect(),
            ends: AtomicU64::new(n << 32),
            done: AtomicUsize::new(0),
            joined: AtomicU64::new(0),
            wake_ns: AtomicU64::new(0),
            wake_max_ns: AtomicU64::new(0),
            run_ns: AtomicU64::new(0),
        })
    }

    /// The cell of the next untaken block from the heavy end or the light; None when every block
    /// is taken. One compare-and-swap, retried while others take.
    fn take(&self, heavy: bool) -> Option<usize> {
        let mut cur = self.ends.load(Ordering::Acquire);
        loop {
            let (first, past) = (cur & 0xffff_ffff, cur >> 32);
            if first >= past {
                return None;
            }
            let (next, i) = if heavy { (cur + 1, first) } else { (cur - (1 << 32), past - 1) };
            match self.ends.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(i as usize),
                Err(now) => cur = now,
            }
        }
    }

    /// A taken block done: whether it was the last.
    fn finish(&self) -> bool {
        self.done.fetch_add(1, Ordering::AcqRel) + 1 == self.cells.len()
    }

    /// Posts a joiner: on a helper it takes blocks from the heavy end until none is left, each run
    /// in its cell (a panic loses the block, which the worker finds missing), and the one that
    /// finishes the last block signals `tx`.
    fn post(share: &Arc<Share>, h: &Helpers, tx: &mpsc::Sender<()>) {
        let share = share.clone();
        let tx = tx.clone();
        let posted = Instant::now();
        (h.post)(Box::new(move || {
            let mut first = true;
            while let Some(i) = share.take(true) {
                if std::mem::take(&mut first) {
                    let wake_ns = posted.elapsed().as_nanos() as u64;
                    share.joined.fetch_add(1, Ordering::Relaxed);
                    share.wake_ns.fetch_add(wake_ns, Ordering::Relaxed);
                    share.wake_max_ns.fetch_max(wake_ns, Ordering::Relaxed);
                }
                let cell = &share.cells[i];
                let block = cell.lock().unwrap_or_else(|e| e.into_inner()).take();
                let run_at = Instant::now();
                let back = block.and_then(|(k, mut r, end)| catch_unwind(AssertUnwindSafe(move || {
                    r.round(end, true);
                    (k, r, end)
                })).ok());
                share.run_ns.fetch_add(run_at.elapsed().as_nanos() as u64, Ordering::Relaxed);
                *cell.lock().unwrap_or_else(|e| e.into_inner()) = back;
                if share.finish() {
                    let _ = tx.send(());
                }
            }
        }));
    }
}

/// What the posted joiners did, for `LiveStats`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PackCount {
    /// Posted joiners that ran a block on a helper.
    pub(crate) helped: u64,
    /// Posted joiners that found every block taken: the worker or another joiner ran their share.
    pub(crate) claimed: u64,
}

/// One block's state into the whole circuit's solver (`Rounds::compose`): the global unknowns
/// it solves (set, or added for those summed across blocks) and its instances' device states.
fn compose_run(r: &Run, whole: &mut Solver) {
    for (j, &(l, g)) in r.own.iter().enumerate() {
        if j < r.set {
            whole.x[g as usize] = r.s.x[l as usize];
        } else {
            whole.x[g as usize] += r.s.x[l as usize];
        }
    }
    copy_states(&r.s, whole);
}

/// The blocks at the end of round `m` composed into the whole circuit's solver as each stage
/// passes that round (`Rounds::mark`); `passed` counts the later stages in, in their order.
#[derive(Clone)]
struct Marker {
    m: u64,
    whole: Solver,
    passed: usize,
}

/// A kept snapshot of the rounds at the end of round `m` (time `t`), its blocks and windows
/// taken as each stage passes that round (`Rounds::mark_keep`); `passed` counts the later stages
/// in, `bytes` what it took so far.
#[derive(Clone)]
struct KeepMarker {
    m: u64,
    t: f64,
    rounds: Box<Rounds>,
    passed: usize,
    bytes: u64,
}

/// A run's blocks in rounds.
#[derive(Clone)]
pub(crate) struct Rounds {
    /// In block order; a block is out of its place only while a flight shares it.
    runs: Vec<Option<Box<Run>>>,
    pub(crate) id: u64,
    side: Option<Box<MergeSide>>,
    pins: bool,
    merge_flight: InFlight<MergeFlight>,
    rows: MergeRows,
    merge_x: Vec<f64>,
    merge_pin_i: Vec<f64>,
    /// The blocks of each stage after the control block's.
    stages: Vec<Vec<u32>>,
    /// Per stage in `stages`: its place in the pipeline among the later stages
    /// (`RoundPlan::slot` less one). A stage in the place of the one before runs right after it,
    /// in the same round (`land`).
    slot: Vec<usize>,
    /// Rounds block 0 has finished that a later stage still needs, oldest first (number, start, end).
    queue: VecDeque<(u64, f64, f64, Arc<[bool]>)>,
    /// The number of rounds block 0 has finished.
    ctl_done: u64,
    /// The last round each later stage finished.
    done: Vec<u64>,
    flight: InFlight<Flight>,
    /// Block 0's recent round ends, used to cut a lagged read at the exporter's round end.
    ends: VecDeque<(u64, f64)>,
    start: f64,
    /// A failure while catching up between rounds, reported at the next close.
    pub(crate) failed: Option<SimDiag>,
    /// The round's length, seconds: a whole number of sample periods.
    pub(crate) round: f64,
    pub(crate) shuffle: Option<u64>,
    /// Where its shadow rounds and chain trees run (`LiveSim::set_checks`); none: on the worker.
    pub(crate) checks: Option<Checks>,
    /// The joiners posted at once this round so far (the most of one stage's or the stages' behind
    /// block 0) and whether it posted a merge; the round before's sum, which bounds its checks
    /// (`Checks`). A new plan starts from all its helpers (`LiveSim::install`).
    posted: (usize, bool),
    pub(crate) packs_last: usize,
    /// Use stage-dependent lags and post later stages behind block 0 when helpers are available.
    pub(crate) pipeline: bool,
    /// Mark the exporting block's crossings, and feed the readers its jumps split onto their grids
    /// (`Table::split`).
    pub(crate) edges: bool,
    /// Every round's hub reads screened (`LiveSpec::hub_screen`); off, only the rounds the merge
    /// gate's take asks for (none yet).
    pub(crate) hub_screen: bool,
    /// Each block on its own step bound (`LiveSpec::own_bound`), and per block whether it is on one
    /// longer than the sample period in its open round.
    pub(crate) own_bound: Option<OwnBound>,
    own: Vec<bool>,
    /// Per block, the grid it was last fed on (`feed`): a holder's port reads on the holder's
    /// grid while the holder runs on a helper (`port_tables`).
    grids: Vec<f64>,
    /// The step cap and the sample period a block off its own bound steps on (`renew_bounds`).
    plain: Option<(f64, f64)>,
    /// What the sinks hear at the sample rate (`Sink::heard`, sorted) as last told (`hear`), and per
    /// block whether it is heard (it solves one of them, or a heard block reads it): it then steps
    /// on the sample period. A later stage's round takes what was heard as block 0 opened that
    /// round (`queue`).
    heard: Vec<u32>,
    heard_by: Arc<[bool]>,
    /// Rounds run.
    pub(crate) n: u64,
    couplings: Vec<Coupling>,
    /// Each reader's delay in seconds, in the order of `Coupling::to`.
    delays: Vec<Vec<f64>>,
    windows: Vec<Window>,
    /// Per block, the couplings it exports (its `Run::exports` order) and those it reads.
    exports: Vec<Vec<u32>>,
    imports: Vec<Vec<u32>>,
    /// The open round's start and its end on the grid (`end_after`): block 0 steps no further.
    t0: f64,
    pub(crate) end: f64,
    /// The global unknowns summed when composing the blocks into the whole solver.
    sums: Vec<u32>,
    /// The plan it runs, its solvers aside (`Rounds::runs_cut`).
    cut: RoundPlan,
    /// Per block, per unknown: on a loop of the plan's round map (`on_loop`).
    loops: Vec<Vec<bool>>,
    /// Per block: an edit since its last proof reached a device of it on a loop (`reprove`).
    reprove: Vec<bool>,
    /// A snapshot of every block at the end of one round, taken as each stage passes it (`mark`).
    marker: Option<Box<Marker>>,
    /// A kept snapshot of every block at the end of one round, taken as each stage passes it
    /// (`mark_keep`).
    keep: Option<Box<KeepMarker>>,
    /// Per block, what makes it the same block in another plan.
    sigs: Vec<BlockSig>,
    /// The largest voltage a value may reach in rounds (`LiveSim::install`: a generous multiple of
    /// the circuit's largest source); past it, or not a number, is a breach. Infinite for a plan
    /// that reads nothing late, whose exports go unchecked: the rule refuses a late read, and with
    /// none what a block exports is the circuit's own, as stepped whole.
    pub(crate) hold: f64,
    /// The first value exported past `hold` or not a number (`export`), for the run's growth rule.
    pub(crate) breach: Option<Breach>,
    /// The hub ports' late terms and their readers' crossings (`Hub`).
    hub: Hub,
    /// Per coupling: whether it carries a voltage (`hold` bounds those; every value must be a
    /// number), and per reader in its `to` the node that reader's read is refused by: the cut it
    /// reads it across, else the exported unknown.
    voltage: Vec<bool>,
    late_at: Vec<Vec<String>>,
    /// The shadow rounds (`Shadows`).
    shadows: Shadows,
}

impl Rounds {
    /// The rounds of a checked plan (`check`), its blocks standing at one instant. Every boundary
    /// source is fed its export's present value; false names one that takes no feed. With `carry`
    /// (from the running rounds, and per block the running block it keeps: `same_blocks`), a kept
    /// block's couplings keep their histories, and the round count goes on where block 0's kept
    /// round ends reach the plan's latest read (else it starts again, as for a run stepping whole).
    pub(crate) fn new(plan: &RoundPlan, mut runs: Vec<Run>, round: f64, pipeline: bool, carry: Option<(Carry, &[Option<usize>])>) -> Result<Rounds, SimDiag> {
        let mut stages: Vec<Vec<u32>> = Vec::new();
        for (k, b) in plan.blocks.iter().enumerate().skip(1) {
            if k == 1 || plan.blocks[k - 1].stage != b.stage {
                stages.push(Vec::new());
            }
            stages.last_mut().expect("a stage was opened").push(k as u32);
        }
        let slot: Vec<usize> = stages.iter().map(|ids| plan.slot(plan.blocks[ids[0] as usize].stage) as usize - 1).collect();
        // a sum's shares go last in each block's own, after what it solves alone
        for (g, _) in &plan.sums {
            for r in &mut runs {
                r.own.retain(|&(_, o)| o != *g);
            }
        }
        for r in &mut runs {
            r.set = r.own.len();
        }
        // a block kept from the cut before runs on with its ports as the plan fitted them now
        for p in &plan.ports {
            let r = &mut runs[p.block as usize];
            if port_into(&mut r.s, p) {
                r.st.restart(&r.s);
            }
        }
        for (g, shares) in &plan.sums {
            for &(b, l) in shares {
                runs[b as usize].own.push((l, *g));
            }
        }
        let merge = runs.iter().map(|r| Merge {
            own: r.own.iter().map(|&(_, g)| g).collect(), set: r.set, pin_ids: r.pins.clone(),
            queue: VecDeque::new(), spares: Vec::new(), t0: r.st.t(),
            vals0: r.own.iter().map(|&(l, _)| r.s.x[l as usize]).collect(),
            pins0: vec![0.0; r.pins.len()], pins: false, cur: 0,
        }).collect();
        let (mut exports, mut imports) = (vec![Vec::new(); runs.len()], vec![Vec::new(); runs.len()]);
        let mut windows = Vec::with_capacity(plan.couplings.len());
        for (c, k) in plan.couplings.iter().enumerate() {
            let r = &mut runs[k.from.0 as usize];
            r.exports.push(k.from.1);
            exports[k.from.0 as usize].push(c as u32);
            let mut w = Window::default();
            w.push(r.st.t(), r.s.x[k.from.1 as usize], false, false);
            windows.push(w);
            for (to, _) in &k.to {
                if !imports[*to as usize].contains(&(c as u32)) {
                    imports[*to as usize].push(c as u32);
                }
            }
        }
        let loops = runs.iter().enumerate().map(|(k, r)| {
            let reads: Vec<&str> = plan.couplings.iter().flat_map(|c| &c.to).filter(|t| t.0 as usize == k).map(|t| t.1.as_str()).collect();
            on_loop(&r.s, &reads, &r.exports, &plan.switches)
        }).collect();
        let t0 = runs.first().map_or(0.0, |r| r.st.t());
        let sums: Vec<u32> = plan.sums.iter().map(|(g, _)| *g).collect();
        let side = MergeSide {
            merge, sums: sums.clone(), prev_point: t0, point_owners: Vec::new(),
            close_points: 0, close_gap: [0; 3], close_same_block: 0, close_pairs: BTreeMap::new(),
            stop: f64::INFINITY,
        };
        let delays = plan.couplings.iter().map(|k| k.to.iter().map(|&(to, _)| match k.lag {
            Lag::Same => 0.0,
            Lag::Round => round * plan.lag_rounds(k.from.0 as usize, to as usize, pipeline) as f64,
        }).collect()).collect();
        let sigs = runs.iter().map(|r| BlockSig::of(&r.s)).collect();
        let unknown = |k: &Coupling| &runs[k.from.0 as usize].s.unknowns[k.from.1 as usize];
        let voltage = plan.couplings.iter().map(|k| unknown(k).kind == UnkKind::Voltage).collect();
        let late_at = plan.couplings.iter().map(|k| k.to.iter().map(|&(to, _)| k.cut_of(to).and_then(|c| plan.cut_nodes.get(&c).cloned()).unwrap_or_else(|| unknown(k).name.clone())).collect()).collect();
        let readers = (0..runs.len() as u32).filter(|&b| imports[b as usize].iter().any(|&c| plan.couplings[c as usize].lag == Lag::Round)).collect();
        let range = windows.iter().map(|w| (w.v[0], w.v[0])).collect();
        let hub = Hub::new(&plan.ports, &runs);
        let mut rounds = Rounds {
            runs: runs.into_iter().map(|r| Some(Box::new(r))).collect(), done: vec![0; stages.len()], stages, slot,
            id: 0, side: Some(Box::new(side)), pins: false, merge_flight: InFlight(None),
            rows: MergeRows::default(), merge_x: Vec::new(), merge_pin_i: Vec::new(),
            queue: VecDeque::new(), ctl_done: 0, flight: InFlight(None), ends: VecDeque::new(), start: t0, failed: None,
            round, shuffle: None, checks: None, posted: (0, false), packs_last: 0, pipeline, edges: false, hub_screen: false, own_bound: None,
            own: vec![false; plan.blocks.len()], grids: vec![0.0; plan.blocks.len()], plain: None, heard: Vec::new(), heard_by: vec![false; plan.blocks.len()].into(), n: 0,
            couplings: plan.couplings.clone(), delays, windows, exports, imports, t0, end: t0,
            sums, cut: plan.clone(),
            loops, reprove: vec![false; plan.blocks.len()], marker: None, keep: None, sigs,
            hold: f64::INFINITY, breach: None, voltage, late_at, shadows: Shadows::new(readers, range, plan.chains.len()), hub,
        };
        if let Some((carry, same)) = carry {
            let Carry { ctl_done, ends, start, windows, chains } = carry;
            // a chain tree whose born export, roots and members the plan keeps carries on its
            // counts: the first jumps are for a tree new to the run (Fable's chain-shadow answer)
            let old = |r: u32| same.get(r as usize).copied().flatten().map(|j| j as u32);
            for (k, ch) in plan.chains.iter().enumerate() {
                let from = plan.couplings[ch.born as usize].from;
                let born = old(from.0).map(|e| (e, from.1));
                let roots: Option<Vec<u32>> = ch.roots.iter().map(|&r| old(r)).collect();
                let members: Option<Vec<u32>> = ch.members.iter().map(|&r| old(r)).collect();
                if let Some(c) = chains.iter().find(|c| Some(c.0) == born && roots.as_ref() == Some(&c.1) && members.as_ref() == Some(&c.2)) {
                    rounds.shadows.seen[k] = c.3.clone();
                    rounds.shadows.fresh = (rounds.shadows.fresh.0 + 1, rounds.shadows.fresh.1 - 1);
                }
            }
            for (c, k) in plan.couplings.iter().enumerate() {
                let Some(j) = same.get(k.from.0 as usize).copied().flatten() else { continue };
                if let Some((_, w)) = windows.iter().find(|(from, _)| *from == (j as u32, k.from.1)) {
                    rounds.windows[c] = w.clone();
                }
            }
            let latest = rounds.delays.iter().flatten().fold(0.0f64, |a, &d| a.max(d));
            let need = (ctl_done + 1).saturating_sub((latest / round).round() as u64);
            if !pipeline || need == 0 || ends.front().is_some_and(|e| e.0 <= need) {
                rounds.done = vec![ctl_done; rounds.stages.len()];
                (rounds.ctl_done, rounds.ends, rounds.start) = (ctl_done, ends, start);
            }
        }
        rounds.shadows.burst_to = rounds.ctl_done + SHADOW_BURST;
        for b in 0..rounds.runs.len() {
            if let Some(name) = rounds.feed(b, t0, f64::INFINITY, rounds.ctl_done + 1, None) {
                return Err(SimDiag::setup(format!("round plan: {name} in block {b} takes no table: not a boundary source")));
            }
        }
        Ok(rounds)
    }

    /// Block `b`'s holder ports (`HolderPort`), each with the table its holder reads its return
    /// with in the round from `t0` and the delay: the holder's own read (`read_table`), at the
    /// holder's delay, grid, landing and crossings, so at rest the port's value is the branch
    /// current to the bit and the port carries nothing. A holder on a helper has its grid as last
    /// fed (`grids`). O(the ports' points).
    fn port_tables(&self, b: usize, t0: f64, end: f64, m: u64) -> Vec<(String, Arc<Table>, f64)> {
        self.cut.ports.iter().filter(|p| p.block as usize == b).filter_map(|p| {
            let c = p.coupling;
            let k = &self.couplings[c as usize];
            let (holder, _) = *k.to.first()?;
            let delay = *self.delays[c as usize].first()?;
            let grid = match self.runs[holder as usize].as_deref() {
                Some(run) if self.edges => run.st.grid(),
                Some(_) => 0.0,
                None => self.grids[holder as usize],
            };
            let lands = self.cut.lands.binary_search(&(c, holder)).is_ok();
            let read = Read { pipeline: self.pipeline, round: self.round, ends: &self.ends, start: self.start };
            let (points, _) = read.table(&self.windows[c as usize], t0, end, m, delay, grid, lands, self.own[holder as usize]);
            Some((p.name.clone(), Arc::new(points), delay))
        }).collect()
    }

    /// What block `b`'s feed for round `m` takes from the shadows before it reads: whether the round
    /// is run again (`shadow_wants`) and whether a long chain tree follows it (`Shadows::follow`).
    /// O(the rounds taken).
    fn feed_takes(&self, b: usize, end: f64, m: u64) -> FeedTakes {
        let shadow = end.is_finite() && self.shadow_wants(b, m);
        let follows = end.is_finite() && self.shadows.taking.iter().any(|r| r.long > 0 && r.m < m && m <= r.m + u64::from(r.long) && r.ks.iter().any(|&k| self.cut.chains[k as usize].members.contains(&(b as u32))));
        FeedTakes { shadow, follows }
    }

    /// Feeds block `b`'s boundary sources what they can read in the round from `t0`; the name of
    /// one that takes no feed. What it takes from the shadows is decided here, or before (`takes`:
    /// block 0's, ahead of the later stages fed before it, `begin`).
    /// A round run again (`Shadows`) is taken as it is fed: block 0 here, a later block by the core
    /// that runs it.
    fn feed(&mut self, b: usize, t0: f64, end: f64, m: u64, takes: Option<FeedTakes>) -> Option<String> {
        let mut refused = None;
        let FeedTakes { shadow, follows } = takes.unwrap_or_else(|| self.feed_takes(b, end, m));
        // a block on its own bound follows its imports: their crossings are data to it
        let crossings_as_data = self.own[b];
        let ports = self.port_tables(b, t0, end, m);
        let run = self.runs[b].as_deref_mut().expect("a block is fed in its place");
        // a split reads a sample further back: the knots of a jump's line before the round
        let grid = if self.edges { run.st.grid() } else { 0.0 };
        self.grids[b] = grid;
        for (name, table, delay) in ports {
            if !run.s.feed(&name, table, delay) {
                refused.get_or_insert(name);
            }
        }
        run.hub.clear();
        let mut splits = Vec::new();
        // a long tree's member in a round it follows keeps what the run fed it (`follows`)
        let mut fed = Vec::new();
        for &c in &self.imports[b] {
            let k = &self.couplings[c as usize];
            let mut table = None;
            let mut split = false;
            let lands = self.cut.lands.binary_search(&(c, b as u32)).is_ok();
            for ((to, name), &delay) in k.to.iter().zip(&self.delays[c as usize]) {
                if *to as usize != b {
                    continue;
                }
                // Every source in this block reads this coupling with the same stage delay.
                let table = table.get_or_insert_with(|| {
                    let read = Read { pipeline: self.pipeline, round: self.round, ends: &self.ends, start: self.start };
                    let (points, jumps) = read.table(&self.windows[c as usize], t0, end, m, delay, grid, lands, crossings_as_data);
                    split = jumps > 0;
                    Arc::new(points)
                });
                if !run.s.feed(name, table.clone(), delay) {
                    refused.get_or_insert_with(|| name.clone());
                }
                if self.hub_screen {
                    self.hub.read(&mut run.hub, b, name, table, delay);
                }
                if follows {
                    fed.push((name.clone(), delay, table.clone()));
                }
            }
            // a born import's jump split, counted
            if split && end.is_finite() {
                self.shadows.jumps += 1;
                splits.push(c);
            }
        }
        if !self.imports[b].is_empty() {
            run.st.breaks_changed(&run.s);
        }
        let trees = if end.is_finite() { self.chain_round(b, t0, end, m, &splits) } else { 0 };
        if follows {
            self.shadows.follow.push(Follow { b: b as u32, m, end, feeds: fed, ran: None });
        }
        let run = self.runs[b].as_deref_mut().expect("a block is fed in its place");
        if shadow || trees > 0 {
            let taken = (b == 0).then(|| {
                let at = Instant::now();
                let mut copy = Box::new(run.clone());
                (copy.rec, copy.shadow) = (Rec::default(), None);
                (copy, at.elapsed().as_nanos() as u64)
            });
            run.shadow_take = b > 0;
            let burst = shadow && m <= self.shadows.burst_to;
            self.shadows.open.push(Open { b: b as u32, m, t0, grid, crossings_as_data, run: taken, ran: None, late: shadow, burst, trees, before: Vec::new() });
        }
        refused
    }

    /// Block `b`'s part in round `m`'s chain round (`Shadows::taking`; Fable's chain-shadow answer,
    /// Q2) as it is fed, `split` the couplings a born jump split in: of the born exports it heads
    /// (`Chain::roots`' first), each that split counts the jump; when one is due (on its first
    /// `CHAIN_FIRST` jumps after its tree's plan, or `SHADOW_EVERY` after it last
    /// joined one) the round is taken; and into a round taken every one that split joins, next due
    /// `SHADOW_EVERY` of its jumps on. An export whose tree holds block 0 where a process steps it
    /// (`shadow_ctl`) never joins. Blocks are fed in stage order, so a joined export's members are
    /// all fed after it joins, and an export a later stage heads joins only the round as it stands
    /// then (the same in every run). A marked export (`Shadows::marks`) joins no union: each of its
    /// jumps is a round of its own, taken alone. How many of round `m`'s trees hold `b`. O(trees).
    fn chain_round(&mut self, b: usize, t0: f64, end: f64, m: u64, split: &[u32]) -> u32 {
        let long = self.long_rounds();
        let sh = &mut self.shadows;
        let (mut joins, mut scheduled) = (Vec::new(), false);
        for (k, ch) in self.cut.chains.iter().enumerate() {
            if ch.roots.first() != Some(&(b as u32)) || !split.contains(&ch.born) {
                continue;
            }
            let jumps = &mut sh.seen[k];
            jumps.seen += 1;
            if !sh.ctl && ch.members.contains(&0) {
                continue;
            }
            let first = jumps.seen <= CHAIN_FIRST;
            if sh.marks[k].is_some() {
                jumps.next = jumps.seen + SHADOW_EVERY;
                sh.taking.push(ChainRound { m, t0, end, ks: vec![k as u32], first, long, alone: true });
                continue;
            }
            scheduled |= first || jumps.seen >= jumps.next;
            joins.push((k as u32, first));
        }
        if scheduled && !sh.taking.iter().any(|r| r.m == m && !r.alone) {
            sh.taking.push(ChainRound { m, t0, end, ks: Vec::new(), first: false, long, alone: false });
        }
        if let Some(round) = sh.taking.iter_mut().find(|r| r.m == m && !r.alone) {
            for (k, first) in joins {
                let jumps = &mut sh.seen[k as usize];
                round.first |= first;
                jumps.next = jumps.seen + SHADOW_EVERY;
                round.ks.push(k);
            }
        }
        let chains = &self.cut.chains;
        sh.taking.iter().filter(|r| r.m == m && r.ks.iter().any(|&k| chains[k as usize].members.contains(&(b as u32)))).count() as u32
    }

    /// The whole rounds a chain tree's member past its bar is followed past its jump round: at
    /// least `CHAIN_LONG` sample periods (the block's own bound's, else the plain step's). O(1).
    fn long_rounds(&self) -> u32 {
        let period = self.own_bound.as_ref().map(|o| o.period).or(self.plain.map(|p| p.1)).filter(|&p| p > 0.0);
        period.map_or(1, |p| (CHAIN_LONG * p / self.round - 1e-9).ceil().max(1.0) as u32)
    }

    /// Appends block `b`'s points of its round `m` to its export windows and hands the buffer to
    /// the merge. The first value not a number, or a voltage past `hold`, is the round's breach. A
    /// round to run again (`Shadows`) keeps the round as it ran, and each export its range.
    /// O(points).
    fn export(&mut self, b: usize, m: u64) {
        let run = self.runs[b].as_deref_mut().expect("a block exports in its place");
        let taken = run.shadow.take();
        if let Some(o) = self.shadows.open.iter_mut().find(|o| o.b as usize == b && o.m == m && o.ran.is_none()) {
            o.run = o.run.take().or(taken);
            o.ran = Some(Ran::of(&run.rec));
            // a chain tree's member keeps, as the round began, each export a tree passes on (an
            // export a later stage joins to the round's tree included)
            if o.trees > 0 {
                let from = o.t0 - self.round;
                for &q in &self.exports[b] {
                    if self.cut.chains.iter().any(|ch| ch.pairs.iter().any(|p| p.0 == q)) {
                        o.before.push((q, self.windows[q as usize].since(from)));
                    }
                }
            }
        }
        if let Some(f) = self.shadows.follow.iter_mut().find(|f| f.b as usize == b && f.m == m && f.ran.is_none()) {
            f.ran = Some(Arc::new(Ran::of(&run.rec)));
        }
        let (rec, n) = (&mut run.rec, run.exports.len());
        let mut breach = None;
        for (j, &c) in self.exports[b].iter().enumerate() {
            let w = &mut self.windows[c as usize];
            // a coupling the plan lands a jump through passes an imported event on
            let relays = self.cut.lands.iter().any(|&(q, _)| q == c);
            let from = w.t.last().copied().unwrap_or(f64::NEG_INFINITY);
            let hold = if self.voltage[c as usize] { self.hold } else { f64::INFINITY };
            let range = &mut self.shadows.range[c as usize];
            pass_on(w, rec, n, j, relays, self.edges, |t, v| {
                if v.is_finite() {
                    *range = (range.0.min(v), range.1.max(v));
                }
                if breach.is_none() && self.hold.is_finite() && (!v.is_finite() || v.abs() > hold) {
                    breach = Some(Breach { block: b, coupling: c, name: run.s.unknowns[self.couplings[c as usize].from.1 as usize].name.clone(), t, v, from });
                }
            });
        }
        if self.breach.is_none() {
            self.breach = breach;
        }
        // its hub reads' rounds past the screen marked (`Hub`): one figure a read, kept by the pack
        if !rec.t.is_empty() && !run.hub.is_empty() {
            self.hub.tally(&run.hub, self.hold);
        }
        if !rec.t.is_empty() {
            let m = &mut self.side.as_deref_mut().expect("the merge side is home").merge[b];
            let mut buf = m.spares.pop().unwrap_or_default();
            buf.pins = rec.pins;
            std::mem::swap(rec, &mut buf);
            m.queue.push_back(buf);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.runs.len()
    }

    pub(crate) fn ctl(&self) -> &Run {
        self.runs[0].as_deref().expect("the control block stays on the worker")
    }

    pub(crate) fn ctl_mut(&mut self) -> &mut Run {
        self.runs[0].as_deref_mut().expect("the control block stays on the worker")
    }

    /// Sets a device's parameter in every block that has it or its copy across a cut
    /// (`RoundPlan::set_param`), each such block marked to restart; true when any took.
    pub(crate) fn set_param(&mut self, device: &str, key: &str, value: f64) -> bool {
        let mut any = false;
        for (k, r) in self.runs.iter_mut().enumerate() {
            let b = r.as_deref_mut().expect("a block is in its place between rounds");
            if self.cut.set_param(k, &mut b.s, device, key, value) {
                b.edited = true;
                any = true;
                let ghosts = self.cut.ghosts.get(device).into_iter().flatten().filter(|g| g.0 as usize == k).map(|g| g.1.as_str());
                self.reprove[k] |= std::iter::once(device).chain(ghosts).any(|name| unks_of(&b.s, name).iter().any(|&u| self.loops[k][u as usize]));
            }
        }
        any
    }

    /// A device edited before the plan went in, its blocks made with the edit (`LiveSim::install`):
    /// each block that has it or its copy across a cut is due its proof again where the edit
    /// reached a loop of the round map, as `set_param` marks an edit made in rounds. O(blocks x
    /// device lookups).
    pub(crate) fn edited_before(&mut self, device: &str) {
        for (k, r) in self.runs.iter().enumerate() {
            let b = r.as_deref().expect("a block is in its place between rounds");
            let ghosts = self.cut.ghosts.get(device).into_iter().flatten().filter(|g| g.0 as usize == k).map(|g| g.1.as_str());
            self.reprove[k] |= std::iter::once(device).chain(ghosts).any(|name| unks_of(&b.s, name).iter().any(|&u| self.loops[k][u as usize]));
        }
    }

    /// What a lateness in block `b` blames (`late_at`). For a breach of `b`'s export `broke`: the
    /// values read late that close a loop through it within a round, read late by `b` or a block
    /// upstream of it in the round and exported by a block the broken value reaches in the round
    /// (`broke` itself, read late upstream of `b`, among them). Otherwise, or with no such loop, the
    /// values `b` reads late; reading none, those read late upstream of it within the round; and
    /// with none there either, every value the plan reads late. Upstream and reaches go through
    /// same-round reads. Sorted, each once; empty for a plan that reads nothing late.
    /// O(couplings).
    pub(crate) fn blame(&self, b: usize, broke: Option<u32>) -> Vec<String> {
        let late = |c: u32| self.couplings[c as usize].lag == Lag::Round;
        let reads = |k: usize| self.imports[k].iter().copied().filter(move |&c| late(c)).map(move |c| self.late_name(c, k));
        // b, and through same-round reads every block upstream of it
        let mut up = vec![false; self.imports.len()];
        up[b] = true;
        let mut todo = vec![b];
        while let Some(k) = todo.pop() {
            for &c in &self.imports[k] {
                let from = self.couplings[c as usize].from.0 as usize;
                if !late(c) && !up[from] {
                    up[from] = true;
                    todo.push(from);
                }
            }
        }
        let mut at = Vec::new();
        if let Some(broke) = broke {
            // the blocks the broken value reaches within the round: its same-round readers, theirs
            let mut reach = vec![false; self.imports.len()];
            let mut todo: Vec<u32> = vec![broke];
            while let Some(c) = todo.pop() {
                if late(c) {
                    continue;
                }
                for &(k, _) in &self.couplings[c as usize].to {
                    if !reach[k as usize] {
                        reach[k as usize] = true;
                        todo.extend_from_slice(&self.exports[k as usize]);
                    }
                }
            }
            at = (0..up.len()).filter(|&k| up[k]).flat_map(|k| self.imports[k].iter().copied().map(move |c| (c, k)))
                .filter(|&(c, _)| late(c) && (c == broke || reach[self.couplings[c as usize].from.0 as usize]))
                .map(|(c, k)| self.late_name(c, k)).collect();
        }
        if at.is_empty() {
            at = reads(b).collect();
        }
        if at.is_empty() {
            at = (0..up.len()).filter(|&k| up[k]).flat_map(reads).collect();
        }
        if at.is_empty() {
            return self.late_nodes();
        }
        at.sort();
        at.dedup();
        at
    }

    /// The node block `k`'s read of coupling `c` is refused by (`late_at`): the cut it reads it
    /// across, else the exported unknown. O(readers).
    fn late_name(&self, c: u32, k: usize) -> String {
        let (coupling, at) = (&self.couplings[c as usize], &self.late_at[c as usize]);
        let i = coupling.to.iter().position(|t| t.0 as usize == k).expect("a block's imports are couplings it reads");
        at[i].clone()
    }

    /// Every node the plan reads a value late at (`late_at`), sorted, each once: empty for a plan
    /// that reads nothing late, which the growth rule leaves unchecked (`hold`).
    pub(crate) fn late_nodes(&self) -> Vec<String> {
        let mut at: Vec<String> = self.couplings.iter().zip(&self.late_at).filter(|(k, _)| k.lag == Lag::Round).flat_map(|(_, n)| n.iter().cloned()).collect();
        at.sort();
        at.dedup();
        at
    }

    /// The first node of `refused` the plan reads a value late at (`late_at`): the run declines
    /// such a plan (`LiveSim::install`).
    pub(crate) fn reads_late_at<'a>(&self, refused: &'a [String]) -> Option<&'a str> {
        refused.iter().find(|n| self.couplings.iter().zip(&self.late_at).any(|(k, at)| k.lag == Lag::Round && at.contains(n))).map(String::as_str)
    }

    /// Whether an edit of `device` reaches the control block alone: no later block holds it or a
    /// ghost of it (`RoundPlan::set_param`). A block on a helper counts as reached. O(blocks)
    /// device lookups.
    pub(crate) fn reaches_ctl_alone(&self, device: &str) -> bool {
        let ghosted = |k: usize| self.cut.ghosts.get(device).into_iter().flatten().any(|g| g.0 as usize == k);
        self.runs.iter().enumerate().skip(1).all(|(k, r)| r.as_deref().is_some_and(|r| r.s.find_device(device).is_none()) && !ghosted(k))
    }

    /// Whether an edit reached a device on a loop of the plan's round map (`on_loop`), whose proof
    /// it can break: the proof is due again (`reprove`).
    pub(crate) fn reprove_due(&self) -> bool {
        self.reprove.iter().any(|&e| e)
    }

    /// Whether the plan keeps a proof to prove an edit by (`RoundPlan::proof`).
    pub(crate) fn proves(&self) -> bool {
        self.cut.proof.is_some()
    }

    /// The plan proved again after the edits that reached its loops: each edited block linearised
    /// where it stands, the others as the plan's proof keeps them, its points shared out with the
    /// `helpers` (`RoundProof::reprove`). The gains it proves, or why it fails (or that the plan
    /// keeps no proof). O(the edited blocks' factorisations and solves at every point of the map,
    /// and its composition there), over the worker and the helpers.
    pub(crate) fn reprove(&mut self, helpers: Option<&Helpers>) -> Result<crate::analysis::round::RoundMap, String> {
        let edited: Vec<usize> = (0..self.reprove.len()).filter(|&k| std::mem::take(&mut self.reprove[k])).collect();
        let Some(proof) = self.cut.proof.as_mut() else { return Err("the plan keeps no round map to prove the edit by".into()) };
        let solvers: Vec<(usize, &Solver)> = edited.iter().map(|&k| (k, &self.runs[k].as_deref().expect("a block is in its place between rounds").s)).collect();
        Arc::make_mut(proof).reprove(&solvers, self.end, helpers)
    }

    /// Whether `ports` fit the holders otherwise than the plan in force (`renew_ports`).
    pub(crate) fn ports_differ(&self, ports: &[HolderPort]) -> bool {
        self.cut.ports != ports
    }

    /// A plan that cuts the run as it runs (`runs_cut`) brings its holder ports as fitted at the
    /// run's state: each block whose port it fits otherwise takes the new impedance and restarts
    /// its step, as after an edit. Between rounds. O(ports).
    pub(crate) fn renew_ports(&mut self, ports: &[HolderPort]) {
        for p in ports {
            let b = self.runs[p.block as usize].as_deref_mut().expect("a block is in its place between rounds");
            if port_into(&mut b.s, p) {
                b.st.restart(&b.s);
            }
        }
        self.cut.ports = ports.to_vec();
    }

    /// A plan that cuts the run as it runs (`runs_cut`) brings its proof, made from the run's
    /// state after every edit: it replaces the one kept.
    pub(crate) fn renew_proof(&mut self, proof: Option<Arc<crate::analysis::round::RoundProof>>) {
        self.cut.proof = proof;
    }

    /// Every block, in block order. Between rounds every block is in its place.
    pub(crate) fn runs_mut(&mut self) -> impl Iterator<Item = &mut Run> {
        self.runs.iter_mut().map(|r| r.as_deref_mut().expect("a block is in its place between rounds"))
    }

    /// The block that solves the whole circuit's unknown `u`.
    pub(crate) fn block_of(&self, u: u32) -> Option<usize> {
        self.cut.blocks.iter().position(|b| b.own.iter().any(|&(_, g)| g == u))
    }

    /// The time every block has stepped to: the slowest stage's, block 0's when none runs behind
    /// it (each block's currents are its own newest point's, `currents_now`). None while a flight
    /// shares a block. O(blocks).
    pub(crate) fn stepped_to(&self) -> Option<f64> {
        self.runs.iter().try_fold(f64::INFINITY, |t, r| Some(t.min(r.as_deref()?.st.t())))
    }

    /// The longest an edit's instant can trail into the blocks' values through what they read
    /// late: over the blocks, the longest delay each reads a coupling with, summed (a path through
    /// the blocks enters each once), seconds. A block that reads a coupling late steps with what
    /// its exporter had that much before. O(blocks x couplings).
    pub(crate) fn lag(&self) -> f64 {
        (0..self.runs.len() as u32).map(|b| {
            self.couplings.iter().zip(&self.delays).flat_map(|(k, delays)| k.to.iter().zip(delays).filter(|((to, _), _)| *to == b).map(|(_, &d)| d)).fold(0.0, f64::max)
        }).sum()
    }

    pub(crate) fn runs(&self) -> impl Iterator<Item = &Run> {
        self.runs.iter().map(|r| r.as_deref().expect("a block is in its place between rounds"))
    }

    /// The plan it runs.
    pub(crate) fn cut(&self) -> &RoundPlan {
        &self.cut
    }

    /// Whether these rounds already run `plan` with `solvers`: the same cut (`RoundPlan::same_cut`),
    /// the same imports landing (`RoundPlan::lands`: a chain landed, or an import the plan's test
    /// lands now, is a plan that goes in) and each block's solver with the same unknowns and the
    /// same instances, by name, read with the blocks anywhere (`BlockSig`). O(unknowns + instances).
    pub(crate) fn runs_cut(&self, plan: &RoundPlan, solvers: &[Solver]) -> bool {
        self.cut.same_cut(plan) && self.cut.lands == plan.lands && solvers.len() == self.sigs.len() && self.sigs.iter().zip(solvers).all(|(sig, s)| sig.matches(s))
    }

    /// Per block of a new plan (`solvers`, block 0 first): the running block it is, if any: the
    /// same unknowns and the same instances, by name and in order (its boundary sources and ghosts
    /// among them, so the same reads across the cut), and block 0 only as block 0. The first
    /// match in block order; each running block matches once. O(blocks^2 x (unknowns +
    /// instances)) at worst, by name.
    pub(crate) fn same_blocks(&self, solvers: &[Solver]) -> Vec<Option<usize>> {
        let mut taken = vec![false; self.sigs.len()];
        solvers.iter().enumerate().map(|(k, s)| {
            let j = self.sigs.iter().enumerate().position(|(j, sig)| !taken[j] && (j == 0) == (k == 0) && sig.matches(s))?;
            taken[j] = true;
            Some(j)
        }).collect()
    }

    /// Taken apart for a plan that cuts the run anew: what its rounds carry on (`Carry`) and the
    /// runs, in block order, every block in its place (`settle` first).
    pub(crate) fn into_parts(self) -> (Carry, Vec<Run>) {
        let windows = self.couplings.iter().map(|k| k.from).zip(self.windows).collect();
        let chains = self.cut.chains.iter().zip(self.shadows.seen).map(|(ch, seen)| (self.couplings[ch.born as usize].from, ch.roots.clone(), ch.members.clone(), seen)).collect();
        let carry = Carry { ctl_done: self.ctl_done, ends: self.ends, start: self.start, windows, chains };
        (carry, self.runs.into_iter().map(|r| *r.expect("a block is in its place between rounds")).collect())
    }

    /// The end of the round that starts at `t`: the next multiple of the round length, past a
    /// rounding (the stepper's grid rule, so no round is a sliver).
    pub(crate) fn end_after(&self, t: f64) -> f64 {
        let mut k = (t / self.round).floor() + 1.0;
        if k * self.round - t < 1e-6 * self.round {
            k += 1.0;
        }
        k * self.round
    }

    /// What the sinks hear at the sample rate (`Sink::heard`, sorted), told before each round
    /// opens. A block is heard when it solves a heard net or a heard block reads it through a
    /// coupling, transitively: what reaches a heard net is resolved at the sample period, not only
    /// the net itself (a line out heard keeps the highs of the voices that feed it). O(1) while it
    /// is unchanged, else O(the blocks' unknowns x log heard + the couplings x the blocks).
    pub(crate) fn hear(&mut self, heard: &[u32]) {
        if heard != self.heard.as_slice() {
            self.heard.clear();
            self.heard.extend_from_slice(heard);
            let mut by: Vec<bool> = self.cut.blocks.iter().map(|b| b.own.iter().any(|(_, g)| heard.binary_search(g).is_ok())).collect();
            let mut grew = true;
            while grew {
                grew = false;
                for c in &self.cut.couplings {
                    let from = c.from.0 as usize;
                    if !by[from] && c.to.iter().any(|&(b, _)| by[b as usize]) {
                        (by[from], grew) = (true, true);
                    }
                }
            }
            self.heard_by = by.into();
        }
    }

    /// Block `b`'s step bound for its round from `t0` (`LiveSpec::own_bound`), `heard` whether it was
    /// heard (`hear`) as block 0 opened that round: the longest the rounds allow,
    /// halved while a step would span more than [`CROSSING_SHARE`] of the spacing of its own
    /// crossings, never under the sample period; the period itself while it is heard, so what is
    /// heard keeps its highs. It goes by the block's own crossings and by what is heard, never by
    /// the block's place in the plan. With no own bound, a block that was on one goes back to the
    /// plain step (`renew_bounds`).
    fn block_bound(&mut self, b: usize, t0: f64, heard: bool) {
        let run = self.runs[b].as_deref_mut().expect("a block is in its place as its round opens");
        self.own[b] = match self.own_bound {
            Some(own) => {
                let room = if heard { 0.0 } else { CROSSING_SHARE * run.st.crossing_spacing(t0) };
                let mut bound = own.longest;
                while bound > own.period && bound > room {
                    bound *= 0.5;
                }
                run.st.set_own_bound(&mut run.s, bound, own.period);
                bound > own.period
            }
            None => {
                if let (true, Some((cap, period))) = (self.own[b], self.plain) {
                    run.st.set_own_bound(&mut run.s, cap, period);
                }
                false
            }
        };
    }

    /// The longest grid a block reads its imports on (`feed`): a read reaches one grid before its
    /// round (`Read::table`), so the windows keep that much more (`begin`'s trim). The longest own
    /// bound, the grid each block was last fed on, and that of each block in its place now (set
    /// since, as by `renew_bounds`). O(blocks).
    fn widest_grid(&self) -> f64 {
        let own = self.own_bound.map_or(0.0, |o| o.longest);
        let home = self.runs.iter().flatten().map(|run| run.st.grid());
        self.grids.iter().copied().chain(home).filter(|g| g.is_finite()).fold(own, f64::max)
    }

    /// Every block's bound set again where it stands, after the step cap `cap` on the sample period
    /// `period` reached every block (`LiveSim::sync_step_cap`); with no own bound, the plain one. A
    /// later stage's next round sets its own again as it opens. A block kept from an earlier plan may
    /// come on its own bound: with none now, it takes the plain step again.
    pub(crate) fn renew_bounds(&mut self, cap: f64, period: f64) {
        self.plain = Some((cap, period));
        for b in 0..self.runs.len() {
            let run = self.runs[b].as_deref().expect("a block is in its place between rounds");
            let (t, heard) = (run.st.t(), self.heard_by[b]);
            self.own[b] |= run.st.on_own_bound();
            self.block_bound(b, t, heard);
        }
    }

    /// Whether block `b`'s round `m` is to run again (`Shadows`): a reader in its turn, or any in a
    /// burst; block 0 only with no process attached.
    fn shadow_wants(&self, b: usize, m: u64) -> bool {
        let sh = &self.shadows;
        (b > 0 || sh.ctl) && sh.readers.iter().position(|&r| r as usize == b).is_some_and(|i| m <= sh.burst_to || (m + i as u64) % SHADOW_EVERY == 0)
    }

    /// The bar where block `b`, reading late, ends is held to (`SHADOW_ACT`): that many times the
    /// loosest promise of what it reads late (`RoundPlan::late_tol`: a pad's or a supply's return,
    /// the hub's bar; an ordinary value, the cut's), and never looser than the hub's bar itself
    /// (`RoundPlan::hub_tol`), so no reader is exempt; infinite for a plan that promises nothing.
    /// O(its imports).
    pub(crate) fn shadow_bar(&self, b: usize) -> f64 {
        let Some(cap) = self.cut.hub_tol else { return f64::INFINITY };
        let tol = |c: u32| self.cut.late_tol.get(c as usize).copied().unwrap_or(f64::INFINITY);
        let late = self.imports[b].iter().filter(|&&c| self.couplings[c as usize].lag == Lag::Round);
        (SHADOW_ACT * late.fold(0.0, |m: f64, &c| m.max(tol(c)))).min(cap)
    }

    /// The bar block `b`'s shifts' drift and smear are held to (`ShiftSums`, Fable's A-next step
    /// 4): the promise itself, not `SHADOW_ACT` times it, as they are means. The loosest promise of
    /// its ordinary late reads (`RoundPlan::late_tol`, under the hub's bar); a block that reads
    /// only returns late, the hub's bar (`RoundPlan::hub_tol`); infinite for a plan that promises
    /// nothing. O(its imports).
    pub(crate) fn shift_bar(&self, b: usize) -> f64 {
        let Some(hub) = self.cut.hub_tol else { return f64::INFINITY };
        let tol = |c: u32| self.cut.late_tol.get(c as usize).copied().unwrap_or(f64::INFINITY);
        let late = self.imports[b].iter().filter(|&&c| self.couplings[c as usize].lag == Lag::Round).map(|&c| tol(c));
        late.filter(|&t| t < hub).fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t)))).unwrap_or(hub)
    }

    /// The first shadow round past its bar since the last taken (`LiveSim::shadow_taken`). O(1).
    pub(crate) fn shadow_over(&mut self) -> Option<ShadowOver> {
        self.shadows.over.take()
    }

    /// Whether block 0 may run again (`Shadows`): no process attached, which steps with it.
    pub(crate) fn shadow_ctl(&mut self, alone: bool) {
        self.shadows.ctl = alone;
    }

    /// An edit or a press: every reader's every round runs again for a while (`SHADOW_BURST`).
    pub(crate) fn shadow_burst(&mut self) {
        self.shadows.burst_to = self.ctl_done + SHADOW_BURST;
    }

    /// The shadow rounds back from the checkers counted, and each taken round whose late values
    /// have all been exported on time made a job for the checkers (`checks_post`). Before the windows are
    /// trimmed: they still hold that round's values (its round is still queued, or was at the last
    /// trim). The run waits for none. O(taken rounds x their imports' windows).
    fn shadows_post(&mut self, split: &mut RoundSplit) -> Vec<Shadow> {
        split.hub_onsets += std::mem::take(&mut self.hub.onsets);
        let mut jobs = Vec::new();
        while let Ok(found) = self.shadows.rx.try_recv() {
            self.shadow_count(found, split);
        }
        let done = |rs: &Rounds, e: u32, m: u64| e == 0 && rs.ctl_done >= m || rs.stages.iter().position(|ids| ids.contains(&e)).is_some_and(|s| rs.done[s] >= m);
        let mut i = 0;
        while i < self.shadows.open.len() {
            let o = &self.shadows.open[i];
            let b = o.b as usize;
            let late = |c: &&u32| self.couplings[**c as usize].lag == Lag::Round;
            if !o.late || o.ran.is_none() || !self.imports[b].iter().filter(late).all(|&c| done(self, self.couplings[c as usize].from.0, o.m)) {
                i += 1;
                continue;
            }
            // a copy a chain tree still wants stays for it
            let (id, m, burst, t0, grid, crossings_as_data) = (o.b, o.m, o.burst, o.t0, o.grid, o.crossings_as_data);
            let last = o.trees == 0;
            let (run, ran) = if last {
                self.shadows.open.swap_remove(i).take_for(true)
            } else {
                let o = &mut self.shadows.open[i];
                o.late = false;
                i += 1;
                o.take_for(false)
            };
            let (Some((run, take_ns)), Some(ran)) = (run, ran) else { continue };
            split.shadow_take_ns += take_ns;
            let end = ran.t.last().copied().unwrap_or(t0);
            let mut feeds = Vec::new();
            for &c in self.imports[b].iter().filter(late) {
                let k = &self.couplings[c as usize];
                let mut points = self.windows[c as usize].table_to(t0 - grid, end);
                let lands = self.cut.lands.binary_search(&(c, b as u32)).is_ok();
                if let Some((fed, _)) = points.split(grid, 0.0, t0, None, end, end).filter(|_| !lands) {
                    points = fed;
                }
                if crossings_as_data && !lands && !points.edges.is_empty() {
                    let edges = std::mem::take(&mut points.edges);
                    points.events.retain(|i| edges.binary_search(i).is_err());
                }
                let table = Arc::new(points);
                feeds.extend(k.to.iter().filter(|t| t.0 as usize == b).map(|t| (t.1.clone(), table.clone())));
            }
            let ranges = self.exports[b].iter().map(|&c| self.shadows.range[c as usize]).collect();
            let peak = if self.hold.is_finite() { self.hold / crate::livesim::HOLD } else { 1.0 };
            jobs.push(Shadow { b: id, m, burst, t0, run, feeds, ran, ranges, peak, bar: self.shadow_bar(b) });
        }
        jobs
    }

    /// The checks ready at this round start (`shadows_post`, `trees_post`) handed to the pool as it
    /// can start them (`Checks::room`, Fable's Q-checks answer). The shadow rounds are owed first,
    /// per reader (`Shadows`): its burst rounds kept until stale and thinned evenly, its newest
    /// sampling round. A reader with no finished check for `SHADOW_FLOOR` sampling turns has its
    /// oldest owed round run here (`shadow_floor`). With room, every owed check goes, split over
    /// that many jobs in order: owed trees, burst rounds oldest first, sampling rounds, fresh trees.
    /// With none, fresh trees not owed are dropped and counted; nothing else is. With no checking
    /// pool the checks run here. O(ready, owed and carried checks).
    fn checks_post(&mut self, shadows: Vec<Shadow>, trees: Vec<Tree>, split: &mut RoundSplit) {
        let mut carried = std::mem::take(&mut self.shadows.owed);
        let waiting = carried.len();
        carried.retain(|tree| tree.owed_until.is_some_and(|until| self.ctl_done <= until));
        split.chain_owed_stale += (waiting - carried.len()) as u64;
        for job in shadows {
            self.owe(job, split);
        }
        self.trim_owed(split);
        let Some(c) = self.checks.clone() else {
            for job in carried {
                self.tree_count(Some(job.run()), split);
            }
            for job in self.take_owed() {
                let found = job.run();
                self.shadow_count(Some(found), split);
            }
            for job in trees {
                let found = job.run();
                self.tree_count(Some(found), split);
            }
            return;
        };
        self.shadow_floor(split);
        let room = c.room(self.packs_last);
        split.checkers = room as u64;
        if room == 0 {
            for tree in trees {
                if tree.owed_until.is_some() {
                    split.chain_owed_carried += 1;
                    carried.push(tree);
                } else {
                    split.chain_skipped += 1;
                }
            }
            self.shadows.owed = carried;
            return;
        }
        let owed = self.take_owed();
        let shadow_job = |job: Shadow| -> Box<dyn FnOnce() + Send> {
            let tx = self.shadows.tx.clone();
            Box::new(move || {
                let _ = tx.send(catch_unwind(AssertUnwindSafe(move || job.run())).ok());
            })
        };
        let tree_job = |job: Tree| -> Box<dyn FnOnce() + Send> {
            let tx = self.shadows.tree_tx.clone();
            Box::new(move || {
                let _ = tx.send(catch_unwind(AssertUnwindSafe(move || job.run())).ok());
            })
        };
        let jobs: Vec<_> = carried.into_iter().map(&tree_job).chain(owed.into_iter().map(shadow_job)).chain(trees.into_iter().map(&tree_job)).collect();
        let k = room.min(jobs.len());
        if k == 0 {
            return;
        }
        let mut packs: Vec<Vec<Box<dyn FnOnce() + Send>>> = (0..k).map(|_| Vec::new()).collect();
        for (i, job) in jobs.into_iter().enumerate() {
            packs[i % k].push(job);
        }
        for pack in packs {
            c.post(Box::new(move || {
                for job in pack {
                    job();
                }
            }));
        }
    }

    /// A shadow round ready, owed to its reader: a burst round at the back of its queue, a sampling
    /// round in its place, replacing an older one (counted). O(1).
    fn owe(&mut self, job: Shadow, split: &mut RoundSplit) {
        let i = self.shadows.readers.iter().position(|&r| r == job.b).expect("a shadow round is a reader's");
        if job.burst {
            self.shadows.owed_burst[i].push_back(job);
        } else if self.shadows.owed_sampling[i].replace(job).is_some() {
            split.shadow_replaced += 1;
        }
    }

    /// The owed burst rounds: a stale one (`CHAIN_STALE` rounds past its own) let go, and a
    /// reader's past `SHADOW_BURST` thinned to every second one, its oldest kept, so coverage
    /// halves evenly across the burst and the rounds right after the plan stay. Counted. O(owed).
    fn trim_owed(&mut self, split: &mut RoundSplit) {
        let done = self.ctl_done;
        for q in &mut self.shadows.owed_burst {
            while q.front().is_some_and(|s| done > s.m + CHAIN_STALE) {
                q.pop_front();
                split.shadow_stale += 1;
            }
            if q.len() > SHADOW_BURST as usize {
                let before = q.len();
                let mut k = 0;
                q.retain(|_| {
                    k += 1;
                    k % 2 == 1
                });
                split.shadow_thinned += (before - q.len()) as u64;
            }
        }
    }

    /// Every owed shadow round, in the order they go: burst rounds oldest first, then sampling
    /// rounds. O(owed log owed).
    fn take_owed(&mut self) -> Vec<Shadow> {
        let mut owed: Vec<Shadow> = self.shadows.owed_burst.iter_mut().flat_map(|q| q.drain(..)).collect();
        owed.sort_by_key(|s| s.m);
        owed.extend(self.shadows.owed_sampling.iter_mut().filter_map(Option::take));
        owed
    }

    /// The floor (Fable's D): a reader with no finished check for `SHADOW_FLOOR` of its sampling
    /// turns has its oldest owed round run here by the worker, counted (`RoundSplit::shadow_floor`).
    /// O(readers), and one shadow round a reader so starved.
    fn shadow_floor(&mut self, split: &mut RoundSplit) {
        for i in 0..self.shadows.readers.len() {
            if self.ctl_done.saturating_sub(self.shadows.checked[i]) <= SHADOW_FLOOR * SHADOW_EVERY {
                continue;
            }
            let Some(job) = self.shadows.owed_burst[i].pop_front().or_else(|| self.shadows.owed_sampling[i].take()) else { continue };
            split.shadow_floor += 1;
            let found = job.run();
            self.shadow_count(Some(found), split);
        }
    }

    /// The shadow rounds still owed as the run leaves these rounds (a new plan, or one stepper):
    /// counted as lost (`RoundSplit::shadow_lost`). O(readers).
    pub(crate) fn owed_lost(&mut self, split: &mut RoundSplit) {
        let burst: usize = self.shadows.owed_burst.iter_mut().map(|q| std::mem::take(q).len()).sum();
        let sampling = self.shadows.owed_sampling.iter_mut().filter_map(Option::take).count();
        split.shadow_lost += (burst + sampling) as u64;
    }

    /// The chain trees back from the checkers counted, and each chain round taken whose stages have
    /// all run it made a job for the checkers (`checks_post`) as one tree (`Tree`): its born exports
    /// landed into their roots, each member's copy (shared with a shadow round), and the windows
    /// of the exports the tree passes on as the round began. Before the windows are trimmed, as
    /// `shadows_post`. A round whose members have not all come back `CHAIN_STALE` rounds on is
    /// dropped, counted as failed. The run waits for none. O(trees taken x their members' imports'
    /// windows).
    fn trees_post(&mut self, split: &mut RoundSplit) -> Vec<Tree> {
        let mut jobs = Vec::new();
        split.chain_jumps += std::mem::take(&mut self.shadows.jumps);
        let (kept, began) = std::mem::take(&mut self.shadows.fresh);
        (split.chain_kept, split.chain_began) = (split.chain_kept + kept, split.chain_began + began);
        while let Ok(found) = self.shadows.tree_rx.try_recv() {
            self.tree_count(found, split);
        }
        let mut i = 0;
        while i < self.shadows.taking.len() {
            let (m, t0, end, first) = { let r = &self.shadows.taking[i]; (r.m, r.t0, r.end, r.first) };
            // the round's tree: its exports' members in the order they are fed, their pairs and
            // the imports landed
            let ks = self.shadows.taking[i].ks.clone();
            let chains = &self.cut.chains;
            let mut members: Vec<u32> = ks.iter().flat_map(|&k| chains[k as usize].members.iter().copied()).collect();
            members.sort_unstable();
            members.dedup();
            let mut pairs: Vec<(u32, u32)> = ks.iter().flat_map(|&k| chains[k as usize].pairs.iter().copied()).collect();
            pairs.sort_unstable();
            pairs.dedup();
            let born: Vec<(u32, u32)> = ks.iter().flat_map(|&k| chains[k as usize].roots.iter().map(move |&r| (chains[k as usize].born, r))).collect();
            let at = |o: &Open, r: u32| o.b == r && o.m == m && o.trees > 0;
            // every stage has run the round: every export that heads one has joined or not
            let long = u64::from(self.shadows.taking[i].long);
            let ready = self.done.iter().all(|&d| d >= m + long) && members.iter().all(|&r| self.shadows.open.iter().any(|o| at(o, r) && o.ran.is_some()) && (1..=long).all(|j| followed(&self.shadows.follow, r, m + j).is_some()));
            let until = m + long + CHAIN_STALE;
            if !ready && self.ctl_done <= until {
                i += 1;
                continue;
            }
            let taken = self.shadows.taking.remove(i);
            let owed_until = (first || taken.alone).then_some(until);
            let stale = owed_until.is_some() && self.ctl_done > until;
            split.chain_owed_stale += u64::from(stale);
            let (mut tree, mut windows, mut take_ns, mut lost) = (Vec::with_capacity(members.len()), Vec::new(), 0, !ready || stale);
            for &r in &members {
                let Some(j) = self.shadows.open.iter().position(|o| at(o, r)) else {
                    lost = true;
                    continue;
                };
                let o = &mut self.shadows.open[j];
                o.trees -= 1;
                let (last, grid, crossings_as_data) = (o.trees == 0 && !o.late, o.grid, o.crossings_as_data);
                if lost {
                    if last { self.shadows.open.swap_remove(j); }
                    continue;
                }
                // the exports it passes on along the tree, as the round began
                for &(q, _) in &pairs {
                    if self.couplings[q as usize].from.0 == r && !windows.iter().any(|w: &(u32, Window)| w.0 == q) {
                        match o.before.iter().find(|x| x.0 == q) {
                            Some(x) => windows.push(x.clone()),
                            None => lost = true,
                        }
                    }
                }
                let (run, ran) = if last { self.shadows.open.swap_remove(j).take_for(true) } else { o.take_for(false) };
                let (Some((run, ns)), Some(ran)) = (run, ran) else {
                    lost = true;
                    continue;
                };
                take_ns += ns;
                if lost {
                    continue;
                }
                // what it reads along the tree: a member's shadow points, else its born import landed
                let mut feeds = Vec::new();
                for &(q, _) in pairs.iter().filter(|p| p.1 == r) {
                    let k = &self.couplings[q as usize];
                    let mut landed = None;
                    for ((to, name), &delay) in k.to.iter().zip(&self.delays[q as usize]) {
                        if *to != r {
                            continue;
                        }
                        let from = t0 - delay - grid;
                        let feed = match windows.iter().position(|w| w.0 == q) {
                            Some(w) => Feed::Window(w, from),
                            None if born.contains(&(q, r)) => Feed::Table(landed.get_or_insert_with(|| Arc::new(self.windows[q as usize].landed_to(from, end))).clone()),
                            None => {
                                lost = true;
                                continue;
                            }
                        };
                        feeds.push((name.clone(), delay, feed));
                    }
                }
                let outs = windows.iter().enumerate().filter(|(_, w)| self.couplings[w.0 as usize].from.0 == r)
                    .filter_map(|(w, x)| self.exports[r as usize].iter().position(|&c| c == x.0).map(|e| (w, e))).collect();
                let ranges = self.exports[r as usize].iter().map(|&c| self.shadows.range[c as usize]).collect();
                let mut later = Vec::with_capacity(long as usize);
                for j in 1..=long {
                    match followed(&self.shadows.follow, r, m + j) {
                        Some(f) => later.push(Later { end: f.end, feeds: f.feeds.clone(), ran: f.ran.clone().expect("a followed round as it ran") }),
                        None => lost = true,
                    }
                }
                tree.push(Member { b: r, run, grid, crossings_as_data, feeds, outs, ran, ranges, later });
            }
            split.chain_take_ns += take_ns;
            if first {
                split.chain_first_ns += take_ns;
            }
            if lost {
                split.chain_trees += 1;
                split.chain_failed += 1;
                continue;
            }
            split.chain_cut_late += u64::from(ks.iter().any(|&k| self.cut.chains[k as usize].cut_late > 0));
            let peak = if self.hold.is_finite() { self.hold / crate::livesim::HOLD } else { 1.0 };
            let bar = ks.iter().fold(f64::INFINITY, |a, &k| a.min(self.chain_bar(k as usize)));
            jobs.push(Tree { ks, first, owed_until, t0, end, peak, bar, edges: self.edges, members: tree, windows });
        }
        let (Shadows { taking, follow, .. }, chains) = (&mut self.shadows, &self.cut.chains);
        follow.retain(|f| taking.iter().any(|r| r.long > 0 && r.m < f.m && f.m <= r.m + u64::from(r.long) && r.ks.iter().any(|&k| chains[k as usize].members.contains(&f.b))));
        jobs
    }

    /// A chain tree's members' figures into the counts (`RoundSplit::chain_trees`); none, or a
    /// member that failed (its round or a later one), a failure. Its bar is its born exports'
    /// strictest (`chain_bar`, as the tree was sent). A member past the screen was followed. Its
    /// worst valid held reading decides conviction. An incomplete sweep cannot heal a mark.
    fn tree_count(&mut self, found: Option<TreeFound>, split: &mut RoundSplit) {
        split.chain_trees += 1;
        let Some(TreeFound { ks, first, end, bar, members, followed, failed, sweep_clear, escalated_by, unswept, own_ns, ns }) = found else {
            split.chain_failed += 1;
            return;
        };
        split.chain_run_ns += ns;
        split.chain_own_ns += own_ns;
        split.chain_escalated += u64::from(!escalated_by.is_empty());
        for b in escalated_by {
            *split.chain_escalated_by.entry(self.cut.blocks[b as usize].name.clone()).or_default() += 1;
        }
        split.chain_unswept += unswept;
        if first {
            split.chain_first += 1;
            split.chain_first_ns += ns;
        }
        split.chain_members += members.len() as u64;
        let name = |k: u32| self.late_at.get(self.cut.chains[k as usize].born as usize).and_then(|n| n.first()).map_or("?", |s| s.as_str());
        let mut born = ks.iter().take(2).map(|&k| name(k)).collect::<Vec<_>>().join(" + ");
        if ks.len() > 2 {
            born.push_str(&format!(" + {} more", ks.len() - 2));
        }
        let mut over = false;
        for &MemberReading { b, figures, .. } in &members {
            let Some((figures, signed)) = figures else { continue };
            let name = self.cut.blocks.get(b as usize).map_or("?", |bl| bl.name.as_str());
            for (f, &x) in figures.iter().enumerate() {
                if x > split.chain_worst[f] {
                    split.chain_worst[f] = x;
                    split.chain_at[f] = format!("{name} from {born} at {end:.6} s");
                }
            }
            if signed.abs() > split.chain_signed.abs() {
                split.chain_signed = signed;
                split.chain_signed_at = format!("{name} from {born} at {end:.6} s");
            }
            if figures[OWN_END] / bar > split.chain_nearest {
                split.chain_nearest = figures[OWN_END] / bar;
                split.chain_nearest_at = format!("{name} from {born} at {end:.6} s, {:.2e} against {bar:.1e}", figures[OWN_END]);
            }
            over |= figures[OWN_END] > bar;
        }
        // the members held past the bar: the tree is convicted on this jump
        let mut held_by: Vec<(u32, String)> = Vec::new();
        if followed {
            split.chain_followed += 1;
            let (mut later_over, mut held_over) = (false, false);
            for MemberReading { b, held, .. } in members {
                let Some(Held { held, node, end: later, offset }) = held else { continue };
                let name = self.cut.blocks.get(b as usize).map_or("?", |bl| bl.name.as_str());
                if held / bar > split.chain_held_nearest {
                    split.chain_held_nearest = held / bar;
                    split.chain_held_nearest_at = format!("{name} from {born} at {end:.6} s on {node}, {held:.2e} against {bar:.1e}, its own end at the window's end {:.2} of it", later / bar);
                }
                later_over |= later > bar;
                held_over |= held > bar;
                if held > bar {
                    let at = offset.map_or_else(|| "its own offset".into(), |k| format!("{k}/{CHAIN_OFFSETS}"));
                    held_by.push((b, format!("{name} own held {held:.2e} on {node} at {at}")));
                }
            }
            split.chain_later_over += u64::from(later_over);
            split.chain_held_over += u64::from(held_over);
            split.chain_healed += u64::from(!held_over && !failed && sweep_clear);
        }
        split.chain_failed += u64::from(failed);
        split.chain_over += u64::from(over);
        self.chain_verdict(&ks, &held_by, &format!("{born} at {end:.6} s"), bar, failed || !sweep_clear, split);
    }

    /// The act on a tree back (Fable's H1/H2 answer, Q3), `held_by` its members held past `bar`
    /// with what each held. A tree of one born export held is its own blame: the export is
    /// convicted (`chain_act`), once. A union held marks only the exports whose chain holds a held
    /// member (`Shadows::marks`): their next jumps are taken alone (`chain_round`). A marked
    /// export's tree that holds nothing heals one jump of its mark, and `CHAIN_FIRST` in a row
    /// clear it; a tree that failed heals nothing. O(the tree's exports x members).
    fn chain_verdict(&mut self, ks: &[u32], held_by: &[(u32, String)], at: &str, bar: f64, failed: bool, split: &mut RoundSplit) {
        let sh = &mut self.shadows;
        match (ks, held_by.is_empty()) {
            (&[k], false) => {
                sh.marks[k as usize] = None;
                if !sh.convicted.iter().any(|c| c.0 == k) {
                    split.chain_convicted += 1;
                }
                for (block, held) in held_by {
                    if !sh.convicted.iter().any(|c| c.0 == k && c.1 == *block) {
                        sh.convicted.push((k, *block, format!("{at}: {held}, against {bar:.1e}")));
                    }
                }
            }
            (_, false) => {
                for &k in ks {
                    let holds = held_by.iter().any(|(b, _)| self.cut.chains[k as usize].members.contains(b));
                    if holds && sh.marks[k as usize].is_none() {
                        sh.marks[k as usize] = Some(0);
                        split.chain_marked += 1;
                    }
                }
            }
            (&[k], true) if !failed => {
                if let Some(healed) = sh.marks[k as usize] {
                    let healed = healed + 1;
                    sh.marks[k as usize] = (healed < CHAIN_FIRST).then_some(healed);
                    split.chain_unmarked += u64::from(healed >= CHAIN_FIRST);
                }
            }
            _ => {}
        }
    }

    /// The landings convicted since the run last took them (`LiveSim::chain_taken`). Each held
    /// member names the couplings it read on its tree. The first witness to each landing is kept.
    pub(crate) fn chain_act(&mut self) -> Vec<(Land, String)> {
        let mut acts: Vec<(Land, String)> = Vec::new();
        for (k, block, what) in self.shadows.convicted.drain(..) {
            let chain = &self.cut.chains[k as usize];
            for (&(_, reader), label) in chain.pairs.iter().zip(&chain.labels) {
                if reader == block && !acts.iter().any(|(land, _)| land.born == chain.name && land.to == *label) {
                    acts.push((Land { born: chain.name.clone(), to: label.clone() }, what.clone()));
                }
            }
        }
        acts
    }

    /// Whether the plan has a chain born at an export of one of `names` (`Chain::name`). O(chains).
    pub(crate) fn chains_named(&self, names: &[String]) -> bool {
        self.cut.chains.iter().any(|ch| names.contains(&ch.name))
    }

    /// The bar where a chain tree's member ends is held to: `SHADOW_ACT` times what the plan
    /// promises of its born import (`RoundPlan::late_tol`), never looser than the hub's bar
    /// (`RoundPlan::hub_tol`); infinite for a plan that promises nothing. O(1).
    pub(crate) fn chain_bar(&self, k: usize) -> f64 {
        let Some(cap) = self.cut.hub_tol else { return f64::INFINITY };
        let c = self.cut.chains[k].born as usize;
        (SHADOW_ACT * self.cut.late_tol.get(c).copied().unwrap_or(f64::INFINITY)).min(cap)
    }

    /// A shadow round's figures into the counts (`RoundSplit::shadows`); none, a failure. The first
    /// whose block ends past its bar however shifted, or whose own crossings disagree
    /// (`end_shift`), is kept for the run (`shadow_over`); one without a shift, past its bar. Its
    /// shift, taken where its block has own crossings, and its length go into its reader's sums
    /// (`Shadows::sums`): one whose sums then drift or smear past `shift_bar`, clear of their
    /// scatter, is kept the same way. O(the readers).
    fn shadow_count(&mut self, found: Option<Found>, split: &mut RoundSplit) {
        split.shadows += 1;
        split.shadow_bar = self.shadows.readers.iter().fold(f64::INFINITY, |m, &r| m.min(self.shadow_bar(r as usize)));
        // a finished check, its reader's (`SHADOW_FLOOR`)
        if let Some(i) = found.as_ref().and_then(|f| self.shadows.readers.iter().position(|&r| r == f.b)) {
            self.shadows.checked[i] = self.ctl_done;
        }
        let Some(Found { b, t0, end, figures, shift, crossings, ns }) = found else {
            split.shadow_failed += 1;
            return;
        };
        split.shadow_run_ns += ns;
        let Some(figures) = figures else {
            split.shadow_failed += 1;
            return;
        };
        for (k, &x) in figures.iter().enumerate() {
            let decade = if x > 0.0 { (x.log10().floor() + 7.0).clamp(0.0, 7.0) as usize } else { 0 };
            split.shadow_hist[k][decade] += 1;
            if x > split.shadow_worst[k] {
                split.shadow_worst[k] = x;
                split.shadow_at[k] = format!("{} at {end:.6} s", self.cut.blocks.get(b as usize).map_or("?", |bl| bl.name.as_str()));
            }
        }
        let (bar, shift_bar) = (self.shadow_bar(b as usize), self.shift_bar(b as usize));
        let name = self.cut.blocks.get(b as usize).map_or_else(|| "?".into(), |bl| bl.name.clone());
        let reader = self.shadows.readers.iter().position(|&r| r == b);
        if let Some((shift, parity)) = shift {
            split.shift_parity += u64::from(!parity);
            let tau = match shift {
                EndShift::Plain(Some(tau)) => (split.shift_plain += 1, Some(tau)).1,
                EndShift::Plain(None) => (split.shift_still += 1, None).1,
                EndShift::Shifted(tau, _) => (split.shift_found += 1, Some(tau)).1,
                EndShift::Over => (split.shift_over += 1, None).1,
            };
            if let Some(tau) = tau {
                let decade = if tau != 0.0 { (tau.abs().log10().floor() + 12.0).clamp(0.0, 7.0) as usize } else { 0 };
                split.shift_hist[decade] += 1;
            }
            // a shift is taken where the block has own crossings in the round, as it ran or again;
            // every round's time counts
            let (tau, span) = (tau.filter(|_| crossings.0 > 0 || crossings.1 > 0), end - t0);
            let add = |s: &mut ShiftSums| match tau {
                Some(tau) => s.add(tau, span),
                None => s.pass(span),
            };
            if !split.shift_by.contains_key(&name) {
                split.shift_by.insert(name.clone(), ShiftSums::default());
            }
            add(split.shift_by.get_mut(&name).expect("the reader's sums are in"));
            if let Some(i) = reader {
                add(&mut self.shadows.sums[i]);
            }
        }
        let why = match shift {
            None if figures[OWN_END] > bar => Some(OverWhy::End(figures[OWN_END], bar)),
            Some((_, false)) => Some(OverWhy::Crossings(crossings.0, crossings.1)),
            Some((EndShift::Over, true)) => Some(OverWhy::End(figures[OWN_END], bar)),
            _ => reader.map(|i| self.shadows.sums[i]).and_then(|s| {
                if s.drifts() && s.drift() > shift_bar {
                    Some(OverWhy::Shifts("drift", s.drift(), shift_bar, s.n))
                } else if s.smears() && s.smear() > shift_bar {
                    Some(OverWhy::Shifts("smear", s.smear(), shift_bar, s.n))
                } else {
                    None
                }
            }),
        };
        // the figure judged for its end: once shifted for a round shifted to an own edge
        let (unshifted, judged) = match shift {
            Some((EndShift::Shifted(_, f), true)) => (figures[OWN_END], f),
            _ => (figures[OWN_END], figures[OWN_END]),
        };
        if unshifted > bar {
            let k = match (shift, &why) {
                (Some((EndShift::Shifted(..), true)), None) => 1,
                (Some((EndShift::Shifted(..), true)), Some(_)) => 2,
                _ => 0,
            };
            split.shadow_past[k] += 1;
        }
        if judged / bar > split.shadow_nearest {
            split.shadow_nearest = judged / bar;
            let kind = match shift {
                None => "no end shift taken".to_string(),
                Some((_, false)) => format!("its own crossings apart, {} as it ran, {} again", crossings.0, crossings.1),
                Some((EndShift::Plain(Some(tau)), true)) => format!("within the bar, shifted {:.4} us along its last step", tau * 1e6),
                Some((EndShift::Plain(None), true)) => "within the bar, still".into(),
                Some((EndShift::Shifted(tau, _), true)) => format!("once shifted {:.4} us to an own edge; {unshifted:.2e} unshifted ({:.2} of its bar)", tau * 1e6, unshifted / bar),
                Some((EndShift::Over, true)) => "past it however shifted within its step bound".into(),
            };
            let sums = reader.map(|i| self.shadows.sums[i]);
            let verdict = match &why {
                Some(OverWhy::End(..)) => "refused for its end".to_string(),
                Some(OverWhy::Crossings(..)) => "refused for its crossings".into(),
                Some(OverWhy::Shifts(which, f, sb, n)) => format!("refused for its {which}, {f:.2e} against {sb:.2e} over {n} rounds"),
                None => sums.map_or_else(|| "refuses nothing".into(), |s| format!("refuses nothing: its shifts' drift {:.2e} and smear {:.2e} against {shift_bar:.2e}", s.drift(), s.smear())),
            };
            split.shadow_nearest_at = format!("{name} at {end:.6} s, {judged:.2e} against {bar:.1e}; {kind}; {verdict}");
        }
        if let Some(why) = why.filter(|_| self.shadows.over.is_none()) {
            self.shadows.over = Some(ShadowOver { block: b as usize, name, end, why });
        }
    }

    /// A round from `t0` to `end` begins: whether its points carry pin currents (and those the
    /// blocks start from), the shadow rounds and chain trees due sent to the checkers
    /// (`shadows_post`, `trees_post`), the windows trimmed to what a read can still ask for, block
    /// 0's sources fed. The stages behind block 0 are posted to the helpers early, so the rest of
    /// the open runs beside their blocks (Fable's Q12, B: decided before the post, read after it):
    /// after the rounds the shadows and the trees take (they copy the windows untrimmed, and a tree
    /// taken sets what a feed's chain round and its follows read), and after block 0's bound (a
    /// port whose holder is block 0 reads its grid) and what block 0's feed takes from the shadows
    /// (the stages' feeds may push a tree it would otherwise see). The trim and block 0's feed then
    /// read nothing a shared block writes. A round that changes the pins posts them last, every block home.
    pub(crate) fn begin(&mut self, t0: f64, end: f64, pins: bool, helpers: Option<&Helpers>, split: &mut RoundSplit) {
        assert!(end - t0 <= self.round * (1.0 + 1e-6), "a round from {t0:e} s to {end:e} s is longer than the round length {:e} s", self.round);
        // the checks the round ahead takes are bounded by the packs the round before posted
        let (packs, merged) = std::mem::take(&mut self.posted);
        self.packs_last = packs + usize::from(merged);
        let shadows = self.shadows_post(split);
        let trees = self.trees_post(split);
        self.t0 = t0;
        self.end = end;
        let heard = self.heard_by[0];
        self.block_bound(0, t0, heard);
        let behind = helpers.filter(|h| self.pipeline && h.count > 0);
        let early = behind.filter(|_| pins == self.pins);
        let mut takes = None;
        if let Some(h) = early {
            takes = Some(self.feed_takes(0, end, self.ctl_done + 1));
            let at = Instant::now();
            self.post_behind(h, split);
            split.open_behind_ns += at.elapsed().as_nanos() as u64;
        }
        self.checks_post(shadows, trees, split);
        let start = self.queue.front().map_or(t0, |q| q.1);
        let oldest = start - self.delays.iter().flatten().fold(self.round, |a, &d| a.max(d)) - self.widest_grid();
        for w in &mut self.windows {
            w.trim(oldest);
        }
        let refused = self.feed(0, t0, end, self.ctl_done + 1, takes);
        debug_assert!(refused.is_none(), "the plan's feeds were checked at its start");
        if pins != self.pins {
            if let Err(d) = self.collect_merge(split) {
                self.failed.get_or_insert(d);
                return;
            }
            let Some(side) = self.side.as_deref_mut() else {
                self.failed.get_or_insert_with(MergeJob::lost);
                return;
            };
            for (run, m) in self.runs.iter_mut().zip(&mut side.merge) {
                let r = run.as_deref_mut().expect("a block is in its place between rounds");
                if pins {
                    let now = r.currents_now();
                    m.pins0 = m.pin_ids.iter().map(|&p| now[p as usize]).collect();
                }
                r.rec.pins = pins;
                m.pins = pins;
            }
            self.pins = pins;
        }
        if let Some(h) = behind.filter(|_| early.is_none()) {
            let at = Instant::now();
            self.post_behind(h, split);
            split.open_behind_ns += at.elapsed().as_nanos() as u64;
        }
    }

    /// Close block 0's round: the posted merge collected (its rows taken with `take_rows`) and
    /// block 0's exports made. The later stages posted at its start may still run on the helpers;
    /// [`Self::close_stages`] collects them.
    pub(crate) fn close_ctl(&mut self, end: f64, count: &mut PackCount, split: &mut RoundSplit) -> Result<(), SimDiag> {
        self.collect_for_export(count, split)?;
        assert!(end <= self.end, "the round to {:e} s ends at {end:e} s, past its end", self.end);
        if split.stage_own_ns.len() < self.stages.len() {
            split.stage_own_ns.resize(self.stages.len(), 0);
            split.stage_wait_ns.resize(self.stages.len(), 0);
        }
        let export_at = Instant::now();
        self.export(0, self.ctl_done + 1);
        split.export_ns += export_at.elapsed().as_nanos() as u64;
        self.ctl_done += 1;
        self.queue.push_back((self.ctl_done, self.t0, end, self.heard_by.clone()));
        if self.pipeline {
            self.ends.push_back((self.ctl_done, end));
            let max_delay = self.delays.iter().flatten().fold(0.0f64, |a, &d| a.max(d));
            let keep = self.stages.len() + 8 + (max_delay / self.round).round() as usize;
            while self.ends.len() > keep {
                self.ends.pop_front();
            }
        }
        match self.failed.take() {
            Some(d) => Err(d),
            None => Ok(()),
        }
    }

    /// After [`Self::close_ctl`], collect the later stages posted at the round's start. With no
    /// stages running behind block 0, run each stage to the same end in order.
    pub(crate) fn close_stages(&mut self, helpers: Option<&Helpers>, count: &mut PackCount, split: &mut RoundSplit) -> Result<(), SimDiag> {
        if let Some(flight) = self.flight.0.take() {
            return self.land(flight, helpers, count, split);
        }
        if self.pipeline && helpers.is_some_and(|h| h.count > 0) && !self.stages.is_empty() {
            // The next round's start posts the stages behind block 0.
            return Ok(());
        }
        self.catch_up(helpers, count, split)
    }

    /// Whether a stage, a posted merge or its rows are still outstanding.
    pub(crate) fn behind(&self) -> bool {
        !self.queue.is_empty() || self.flight.0.is_some() || self.merge_flight.0.is_some() || !self.rows.t.is_empty()
    }

    pub(crate) fn post_merge(&mut self, h: &Helpers, x: &[f64], pin_i: &[f64]) {
        assert!(self.merge_flight.0.is_none() && self.rows.t.is_empty());
        let stop = self.merge_stop();
        let mut side = self.side.take().expect("the merge side is home");
        side.stop = stop;
        self.merge_x.extend_from_slice(x);
        self.merge_pin_i.extend_from_slice(pin_i);
        self.rows.rounds_id = self.id;
        let job = MergeJob {
            side, x: std::mem::take(&mut self.merge_x), pin_i: std::mem::take(&mut self.merge_pin_i),
            rows: std::mem::take(&mut self.rows), run_ns: 0,
        };
        let slot = Arc::new(MergeSlot { claimed: AtomicBool::new(false), job: Mutex::new(Some(job)) });
        let helper = slot.clone();
        let (tx, rx) = mpsc::channel();
        (h.post)(Box::new(move || {
            let Some(mut job) = helper.claim() else { return };
            let back = catch_unwind(AssertUnwindSafe(move || {
                job.run();
                job
            })).ok();
            let _ = tx.send(back);
        }));
        self.merge_flight.0 = Some(MergeFlight { slot, rx });
        self.posted.1 = true;
    }

    pub(crate) fn collect_merge(&mut self, split: &mut RoundSplit) -> Result<(), SimDiag> {
        let Some(flight) = self.merge_flight.0.take() else { return Ok(()) };
        let at = Instant::now();
        let back = if let Some(mut job) = flight.slot.claim() {
            split.merge_claimed += 1;
            job.run();
            Some(job)
        } else {
            flight.rx.recv().ok().flatten()
        };
        drop(flight);
        let Some(mut job) = back else {
            split.merge_wait_ns += at.elapsed().as_nanos() as u64;
            return Err(MergeJob::lost());
        };
        job.side.take_counts(split);
        job.x.clear();
        job.pin_i.clear();
        self.side = Some(job.side);
        self.merge_x = job.x;
        self.merge_pin_i = job.pin_i;
        self.rows = job.rows;
        split.merge_job_ns += job.run_ns;
        split.merge_jobs += 1;
        split.merge_wait_ns += at.elapsed().as_nanos() as u64;
        Ok(())
    }

    /// The blocks must also come home if their merge was lost while they were on helpers.
    fn collect_for_export(&mut self, count: &mut PackCount, split: &mut RoundSplit) -> Result<(), SimDiag> {
        let merged = self.collect_merge(split);
        if self.side.is_none() {
            if let Some(flight) = self.flight.0.take() {
                self.land(flight, None, count, split)?;
            }
            return Err(self.failed.take().or_else(|| merged.err()).unwrap_or_else(MergeJob::lost));
        }
        merged
    }

    pub(crate) fn take_rows(&mut self, into: &mut MergeRows) {
        if !self.rows.t.is_empty() {
            assert!(into.t.is_empty(), "the previous rows were delivered");
            std::mem::swap(&mut self.rows, into);
        }
    }

    pub(crate) fn merge_home(&self) -> bool {
        self.side.is_some()
    }

    /// At the start of block 0's round n, a later stage in pipeline place p (zero-based, `slot`) may
    /// run round n - p - 1. Feed stages whose same-round and lagged inputs are ready, then share
    /// their blocks together (`Share`). The worker collects their exports in stage order when
    /// block 0 finishes; a stage in the place of the one before runs then, right after it (`land`).
    fn post_behind(&mut self, h: &Helpers, split: &mut RoundSplit) {
        let n = self.ctl_done + 1;
        let mut ran = Vec::new();
        let mut blocks: Vec<(u32, f64)> = Vec::new();
        for s in 0..self.stages.len() {
            if s > 0 && self.slot[s] == self.slot[s - 1] {
                continue;
            }
            let m = self.done[s] + 1;
            if m + self.slot[s] as u64 + 1 > n || (0..s).any(|e| self.done[e] < m) {
                continue;
            }
            if (s + 1..self.stages.len()).any(|j| self.done[j] + ((self.slot[j] - self.slot[s] + 1) as u64) < m) {
                continue;
            }
            let Some((t0, end, heard)) = self.queue.iter().find(|q| q.0 == m).map(|q| (q.1, q.2, q.3.clone())) else { continue };
            let ids = self.stages[s].clone();
            for &k in &ids {
                self.block_bound(k as usize, t0, heard[k as usize]);
                let refused = self.feed(k as usize, t0, end, m, None);
                debug_assert!(refused.is_none(), "the plan's feeds were checked at its start");
                blocks.push((k, end));
            }
            ran.push((s, m));
        }
        if blocks.is_empty() {
            return;
        }
        let ids: Vec<u32> = blocks.iter().map(|b| b.0).collect();
        let (order, joiners) = self.order(&ids, h.count.saturating_sub(1), 0);
        let share = Share::new(order.into_iter().map(|id| {
            let &(_, end) = blocks.iter().find(|b| b.0 == id).expect("each shared block was fed");
            (id, self.take(id), end)
        }).collect());
        let (tx, rx) = mpsc::channel();
        let at = Instant::now();
        for _ in 0..joiners {
            Share::post(&share, h, &tx);
        }
        split.open_post_ns += at.elapsed().as_nanos() as u64;
        self.posted.0 = self.posted.0.max(joiners);
        self.flight.0 = Some(Flight { ran, share, joiners, rx });
    }

    /// Collect the shared blocks, the worker taking those no joiner has from the light end, then
    /// export in stage order; then each stage in the place of one that ran runs the same round
    /// right after it (`run_stage`, on the worker and the `helpers`).
    fn land(&mut self, flight: Flight, helpers: Option<&Helpers>, count: &mut PackCount, split: &mut RoundSplit) -> Result<(), SimDiag> {
        let claim_at = Instant::now();
        let Flight { ran, share, joiners, rx } = flight;
        while let Some(i) = share.take(false) {
            self.run_taken(&share, i);
        }
        let wait_at = Instant::now();
        split.claimed_ns += (wait_at - claim_at).as_nanos() as u64;
        let lost = !self.gather(&share, &rx, joiners, count, split);
        let export_at = Instant::now();
        split.wait_ns += (export_at - wait_at).as_nanos() as u64;
        if lost || self.runs.iter().any(Option::is_none) {
            return Err(SimDiag::setup("a block's round was lost on a helper core (it panicked)".into()));
        }
        if self.side.is_none() {
            return Err(MergeJob::lost());
        }
        if let Some(d) = self.runs_mut().find_map(|r| r.failed.take()) {
            return Err(d);
        }
        for &(s, m) in &ran {
            for k in self.stages[s].clone() {
                self.export(k as usize, m);
            }
            self.done[s] = m;
            self.pass_marker(s, m);
            self.pass_keep(s, m);
        }
        split.export_ns += export_at.elapsed().as_nanos() as u64;
        for &(s, m) in &ran {
            let Some((t0, end, heard)) = self.queue.iter().find(|q| q.0 == m).map(|q| (q.1, q.2, q.3.clone())) else { continue };
            let mut j = s + 1;
            while j < self.stages.len() && self.slot[j] == self.slot[s] {
                self.run_stage(j, m, t0, end, &heard, helpers, count, split)?;
                j += 1;
            }
        }
        while self.queue.front().is_some_and(|q| self.done.iter().all(|&d| d >= q.0)) {
            self.queue.pop_front();
        }
        Ok(())
    }

    /// Runs stage `stage`'s round `m` (from `t0` to `end`, `heard` as block 0 opened it): its
    /// blocks fed, shared by cost, run on the worker and the `helpers` (the worker waits only for
    /// blocks a helper is running), then exported.
    #[allow(clippy::too_many_arguments)]
    fn run_stage(&mut self, stage: usize, m: u64, t0: f64, end: f64, heard: &[bool], helpers: Option<&Helpers>, count: &mut PackCount, split: &mut RoundSplit) -> Result<(), SimDiag> {
        let stage_at = Instant::now();
        let ids = self.stages[stage].clone();
        let feed_at = Instant::now();
        for &k in &ids {
            self.block_bound(k as usize, t0, heard[k as usize]);
            let refused = self.feed(k as usize, t0, end, m, None);
            debug_assert!(refused.is_none(), "the plan's feeds were checked at its start");
        }
        let pack_at = Instant::now();
        let (order, takers) = self.order(&ids, helpers.map_or(0, |h| h.count), stage);
        let share = Share::new(order.into_iter().map(|k| (k, self.take(k), end)).collect());
        let (tx, rx) = mpsc::channel();
        // the worker is one of the takers; with no helpers it is the only one
        let joiners = takers - 1;
        if let Some(h) = helpers {
            for _ in 0..joiners {
                Share::post(&share, h, &tx);
            }
        }
        self.posted.0 = self.posted.0.max(joiners);
        drop(tx);
        let own_at = Instant::now();
        while let Some(i) = share.take(true) {
            self.run_taken(&share, i);
        }
        let wait_at = Instant::now();
        let own_ns = (wait_at - own_at).as_nanos() as u64;
        let lost = !self.gather(&share, &rx, joiners, count, split);
        let wait_ns = wait_at.elapsed().as_nanos() as u64;
        let failed = if lost || self.runs.iter().any(Option::is_none) {
            Some(SimDiag::setup("a block's round was lost on a helper core (it panicked)".into()))
        } else {
            self.runs_mut().find_map(|r| r.failed.take())
        };
        let feed_ns = (pack_at - feed_at).as_nanos() as u64;
        let pack_ns = (own_at - pack_at).as_nanos() as u64;
        split.feed_ns += feed_ns;
        split.pack_ns += pack_ns;
        split.own_ns += own_ns;
        split.wait_ns += wait_ns;
        split.stage_own_ns[stage] += own_ns;
        split.stage_wait_ns[stage] += wait_ns;
        let export_at = Instant::now();
        let before_exports_ns = (export_at - stage_at).as_nanos() as u64;
        split.stage_rest_ns += before_exports_ns.saturating_sub(feed_ns + pack_ns + own_ns + wait_ns);
        if let Some(d) = failed {
            return Err(d);
        }
        for &k in &ids {
            self.export(k as usize, m);
        }
        let exported_at = Instant::now();
        split.export_ns += (exported_at - export_at).as_nanos() as u64;
        self.done[stage] = m;
        self.pass_marker(stage, m);
        self.pass_keep(stage, m);
        // the stage's share and channel go out of scope with it
        drop((share, rx));
        split.stage_rest_ns += exported_at.elapsed().as_nanos() as u64;
        Ok(())
    }

    /// Runs the stages still due for each queued round, oldest round first.
    pub(crate) fn catch_up(&mut self, helpers: Option<&Helpers>, count: &mut PackCount, split: &mut RoundSplit) -> Result<(), SimDiag> {
        self.collect_for_export(count, split)?;
        // blocks still shared with the helpers land first (a firmware stop settles inside block 0's round)
        if let Some(flight) = self.flight.0.take() {
            self.land(flight, helpers, count, split)?;
        }
        for i in 0..self.queue.len() {
            let (m, t0, end) = (self.queue[i].0, self.queue[i].1, self.queue[i].2);
            let heard = self.queue[i].3.clone();
            for stage in 0..self.stages.len() {
                if self.done[stage] < m {
                    self.run_stage(stage, m, t0, end, &heard, helpers, count, split)?;
                }
            }
        }
        while self.queue.front().is_some_and(|q| self.done.iter().all(|&d| d >= q.0)) {
            self.queue.pop_front();
        }
        Ok(())
    }

    fn take(&mut self, k: u32) -> Box<Run> {
        self.runs[k as usize].take().expect("a block is packed once a round")
    }

    /// A block the worker took from `share` (`Share::take`) run on the worker, back in its place.
    fn run_taken(&mut self, share: &Share, i: usize) {
        let (k, mut r, end) = share.cells[i].lock().unwrap_or_else(|e| e.into_inner()).take().expect("a taken block waits in its cell");
        r.round(end, false);
        self.runs[k as usize] = Some(r);
        share.finish();
    }

    /// After the worker's last take from `share`: wait for the blocks helpers are still running
    /// (once, for the signal of the last), put every block back in its place, and count the
    /// `joiners` posted. False when the signal was lost (every joiner gone with a block undone).
    fn gather(&mut self, share: &Share, rx: &mpsc::Receiver<()>, joiners: usize, count: &mut PackCount, split: &mut RoundSplit) -> bool {
        let home = share.done.load(Ordering::Acquire) == share.cells.len() || rx.recv().is_ok();
        for cell in &share.cells {
            if let Some((k, r, _)) = cell.lock().unwrap_or_else(|e| e.into_inner()).take() {
                self.runs[k as usize] = Some(r);
            }
        }
        let joined = share.joined.load(Ordering::Acquire);
        split.wake_ns += share.wake_ns.load(Ordering::Acquire);
        split.wake_max_ns = split.wake_max_ns.max(share.wake_max_ns.load(Ordering::Acquire));
        split.wake_n += joined;
        split.helper_run_ns += share.run_ns.load(Ordering::Acquire);
        count.helped += joined;
        count.claimed += (joiners as u64).saturating_sub(joined);
        home
    }

    /// A stage's blocks in the order they are taken (`Share`), and how many take them, the
    /// worker's turn among them: at most `helpers + 1`. By measured cost, the heaviest first, in
    /// no more takers than the stage's cost over its largest block's: past that the stage still
    /// takes its largest block's time, and each further taker is one more helper the round wakes.
    /// Shuffled, at random, with every taker it may use.
    fn order(&self, ids: &[u32], helpers: usize, stage: usize) -> (Vec<u32>, usize) {
        let mut m = (helpers + 1).min(ids.len()).max(1);
        let mut order = ids.to_vec();
        let cost = |k: u32| self.runs[k as usize].as_ref().map_or(0.0, |r| r.cost);
        if let Some(seed) = self.shuffle {
            let mut z = seed ^ self.n.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (stage as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            for i in (1..order.len()).rev() {
                z ^= z << 13;
                z ^= z >> 7;
                z ^= z << 17;
                order.swap(i, (z % (i as u64 + 1)) as usize);
            }
            return (order, m);
        }
        order.sort_by(|&a, &b| cost(b).total_cmp(&cost(a)));
        let (total, largest) = order.iter().fold((0.0, 0.0f64), |(t, l), &k| (t + cost(k), l.max(cost(k))));
        if largest > 0.0 {
            m = m.min((total / largest).ceil() as usize).max(1);
        }
        (order, m)
    }

    pub(crate) fn next_point(&mut self, x: &mut [f64], pin_i: &mut [f64], _split: &mut RoundSplit) -> Option<(f64, u8)> {
        let stop = self.merge_stop();
        let side = self.side.as_deref_mut().expect("the merge side is home");
        side.stop = stop;
        side.next_point(x, pin_i)
    }

    pub(crate) fn end_merge(&mut self, split: &mut RoundSplit) {
        let side = self.side.as_deref_mut().expect("the merge side is home");
        side.end_merge();
        side.take_counts(split);
    }
}

impl MergeSide {
    /// Whether every point up to `t` is merged and every block has recorded through `t`: the
    /// merged stream stands at `t`. O(blocks).
    fn through(&self, t: f64) -> bool {
        self.merge.iter().all(|m| {
            let newest = m.queue.back().and_then(|rec| rec.t.last()).copied().unwrap_or(m.t0);
            let next = match m.queue.front() {
                Some(rec) if m.cur < rec.t.len() => Some(rec.t[m.cur]),
                _ => m.queue.get(1).and_then(|rec| rec.t.first()).copied(),
            };
            newest >= t && next.is_none_or(|n| n > t)
        })
    }

    /// The side as it stands at `t` (`through`), for a snapshot: the records of rounds after it
    /// dropped, no stop. O(blocks + their records after `t`).
    fn at(&self, t: f64) -> MergeSide {
        let mut side = self.clone();
        for m in &mut side.merge {
            while m.queue.back().is_some_and(|rec| rec.t.first().is_some_and(|&first| first > t)) {
                m.queue.pop_back();
            }
        }
        side.stop = f64::INFINITY;
        side
    }
}

impl MergeSide {
    /// The round's next point of the merged stream: its time and flags, with every global
    /// unknown's value written into `x` and, when the round carries them, every pin's current
    /// into `pin_i`. The merge stops at the earliest of the blocks' newest times and at its `stop`.
    fn next_point(&mut self, x: &mut [f64], pin_i: &mut [f64]) -> Option<(f64, u8)> {
        for m in &mut self.merge {
            m.recycle(1);
        }
        let tau = self.merge.iter().filter_map(Merge::next_time).fold(f64::INFINITY, f64::min);
        let limit = self.merge.iter().map(|m| m.queue.back().and_then(|rec| rec.t.last()).copied().unwrap_or(m.t0)).fold(self.stop, f64::min);
        if tau == f64::INFINITY || tau > limit {
            return None;
        }
        let pins = self.merge.iter().any(|m| m.pins);
        if pins {
            pin_i.fill(0.0);
        }
        for &g in &self.sums {
            x[g as usize] = 0.0;
        }
        let gap = tau - self.prev_point;
        let prev_owner = self.point_owners.first().copied().filter(|_| gap < 5e-9);
        if prev_owner.is_some() {
            self.close_points += 1;
            let bucket = if gap < 1e-12 { 0 } else if gap < 1e-10 { 1 } else { 2 };
            self.close_gap[bucket] += 1;
            let shared = self.point_owners.iter().any(|&b| self.merge[b as usize].next_time() == Some(tau));
            self.close_same_block += u64::from(shared);
        }
        self.point_owners.clear();
        let mut flags = 0;
        for (block, m) in self.merge.iter_mut().enumerate() {
            let rec = m.queue.front().expect("the merge limit covers tau in every block");
            let (k, p, set) = (m.own.len(), m.pin_ids.len(), m.set);
            let at = |n: usize| (rec.t[n], &rec.vals[n * k..(n + 1) * k], if m.pins { &rec.pin_i[n * p..(n + 1) * p] } else { &[][..] });
            if rec.t.get(m.cur) == Some(&tau) {
                self.point_owners.push(block as u32);
                let (_, v, i) = at(m.cur);
                for (j, (&g, &y)) in m.own.iter().zip(v).enumerate() {
                    if j < set {
                        x[g as usize] = y;
                    } else {
                        x[g as usize] += y;
                    }
                }
                for (&pin, &y) in m.pin_ids.iter().zip(i) {
                    pin_i[pin as usize] += y;
                }
                flags |= rec.flags[m.cur];
                m.cur += 1;
                continue;
            }
            // The merge limit keeps the next point at or beyond tau.
            let (ta, va, ia) = if m.cur == 0 { (m.t0, &m.vals0[..], &m.pins0[..]) } else { at(m.cur - 1) };
            let (tb, vb, ib) = at(m.cur);
            let w = if tb > ta { (tau - ta) / (tb - ta) } else { 1.0 };
            for (j, ((&g, &a), &b)) in m.own.iter().zip(va).zip(vb).enumerate() {
                if j < set {
                    x[g as usize] = a + (b - a) * w;
                } else {
                    x[g as usize] += a + (b - a) * w;
                }
            }
            if m.pins {
                for ((&pin, &a), &b) in m.pin_ids.iter().zip(ia).zip(ib) {
                    pin_i[pin as usize] += a + (b - a) * w;
                }
            }
        }
        if let Some(prev) = prev_owner {
            *self.close_pairs.entry((prev, self.point_owners[0])).or_default() += 1;
        }
        self.prev_point = tau;
        Some((tau, flags))
    }

    /// Recycles each block's exhausted buffers and keeps their last point for interpolation.
    fn end_merge(&mut self) {
        self.point_owners.clear();
        for m in &mut self.merge {
            m.recycle(0);
        }
    }

    fn take_counts(&mut self, split: &mut RoundSplit) {
        if self.close_points == 0 {
            return;
        }
        split.close_points += std::mem::take(&mut self.close_points);
        split.close_same_block += std::mem::take(&mut self.close_same_block);
        for (sum, n) in split.close_gap.iter_mut().zip(std::mem::take(&mut self.close_gap)) {
            *sum += n;
        }
        for (pair, n) in std::mem::take(&mut self.close_pairs) {
            *split.close_pairs.entry(pair).or_default() += n;
        }
    }
}

impl Rounds {
    /// Every pin's current at the blocks' newest points (a round's end: the same instant).
    pub(crate) fn currents_now(&mut self, out: &mut [f64]) {
        out.fill(0.0);
        for r in self.runs_mut() {
            let ids = r.pins.clone();
            let now = r.currents_now();
            for p in ids {
                out[p as usize] += now[p as usize];
            }
        }
    }

    /// The blocks' state into the whole circuit's solver: each global unknown from the block that
    /// solves it, each instance's device state by name.
    pub(crate) fn compose(&self, whole: &mut Solver) {
        for &g in &self.sums {
            whole.x[g as usize] = 0.0;
        }
        for r in self.runs() {
            compose_run(r, whole);
        }
    }

    /// Marks the end of the round block 0 has finished (where the run stands at a round start)
    /// for a snapshot of every block there in `whole`, `compose`'s, bit for bit: block 0 now,
    /// each later stage as it exports that round (`pass_marker`), a stage already past it now.
    /// The stages pass in their order, which is the blocks' (`check`), so the summed unknowns add
    /// up in `compose`'s order. No stage is caught up for it: `take_marked` has it once every stage
    /// has passed. A marker set before is dropped. O(the blocks' unknowns and instances) in all,
    /// spread over the stages' exports.
    pub(crate) fn mark(&mut self, mut whole: Solver) {
        for &g in &self.sums {
            whole.x[g as usize] = 0.0;
        }
        compose_run(self.ctl(), &mut whole);
        let mut marker = Box::new(Marker { m: self.ctl_done, whole, passed: 0 });
        while marker.passed < self.stages.len() && self.done[marker.passed] >= marker.m {
            for &k in &self.stages[marker.passed] {
                compose_run(self.runs[k as usize].as_deref().expect("a stage past the marker's round is in its place"), &mut marker.whole);
            }
            marker.passed += 1;
        }
        self.marker = Some(marker);
    }

    /// Stage `s` has exported round `m`: a marker on that round takes its blocks.
    fn pass_marker(&mut self, s: usize, m: u64) {
        let Rounds { marker, stages, runs, .. } = self;
        let Some(marker) = marker.as_deref_mut().filter(|k| k.m == m && k.passed == s) else { return };
        for &k in &stages[s] {
            compose_run(runs[k as usize].as_deref().expect("a block exports in its place"), &mut marker.whole);
        }
        marker.passed += 1;
    }

    /// The marker's snapshot (`mark`) once every stage has passed its round, taken out.
    pub(crate) fn take_marked(&mut self) -> Option<Solver> {
        if self.marker.as_ref().is_some_and(|k| k.passed == self.stages.len()) {
            return self.marker.take().map(|k| k.whole);
        }
        None
    }

    /// Drops a marker (`mark`): the plan it was for goes nowhere.
    pub(crate) fn unmark(&mut self) {
        self.marker = None;
    }

    /// Marks the end of the round block 0 has finished for a kept snapshot of the rounds there,
    /// as a flush leaves them (`LiveSim::keep_snapshot`): block 0 and its windows now, each later
    /// stage's blocks and windows as it exports that round (`pass_keep`), and the merge side once
    /// the merged stream stands at that round's end (`take_kept`): the merge stops there until
    /// then. A plan's marker on its way travels with it, passed by the stages as the live one is.
    /// No stage is caught up for it. O(block 0's state and the windows) now, each later stage's
    /// own as it passes.
    pub(crate) fn mark_keep(&mut self) {
        let (rounds, bytes) = crate::alloc::measure(|| Box::new(self.skeleton()));
        let mut keep = Box::new(KeepMarker { m: self.ctl_done, t: self.ctl().st.t(), rounds, passed: 0, bytes: bytes.unwrap_or(0) });
        while keep.passed < self.stages.len() && self.done[keep.passed] >= keep.m {
            Self::copy_stage(&self.stages, &self.exports, &self.runs, &self.windows, keep.passed, &mut keep);
        }
        if let Some(side) = self.side.as_deref_mut() {
            side.stop = keep.t;
        }
        self.keep = Some(keep);
    }

    /// The rounds as they stand at a round start with every stage caught up, from block 0 as it
    /// is: the later stages' blocks to come (`pass_keep`), no work out, no merge side yet.
    fn skeleton(&self) -> Rounds {
        Rounds {
            runs: self.runs.iter().enumerate().map(|(k, r)| if k == 0 { r.clone() } else { None }).collect(),
            id: self.id, side: None, pins: self.pins, merge_flight: InFlight(None), rows: MergeRows::default(),
            merge_x: Vec::new(), merge_pin_i: Vec::new(), stages: self.stages.clone(), slot: self.slot.clone(), queue: VecDeque::new(),
            ctl_done: self.ctl_done, done: vec![self.ctl_done; self.stages.len()], flight: InFlight(None),
            ends: self.ends.clone(), start: self.start, failed: None, round: self.round, shuffle: self.shuffle, checks: self.checks.clone(), posted: (0, false), packs_last: self.packs_last,
            pipeline: self.pipeline, edges: self.edges, hub_screen: self.hub_screen, own_bound: self.own_bound.clone(), own: self.own.clone(), grids: self.grids.clone(),
            plain: self.plain, heard: self.heard.clone(), heard_by: self.heard_by.clone(), n: self.n, couplings: self.couplings.clone(),
            delays: self.delays.clone(), windows: self.windows.clone(), exports: self.exports.clone(),
            imports: self.imports.clone(), t0: self.t0, end: self.end, sums: self.sums.clone(), cut: self.cut.clone(),
            loops: self.loops.clone(), reprove: self.reprove.clone(), marker: self.marker.clone(), keep: None,
            sigs: self.sigs.clone(), hold: self.hold, breach: None, voltage: self.voltage.clone(), late_at: self.late_at.clone(),
            shadows: self.shadows.clone(), hub: self.hub.clone(),
        }
    }

    /// Stage `s`'s blocks and the windows they export into the kept snapshot (`mark_keep`).
    fn copy_stage(stages: &[Vec<u32>], exports: &[Vec<u32>], runs: &[Option<Box<Run>>], windows: &[Window], s: usize, keep: &mut KeepMarker) {
        let ((), bytes) = crate::alloc::measure(|| {
            for &k in &stages[s] {
                let k = k as usize;
                keep.rounds.runs[k] = Some(Box::new(runs[k].as_deref().expect("a stage past the snapshot's round is in its place").clone()));
                for &c in &exports[k] {
                    keep.rounds.windows[c as usize] = windows[c as usize].clone();
                }
            }
        });
        keep.bytes += bytes.unwrap_or(0);
        keep.passed += 1;
    }

    /// Stage `s` has exported round `m`: a kept snapshot on that round takes its blocks, and the
    /// plan's marker it carries takes them on that marker's round.
    fn pass_keep(&mut self, s: usize, m: u64) {
        let Rounds { keep, stages, exports, runs, windows, .. } = self;
        let Some(keep) = keep.as_deref_mut() else { return };
        if let Some(marker) = keep.rounds.marker.as_deref_mut().filter(|k| k.m == m && k.passed == s) {
            for &k in &stages[s] {
                compose_run(runs[k as usize].as_deref().expect("a block exports in its place"), &mut marker.whole);
            }
            marker.passed += 1;
        }
        if keep.m == m && keep.passed == s {
            Self::copy_stage(stages, exports, runs, windows, s, keep);
        }
    }

    /// The kept snapshot's rounds (`mark_keep`) once every stage has passed its round and the
    /// merged stream stands at its end, the merge side copied there, and the merge's stop lifted;
    /// with the time and the bytes it took in all. None until then. O(the merge side) then.
    pub(crate) fn take_kept(&mut self) -> Option<(Box<Rounds>, f64, u64)> {
        let keep = self.keep.as_deref()?;
        let side = self.side.as_deref()?;
        if keep.passed < self.stages.len() || !self.rows.t.is_empty() || !side.through(keep.t) {
            return None;
        }
        let mut keep = self.keep.take().expect("a kept snapshot on its way");
        let (side, bytes) = crate::alloc::measure(|| Box::new(side.at(keep.t)));
        keep.rounds.side = Some(side);
        self.unstop();
        Some((keep.rounds, keep.t, keep.bytes + bytes.unwrap_or(0)))
    }

    fn unstop(&mut self) {
        if let Some(side) = self.side.as_deref_mut() {
            side.stop = f64::INFINITY;
        }
    }

    /// Where the merge stops: a kept snapshot's round end while one is on its way.
    fn merge_stop(&self) -> f64 {
        self.keep.as_ref().map_or(f64::INFINITY, |k| k.t)
    }

    /// Each block's work since these rounds began, in block order.
    pub(crate) fn work(&self) -> Vec<BlockWork> {
        self.runs().zip(&self.cut.blocks).map(|(r, b)| BlockWork {
            name: b.name.clone(),
            stage: b.stage,
            unknowns: r.s.n,
            stats: r.s.stats,
            restarts: r.st.restarts,
            cost_ns: r.cost,
            first_ns: r.first_ns,
            first_n: r.first_n,
            later_ns: r.later_ns,
            later_n: r.later_n,
            worker_ns: r.worker_ns,
            worker_rounds: r.worker_rounds,
            helper_ns: r.helper_ns,
            helper_rounds: r.helper_rounds,
        }).collect()
    }

    pub(crate) fn restarts(&self) -> Restarts {
        let mut sum = Restarts::default();
        for r in self.runs() {
            sum += r.st.restarts;
        }
        sum
    }
}
