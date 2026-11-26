//! Blocks for a run in rounds (ESIMRT S4): the held step's islands (`held`), each its own circuit
//! and `Solver`, joined by boundary sources (`dev::boundary`), with the round map as the proof of
//! the whole plan before it runs.
//!
//! - Every device goes to the island of the unknowns it loads (the rows it writes). One that
//!   loads only sources, sinks and ground (an ideal source fixing a rail) is copied into every
//!   block that uses its unknowns. A node a device only reads in another island is a boundary
//!   voltage `<node>#in`, read in the same round.
//! - At a node in the hub form (a pad or a declared supply, `held`) each consumer reads the node in
//!   the same round through a boundary voltage `<node>#in`, and the node's block takes what the
//!   consumer's block draws through it (that boundary's branch current) a round late, as a current
//!   boundary `<node>#ret<k>` (`k` the consumer's block). The holder keeps no ghost. A cut through
//!   an instance that is not a resistor or a capacitor is made only there.
//! - Elsewhere a resistor or capacitor of a weak cut at `s` goes to the soft side, with `s` a
//!   boundary there. The stiff side keeps the same element again (`<name>#ghost`) to a boundary at
//!   the element's far node, so the load on `s` stays inside the stiff side's matrix. The held
//!   test picks which way is read in the same round: the one whose late read forces the smaller
//!   error (the stiff side's `tau` against the soft side's `gain`). The other way is read one round
//!   late.
//!
//!   An echo corrects a ghost's late far-node voltage `X(t-D)` as the local voltage `x_s(t)`
//!   moves away from the value `u_far` the far block read. The ghost sees
//!   `X(t-D) + g (x_s(t) - u_far(t-D))`. At each accepted step the far block exports `g`
//!   from the inverse row for `X`, at the column of its voltage import's branch. The echo reads
//!   that gain `D` late. A gap in the far block's read history or gain table clears the correction
//!   for the round.
//!   An echo the size screen would export at a reader whose own crossings set a pitch corrects a
//!   cut that pitch hears. Once the plan passes its round map, such a cut's reader and far block
//!   merge instead (`merge_pitch_cuts`, Fable's Q22 option (c)); the plan is then taken again from
//!   there. The echo corrects the reader's late copy for its own moves, not for the far block's: an
//!   event there (a switch, a gate) reaches the reader late and moves its crossings for good.
//! - Reading a branch current across islands cannot be a boundary voltage, and a same-round cycle
//!   cannot be ordered: such islands merge, and the report says why.
//! - Block 0 is the control island (every GPIO pad), alone at stage 0: nothing it reads is late.
//!   Each other block's stage is one past the latest block it reads in the same round. Without a
//!   pad the first block that reads nothing in the same round is at stage 0.
//! - Once the plan passes its round map, a declared supply's block whose instances raise no event
//!   and that reads nothing in the same round but block 0 runs in block 0. Without a pad the first
//!   such block that reads nothing in the same round is block 0. A supply joins block 0 only after
//!   the map, so a merge the map makes into it (a loop through the supply) stays outside block 0
//!   when it reads more than block 0.
//! - Then small blocks (under `min_island` own unknowns) of one stage whose instances raise no
//!   event merge, in groups, so their fixed cost a step is paid once (`merge_small`).
//! - Neither reads the blocks or their map, so both are settled ahead and the plan is built and
//!   mapped once: the plan it gives when every map passes. When the settled plan's map fails,
//!   its link says what to take out. A supply placed in block 0 whose node the link reads across
//!   stays out of block 0 (every supply placed there, when block 0 is one of the link's blocks
//!   and it names none), and a group of small blocks the link's blocks hold is unbinned; the
//!   rest settle again from where settling began, and the plan is mapped once more. A link that
//!   touches nothing settled is the start's own: the plan goes back to where settling began, is
//!   mapped and merged there until it passes, and settles again; when the start had passed, the
//!   failure is in what settled, and every settled item comes out (a run logs it when the link is
//!   a return's carried path, which names no supply). A settled plan of a single
//!   stage takes every settled item out. A shape that passed its map is never mapped again.
//! - The round map proves the plan's loops; accuracy is the held step's per-cut tests. A per-cut
//!   rho does not see a loop that closes through another block's gain. Each value read a round
//!   late (a returned current scaled by the driving-point impedance at its node in the block that
//!   takes it, so every entry is volts per volt; a ghost's far node; a held node read late) is set
//!   to one in turn, the blocks are solved in stage order with the same-round reads fed forward,
//!   and every late value is read back: column `k` of `M(f)`, at the linearisation of `whole`'s
//!   state, on the band and on the matrix a step solves. At every point the loop gain a round is
//!   bounded by `g(A)` for the matrix `A` of entrywise magnitudes. It takes the smaller of the
//!   power and Collatz-Wielandt bounds (`n` = 16), so a follower's unit no longer sets the bound.
//!   It is judged by the held tests' two bars
//!   (`held::bar_failed`). Stability requires `g(|M|)` under `STABILITY`. The accuracy bound is
//!   `g(|M| W) / (1 - g(|M|))`. For each column, `W` takes the band envelope or the step's real
//!   weight at its maximum delay (`read_lags`, `Shape::wrong`). One late value gives the held
//!   tests' `late_wrong`. The bound must stay under `hub_tol` for the whole map and under
//!   `cut_tol` for the map over late values other than hub returns (`|M_o|`) through
//!   `PartOpts::band`. Above it only stability is judged. A loop through ghosts alone therefore
//!   keeps the ordinary bar. A value that only follows a late one (a mixer's sum following its
//!   supply) is on no loop and passes.
//!   Otherwise the entry `(i, j)` of the failing map that maximises `w_i |M_ij| x_j` (`x = A^n 1`,
//!   `w = 1^T A^n`), the heaviest link of the dominant loop, names the late value `j` whose two
//!   blocks (where it is solved and where it comes in) merge, and the map is taken again; one
//!   block always passes. A return into a hub node is carried by the port its block holds for the
//!   node, unless most of the entry reaches that block through a boundary that reads another
//!   block's export in the same round (`carried`): that block passes the return on within the
//!   round, so it and the return's block merge instead. The largest row sum of `|M|` (sensitivity) and each late value's largest
//!   entry from a return in the hub form (it follows that node with that gain) are printed and
//!   gate nothing.
//! - The held step takes its cuts at the present state and tests them in other states made from a
//!   fixed start and kept by what the board is set to (`held_states`, `states`): its rest, its own
//!   devices flipped, each pad's edge, rest and edge back, the switches a person sets. The round
//!   map passes both bars at the present state and stability with every switch a person sets
//!   flipped.
//! - Measured once, weighed twice: the held step judges a cut read a round late, the least a cut
//!   can be, and keeps its rho at each point. Once the blocks are placed in their stages, each
//!   value read late through a cut is weighed again from those numbers at the rounds its places
//!   give it with the pipeline on (`late_reads`, `reweigh`), before anything is built. One that
//!   fails and passes read one round late unpipelines the stages between (`RoundPlan::unpipelined`)
//!   where that round is estimated no slower than merging (`round_estimate`); otherwise its two
//!   blocks merge. The plan is then built once and mapped once; a cut the built plan reads later
//!   than its places said (`late_cuts`) merges too, counted as an anomaly.
//! - The imports' test (`charge_hold`, A-K1) lands a jump when its charge times hold or the cost
//!   of a step-wide ramp at the top of the judged band exceeds the import's bar at that amplitude.
//!   A landing reader keeps an event when the export steps by at least the voltage those figures
//!   require. Named landings and unjudged figures keep every event.
//!   The band figure uses the reader's voltage exports and the nets it owns that the design's
//!   output parts or bench voltage probes read. A live probe adds its net to the test when the
//!   run re-plans behind the sound.

use crate::analysis::held::{accuracy_band, bar_failed, held_states, in_band, judge, late_wrong, recirculated, weight, Bar, Held, Others, Seed, STABILITY};
use crate::analysis::states::{self, StateCache};
use crate::analysis::partition::{hz, Land, PartOpts};
use crate::circuit::{Boundary, BoundaryKind, SimCircuit};
use crate::newton::{SimDiag, Solver};
use crate::num::C64;
use crate::rounds::{fan_out, flipped, hold_switches, lag_rounds, seed_from, slot_of, switches, Block, CarriedPlan, CarryRefused, Chain, Coupling, Echo, Fanned, GainCoupling, Helpers, HolderPort, Lag, Planner, RoundPlan, Unit, Warm};
use crate::sparse::Layout;
use crate::stamp::{EvalCx, Stamps, UnkKind};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::f64::consts::PI;
use std::fmt::Write;
use std::ops::Range;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

/// A plan (its blocks, their couplings, the copied sources' shares and the ghosts of each cut
/// element, `RoundPlan`) with its solvers, what the round map found and the held search it was cut
/// from: each cut's verdicts and figures in the other states, the states named.
pub struct BlockPlan {
    pub plan: RoundPlan,
    pub solvers: Vec<Solver>,
    pub map: RoundMap,
    pub report: String,
    pub held: Held,
    /// The plan-time test of its imports (`charge_hold`): which land, which split, and which
    /// readers' tests were made by this plan.
    pub tested: Vec<ChargeHold>,
    /// The relay rule's count over the plan (`charge_hold`): the transfers, the paths followed and cut.
    pub relay: Relay,
    /// What it was built from, to build its cut again at other part values (`build_carried`):
    /// only a plan made in its key's states has it, and a plan built from one has none.
    pub carry: Option<Arc<Carry>>,
}

/// What a plan made in its key's states was built from, by what the held search and the round
/// map decided: the held step's islands and figures and the cuts the build reads, the islands'
/// merges and block 0 as the plan ended, the stages read one round apart, and what the import
/// tests decided (the lands, the chains, the tests themselves). Its figures are the source's, at
/// the source's part values; a plan built from it is proved again where it is built
/// (`build_carried`).
pub struct Carry {
    h: Held,
    cuts: Vec<Cut>,
    island: Vec<usize>,
    root: Vec<usize>,
    zero: Option<usize>,
    unpipelined: Vec<u32>,
    lands: Vec<(u32, u32)>,
    land_steps: Vec<f64>,
    chains: Vec<Chain>,
    tested: Vec<ChargeHold>,
    relay: Relay,
    /// The chains the source's planner landed by name (`PartOpts::land`), which its lands hold: a
    /// start on the carry learns them (`CarriedPlan::landed`).
    land: Vec<Land>,
}

/// What `build_carried` made of a carry: a plan that passed, one built that did not pass (and
/// why), or none built (and why).
pub enum Carried {
    Plan(Box<BlockPlan>),
    Refused(Box<RoundPlan>, String),
    Unbuilt(String),
}

/// What a cut's coupling may get wrong read late in a live run's plan (`PartOpts::cut_tol`,
/// `BlockPlanner::for_run`): Fast's floor, nothing new above -50 dB, as a linear error in a
/// coupling (10^(-50/20) to two figures). Stability's bar does not move with it, and Exact runs
/// no rounds.
pub const CUT_TOL: f64 = 3.2e-3;

/// A live run's planner: `plan_blocks` with `opts`, for a host that runs any circuit in rounds,
/// its other states kept in `states` (`plan_blocks_kept`), which its forks share, and made and
/// tested on `pool`'s cores (the host's background jobs) beside the thread that plans.
#[derive(Clone, Debug)]
pub struct BlockPlanner {
    pub opts: PartOpts,
    pub states: StateCache,
    pub pool: Option<Helpers>,
}

impl BlockPlanner {
    /// For `sim`, a live run, its plan held accurate to `band` Hz (`PartOpts::band`), which every
    /// caller states: [`take_band`] for a run whose takes leave at a recording's rate, as the host
    /// plans, or `f64::INFINITY` for everything the step resolves. It resolves up to half its step
    /// rate (`LiveSim::step_cap`), at its round's length (`LiveSim::round_len`), a cut's coupling
    /// under `CUT_TOL` and a return into block 0 under a tenth of the block's swing, islands of at
    /// least 8 unknowns; no pool until the host gives one.
    pub fn for_run(sim: &crate::livesim::LiveSim, band: f64) -> BlockPlanner {
        BlockPlanner { opts: PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: CUT_TOL, hub_tol: 0.1, fmin: 10.0, fmax: 0.5 / sim.step_cap(), band, per_octave: 4, min_island: 8, control: Vec::new(), supplies: Vec::new(), heard: Vec::new(), round: sim.round_len() }, states: StateCache::default(), pool: None }
    }
}

/// The band a run in rounds holds its plan accurate to when its takes leave at `recording_rate`,
/// Hz: half that rate, at most half its own `sample_rate` (both positive). The host plans each run
/// in it (`SimHost::run_band`); a rig that plays as the host does states the same.
pub fn take_band(sample_rate: f64, recording_rate: f64) -> f64 {
    sample_rate.min(recording_rate) / 2.0
}

impl Planner for BlockPlanner {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        self.plan_from(ckt, whole, t, None)
    }

    fn plan_from(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64, in_force: Option<&RoundPlan>) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let planned = plan_blocks_kept(ckt, whole, t, &self.opts, &self.states, self.pool.as_ref(), in_force)?;
        // a chain landed by a name the board no longer has is dropped (the report's line says so)
        if let Some(p) = &planned {
            self.opts.land.retain(|land| !p.relay.absent.contains(&land.born));
        }
        Ok(planned.map(|p| (p.plan, p.solvers)))
    }

    fn refuse_cut(&mut self, node: &str) {
        if !self.opts.never_cut.iter().any(|n| n == node) {
            self.opts.never_cut.push(node.to_string());
        }
    }

    fn land_chain(&mut self, land: &Land) {
        if !self.opts.land.contains(land) {
            self.opts.land.push(land.clone());
        }
    }

    fn warm(&self, ckt: &Arc<SimCircuit>, whole: &Solver, t: f64) -> Option<Warm> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let pool = self.pool.clone()?;
        let (ckt, whole) = (ckt.clone(), whole.clone());
        let (opts, cache, post) = (self.opts.clone(), self.states.clone(), pool.post.clone());
        let done = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, job_stop) = (done.clone(), stop.clone());
        post(Box::new(move || {
            crate::rounds::with_stop(job_stop, || warm_plan(&ckt, &whole, t, &opts, &cache, Some(&pool)));
            flag.store(true, Ordering::Release);
        }));
        Some(Warm::new(done, stop))
    }

    fn can_carry(&self, ckt: &SimCircuit, whole: &Solver) -> bool {
        can_carry(ckt, whole, &self.opts, &self.states)
    }

    fn carried(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64, from: Option<&Arc<Carry>>) -> Option<Result<CarriedPlan, CarryRefused>> {
        carry_plan(ckt, whole, t, &self.opts, &self.states, self.pool.as_ref(), from)
    }
}

/// Whether a start at `whole` can begin on the cut `cache` keeps for its circuit
/// (`states::circuit_key`): not Exact, and a cut kept. A key planned already carries too: its own
/// plan, made behind the carried one in its kept states, costs a plan there rather than a warm.
/// O(the circuit's size).
pub fn can_carry(ckt: &SimCircuit, whole: &Solver, opts: &PartOpts, cache: &StateCache) -> bool {
    let o = circuit_opts(ckt, opts);
    !ckt.opts.exact && cache.carry(states::circuit_key(ckt, whole, &o)).is_some()
}

/// A start's plan from `from` (a run made again: the carry on its timeline) or else the cut `cache`
/// keeps for its circuit, built and proved by `build_carried`: None with none, a refusal saying why
/// it was not taken.
pub fn carry_plan(ckt: &SimCircuit, whole: &Solver, t: f64, opts: &PartOpts, cache: &StateCache, pool: Option<&Helpers>, from: Option<&Arc<Carry>>) -> Option<Result<CarriedPlan, CarryRefused>> {
    let carry = match from {
        Some(c) => c.clone(),
        None => cache.carry(states::circuit_key(ckt, whole, &circuit_opts(ckt, opts)))?,
    };
    let why = match build_carried(ckt, whole, t, opts, &carry, pool) {
        Ok(Carried::Plan(p)) => return Some(Ok(CarriedPlan { plan: p.plan, solvers: p.solvers, landed: carry.land.clone(), carry })),
        Ok(Carried::Refused(_, why)) => format!("refused: {why}"),
        Ok(Carried::Unbuilt(why)) => why,
        Err(d) => d.message,
    };
    Some(Err(CarryRefused { carry, why }))
}

/// The round map of the plan that passed: the loop gains it proves, and what it prints beside them.
#[derive(Clone, Debug, Default)]
pub struct RoundMap {
    /// The values read a round late: the rows and columns of `M`.
    pub lagged: usize,
    /// Points taken, and points left out because a block's matrix is singular there.
    pub points: usize,
    pub singular: usize,
    /// The bound on the loop gain a round, `g(|M|)`, at its largest over the points, and where
    /// (passes under `STABILITY`).
    pub gain: (f64, String),
    /// The same over the late values that are not returns in the hub form, `g(|M_o|)`.
    pub gain_ordinary: (f64, String),
    /// What each gets wrong read late (`bars`), at its largest and where: under `hub_tol` for the
    /// whole map, `cut_tol` for its ordinary late values, over the key's states from a fixed
    /// start (`key_maps`), else at the present state.
    pub wrong: (f64, String),
    pub wrong_ordinary: (f64, String),
    /// Above the accuracy band, the largest whole-map loop gain and what each map gets wrong
    /// with full weights. These figures and their points are for the report alone.
    pub gain_above: (f64, String),
    pub wrong_above: (f64, String),
    pub wrong_ordinary_above: (f64, String),
    /// Gating nothing: the largest row sum of `|M|`, whose size depends on the columns' unit.
    pub sensitivity: f64,
    /// Gating nothing: per late value that is not a return in the hub form, its largest one-round
    /// entry from such a return, the largest first: (the late value, the return's node, the gain,
    /// the point).
    pub followers: Vec<(String, String, f64, String)>,
    /// The merges the map made before it passed, and the maps taken.
    pub merges: usize,
    pub maps: usize,
    /// Before each build, the cuts re-weighed at the lateness their places give them: the late
    /// cuts whose stages were unpipelined, and those whose blocks merged (`reweigh`).
    pub joins: usize,
    pub reweighed: usize,
    /// The map's merges for a cut's lateness after it was re-weighed before the build: each is
    /// a plan read later than its places said, an anomaly.
    pub anomalies: usize,
    /// Wall time of the maps taken, ms.
    pub ms: f64,
    /// The most rounds a cut's late value is read late in the plan (`late_cuts`).
    pub late_rounds: u64,
    /// From a fixed start (`plan_blocks_kept`): the states the map was judged in beside the base,
    /// and of the maps in the key's states those made and those kept from before.
    pub states: usize,
    pub key_maps: usize,
    pub kept: usize,
    /// The present state's guard: the merges its map made on the stability bar, and what it gets
    /// wrong read late there as a share of its bar (over one, counted and gating nothing), and where.
    pub guard_merges: usize,
    pub present: (f64, String),
}

impl RoundMap {
    /// Include the peaks above the band from another map of the same plan, naming its state.
    fn include_above(&mut self, other: &RoundMap, state: &str) {
        for (peak, at) in [(&mut self.gain_above, &other.gain_above), (&mut self.wrong_above, &other.wrong_above),
            (&mut self.wrong_ordinary_above, &other.wrong_ordinary_above)] {
            if !at.1.is_empty() && (peak.1.is_empty() || at.0 > peak.0) {
                *peak = (at.0, format!("{}, {state}", at.1));
            }
        }
    }
}

/// The keep rule's band (`plan_blocks_kept`): a cut made at or under its accuracy bar is undone
/// only past this many times it, so a figure drifting about its bar (a pot turned) does not move
/// the plan each way it crosses.
pub const KEEP: f64 = 1.25;

/// The power `n` used by both bounds in `loop_gain`.
const LOOP_POWER: usize = 16;

/// What one device of the whole solver loads and reads.
struct Dev {
    name: String,
    kind: &'static str,
    loaded: Vec<u32>,
    read: Vec<u32>,
}

/// A held node's cut, in the held step's islands.
#[derive(Clone)]
struct Cut {
    node: u32,
    /// The hub form (a pad or a declared supply): consumers return their current a round late,
    /// with no ghost.
    hub: bool,
    stiff_first: bool,
    /// The node's rho, the band's and the step's largest.
    rho: f64,
    /// Each consumer island and its rho.
    soft: BTreeMap<usize, f64>,
    /// Every consumer instance.
    insts: BTreeSet<String>,
    /// Per test point, its rho in the states its accuracy bar holds in (`HeldNode::acc_rho`), and
    /// that bar.
    acc_rho: Vec<f64>,
    bar: f64,
}

/// Where the devices went, for one assignment of islands to blocks.
#[derive(Default)]
struct Placed {
    /// Per device: its block root, or `None` when it is copied where needed.
    home: Vec<Option<usize>>,
    /// (block root, global unknown, cut): an unknown read through a boundary.
    imports: BTreeSet<(usize, u32, Option<u32>)>,
    /// (device, stiff block root): a ghost of a cut element.
    ghosts: Vec<(usize, usize)>,
    /// (consumer block root, cut): what a consumer draws at a node in the hub form, returned there.
    returns: BTreeSet<(usize, u32)>,
}

/// A boundary source of a block: its name, kind, local node and branch (0 for a current; a holder
/// port's is its voltage boundary's), and its port memories (0 for each absent memory).
#[derive(Clone)]
struct Bound {
    name: String,
    kind: BoundaryKind,
    node: u32,
    branch: u32,
    w: u32,
    w2: u32,
}

/// A built plan before its round map.
struct Built {
    plan: RoundPlan,
    solvers: Vec<Solver>,
    /// Per block: the root island it is.
    roots: Vec<usize>,
    bounds: Vec<Vec<Bound>>,
    /// Per coupling: the node of a return in the hub form (a pad or a declared supply), and the
    /// coupling's name.
    hub: Vec<Option<String>>,
    labels: Vec<String>,
}

fn find(root: &mut [usize], mut i: usize) -> usize {
    while root[i] != i {
        root[i] = root[root[i]];
        i = root[i];
    }
    i
}

/// Cuts `ckt`, as `whole` holds it at time `t`, into blocks. `None` for Exact, or when the plan
/// that passes the round map has a single stage. `circuit_opts` adds the circuit's control nodes,
/// declared supplies and heard nets to the plan options.
pub fn plan_blocks(ckt: &SimCircuit, whole: &Solver, t: f64, opts: &PartOpts) -> Result<Option<BlockPlan>, SimDiag> {
    plan_blocks_kept(ckt, whole, t, opts, &StateCache::default(), None, None)
}

/// `plan_blocks` with the other states kept in `cache` by what the board is set to
/// (`states::key`): a plan whose key is kept takes its states and the tests made in them. A miss
/// makes them, and every plan tests in them, on `pool`'s cores beside the caller (`fan_out`); the
/// plan is the same on any pool or none.
///
/// With `in_force` (the plan the run runs) the keep rule holds from a fixed start: a cut in force
/// is undone only past `KEEP` times its accuracy bar, in the held tests and, for a plan cut as
/// that one is, in the key's maps. The plan in force is the run's, so a replay keeps the same.
pub fn plan_blocks_kept(ckt: &SimCircuit, whole: &Solver, t: f64, opts: &PartOpts, cache: &StateCache, pool: Option<&Helpers>, in_force: Option<&RoundPlan>) -> Result<Option<BlockPlan>, SimDiag> {
    plan_in_states(ckt, whole, t, opts, cache, pool, in_force)
}

/// What a plan of the board as it is now pays first, made ahead on a pool when a board is opened
/// (`Planner::warm`): its plan at `t` in the key's states from a fixed start, made and dropped, so
/// the states, the tests made in them and its round maps are kept in `cache` and the next plan of
/// the key pays no miss (`StateCache::planned`). A key planned already costs nothing; one whose
/// fixed start cannot be made keeps nothing, and one that does not plan says why in the run's own.
/// A run waiting for its set-up warms when its first plan would be due (`LiveSim::set_planner`).
/// Whether the key is planned when it returns.
pub fn warm_plan(ckt: &SimCircuit, whole: &Solver, t: f64, opts: &PartOpts, cache: &StateCache, pool: Option<&Helpers>) -> bool {
    if crate::rounds::stopped() || ckt.opts.exact {
        return false;
    }
    let key = states::key(ckt, whole, &circuit_opts(ckt, opts));
    if !cache.planned(key) {
        let _ = plan_in_states(ckt, whole, t, opts, cache, pool, None);
    }
    cache.planned(key)
}

/// Tests the plan in the key's cached states or makes them from a fixed start. If the fixed
/// start fails, it tests in states from the run and leaves them out of the cache. A stopped plan
/// (`rounds::with_stop`) ends at its next unit: states it finished stay kept for their key, as
/// any made states are, and it never marks the key planned.
fn plan_in_states(ckt: &SimCircuit, whole: &Solver, t: f64, opts: &PartOpts, cache: &StateCache, pool: Option<&Helpers>, in_force: Option<&RoundPlan>) -> Result<Option<BlockPlan>, SimDiag> {
    if ckt.opts.exact {
        return Ok(None);
    }
    let start = Instant::now();
    let o = circuit_opts(ckt, opts);
    // the cut is taken at the present state and tested in other states (`held_states`), made from
    // a fixed start and kept by key (`states`); a failure in a state makes the later ones' tests
    // stale. The round map passes at the present state and with every switch a person sets
    // flipped from it.
    let (user, own) = switches(ckt, whole);
    let span = 0.5 / o.fmax;
    let settle_start = Instant::now();
    let other = flipped(whole, &user, t, span);
    let key = states::key(ckt, whole, &o);
    let (others, kept) = match cache.get(key) {
        Some(x) => (x, true),
        None => {
            let made = Instant::now();
            let (list, notes, fixed) = match states::fixed(ckt, whole, &o, &user, &own, pool) {
                Ok((list, notes)) => (list, notes, true),
                Err(_) if crate::rounds::stopped() => return Err(Fanned::Stopped.diag("round plan")),
                Err(why) => (states::run_states(whole, &user, &own, t, span), vec![format!("from the run at t = {t:.4} s, not kept: {why}")], false),
            };
            let what = std::iter::once(if fixed { format!("from a fixed start, key {key:016x}") } else { "from the run".into() }).chain(notes).collect::<Vec<_>>().join("; ");
            let x = Arc::new(Others::new(list, whole, &o, what, made.elapsed().as_secs_f64() * 1e3, pool, fixed, own.clone())?);
            if fixed {
                cache.put(key, x.clone());
            }
            (x, false)
        }
    };
    let settle_ms = settle_start.elapsed().as_secs_f64() * 1e3;
    // from a fixed start the held step stands on the base and the present state only guards; from
    // the run, on the present state
    let mut h = match others.base() {
        Some((base, at)) => {
            // the keep rule: the cuts in force by node
            let mut kept = vec![false; whole.n + 1];
            for name in in_force.iter().flat_map(|p| &p.cuts) {
                if let Some(u) = whole.unknown(name) {
                    kept[u as usize] = true;
                }
            }
            held_states(&base, &others, at, &o, pool, Some((whole, t)), &kept)?
        }
        None => held_states(whole, &others, t, &o, pool, None, &[])?,
    };
    h.t = t;
    h.searched.settle_ms = settle_ms;
    let count = cache.count();
    let states_line = format!("{} {}; {} in {:.1} ms; {} of their {} tests taken from before; kept states: {} hits, {} misses, {} evicted, {} keys ({:.1} MB)",
        h.searched.states, others.what, if kept { "kept" } else { "made" }, if kept { settle_ms } else { others.ms }, h.searched.memo, h.searched.state_tests,
        count.hits, count.misses, count.evicted, count.keys, count.bytes as f64 / 1e6);
    if h.islands.len() < 2 {
        return Ok(None);
    }
    let held_ms = start.elapsed().as_secs_f64() * 1e3;
    let mut island = vec![usize::MAX; whole.n + 1];
    for (k, isl) in h.islands.iter().enumerate() {
        for &u in &isl.unknowns {
            island[u as usize] = k;
        }
    }
    let cuts: Vec<Cut> = h.nodes.iter().map(|n| Cut {
        node: n.node,
        hub: n.hub,
        stiff_first: n.stiff_first,
        rho: n.rho.iter().copied().fold(0.0, f64::max),
        soft: n.consumers.iter().map(|c| (c.island, c.rho)).collect(),
        insts: n.consumers.iter().flat_map(|c| c.insts.iter().cloned()).collect(),
        acc_rho: n.acc_rho.clone(),
        bar: n.bar,
    }).collect();
    let devs = devices(whole);
    let points = points(&h);
    // the unknowns each island holds: a block's cost before it is built (`round_estimate`)
    let sizes = |root: &mut [usize]| -> BTreeMap<usize, usize> {
        let mut out = BTreeMap::new();
        for &i in island.iter().filter(|&&i| i != usize::MAX) {
            *out.entry(find(root, i)).or_default() += 1;
        }
        out
    };
    let mut root: Vec<usize> = (0..h.islands.len()).collect();
    let mut log = Vec::new();
    let mut map = RoundMap::default();
    let mut zero = h.zero;
    // the (exporter, reader) islands of each late cut read one round late (`unpipelined`)
    let mut joined: Vec<(usize, usize)> = Vec::new();
    // the supplies' places and the small blocks read neither the blocks nor their map, so they are
    // settled ahead and the plan is built and mapped once; `settled` is where settling began
    let mut ahead = true;
    let mut settled: Option<(Vec<usize>, Option<usize>, usize, usize)> = None;
    let mut out_supply = BTreeSet::new();
    let mut out_bin = BTreeSet::new();
    let mut supplies: Vec<Vec<usize>> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    type ShapeKey = (Vec<usize>, Option<usize>, Vec<(usize, usize)>);
    let mut proven: Option<(ShapeKey, Built, RoundMap, RoundProof)> = None;
    // the plan-time test of each import (A-K1 revisit) on each plan that passes its map, which the
    // pitch rule reads (`merge_pitch_cuts`)
    let mut table_ms = 0.0;
    let (built, taken, proof, table, relay) = loop {
        let (placed, stages) = placed_stages(whole, &devs, &island, &cuts, &mut root, zero, &h, &mut log);
        if stages.len() < 2 {
            let Some((r, z, n, j)) = settled.take() else { return Ok(None) };
            out_supply.extend(supplies.drain(..).flatten());
            out_bin.extend(groups.drain(..).flatten());
            (root, zero, ahead) = (r, z, true);
            log.truncate(n);
            joined.truncate(j);
            log.push("the settled plan has a single stage: every placed supply stays out of block 0 and every group is unbinned".into());
            continue;
        }
        if ahead {
            let before = (root.clone(), zero, log.len(), joined.len());
            let moved = place_supplies(&h, &placed, &island, &cuts, &mut root, &mut zero, &mut log, &out_supply);
            if moved.is_empty() {
                let merged = merge_small(&h, &stages, &mut root, zero, o.min_island, &mut log, &out_bin);
                if !merged.is_empty() {
                    groups.extend(merged);
                    settled.get_or_insert(before);
                    continue;
                }
            } else {
                supplies.extend(moved);
                settled.get_or_insert(before);
                continue;
            }
        }
        // each cut at the lateness its place gives it, from the held step's numbers, before
        // anything is built: one that fails read as late as the pipeline reads it and passes one
        // round late unpipelines the stages between where that round is estimated no slower than
        // merging, and merges otherwise
        let unpipe = unpipelined(&stages, &joined, &mut root);
        let reads = late_reads(&placed, &island, &cuts, &mut root, &stages, &unpipe);
        if let Some(l) = reweigh(&reads, &cuts, &h, &stages) {
            let again = joined.iter().any(|&(a, b)| (find(&mut root, a), find(&mut root, b)) == (l.from, l.to));
            if l.one && stages[&l.to] >= 1 && !again {
                let mut with = joined.clone();
                with.push((l.from, l.to));
                let est_join = round_estimate(&stages, &unpipelined(&stages, &with, &mut root), &sizes(&mut root));
                let mut merged = root.clone();
                let (a, b) = (find(&mut merged, l.from), find(&mut merged, l.to));
                merged[a.max(b)] = a.min(b);
                let (_, merged_stages) = placed_stages(whole, &devs, &island, &cuts, &mut merged, zero, &h, &mut Vec::new());
                let est_merge = round_estimate(&merged_stages, &unpipelined(&merged_stages, &joined, &mut merged), &sizes(&mut merged));
                if est_join <= est_merge {
                    log.push(format!("{}; read one round late it passes: stages {} to {} run in one round (a round of an estimated {est_join:.0} unknowns one after another, {est_merge:.0} merged)",
                        l.why, stages[&l.to], stages[&l.from]));
                    joined.push((l.from, l.to));
                    map.joins += 1;
                    continue;
                }
                let why = format!("{}; one round late it passes, but merged its round is estimated shorter ({est_merge:.0} unknowns one after another, {est_join:.0} unpipelined)", l.why);
                merge(&mut root, l.from, l.to, &why, &h, &mut log);
            } else {
                merge(&mut root, l.from, l.to, &l.why, &h, &mut log);
            }
            map.reweighed += 1;
            continue;
        }
        let shape_key = |root: &mut [usize]| -> ShapeKey {
            let roots: Vec<usize> = (0..root.len()).map(|i| find(root, i)).collect();
            let z = zero.map(|i| roots[i]);
            let mut pairs: Vec<_> = joined.iter().map(|&(a, b)| (roots[a], roots[b])).collect();
            pairs.sort_unstable();
            (roots, z, pairs)
        };
        if proven.as_ref().is_some_and(|(key, ..)| *key == shape_key(&mut root)) {
            let (_, built, taken, proof) = proven.take().expect("this shape passed");
            map.states = taken.states;
            let table_start = Instant::now();
            let (table, relay) = charge_hold(&built, whole, &others, t, 0.5 / o.fmax, &o, &own, cache, key);
            table_ms += table_start.elapsed().as_secs_f64() * 1e3;
            if merge_pitch_cuts(&mut root, &h, &built, &relay, &mut log) {
                settled = None;
                continue;
            }
            break (built, taken, proof, table, relay);
        }
        let mut built = build(ckt, whole, t, &cuts, &devs, &island, &mut root, &placed, &stages)?;
        // the blocks are built from the circuit as lowered: the values set since, as the run has them
        for (k, s) in built.solvers.iter_mut().enumerate() {
            for (device, key, value) in &whole.edits {
                built.plan.set_param(k, s, device, key, *value);
            }
        }
        built.plan.unpipelined = unpipe;
        fit_ports_keyed(&mut built, t, &others, whole, &points, o.fmin / 100.0, &h, &mut log);
        let map_start = Instant::now();
        // the map at the present state is the plan's proof (its re-proofs after an edit start from
        // the run's state). From a fixed start the plan's shape is the key's: its map is judged in
        // the key's states (`key_maps`), and the present one only guards, on the stability bar,
        // what it gets wrong read late over a bar there gating nothing. From the run the present
        // map is judged on both bars.
        let keyed = others.base_seed().is_some();
        let (present, present_failed, proof) = round_map(&built, t, &points, o.fmin / 100.0, &h, !keyed, pool)?;
        map.maps += 1;
        let (mut taken, mut failed) = if keyed {
            let (mut taken, failed) = key_maps(&built, whole, &others, &points, o.fmin / 100.0, &h, if in_force.is_some_and(|p| p.same_partition(&built.plan)) { KEEP } else { 1.0 }, pool, &mut map)?;
            taken.include_above(&present, "the present state");
            let (hub, ordinary) = (present.wrong.0 / o.hub_tol, present.wrong_ordinary.0 / o.cut_tol);
            taken.present = if hub >= ordinary { (hub, present.wrong.1.clone()) } else { (ordinary, format!("{}, through ordinary late values", present.wrong_ordinary.1)) };
            let failed = failed.or_else(|| present_failed.map(|f| {
                map.guard_merges += 1;
                Fail { why: format!("the present state (a guard): {}", f.why), ..f }
            }));
            (taken, failed)
        } else {
            (present, present_failed)
        };
        // the cuts at their lateness again, on the plan as built: re-weighed before the build, one
        // that fails here is read later than its place said (an anomaly, counted) and merges
        let (late, late_rounds) = late_cuts(&built, &cuts, &h);
        taken.late_rounds = late_rounds;
        if let (None, Some(l)) = (&failed, late) {
            map.anomalies += 1;
            failed = Some(Fail { from: l.from, other: l.to, why: format!("{} (an anomaly: re-weighed before the build, it passed)", l.why), cuts: Vec::new(), carried: false });
        }
        // and with those switches flipped, stability only (where a device may saturate): a press
        // keeps the proof
        let mut flips: Vec<(usize, Solver)> = Vec::new();
        if let (None, Some(other)) = (&failed, &other) {
            flips = built.solvers.iter().enumerate().filter_map(|(k, s)| hold_switches(s, &user, None).map(|mut f| {
                seed_from(&mut f, other);
                (k, f)
            })).collect();
        }
        if !flips.is_empty() {
            flips.iter_mut().for_each(|(k, s)| std::mem::swap(&mut built.solvers[*k], s));
            let (at, why, _) = round_map(&built, t, &points, o.fmin / 100.0, &h, false, pool)?;
            flips.iter_mut().for_each(|(k, s)| std::mem::swap(&mut built.solvers[*k], s));
            failed = why.map(|f| Fail { why: format!("the switches a person sets flipped: {}", f.why), ..f });
            let flip = |(g, p): (f64, String)| (g, format!("{p}, the switches flipped"));
            taken.include_above(&at, "the switches flipped");
            taken.gain = if at.gain.0 > taken.gain.0 { flip(at.gain) } else { taken.gain };
            taken.gain_ordinary = if at.gain_ordinary.0 > taken.gain_ordinary.0 { flip(at.gain_ordinary) } else { taken.gain_ordinary };
            map.maps += 1;
        }
        map.ms += map_start.elapsed().as_secs_f64() * 1e3;
        if let Some(f) = failed {
            let (ra, rb) = (built.roots[f.from], built.roots[f.other]);
            let Some((r, z, n, j)) = settled.take() else {
                map.merges += 1;
                merge(&mut root, ra, rb, &f.why, &h, &mut log);
                continue;
            };
            let mut keep_out: Vec<&Vec<usize>> = Vec::new();
            if !supplies.is_empty() && zero.is_some_and(|z| [ra, rb].contains(&find(&mut root, z))) {
                let named: BTreeSet<usize> = f.cuts.iter().map(|&c| island[cuts[c as usize].node as usize]).filter(|&i| i != usize::MAX).collect();
                keep_out.extend(supplies.iter().filter(|s| s.iter().any(|i| named.contains(i))));
                if keep_out.is_empty() {
                    keep_out.extend(&supplies);
                }
            }
            let mut unbin: Vec<_> = groups.iter().filter(|g| [ra, rb].contains(&find(&mut root, g[0]))).collect();
            // the start passed, so the failure is in what settled; a pair carried along a return's
            // path names neither the supply nor the group, so all of them come out (it costs speed,
            // never the proof): logged, so a board that meets it is seen
            let all = keep_out.is_empty() && unbin.is_empty() && proven.is_some();
            if all {
                keep_out.extend(&supplies);
                unbin.extend(&groups);
            }
            if all && f.carried && (!supplies.is_empty() || !groups.is_empty()) {
                eprintln!("fab-elec plan: at t = {t:.4} s every supply stays out of block 0 and every group of small blocks is unbinned: the settled plan's map fails on a pair carried along a return's path, which names none of them ({})", f.why);
            }
            ahead = !keep_out.is_empty() || !unbin.is_empty();
            let action = if ahead {
                let names: Vec<_> = keep_out.iter().map(|s| supply_name(&h, s)).collect();
                out_supply.extend(keep_out.iter().flat_map(|s| s.iter().copied()));
                out_bin.extend(unbin.iter().flat_map(|g| g.iter().copied()));
                format!("supplies kept out of block 0: {}; groups unbinned: {}", if names.is_empty() { "none".into() } else { names.join(", ") }, unbin.len())
            } else {
                "map where settling began".into()
            };
            (root, zero) = (r, z);
            log.truncate(n);
            joined.truncate(j);
            supplies.clear();
            groups.clear();
            log.push(format!("the settled plan's map fails ({}): {action}", f.why));
            continue;
        }
        if !ahead {
            taken.states = map.states;
            proven = Some((shape_key(&mut root), built, taken, proof));
            ahead = true;
            continue;
        }
        let table_start = Instant::now();
        let (table, relay) = charge_hold(&built, whole, &others, t, 0.5 / o.fmax, &o, &own, cache, key);
        table_ms += table_start.elapsed().as_secs_f64() * 1e3;
        // a pitch reader's echo the screen would export: its cut merges, and the settled plan stands
        // (a later failure merges its pair)
        if merge_pitch_cuts(&mut root, &h, &built, &relay, &mut log) {
            (settled, proven) = (None, None);
            continue;
        }
        break (built, taken, proof, table, relay);
    };
    map = RoundMap { merges: map.merges, maps: map.maps, ms: map.ms, joins: map.joins, reweighed: map.reweighed, anomalies: map.anomalies,
        states: map.states, key_maps: map.key_maps, kept: map.kept, guard_merges: map.guard_merges, ..taken };
    let total = start.elapsed().as_secs_f64() * 1e3;
    let mut report = report(&h, &built, &cuts, &log, &map, [held_ms, total - held_ms - map.ms - table_ms, map.ms, table_ms], &states_line, &table);
    report.push_str(&charge_hold_report(&built, &table, &relay));
    // the chain shadow's trees: what a born import that splits reaches in the same round
    let trees = chains(&built.plan, &built.labels, &table, &relay.steps);
    let born_late = table.iter().filter(|x| x.jumps && !x.lands && built.plan.couplings[x.coupling].lag != Lag::Same).count();
    report.push_str(&chains_report(&built, born_late, &trees));
    let Built { mut plan, mut solvers, .. } = built;
    // an import whose jump would be held where its reader charges fast lands (A-K1), and every
    // pair before it on the jump's path: its jumps are not split, and pass on as events
    let mut landed: Vec<_> = table.iter().filter(|x| x.lands).map(|x| ((x.coupling as u32, x.reader as u32), x.step)).collect();
    landed.sort_unstable_by_key(|&(pair, _)| pair);
    (plan.lands, plan.land_steps) = landed.into_iter().unzip();
    plan.chains = trees;
    plan.switches = if other.is_some() { user } else { Vec::new() };
    plan.late_tol = proof.late_tol();
    plan.hub_tol = Some(proof.hub_tol());
    plan.proof = Some(Arc::new(proof));
    name_cuts(&mut plan, &h, &cuts);
    fit_echoes(&mut plan, &mut solvers, &relay, &mut report);
    // a plan made in the key's states: the next with the key pays no miss, and its cut is kept to
    // build again at other part values
    others.planned.store(true, std::sync::atomic::Ordering::Release);
    let carry = others.base().is_some().then(|| Arc::new(Carry {
        h: h.clone(), cuts, island, root, zero, unpipelined: plan.unpipelined.clone(), lands: plan.lands.clone(), land_steps: plan.land_steps.clone(), chains: plan.chains.clone(),
        tested: table.clone(), relay: relay.clone(), land: o.land.clone(),
    }));
    if let Some(c) = &carry {
        cache.keep_carry(states::circuit_key(ckt, whole, &o), c.clone());
    }
    Ok(Some(BlockPlan { plan, solvers, map, report, held: h, tested: table, relay, carry }))
}

/// The plan's cut nodes by name, per cut and all of them sorted (`RoundPlan::cut_nodes`, `cuts`).
fn name_cuts(plan: &mut RoundPlan, h: &Held, cuts: &[Cut]) {
    plan.cut_nodes = plan.couplings.iter().flat_map(|c| c.cuts.iter().map(|r| r.1)).map(|c| (c, h.names[cuts[c as usize].node as usize].clone())).collect();
    let mut at: Vec<String> = plan.cut_nodes.values().cloned().collect();
    at.sort();
    at.dedup();
    plan.cuts = at;
}

/// The cut `carry` was built from, built again on `ckt` as `whole` holds it at `t` (the circuit
/// at other part values), with the edits as the run has them, and proved there as `plan_in_states`
/// proves a plan after its build, but with no states: its round map at the present state on both
/// bars, the cuts at their lateness, and with the switches a person sets flipped (stability). Its
/// lands and chains are the carry's. Exact, or a carry that does not build or pass there, makes no
/// plan, and says why. A carried plan carries nothing on. Its cost: a build and two or three maps.
pub fn build_carried(ckt: &SimCircuit, whole: &Solver, t: f64, opts: &PartOpts, carry: &Carry, pool: Option<&Helpers>) -> Result<Carried, SimDiag> {
    if ckt.opts.exact {
        return Ok(Carried::Unbuilt("an Exact run has no plan".into()));
    }
    let start = Instant::now();
    let o = circuit_opts(ckt, opts);
    let (user, _) = switches(ckt, whole);
    let other = flipped(whole, &user, t, 0.5 / o.fmax);
    let (h, cuts, island) = (&carry.h, &carry.cuts, &carry.island);
    if island.len() != whole.n + 1 {
        return Ok(Carried::Unbuilt(format!("the carry has {} unknowns, the circuit {}", island.len() - 1, whole.n)));
    }
    let mut root = carry.root.clone();
    let devs = devices(whole);
    let (placed, stages) = placed_stages(whole, &devs, island, cuts, &mut root, carry.zero, h, &mut Vec::new());
    if stages.len() < 2 {
        return Ok(Carried::Unbuilt("its blocks place in a single stage".into()));
    }
    let mut built = match build(ckt, whole, t, cuts, &devs, island, &mut root, &placed, &stages) {
        Ok(b) => b,
        Err(d) => return Ok(Carried::Unbuilt(format!("it does not build: {}", d.message))),
    };
    for (k, s) in built.solvers.iter_mut().enumerate() {
        for (device, key, value) in &whole.edits {
            built.plan.set_param(k, s, device, key, *value);
        }
    }
    built.plan.unpipelined = carry.unpipelined.clone();
    built.plan.lands = carry.lands.clone();
    built.plan.land_steps = carry.land_steps.clone();
    built.plan.chains = carry.chains.clone();
    built.plan.switches = if other.is_some() { user.clone() } else { Vec::new() };
    name_cuts(&mut built.plan, h, cuts);
    if let Err(d) = crate::rounds::check(&built.plan, &built.solvers, whole) {
        return Ok(Carried::Unbuilt(format!("its plan does not check: {}", d.message)));
    }
    let points = points(h);
    let mut fits = Vec::new();
    fit_ports(&mut built, t, None, &points, o.fmin / 100.0, h, &mut fits);
    let (mut map, failed, proof) = round_map(&built, t, &points, o.fmin / 100.0, h, true, pool)?;
    let (late, late_rounds) = late_cuts(&built, cuts, h);
    map.late_rounds = late_rounds;
    let mut why = failed.map(|f| f.why).or(late.map(|l| format!("{} (a cut at its lateness)", l.why)));
    if let (None, Some(other)) = (&why, &other) {
        let mut flips: Vec<(usize, Solver)> = built.solvers.iter().enumerate().filter_map(|(k, s)| hold_switches(s, &user, None).map(|mut f| {
            seed_from(&mut f, other);
            (k, f)
        })).collect();
        if !flips.is_empty() {
            flips.iter_mut().for_each(|(k, s)| std::mem::swap(&mut built.solvers[*k], s));
            let (at, failed, _) = round_map(&built, t, &points, o.fmin / 100.0, h, false, pool)?;
            flips.iter_mut().for_each(|(k, s)| std::mem::swap(&mut built.solvers[*k], s));
            why = failed.map(|f| format!("the switches a person sets flipped: {}", f.why));
            map.include_above(&at, "the switches flipped");
            map.maps += 1;
        }
    }
    map.maps += 1;
    map.ms = start.elapsed().as_secs_f64() * 1e3;
    let Built { mut plan, mut solvers, .. } = built;
    plan.late_tol = proof.late_tol();
    plan.hub_tol = Some(proof.hub_tol());
    plan.proof = Some(Arc::new(proof));
    if let Some(why) = why {
        return Ok(Carried::Refused(Box::new(plan), why));
    }
    let mut report = format!("round plan at t = {t:.4} s, carried: {} blocks, {} couplings, the cut of a plan made at other part values, built and proved at the present state in {:.1} ms\n",
        plan.blocks.len(), plan.couplings.len(), map.ms);
    for line in &fits {
        let _ = writeln!(report, "  {line}");
    }
    fit_echoes(&mut plan, &mut solvers, &carry.relay, &mut report);
    Ok(Carried::Plan(Box::new(BlockPlan { plan, solvers, map, report, held: h.clone(), tested: carry.tested.clone(), relay: carry.relay.clone(), carry: None })))
}

/// `opts` with the circuit's own facts added: every GPIO pad and timing cell output to `control`,
/// every declared supply to `supplies` and every declared heard net to `heard`.
pub fn circuit_opts(ckt: &SimCircuit, opts: &PartOpts) -> PartOpts {
    let mut o = opts.clone();
    for node in ckt.devices.gpio.iter().map(|g| g.pad).chain(ckt.devices.cell.iter().map(|c| c.out)) {
        let name = &ckt.nodes[node as usize].name;
        if !o.control.contains(name) {
            o.control.push(name.clone());
        }
    }
    for node in &ckt.nodes {
        if node.supply && !o.supplies.contains(&node.name) {
            o.supplies.push(node.name.clone());
        }
        if node.heard && !o.heard.contains(&node.name) {
            o.heard.push(node.name.clone());
        }
    }
    o
}

/// Where a declared supply's block runs, once the plan passes its round map: in block 0 when none
/// of its instances raises an event and it reads nothing in the same round but block 0. Without a
/// pad the first such block that reads nothing in the same round is block 0. Placement and cost
/// only, the nodes keep their bars. Returns the islands of each block placed.
/// A block holding any island in `out` stays out of block 0.
fn place_supplies(h: &Held, placed: &Placed, island: &[usize], cuts: &[Cut], root: &mut [usize], zero: &mut Option<usize>, log: &mut Vec<String>, out: &BTreeSet<usize>) -> Vec<Vec<usize>> {
    let mut moved = Vec::new();
    loop {
        let z = zero.map(|z| find(root, z));
        let blocks: BTreeSet<usize> = placed.home.iter().flatten().map(|&b| find(root, b)).collect();
        let found = blocks.into_iter().filter(|&b| Some(b) != z).find_map(|b| {
            let mine: Vec<usize> = (0..h.islands.len()).filter(|&i| find(root, i) == b).collect();
            let qualifies = mine.iter().any(|&i| h.islands[i].declared) && !mine.iter().any(|&i| h.islands[i].events || out.contains(&i)) && placed.imports.iter().all(|imp| {
                let owner = find(root, island[imp.1 as usize]);
                find(root, imp.0) != b || owner == b || !same_round(imp, owner, cuts) || Some(owner) == z
            });
            qualifies.then_some((b, mine))
        });
        let Some((b, mine)) = found else { return moved };
        let named = supply_name(h, &mine);
        match z {
            Some(z) => merge(root, z, b, &format!("the declared supply {named} raises no event and reads only block 0"), h, log),
            None => {
                log.push(format!("block 0 is the declared supply {named}'s: no pad, and it reads nothing in the same round"));
                *zero = Some(b);
            }
        }
        moved.push(mine);
    }
}

/// The name of the declared supply node these islands hold (empty when they hold none).
fn supply_name<'a>(h: &'a Held, islands: &[usize]) -> &'a str {
    islands.iter().map(|&i| &h.islands[i]).filter(|isl| isl.declared).flat_map(|isl| &isl.unknowns)
        .map(|&u| &h.names[u as usize]).find(|name| h.supplies.contains(name)).map_or("", String::as_str)
}

/// Own unknowns a block of small blocks fills up to; more of them start another. The fixed cost of
/// a block's step (stepping, boundary tables, points for the merge, its share of a round's
/// bookkeeping) is paid once a group; the bound keeps a board of many small cells spread over
/// several blocks for the helpers.
const SMALL_GROUP: usize = 64;

/// Small blocks of one stage whose instances raise no event step together. A small island
/// (`min_island`) with a single reader already joins it (`held`); these are the ones no block reads
/// in the same round, such as a pad's indicator cells, each a block whose fixed cost a step is
/// over its arithmetic (drum16 at 96 kHz: about 0.33 us a step for any block, about 0.04 us an
/// unknown more). Each run of them merges into one block of at most `SMALL_GROUP` own unknowns.
/// They read nothing of each other in the same round (one stage), so the merge adds no order, and
/// it keeps every instance: what one of them read late of another becomes the same step's. Not
/// block 0, a declared supply's block (`place_supplies`) or a block with an event (a merged block
/// takes the union of its members' corners and crossings). Returns the islands of each merged group.
/// A block holding any island in `out` stays out of the bins.
fn merge_small(h: &Held, stages: &BTreeMap<usize, u32>, root: &mut [usize], zero: Option<usize>, min_island: usize, log: &mut Vec<String>, out: &BTreeSet<usize>) -> Vec<Vec<usize>> {
    let z = zero.map(|z| find(root, z));
    let mut by_stage: BTreeMap<u32, Vec<(usize, usize)>> = BTreeMap::new();
    for (&b, &stage) in stages {
        let b = find(root, b);
        if Some(b) == z {
            continue;
        }
        let mine: Vec<usize> = (0..h.islands.len()).filter(|&i| find(root, i) == b).collect();
        let own: usize = mine.iter().map(|&i| h.islands[i].unknowns.len()).sum();
        if own < min_island && !mine.iter().any(|&i| h.islands[i].events || h.islands[i].declared || h.islands[i].zero || out.contains(&i)) {
            by_stage.entry(stage).or_default().push((b, own));
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (stage, small) in by_stage {
        let (mut first, mut filled) = (None, 0);
        for (b, own) in small {
            match first {
                Some(f) if filled + own <= SMALL_GROUP => {
                    merge(root, f, b, &format!("small blocks at stage {stage} with no event step together"), h, log);
                    filled += own;
                    groups.entry(find(root, f)).or_default();
                }
                _ => (first, filled) = (Some(b), own),
            }
        }
    }
    if !groups.is_empty() {
        for i in 0..h.islands.len() {
            if let Some(group) = groups.get_mut(&find(root, i)) {
                group.push(i);
            }
        }
    }
    groups.into_values().filter(|g| !g.is_empty()).collect()
}

fn merge(root: &mut [usize], a: usize, b: usize, why: &str, h: &Held, log: &mut Vec<String>) {
    let (ra, rb) = (find(root, a), find(root, b));
    if ra != rb {
        let name = |r: usize| h.names[h.islands[r].unknowns[0] as usize].clone();
        log.push(format!("the islands of {} and {} merge: {why}", name(ra), name(rb)));
        let (lo, hi) = (ra.min(rb), ra.max(rb));
        root[hi] = lo;
    }
}

/// Each device of `whole`: the rows it writes, and what it reads only.
fn devices(whole: &Solver) -> Vec<Dev> {
    let mut out = Vec::new();
    let (mut unks, mut slots) = (Vec::new(), Vec::new());
    for bank in &whole.banks {
        for i in 0..bank.len() {
            unks.clear();
            slots.clear();
            bank.inst_unks(i, &mut unks);
            bank.inst_slots(i, &mut slots);
            let mut loaded: BTreeSet<u32> = slots.iter().map(|&s| whole.slot_row[s as usize]).filter(|&r| r != 0).collect();
            if slots.is_empty() {
                loaded.extend(unks.iter().copied().filter(|&u| u != 0));
            }
            let read: BTreeSet<u32> = unks.iter().copied().filter(|&u| u != 0 && !loaded.contains(&u))
                .chain(slots.iter().map(|&s| whole.slot_col[s as usize]).filter(|&c| c != 0 && !loaded.contains(&c)))
                .collect();
            out.push(Dev { name: bank.inst_name(i).to_string(), kind: bank.kind(), loaded: loaded.into_iter().collect(), read: read.into_iter().collect() });
        }
    }
    out
}

/// How a device that loads two blocks is cut, if it is a cut's consumer.
enum Form {
    /// At a node in the hub form: at home in `consumer`, reading the node, its current returned.
    Return { cut: u32, consumer: usize },
    /// A resistor or capacitor elsewhere: at home on the soft side, a ghost on the stiff side.
    Ghost { cut: u32, node: u32, far: u32 },
}

fn form(dev: &Dev, island: &[usize], cuts: &[Cut], root: &mut [usize]) -> Option<Form> {
    for (c, cut) in cuts.iter().enumerate() {
        if !cut.insts.contains(&dev.name) || !dev.loaded.contains(&cut.node) {
            continue;
        }
        let holder = find(root, island[cut.node as usize]);
        let others: BTreeSet<usize> = dev.loaded.iter().filter(|&&u| u != cut.node && island[u as usize] != usize::MAX)
            .map(|&u| find(root, island[u as usize])).collect();
        if others.len() != 1 || others.contains(&holder) {
            continue;
        }
        let consumer = *others.first().expect("one other block");
        if !cut.soft.keys().any(|&s| find(root, s) == consumer) {
            continue;
        }
        if cut.hub {
            return Some(Form::Return { cut: c as u32, consumer });
        }
        if matches!(dev.kind, "R" | "C") && dev.loaded.len() == 2 {
            let far = if dev.loaded[0] == cut.node { dev.loaded[1] } else { dev.loaded[0] };
            return Some(Form::Ghost { cut: c as u32, node: cut.node, far });
        }
    }
    None
}

/// Places every device, or names two islands that must merge and why.
fn place(whole: &Solver, devs: &[Dev], island: &[usize], cuts: &[Cut], root: &mut [usize]) -> Result<Placed, (usize, usize, String)> {
    let mut placed = Placed::default();
    for (d, dev) in devs.iter().enumerate() {
        let homes: BTreeSet<usize> = dev.loaded.iter().filter(|&&u| island[u as usize] != usize::MAX).map(|&u| find(root, island[u as usize])).collect();
        let home = match homes.len() {
            0 => None,
            1 => homes.first().copied(),
            _ => {
                let (a, b) = (*homes.first().unwrap(), *homes.last().unwrap());
                match form(dev, island, cuts, root) {
                    Some(Form::Return { cut, consumer }) => {
                        placed.imports.insert((consumer, cuts[cut as usize].node, Some(cut)));
                        placed.returns.insert((consumer, cut));
                        Some(consumer)
                    }
                    Some(Form::Ghost { cut, node, far }) => {
                        let (stiff, soft) = (find(root, island[node as usize]), find(root, island[far as usize]));
                        placed.imports.insert((soft, node, Some(cut)));
                        placed.imports.insert((stiff, far, Some(cut)));
                        placed.ghosts.push((d, stiff));
                        Some(soft)
                    }
                    None => return Err((a, b, format!("{} loads both", dev.name))),
                }
            }
        };
        if let Some(h) = home {
            for &u in &dev.read {
                let i = island[u as usize];
                if i == usize::MAX || find(root, i) == h {
                    continue;
                }
                if whole.unknowns[u as usize].kind == UnkKind::Current {
                    return Err((h, i, format!("{} reads the branch current {}", dev.name, whole.unknowns[u as usize].name)));
                }
                placed.imports.insert((h, u, None));
            }
        }
        placed.home.push(home);
    }
    Ok(placed)
}

/// Whether an import is read in the same round, from the exporter's block to the importer's.
fn same_round(imp: &(usize, u32, Option<u32>), owner: usize, cuts: &[Cut]) -> bool {
    match imp.2 {
        None => true,
        // a consumer reads a node in the hub form in the same round
        Some(c) if cuts[c as usize].hub => true,
        // the cut node goes to the soft side, the far node to the stiff side's ghost
        Some(c) => (imp.1 == cuts[c as usize].node) == cuts[c as usize].stiff_first && owner != imp.0,
    }
}

/// The stages of the blocks (by root), or two islands to merge: two blocks of a same-round cycle
/// joined by one of its edges, an island the control block reads in the same round, or blocks
/// whose returns go round in a cycle (each return's first value is its consumer's draw, so a
/// consumer's block is built before the block that takes its return).
fn order(placed: &Placed, island: &[usize], cuts: &[Cut], root: &mut [usize], control: Option<usize>)
    -> Result<BTreeMap<usize, u32>, (usize, usize, String)>
{
    let blocks: BTreeSet<usize> = placed.home.iter().flatten().copied().collect();
    let mut edges: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for imp in &placed.imports {
        let owner = find(root, island[imp.1 as usize]);
        if owner != imp.0 && same_round(imp, owner, cuts) {
            edges.entry(owner).or_default().insert(imp.0);
        }
    }
    let control = control.map(|c| find(root, c)).filter(|c| blocks.contains(c)).unwrap_or_else(|| {
        // no control island: the first block nothing reads in the same round
        let read: BTreeSet<usize> = edges.values().flatten().copied().collect();
        blocks.iter().copied().find(|b| !read.contains(b)).unwrap_or(*blocks.first().unwrap())
    });
    for (&from, to) in &edges {
        if to.contains(&control) {
            return Err((from, control, "the control block reads it in the same round".to_string()));
        }
    }
    // longest path from the control block over same-round edges; a cycle merges
    let mut stage: BTreeMap<usize, u32> = blocks.iter().map(|&b| (b, if b == control { 0 } else { 1 })).collect();
    for _ in 0..=blocks.len() {
        let mut changed = false;
        for (&from, to) in &edges {
            for &b in to {
                let s = stage[&from] + 1;
                if stage[&b] < s {
                    stage.insert(b, s);
                    changed = true;
                }
            }
        }
        if !changed {
            return returns_order(placed, island, cuts, root).map(|_| stage);
        }
    }
    // a cycle merges: the highest stage's same-round edge that lies on one (the highest stage's
    // first edge may lead off every cycle, and merge two blocks no ring joins)
    let reach = |from: usize| {
        let mut seen = BTreeSet::new();
        let mut todo = vec![from];
        while let Some(v) = todo.pop() {
            for &t in edges.get(&v).into_iter().flatten() {
                if seen.insert(t) {
                    todo.push(t);
                }
            }
        }
        seen
    };
    let mut froms: Vec<usize> = edges.keys().rev().copied().collect();
    froms.sort_by_key(|f| std::cmp::Reverse(stage[f]));
    let on_ring = froms.iter().find_map(|&from| edges[&from].iter().find(|&&to| reach(to).contains(&from)).map(|&to| (from, to)));
    let (from, to) = on_ring.expect("relaxing past the blocks' count leaves a cycle");
    Err((from, to, "a same-round cycle".to_string()))
}

/// The blocks (by root) in an order where every block that returns a current comes before the
/// block taking it, or two blocks of a cycle of returns.
fn returns_order(placed: &Placed, island: &[usize], cuts: &[Cut], root: &mut [usize]) -> Result<Vec<usize>, (usize, usize, String)> {
    let blocks: BTreeSet<usize> = placed.home.iter().flatten().copied().collect();
    let mut takes: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for &(consumer, c) in &placed.returns {
        let holder = find(root, island[cuts[c as usize].node as usize]);
        takes.entry(holder).or_default().insert(consumer);
    }
    let mut done: Vec<usize> = Vec::new();
    let mut left: BTreeSet<usize> = blocks;
    while !left.is_empty() {
        let ready: Vec<usize> = left.iter().copied().filter(|b| takes.get(b).is_none_or(|from| from.iter().all(|f| !left.contains(f)))).collect();
        if ready.is_empty() {
            let (&holder, from) = takes.iter().find(|(h, _)| left.contains(h)).expect("a block left takes a return");
            let consumer = *from.iter().find(|f| left.contains(f)).expect("from a block left");
            return Err((holder, consumer, "their returns go round in a cycle".to_string()));
        }
        for b in ready {
            left.remove(&b);
            done.push(b);
        }
    }
    Ok(done)
}

/// The linearisation of `s`'s banks at its `x` and time `t` (limiting and bypass off), on a copy of
/// the banks: `g`, `c` and the static currents `f`.
fn linearise(s: &Solver, t: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let cx = EvalCx { t, tran: true, init_junctions: false, limit: false, edge_hold: false, bypass: false, settle: false, ..s.base_cx() };
    let mut banks = s.banks.clone();
    let len = s.g.len();
    let (mut g, mut c) = (vec![0.0; len], vec![0.0; len]);
    let (mut f, mut q) = (vec![0.0; s.n + 1], vec![0.0; s.n + 1]);
    for b in banks.iter_mut() {
        let mut st = Stamps { g: &mut g, c: &mut c, f: &mut f, q: &mut q, noncon: 0, noncon_at: 0 };
        b.eval(&s.x, &cx, &mut st);
    }
    g[0] = 0.0;
    c[0] = 0.0;
    (g, c, f)
}

#[allow(clippy::too_many_arguments)]
fn build(ckt: &SimCircuit, whole: &Solver, t: f64, cuts: &[Cut], devs: &[Dev], island: &[usize], root: &mut [usize], placed: &Placed,
    stages: &BTreeMap<usize, u32>) -> Result<Built, SimDiag>
{
    // blocks by (stage, smallest unknown)
    let mut order: Vec<usize> = stages.keys().copied().collect();
    let first = |r: usize, root: &mut [usize]| (1..island.len()).find(|&u| island[u] != usize::MAX && find(root, island[u]) == r).unwrap_or(usize::MAX);
    let mut key: HashMap<usize, (u32, usize)> = HashMap::new();
    for &r in &order {
        key.insert(r, (stages[&r], first(r, root)));
    }
    order.sort_by_key(|r| key[r]);
    let index: HashMap<usize, usize> = order.iter().enumerate().map(|(k, &r)| (r, k)).collect();
    // the devices of each block, copies included where their unknowns are used
    let mut keep: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); order.len()];
    for (d, home) in placed.home.iter().enumerate() {
        if let Some(h) = home {
            keep[index[h]].insert(d);
        }
    }
    let copies: Vec<usize> = (0..devs.len()).filter(|&d| placed.home[d].is_none()).collect();
    for k in 0..order.len() {
        loop {
            let used: BTreeSet<u32> = keep[k].iter().flat_map(|&d| devs[d].loaded.iter().chain(&devs[d].read).copied()).collect();
            let add: Vec<usize> = copies.iter().copied().filter(|&d| !keep[k].contains(&d)
                && devs[d].loaded.iter().chain(&devs[d].read).any(|u| used.contains(u))).collect();
            if add.is_empty() {
                break;
            }
            keep[k].extend(add);
        }
    }
    for &d in &copies {
        if !keep.iter().any(|k| k.contains(&d)) {
            keep[0].insert(d);
        }
    }
    let mut imports: Vec<BTreeMap<u32, Option<u32>>> = vec![BTreeMap::new(); order.len()];
    for &(r, u, c) in &placed.imports {
        imports[index[&r]].insert(u, c);
    }
    // a copied device's reads from another block are imports too
    for k in 0..order.len() {
        for &d in &keep[k] {
            if placed.home[d].is_some() {
                continue;
            }
            for &u in &devs[d].read {
                if island[u as usize] != usize::MAX && index[&find(root, island[u as usize])] != k {
                    imports[k].entry(u).or_insert(None);
                }
            }
        }
    }
    let mut ghosts_of: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    for &(d, stiff) in &placed.ghosts {
        ghosts_of[index[&stiff]].push(d);
    }
    // per block: the returns it takes, (cut, consumer block)
    let mut returns: Vec<Vec<(u32, usize)>> = vec![Vec::new(); order.len()];
    for &(r, c) in &placed.returns {
        let holder = index[&find(root, island[cuts[c as usize].node as usize])];
        returns[holder].push((c, index[&r]));
    }
    // who solves each global unknown: its island's block, else the first block holding it
    let mut owner = vec![usize::MAX; island.len()];
    for u in 1..island.len() {
        if island[u] != usize::MAX {
            owner[u] = index[&find(root, island[u])];
        }
    }
    for (k, devices) in keep.iter().enumerate() {
        for &d in devices {
            for &u in devs[d].loaded.iter().chain(&devs[d].read) {
                if owner[u as usize] == usize::MAX {
                    owner[u as usize] = k;
                }
            }
        }
    }
    let name_of = |u: u32| whole.unknowns[u as usize].name.clone();
    let names: Vec<String> = order.iter().map(|&r| name_of(first(r, root) as u32)).collect();
    // One coupling per exported voltage, read on time only when every importer runs later.
    // Echoes use these indices before the solvers supply the exporter-local unknowns.
    let mut by_export: BTreeMap<u32, (Coupling, String)> = BTreeMap::new();
    for (k, imp) in imports.iter().enumerate() {
        for (&u, &cut) in imp {
            let from = owner[u as usize];
            let (coupling, _) = by_export.entry(u).or_insert_with(|| (Coupling { from: (from as u32, 0), to: Vec::new(), lag: Lag::Same, cuts: Vec::new(), rho: 0.0 }, name_of(u)));
            coupling.to.push((k as u32, format!("{}#in", name_of(u))));
            // each reader across its own cut: one value's readers can each have one
            if let Some(c) = cut {
                coupling.cuts.push((k as u32, c));
                coupling.rho = coupling.rho.max(cuts[c as usize].rho);
            }
            if stages[&order[from]] >= stages[&order[k]] {
                coupling.lag = Lag::Round;
            }
        }
    }
    let mut echoes = Vec::new();
    let mut subs = Vec::with_capacity(order.len());
    let mut ghosts: BTreeMap<String, Vec<(u32, String)>> = BTreeMap::new();
    for k in 0..order.len() {
        let keep_names: BTreeSet<&str> = keep[k].iter().map(|&d| devs[d].name.as_str()).collect();
        let mut sub = ckt.subcircuit(|n| keep_names.contains(n));
        for &d in &ghosts_of[k] {
            let name = &devs[d].name;
            let ghost = format!("{name}#ghost");
            let node = |sub: &mut SimCircuit, n: u32| sub.node(&ckt.nodes[n as usize].name);
            if let Some(res) = ckt.devices.r.iter().find(|e| e.name == *name) {
                let mut e = res.clone();
                (e.name, e.a, e.b) = (ghost.clone(), node(&mut sub, res.a), node(&mut sub, res.b));
                sub.devices.r.push(e);
            } else if let Some(cap) = ckt.devices.c.iter().find(|e| e.name == *name) {
                let mut e = cap.clone();
                (e.name, e.a, e.b) = (ghost.clone(), node(&mut sub, cap.a), node(&mut sub, cap.b));
                sub.devices.c.push(e);
            }
            ghosts.entry(name.clone()).or_default().push((k as u32, ghost));
        }
        for &u in imports[k].keys() {
            let name = name_of(u);
            let node = sub.node(&name);
            sub.devices.boundary.push(Boundary { name: format!("{name}#in"), node, kind: BoundaryKind::Voltage, v0: whole.x[u as usize] });
        }
        if !ghosts_of[k].is_empty() {
            let far: BTreeSet<u32> = ghosts_of[k].iter().flat_map(|&d| devs[d].loaded.iter().copied())
                .filter(|u| imports[k].contains_key(u)).collect();
            // The voltage couplings come first in this order, followed by the hub returns.
            for (of, (&f, (back, far_name))) in by_export.iter().enumerate() {
                if back.lag == Lag::Same || !far.contains(&f) || !back.to.iter().any(|to| to.0 as usize == k) { continue; }
                let input = format!("{far_name}#in");
                let boundary = sub.devices.boundary.iter().position(|b| b.name == input && b.kind == BoundaryKind::Voltage).expect("ghost imports its far node");
                for (q, (&u, (c, own))) in by_export.iter().enumerate() {
                    if c.from.0 as usize != k || c.lag != Lag::Same || !c.to.iter().any(|to| to.0 == back.from.0) { continue; }
                    let node = sub.node(own);
                    let name = format!("{far_name}#echo{node}");
                    sub.devices.boundary.push(Boundary { name: name.clone(), node, kind: BoundaryKind::Echo { of: boundary as u32 }, v0: whole.x[u as usize] });
                    echoes.push(Echo { block: k as u32, name, of: of as u32, coupling: q as u32, g: 0.0, gain: None });
                }
            }
        }
        subs.push(sub);
    }
    // each hub consumer's holder port at the node it returns its current into (`HolderPort`):
    // inert until it is fitted (`fit_ports`), its first value the return's
    let mut ports = Vec::new();
    for (k, taken) in returns.iter().enumerate() {
        for &(c, j) in taken {
            let name = name_of(cuts[c as usize].node);
            let Some(of) = subs[j].devices.boundary.iter().position(|b| b.name == format!("{name}#in")) else { continue };
            let node = subs[j].devices.boundary[of].node;
            let second = ckt.find_node(&name).is_some_and(|n| ckt.nodes[n as usize].supply);
            let port = HolderPort { block: j as u32, name: format!("{name}#port{k}"), second, ..HolderPort::default() };
            subs[j].devices.boundary.push(Boundary { name: port.name.clone(), node, kind: BoundaryKind::Port { of: of as u32, second }, v0: 0.0 });
            ports.push((k, c, port));
        }
    }
    // the solvers, each block after the blocks whose returns it takes, so each return's first value
    // is what its consumer's block draws at the state
    let mut solvers: Vec<Option<Solver>> = (0..order.len()).map(|_| None).collect();
    let mut owns: Vec<Vec<(u32, u32)>> = vec![Vec::new(); order.len()];
    let make = |k: usize, sub: &SimCircuit, owns: &mut Vec<Vec<(u32, u32)>>| -> Result<Solver, SimDiag> {
        let mut s = Solver::new(sub).map_err(|e| SimDiag::setup(format!("block {k} ({}): {}", names[k], e.message)))?;
        owns[k] = seed_from(&mut s, whole).into_iter().filter(|&(_, g)| owner[g as usize] == k && !imports[k].contains_key(&g)).collect();
        Ok(s)
    };
    let built_order = returns_order(placed, island, cuts, root).map_err(|(_, _, why)| SimDiag::setup(format!("round plan: {why}")))?;
    let mut return_from: Vec<(usize, u32, String, u32)> = Vec::new();
    for k in built_order.into_iter().map(|r| index[&r]) {
        for &(c, j) in &returns[k] {
            let name = name_of(cuts[c as usize].node);
            let s = solvers[j].as_mut().ok_or_else(|| SimDiag::setup(format!("round plan: block {j} ({}) is not built before the block taking its return", names[j])))?;
            let (Some(at), Some(br)) = (s.unknown(&name), s.unknown(&format!("{name}#in#branch"))) else {
                return Err(SimDiag::setup(format!("round plan: block {j} ({}) reads {name} without its boundary", names[j])));
            };
            let (_, _, f) = linearise(s, t);
            let v0 = -f[at as usize];
            s.x[br as usize] = v0;
            s.set_param(&format!("{name}#port{k}"), "v0", v0);
            let node = subs[k].node(&name);
            subs[k].devices.boundary.push(Boundary { name: format!("{name}#ret{j}"), node, kind: BoundaryKind::Current, v0 });
            return_from.push((j, br, format!("{name}#ret{j}"), c));
        }
        // Append the gain feeds after the physical boundaries so their instance indices stay fixed.
        let boundaries = &mut subs[k].devices.boundary;
        let gains: Vec<_> = boundaries.iter().enumerate().filter(|(_, b)| matches!(b.kind, BoundaryKind::Echo { .. }))
            .map(|(i, b)| Boundary { name: format!("{}#gain", b.name), node: 0, kind: BoundaryKind::EchoGain { echo: i as u32 }, v0: 0.0 }).collect();
        boundaries.extend(gains);
        solvers[k] = Some(make(k, &subs[k], &mut owns)?);
    }
    let solvers: Vec<Solver> = solvers.into_iter().map(|s| s.expect("every block built")).collect();
    let blocks: Vec<Block> = order.iter().enumerate().map(|(k, r)| Block { name: names[k].clone(), stage: stages[r], own: std::mem::take(&mut owns[k]) }).collect();
    for (&u, (coupling, _)) in &mut by_export {
        coupling.from.1 = solvers[coupling.from.0 as usize].unknown(&name_of(u)).unwrap_or(0);
    }
    // the late values, and which are currents returned in the hub form
    let (mut couplings, mut labels): (Vec<Coupling>, Vec<String>) = by_export.into_values().unzip();
    let mut hub = vec![None; couplings.len()];
    let mut plan_ports: Vec<HolderPort> = Vec::new();
    for (j, br, name, c) in return_from {
        let holder = index[&find(root, island[cuts[c as usize].node as usize])];
        let consumer = order[j];
        let rho = cuts[c as usize].soft.iter().filter(|&(&s, _)| find(root, s) == consumer).map(|(_, &r)| r).fold(0.0, f64::max);
        couplings.push(Coupling { from: (j as u32, br), to: vec![(holder as u32, name)], lag: Lag::Round, cuts: vec![(holder as u32, c)], rho });
        if let Some((_, _, port)) = ports.iter().find(|(k, cut, p)| *k == holder && *cut == c && p.block as usize == j) {
            plan_ports.push(HolderPort { coupling: (couplings.len() - 1) as u32, ..port.clone() });
        }
        labels.push(format!("the return at {} from block {j} ({})", name_of(cuts[c as usize].node), names[j]));
        hub.push(Some(name_of(cuts[c as usize].node)));
    }
    // the branch currents of copied sources: one share per copy
    let mut sums: Vec<(u32, Vec<(u32, u32)>)> = Vec::new();
    for u in 1..island.len() as u32 {
        if island[u as usize] != usize::MAX || whole.unknowns[u as usize].kind != UnkKind::Current {
            continue;
        }
        let name = name_of(u);
        let shares: Vec<(u32, u32)> = solvers.iter().enumerate().filter_map(|(k, s)| s.unknown(&name).map(|l| (k as u32, l))).collect();
        if shares.len() > 1 {
            sums.push((u, shares));
        }
    }
    let bounds = subs.iter().zip(&solvers).map(|(sub, s)| sub.devices.boundary.iter().filter(|b| !matches!(b.kind, BoundaryKind::EchoGain { .. })).map(|b| Bound {
        name: b.name.clone(),
        kind: b.kind,
        node: s.unknown(&sub.nodes[b.node as usize].name).unwrap_or(0),
        branch: match b.kind {
            BoundaryKind::Voltage => s.unknown(&format!("{}#branch", b.name)).unwrap_or(0),
            BoundaryKind::Port { of, .. } => s.unknown(&format!("{}#branch", sub.devices.boundary[of as usize].name)).unwrap_or(0),
            BoundaryKind::Current | BoundaryKind::Echo { .. } | BoundaryKind::EchoGain { .. } => 0,
        },
        w: if matches!(b.kind, BoundaryKind::Port { .. }) { s.unknown(&format!("{}#w", b.name)).unwrap_or(0) } else { 0 },
        w2: if matches!(b.kind, BoundaryKind::Port { second: true, .. }) { s.unknown(&format!("{}#w2", b.name)).unwrap_or(0) } else { 0 },
    }).collect()).collect();
    Ok(Built { plan: RoundPlan { blocks, couplings, sums, ghosts, echoes, gain_couplings: Vec::new(), switches: Vec::new(), proof: None, late_tol: Vec::new(), hub_tol: None, unpipelined: Vec::new(), cuts: Vec::new(), cut_nodes: BTreeMap::new(), lands: Vec::new(), land_steps: Vec::new(), chains: Vec::new(), ports: plan_ports }, solvers, roots: order, bounds, hub, labels })
}

/// The round map's points: the held step's band, then the matrix a step at the band's top solves
/// (as `2 / h` on the real axis).
fn points(h: &Held) -> Vec<(C64, String)> {
    (0..=h.freqs.len()).map(|k| (h.test_point(k), h.point(k))).collect()
}

/// Where the devices went and the blocks' stages (`place`, `order`), merging the two islands
/// either names until both pass.
#[allow(clippy::too_many_arguments)]
fn placed_stages(whole: &Solver, devs: &[Dev], island: &[usize], cuts: &[Cut], root: &mut [usize], zero: Option<usize>, h: &Held, log: &mut Vec<String>)
    -> (Placed, BTreeMap<usize, u32>)
{
    loop {
        let placed = match place(whole, devs, island, cuts, root) {
            Ok(placed) => placed,
            Err((a, b, why)) => {
                merge(root, a, b, &why, h, log);
                continue;
            }
        };
        match order(&placed, island, cuts, root, zero) {
            Ok(stages) => return (placed, stages),
            Err((a, b, why)) => merge(root, a, b, &why, h, log),
        }
    }
}

/// A plan's estimated round before it is built, in unknowns solved one after another (a block's
/// cost taken as the unknowns its islands hold, `sizes` by root): block 0 and the first stage of
/// each place in the pipeline run side by side, the stages that run right after the stage before
/// them (`RoundPlan::unpipelined`) then one after another, each as long as its largest block. With
/// the pipeline on and a helper for every block: a plan cannot know the helpers.
fn round_estimate(stages: &BTreeMap<usize, u32>, unpipelined: &[u32], sizes: &BTreeMap<usize, usize>) -> f64 {
    let mut by: BTreeMap<u32, f64> = BTreeMap::new();
    for (r, &s) in stages {
        let e = by.entry(s).or_default();
        *e = e.max(sizes.get(r).copied().unwrap_or(0) as f64);
    }
    let side = by.iter().filter(|(s, _)| !unpipelined.contains(s)).map(|(_, &c)| c).fold(0.0, f64::max);
    let after: f64 = by.iter().filter(|(s, _)| unpipelined.contains(s)).map(|(_, &c)| c).sum();
    side + after
}

/// The stages that run right after the stage before them (`RoundPlan::unpipelined`): for each
/// joined pair (an exporter's island and its reader's) whose blocks stand apart with the reader
/// after block 0, every stage after the reader's up to the exporter's.
fn unpipelined(stages: &BTreeMap<usize, u32>, joined: &[(usize, usize)], root: &mut [usize]) -> Vec<u32> {
    let mut out = BTreeSet::new();
    for &(from, to) in joined {
        if let (Some(&a), Some(&z)) = (stages.get(&find(root, from)), stages.get(&find(root, to))) {
            if a > z && z >= 1 {
                out.extend(z + 1..=a);
            }
        }
    }
    out.into_iter().collect()
}

/// A value read late through a cut, before the build: the cut, its exporter's and its reader's
/// blocks (by root), and how many rounds late the reader reads it with the pipeline on.
struct LateRead {
    cut: u32,
    from: usize,
    to: usize,
    k: u64,
}

/// The values the plan will read late through its cuts, from where the devices went and the
/// blocks' stages: each import not read in the same round (`same_round`) and each return in the
/// hub form, `lag_rounds` on the blocks' places in the pipeline (`slot_of`). The pipeline taken
/// as on: a plan cannot know the helpers.
fn late_reads(placed: &Placed, island: &[usize], cuts: &[Cut], root: &mut [usize], stages: &BTreeMap<usize, u32>, unpipelined: &[u32]) -> Vec<LateRead> {
    let place = |r: usize| slot_of(stages[&r], unpipelined);
    let mut out = Vec::new();
    for imp in &placed.imports {
        let owner = find(root, island[imp.1 as usize]);
        if let Some(c) = imp.2.filter(|_| owner != imp.0 && !same_round(imp, owner, cuts)) {
            out.push(LateRead { cut: c, from: owner, to: imp.0, k: lag_rounds(place(owner), place(imp.0), true) });
        }
    }
    for &(consumer, c) in &placed.returns {
        let holder = find(root, island[cuts[c as usize].node as usize]);
        if holder != consumer {
            out.push(LateRead { cut: c, from: consumer, to: holder, k: lag_rounds(place(consumer), place(holder), true) });
        }
    }
    out
}

/// Each late read judged at its lateness from the held step's numbers (`judge` on its cut's
/// `acc_rho` at `k` rounds), before anything is built: the first that fails, its exporter and
/// reader by root.
fn reweigh(reads: &[LateRead], cuts: &[Cut], h: &Held, stages: &BTreeMap<usize, u32>) -> Option<Late> {
    let mut weights: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for r in reads {
        let cut = &cuts[r.cut as usize];
        let w = weights.entry(r.k).or_insert_with(|| h.weights(r.k as f64 * h.round));
        let j = judge(&cut.acc_rho, w, Some(cut.bar));
        if let Some((bar, p)) = j.fails {
            let what = match bar {
                Bar::Stability => format!("rho {:.1e} at {} over {STABILITY}", cut.acc_rho[p], h.point(p)),
                Bar::Accuracy => format!("wrong by {:.1e} at {}, over the bar {:.0e}", j.wrong.1, h.point(p), cut.bar),
            };
            let one = r.k > 1 && judge(&cut.acc_rho, &h.weights(h.round), Some(cut.bar)).fails.is_none();
            let why = format!("the cut at {} is read {} round{} late (stage {} to stage {}): {what}",
                h.names[cut.node as usize], r.k, if r.k == 1 { "" } else { "s" }, stages[&r.from], stages[&r.to]);
            return Some(Late { from: r.from, to: r.to, one, why });
        }
    }
    None
}

/// Per coupling, per reader (in `Coupling::to` order): the rounds it is read late with the
/// pipeline on (`RoundPlan::lag_rounds`), 0 for a same-round read. A run on helpers has the
/// pipeline, and a plan cannot know the helpers: the worst case.
fn read_lags(plan: &RoundPlan) -> Vec<Vec<u64>> {
    plan.couplings.iter().map(|c| c.to.iter().map(|&(to, _)| match c.lag {
        Lag::Same => 0,
        Lag::Round => plan.lag_rounds(c.from.0 as usize, to as usize, true),
    }).collect()).collect()
}

/// A late cut that fails: its exporter and reader (blocks in `late_cuts`, roots in `reweigh`),
/// whether it passes read one round late, and why.
struct Late {
    from: usize,
    to: usize,
    one: bool,
    why: String,
}

/// The plan's cuts at the lateness its stages give them: a reader of a late coupling reads it
/// `k` rounds late (`read_lags`) across its own cut (`Coupling::cut_of`), and `judge` takes that
/// cut's `acc_rho` at that lateness under its bar.
/// The first that fails, and the largest `k` taken. This judges the lateness the plan has where
/// `held` judges one round.
fn late_cuts(b: &Built, cuts: &[Cut], h: &Held) -> (Option<Late>, u64) {
    let mut weights: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    let mut most = 0;
    for (c, lags) in b.plan.couplings.iter().zip(read_lags(&b.plan)) {
        let from = c.from.0 as usize;
        for (&(to, _), k) in c.to.iter().zip(lags).filter(|(_, k)| *k > 0) {
            // each reader at the cut it reads across, its own
            let Some(cut) = c.cut_of(to).map(|i| &cuts[i as usize]) else { continue };
            most = most.max(k);
            let w = weights.entry(k).or_insert_with(|| h.weights(k as f64 * h.round));
            let j = judge(&cut.acc_rho, w, Some(cut.bar));
            if let Some((bar, p)) = j.fails {
                let what = match bar {
                    Bar::Stability => format!("rho {:.1e} at {} over {STABILITY}", cut.acc_rho[p], h.point(p)),
                    Bar::Accuracy => format!("wrong by {:.1e} at {}, over the bar {:.0e}", j.wrong.1, h.point(p), cut.bar),
                };
                let one = k > 1 && judge(&cut.acc_rho, &h.weights(h.round), Some(cut.bar)).fails.is_none();
                let why = format!("the cut at {} is read {k} rounds late (stage {} to stage {}, pipelined): {what}",
                    h.names[cut.node as usize], b.plan.blocks[from].stage, b.plan.blocks[to as usize].stage);
                return (Some(Late { from, to: to as usize, one, why }), most);
            }
        }
    }
    (None, most)
}

/// The round map of a built plan, the two blocks to merge when it fails, and what it keeps to
/// prove the plan again after an edit. Stability always; accuracy where `accuracy` (the present
/// state). Each block's responses are a unit on `pool`'s cores beside the caller (`fan_out`), on
/// a copy of its solver.
fn round_map(b: &Built, t: f64, points: &[(C64, String)], dc_alt: f64, h: &Held, accuracy: bool, pool: Option<&Helpers>) -> Result<(RoundMap, Option<Fail>, RoundProof), SimDiag> {
    let shape = Arc::new(Shape::new(b, points, dc_alt, h, 1.0));
    let all = 0..points.len();
    let resp: Vec<Arc<Responses>> = if shape.lagged.is_empty() {
        Vec::new()
    } else {
        let units: Vec<Unit<Responses>> = b.solvers.iter().enumerate().map(|(k, s)| {
            let (s, shape, all) = (s.clone(), shape.clone(), all.clone());
            Box::new(move || shape.respond(k, &Load::of(&s, t, &shape), all)) as Unit<Responses>
        }).collect();
        fan_out(units, pool).map_err(|f| f.diag("round map: a block's responses"))?.into_iter().map(Arc::new).collect()
    };
    let (map, failed) = shape.fold(all.map(|p| shape.point(p, &resp.iter().map(|r| &r[p]).collect::<Vec<_>>(), accuracy)));
    Ok((map, failed, RoundProof { shape, resp, accuracy }))
}

/// The most a holder port's fit may leave of its holder's driving-point impedance at any point it
/// is fitted over: there it takes at least nine tenths of every late entry of the holder's returns
/// (`fit_ports`).
const PORT_FIT: f64 = 0.1;

/// A static echo's bar in a reader with switches of the circuit's own (it sets a pitch), as a
/// share of `cut_tol`: the pitch proof's 0.002 c at the scale drum16 measured, where its hats'
/// mid-range static echoes, 2.07e-5 summed at an oscillator, move the pitch about 0.03 c. Too
/// tight only exports more gain rows, which costs speed and never accuracy.
pub const ECHO_PITCH_SHARE: f64 = 1.0 / 2000.0;

/// The bar a reader's static echoes are judged against together (`screen_echoes`): `cut_tol`,
/// or `ECHO_PITCH_SHARE` of it where the reader holds a switch of the circuit's own.
pub fn echo_bar(cut_tol: f64, crossings: bool) -> f64 {
    if crossings { cut_tol * ECHO_PITCH_SHARE } else { cut_tol }
}

/// Fable's rule 1 prices a split that crosses a jump as a ramp a step wide. The ramp costs
/// `(w h)^2 / 8` of the jump's content at angular frequency `w`, with transfer `t` to the export.
pub fn band_figure(t: f64, f_top: f64, h: f64) -> f64 {
    t * ((2.0 * PI * f_top * h).powi(2) / 8.0)
}

/// The size screen (Fable's Q22 follow-up 2): per echo its reader, its bar (`echo_bar`) and its
/// size, `(g_hi - g_lo) / 2 x S`: what a mid-range constant gain can leave wrong at the ghost's far
/// node, per volt of the followed export's swing, times `S`, the reader's own export's response to
/// that node. Per reader, from the smallest up, an echo is static while the sizes taken stay under
/// the reader's bar with it, else exported. A constant gain is size 0, always static. In order of
/// `sizes`; O(echoes log echoes).
pub fn screen_echoes(sizes: &[(u32, f64, f64)]) -> Vec<bool> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| sizes[a].0.cmp(&sizes[b].0).then(sizes[a].2.total_cmp(&sizes[b].2)).then(a.cmp(&b)));
    let mut out = vec![false; sizes.len()];
    let (mut reader, mut taken) = (None, 0.0);
    for i in order {
        let (r, bar, size) = sizes[i];
        if reader != Some(r) {
            (reader, taken) = (Some(r), 0.0);
        }
        if taken + size <= bar {
            taken += size;
            out[i] = true;
        }
    }
    out
}

/// One echo as the size screen judges it (`screen`): its index in `RoundPlan::echoes`, its gain's
/// range over the plan's states, its reader's bar and its size, and whether it is static.
struct Screened {
    echo: usize,
    r: EchoResponse,
    bar: f64,
    size: f64,
    stat: bool,
}

/// The size screen (`screen_echoes`) on `plan`'s echoes from the import tests' figures (`Relay`):
/// an echo whose gain stays under 1e-3 in every state is dropped, and one with no reader
/// response is exported. In the order of `plan.echoes`.
fn screen(plan: &RoundPlan, relay: &Relay) -> Vec<Screened> {
    let mut kept = Vec::new();
    for (i, e) in plan.echoes.iter().enumerate() {
        let (q, of) = (e.coupling as usize, e.of as usize);
        let far = plan.couplings[of].from.0 as usize;
        let r = relay.echo_responses.get(&(q, far, of)).copied().unwrap_or_default();
        // Even a zero static fit can have an appreciable gain in another state.
        if r.min.abs().max(r.max.abs()) < 1e-3 {
            continue;
        }
        let sense = relay.echo_sense.get(&(of, e.block as usize, q)).copied();
        let (bar, size) = match sense {
            Some((s, bar)) => (bar, 0.5 * (r.max - r.min).abs() * s.min.abs().max(s.max.abs())),
            None => (0.0, f64::INFINITY),
        };
        kept.push((i, r, bar, size));
    }
    let sizes: Vec<(u32, f64, f64)> = kept.iter().map(|&(i, _, bar, size)| (plan.echoes[i].block, bar, size)).collect();
    kept.into_iter().zip(screen_echoes(&sizes)).map(|((echo, r, bar, size), stat)| Screened { echo, r, bar, size, stat }).collect()
}

/// Install each ghost's gain from its own export through the far reader to its late import, and
/// screen it (`screen`): a static echo keeps the middle of its gain's range over the plan's
/// states, with no export and no solve; an exported one is fed the far block's gain at each step,
/// and the fixed start's until its first feed.
fn fit_echoes(plan: &mut RoundPlan, solvers: &mut [Solver], relay: &Relay, report: &mut String) {
    let screened = screen(plan, relay);
    let mut echoes: Vec<Option<Echo>> = std::mem::take(&mut plan.echoes).into_iter().map(Some).collect();
    for Screened { echo, r, bar, size, stat } in screened {
        let mut e = echoes[echo].take().expect("each echo is screened once");
        let (k, of, q) = (e.block as usize, e.of as usize, e.coupling as usize);
        let back = &plan.couplings[of];
        let far = back.from.0 as usize;
        let node = &solvers[k].unknowns[plan.couplings[q].from.1 as usize].name;
        let (_, input) = back.to.iter().find(|to| to.0 == e.block).expect("echo corrects its own import");
        let lag = plan.lag_rounds(far, k, true);
        let how = if stat { "static" } else { "exported" };
        let judged = if size.is_finite() { format!("size {size:.1e} against {bar:.0e}") } else { "no reader response".to_string() };
        let _ = writeln!(report, "  {} on {input} in block {k} follows {node} with g {:.3e}..{:.3e} over {} states (fixed start {:.3e}), {judged}: {how}, read {lag} rounds late",
            e.name, r.min, r.max, r.states, r.static_g);
        e.g = if stat { 0.5 * (r.min + r.max) } else { r.static_g };
        if e.g != 0.0 {
            solvers[k].set_param(&e.name, "g", e.g);
        }
        if stat {
            solvers[k].set_param(&e.name, "exported", 0.0);
        } else {
            let (_, boundary) = plan.couplings[q].to.iter().find(|to| to.0 as usize == far).expect("echo follows an import of the far block");
            let branch = solvers[far].unknown(&format!("{boundary}#branch")).expect("voltage import has a branch row");
            e.gain = Some(plan.gain_couplings.len() as u32);
            plan.gain_couplings.push(GainCoupling { of: e.of, branch, to: (e.block, format!("{}#gain", e.name)), initial: e.g });
        }
        plan.echoes.push(e);
    }
}

/// The pitch rule (Fable's Q22 option (c)), on a plan `b` that passed its round map: an echo the
/// size screen would export (`screen`) at a reader holding a switch of the circuit's own, whose
/// crossings set a pitch (`Relay::switching`), corrects a cut that pitch hears, so its reader's
/// and its far block's islands merge instead. The echo corrects the reader's late copy of the far
/// node for the reader's own moves; an event of the far block reaches it late and moves its
/// crossings for good (drum16 at 48 kHz: a hit stepped the hats 15 to 26 us). The merged block
/// steps at the reader's crossings, which no static price sees, even where it is the plan's
/// longest. The echoes group by the blocks they would join and each group merges into `root`;
/// `log` says which. Whether any merged. O(echoes).
fn merge_pitch_cuts(root: &mut [usize], h: &Held, b: &Built, relay: &Relay, log: &mut Vec<String>) -> bool {
    let nb = b.plan.blocks.len();
    let mut group: Vec<usize> = (0..nb).collect();
    let (mut count, mut far_of) = (vec![0usize; nb], vec![false; nb]);
    for s in screen(&b.plan, relay).iter().filter(|s| !s.stat) {
        let e = &b.plan.echoes[s.echo];
        let (k, far) = (e.block as usize, b.plan.couplings[e.of as usize].from.0 as usize);
        if !relay.switching.get(k).copied().unwrap_or(false) {
            continue;
        }
        let (x, y) = (find(&mut group, k), find(&mut group, far));
        group[x.max(y)] = x.min(y);
        count[k] += 1;
        far_of[far] = true;
    }
    // per group (by its smallest block): its blocks, its pitch readers, their exported echoes and
    // the far blocks they follow
    let mut groups: BTreeMap<usize, (Vec<usize>, Vec<usize>, usize, Vec<usize>)> = BTreeMap::new();
    for k in 0..nb {
        let e = groups.entry(find(&mut group, k)).or_default();
        e.0.push(k);
        if count[k] > 0 {
            e.1.push(k);
            e.2 += count[k];
        }
        if far_of[k] {
            e.3.push(k);
        }
    }
    groups.retain(|_, g| g.0.len() > 1);
    let named = |ks: &[usize]| ks.iter().map(|&k| b.plan.blocks[k].name.as_str()).collect::<Vec<_>>().join(", ");
    for (blocks, pitch, echoes, far) in groups.values() {
        log.push(format!("{echoes} echoes the size screen would export at {} (whose crossings set a pitch), following {}: merged", named(pitch), named(far)));
        for w in blocks.windows(2) {
            merge(root, b.roots[w[0]], b.roots[w[1]], "the pitch rule, as above", h, log);
        }
    }
    !groups.is_empty()
}

/// Fits each holder port of `b` (`HolderPort`) to its holder's driving-point impedance at the
/// node, at DC, in the band and at the step's matrix (`fit_port`), and sets it in the port's block
/// and the plan. The impedance is taken where the plan's map is judged: from a fixed start at the
/// key's base (`base`: its seed and the whole circuit), so a plan made again on the key fits the
/// same ports and keeps its maps (`Others::maps`); else at the state the blocks were built in. The holders go in an order where a
/// holder that is itself a consumer comes after the holders it returns into, so its impedance is
/// taken with its own ports in. A holder no one-port fits within `PORT_FIT` leaves its consumers'
/// ports inert: their returns are read bare, as `log` says, and the map judges them so. O(holders x
/// a block's responses + ports x points x the fit's grid).
fn fit_ports(b: &mut Built, t: f64, base: Option<(&Seed, &Solver)>, points: &[(C64, String)], dc_alt: f64, h: &Held, log: &mut Vec<String>) {
    if b.plan.ports.is_empty() {
        return;
    }
    let mut shape = Shape::new(b, points, dc_alt, h, 1.0);
    let step = 2.0 / points[points.len() - 1].0.re;
    // each port's holder and the boundary there its return is taken at
    let at: Vec<Option<(usize, usize)>> = b.plan.ports.iter().map(|p| {
        let (holder, name) = b.plan.couplings[p.coupling as usize].to.first()?;
        Some((*holder as usize, b.bounds[*holder as usize].iter().position(|x| x.name == *name)?))
    }).collect();
    // the holders, each after the holders its own ports stand for
    let mut holders: Vec<usize> = at.iter().flatten().map(|a| a.0).collect();
    holders.sort_unstable();
    holders.dedup();
    let mut order = Vec::new();
    while !holders.is_empty() {
        let ready = holders.iter().position(|&k| !b.plan.ports.iter().zip(&at).any(|(p, a)| p.block as usize == k && a.is_some_and(|a| holders.contains(&a.0) && a.0 != k))).unwrap_or(0);
        order.push(holders.remove(ready));
    }
    for k in order {
        // only its returns' boundaries
        for (j, want) in shape.wants[k].iter_mut().enumerate() {
            *want = at.iter().flatten().any(|&(hk, hj)| hk == k && hj == j);
        }
        let load = match base {
            Some((seed, whole)) => Seedable::of(&b.solvers[k], whole).load(seed, &shape),
            None => Load::of(&b.solvers[k], t, &shape),
        };
        let resp = shape.respond(k, &load, 0..points.len());
        // one fit a node: every return into it sees the same impedance
        let mut done: Vec<(String, Option<(PortZ, f64, usize)>)> = Vec::new();
        for i in 0..b.plan.ports.len() {
            let Some((_, hj)) = at[i].filter(|a| a.0 == k) else { continue };
            let node = b.hub[b.plan.ports[i].coupling as usize].clone().unwrap_or_default();
            let fitted = match done.iter().find(|d| d.0 == node) {
                Some(d) => d.1,
                None => {
                    // A holder singular at DC has no DC impedance to fit. Use its band and step points.
                    let fit: Vec<(usize, C64)> = (0..points.len()).filter(|&p| port_point(points, p, shape.band))
                        .filter_map(|p| Some((p, resp[p].as_ref()?[hj].as_ref().filter(|r| !r.floor)?.drive)))
                        .filter(|(_, z)| z.norm() > 0.0 && z.norm().is_finite()).collect();
                    let slowest = fit.iter().map(|&(p, _)| points[p].0)
                        .filter(|s| s.re == 0.0 && s.im > 0.0).map(|s| s.im)
                        .min_by(f64::total_cmp).map_or(f64::INFINITY, f64::recip);
                    let second = b.plan.ports[i].second;
                    let found = fit_port(&fit.iter().map(|&(p, z)| (points[p].0, z)).collect::<Vec<_>>(), step, second, slowest)
                        .map(|(z, worst, w)| (z, worst, fit[w].0));
                    let consumers = b.plan.ports.iter().zip(&at).filter(|(p, a)| a.is_some_and(|a| a.0 == k) && b.hub[p.coupling as usize].as_deref() == Some(node.as_str())).count();
                    log.push(match found {
                        Some((z, worst, p)) if worst <= PORT_FIT => {
                            let mut line = format!("the holder of {node} (block {k}) stands beside its {consumers} consumers as {}, leaving at most {:.1} % of its impedance (at {})",
                                one_port(z), worst * 100.0, points[p].1);
                            if second && z.rb2 == 0.0 {
                                if slowest.is_finite() {
                                    let _ = write!(line, "; one section: no second within the band's lowest point (a time constant up to {slowest:.3e} s) fits better");
                                } else {
                                    line.push_str("; one section: no second fits better with the available points");
                                }
                            }
                            line
                        }
                        Some((z, worst, p)) => format!("the holder of {node} (block {k}) fits no one-port within {PORT_FIT} (the nearest, {}, leaves {:.0} % at {}): its {consumers} consumers' returns are read bare",
                            one_port(z), worst * 100.0, points[p].1),
                        None => format!("the holder of {node} (block {k}) has no impedance to fit: its {consumers} consumers' returns are read bare"),
                    });
                    let kept = found.filter(|f| f.1 <= PORT_FIT);
                    done.push((node, kept));
                    kept
                }
            };
            let Some((z, _, _)) = fitted else { continue };
            let port = &mut b.plan.ports[i];
            (port.r, port.rb, port.tau, port.rb2, port.tau2, port.l) = (z.r, z.rb, z.tau, z.rb2, z.tau2, z.l);
            let (block, name) = (port.block as usize, port.name.clone());
            for (key, v) in port.params() {
                b.solvers[block].set_param(&name, key, v);
            }
            if let Some(j) = b.bounds[block].iter().position(|x| x.name == name) {
                shape.port_z[block][j] = Some(z);
            }
        }
    }
}

/// Whether a holder port is fitted at point `p` of the map's `points` (`fit_ports`): the step's
/// matrix (the last), the band's points, and DC.
fn port_point(points: &[(C64, String)], p: usize, band: f64) -> bool {
    p + 1 == points.len() || in_band(points[p].0, band)
}

/// The fitted impedance shared by the holder's consumers and checked again on a re-proof.
#[derive(Clone, Copy, Default)]
struct PortZ {
    r: f64,
    rb: f64,
    tau: f64,
    rb2: f64,
    tau2: f64,
    l: f64,
}

impl From<&HolderPort> for PortZ {
    fn from(p: &HolderPort) -> Self {
        PortZ { r: p.r, rb: p.rb, tau: p.tau, rb2: p.rb2, tau2: p.tau2, l: p.l }
    }
}

/// A holder port's impedance at `s`: `r + rb / (1 + s tau) + rb2 / (1 + s tau2) + s l`.
fn port_z(s: C64, p: PortZ) -> C64 {
    let one = C64::new(1.0, 0.0);
    let z = C64::new(p.r, 0.0) + C64::new(p.rb, 0.0) / (one + s.scale(p.tau)) + s.scale(p.l);
    if p.rb2 > 0.0 { z + C64::new(p.rb2, 0.0) / (one + s.scale(p.tau2)) } else { z }
}

/// `fit_ports` for a plan from the run or a fixed start: from a fixed start the fit at the key's
/// base is kept by the plan's structure (`Others::ports`), so a plan made again on the key (a
/// re-plan after an edit) sets the same ports and gives their report lines as they were, without
/// fitting.
/// O(ports) kept, else `fit_ports`'s.
#[allow(clippy::too_many_arguments)]
fn fit_ports_keyed(b: &mut Built, t: f64, others: &Others, whole: &Solver, points: &[(C64, String)], dc_alt: f64, h: &Held, log: &mut Vec<String>) {
    let Some((seed, _)) = others.base_seed() else {
        fit_ports(b, t, None, points, dc_alt, h, log);
        return;
    };
    if b.plan.ports.is_empty() {
        return;
    }
    let key = structure_of(&b.plan);
    let kept = others.ports.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
    if let Some((ports, lines)) = kept {
        for p in ports.iter().filter(|p| p.r + p.rb + p.rb2 + p.l > 0.0) {
            for (k, v) in p.params() {
                b.solvers[p.block as usize].set_param(&p.name, k, v);
            }
        }
        b.plan.ports = ports;
        log.extend(lines);
        return;
    }
    let from = log.len();
    fit_ports(b, t, Some((seed, whole)), points, dc_alt, h, log);
    others.ports.lock().unwrap_or_else(|e| e.into_inner()).insert(key, (b.plan.ports.clone(), log[from..].to_vec()));
}

/// A fitted one-port in words, with each parallel RC section expressed in ohms and farads.
fn one_port(p: PortZ) -> String {
    let mut words = format!("{:.3e} ohm", p.r);
    for (rb, tau) in [(p.rb, p.tau), (p.rb2, p.tau2)] {
        if rb > 0.0 {
            let _ = write!(words, " and {rb:.3e} ohm || {:.3e} F", tau / rb);
        }
    }
    if p.l > 0.0 { let _ = write!(words, " and {:.3e} H", p.l); }
    if p.rb > 0.0 || p.rb2 > 0.0 || p.l > 0.0 { words.push_str(" in series"); }
    words
}

/// A passive one-port fitted to a driving-point impedance `z` at DC (when available), the band's
/// points and the step's matrix, weighed to `|z|`: `r + rb / (1 + s tau)` with `r, rb >= 0` and the memory
/// no faster than the `step` (a faster one rings under the trapezoid), or `r + s l` with
/// `r, l >= 0` (a rail behind a buffer: an inductance in the band); at a declared supply
/// (`second`) also `r + rb / (1 + s tau) + rb2 / (1 + s tau2)`, with `rb, rb2 >= 0` (a rail's
/// local and bulk capacitance, each behind its own resistance). Each is a least-squares fit in its
/// linear values, its time constants searched from the step up: one `tau` over a grid of 20 a
/// decade over twelve decades; a pair over every pair of every eighth of those, then by a pattern
/// search from the best (a pair's local minimum is not the single `tau`'s: that one sits between
/// the pair's). Both time constants of a pair are bounded by `slowest`, the reciprocal of the
/// lowest fitted positive angular frequency. The band cannot resolve a slower section. With DC
/// pinned, it could only correct DC on a time scale too long for the run.
/// The form that leaves least at its worst point wins: its impedance, that worst
/// share of `|z|`, and its point's index in `s`. None with no point. O(points x (the grid + the
/// coarse grid's pairs)).
fn fit_port(fit: &[(C64, C64)], step: f64, second: bool, slowest: f64) -> Option<(PortZ, f64, usize)> {
    if fit.is_empty() {
        return None;
    }
    // the worst share left and where, of a one-port `zf`
    let worst = |zf: &dyn Fn(C64) -> C64| fit.iter().enumerate().map(|(i, &(s, z))| ((z - zf(s)).norm() / z.norm(), i)).fold((0.0f64, 0usize), |m, x| if x.0 > m.0 { x } else { m });
    // least squares in (a, b) of `z ~ a + b g(s)`, rows weighed by 1/|z|; a negative `a` refitted
    // as zero; None for a negative `b`
    let solve = |g: &dyn Fn(C64) -> C64| -> Option<(f64, f64)> {
        let (mut a11, mut a12, mut a22, mut b1, mut b2) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &(s, z) in fit {
            let w2 = 1.0 / (z.norm() * z.norm());
            let gs = g(s);
            a11 += w2;
            a12 += w2 * gs.re;
            a22 += w2 * (gs.re * gs.re + gs.im * gs.im);
            b1 += w2 * z.re;
            b2 += w2 * (gs.re * z.re + gs.im * z.im);
        }
        let det = a11 * a22 - a12 * a12;
        let (mut a, mut b) = if det.abs() > 1e-300 * a11 * a22 { ((b1 * a22 - b2 * a12) / det, (a11 * b2 - a12 * b1) / det) } else { (0.0, b2 / a22) };
        if a < 0.0 {
            (a, b) = (0.0, b2 / a22);
        }
        (b >= 0.0 && a.is_finite() && b.is_finite()).then_some((a, b))
    };
    let mut best: Option<(PortZ, f64, usize)> = None;
    let mut take = |cand: (PortZ, f64, usize)| {
        if best.is_none_or(|b| cand.1 < b.1) {
            best = Some(cand);
        }
    };
    for k in 0..=240 {
        let tau = step * 10f64.powf(k as f64 / 20.0);
        let g = move |s: C64| C64::new(1.0, 0.0) / (C64::new(1.0, 0.0) + s.scale(tau));
        if let Some((r, rb)) = solve(&g) {
            let z = PortZ { r, rb, tau, ..PortZ::default() };
            let (w, at) = worst(&|s| port_z(s, z));
            take((z, w, at));
        }
    }
    if let Some((r, l)) = solve(&|s| s) {
        let z = PortZ { r, l, ..PortZ::default() };
        let (w, at) = worst(&|s| port_z(s, z));
        take((z, w, at));
    }
    if second {
        let one = C64::new(1.0, 0.0);
        // least squares in (r, rb, rb2) of `z ~ r + rb / (1 + s t1) + rb2 / (1 + s t2)`, rows
        // weighed by 1/|z|; a negative `r` refitted as zero; None for a negative section or a
        // singular pair
        let pair = |t1: f64, t2: f64| -> Option<(PortZ, f64, usize)> {
            let (mut a, mut b) = ([[0.0f64; 3]; 3], [0.0f64; 3]);
            for &(s, z) in fit {
                let w2 = 1.0 / (z.norm() * z.norm());
                let f = [one, one / (one + s.scale(t1)), one / (one + s.scale(t2))];
                for i in 0..3 {
                    for j in 0..3 {
                        a[i][j] += w2 * (f[i].re * f[j].re + f[i].im * f[j].im);
                    }
                    b[i] += w2 * (f[i].re * z.re + f[i].im * z.im);
                }
            }
            let mut x = solve3(a, b)?;
            if x[0] < 0.0 {
                let d = a[1][1] * a[2][2] - a[1][2] * a[2][1];
                if !(d.abs() > 1e-12 * a[1][1] * a[2][2]) {
                    return None;
                }
                x = [0.0, (b[1] * a[2][2] - b[2] * a[1][2]) / d, (a[1][1] * b[2] - a[2][1] * b[1]) / d];
            }
            if !(x[1] >= 0.0 && x[2] >= 0.0 && x.iter().all(|v| v.is_finite())) {
                return None;
            }
            let z = PortZ { r: x[0], rb: x[1], tau: t1, rb2: x[2], tau2: t2, l: 0.0 };
            let (w, at) = worst(&|s| port_z(s, z));
            Some((z, w, at))
        };
        // every pair of a coarse grid, every eighth time constant of the one-section grid; then
        // a pattern search from the best, each time constant moved up or down by a factor that
        // starts at the coarse grid's step and is square-rooted where neither moves, to 1.001
        let coarse: Vec<f64> = (0..=240).step_by(8).map(|k| step * 10f64.powf(k as f64 / 20.0)).filter(|&tau| tau <= slowest).collect();
        let mut found: Option<((PortZ, f64, usize), f64, f64)> = None;
        for (i, &t1) in coarse.iter().enumerate() {
            for &t2 in &coarse[i + 1..] {
                if let Some(f) = pair(t1, t2).filter(|f| found.is_none_or(|b| f.1 < b.0 .1)) {
                    found = Some((f, t1, t2));
                }
            }
        }
        if let Some(mut b) = found {
            let mut factor = 10f64.powf(8.0 / 20.0);
            while factor > 1.001 {
                let mut moved = false;
                for which in 0..2 {
                    for d in [factor, 1.0 / factor] {
                        let (t1, t2) = if which == 0 { (b.1 * d, b.2) } else { (b.1, b.2 * d) };
                        if t1 >= t2 || t1 < step || t2 > slowest {
                            continue;
                        }
                        if let Some(f) = pair(t1, t2).filter(|f| f.1 < b.0 .1) {
                            (b, moved) = ((f, t1, t2), true);
                        }
                    }
                }
                if !moved {
                    factor = factor.sqrt();
                }
            }
            take(b.0);
        }
    }
    best
}

/// `a x = b` for three unknowns by Cramer's rule; None where `a` is singular to its diagonal.
fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: &[[f64; 3]; 3]| m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let d = det(&a);
    if !(d.abs() > 1e-12 * a[0][0] * a[1][1] * a[2][2]) {
        return None;
    }
    let mut x = [0.0; 3];
    for (k, x) in x.iter_mut().enumerate() {
        let mut m = a;
        for i in 0..3 {
            m[i][k] = b[i];
        }
        *x = det(&m) / d;
    }
    Some(x)
}

/// What a map found, and failing, the two blocks to merge and why (`Shape::fold`).
type Found = (RoundMap, Option<Fail>);

/// A failed map: the two blocks to merge (where the late value is solved and where it
/// comes in, or a return's block and the block whose export carries it there: `carried`), why,
/// and the cuts the heaviest link's two late values are read across.
#[derive(Clone, Debug)]
pub(crate) struct Fail { from: usize, other: usize, why: String, cuts: Vec<u32>, carried: bool }

/// A block's solver with where its unknowns and devices are in the whole circuit's solver, to put
/// it in a state taken there (`Seed::put`): (local, whole) unknowns, and per device of the whole
/// in it (a cut element's copy, `<name>#ghost`, too) its (bank, instance) here.
#[derive(Clone)]
struct Seedable {
    s: Solver,
    unks: Vec<(usize, usize)>,
    devs: Vec<((u32, u32), (usize, usize))>,
}

impl Seedable {
    fn of(s: &Solver, whole: &Solver) -> Seedable {
        let unks = (1..s.unknowns.len()).filter_map(|l| whole.unknown(&s.unknowns[l].name).map(|g| (l, g as usize))).collect();
        let mut devs = Vec::new();
        for (b, bank) in whole.banks.iter().enumerate() {
            for i in 0..bank.len() {
                let name = bank.inst_name(i);
                for at in [s.find_device(name), s.find_device(&format!("{name}#ghost"))].into_iter().flatten() {
                    devs.push(((b as u32, i as u32), at));
                }
            }
        }
        Seedable { s: s.clone(), unks, devs }
    }

    /// Its matrix at the map's points (`Load`) in the state `seed` holds.
    fn load(&self, seed: &Seed, shape: &Shape) -> Load {
        let mut s = self.s.clone();
        seed.put(&mut s, &self.unks, &self.devs);
        Load::of(&s, seed.t, shape)
    }
}

/// The fields populated by build, stage placement and port fitting, before the proof.
/// The gain transports attached to a finished plan have no part in shape reuse.
fn structure_of(plan: &RoundPlan) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let shape = (&plan.blocks, &plan.couplings, &plan.sums, &plan.ghosts, &plan.unpipelined, &plan.ports, &plan.echoes);
    format!("{shape:?}").hash(&mut h);
    h.finish()
}

/// The plan's round map in the key's states (`Others`): at the base, every block put in it, then
/// in each other state that reaches a block the map takes a response of, those blocks put in it
/// and the rest as at the base (as a re-proof takes an edit, `Share`); both bars where both hold
/// there, the accuracy bars `scale` times theirs (the keep rule). The worst figures over them,
/// each with its state, and the first failure in that order with the blocks to merge. The base's
/// blocks and each state are units on `pool`'s cores (`fan_out`), and each map is kept in
/// `others` by the plan's structure, so a plan made again on the same key maps nothing. `count`
/// takes the states mapped and the maps made and kept.
#[allow(clippy::too_many_arguments)]
fn key_maps(b: &Built, whole: &Solver, others: &Arc<Others>, points: &[(C64, String)], dc_alt: f64, h: &Held, scale: f64, pool: Option<&Helpers>, count: &mut RoundMap) -> Result<Found, SimDiag> {
    let (_, base_what) = others.base_seed().expect("the key's maps stand on a base");
    let base_what = base_what.to_string();
    let shape = Arc::new(Shape::new(b, points, dc_alt, h, scale));
    let n = points.len();
    if shape.lagged.is_empty() || n == 0 {
        return Ok((RoundMap { lagged: shape.lagged.len(), ..RoundMap::default() }, None));
    }
    let structure = structure_of(&b.plan);
    let key = |at: usize| (structure, at, scale.to_bits());
    let kept = |at: usize| others.maps.lock().unwrap_or_else(|e| e.into_inner()).get(&key(at)).cloned();
    let keep = |at: usize, got: &Found| {
        others.maps.lock().unwrap_or_else(|e| e.into_inner()).insert(key(at), got.clone());
    };
    let blocks: Vec<Seedable> = b.solvers.iter().map(|s| Seedable::of(s, whole)).collect();
    // the blocks the map takes a response of, and per state those it reaches
    let taken: Vec<bool> = shape.wants.iter().map(|w| w.iter().any(|&x| x)).collect();
    let states: Vec<(usize, bool, String, Vec<usize>)> = others.mapped().filter_map(|m| {
        let reached: Vec<usize> = (0..blocks.len()).filter(|&k| taken[k] && blocks[k].unks.iter().any(|&(_, g)| m.touch[g])).collect();
        (!reached.is_empty()).then(|| (m.at, m.stability_only, m.what.to_string(), reached))
    }).collect();
    count.states = states.len();
    // the base's responses, every block put in it, when a map in the key's states is made
    let respond_base = || -> Result<Vec<Arc<Responses>>, SimDiag> {
        let units: Vec<Unit<Responses>> = (0..blocks.len()).map(|k| {
            let (block, shape, others) = (blocks[k].clone(), shape.clone(), others.clone());
            Box::new(move || {
                let (seed, _) = others.base_seed().expect("a base");
                shape.respond(k, &block.load(seed, &shape), 0..shape.points.len())
            }) as Unit<Responses>
        }).collect();
        Ok(fan_out(units, pool).map_err(|f| f.diag("round map: a block's responses at the base"))?.into_iter().map(Arc::new).collect())
    };
    let mut base_resp = None;
    let base = match kept(usize::MAX) {
        Some(got) => {
            count.kept += 1;
            got
        }
        None => {
            let resp = respond_base()?;
            let got = shape.fold((0..n).map(|p| shape.point(p, &resp.iter().map(|r| &r[p]).collect::<Vec<_>>(), true)));
            count.key_maps += 1;
            keep(usize::MAX, &got);
            base_resp = Some(resp);
            got
        }
    };
    let (mut worst, failed) = base;
    if let Some(f) = failed {
        return Ok((worst, Some(Fail { why: format!("{base_what}: {}", f.why), ..f })));
    }
    let mut got: Vec<Option<Found>> = states.iter().map(|s| kept(s.0)).collect();
    count.kept += got.iter().filter(|g| g.is_some()).count();
    let fresh: Vec<usize> = (0..states.len()).filter(|&i| got[i].is_none()).collect();
    if !fresh.is_empty() {
        let resp = match base_resp {
            Some(r) => r,
            None => respond_base()?,
        };
        let units: Vec<Unit<Found>> = fresh.iter().map(|&i| {
            let (at, stability_only) = (states[i].0, states[i].1);
            let reached: Vec<(usize, Seedable)> = states[i].3.iter().map(|&k| (k, blocks[k].clone())).collect();
            let (shape, others, resp) = (shape.clone(), others.clone(), resp.clone());
            Box::new(move || {
                let seed = others.seed(at);
                let edited = Arc::new(reached.iter().map(|(k, block)| (*k, block.load(seed, &shape))).collect::<Vec<_>>());
                let done = Share { shape: shape.clone(), resp, edited, points: 0..shape.points.len(), accuracy: !stability_only }.run();
                shape.fold(done.outs.into_iter())
            }) as Unit<Found>
        }).collect();
        let made = fan_out(units, pool).map_err(|f| f.diag("round map: a state's map"))?;
        count.key_maps += made.len();
        for (&i, m) in fresh.iter().zip(made) {
            keep(states[i].0, &m);
            got[i] = Some(m);
        }
    }
    for ((_, _, what, _), g) in states.iter().zip(got) {
        let (m, failed) = g.expect("each state's map is made or kept");
        let at = |(v, p): (f64, String)| (v, format!("{p}, {what}"));
        worst.include_above(&m, what);
        if m.gain.0 > worst.gain.0 { worst.gain = at(m.gain); }
        if m.gain_ordinary.0 > worst.gain_ordinary.0 { worst.gain_ordinary = at(m.gain_ordinary); }
        if m.wrong.0 > worst.wrong.0 { worst.wrong = at(m.wrong); }
        if m.wrong_ordinary.0 > worst.wrong_ordinary.0 { worst.wrong_ordinary = at(m.wrong_ordinary); }
        worst.sensitivity = worst.sensitivity.max(m.sensitivity);
        if let Some(f) = failed {
            return Ok((worst, Some(Fail { why: format!("{what}: {}", f.why), ..f })));
        }
    }
    Ok((worst, None))
}

/// A block's response at one point of the round map to a unit at one of its boundaries: at the
/// boundary's own node (the driving point, which scales a current returned there) and at each
/// unknown the block exports (`Shape::mine` order).
#[derive(Clone)]
struct Resp {
    drive: C64,
    out: Vec<C64>,
    /// True when the block's matrix was singular at DC and the band's floor supplied this response.
    floor: bool,
}

/// A block's responses over the map's points: at each, None where its matrix is singular, else
/// per boundary its response when the map takes one.
type Responses = Vec<Option<Vec<Option<Resp>>>>;

/// What a plan's round map keeps to prove the plan again after an edit (`reprove`): how its
/// couplings compose (`Shape`), and per block its responses at the state the map, or the block's
/// last re-proof, linearised it at. The map is three parts: each block's responses
/// (`Shape::respond`), their composition in stage order into the matrix `M` of a round with its
/// gains at each point (`Shape::point`), and the points together (`Shape::fold`).
/// Whether a re-proof holds the accuracy bar too: from the run, yes; from a fixed start no, the
/// run's state only guarding there (on the stability bar), as the plan's own map at it does.
#[derive(Clone)]
pub struct RoundProof {
    shape: Arc<Shape>,
    resp: Vec<Arc<Responses>>,
    accuracy: bool,
}

impl std::fmt::Debug for RoundProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundProof").field("points", &self.shape.points.len()).field("blocks", &self.shape.bounds.len()).field("lagged", &self.shape.lagged.len()).finish()
    }
}

impl RoundProof {
    /// Per coupling, what the plan proves its late read gets wrong at most: `PartOpts::hub_tol` for a
    /// return in the hub form (a pad or a declared supply), whose bar is the whole map's, else
    /// `cut_tol` (`RoundPlan::late_tol`, the rounds' shadow rounds' bars). O(couplings).
    pub(crate) fn late_tol(&self) -> Vec<f64> {
        self.shape.hub.iter().map(|h| if h.is_some() { self.shape.hub_tol } else { self.shape.cut_tol }).collect()
    }

    /// The plan's bar for a return in the hub form (`PartOpts::hub_tol`): the loosest it promises,
    /// and the cap on every reader's shadow bar (`RoundPlan::hub_tol`).
    pub(crate) fn hub_tol(&self) -> f64 {
        self.shape.hub_tol
    }

    /// The plan proved again after an edit: the blocks `edited` (each with its solver as it
    /// stands) linearised at `t`, the others' responses as kept. Each point's work is its own
    /// (the edited blocks' factorisations and solves there, the composition, the gains), so the
    /// points are shared out between the worker and the `helpers`; the worker runs any share no
    /// helper has started and waits only for those a helper runs, as for a stage's packs. Passing
    /// its bars (both, or from a fixed start the stability bar alone, with how far the accuracy
    /// figure reads against its bar in `RoundMap::present`), the new responses are kept and the map
    /// comes back; failing, why.
    pub(crate) fn reprove(&mut self, edited: &[(usize, &Solver)], t: f64, helpers: Option<&Helpers>) -> Result<RoundMap, String> {
        let n = self.shape.points.len();
        if self.shape.lagged.is_empty() || n == 0 {
            return Ok(RoundMap::default());
        }
        let edited: Arc<Vec<(usize, Load)>> = Arc::new(edited.iter().map(|&(k, s)| (k, Load::of(s, t, &self.shape))).collect());
        let shares = helpers.map_or(1, |h| h.count + 1).min(n);
        let share = |i: usize| Share { shape: self.shape.clone(), resp: self.resp.clone(), edited: edited.clone(), points: i * n / shares..(i + 1) * n / shares, accuracy: self.accuracy };
        let (tx, rx) = mpsc::channel();
        let mut posted = Vec::new();
        if let Some(h) = helpers {
            for i in 1..shares {
                let slot = Arc::new(Mutex::new(Some(share(i))));
                let (taken, tx) = (slot.clone(), tx.clone());
                (h.post)(Box::new(move || {
                    let job = taken.lock().unwrap_or_else(|e| e.into_inner()).take();
                    if let Some(job) = job {
                        let _ = tx.send((i, job.run()));
                    }
                }));
                posted.push((i, slot));
            }
        }
        drop(tx);
        let mut done: Vec<Option<Done>> = (0..shares).map(|_| None).collect();
        done[0] = Some(share(0).run());
        let mut running = 0;
        for (i, slot) in &posted {
            let job = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            match job {
                Some(job) => done[*i] = Some(job.run()),
                None => running += 1,
            }
        }
        for _ in 0..running {
            let Ok((i, got)) = rx.recv() else { return Err("the re-proof: a helper core lost its share".into()) };
            done[i] = Some(got);
        }
        let mut fresh: Vec<Responses> = edited.iter().map(|_| Vec::with_capacity(n)).collect();
        let mut outs = Vec::with_capacity(n);
        for d in done.into_iter().flatten() {
            for (f, r) in fresh.iter_mut().zip(d.resp) {
                f.extend(r);
            }
            outs.extend(d.outs);
        }
        // an edited holder still within its ports' fit bar, else the plan is made again with a
        // fresh fit (`fit_ports`)
        let points = &self.shape.points;
        for &(k, j, port, ref node) in &self.shape.port_fits {
            let Some(e) = edited.iter().position(|x| x.0 == k) else { continue };
            let worst = (0..n).filter(|&p| port_point(points, p, self.shape.band)).filter_map(|p| {
                // A holder singular at DC has no DC impedance to check. Re-prove its band and step points.
                let z = fresh[e][p].as_ref()?[j].as_ref().filter(|r| !r.floor)?.drive;
                (z.norm() > 0.0 && z.norm().is_finite()).then(|| ((z - port_z(points[p].0, port)).norm() / z.norm(), p))
            }).fold((0.0f64, 0usize), |m, x| if x.0 > m.0 { x } else { m });
            if worst.0 > PORT_FIT {
                return Err(format!("the holder of {node} (block {k}) no longer fits its port within {PORT_FIT}: it leaves {:.0} % at {}; the plan is made again with a fresh fit", worst.0 * 100.0, points[worst.1].1));
            }
        }
        match self.shape.fold(outs.into_iter()) {
            (mut map, None) => {
                for ((k, _), r) in edited.iter().zip(fresh) {
                    self.resp[*k] = Arc::new(r);
                }
                if !self.accuracy {
                    let (hub, ordinary) = (map.wrong.0 / self.shape.hub_tol, map.wrong_ordinary.0 / self.shape.cut_tol);
                    map.present = if hub >= ordinary { (hub, map.wrong.1.clone()) } else { (ordinary, format!("{}, through ordinary late values", map.wrong_ordinary.1)) };
                }
                Ok(map)
            }
            (_, Some(f)) => Err(f.why),
        }
    }
}

/// A block's matrix at every point of the map from its linearisation where it stands, with what
/// its banks add on the axis, and at DC also at the band's floor (for a block singular at DC):
/// with its layout and size, what a share of the map needs to factor and solve it on any core.
struct Load {
    layout: Layout,
    n: usize,
    at: Vec<Vec<C64>>,
    floor: Vec<C64>,
}

impl Load {
    fn of(s: &Solver, t: f64, shape: &Shape) -> Load {
        let (g, c, _) = linearise(s, t);
        let load = |sp: C64| -> Vec<C64> {
            let mut vals: Vec<C64> = g.iter().zip(&c).map(|(&g, &c)| C64::new(g + sp.re * c, sp.im * c)).collect();
            if sp.re == 0.0 {
                for bank in &s.banks {
                    bank.ac_stamp(sp.im, &mut vals);
                }
            }
            vals
        };
        Load { layout: s.layout.clone(), n: s.n, at: shape.points.iter().map(|(sp, _)| load(*sp)).collect(), floor: load(C64::new(0.0, 2.0 * PI * shape.dc_alt)) }
    }
}

/// A share of a re-proof: its points, the blocks edited with their matrices, the others'
/// responses as kept.
struct Share {
    shape: Arc<Shape>,
    resp: Vec<Arc<Responses>>,
    edited: Arc<Vec<(usize, Load)>>,
    points: Range<usize>,
    /// Both bars, or the stability bar alone (`Shape::point`).
    accuracy: bool,
}

/// What a share gives: per edited block its responses at the share's points, and per point what
/// the point gives (`Shape::point`).
struct Done {
    resp: Vec<Responses>,
    outs: Vec<Option<PointGains>>,
}

impl Share {
    fn run(self) -> Done {
        let fresh: Vec<Responses> = self.edited.iter().map(|(k, load)| self.shape.respond(*k, load, self.points.clone())).collect();
        let outs = self.points.clone().map(|p| {
            let at: Vec<&Option<Vec<Option<Resp>>>> = (0..self.resp.len()).map(|k| match self.edited.iter().position(|e| e.0 == k) {
                Some(e) => &fresh[e][p - self.points.start],
                None => &self.resp[k][p],
            }).collect();
            self.shape.point(p, &at, self.accuracy)
        }).collect();
        Done { resp: fresh, outs }
    }
}

/// One point of the map where no block is singular: the loop gain of the whole map and of its
/// ordinary late values, what each gets wrong read late (`bars`), the largest row sum of `|M|`,
/// per row its largest entry from a return in the hub form and that return's column, and the
/// point's worst failure (its figure over its bar, the test, the gain, what it gets wrong, the
/// heaviest link (i, j, |M_ij|), its row's three largest terms).
struct PointGains {
    gains: [f64; 2],
    wrongs: [f64; 2],
    /// The same errors with full weights, only at a point above the band.
    wrongs_above: Option<[f64; 2]>,
    sensitivity: f64,
    follows: Vec<(f64, usize)>,
    /// A failure here: its figure over its bar, the test, the gain, what it gets wrong, the
    /// heaviest link, its row's largest terms, and the blocks a return's path names (`carried`).
    worst: Option<(f64, usize, f64, f64, (usize, usize, f64), String, Option<(usize, usize, usize)>)>,
}

/// What `Shape::point` works in, kept by each thread that maps (a pool's worker, a helper or the
/// caller) and cleared for each point instead of made again, at the largest sizes it has needed:
/// the exports as combinations of the late values, an input to a boundary, `M`, its entries and its
/// ordinary entries, an error's weighed entries, and the rows' scales and the columns' weights.
#[derive(Default)]
struct PointScratch {
    ev: Vec<C64>,
    input: Vec<C64>,
    m: Vec<C64>,
    all: Vec<(usize, usize, f64)>,
    ordinary: Vec<(usize, usize, f64)>,
    weighed: Vec<(usize, usize, f64)>,
    scale: Vec<f64>,
    colw: Vec<f64>,
}

thread_local! {
    static POINT_SCRATCH: RefCell<PointScratch> = RefCell::new(PointScratch::default());
}

/// How a plan's couplings compose in its round map: its points and bars, the late values (the
/// rows and columns of `M`), what feeds each boundary and what each block exports.
struct Shape {
    points: Vec<(C64, String)>,
    dc_alt: f64,
    cut_tol: f64,
    hub_tol: f64,
    band: f64,
    /// At each point, the accuracy weight for each lateness the plan has (`read_lags`), zero
    /// above the band. The report's table takes the complementary points.
    weights: Vec<Vec<f64>>,
    weights_above: Vec<Vec<f64>>,
    /// Per row of `M`: the latenesses its readers read it at, as places in a point's `weights`.
    lags: Vec<Vec<usize>>,
    couplings: Vec<Coupling>,
    labels: Vec<String>,
    /// Per coupling: the node of a return in the hub form.
    hub: Vec<Option<String>>,
    /// The couplings read a round late: the rows and columns of `M`, and per row whether it is
    /// a return in the hub form.
    lagged: Vec<usize>,
    hub_row: Vec<bool>,
    /// Per coupling: its row of `M`, if read a round late, and the export it reads.
    row: Vec<Option<usize>>,
    export_of: Vec<usize>,
    exports: usize,
    bounds: Vec<Vec<Bound>>,
    /// Per block, per boundary: the coupling that feeds it, and whether the map takes its response.
    fed: Vec<Vec<Option<usize>>>,
    wants: Vec<Vec<bool>>,
    /// Per block: its exported unknowns and their exports.
    mine: Vec<Vec<(u32, usize)>>,
    /// Per row of `M` that is a returned current: the (block, boundary) it is taken at.
    drives: Vec<Vec<(usize, usize)>>,
    /// Per block, per boundary: a holder port's impedance as the plan fitted it
    /// (`RoundPlan::ports`), what a unit of its value moves its rows by (`respond`).
    port_z: Vec<Vec<Option<PortZ>>>,
    /// Each fitted holder port's holder, the boundary its returns are taken at, its impedance
    /// and its node: what a re-proof checks the holder against again
    /// (`RoundProof::reprove`).
    port_fits: Vec<(usize, usize, PortZ, String)>,
}

impl Shape {
    fn new(b: &Built, points: &[(C64, String)], dc_alt: f64, h: &Held, scale: f64) -> Shape {
        let couplings = &b.plan.couplings;
        let read = read_lags(&b.plan);
        let mut ks: Vec<u64> = read.iter().flatten().copied().filter(|&k| k > 0).collect();
        ks.sort_unstable();
        ks.dedup();
        let band = accuracy_band(h.band, h.fmax);
        let (weights, weights_above) = points.iter().map(|&(sp, _)| {
            let full = ks.iter().map(|&k| weight(sp, k as f64 * h.round)).collect();
            let zero = vec![0.0; ks.len()];
            if in_band(sp, band) { (full, zero) } else { (zero, full) }
        }).unzip();
        let lagged: Vec<usize> = (0..couplings.len()).filter(|&q| couplings[q].lag == Lag::Round).collect();
        let lags = lagged.iter().map(|&q| {
            let mut at: Vec<usize> = read[q].iter().filter_map(|k| ks.binary_search(k).ok()).collect();
            at.sort_unstable();
            at.dedup();
            at
        }).collect();
        let mut row = vec![None; couplings.len()];
        for (i, &q) in lagged.iter().enumerate() {
            row[q] = Some(i);
        }
        // what feeds each boundary, and which unknowns each block exports
        let mut feed: HashMap<(u32, &str), usize> = HashMap::new();
        for (q, l) in couplings.iter().enumerate() {
            for (k, name) in &l.to {
                feed.insert((*k, name.as_str()), q);
            }
        }
        // a holder port reads its own return as the holder does: the same late value
        for p in &b.plan.ports {
            feed.insert((p.block, p.name.as_str()), p.coupling as usize);
        }
        let port_z = b.bounds.iter().enumerate().map(|(k, bounds)| bounds.iter().map(|x| {
            b.plan.ports.iter().find(|p| p.block as usize == k && p.name == x.name).map(PortZ::from)
        }).collect()).collect();
        let mut port_fits: Vec<(usize, usize, PortZ, String)> = Vec::new();
        for p in b.plan.ports.iter().filter(|p| p.r + p.rb + p.rb2 + p.l > 0.0) {
            let Some((holder, name)) = couplings[p.coupling as usize].to.first() else { continue };
            let Some(j) = b.bounds[*holder as usize].iter().position(|x| x.name == *name) else { continue };
            if !port_fits.iter().any(|f| (f.0, f.1) == (*holder as usize, j)) {
                port_fits.push((*holder as usize, j, PortZ::from(p), b.hub[p.coupling as usize].clone().unwrap_or_default()));
            }
        }
        let mut export: HashMap<(u32, u32), usize> = HashMap::new();
        for l in couplings {
            let next = export.len();
            export.entry(l.from).or_insert(next);
        }
        let mut mine: Vec<Vec<(u32, usize)>> = vec![Vec::new(); b.solvers.len()];
        for (&(k, u), &e) in &export {
            mine[k as usize].push((u, e));
        }
        for list in &mut mine {
            list.sort_unstable();
        }
        let fed: Vec<Vec<Option<usize>>> = b.bounds.iter().enumerate().map(|(k, bounds)| bounds.iter().map(|x| feed.get(&(k as u32, x.name.as_str())).copied()).collect()).collect();
        let mut wants: Vec<Vec<bool>> = fed.iter().enumerate().map(|(k, f)| f.iter().map(|q| q.is_some() && !mine[k].is_empty()).collect()).collect();
        // a returned current's scale: the driving-point impedance at its node where it is taken
        let drives: Vec<Vec<(usize, usize)>> = lagged.iter().map(|&q| {
            let (k, u) = couplings[q].from;
            if b.solvers[k as usize].unknowns[u as usize].kind != UnkKind::Current {
                return Vec::new();
            }
            couplings[q].to.iter().filter_map(|(k, name)| {
                let j = b.bounds[*k as usize].iter().position(|x| x.name == *name && x.kind == BoundaryKind::Current)?;
                wants[*k as usize][j] = true;
                Some((*k as usize, j))
            }).collect()
        }).collect();
        Shape {
            points: points.to_vec(), dc_alt, cut_tol: h.cut_tol * scale, hub_tol: h.hub_tol * scale, band, weights, weights_above, lags,
            couplings: couplings.clone(), labels: b.labels.clone(), hub: b.hub.clone(),
            hub_row: lagged.iter().map(|&q| b.hub[q].is_some()).collect(), lagged, row, export_of: couplings.iter().map(|l| export[&l.from]).collect(),
            exports: export.len(), bounds: b.bounds.clone(), fed, wants, mine, drives, port_z, port_fits,
        }
    }

    /// Block `k`'s responses at the map's `points`, through its matrix there (`load`; DC through
    /// the band's floor when singular there): what the map takes of a unit at each boundary.
    fn respond(&self, k: usize, load: &Load, points: Range<usize>) -> Responses {
        let mut lu = load.layout.lu::<C64>();
        // a unit at one boundary, then its solution: one buffer, zeroed for each solve
        let mut x = vec![C64::ZERO_C; load.n + 1];
        points.map(|p| {
            let sp = self.points[p].0;
            let floor = lu.factor(&load.at[p]).is_err();
            if floor && !(sp.re == 0.0 && sp.im == 0.0 && lu.factor(&load.floor).is_ok()) {
                return None;
            }
            Some(self.bounds[k].iter().zip(&self.wants[k]).zip(&self.port_z[k]).map(|((bound, &want), port)| {
                if !want {
                    return None;
                }
                x.fill(C64::ZERO_C);
                match (bound.kind, *port) {
                    (BoundaryKind::Voltage, _) => x[bound.branch as usize] = C64::new(1.0, 0.0),
                    // A unit of the port's value adds r + s l to the branch residual and
                    // the section's resistance to each memory residual.
                    (BoundaryKind::Port { .. }, Some(z)) => {
                        x[bound.branch as usize] = -(C64::new(z.r, 0.0) + sp.scale(z.l));
                        x[bound.w as usize] = C64::new(-z.rb, 0.0);
                        if bound.w2 != 0 { x[bound.w2 as usize] = C64::new(-z.rb2, 0.0); }
                    }
                    (BoundaryKind::Port { .. }, None) => {}
                    (BoundaryKind::Echo { .. } | BoundaryKind::EchoGain { .. }, _) => return None,
                    (BoundaryKind::Current, _) => x[bound.node as usize] = C64::new(1.0, 0.0),
                }
                lu.solve(&mut x[1..]);
                x[0] = C64::ZERO_C;
                Some(Resp { drive: x[bound.node as usize], out: self.mine[k].iter().map(|&(u, _)| x[u as usize]).collect(), floor })
            }).collect())
        }).collect()
    }

    /// Late-read error bound at one point for the map of `entries` with loop gain `g`. Each
    /// column takes the largest `weight` over its readers' delays. These form the diagonal of `W`.
    /// The error `g(|M| W)` gathers round the loop (`recirculated`). For one late value this
    /// gives the held tests' `late_wrong`. Equal delays give `late_wrong` of the map's gain.
    /// It works in `colw` and `weighed`, cleared first.
    fn wrong(&self, weights: &[f64], entries: &[(usize, usize, f64)], r: usize, g: f64, colw: &mut Vec<f64>, weighed: &mut Vec<(usize, usize, f64)>) -> f64 {
        colw.clear();
        colw.extend(self.lags.iter().map(|at| at.iter().map(|&k| weights[k]).fold(0.0, f64::max)));
        weighed.clear();
        weighed.extend(entries.iter().map(|&(i, j, a)| (i, j, a * colw[j])));
        recirculated(loop_gain(weighed, r).0, g)
    }

    /// Point `p` of the map from every block's responses there (`at`, in block order): `M`
    /// composed block by block in stage order, and its loop gains against the bars (`bars`):
    /// stability on the whole map; where `accuracy`, what the whole map gets wrong read late under
    /// `hub_tol` and what its ordinary late values do under `cut_tol`. None where a block is
    /// singular. It works in this thread's `PointScratch`.
    fn point(&self, p: usize, at: &[&Option<Vec<Option<Resp>>>], accuracy: bool) -> Option<PointGains> {
        let at: Vec<&Vec<Option<Resp>>> = at.iter().map(|x| x.as_ref()).collect::<Option<_>>()?;
        Some(POINT_SCRATCH.with(|s| match s.try_borrow_mut() {
            Ok(mut s) => self.point_in(&mut s, p, &at, accuracy),
            Err(_) => self.point_in(&mut PointScratch::default(), p, &at, accuracy),
        }))
    }

    /// `point` in `s`, at a point where no block is singular.
    fn point_in(&self, s: &mut PointScratch, p: usize, at: &[&Vec<Option<Resp>>], accuracy: bool) -> PointGains {
        let PointScratch { ev, input, m, all, ordinary, weighed, scale, colw } = s;
        let (lagged, hub) = (&self.lagged, &self.hub_row);
        let r = lagged.len();
        scale.clear();
        scale.resize(r, 1.0);
        for (i, drives) in self.drives.iter().enumerate() {
            let z = drives.iter().map(|&(k, j)| at[k][j].as_ref().map_or(0.0, |x| x.drive.norm())).fold(0.0, f64::max);
            if z > 0.0 && z.is_finite() {
                scale[i] = z;
            }
        }
        // each export as a combination of the late values, block by block in stage order (export
        // `e`'s from `e * r`)
        ev.clear();
        ev.resize(self.exports * r, C64::ZERO_C);
        for (k, mine) in self.mine.iter().enumerate() {
            if mine.is_empty() {
                continue;
            }
            for (j, fed) in self.fed[k].iter().enumerate() {
                let Some(q) = *fed else { continue };
                input.clear();
                match self.row[q] {
                    Some(i) => {
                        input.resize(r, C64::ZERO_C);
                        input[i] = C64::new(1.0 / scale[i], 0.0);
                    }
                    None => {
                        let e = self.export_of[q];
                        input.extend_from_slice(&ev[e * r..(e + 1) * r]);
                    }
                }
                if input.iter().all(|z| *z == C64::ZERO_C) {
                    continue;
                }
                let x = &at[k][j].as_ref().expect("the map takes every fed boundary of a block that exports").out;
                for (&a, &(_, e)) in x.iter().zip(mine) {
                    if a == C64::ZERO_C {
                        continue;
                    }
                    for (dst, &v) in ev[e * r..(e + 1) * r].iter_mut().zip(input.iter()) {
                        *dst += a * v;
                    }
                }
            }
        }
        // `M`, row `i` from `i * r`
        m.clear();
        for (i, &q) in lagged.iter().enumerate() {
            let e = self.export_of[q];
            m.extend(ev[e * r..(e + 1) * r].iter().map(|v| v.scale(scale[i])));
        }
        let mut out = PointGains { gains: [0.0; 2], wrongs: [0.0; 2], wrongs_above: None, sensitivity: 0.0, follows: vec![(0.0, 0); r], worst: None };
        // the entrywise magnitudes, sparse
        all.clear();
        for i in 0..r {
            let row = &m[i * r..(i + 1) * r];
            let mut sum = 0.0;
            for (j, v) in row.iter().enumerate() {
                let a = v.norm();
                if a > 0.0 {
                    all.push((i, j, a));
                    sum += a;
                    if !hub[i] && hub[j] && a > out.follows[i].0 {
                        out.follows[i] = (a, j);
                    }
                }
            }
            out.sensitivity = out.sensitivity.max(sum);
        }
        ordinary.clear();
        ordinary.extend(all.iter().copied().filter(|&(i, j, _)| !hub[i] && !hub[j]));
        let gains = [loop_gain(all, r), loop_gain(ordinary, r)];
        // Only one table can have nonzero weights here. The other error stays zero.
        let (weights, wrongs) = if in_band(self.points[p].0, self.band) {
            (&self.weights[p], &mut out.wrongs)
        } else {
            (&self.weights_above[p], out.wrongs_above.insert([0.0; 2]))
        };
        for (i, (g, _, _)) in gains.iter().enumerate() {
            out.gains[i] = *g;
            wrongs[i] = self.wrong(weights, if i == 0 { all } else { ordinary }, r, *g, colw, weighed);
        }
        // the tests: (test, its map, the bar): stability on the whole map, then accuracy
        let tests: [(usize, usize, Option<f64>); 3] = [(0, 0, None), (1, 0, Some(self.hub_tol)), (2, 1, Some(self.cut_tol))];
        for (test, which, bar) in tests.into_iter().filter(|t| accuracy || t.0 == 0) {
            let (g, x, w) = &gains[which];
            let entries = if which == 0 { &*all } else { &*ordinary };
            let wrong = out.wrongs[which];
            let fails = bar_failed(*g, wrong, bar);
            let ratio = match fails {
                None => continue,
                Some(Bar::Stability) => g / STABILITY,
                Some(Bar::Accuracy) => wrong / bar.expect("an accuracy bar"),
            };
            if out.worst.as_ref().is_some_and(|w| w.0 >= ratio) {
                continue;
            }
            // the heaviest link of the dominant loop, and its row's largest terms
            let link = entries.iter().copied().max_by(|a, c| (w[a.0] * a.2 * x[a.1]).total_cmp(&(w[c.0] * c.2 * x[c.1]))).unwrap_or((0, 0, 0.0));
            let mut row: Vec<(usize, f64)> = m[link.0 * r..(link.0 + 1) * r].iter().enumerate().map(|(j, v)| (j, v.norm())).filter(|&(_, a)| a > 0.0).collect();
            row.sort_by(|a, c| c.1.total_cmp(&a.1));
            let top = row.iter().take(3).map(|&(j, a)| format!("{} {a:.1e}", self.labels[lagged[j]])).collect::<Vec<_>>().join(", ");
            let path = if hub[link.0] { self.carried(at, ev, scale, r, (link.0, link.1)) } else { None };
            out.worst = Some((ratio, test, *g, wrong, link, top, path));
        }
        out
    }

    /// Where most of entry `(i, j)` of `M` reaches the block that exports row `i`, a return into a
    /// hub node: the boundary of that block whose term in the entry is the largest. When that
    /// boundary reads another block's export in the same round, that block passes the late value on
    /// within the round (it reads the node and drives what the return's block reads), and no port
    /// at the node carries it: (the return's block, that block, the export's coupling). For the
    /// node's own read it is the holder, which merges as any late value's two blocks do. None when
    /// the largest part is the late value itself (the port's term) or no boundary carries it.
    /// O(the block's boundaries).
    fn carried(&self, at: &[&Vec<Option<Resp>>], ev: &[C64], scale: &[f64], r: usize, (i, j): (usize, usize)) -> Option<(usize, usize, usize)> {
        let q = self.lagged[i];
        let k = self.couplings[q].from.0 as usize;
        let a = self.mine[k].iter().position(|&(_, e)| e == self.export_of[q])?;
        let mut best: Option<(f64, usize)> = None;
        for (b, fed) in self.fed[k].iter().enumerate() {
            let Some(q2) = *fed else { continue };
            let input = match self.row[q2] {
                Some(i2) if i2 == j => C64::new(1.0 / scale[i2], 0.0),
                Some(_) => continue,
                None => ev[self.export_of[q2] * r + j],
            };
            let Some(resp) = at[k][b].as_ref() else { continue };
            let term = (resp.out[a] * input).norm() * scale[i];
            if best.is_none_or(|x| term > x.0) {
                best = Some((term, q2));
            }
        }
        let (_, q2) = best?;
        let by = self.couplings[q2].from.0 as usize;
        (self.row[q2].is_none() && by != k).then_some((k, by, q2))
    }

    /// The map of its points in order (`point`): the largest gains and where, the followers, and
    /// when a bar fails, the two blocks to merge and why.
    fn fold(&self, outs: impl Iterator<Item = Option<PointGains>>) -> Found {
        let (couplings, lagged, points) = (&self.couplings, &self.lagged, &self.points);
        let r = lagged.len();
        let mut map = RoundMap { lagged: r, ..RoundMap::default() };
        if r == 0 {
            return (map, None);
        }
        // per map (the whole, its ordinary late values): the largest gain and what it gets wrong read
        // late, each with its point
        let mut gains = [(0.0f64, 0usize); 2];
        let mut wrongs = [(0.0f64, 0usize); 2];
        // per late value: its largest entry from a hub return (gain, the return, the point)
        let mut follows: Vec<(f64, usize, usize)> = vec![(0.0, 0, 0); r];
        // the worst failure: (its figure over its bar, the test, the point, the gain, what it gets
        // wrong, the link, its row's terms)
        let mut worst: Option<(f64, usize, usize, f64, f64, (usize, usize, f64), String, Option<(usize, usize, usize)>)> = None;
        for (p, out) in outs.enumerate() {
            let Some(out) = out else {
                map.singular += 1;
                continue;
            };
            map.points += 1;
            if let Some([wrong, ordinary]) = out.wrongs_above {
                for (peak, value) in [(&mut map.gain_above, out.gains[0]), (&mut map.wrong_above, wrong), (&mut map.wrong_ordinary_above, ordinary)] {
                    if peak.1.is_empty() || value > peak.0 {
                        *peak = (value, points[p].1.clone());
                    }
                }
            }
            map.sensitivity = map.sensitivity.max(out.sensitivity);
            for (f, &(a, j)) in follows.iter_mut().zip(&out.follows) {
                if a > f.0 {
                    *f = (a, j, p);
                }
            }
            for (g, &at) in gains.iter_mut().zip(&out.gains) {
                if at > g.0 {
                    *g = (at, p);
                }
            }
            for (g, &at) in wrongs.iter_mut().zip(&out.wrongs) {
                if at > g.0 {
                    *g = (at, p);
                }
            }
            if let Some((ratio, test, g, wrong, link, top, path)) = out.worst {
                if !worst.as_ref().is_some_and(|w| w.0 >= ratio) {
                    worst = Some((ratio, test, p, g, wrong, link, top, path));
                }
            }
        }
        map.gain = (gains[0].0, points[gains[0].1].1.clone());
        map.gain_ordinary = (gains[1].0, points[gains[1].1].1.clone());
        map.wrong = (wrongs[0].0, points[wrongs[0].1].1.clone());
        map.wrong_ordinary = (wrongs[1].0, points[wrongs[1].1].1.clone());
        let mut followers: Vec<(String, String, f64, String)> = (0..r).filter(|&i| follows[i].0 > 0.0)
            .map(|i| (self.labels[lagged[i]].clone(), self.hub[lagged[follows[i].1]].clone().unwrap_or_default(), follows[i].0, points[follows[i].2].1.clone())).collect();
        followers.sort_by(|a, c| c.2.total_cmp(&a.2));
        map.followers = followers;
        let Some((_, test, p, g, wrong, (i, j, a), top, path)) = worst else { return (map, None) };
        // the late value `j`'s two blocks: where it is solved and where it comes in, so that it is read
        // late no more; or, for a return whose path names them (`carried`), its block and the block
        // whose export carries it there
        let late = &couplings[lagged[j]];
        let (from, other) = match path {
            Some((k, b, _)) => (k, b),
            None => (late.from.0 as usize, late.to.first().map_or(late.from.0 as usize, |t| t.0 as usize)),
        };
        let what = match test {
            0 => format!("the loop gain {g:.2e} a round at {}, over the stability bar {STABILITY}", points[p].1),
            1 => format!("the loop gain {g:.2e} a round at {}, wrong by {wrong:.1e} read late, over the bar {:.0e}", points[p].1, self.hub_tol),
            _ => format!("the loop gain through ordinary late values {g:.2e} a round at {}, wrong by {wrong:.1e} read late, over the bar {:.0e}", points[p].1, self.cut_tol),
        };
        let mut why = format!("the round map: {what}; the heaviest link: {} takes {a:.1e} of {} in a round; that row's largest terms: {top}",
            self.labels[lagged[i]], self.labels[lagged[j]]);
        if let Some((_, _, q)) = path {
            let _ = write!(why, "; most of it reaches the return's block through {}, read in the same round: the two blocks merge", self.labels[q]);
        }
        let through = path.map(|(_, _, q)| &couplings[q].cuts);
        let mut cuts: Vec<u32> = couplings[lagged[i]].cuts.iter().chain(through.unwrap_or(&late.cuts)).map(|r| r.1).collect();
        cuts.sort_unstable();
        cuts.dedup();
        (map, Some(Fail { from, other, why, cuts, carried: path.is_some() }))
    }
}

/// The Collatz-Wielandt bound on the spectral radius of `A` from `x = A^n 1`: the largest
/// `(A x)_i / x_i` over the rows where `x_i > 0`. Rows where `x` is zero start no walk of length
/// `n` and carry no radius. None, so the caller keeps the power bound, when no row has `x_i > 0`,
/// when a row where `x` is zero still has a positive product (the power underflowed), or when a
/// ratio is not finite.
fn collatz_wielandt(entries: &[(usize, usize, f64)], r: usize, x: &[f64]) -> Option<f64> {
    let mut y = vec![0.0; r];
    for &(i, j, a) in entries {
        y[i] += a * x[j];
    }
    let mut bound: Option<f64> = None;
    for (&xi, yi) in x.iter().zip(y) {
        if xi > 0.0 {
            let ratio = yi / xi;
            if !ratio.is_finite() {
                return None;
            }
            bound = Some(bound.unwrap_or(0.0).max(ratio));
        } else if xi == 0.0 && yi > 0.0 {
            return None;
        }
    }
    bound
}

/// The loop gain bound for an `r` by `r` matrix `A` of magnitudes given as (row, column, value).
/// With `n = LOOP_POWER` and `x = A^n 1`, take the smaller of `max(A^n 1)^(1/n)` and the
/// Collatz-Wielandt maximum of `(A x)_i / x_i` over rows with `x_i > 0`. Both bound the spectral
/// radius `rho` from above. A row sum `S` for a value on no loop can inflate the first bound by
/// `(S / rho)^(1/n)`. The second is exact for a loop of rank one. Return the gain with `x` and
/// `1^T A^n`, each scaled to a largest entry of one. The gain is zero when `A^n` is.
fn loop_gain(entries: &[(usize, usize, f64)], r: usize) -> (f64, Vec<f64>, Vec<f64>) {
    let (mut x, mut w) = (vec![1.0; r], vec![1.0; r]);
    let (mut y, mut v) = (vec![0.0; r], vec![0.0; r]);
    let mut log = 0.0;
    for _ in 0..LOOP_POWER {
        y.fill(0.0);
        v.fill(0.0);
        for &(i, j, a) in entries {
            y[i] += a * x[j];
            v[j] += w[i] * a;
        }
        let (top, top_w) = (y.iter().copied().fold(0.0, f64::max), v.iter().copied().fold(0.0, f64::max));
        if top == 0.0 || top_w == 0.0 {
            return (0.0, x, w);
        }
        log += top.ln();
        for (x, y) in x.iter_mut().zip(&y) {
            *x = y / top;
        }
        for (w, v) in w.iter_mut().zip(&v) {
            *w = v / top_w;
        }
    }
    let g = (log / LOOP_POWER as f64).exp();
    // both bound the same radius, so the smaller holds; a power bound not finite stands as it is
    let gain = match collatz_wielandt(entries, r, &x) {
        Some(cw) if g.is_finite() => g.min(cw),
        _ => g,
    };
    (gain, x, w)
}

/// The damped steps a charge on a node is followed over (`charge_hold`): what the node still holds
/// this many sample periods after the charge is put on it. A first guess (A-K1 revisit).
const HOLD_STEPS: usize = 4;

/// One import's fast charge and slow hold (A-K1 revisit, the plan-time test). A jump the import
/// makes reaches its reader as a ramp a sample wide (`Table::split`). A node the ramp charges
/// within a step keeps a current the trapezoid carries past the ramp's corner, and a node that is
/// then held (a junction off) keeps the charge that current pushes in.
#[derive(Clone, Debug)]
pub struct ChargeHold {
    /// The import's coupling and its reader block.
    pub coupling: usize,
    pub reader: usize,
    /// Whether a jump is born on the import (`can_jump`): a path starts there, amplitude 1.
    pub jumps: bool,
    /// Whether this plan made its reader's test, or took it from a plan before whose reader had
    /// the same matrices (`StateCache::table`). An import of a block not tested: false.
    pub made: bool,
    /// The charge the trapezoid wants returned a step after a unit ramp, coulombs per volt of the
    /// jump, on the node and in the state where the figure is largest.
    pub q: f64,
    pub node: String,
    pub q_in: String,
    /// What that node still holds of a unit charge `HOLD_STEPS` damped steps later, volts per
    /// coulomb, at its largest over the states, and in which.
    pub hold: f64,
    pub hold_in: String,
    /// `q x hold`: volts left per volt of the jump (infinite where a state could not judge it);
    /// and the import's bar.
    pub figure: f64,
    pub bar: f64,
    /// The import's largest transfer to a voltage export or heard net of its reader, zero for an untested reader.
    pub t: f64,
    /// The import's own band figure before carry, volts per volt of the jump.
    pub band: f64,
    /// The largest amplitude a born jump reaches this import with along a path that passes no block
    /// twice, volts per volt born (1 where it is born, 0 where no path reaches it); and the verdict
    /// (`RoundPlan::lands`) after the relay rule, named landings and late-loop splits.
    pub amp: f64,
    pub lands: bool,
    /// The smallest export step this pair lands, in volts. Zero for split, named or unjudged pairs.
    pub step: f64,
    /// Whether this pair lands only by the band figure, including a figure carried back to it.
    pub by_band: bool,
}

/// The relay rule's count over a plan (`charge_hold`): per tested block, each voltage import's
/// transfer to each voltage export (coupling in, block, coupling out, `T`); the path steps
/// followed, those cut off under a thousandth of the next import's bar, the blocks tested, and the
/// landed pairs split because they closed a loop through a round-late read; each path step
/// followed, from pair to pair (coupling, reader block), which the chain shadow's trees are made
/// of (`PartOpts::land` adds named prefixes after the rule).
#[derive(Clone, Debug, Default)]
pub struct Relay {
    pub transfers: Vec<(usize, usize, usize, f64)>,
    /// The top of the judged band in Hz and the split's factor there (`band_figure` at unit transfer).
    pub band_at: (f64, f64),
    /// Echo candidates' signed responses, keyed by (input coupling, far reader, output coupling).
    pub echo_responses: BTreeMap<(usize, usize, usize), EchoResponse>,
    /// Each echo's reader: its own export's signed response to the ghost's import, keyed by (ghost
    /// coupling, reader, followed export), and the reader's bar for its static echoes (`echo_bar`).
    pub echo_sense: BTreeMap<(usize, usize, usize), (EchoResponse, f64)>,
    /// Per block: whether it holds a switch of the circuit's own, whose crossings set a pitch
    /// (`echo_bar`, `merge_pitch_cuts`).
    pub switching: Vec<bool>,
    pub followed: usize,
    pub cut: usize,
    pub blocks: usize,
    pub loops: usize,
    pub steps: Vec<((usize, usize), (usize, usize))>,
    /// Each named landing and the number of pairs in its prefix before late-loop splits.
    pub named: Vec<(Land, usize)>,
    /// Distinct (coupling, reader) pairs in the named prefixes that land in the final plan.
    pub by_name: BTreeSet<(usize, usize)>,
    /// Landings whose born pairs exist but whose target is off their tree.
    pub off_tree: Vec<Land>,
    /// Born export names with no jumping pair on this board.
    pub absent: Vec<String>,
}

/// The chain shadow's trees (`RoundPlan::chains`): per export a jump is born on that a reader
/// splits, its imports that split (the roots) and the pairs the relay rule's path steps reach from
/// them (`Relay::steps`, the same cut-off) read in the same round, and its members in stage order.
/// A step read a round late ends the tree there, counted. O(trees x steps x pairs).
fn chains(plan: &RoundPlan, labels: &[String], table: &[ChargeHold], steps: &[((usize, usize), (usize, usize))]) -> Vec<Chain> {
    let same = |q: usize| plan.couplings[q].lag == Lag::Same;
    let stage = |r: usize| plan.blocks[r].stage;
    let mut out = Vec::new();
    let mut born: Vec<usize> = table.iter().filter(|x| x.jumps && !x.lands && same(x.coupling)).map(|x| x.coupling).collect();
    born.sort_unstable();
    born.dedup();
    for c in born {
        let roots: Vec<(usize, usize)> = table.iter().filter(|x| x.jumps && !x.lands && x.coupling == c).map(|x| (c, x.reader)).collect();
        let (mut seen, mut late, mut todo) = (roots.clone(), Vec::new(), roots.clone());
        while let Some(p) = todo.pop() {
            for &(_, next) in steps.iter().filter(|s| s.0 == p) {
                if seen.contains(&next) || late.contains(&next) {
                    continue;
                }
                if same(next.0) {
                    seen.push(next);
                    todo.push(next);
                } else {
                    late.push(next);
                }
            }
        }
        seen.sort_by_key(|&(q, r)| (stage(r), r, q));
        let mut members: Vec<u32> = Vec::new();
        for &(_, r) in &seen {
            if !members.contains(&(r as u32)) {
                members.push(r as u32);
            }
        }
        let mut roots: Vec<u32> = roots.iter().map(|&(_, r)| r as u32).collect();
        roots.sort_unstable();
        roots.dedup();
        let (pairs, pair_labels) = seen.into_iter().map(|(q, r)| ((q as u32, r as u32), labels[q].clone())).unzip();
        out.push(Chain { born: c as u32, name: labels[c].clone(), roots, pairs, labels: pair_labels, members, cut_late: late.len() as u32 });
    }
    out
}

/// The chain shadow's trees, a line in all and one a tree: its born import, its members by name.
fn chains_report(b: &Built, born_late: usize, chains: &[Chain]) -> String {
    let mut out = String::new();
    let members: usize = chains.iter().map(|c| c.members.len()).sum();
    let largest = chains.iter().map(|c| c.members.len()).max().unwrap_or(0);
    let pairs: usize = chains.iter().map(|c| c.pairs.len()).sum();
    let late: u32 = chains.iter().map(|c| c.cut_late).sum();
    let _ = writeln!(out, "chain shadow: {} trees from born exports that split ({born_late} more imports read a round late, not followed), {members} members, at most {largest} a tree, {pairs} pairs, {late} path steps cut at a round-late read",
        chains.len());
    for c in chains {
        let names: Vec<&str> = c.members.iter().map(|&r| b.plan.blocks[r as usize].name.as_str()).collect();
        let roots: Vec<String> = c.roots.iter().map(|r| r.to_string()).collect();
        let _ = writeln!(out, "  {} into blocks {}: {} ({} pairs{})", b.labels[c.born as usize], roots.join(", "), names.join(" > "), c.pairs.len(),
            if c.cut_late > 0 { format!(", {} cut late", c.cut_late) } else { String::new() });
    }
    out
}

/// `C x`, `C` on `layout`'s value slots (`coords`), `x` and the result over the unknowns 1..=n.
fn c_times(c: &[f64], coords: &(Vec<u32>, Vec<u32>), x: &[f64]) -> Vec<f64> {
    let mut y = vec![0.0; x.len()];
    for v in 1..c.len() {
        if c[v] != 0.0 {
            y[coords.0[v] as usize] += c[v] * x[coords.1[v] as usize];
        }
    }
    y
}

/// Whether coupling `q`'s import can jump, which is all the split acts on (`Table::split`): a pad
/// or a timing cell's output (`circuit_opts`'s `control`), whose writes and flips are corners its
/// exporter lands on with the value's step, or any export of a block holding one of the circuit's
/// own switching devices (`switching`), whose crossings are edges on each of its exports. A rail
/// a pad's corner sags by microvolts takes such a step too, but no jump this test is for.
fn can_jump(b: &Built, o: &PartOpts, switching: &[bool], q: usize) -> bool {
    o.control.iter().any(|c| *c == b.labels[q]) || switching[b.plan.couplings[q].from.0 as usize]
}

/// The plan-time test of the voltage imports into every block a born jump reaches (A-K1 revisit,
/// and the relay answer), on the block's step matrices at the sample period `h`, in the
/// states that reach it:
/// - fast charge, in each state that holds the accuracy bar and each pad's first instant
///   (`State::first_instant`): from rest, a unit ramp on the import over one step, then one flat
///   step, both trapezoidal. On each node whose capacitor current turns back in the second step,
///   that current times `h` is the charge the trapezoid wants returned;
/// - slow hold, in each state that holds the accuracy bar, on the nodes some import charges: a
///   unit charge put on the node, followed `HOLD_STEPS` backward Euler steps: what the node still
///   holds;
/// - the figure: per import, the largest charge times the largest hold on its node;
/// - the transfer `T` of each import to each voltage export or owned heard net: its grid values from rest,
///   through the ramp and three flat steps, half their summed absolute second differences, at its
///   largest over the states (an ideal buffer of gain g: |g|; an RC much slower than the step:
///   about h / tau).
/// From each import a jump is born on (`can_jump`) at amplitude 1, the paths: through a block, an
/// import's amplitude times its transfer to an export reaches that export's readers, never a block
/// twice, followed while at least a thousandth of the next import's bar. A block is tested when a
/// path reaches it. Each reached pair starts with two figures divided by its bar, charge times hold
/// and `band_figure` at its largest transfer to an export or heard net. Each carries back independently
/// from a downstream pair where amplitude times that figure exceeds one. The upstream pair takes the larger of its
/// figure and the step's transfer times the downstream figure, until no figure grows or once a block.
/// Only a same-round downstream import carries the band figure back. A pair lands where its amplitude
/// times either figure exceeds one and passes the jump on as an event (`Run::step`'s `RELAYED`).
/// Once the rule finishes, a chain a run landed (`PartOpts::land`) adds its born pairs and the pairs
/// on same-round paths from them to the coupling the held member read. A landed pair read a
/// round late whose reader reaches its exporter through landed pairs would return its event
/// every round: it is split, and counted.
/// A state whose linearisation of the block is another's to the bit is that one's test, taken
/// once (the first state named). In states kept by key (a fixed start), a block's test is kept by
/// everything it reads (`ReaderTable`, `StateCache::table`): its distinct linearisations, its
/// shape, its imports, export and heard rows, and the step; a plan whose block has them all takes it,
/// whatever else changed. The amplitudes and verdicts are made again every plan.
/// Planning only. Per block tested: a linearisation a state; per distinct one not kept, one
/// factorisation and two solves an import (four with export or heard rows), then one more factorisation and
/// `HOLD_STEPS` solves a charged node.
fn charge_hold(b: &Built, whole: &Solver, others: &Others, t: f64, h: f64, o: &PartOpts, own: &[String], cache: &StateCache, key: u64) -> (Vec<ChargeHold>, Relay) {
    let switching: Vec<bool> = b.solvers.iter().map(|s| own.iter().any(|d| s.find_device(d).is_some())).collect();
    let nb = b.bounds.len();
    // per block: its voltage imports (boundary, coupling), and its voltage exports (coupling, row)
    let imports: Vec<Vec<(usize, usize)>> = b.bounds.iter().enumerate().map(|(r, bounds)| bounds.iter().enumerate().filter(|(_, x)| x.kind == BoundaryKind::Voltage && x.branch > 0)
        .filter_map(|(j, x)| b.plan.couplings.iter().position(|c| c.to.iter().any(|(k, name)| *k as usize == r && *name == x.name)).map(|q| (j, q)))
        .collect()).collect();
    let exports: Vec<Vec<(usize, usize)>> = (0..nb).map(|r| b.plan.couplings.iter().enumerate()
        .filter(|(_, c)| c.from.0 as usize == r && b.solvers[r].unknowns[c.from.1 as usize].kind == UnkKind::Voltage)
        .map(|(q, c)| (q, c.from.1 as usize - 1)).collect()).collect();
    let heard_names: BTreeSet<&str> = o.heard.iter().map(String::as_str).collect();
    let heard: Vec<bool> = whole.unknowns.iter().map(|u| u.kind == UnkKind::Voltage && heard_names.contains(u.name.as_str())).collect();
    let heard_rows: Vec<Vec<usize>> = b.plan.blocks.iter().map(|block| {
        block.own.iter().filter(|&&(_, global)| heard[global as usize]).map(|&(local, _)| local as usize - 1).collect()
    }).collect();
    let bar = |q: usize| if b.hub[q].is_some() { o.hub_tol } else { o.cut_tol };
    let mut tested: Vec<Option<Tested>> = (0..nb).map(|_| None).collect();
    // the paths from each born jump, deepest first: (block, its import, amplitude, the blocks passed)
    let mut stack: Vec<(usize, usize, f64, Vec<bool>)> = Vec::new();
    for (r, imps) in imports.iter().enumerate() {
        for (i, &(_, q)) in imps.iter().enumerate().filter(|(_, x)| can_jump(b, o, &switching, x.1)) {
            let mut passed = vec![false; nb];
            passed[r] = true;
            passed[b.plan.couplings[q].from.0 as usize] = true;
            stack.push((r, i, 1.0, passed));
        }
    }
    stack.reverse();
    let mut amp: Vec<Vec<f64>> = imports.iter().map(|x| vec![0.0; x.len()]).collect();
    let mut steps: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let mut step_transfers = Vec::new();
    let mut relay = Relay::default();
    while let Some((r, i, a, passed)) = stack.pop() {
        // a pair reached again no stronger adds nothing a path through it could not already carry
        if a <= amp[r][i] {
            continue;
        }
        amp[r][i] = a;
        let x = tested[r].get_or_insert_with(|| test_block(b, whole, others, t, h, r, &imports[r], &exports[r], &heard_rows[r], cache, key));
        let transfer = &x.table.transfer[i];
        for (e, &(q2, _)) in exports[r].iter().enumerate() {
            let a2 = a * transfer[e];
            for (k, _) in &b.plan.couplings[q2].to {
                let r2 = *k as usize;
                if passed[r2] {
                    continue;
                }
                for i2 in (0..imports[r2].len()).filter(|&i2| imports[r2][i2].1 == q2) {
                    if a2 < bar(q2) / 1000.0 {
                        relay.cut += 1;
                        continue;
                    }
                    relay.followed += 1;
                    steps.push(((r, i), (r2, i2)));
                    step_transfers.push(transfer[e]);
                    let mut more = passed.clone();
                    more[r2] = true;
                    stack.push((r2, i2, a2, more));
                }
            }
        }
    }
    let figure = |r: usize, i: usize| tested[r].as_ref().map_or(0.0, |x| if x.table.unjudged { f64::INFINITY } else { x.table.imports[i].figure });
    let f_top = o.band.min(o.fmax);
    let factor = band_figure(1.0, f_top, h);
    relay.band_at = (f_top, factor);
    let peak_transfer = |r: usize, i: usize| tested[r].as_ref().map_or(0.0, |x| x.table.transfer[i].iter().copied().fold(0.0, f64::max));
    let mut gain: Vec<Vec<f64>> = imports.iter().enumerate().map(|(r, imps)| {
        imps.iter().enumerate().map(|(i, &(_, q))| {
            if amp[r][i] > 0.0 { figure(r, i) / bar(q) } else { 0.0 }
        }).collect()
    }).collect();
    let mut band: Vec<Vec<f64>> = imports.iter().enumerate().map(|(r, imps)| {
        imps.iter().enumerate().map(|(i, &(_, q))| {
            if amp[r][i] > 0.0 { peak_transfer(r, i) * factor / bar(q) } else { 0.0 }
        }).collect()
    }).collect();
    // Each figure carries independently. Only the band pass stops at a late coupling.
    for (values, same_round_only) in [(&mut gain, false), (&mut band, true)] {
        // Read the previous pass so each pass extends the downstream figure by one step.
        let mut previous = Vec::new();
        for _ in 0..nb {
            previous.clone_from(values);
            let mut changed = false;
            for (&((r, i), (r2, i2)), &transfer) in steps.iter().zip(&step_transfers) {
                // Only a pair that lands at its own amplitude carries this figure back.
                if amp[r2][i2] * previous[r2][i2] <= 1.0 || (same_round_only && b.plan.couplings[imports[r2][i2].1].lag != Lag::Same) {
                    continue;
                }
                let carried = transfer * previous[r2][i2];
                // A zero transfer into an infinite figure gives NaN and cannot raise this pair's figure.
                if carried > values[r][i] {
                    values[r][i] = carried;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    let by_charge = |r: usize, i: usize| amp[r][i] * gain[r][i] > 1.0;
    let mut lands: Vec<Vec<bool>> = amp.iter().enumerate().map(|(r, amps)| {
        amps.iter().enumerate().map(|(i, &a)| by_charge(r, i) || a * band[r][i] > 1.0).collect()
    }).collect();
    // A conviction adds the born pairs and every same-round path from them to its target.
    let coupling = |(r, i): (usize, usize)| imports[r][i].1;
    for land in &o.land {
        let mut born = Vec::new();
        for (r, imps) in imports.iter().enumerate() {
            for (i, &(_, q)) in imps.iter().enumerate() {
                if b.labels[q] == land.born && can_jump(b, o, &switching, q) {
                    born.push((r, i));
                }
            }
        }
        if born.is_empty() {
            if !relay.absent.contains(&land.born) {
                relay.absent.push(land.born.clone());
            }
            continue;
        }
        let mut prefix = BTreeSet::new();
        if land.to != land.born {
            // Keep every incoming step on the tree, including where two paths meet.
            let mut parents: BTreeMap<_, Vec<_>> = born.iter().map(|&p| (p, Vec::new())).collect();
            let mut todo = born.clone();
            while let Some(p) = todo.pop() {
                for &(_, next) in steps.iter().filter(|s| s.0 == p) {
                    if b.plan.couplings[coupling(next)].lag == Lag::Same {
                        if !parents.contains_key(&next) {
                            todo.push(next);
                        }
                        parents.entry(next).or_default().push(p);
                    }
                }
            }
            todo.extend(parents.keys().copied().filter(|&p| b.labels[coupling(p)] == land.to));
            if todo.is_empty() {
                relay.off_tree.push(land.clone());
            }
            while let Some(p) = todo.pop() {
                if prefix.insert(p) {
                    todo.extend(parents[&p].iter().copied());
                }
            }
        }
        prefix.extend(born);
        relay.named.push((land.clone(), prefix.len()));
        for (r, i) in prefix {
            lands[r][i] = true;
            relay.by_name.insert((imports[r][i].1, r));
        }
    }
    let by_band: Vec<Vec<bool>> = imports.iter().enumerate().map(|(r, imps)| {
        imps.iter().enumerate().map(|(i, &(_, q))| lands[r][i] && !by_charge(r, i) && !relay.by_name.contains(&(q, r))).collect()
    }).collect();
    // a landed pair read a round late that closes a loop through landed pairs: split
    let from = |q: usize| b.plan.couplings[q].from.0 as usize;
    loop {
        let landed: Vec<(usize, usize)> = (0..nb).flat_map(|r| (0..imports[r].len()).map(move |i| (r, i))).filter(|&(r, i)| lands[r][i]).collect();
        let closes = landed.iter().copied().find(|&(r, i)| {
            let q = imports[r][i].1;
            if b.plan.couplings[q].lag == Lag::Same {
                return false;
            }
            let mut seen = vec![false; nb];
            let mut todo = vec![r];
            seen[r] = true;
            while let Some(x) = todo.pop() {
                if x == from(q) {
                    return true;
                }
                for &(r2, i2) in &landed {
                    if from(imports[r2][i2].1) == x && !seen[r2] {
                        seen[r2] = true;
                        todo.push(r2);
                    }
                }
            }
            false
        });
        let Some((r, i)) = closes else { break };
        lands[r][i] = false;
        relay.by_name.remove(&(imports[r][i].1, r));
        relay.loops += 1;
    }
    let mut out = Vec::new();
    for (r, imps) in imports.iter().enumerate() {
        for (i, &(_, q)) in imps.iter().enumerate() {
            let t = peak_transfer(r, i);
            let step = if lands[r][i] && !relay.by_name.contains(&(q, r)) && gain[r][i].is_finite() && band[r][i].is_finite() {
                1.0 / gain[r][i].max(band[r][i])
            } else {
                0.0
            };
            let row = ChargeHold { coupling: q, reader: r, jumps: can_jump(b, o, &switching, q), made: false, q: 0.0, node: String::new(), q_in: String::new(), hold: 0.0,
                hold_in: String::new(), figure: 0.0, bar: bar(q), t, band: t * factor, amp: amp[r][i], lands: lands[r][i], step, by_band: lands[r][i] && by_band[r][i] };
            out.push(match &tested[r] {
                None => row,
                Some(x) => {
                    let j = x.table.imports[i];
                    let (node, q_in, hold_in) = match j.node {
                        usize::MAX => (String::new(), String::new(), String::new()),
                        u => (b.solvers[r].unknowns[u + 1].name.clone(), x.names[j.q_at].clone(), x.names[j.hold_at].clone()),
                    };
                    ChargeHold { made: x.made, q: j.q, node, q_in, hold: j.hold, hold_in, figure: figure(r, i), ..row }
                }
            });
        }
        if let Some(x) = &tested[r] {
            relay.blocks += 1;
            for (i, &(_, q)) in imps.iter().enumerate() {
                relay.transfers.extend(exports[r].iter().enumerate().map(|(e, &(q2, _))| (q, r, q2, x.table.transfer[i][e])));
            }
        }
    }
    // A ghost can follow a block that no born jump reached. Rehearse it for the echoes
    // after the landing decisions, reusing the test wherever the relay already made it.
    for e in &b.plan.echoes {
        let (q, of) = (e.coupling as usize, e.of as usize);
        let r = b.plan.couplings[of].from.0 as usize;
        let x = tested[r].get_or_insert_with(|| test_block(b, whole, others, t, h, r, &imports[r], &exports[r], &heard_rows[r], cache, key));
        let i = imports[r].iter().position(|&(_, c)| c == q).expect("echo follows a voltage import");
        let j = exports[r].iter().position(|&(c, _)| c == of).expect("echo corrects a voltage export");
        relay.echo_responses.insert((q, r, of), x.table.gains[i][j]);
    }
    // and each echo's reader, for the size screen: its own export's response to its ghost's import
    for e in &b.plan.echoes {
        let (q, of, k) = (e.coupling as usize, e.of as usize, e.block as usize);
        let x = tested[k].get_or_insert_with(|| test_block(b, whole, others, t, h, k, &imports[k], &exports[k], &heard_rows[k], cache, key));
        let i = imports[k].iter().position(|&(_, c)| c == of);
        let j = exports[k].iter().position(|&(c, _)| c == q);
        if let Some(g) = i.zip(j).and_then(|(i, j)| x.table.gains.get(i)?.get(j)) {
            relay.echo_sense.insert((of, k, q), (*g, echo_bar(o.cut_tol, switching[k])));
        }
    }
    relay.steps = steps.iter().map(|&((r, i), (r2, i2))| ((imports[r][i].1, r), (imports[r2][i2].1, r2))).collect();
    relay.switching = switching;
    (out, relay)
}

/// One block's import test (`charge_hold`), kept or made: the table and, per distinct
/// linearisation in it, the state that names it.
struct Tested {
    table: Arc<ReaderTable>,
    made: bool,
    names: Vec<String>,
}

/// Block `r`'s import test (`charge_hold`) on its voltage `imports` (boundary, coupling),
/// `exports` (coupling, row) and owned heard rows, in the states that reach it. When the states
/// are kept the reader's key can take its table from `cache`.
fn test_block(b: &Built, whole: &Solver, others: &Others, t: f64, h: f64, r: usize, imports: &[(usize, usize)], exports: &[(usize, usize)], heard_rows: &[usize], cache: &StateCache, key: u64) -> Tested {
    // the run's states move with the run: a test in them is made every time
    let keep = others.base_seed().is_some();
    let s0 = &b.solvers[r];
    let block = Seedable::of(s0, whole);
    let reaches = |touch: &[bool]| block.unks.iter().any(|&(_, g)| touch[g]);
    // the states, each with whether the hold is read in it (not at a first instant)
    // each a seed put into one copy of the block in turn (none: the present state, the block as it is)
    let mut states: Vec<(String, Option<&Seed>, bool)> = Vec::new();
    match others.base_seed() {
        Some((base, what)) => states.push((what.to_string(), Some(base), true)),
        None => states.push(("the present state".into(), None, true)),
    }
    for m in others.mapped().filter(|m| !m.stability_only && reaches(m.touch)) {
        states.push((m.what.to_string(), Some(others.seed(m.at)), true));
    }
    for (seed, _, what) in others.first_instants().filter(|x| reaches(x.1)) {
        states.push((what.to_string(), Some(seed), false));
    }
    // each state's linearisation; one alike to the bit with one before is that one's (the held
    // states come first, so a first instant's twin keeps the hold read)
    let mut lins: Vec<(Vec<f64>, Vec<f64>)> = Vec::new();
    let mut first: Vec<usize> = Vec::new();
    let mut bits: HashMap<u128, usize> = HashMap::new();
    let mut key_bits = Bits::new();
    let mut copy = block.s.clone();
    for (k, (_, seed, _)) in states.iter().enumerate() {
        let (g, c, _) = match seed {
            Some(seed) => {
                seed.put(&mut copy, &block.unks, &block.devs);
                linearise(&copy, seed.t)
            }
            None => linearise(s0, t),
        };
        let mut at = Bits::new();
        at.f64s(&g);
        at.f64s(&c);
        let at = at.key();
        let same = |(g0, c0): &(Vec<f64>, Vec<f64>)| g0.iter().zip(&g).all(|(a, b)| a.to_bits() == b.to_bits()) && c0.iter().zip(&c).all(|(a, b)| a.to_bits() == b.to_bits());
        if bits.get(&at).is_some_and(|&d| same(&lins[d])) {
            continue;
        }
        bits.insert(at, lins.len());
        key_bits.u128(at);
        key_bits.u64(u64::from(states[k].2));
        lins.push((g, c));
        first.push(k);
    }
    let nodes: Vec<usize> = (0..s0.n).filter(|&i| s0.unknowns[i + 1].kind == UnkKind::Voltage).collect();
    let branches: Vec<usize> = imports.iter().map(|&(j, _)| b.bounds[r][j].branch as usize).collect();
    // Path steps and echo gains index the exports at the front. The band figure takes all rows.
    let rows: Vec<usize> = exports.iter().map(|&(_, row)| row).chain(heard_rows.iter().copied()).collect();
    // everything else the test reads: the shape, the step, the nodes, the imports' branches and
    // the export and heard rows
    let coords = s0.layout.coords();
    key_bits.u64(s0.n as u64);
    key_bits.u64(h.to_bits());
    key_bits.u64(HOLD_STEPS as u64);
    key_bits.u64s(coords.0.iter().zip(&coords.1).map(|(&a, &b)| u64::from(a) | (u64::from(b) << 32)));
    key_bits.u64s(nodes.iter().map(|&u| u as u64));
    key_bits.u64s(branches.iter().map(|&j| j as u64));
    key_bits.u64s(rows.iter().map(|&j| j as u64));
    let echo_gains = !b.plan.echoes.is_empty();
    if echo_gains { key_bits.u64(1); }
    let reader_key = key_bits.key();
    let kept = if keep { cache.table(reader_key, key) } else { None };
    let made = kept.is_none();
    let table = kept.unwrap_or_else(|| {
        let held: Vec<bool> = first.iter().map(|&k| states[k].2).collect();
        let made = Arc::new(test_reader(s0, &coords, &lins, &held, &nodes, &branches, &rows, h, echo_gains));
        if keep {
            cache.put_table(reader_key, key, made.clone());
        }
        made
    });
    Tested { table, made, names: first.iter().map(|&k| states[k].0.clone()).collect() }
}

/// One block's import test (`charge_hold`) on its distinct linearisations `lins` (`held`: whether
/// the hold is read in each), its voltage nodes, the branches of its imports and the rows of its
/// exports followed by its owned heard rows.
fn test_reader(s0: &Solver, coords: &(Vec<u32>, Vec<u32>), lins: &[(Vec<f64>, Vec<f64>)], held: &[bool], nodes: &[usize], branches: &[usize], rows: &[usize], h: f64, echo_gains: bool) -> ReaderTable {
    let n = s0.n;
    // per import and node: the largest charge and its linearisation; per node: the largest hold and its
    let mut charge: Vec<Vec<(f64, usize)>> = vec![vec![(0.0, 0); n]; branches.len()];
    let mut hold: Vec<(f64, usize)> = vec![(0.0, 0); n];
    let mut transfer: Vec<Vec<f64>> = vec![vec![0.0; rows.len()]; branches.len()];
    let mut gains = if echo_gains { vec![vec![EchoResponse::default(); rows.len()]; branches.len()] } else { Vec::new() };
    let mut unjudged = false;
    let mut judged = vec![true; lins.len()];
    let mut trap = s0.layout.lu::<f64>();
    let mut factored = false;
    for (k, (g, c)) in lins.iter().enumerate() {
        let a: Vec<f64> = g.iter().zip(c).map(|(g, c)| g + 2.0 / h * c).collect();
        // the pattern is the block's in every state: after the first, the pivots are reused
        if (if factored { trap.refactor(&a).or_else(|_| trap.factor(&a)) } else { trap.factor(&a) }).is_err() {
            unjudged = true;
            judged[k] = false;
            continue;
        }
        for (i, &br) in branches.iter().enumerate() {
            let mut e = vec![0.0; n];
            e[br - 1] = 1.0;
            let mut x1 = e.clone();
            trap.solve(&mut x1);
            let i1: Vec<f64> = c_times(c, coords, &x1).iter().map(|v| 2.0 / h * v).collect();
            let mut x2: Vec<f64> = e.iter().zip(&i1).map(|(e, i)| e + 2.0 * i).collect();
            trap.solve(&mut x2);
            let dx: Vec<f64> = x2.iter().zip(&x1).map(|(a, b)| a - b).collect();
            let i2: Vec<f64> = c_times(c, coords, &dx).iter().zip(&i1).map(|(v, i)| 2.0 / h * v - i).collect();
            for &u in nodes {
                if i1[u] * i2[u] < 0.0 && h * i2[u].abs() > charge[i][u].0 {
                    charge[i][u] = (h * i2[u].abs(), k);
                }
            }
            if rows.is_empty() {
                continue;
            }
            // the transfer: two more flat steps on the same factor, each from the last point and
            // its capacitor currents
            let mut grid = vec![x1, x2];
            let mut cur = i2;
            for _ in 0..2 {
                let last = &grid[grid.len() - 1];
                let cx = c_times(c, coords, last);
                let mut x: Vec<f64> = e.iter().zip(&cx).zip(&cur).map(|((e, cx), i)| e + 2.0 / h * cx + i).collect();
                trap.solve(&mut x);
                let dx: Vec<f64> = x.iter().zip(last).map(|(a, b)| a - b).collect();
                cur = c_times(c, coords, &dx).iter().zip(&cur).map(|(v, i)| 2.0 / h * v - i).collect();
                grid.push(x);
            }
            for (o, &row) in rows.iter().enumerate() {
                let v: Vec<f64> = [0.0, 0.0].into_iter().chain(grid.iter().map(|x| x[row])).collect();
                let tr = 0.5 * (1..v.len() - 1).map(|m| (v[m + 1] - 2.0 * v[m] + v[m - 1]).abs()).sum::<f64>();
                if echo_gains {
                    let g = &mut gains[i][o];
                    let response = v[v.len() - 1];
                    if tr > transfer[i][o] { g.static_g = response; }
                    g.min = if g.states == 0 { response } else { g.min.min(response) };
                    g.max = if g.states == 0 { response } else { g.max.max(response) };
                    g.states += 1;
                }
                transfer[i][o] = transfer[i][o].max(tr);
            }
        }
        factored = true;
    }
    // the hold, only on the nodes some import charges
    let charged: Vec<usize> = nodes.iter().copied().filter(|&u| charge.iter().any(|c| c[u].0 > 0.0)).collect();
    if !charged.is_empty() {
        let mut damped = s0.layout.lu::<f64>();
        let mut factored = false;
        for (k, (g, c)) in lins.iter().enumerate().filter(|(k, _)| held[*k] && judged[*k]) {
            let d: Vec<f64> = g.iter().zip(c).map(|(g, c)| g + c / h).collect();
            if (if factored { damped.refactor(&d).or_else(|_| damped.factor(&d)) } else { damped.factor(&d) }).is_err() {
                unjudged = true;
                continue;
            }
            factored = true;
            for &u in &charged {
                let mut y = vec![0.0; n];
                y[u] = 1.0 / h;
                damped.solve(&mut y);
                for _ in 1..HOLD_STEPS {
                    y = c_times(c, coords, &y).iter().map(|v| v / h).collect();
                    damped.solve(&mut y);
                }
                if y[u].abs() > hold[u].0 {
                    hold[u] = (y[u].abs(), k);
                }
            }
        }
    }
    let imports = (0..branches.len()).map(|i| {
        let best = charged.iter().map(|&u| (charge[i][u].0 * hold[u].0, u)).fold((0.0, usize::MAX), |m, x| if x.0 > m.0 { x } else { m });
        match best.1 {
            usize::MAX => Judged { q: 0.0, node: usize::MAX, q_at: 0, hold: 0.0, hold_at: 0, figure: best.0 },
            u => Judged { q: charge[i][u].0, node: u, q_at: charge[i][u].1, hold: hold[u].0, hold_at: hold[u].1, figure: best.0 },
        }
    }).collect();
    ReaderTable { imports, transfer, gains, unjudged }
}

/// One block's import test as kept (`StateCache::table`, `charge_hold`): per voltage import, in
/// order, the figure's parts, each state by its place among the block's distinct linearisations,
/// and its transfer to each voltage export and owned heard net; and whether a state could not judge it.
#[derive(Debug)]
pub(crate) struct ReaderTable {
    imports: Vec<Judged>,
    transfer: Vec<Vec<f64>>,
    gains: Vec<Vec<EchoResponse>>,
    unjudged: bool,
}

/// An import's signed final response at one export. The static fit is the response
/// at peak transient transfer. The extrema cover all successfully rehearsed linearisations.
#[derive(Clone, Copy, Debug, Default)]
pub struct EchoResponse {
    pub static_g: f64,
    pub min: f64,
    pub max: f64,
    pub states: usize,
}

/// One import's figure in a reader's test (`ReaderTable`): its charge, node and the linearisation
/// it was largest in, the node's hold and its linearisation (`node` `usize::MAX`: none charged).
#[derive(Clone, Copy, Debug)]
struct Judged {
    q: f64,
    node: usize,
    q_at: usize,
    hold: f64,
    hold_at: usize,
    figure: f64,
}

/// A 128-bit fingerprint of numbers by their bits (`charge_hold`'s keys), in two lanes, each a
/// bijection of its state per number: two runs of one length that differ in one number never meet.
struct Bits(u64, u64);

impl Bits {
    fn new() -> Bits {
        Bits(0x243f_6a88_85a3_08d3, 0x1319_8a2e_0370_7344)
    }

    fn u64(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(23) ^ x).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.1 = (self.1 ^ x.rotate_left(31)).wrapping_mul(0xc2b2_ae3d_27d4_eb4f).rotate_left(29);
    }

    fn u128(&mut self, x: u128) {
        self.u64((x >> 64) as u64);
        self.u64(x as u64);
    }

    /// A run, its length first.
    fn u64s(&mut self, x: impl ExactSizeIterator<Item = u64>) {
        self.u64(x.len() as u64);
        for v in x {
            self.u64(v);
        }
    }

    fn f64s(&mut self, x: &[f64]) {
        self.u64s(x.iter().map(|v| v.to_bits()));
    }

    fn key(&self) -> u128 {
        let mix = |mut h: u64| {
            h ^= h >> 33;
            h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
            h ^= h >> 33;
            h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
            h ^ (h >> 33)
        };
        (u128::from(mix(self.0)) << 64) | u128::from(mix(self.1))
    }
}

/// The plan-time test's lines in a plan's report (`charge_hold`): the blocks a born jump's paths
/// reach, the pairs that land (including those that land for a later pair, by band or by name), the largest
/// hazard split; the pairs that land and the largest split, with their figure's parts; the largest
/// transfers.
fn charge_hold_report(b: &Built, table: &[ChargeHold], relay: &Relay) -> String {
    let mut out = String::new();
    let hazard = |x: &ChargeHold| x.amp * (x.figure / x.bar);
    let mut reached: Vec<&ChargeHold> = table.iter().filter(|x| x.amp > 0.0).collect();
    let born = reached.iter().filter(|x| x.jumps).count();
    let lands = reached.iter().filter(|x| x.lands).count();
    let named = relay.by_name.len();
    let passed = reached.iter().filter(|x| x.lands && !x.by_band && hazard(x) <= 1.0 && !relay.by_name.contains(&(x.coupling, x.reader))).count();
    let by_band = reached.iter().filter(|x| x.lands && x.by_band).count();
    let (f_top, factor) = relay.band_at;
    let split = reached.iter().filter(|x| !x.lands).max_by(|a, c| hazard(a).total_cmp(&hazard(c)));
    let _ = writeln!(out, "fast charge, slow hold: {} imports into {} blocks tested ({born} born, {} reached along {} path steps, {} cut under a thousandth of the bar); {lands} land ({passed} for a landed pair after them, {by_band} by the band figure, {named} by name), {} split, {} split for a late loop; band figure at {f_top:.0} Hz, factor {factor:.3}; {} imports not reached; largest amp x figure split: {}",
        reached.len(), relay.blocks, reached.len() - born, relay.followed, relay.cut, reached.len() - lands, relay.loops, table.len() - reached.len(),
        split.map_or("none".into(), |x| format!("{:.2e} against {:.0e} ({} into block {} {})", x.amp * x.figure, x.bar, b.labels[x.coupling], x.reader, b.plan.blocks[x.reader].name)));
    if !relay.named.is_empty() || !relay.absent.is_empty() {
        let named: Vec<String> = relay.named.iter().map(|(land, pairs)| {
            let missing = if relay.off_tree.contains(land) { format!(", {} not on its tree", land.to) } else { String::new() };
            format!("{} to {} ({pairs} pairs{missing})", land.born, land.to)
        }).collect();
        let _ = writeln!(out, "chains landed by name: {}; not on the board, dropped: {}", if named.is_empty() { "none".into() } else { named.join(", ") },
            if relay.absent.is_empty() { "none".into() } else { relay.absent.join(", ") });
    }
    reached.sort_by(|a, c| c.lands.cmp(&a.lands).then(hazard(c).total_cmp(&hazard(a))));
    for x in reached.iter().take(lands.max(12)) {
        let step = if x.lands { format!("; lands steps over {:.1e} V", x.step) } else { String::new() };
        let _ = writeln!(out, "  {} into block {} {}{}: amp {:.2e} x figure {:.2e} = {:.2e} V/V against {:.0e} ({}); band T {:.2e} x amp = {:.2e}{step}; charge {:.2e} C/V at {} ({}), hold {:.2e} V/C ({})",
            b.labels[x.coupling], x.reader, b.plan.blocks[x.reader].name, if x.jumps { " (born)" } else { "" }, x.amp, x.figure, x.amp * x.figure, x.bar,
            if x.lands { "lands" } else { "split" }, x.t, x.amp * x.band, x.q, x.node, x.q_in, x.hold, x.hold_in);
    }
    let mut transfers: Vec<&(usize, usize, usize, f64)> = relay.transfers.iter().collect();
    transfers.sort_by(|a, c| c.3.total_cmp(&a.3));
    for &&(q, r, q2, tr) in transfers.iter().take(8) {
        let _ = writeln!(out, "  T {tr:.3e}: {} -> block {r} {} -> {}", b.labels[q], b.plan.blocks[r].name, b.labels[q2]);
    }
    out
}

fn report(h: &Held, b: &Built, cuts: &[Cut], log: &[String], map: &RoundMap, ms: [f64; 4], states: &str, tested: &[ChargeHold]) -> String {
    let mut out = String::new();
    let (blocks, solvers, couplings) = (&b.plan.blocks, &b.solvers, &b.plan.couplings);
    let returns = b.labels.iter().filter(|l| l.starts_with("the return")).count();
    // the held search's node tests are where a plan's time grows first (each attachment a share
    // does not anchor is tested): their share of the whole plan, beside the parts
    let (total, w) = (ms.iter().sum::<f64>(), &h.searched);
    let _ = writeln!(out, "round plan at t = {:.4} s: {} blocks, {} couplings ({} returns), {} ghosts; cost {total:.1} ms (held {:.1}, build {:.1}, round map {:.1}, imports' test {:.1}); {} node tests ({} failed) and {} in {} other states ({} failed, settled in {:.1} ms), {} at the present state as a guard ({} failed) took {:.1} ms, {:.0}% of the plan ({:.1} ms in the other states); late cuts read up to {} rounds late",
        h.t, blocks.len(), couplings.len(), returns, b.plan.ghosts.values().map(Vec::len).sum::<usize>(), ms[0], ms[1], ms[2], ms[3], w.tests, w.failed, w.state_tests, w.states, w.state_failed,
        w.settle_ms, w.guard_tests, w.guard_failed, w.ms, 100.0 * w.ms / total.max(1e-9), w.state_ms, map.late_rounds);
    // the blocks whose import test this plan made, and those it took from before (`charge_hold`)
    let readers = |made: bool| tested.iter().filter(|x| x.amp > 0.0 && x.made == made).map(|x| x.reader).collect::<BTreeSet<usize>>().len();
    let _ = writeln!(out, "  other states: {states}; maps in them: {} made, {} kept; imports' tests in them: {} readers made, {} kept", map.key_maps, map.kept, readers(true), readers(false));
    let (g, go) = (&map.gain, &map.gain_ordinary);
    let (w, wo) = (&map.wrong, &map.wrong_ordinary);
    let _ = writeln!(out, "  round map: {} late values on {} points ({} singular), {} maps, {} merges ({} for a cut's lateness, anomalies), cuts re-weighed at their lateness before the build: {} merged, {} unpipelined; loop gain {:.2e} at {} (under {STABILITY}), wrong by {:.1e} read late at {} (bar {:.0e}); through ordinary late values {:.2e} at {}, wrong by {:.1e} at {} (bar {:.0e}); sensitivity {:.1e}, gating nothing",
        map.lagged, map.points, map.singular, map.maps, map.merges, map.anomalies, map.reweighed, map.joins, g.0, g.1, w.0, w.1, h.hub_tol, go.0, go.1, wo.0, wo.1, h.cut_tol, map.sensitivity);
    if map.states > 0 || map.key_maps > 0 || map.kept > 0 {
        let _ = writeln!(out, "  the key's maps: the base and {} states that reach a block the map takes; the present state, a guard on the stability bar: {} merges, wrong read late {:.2} of its bar at {}{}",
            map.states, map.guard_merges, map.present.0, map.present.1, if map.present.0 > 1.0 { " (over it, gating nothing)" } else { "" });
    }
    // gating nothing: the plan's cuts the held step's old bar alone, rho under the node's bar at
    // the present state, would refuse
    let used: BTreeSet<u32> = couplings.iter().flat_map(|c| c.cuts.iter().map(|r| r.1)).collect();
    let old: Vec<String> = used.iter().map(|&c| &cuts[c as usize]).filter(|c| c.rho >= c.bar)
        .map(|c| format!("{} (rho {:.1e}, bar {:.0e})", h.names[c.node as usize], c.rho, c.bar)).collect();
    let _ = writeln!(out, "  cuts the old bar alone would refuse (rho under the node's bar at the present state), gating nothing: {}",
        if old.is_empty() { "none".to_string() } else { format!("{} ({})", old.len(), old.join(", ")) });
    if h.band < h.fmax {
        let above: Vec<_> = h.weights_full(h.round).into_iter().enumerate().filter(|&(k, _)| !in_band(h.test_point(k), h.band)).collect();
        let mut largest = None;
        for &c in &used {
            let cut = &cuts[c as usize];
            for &(k, w) in &above {
                let Some(&rho) = cut.acc_rho.get(k) else { continue };
                let ratio = late_wrong(rho, w) / cut.bar;
                if largest.is_none_or(|(v, _, _)| ratio > v) {
                    largest = Some((ratio, k, cut.node));
                }
            }
        }
        let held = largest.map_or_else(|| "the held tests have no measured cut".to_string(), |(v, k, node)| {
            format!("the held tests' largest {v:.2e} of its bar at {} ({})", h.point(k), h.names[node as usize])
        });
        let at = |peak: &(f64, String), bar: f64, unit: &str| {
            if peak.1.is_empty() { "unmeasured".to_string() } else { format!("{:.2e}{unit} at {}", peak.0 / bar, peak.1) }
        };
        let _ = writeln!(out, "  above the band ({}), stability alone, gating nothing: loop gain {}; the round map's accuracy {} (ordinary {}); {held}",
            hz(h.band), at(&map.gain_above, 1.0, ""), at(&map.wrong_above, h.hub_tol, " of its bar"), at(&map.wrong_ordinary_above, h.cut_tol, ""));
    }
    let follow: Vec<String> = map.followers.iter().filter(|f| f.2 >= h.cut_tol).take(8).map(|(v, at, gain, p)| format!("{v} follows {at} with gain {gain:.1e} at {p}")).collect();
    if !follow.is_empty() {
        let _ = writeln!(out, "  followers, gating nothing: {}", follow.join("; "));
    }
    for (k, (blk, s)) in blocks.iter().zip(solvers).enumerate() {
        let _ = writeln!(out, "  block {k} {}: stage {}, solves {} of its {} unknowns, {} devices", blk.name, blk.stage, blk.own.len(), s.n,
            s.banks.iter().map(|b| b.len()).sum::<usize>());
    }
    if !b.plan.unpipelined.is_empty() {
        let _ = writeln!(out, "  stages {:?} unpipelined: each runs right after the stage before it in its round", b.plan.unpipelined);
    }
    for ((l, label), lags) in couplings.iter().zip(&b.labels).zip(read_lags(&b.plan)) {
        let to: Vec<String> = l.to.iter().map(|(b, n)| format!("block {b} {n}{}", l.cut_of(*b).map_or(String::new(), |c| format!(" across the cut at {}", h.names[cuts[c as usize].node as usize])))).collect();
        let late = lags.iter().copied().max().unwrap_or(0);
        let _ = writeln!(out, "  coupling {label} from block {} unknown {} to {}: {}, rho {:.1e}", l.from.0, l.from.1, to.join(", "),
            if l.lag == Lag::Same { "same round".to_string() } else { format!("read up to {late} rounds late in the pipeline") }, l.rho);
    }
    for line in log {
        let _ = writeln!(out, "  {line}");
    }
    out
}

