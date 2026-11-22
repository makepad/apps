//! The live loop: observe, update, plan, execute, at the rate the arms' controllers take
//! setpoints. Anything behind [`CellLink`] runs it: the virtual cell, a dry run, a real cell.
//!
//! - Observe: encoders, the trackers, melt pool widths and line scans arrive stamped; each is
//!   used at the time it was captured.
//! - Update: beads the melt pool saw go into the as-built field where the tracker saw the tool,
//!   scans set the tops; the tracker's view of the TCP against where it was sent corrects the
//!   paths still to come (an outer loop over the arm's own). The part's heat runs on the
//!   thermal model from the power sent, its mean held to the plate's thermocouple: the field
//!   and the design hold the part cold, paths go where the warp puts it now, and what is seen
//!   hot goes into the field where the warp takes it back to.
//! - Plan: the route plans each task from the field as it is when an arm comes free; within a
//!   deposit, each setpoint's wire feed follows the height still missing under the tool.
//! - Execute: the next setpoints of the path (a horizon of the controller's latency) through
//!   IK and the arm's dynamic model: the torques the motion needs, the motor angles that bend
//!   the joints' springs onto the path, the maker's loop's lag led out where it has one.

use crate::path::{Motion, Profile, Turning, Waypoint};
use crate::route::{Ctx, Route, Task, TaskKind, IK, WORKING_END, W_MIN};
use crate::design::Design;
use fab_robot::arm::*;
use fab_robot::cell::Positioner;
use fab_robot::collide::{arms_gap, capsules, part_gap};
use fab_robot::dynamics::*;
use fab_robot::field::*;
use fab_robot::io::*;
use fab_robot::kin::{frames, ik, manipulability, IkGoal};
use fab_robot::math::*;
use fab_robot::process::{BeadModel, Heat, Thermal};
use std::collections::VecDeque;

/// How much of the loop runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
    /// A rigid slicer's: the path through IK and nothing else; no observation used.
    Rigid,
    /// The dynamic model's feed-forward (torques, spring pre-bend, the maker's loop led out);
    /// still blind.
    Feedforward,
    /// Feed-forward and every observation: the tracker's outer loop, the melt pool's beads and
    /// the scans in the field, the field in every plan.
    Live,
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub mode: LoopMode,
    /// How paths move: TCP acceleration and jerk, corner rounding, tool turn rate (each
    /// task's `hold` sets whether it rests at process starts).
    pub motion: Motion,
    /// The outer loop's bandwidth on the tracker (Hz): well under the arm's own loop's (its
    /// resonance sees the correction's steps unshaped).
    pub outer_hz: f64,
    /// Where each arm waits while the other works.
    pub park: Vec<Q>,
    /// The as-built field's sample spacing (mm).
    pub field_cell: f64,
    /// Stop after this much cell time (s).
    pub max_time: f64,
    /// Frames recorded per second.
    pub record_hz: f64,
    /// The bead model the planner plans with.
    pub bead: BeadModel,
    /// Cut with the table turning under the tool (the head nearly still) where a task allows it
    /// and that is the faster; else the table turns only between paths.
    pub coordinate: bool,
    /// The thermal model the planner keeps the part's heat on, and whether it takes the warp
    /// into account (off: it plans and reads the part as if it stood cold). A rigid plan never
    /// does.
    pub thermal: Thermal,
    pub warp: bool,
}

impl Opts {
    pub fn new(mode: LoopMode, park: Vec<Q>) -> Opts {
        let motion = Motion { accel: 400.0, jerk: f64::INFINITY, deviation: 0.05, turn: 0.6, lever: 0.0, rest: true };
        Opts { mode, motion, outer_hz: 1.0, park, field_cell: 0.25, max_time: 3600.0, record_hz: 25.0, bead: BeadModel::NOMINAL, coordinate: true, thermal: Thermal::NOMINAL, warp: true }
    }
}

enum Step {
    Head(usize),
    /// The positioner's axes (tilt, turn) from `from` to `to`, the arm held still and clear.
    Table { from: [f64; 2], to: [f64; 2] },
    Move { to: Q },
    /// `end`: the joint pose at its last waypoint, as traced through IK from its first;
    /// `turns`: the table's turn at each waypoint where it turns while the path runs (its tilt
    /// as it stands).
    Path { path: Vec<Waypoint>, bead: Vec<(f64, f64)>, profile: Profile, kind: TaskKind, hold: bool, end: Q, turns: Option<Vec<f64>> },
}

struct Active {
    step: Step,
    t0: f64,
    from: Q,
    dur: f64,
    /// Live: the path waits at each process start until the tracker sees the tool on it
    /// (the next such waypoint, and how long it has waited there).
    gate: usize,
    held: f64,
}

struct ArmRun {
    model: ArmModel,
    q: Q,
    q_prev: Q,
    steps: VecDeque<Step>,
    active: Option<Active>,
    next_send: f64,
    head_ready: Option<usize>,
    /// Where the TCP was sent to be, and when (part frame).
    targets: VecDeque<(f64, V3)>,
    /// The tracker's poses (cell frame) and when.
    seen: VecDeque<(f64, Pose)>,
    /// The outer loop's correction (cell frame: the arm's own error stays put as the table
    /// turns).
    correction: V3,
    /// The last setpoint's IK reached its goal.
    reached: bool,
    /// The tracker's error against the path, smoothed over 20 ms and as last seen (mm).
    err: f64,
    err_now: f64,
    last_tracker: f64,
    laser: f64,
    wire: f64,
    cutter: bool,
    /// The extruder's drive as sent and the melt it makes, by the head's model (mm³/s).
    extrude: f64,
    melt: f64,
    /// The wire feed as sent, for the beads the planner books: (from when, mm/s).
    feeds: VecDeque<(f64, f64)>,
    /// Where the last booked bead ended: (when, where, its base).
    bead_from: Option<(f64, V3, f64)>,
    /// When the laser last went off: a melt-pool report from before it cannot join one after.
    laser_off: f64,
    busy: f64,
    parked: bool,
    /// What the drives were asked for so far: each joint's torque squared, summed over the
    /// time (torque² s), and that time (s).
    heat: Q,
    heat_time: f64,
    /// Where its process works now (part frame, cold): the height its heat goes in at.
    work_z: f64,
}

/// One recorded moment, for the panel and playback.
#[derive(Clone, Debug)]
pub struct Frame {
    pub t: f64,
    /// Per arm: the link pose sent, where the TCP was sent (cell frame), where the tracker saw it
    /// (none before it first has).
    pub q: Vec<Q>,
    pub target: Vec<V3>,
    pub seen: Vec<Option<V3>>,
    /// What each arm is doing.
    pub doing: Vec<&'static str>,
    /// The next half second of the working arm's path (part frame).
    pub horizon: Vec<V3>,
    /// The field's revision then.
    pub field: u64,
    /// The positioner's setpoint then (tilt, turn).
    pub table: [f64; 2],
}

/// One arm as the planner drives it now.
#[derive(Clone, Copy, Debug)]
pub struct ArmNow {
    /// The joint setpoint last sent, and where the TCP was sent to be (cell frame).
    pub q: Q,
    pub target: V3,
    pub doing: &'static str,
    /// Its laser or cutter is on.
    pub on: bool,
    /// The tracker's error against the path, smoothed (mm; Live only).
    pub err: f64,
}

/// What a run did.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub time: f64,
    /// Per arm: seconds at work (moving or processing).
    pub busy: Vec<f64>,
    pub tasks: usize,
    pub flags: Vec<(V3, &'static str)>,
    /// Setpoints whose IK missed (out of reach).
    pub unreachable: usize,
    /// Tasks not run because their paths leave the arm's reach.
    pub skipped: Vec<TaskKind>,
    pub faults: Vec<String>,
    pub done: bool,
    /// Time per task kind: deposit, scan, cut (s).
    pub deposit_time: f64,
    pub scan_time: f64,
    pub cut_time: f64,
    /// What happened when (task starts, misses, faults).
    pub events: Vec<(f64, String)>,
    /// Per arm, each joint's RMS torque over the run as a share of its continuous rating: past
    /// 1 the drive overheats on a job run back to back (see [`Report::over_rating`]).
    pub rms_use: Vec<Q>,
    /// Live: process starts the tool had not settled onto within `GATE_MAX`, and the worst
    /// error one started with (mm).
    pub off_starts: usize,
    pub off_start_worst: f64,
    /// The positioner's moves and their time, settles included (s).
    pub table_moves: usize,
    pub table_time: f64,
    /// Cuts run with the table turning under them.
    pub coordinated: usize,
    /// Deposits, then scans, leaned off their axes clear of the wrist's singularity: how many,
    /// and the most one leaned (rad).
    pub leaned: [(usize, f64); 2],
    /// What runs nearer a singularity than a process pose may ([`W_MIN`]): the park, a kind of
    /// task; and the least manipulability it has there.
    pub low: Vec<(&'static str, f64)>,
    /// The part's heat as the planner held it: its hottest mean (K over ambient), and the
    /// furthest the warp moved a path's point (mm).
    pub hottest: f64,
    pub warp: f64,
}

/// An arm's park nearer a singularity than [`W_MIN`].
pub const LOW_PARK: &str = "park: no clear pose";
/// A task skipped because its arm cannot reach its start keeping clear of the part.
pub const NO_CLEAR_MOVE: &str = "no clear way to the start";
/// A task skipped because the table cannot move with the arm clear of the part.
pub const TABLE_BLOCKED: &str = "the table cannot move clear of the arm";
/// A task skipped because its path runs the arm (past the tool's working end) into the part.
pub const PATH_BLOCKED: &str = "the path runs the arm into the part";

impl Report {
    /// The joints whose RMS torque is past their continuous rating: (arm, joint, share), the
    /// worst first.
    pub fn over_rating(&self) -> Vec<(usize, usize, f64)> {
        let mut over: Vec<(usize, usize, f64)> = self.rms_use.iter().enumerate().flat_map(|(i, u)| u.iter().enumerate().filter(|(_, u)| **u > 1.0).map(move |(j, u)| (i, j, *u))).collect();
        over.sort_by(|a, b| b.2.total_cmp(&a.2));
        over
    }

    /// "over rating: J2 arm 1 1.7×, ..." when a joint is past its rating.
    pub fn over_rating_line(&self) -> Option<String> {
        let over = self.over_rating();
        (!over.is_empty()).then(|| format!("over rating: {}", over.iter().map(|(i, j, u)| format!("J{} arm {i} {u:.1}×", j + 1)).collect::<Vec<_>>().join(", ")))
    }

    /// "park: no clear pose, w 0.007; ..." when something runs near a singularity.
    pub fn low_line(&self) -> Option<String> {
        (!self.low.is_empty()).then(|| self.low.iter().map(|(what, w)| format!("{what}, w {w:.3}")).collect::<Vec<_>>().join("; "))
    }

    fn note_low(&mut self, what: &'static str, w: f64) {
        match self.low.iter_mut().find(|l| l.0 == what) {
            Some(l) => l.1 = l.1.min(w),
            None => self.low.push((what, w)),
        }
    }
}

pub struct Planner {
    info: CellInfo,
    design: Design,
    route: Box<dyn Route>,
    opts: Opts,
    field: BuildField,
    arms: Vec<ArmRun>,
    /// The arms as planned: inside their limits by the margin.
    models: Vec<ArmModel>,
    pending: Option<Task>,
    /// A planned task waiting for the other arms to park: its arm and steps.
    ready: Option<(usize, Vec<Step>)>,
    pub report: Report,
    pub frames: Vec<Frame>,
    next_frame: f64,
    obs: Vec<Observation>,
    finished: bool,
    /// A dry run: plans from the nominal stack (no observation comes).
    dry: bool,
    /// Where the positioner stands at the setpoints' time (tilt, turn), and its encoders as
    /// last read.
    table: [f64; 2],
    table_seen: Option<[f64; 2]>,
    /// Where it stood when, while it moved and where it stopped: what is seen is mapped through
    /// the part frame at the time it was seen.
    table_log: VecDeque<(f64, [f64; 2])>,
    /// The part's heat as the planner holds it, its mean then (for the thermocouple's late
    /// readings), and the metal laid so far (mm³, from the wire sent: the part's heat capacity).
    heat: Heat,
    bulk_log: VecDeque<(f64, f64)>,
    deposited: f64,
}

/// Two arms moving at once keep this far apart (mm).
const ARM_GAP: f64 = 30.0;

/// The fastest the outer loop moves its correction (mm/s).
const OUTER_RATE: f64 = 5.0;

/// Live: a process starts once the tool is this near its path (mm), or after waiting this
/// long (s).
const GATE_TOL: f64 = 0.05;
const GATE_MAX: f64 = 1.0;

/// Live: the process pauses while the tracker sees the tool this far off its path (mm), so a
/// swing neither gouges the part nor lays a bead astray; what it leaves the next scan finds.
const GUARD: f64 = 0.25;

/// A joint move ends with the arm held this long at its end (s): its springs ring down.
const SETTLE: f64 = 0.15;

/// How far inside each joint's hard limits the planner stays (rad).
const LIMIT_MARGIN: f64 = 0.08;

/// A positioner's move ends with it held this many of its servo's lags (its setpoint's last
/// creep); live, also until its encoders read it within `TABLE_TOL` (rad) of the setpoint (a
/// part 120 mm off its axes 0.012 mm off), which it must within `TABLE_WAIT` (s) more.
const TABLE_SETTLE: f64 = 5.0;
const TABLE_TOL: f64 = 1e-4;
const TABLE_WAIT: f64 = 1.0;
/// How far a tool draws back along its axis, out of the work, to move over it (mm).
const BACK_OFF: f64 = 80.0;
/// A way over the work: how far up over the middle of a move that does not keep clear its
/// way goes instead (mm), and how many times a half is split again.
const RAISE: f64 = 100.0;
const WAY_DEPTH: u32 = 3;
/// Live: the share of a thermocouple reading's difference from the planner's mean the mean
/// takes (the reading's noise against its lag behind the model).
const THERMO_GAIN: f64 = 0.5;

/// A lean off a path's axis goes in steps of this (rad), and clears the wrist's singularity
/// by this much manipulability over [`W_MIN`].
const LEAN_STEP: f64 = 2.5 * std::f64::consts::PI / 180.0;
const W_MARGIN: f64 = 0.005;


impl Planner {
    /// `start`: each arm's joint pose at the start, one per arm; the positioner (if any) stands
    /// level at turn 0.
    pub fn new(info: CellInfo, design: Design, route: Box<dyn Route>, opts: Opts, start: &[Q]) -> Planner {
        assert_eq!(start.len(), info.arms.len(), "a start pose per arm");
        let arms = info
            .arms
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let q = start[i];
                // Plan inside the hard limits: a following error must not reach them.
                let mut model = m.clone();
                for j in model.joints.iter_mut() {
                    if j.hi - j.lo > 4.0 * LIMIT_MARGIN {
                        j.lo += LIMIT_MARGIN;
                        j.hi -= LIMIT_MARGIN;
                    }
                }
                ArmRun {
                    model,
                    q,
                    q_prev: q,
                    steps: VecDeque::new(),
                    active: None,
                    next_send: 0.0,
                    head_ready: None,
                    targets: VecDeque::new(),
                    seen: VecDeque::new(),
                    correction: ZERO,
                    reached: true,
                    err: 0.0,
                    err_now: 0.0,
                    last_tracker: f64::NAN,
                    laser: 0.0,
                    wire: 0.0,
                    cutter: false,
                    extrude: 0.0,
                    melt: 0.0,
                    feeds: VecDeque::new(),
                    bead_from: None,
                    laser_off: f64::NEG_INFINITY,
                    busy: 0.0,
                    parked: true,
                    heat: [0.0; DOF],
                    heat_time: 0.0,
                    work_z: 0.0,
                }
            })
            .collect::<Vec<ArmRun>>();
        let field = BuildField::new(opts.field_cell);
        let models: Vec<ArmModel> = arms.iter().map(|a: &ArmRun| a.model.clone()).collect();
        // A park nearer a singularity than a process pose may be is the cell's to say.
        let mut report = Report::default();
        for (m, q) in models.iter().zip(&opts.park) {
            let w = manipulability(m, q);
            if w < W_MIN {
                report.note_low(LOW_PARK, w);
            }
        }
        Planner { info, design, route, opts, field, arms, models, pending: None, ready: None, report, frames: Vec::new(), next_frame: 0.0, obs: Vec::new(), finished: false, dry: false, table: [0.0; 2], table_seen: None, table_log: VecDeque::new(), heat: Heat::default(), bulk_log: VecDeque::new(), deposited: 0.0 }
    }

    /// The as-built field as the planner knows it.
    pub fn field(&self) -> &BuildField {
        &self.field
    }
    pub fn design(&self) -> &Design {
        &self.design
    }
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Arm `i` as driven now.
    pub fn arm_now(&self, i: usize) -> ArmNow {
        let a = &self.arms[i];
        ArmNow { q: a.q, target: target(a, &self.part()), doing: doing(a), on: a.laser > 0.0 || a.cutter || a.extrude > 0.0, err: a.err }
    }

    /// The positioner's setpoint now (tilt, turn; rad).
    pub fn table(&self) -> [f64; 2] {
        self.table
    }

    /// Whether cuts may run with the table turning under them ([`Opts::coordinate`]); set it
    /// before the run.
    pub fn set_coordinate(&mut self, on: bool) {
        self.opts.coordinate = on;
    }

    /// The part's heat as the planner holds it now.
    pub fn heat(&self) -> Heat {
        self.heat
    }

    /// Whether paths and readings take the warp into account.
    fn warps(&self) -> bool {
        self.opts.warp && self.opts.mode != LoopMode::Rigid
    }

    /// The material (cold) at `p` (part frame, as it stands now).
    fn cold(&self, p: V3) -> V3 {
        if self.warps() {
            self.opts.thermal.cold(&self.heat, p)
        } else {
            p
        }
    }

    /// The part's heat a tick of `dt` on: the absorbed share of the power each arm's process
    /// runs at, where it works; the metal its wire lays.
    fn warm(&mut self, t: f64, dt: f64) {
        let th = self.opts.thermal;
        let (mut watts, mut at) = (0.0, 0.0);
        for a in &self.arms {
            let w = th.absorb * a.laser + if a.cutter { th.absorb_cut * th.cut_power } else { 0.0 };
            watts += w;
            at += w * a.work_z;
            if let (ToolKind::LaserWire { wire_mm, .. }, true) = (a.model.tool().kind, a.laser > 0.0) {
                self.deposited += self.opts.bead.efficiency * 0.25 * std::f64::consts::PI * wire_mm * wire_mm * a.wire * dt;
            }
        }
        let capacity = th.base_capacity + th.capacity_per_mm3 * self.deposited;
        self.heat = th.step(self.heat, watts, at / watts.max(1e-12), capacity, dt);
        self.report.hottest = self.report.hottest.max(self.heat.bulk);
        if self.heat.bulk != 0.0 || !self.bulk_log.is_empty() {
            self.bulk_log.push_back((t + dt, self.heat.bulk));
            while self.bulk_log.len() > 4096 {
                self.bulk_log.pop_front();
            }
        }
    }

    /// The part frame in the cell frame, the positioner where it stands now.
    fn part(&self) -> Pose {
        self.info.part_frame(self.table)
    }

    /// Where the positioner stood at `at`: between the two logged stands either side.
    fn table_at(&self, at: f64) -> [f64; 2] {
        let log = &self.table_log;
        match log.iter().rposition(|x| x.0 <= at) {
            None => log.front().map_or(self.table, |x| x.1),
            Some(i) if i + 1 < log.len() => {
                let ((t0, a), (t1, b)) = (log[i], log[i + 1]);
                let u = ((at - t0) / (t1 - t0).max(1e-12)).clamp(0.0, 1.0);
                [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]
            }
            Some(i) => log[i].1,
        }
    }

    /// The positioner to `q` at `at`, logged.
    fn set_table(&mut self, at: f64, q: [f64; 2]) {
        self.table = q;
        if self.table_log.back().is_some_and(|x| x.0 >= at) {
            self.table_log.pop_back();
        }
        self.table_log.push_back((at, q));
        while self.table_log.len() > 1024 {
            self.table_log.pop_front();
        }
    }

    /// Run the loop until the build is done (or stops, or `max_time` passes).
    pub fn run(&mut self, link: &mut dyn CellLink) -> &Report {
        self.run_until(link, f64::MAX);
        &self.report
    }

    /// Run the loop to cell time `until`, or less if the build ends first; true once it has
    /// ended (done, stopped, or `max_time` passed).
    pub fn run_until(&mut self, link: &mut dyn CellLink, until: f64) -> bool {
        let tick = self.arms.iter().map(|a| 1.0 / a.model.controller.setpoint_hz).fold(f64::MAX, f64::min);
        while !self.finished && link.now() < until {
            let t = link.now();
            if t > self.opts.max_time {
                self.finished = true;
                break;
            }
            self.tick(link, t);
            self.warm(t, tick);
            let mut obs = std::mem::take(&mut self.obs);
            obs.clear();
            link.advance(t + tick, &mut obs);
            for o in &obs {
                self.observe(o);
            }
            self.obs = obs;
        }
        self.report.time = link.now();
        self.report.busy = self.arms.iter().map(|a| a.busy).collect();
        self.report.rms_use = self.arms.iter().map(rms_use).collect();
        self.finished
    }

    /// The job's timeline with no cell behind it: every task planned as the loop plans it (from
    /// the nominal layer stack, since no observation comes), each step as long as the loop runs
    /// it on the controller's setpoint ticks (a path its profile, a joint move its minimum-jerk
    /// time and the settle, a head change the head's switch), the next task a tick after.
    /// Only the steps' ends are visited, so it runs far faster than the cell. Its end time, as
    /// [`Planner::run`] reports it; the report holds the task times. A live gate's waits at
    /// process starts are not in it (the tasks that hold there wait on the tracker).
    pub fn dry_run(&mut self) -> f64 {
        self.dry = true;
        let tick = self.arms.iter().map(|a| 1.0 / a.model.controller.setpoint_hz).fold(f64::MAX, f64::min);
        let settle = self.table_settle();
        let mut k: u64 = 0;
        loop {
            let t = k as f64 * tick;
            if self.finished || t > self.opts.max_time {
                self.finished = true;
                break;
            }
            self.schedule(t);
            if self.finished {
                break;
            }
            let mut next = u64::MAX;
            for i in 0..self.arms.len() {
                let latency = self.arms[i].model.controller.latency;
                let tau = t + latency;
                // A head is ready its switch time after the change starts.
                let switched = match self.arms[i].active.as_ref().map(|s| (&s.step, s.t0)) {
                    Some((Step::Head(h), t0)) if tau - t0 >= self.arms[i].model.heads[*h].switch => Some(*h),
                    _ => None,
                };
                if let Some(h) = switched {
                    let a = &mut self.arms[i];
                    a.model.set_head(h);
                    a.head_ready = Some(h);
                }
                self.advance_steps(i, tau);
                let a = &mut self.arms[i];
                let Some(s) = a.active.as_ref() else { continue };
                // The pose the step ends at, for the next step's start and the next plan.
                let (end, len) = match &s.step {
                    Step::Head(h) => (a.q, a.model.heads[*h].switch),
                    Step::Table { .. } => (a.q, s.dur + settle),
                    Step::Move { to } => (*to, s.dur + SETTLE),
                    Step::Path { end, profile, .. } => (*end, profile.duration()),
                };
                // The first tick whose setpoint time reaches the step's end.
                let ends = ((s.t0 - latency + len) / tick - 1e-9).ceil().max(k as f64 + 1.0) as u64;
                if s.t0 == tau {
                    a.busy += (ends - k) as f64 * tick;
                }
                a.q_prev = a.q;
                a.q = end;
                next = next.min(ends);
            }
            let k1 = if next == u64::MAX { k + 1 } else { next };
            // Till then each arm holds where its step ends (or where it waits): the dry run's
            // torques are the holding ones.
            let gravity = self.info.gravity;
            for a in self.arms.iter_mut() {
                let hold = static_torque(&a.model, &frames(&a.model, &a.q), gravity, &Wrench::NONE);
                let dt = (k1 - k) as f64 * tick;
                for j in 0..DOF {
                    a.heat[j] += hold[j] * hold[j] * dt;
                }
                a.heat_time += dt;
            }
            k = k1;
        }
        let t = (k + 1) as f64 * tick;
        self.report.time = t;
        self.report.busy = self.arms.iter().map(|a| a.busy).collect();
        self.report.rms_use = self.arms.iter().map(rms_use).collect();
        t
    }

    fn observe(&mut self, o: &Observation) {
        let live = self.opts.mode == LoopMode::Live;
        let part = self.info.part_frame(self.table_at(o.at()));
        let part_inv = part.inv();
        match o {
            Observation::Tracker { arm, at, tcp } => {
                let a = &mut self.arms[arm.index()];
                a.seen.push_back((*at, *tcp));
                while a.seen.len() > 512 {
                    a.seen.pop_front();
                }
                if live {
                    let p = part_inv.apply(tcp.t);
                    if let Some(want) = target_at(&a.targets, *at) {
                        let dt = if a.last_tracker.is_nan() { 0.0 } else { (*at - a.last_tracker).clamp(0.0, 0.05) };
                        let k = std::f64::consts::TAU * self.opts.outer_hz * dt;
                        // Rate-limited, and held while the arm cannot follow (no wind-up).
                        // The arm's own error: kept in the cell frame (the table may turn).
                        let mut step = part.rotate(scale(sub(want, p), k));
                        let l = len(step);
                        if l > OUTER_RATE * dt {
                            step = scale(step, OUTER_RATE * dt / l);
                        }
                        if a.reached {
                            a.correction = add(a.correction, step);
                        }
                        let k = (-dt / 0.02).exp();
                        a.err = a.err * k + len(sub(want, p)) * (1.0 - k);
                        a.err_now = len(sub(want, p));
                        let l = len(a.correction);
                        if l > 15.0 {
                            a.correction = scale(a.correction, 15.0 / l);
                        }
                    }
                    a.last_tracker = *at;
                }
            }
            Observation::BeadWidth { arm, at, width } if live => {
                let Some(seen) = pose_at(&self.arms[arm.index()].seen, *at) else { return };
                // Seen on the hot part, booked on the cold.
                let p = self.cold(part_inv.apply(seen.t));
                let planned = target_at(&self.arms[arm.index()].targets, *at).map(|t| self.cold(t));
                let a = &mut self.arms[arm.index()];
                let wire = match a.model.tool().kind {
                    ToolKind::LaserWire { wire_mm, .. } => wire_mm,
                    _ => return,
                };
                // The bead sits on the surface the pass was planned on: the path's height there,
                // from the field before the pass (the field now has this pass's beads too).
                let floor = self.info.substrate.top(p[0], p[1]);
                let base = planned.map_or_else(|| self.field.top_below(add(p, [0.0, 0.0, 3.0]), 8.0).unwrap_or(floor).max(floor), |t| t[2]);
                // The pool was made with the wire fed then (the report is late) at the speed the
                // tracker saw (a soft arm lags and overshoots its profile).
                let Some(speed) = seen_speed(&a.seen, &part_inv, *at, 0.01) else { return };
                // A report from before the laser went off may come in after it did: a bead joins
                // only reports with no laser-off between them.
                let (from, from_base) = match a.bead_from {
                    Some((t0, from, from_base)) if !(t0 < a.laser_off && a.laser_off <= *at) => (from, from_base),
                    _ => (p, base),
                };
                a.bead_from = Some((*at, p, base));
                let area = self.opts.bead.area(wire, feed_at(&a.feeds, *at), speed);
                let height = BeadModel::height(area, *width);
                let bead = Bead { a: [from[0], from[1], from_base], b: [p[0], p[1], base], width: *width, height, up: [0.0, 0.0, 1.0] };
                if dist(bead.a, bead.b) < 8.0 {
                    self.field.add(&bead.shape(), 120);
                }
            }
            Observation::Profile { arm, at, points } if live => {
                let a = &self.arms[arm.index()];
                let Some(pose) = pose_at(&a.seen, *at) else { return };
                let at_part = part_inv.mul(&pose);
                let r = 0.6 * self.field.cell().max(0.4);
                for p in points {
                    let q = self.cold(at_part.apply(*p));
                    self.field.fuse_height(q[0], q[1], q[2].max(0.0), r, 230);
                }
            }
            Observation::Positioner { q, .. } => self.table_seen = Some(*q),
            Observation::Temperature { at, rise } if live => {
                // The reading is late: its difference from the mean as the planner held it then.
                let then = self.bulk_log.iter().rev().find(|x| x.0 <= *at).map_or(self.heat.bulk, |x| x.1);
                let step = THERMO_GAIN * (rise - then);
                self.heat.bulk += step;
                for x in self.bulk_log.iter_mut().filter(|x| x.0 > *at) {
                    x.1 += step;
                }
            }
            Observation::HeadReady { arm, head, .. } => {
                let a = &mut self.arms[arm.index()];
                a.model.set_head(*head as usize);
                a.head_ready = Some(*head as usize);
            }
            Observation::Drive { arm, at, state: DriveState::Fault(f) } => {
                self.report.faults.push(format!("arm {}: {f:?}", arm.0));
                let a = &self.arms[arm.index()];
                self.report.events.push((*at, format!("fault arm {} {f:?} at q {:?}", arm.0, a.q.map(|v| (v * 100.0).round() / 100.0))));
                self.finished = true;
            }
            _ => {}
        }
    }

    /// Plan what comes next when the arms are free.
    fn schedule(&mut self, t: f64) {
        let idle = |a: &ArmRun| a.active.is_none() && a.steps.is_empty();
        if !self.arms.iter().all(idle) {
            return;
        }
        if self.ready.is_none() {
            if self.pending.is_none() {
                let seeds = self.arms.iter().enumerate().map(|(i, a)| std::iter::once(a.q).chain(self.opts.park.get(i).copied()).collect()).collect();
                let observed = self.opts.mode == LoopMode::Live && !self.dry;
                let ctx = Ctx { design: &self.design, field: &self.field, arms: &self.models, seeds, substrate: self.info.substrate, observed, positioner: self.info.positioner.as_ref(), plate: self.info.plate, table: self.table, motion: self.opts.motion };
                self.pending = self.route.next(&ctx);
                self.report.flags.extend(self.route.take_flags());
                if self.pending.is_none() {
                    self.report.done = true;
                    self.finished = true;
                    return;
                }
            }
            let Some(task) = self.pending.take() else { return };
            self.ready = self.plan_task(t, task);
        }
        let Some((worker, steps)) = self.ready.take() else { return };
        // The others go to their parks; the worker sets off with them when its first move
        // keeps clear of theirs, else once they are there.
        let parks: Vec<(usize, Q, Q)> = self.arms.iter().enumerate().filter(|(i, a)| *i != worker && !a.parked).filter_map(|(i, a)| self.opts.park.get(i).map(|p| (i, a.q, *p))).collect();
        let from = self.arms[worker].q;
        let to = steps.iter().find_map(|s| if let Step::Move { to } = s { Some(*to) } else { None }).unwrap_or(from);
        let together = parks.iter().all(|(i, a, b)| self.moves_apart(worker, &from, &to, *i, a, b));
        for (i, _, p) in &parks {
            self.arms[*i].steps.push_back(Step::Move { to: *p });
            self.arms[*i].parked = true;
        }
        if parks.is_empty() || together {
            let a = &mut self.arms[worker];
            a.steps.extend(steps);
            a.parked = false;
        } else {
            self.ready = Some((worker, steps));
        }
    }

    /// A task's steps for its arm: the head it needs (the last one drawn out of the work
    /// first), the positioner to the task's table (the arm drawn clear first where the part
    /// would meet it), the joint moves to its start (drawn out, over and in when the direct one
    /// is not clear), its path. None, flagged, when its path leaves reach or runs the arm into
    /// the part, or no way keeps clear.
    fn plan_task(&mut self, t: f64, task: Task) -> Option<(usize, Vec<Step>)> {
        let worker = task.arm.index();
        self.report.tasks += 1;
        self.report.events.push((t, format!("arm {} {}: {} waypoints", task.arm.0, task.kind, task.path.len())));
        // The part frame the task runs in: its table's (a cell without a positioner has one).
        let table = if self.info.positioner.is_some() { task.table } else { self.table };
        let part = self.info.part_frame(table);
        let a = &self.arms[worker];
        let mut model = a.model.clone();
        model.set_head(task.head);
        // A scanner's line reads the same either way round: take the roll the arm reaches
        // best. Every task's path is traced through IK before it runs; one that leaves the
        // arm's reach is flagged, not run.
        let start = task.path[0].p;
        let kind = task.kind.clone();
        let mut options = vec![task];
        if options[0].kind == TaskKind::Scan {
            let mut flipped = options[0].clone();
            for w in flipped.path.iter_mut() {
                w.x = w.x.map(|x| scale(x, -1.0));
            }
            options.push(flipped);
        }
        // The start from where the arm is, its park, or mid-range (the route's own seeds): a
        // path one branch of the arm follows through and another loses is the first's.
        let mid: Q = std::array::from_fn(|j| 0.5 * (model.joints[j].lo + model.joints[j].hi));
        let seeds = [Some(a.q), self.opts.park.get(worker).copied(), Some(mid)];
        let from = a.q;
        let gravity = self.info.gravity;
        let positioner = self.info.positioner.as_ref();
        // Each option from each start, the table still or (a cut the route gave turns) turning
        // under it: the fewest misses, then the fewest poses into the part, then a start the
        // joints hold within their rating, then the least travel.
        let clear = |q: &Q, part: &Pose| self.pose_clear(worker, &model, q, part);
        let mut best: [Option<Pick>; 2] = [None, None];
        for (k, task) in options.iter().enumerate() {
            let turning = task.turns.as_ref().filter(|_| self.opts.coordinate && positioner.is_some());
            for (v, turns) in [(0, None), (1, turning)] {
                if v == 1 && turns.is_none() {
                    continue;
                }
                let part_at = |n: usize| turns.map_or(part, |tr| self.info.part_frame([table[0], tr[n]]));
                for s in seeds.iter().flatten() {
                    let Ok(q0) = ik(&model, &goal(&part_at(0), &task.path[0], ZERO), s, &IK) else { continue };
                    let tr = trace(&model, &part_at, &task.path, &q0, &clear);
                    let over = torque_use(&model, &frames(&model, &q0), gravity, &Wrench::NONE).iter().any(|u| *u > 1.0);
                    let travel: f64 = (0..DOF).map(|j| (q0[j] - from[j]).abs()).sum();
                    let key = (tr.misses, tr.hits, over);
                    if best[v].as_ref().is_none_or(|b| key < b.key || (key == b.key && travel < b.travel)) {
                        best[v] = Some(Pick { key, travel, q0, k, tr });
                    }
                }
            }
        }
        // Still or turning: the turning one where it runs clean and is no worse, the faster of
        // the two where both are clean (the table's move there, the arm's, the path).
        let motion = Motion { rest: options[0].hold, lever: len(model.tool().tcp.t), ..self.opts.motion };
        let settle = self.table_settle();
        let timed = |pick: &Pick, turns: Option<&Vec<f64>>| -> (f64, Profile) {
            let path = &options[pick.k].path;
            let (to, profile) = match (turns, positioner) {
                (Some(tr), Some(p)) => ([table[0], tr[0]], Profile::turning(path, &motion, Turning { turns: tr, vmax: 0.8 * p.vmax[1], accel: 0.8 * p.accel[1] })),
                _ => (table, Profile::new(path, &motion)),
            };
            let table_s = positioner.filter(|_| moved(self.table, to)).map_or(0.0, |p| table_time(p, &self.table, &to) + settle);
            (table_s + move_time(&model, &from, &pick.q0) + SETTLE + profile.duration(), profile)
        };
        let [still, turning] = best;
        let clean = |p: &Pick| p.key.0 == 0 && p.key.1 == 0;
        let (pick, turns, profile, times) = match (still, turning) {
            (Some(s), Some(c)) if clean(&c) && c.key <= s.key => {
                let turns = options[c.k].turns.clone();
                let ((ts, ps), (tc, pc)) = (timed(&s, None), timed(&c, turns.as_ref()));
                if c.key < s.key || tc < ts {
                    (c, turns, pc, Some((tc, ts)))
                } else {
                    (s, None, ps, None)
                }
            }
            (Some(s), _) => {
                let (_, ps) = timed(&s, None);
                (s, None, ps, None)
            }
            (None, Some(c)) => {
                let turns = options[c.k].turns.clone();
                let (_, pc) = timed(&c, turns.as_ref());
                (c, turns, pc, None)
            }
            (None, None) => return self.skip(t, &kind, start, "the start is out of reach", None),
        };
        let Pick { q0, k, tr, .. } = pick;
        if turns.is_some() {
            self.report.coordinated += 1;
            if let Some((tc, ts)) = times {
                self.report.events.push((t, format!("cuts with the table turning: {tc:.2} s against {ts:.2} s with it still")));
            }
        }
        // The table where the path starts, and the part frame there.
        let table = turns.as_ref().map_or(table, |tr| [table[0], tr[0]]);
        let part = self.info.part_frame(table);
        let mut task = options.swap_remove(k);
        let round = |v: V3| v.map(|x| (x * 100.0).round() / 100.0);
        if tr.misses > 0 {
            self.report.unreachable += tr.misses;
            let note = format!("{} waypoints out of reach, first at {:?}", tr.misses, round(tr.first_miss));
            return self.skip(t, &kind, tr.first_miss, "out of reach", Some(note));
        }
        if tr.hits > 0 {
            let note = format!("{} waypoints run the arm into the part, first at {:?}", tr.hits, round(tr.first_hit));
            return self.skip(t, &kind, tr.first_hit, PATH_BLOCKED, Some(note));
        }
        let (mut q0, mut end, mut w) = (q0, tr.end, tr.w_min);
        // A deposit or scan that runs the wrist near its singularity leans off its axis, as
        // little as clears it, within what its process allows; one no lean clears is flagged.
        let which = match kind {
            TaskKind::Deposit { .. } => Some(0),
            TaskKind::Scan => Some(1),
            _ => None,
        };
        if let Some(n) = which.filter(|_| w < W_MIN) {
            match self.lean(worker, &model, &part, &task.path, tr.low, &q0) {
                Ok((angle, path, q, e, w1)) => {
                    let l = &mut self.report.leaned[n];
                    (l.0, l.1) = (l.0 + 1, l.1.max(angle));
                    self.report.events.push((t, format!("leans the {kind} {:.1}° off its axis: w {w:.3} to {w1:.3}", angle.to_degrees())));
                    (task.path, q0, end, w) = (path, q, e, w1);
                }
                Err(most) => {
                    let window = model.tool().lean_window().to_degrees();
                    self.report.events.push((t, format!("the {kind} runs near the wrist's singularity, w {w:.3}: no lean within {window:.0}° clears {:.3} (at most {most:.3})", W_MIN + W_MARGIN)));
                }
            }
        }
        if w < W_MIN {
            self.report.note_low(near_singularity(&kind), w);
        }
        let mut steps = Vec::new();
        // Drawn out of the work, where that keeps clear: back along the tool's axis, up off the
        // plate, or between the two (a tool under an overhang draws back into the plate).
        let out = |m: &ArmModel, q: &Q, part: &Pose| {
            let f = frames(m, q);
            let (z, up) = (f.tcp.z(), part.rotate([0.0, 0.0, 1.0]));
            [scale(z, -1.0), up, norm(sub(up, z))].into_iter().find_map(|d| {
                let g = IkGoal { p: add(f.tcp.t, scale(d, BACK_OFF)), z, x: None };
                ik(m, &g, q, &IK).ok().filter(|o| self.move_clear(worker, m, q, o, part))
            })
        };
        let mut from = from;
        // A head change where the last head is still at the work (only a working head's end
        // may touch it): that head drawn out first.
        let here = &self.arms[worker].model;
        if here.head != task.head {
            if !self.pose_clear(worker, &model, &from, &self.part()) {
                let Some(o) = out(here, &from, &self.part()).filter(|o| self.pose_clear(worker, &model, o, &self.part())) else {
                    return self.skip(t, &kind, start, NO_CLEAR_MOVE, None);
                };
                steps.push(Step::Move { to: o });
                from = o;
            }
            steps.push(Step::Head(task.head));
        }
        // From here on the task's head (`model`) is the working one.
        let out = |q: &Q, part: &Pose| out(&model, q, part);
        let park = self.opts.park.get(worker).copied();
        // Over from one pose clear of the work to another: a clear way between them, else one
        // by the park.
        let over = |a: &Q, b: &Q, part: &Pose| {
            self.way(worker, &model, a, b, part, WAY_DEPTH).or_else(|| {
                let p = park?;
                let mut w = self.way(worker, &model, a, &p, part, WAY_DEPTH)?;
                w.push(p);
                w.extend(self.way(worker, &model, &p, b, part, WAY_DEPTH)?);
                Some(w)
            })
        };
        // The table moves with the arm held clear of the part: drawn out first where it is not,
        // on to the park where that is not enough.
        if moved(self.table, table) {
            if !self.table_clear(&model, &from, self.table, table) {
                let now = self.part();
                let way = match out(&from, &now) {
                    Some(o) if self.table_clear(&model, &o, self.table, table) => Some(vec![o]),
                    Some(o) => park.filter(|p| self.table_clear(&model, p, self.table, table)).and_then(|p| {
                        let mut w = vec![o];
                        w.extend(self.way(worker, &model, &o, &p, &now, WAY_DEPTH)?);
                        w.push(p);
                        Some(w)
                    }),
                    None => None,
                };
                let Some(way) = way else {
                    return self.skip(t, &kind, start, TABLE_BLOCKED, None);
                };
                from = way[way.len() - 1];
                steps.extend(way.into_iter().map(|to| Step::Move { to }));
            }
            steps.push(Step::Table { from: self.table, to: table });
        }
        // To the start: straight where that keeps clear of the part, else drawn out, over and in.
        if self.move_clear(worker, &model, &from, &q0, &part) {
            steps.push(Step::Move { to: q0 });
        } else {
            let way = match (out(&from, &part), out(&q0, &part)) {
                (Some(a), Some(b)) => over(&a, &b, &part).map(|mid| [vec![a], mid, vec![b, q0]].concat()),
                _ => None,
            };
            let Some(way) = way else {
                return self.skip(t, &kind, start, NO_CLEAR_MOVE, None);
            };
            steps.extend(way.into_iter().map(|to| Step::Move { to }));
        }
        steps.push(Step::Path { path: task.path, bead: task.bead, profile, kind: task.kind, hold: task.hold, end, turns });
        Some((worker, steps))
    }

    /// A task not run: flagged at `at` with `why`, and why in the events.
    fn skip(&mut self, t: f64, kind: &TaskKind, at: V3, why: &'static str, note: Option<String>) -> Option<(usize, Vec<Step>)> {
        self.report.flags.push((at, why));
        self.report.skipped.push(kind.clone());
        self.report.events.push((t, format!("skips {kind}: {}", note.unwrap_or_else(|| why.to_string()))));
        None
    }

    /// The positioner's settle after a move (s): none without one.
    fn table_settle(&self) -> f64 {
        self.info.positioner.as_ref().map_or(0.0, |p| TABLE_SETTLE * p.lag)
    }

    /// Whether the arm `m` held at `q` keeps clear of the part (as built, the design, the plate)
    /// while the table moves `from`..`to`, its working end too (the part comes to it).
    fn table_clear(&self, m: &ArmModel, q: &Q, from: [f64; 2], to: [f64; 2]) -> bool {
        let caps = capsules(m, &frames(m, q));
        (0..=8).all(|k| {
            let u = k as f64 / 8.0;
            let part = self.info.part_frame([from[0] + (to[0] - from[0]) * u, from[1] + (to[1] - from[1]) * u]).inv();
            let field = |p: V3| {
                let q = part.apply(p);
                self.field.clearance(q).min(self.design.distance(q)).min(self.info.substrate.distance(q))
            };
            part_gap(&caps, &field, 0.0).0 > 0.0
        })
    }

    /// Whether arm `i` moving `from_i`..`to_i` and arm `j` moving `from_j`..`to_j` at the same
    /// time keep `ARM_GAP` apart, whatever their pace.
    fn moves_apart(&self, i: usize, from_i: &Q, to_i: &Q, j: usize, from_j: &Q, to_j: &Q) -> bool {
        let at = |k: usize, from: &Q, to: &Q, u: f64| {
            let m = &self.arms[k].model;
            let q: Q = std::array::from_fn(|n| from[n] + (to[n] - from[n]) * u);
            capsules(m, &frames(m, &q))
        };
        let us = [0.0, 0.25, 0.5, 0.75, 1.0];
        let ci: Vec<_> = us.iter().map(|u| at(i, from_i, to_i, *u)).collect();
        let cj: Vec<_> = us.iter().map(|u| at(j, from_j, to_j, *u)).collect();
        ci.iter().all(|a| cj.iter().all(|b| arms_gap(a, b).0 > ARM_GAP))
    }

    fn tick(&mut self, link: &mut dyn CellLink, t: f64) {
        self.schedule(t);
        let _ = link.send(Command::Heartbeat { at: t });
        let gravity = self.info.gravity;
        let mode = self.opts.mode;
        for i in 0..self.arms.len() {
            if t + 1e-9 < self.arms[i].next_send {
                continue;
            }
            let hz = self.arms[i].model.controller.setpoint_hz;
            self.arms[i].next_send = t + 1.0 / hz - 1e-9;
            let latency = self.arms[i].model.controller.latency;
            let tau = t + latency;
            let dt = 1.0 / hz;
            self.advance_steps(i, tau);
            // The positioner's setpoint along its move, while one runs.
            if let Some(Active { step: Step::Table { from, to }, t0, dur, .. }) = &self.arms[i].active {
                let (h, hd, _) = min_jerk((tau - t0) / dur.max(1e-9), *dur);
                let q = [from[0] + (to[0] - from[0]) * h, from[1] + (to[1] - from[1]) * h];
                let qd = [(to[0] - from[0]) * hd, (to[1] - from[1]) * hd];
                self.set_table(tau, q);
                let _ = link.send(Command::Positioner { at: t, q, qd });
            }
            let part = self.part();
            let (q, qd, qdd, process) = self.desired(i, tau, dt, &part);
            // The positioner turning under a cut: where the path has it at `tau` (a hold at a
            // process start holds it too), its setpoint led by its own latency and lag over the
            // arm's so it stands there when the arm's setpoint lands.
            if let (Some(p), Some(Active { step: Step::Path { path, profile, turns: Some(turns), .. }, t0, .. })) = (self.info.positioner.as_ref(), &self.arms[i].active) {
                let tt = tau - t0;
                let turn = |t: f64| profile.turn_at(path, turns, t);
                let lead = tt + p.latency + p.lag - latency;
                let (tilt, now) = (self.table[0], turn(tt));
                let q = [tilt, turn(lead)];
                let qd = [0.0, (turn(lead + dt) - turn(lead - dt)) / (2.0 * dt)];
                self.set_table(tau, [tilt, now]);
                let _ = link.send(Command::Positioner { at: t, q, qd });
            }
            let a = &mut self.arms[i];
            a.q_prev = a.q;
            a.q = q;
            // An extruder's drive: the melt model's inverse, so the melt reaches the flow the
            // next setpoint needs within this one (pressure advance; where the flow stops the
            // drive runs back, a retraction), within the drive's limits.
            let process = process.map(|mut p| {
                if let ToolKind::Extruder(e) = a.model.tool().kind {
                    let alpha = 1.0 - (-dt / e.lag.max(1e-6)).exp();
                    let mut u = (a.melt + (p.extrude - a.melt) / alpha).clamp(-e.flow_max, e.flow_max);
                    if u.abs() < 0.01 && p.extrude == 0.0 {
                        u = 0.0;
                    }
                    a.melt += (u - a.melt) * alpha;
                    p.extrude = u;
                }
                p
            });
            let (cmd, need) = joint_cmd(&a.model, mode, &q, &qd, &qdd, gravity);
            for j in 0..DOF {
                a.heat[j] += need[j] * need[j] * dt;
            }
            a.heat_time += dt;
            let id = ArmId(i as u8);
            let _ = link.send(Command::Joints { arm: id, at: t, cmd });
            if let Some(p) = process {
                let at = (tau - 0.001).max(t);
                if p.laser != a.laser {
                    if p.laser == 0.0 {
                        a.laser_off = at;
                    }
                    a.laser = p.laser;
                    let _ = link.send(Command::Laser { arm: id, at, watts: p.laser });
                }
                if (p.wire - a.wire).abs() > 0.02 * a.wire.max(1e-9) || (p.wire == 0.0) != (a.wire == 0.0) {
                    a.wire = p.wire;
                    let _ = link.send(Command::Wire { arm: id, at, mm_s: p.wire });
                }
                if p.cutter != a.cutter {
                    a.cutter = p.cutter;
                    let _ = link.send(Command::Cutter { arm: id, at, on: p.cutter });
                }
                if (p.extrude - a.extrude).abs() > 0.02 * a.extrude.abs().max(1e-9) || (p.extrude == 0.0) != (a.extrude == 0.0) {
                    a.extrude = p.extrude;
                    let _ = link.send(Command::Extrude { arm: id, at, mm3_s: p.extrude });
                }
                if a.feeds.back().is_none_or(|f| f.1 != a.wire) {
                    a.feeds.push_back((at, a.wire));
                    while a.feeds.len() > 512 {
                        a.feeds.pop_front();
                    }
                }
            }
            if let Some(Step::Head(h)) = a.active.as_ref().map(|s| &s.step) {
                if a.head_ready.is_none() {
                    let _ = link.send(Command::Head { arm: id, at: t, head: *h as u8 });
                    a.head_ready = Some(usize::MAX);
                }
            }
            if a.active.is_some() {
                a.busy += dt;
            }
        }
        if t >= self.next_frame {
            self.next_frame = t + 1.0 / self.opts.record_hz;
            let part = self.part();
            self.record(t, &part);
        }
    }

    /// Finish what is done and start what comes next, for the setpoint at `tau`.
    fn advance_steps(&mut self, i: usize, tau: f64) {
        // Live, a table move ends once its encoders read it there too.
        let live = self.opts.mode == LoopMode::Live && !self.dry;
        let settle = self.table_settle();
        let off = |seen: Option<[f64; 2]>, to: &[f64; 2]| seen.map_or(f64::INFINITY, |q| (q[0] - to[0]).abs().max((q[1] - to[1]).abs()));
        let seen = self.table_seen;
        let a = &mut self.arms[i];
        let done = match &a.active {
            None => true,
            Some(s) => match &s.step {
                Step::Head(h) => a.head_ready == Some(*h),
                Step::Table { to, .. } => tau - s.t0 >= s.dur + settle && (!live || off(seen, to) <= TABLE_TOL),
                Step::Move { .. } => tau - s.t0 >= s.dur + SETTLE,
                Step::Path { profile, .. } => tau - s.t0 >= profile.duration(),
            },
        };
        if !done {
            if let Some(Active { step: Step::Table { to, .. }, t0, dur, .. }) = &a.active {
                if tau - t0 > dur + settle + TABLE_WAIT {
                    let (to, late) = (*to, tau - t0 - dur);
                    self.report.faults.push(format!("positioner: {:.5} rad off its setpoint {late:.1} s after its move", off(seen, &to)));
                    self.finished = true;
                }
            }
            return;
        }
        if let Some(s) = a.active.take() {
            match &s.step {
                Step::Path { profile, kind, turns, .. } => {
                    let d = profile.duration();
                    match kind {
                        TaskKind::Deposit { .. } => self.report.deposit_time += d,
                        TaskKind::Scan => self.report.scan_time += d,
                        TaskKind::Cut { .. } => self.report.cut_time += d,
                        TaskKind::Repair(_) => {}
                    }
                    if let Some(last) = turns.as_ref().and_then(|t| t.last()) {
                        self.set_table(tau, [self.table[0], *last]);
                    }
                }
                Step::Table { to, .. } => {
                    self.set_table(tau, *to);
                    self.report.table_time += tau - s.t0;
                }
                _ => {}
            }
        }
        let positioner = self.info.positioner.as_ref();
        let a = &mut self.arms[i];
        if let Some(step) = a.steps.pop_front() {
            let dur = match &step {
                Step::Move { to } => move_time(&a.model, &a.q, to),
                Step::Table { from, to } => {
                    self.report.table_moves += 1;
                    positioner.map_or(0.0, |p| table_time(p, from, to))
                }
                _ => 0.0,
            };
            if let Step::Head(_) = step {
                a.head_ready = None;
            }
            a.active = Some(Active { step, t0: tau, from: a.q, dur, gate: 0, held: 0.0 });
        }
    }

    /// The link pose, speed and acceleration wanted at `tau`, and the process then.
    fn desired(&mut self, i: usize, tau: f64, dt: f64, part: &Pose) -> (Q, Q, Q, Option<Process>) {
        let mode = self.opts.mode;
        // The path holds the part cold: each point goes where the heat has warped it to now.
        let (th, heat, warps) = (self.opts.thermal, self.heat, self.warps());
        let warp = |p: V3| if warps { th.warp(&heat, p) } else { p };
        let a = &mut self.arms[i];
        let (err, err_now) = (a.err, a.err_now);
        let Some(s) = a.active.as_mut() else { return (a.q, [0.0; DOF], [0.0; DOF], Some(Process::OFF)) };
        // Held at a process start: the arm is sent at rest there (no feed-forward of the motion
        // either side of it).
        let mut holding = false;
        if let Step::Path { path, profile, hold, .. } = &s.step {
            // Hold at the next process start while the tool is still settling onto the path;
            // one that has not settled by `GATE_MAX` starts all the same (the guard pauses the
            // process while it is off), and is counted.
            if mode == LoopMode::Live && *hold {
                while s.gate + 1 < path.len() && (path[s.gate].on || !path[s.gate + 1].on) {
                    s.gate += 1;
                }
                if s.gate + 1 < path.len() && tau - s.t0 >= profile.t[s.gate] {
                    if err > GATE_TOL && s.held < GATE_MAX {
                        s.held += dt;
                        s.t0 = tau - profile.t[s.gate];
                        holding = true;
                    } else {
                        if err > GATE_TOL {
                            self.report.off_starts += 1;
                            self.report.off_start_worst = self.report.off_start_worst.max(err);
                        }
                        s.gate += 1;
                        s.held = 0.0;
                    }
                }
            }
        }
        let s = &*s;
        match &s.step {
            Step::Head(_) | Step::Table { .. } => (a.q, [0.0; DOF], [0.0; DOF], Some(Process::OFF)),
            Step::Move { to } => {
                let (h, hd, hdd) = min_jerk((tau - s.t0) / s.dur.max(1e-9), s.dur);
                let d: Q = std::array::from_fn(|j| to[j] - s.from[j]);
                let q = std::array::from_fn(|j| s.from[j] + d[j] * h);
                let qd = std::array::from_fn(|j| d[j] * hd);
                let qdd = std::array::from_fn(|j| d[j] * hdd);
                (q, qd, qdd, Some(Process::OFF))
            }
            Step::Path { path, bead, profile, kind, hold, turns, .. } => {
                let correction = if mode == LoopMode::Live { a.correction } else { ZERO };
                // The part frame at each sample: the table's where it turns under the path.
                let (info, tilt) = (&self.info, self.table[0]);
                let part_at = |t: f64| turns.as_ref().map_or(*part, |tr| info.part_frame([tilt, profile.turn_at(path, tr, t)]));
                let pose_q = |t: f64, seed: &Q, model: &ArmModel| -> (Q, bool) {
                    let smp = profile.at(path, t);
                    let w = Waypoint { p: warp(smp.p), z: smp.z, x: smp.x, speed: smp.speed, on: smp.on };
                    let g = goal(&part_at(t), &w, correction);
                    match ik(model, &g, seed, &IK) {
                        Ok(q) => (q, true),
                        Err(m) => (m.q, m.err_mm < 0.05),
                    }
                };
                // Three samples under the same correction: speed and acceleration are the
                // path's, never the correction's steps.
                let tt = tau - s.t0;
                let (q0, _) = pose_q(tt - dt, &a.q, &a.model);
                let (q, ok) = pose_q(tt, &q0, &a.model);
                let (q0, q1) = if holding { (q, q) } else { (q0, pose_q(tt + dt, &q, &a.model).0) };
                a.reached = ok;
                if !ok {
                    if self.report.unreachable == 0 {
                        self.report.events.push((tau, format!("arm {i}: IK miss at {:?}", profile.at(path, tt).p)));
                    }
                    self.report.unreachable += 1;
                }
                let qd: Q = std::array::from_fn(|j| (q1[j] - q0[j]) / (2.0 * dt));
                let qdd: Q = std::array::from_fn(|j| (q1[j] - 2.0 * q[j] + q0[j]) / (dt * dt));
                let smp = profile.at(path, tt);
                let hot = warp(smp.p);
                self.report.warp = self.report.warp.max(dist(hot, smp.p));
                a.work_z = smp.p[2];
                a.targets.push_back((tau, hot));
                while a.targets.len() > 512 {
                    a.targets.pop_front();
                }
                let lost = mode == LoopMode::Live && *hold && err_now > GUARD;
                let process = match kind {
                    TaskKind::Deposit { .. } if !lost && matches!(a.model.tool().kind, ToolKind::Extruder(_)) => {
                        // The melt flow the bead needs by the next setpoint, at the speed the
                        // profile moves there (the drive that gets it there: `tick`).
                        let p = profile.at(path, tt + dt);
                        let extrude = if p.on {
                            let (w, h) = bead[p.seg];
                            bead_area(w, h) * p.speed
                        } else {
                            0.0
                        };
                        Some(Process { extrude, speed: smp.speed, ..Process::OFF })
                    }
                    TaskKind::Deposit { top, .. } if smp.on && !lost => {
                        // The height still missing under the tool: the layer's top over the
                        // path, which lies on the field as it was before the pass (the field
                        // under the tool now holds the bead just laid).
                        let (width, planned) = bead[smp.seg];
                        let wire = match a.model.tool().kind {
                            ToolKind::LaserWire { wire_mm, .. } => wire_mm,
                            _ => 0.8,
                        };
                        let height = (top - smp.p[2]).clamp(0.4 * planned.max(0.1), 1.6 * planned.max(0.1));
                        let (watts, feed) = self.opts.bead.settings(wire, width, height, smp.speed);
                        Some(Process { laser: watts, wire: feed, cutter: false, extrude: 0.0, speed: smp.speed })
                    }
                    TaskKind::Cut { .. } => Some(Process { cutter: smp.on && !lost, ..Process::OFF }),
                    _ => Some(Process::OFF),
                };
                (q, qd, qdd, process)
            }
        }
    }

    fn record(&mut self, t: f64, part: &Pose) {
        let mut horizon = Vec::new();
        let mut doing = Vec::new();
        for a in &self.arms {
            doing.push(self::doing(a));
            if let Some(Active { step: Step::Path { path, profile, .. }, t0, .. }) = &a.active {
                let mut k = 0.0;
                while k <= 0.5 {
                    horizon.push(profile.at(path, t - t0 + k).p);
                    k += 0.05;
                }
            }
        }
        let target = self.arms.iter().map(|a| target(a, part)).collect();
        let seen = self.arms.iter().map(|a| a.seen.back().map(|x| x.1.t)).collect();
        self.frames.push(Frame { t, q: self.arms.iter().map(|a| a.q).collect(), target, seen, doing, horizon, field: self.field.revision(), table: self.table });
    }

    /// Whether arm `i` as `m` at `q` keeps clear of the part (as built, and the design) and
    /// of the plate, the part frame at `part`, its tool's working end at the work; and
    /// `ARM_GAP` from the other arms where they wait while it works (at their parks, the
    /// planner sends them there; one with none where it is).
    fn pose_clear(&self, i: usize, m: &ArmModel, q: &Q, part: &Pose) -> bool {
        let inv = part.inv();
        let field = |p: V3| {
            let q = inv.apply(p);
            self.field.clearance(q).min(self.design.distance(q)).min(self.info.substrate.distance(q))
        };
        let caps = capsules(m, &frames(m, q));
        part_gap(&caps, &field, WORKING_END).0 > 0.0
            && self.arms.iter().enumerate().filter(|(k, _)| *k != i).all(|(k, o)| {
                let at = self.opts.park.get(k).copied().unwrap_or(o.q);
                arms_gap(&caps, &capsules(&o.model, &frames(&o.model, &at))).0 > ARM_GAP
            })
    }

    /// `path`, for arm `i` as `m` with the part frame at `part`, leaned off its axis clear of
    /// the wrist's singularity: the least lean within the tool's window, in `LEAN_STEP`s, about
    /// the level line toward the arm's base, across it or between (eight ways), whose path the arm follows
    /// from near `q0` in reach, clear of the part and `W_MARGIN` over [`W_MIN`] wherever the
    /// process runs. Each lean is tried first where the path ran nearest the singularity
    /// (`low`: that waypoint and the pose there), the one that clears it most first. The lean,
    /// the path, the poses at its start and end, and its least manipulability; where none
    /// clears, the most manipulability a lean reached there.
    fn lean(&self, i: usize, m: &ArmModel, part: &Pose, path: &[Waypoint], low: (usize, Q), q0: &Q) -> Result<(f64, Vec<Waypoint>, Q, Q, f64), f64> {
        let window = m.tool().lean_window();
        let base = part.inv().apply(m.base.t);
        let mid = scale(path.iter().fold(ZERO, |s, w| add(s, w.p)), 1.0 / path.len() as f64);
        let d = [base[0] - mid[0], base[1] - mid[1], 0.0];
        let toward = if len(d) > 1e-6 { norm(d) } else { [1.0, 0.0, 0.0] };
        let side = cross([0.0, 0.0, 1.0], toward);
        let axes: [V3; 8] = std::array::from_fn(|k| {
            let a = k as f64 * std::f64::consts::FRAC_PI_4;
            add(scale(side, a.cos()), scale(toward, a.sin()))
        });
        let want = W_MIN + W_MARGIN;
        let lean = |w: &Waypoint, r: &M3| Waypoint { z: mv(r, w.z), x: w.x.map(|x| mv(r, x)), ..*w };
        let clear = |q: &Q, part: &Pose| self.pose_clear(i, m, q, part);
        let mut most = 0.0f64;
        let mut k = 1;
        while k as f64 * LEAN_STEP <= window + 1e-9 {
            let angle = k as f64 * LEAN_STEP;
            let mut probes: Vec<(f64, M3)> = axes
                .iter()
                .filter_map(|a| {
                    let r = rot_axis(*a, angle);
                    let q = ik(m, &goal(part, &lean(&path[low.0], &r), ZERO), &low.1, &IK).ok()?;
                    let w = manipulability(m, &q);
                    most = most.max(w);
                    (w >= want).then_some((w, r))
                })
                .collect();
            probes.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (_, r) in probes {
                let leaned: Vec<Waypoint> = path.iter().map(|w| lean(w, &r)).collect();
                let Ok(q) = ik(m, &goal(part, &leaned[0], ZERO), q0, &IK) else { continue };
                let tr = trace(m, &|_| *part, &leaned, &q, &clear);
                if tr.misses == 0 && tr.hits == 0 && tr.w_min >= want {
                    return Ok((angle, leaned, q, tr.end, tr.w_min));
                }
            }
            k += 1;
        }
        Err(most)
    }

    /// The same along a joint move.
    fn move_clear(&self, i: usize, m: &ArmModel, from: &Q, to: &Q, part: &Pose) -> bool {
        (0..=8).all(|k| {
            let u = k as f64 / 8.0;
            let q: Q = std::array::from_fn(|j| from[j] + (to[j] - from[j]) * u);
            self.pose_clear(i, m, &q, part)
        })
    }

    /// A clear way for arm `i` as `m` from `a` to `b`, the poses between that joint moves go
    /// through (`b` not among them): straight where that keeps clear, else through a pose
    /// `RAISE` mm up over the middle of the two, each half found the same way, `depth` deep at
    /// most; none where no such way keeps clear.
    fn way(&self, i: usize, m: &ArmModel, a: &Q, b: &Q, part: &Pose, depth: u32) -> Option<Vec<Q>> {
        if self.move_clear(i, m, a, b, part) {
            return Some(Vec::new());
        }
        if depth == 0 {
            return None;
        }
        let mid: Q = std::array::from_fn(|j| 0.5 * (a[j] + b[j]));
        let (fa, fb) = (frames(m, a).tcp, frames(m, b).tcp);
        let up = part.rotate([0.0, 0.0, 1.0]);
        let z = add(fa.z(), fb.z());
        let z = if len(z) > 1e-6 { norm(z) } else { scale(up, -1.0) };
        let g = IkGoal { p: add(lerp(fa.t, fb.t, 0.5), scale(up, RAISE)), z, x: None };
        let v = ik(m, &g, &mid, &IK).ok().filter(|v| self.pose_clear(i, m, v, part))?;
        let mut w = self.way(i, m, a, &v, part, depth - 1)?;
        w.push(v);
        w.extend(self.way(i, m, &v, b, part, depth - 1)?);
        Some(w)
    }
}

/// What an arm is doing, in a word.
fn doing(a: &ArmRun) -> &'static str {
    match a.active.as_ref().map(|s| &s.step) {
        None => "idle",
        Some(Step::Head(_)) => "switching head",
        Some(Step::Table { .. }) => "turning the table",
        Some(Step::Move { .. }) => "travel",
        Some(Step::Path { kind: TaskKind::Deposit { .. }, .. }) => "deposit",
        Some(Step::Path { kind: TaskKind::Scan, .. }) => "scan",
        Some(Step::Path { kind: TaskKind::Cut { .. }, .. }) => "cut",
        Some(Step::Path { kind: TaskKind::Repair(_), .. }) => "repair",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Process {
    laser: f64,
    wire: f64,
    cutter: bool,
    extrude: f64,
    speed: f64,
}

impl Process {
    const OFF: Process = Process { laser: 0.0, wire: 0.0, cutter: false, extrude: 0.0, speed: 0.0 };
}

/// The IK goal for a waypoint (part frame) shifted by `correction` (cell frame), in the cell
/// frame.
fn goal(part: &Pose, w: &Waypoint, correction: V3) -> IkGoal {
    IkGoal { p: add(part.apply(w.p), correction), z: part.rotate(w.z), x: w.x.map(|x| part.rotate(x)) }
}

/// A start for a task: its key (misses, poses into the part, a start past the joints'
/// rating), the joint travel to it, the pose, which option, and its trace.
struct Pick {
    key: (usize, usize, bool),
    travel: f64,
    q0: Q,
    k: usize,
    tr: Traced,
}

/// A path through IK from `q0` waypoint by waypoint: how many leave reach, the first that
/// does, the pose at the last, and the least manipulability where the process is on.
struct Traced {
    /// Waypoints out of reach, the first's.
    misses: usize,
    first_miss: V3,
    /// Waypoints where the arm is into the part (`clear` says not), the first's.
    hits: usize,
    first_hit: V3,
    /// The arm at the path's end, and the least manipulability while the process runs, the
    /// waypoint where it is least and the pose there.
    end: Q,
    w_min: f64,
    low: (usize, Q),
}

/// The part frame at each waypoint is `part`'s.
fn trace(model: &ArmModel, part: &dyn Fn(usize) -> Pose, path: &[Waypoint], q0: &Q, clear: &dyn Fn(&Q, &Pose) -> bool) -> Traced {
    let mut tr = Traced { misses: 0, first_miss: ZERO, hits: 0, first_hit: ZERO, end: *q0, w_min: f64::INFINITY, low: (0, *q0) };
    for (n, w) in path.iter().enumerate() {
        let part = part(n);
        match ik(model, &goal(&part, w, ZERO), &tr.end, &IK) {
            Ok(next) => tr.end = next,
            Err(m) => {
                if tr.misses == 0 {
                    tr.first_miss = w.p;
                }
                tr.misses += 1;
                tr.end = m.q;
                continue;
            }
        }
        if !clear(&tr.end, &part) {
            if tr.hits == 0 {
                tr.first_hit = w.p;
            }
            tr.hits += 1;
        }
        if w.on {
            let m = manipulability(model, &tr.end);
            if m < tr.w_min {
                tr.w_min = m;
                tr.low = (n, tr.end);
            }
        }
    }
    tr
}

/// What a task of `kind` near a singularity is listed as.
fn near_singularity(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::Deposit { .. } => "deposit: near a singularity",
        TaskKind::Scan => "scan: near a singularity",
        TaskKind::Cut { .. } => "cut: near a singularity",
        TaskKind::Repair(_) => "repair: near a singularity",
    }
}

/// Where arm `a`'s TCP was sent to be (cell frame): on its path's last target while it runs
/// one, else where its setpoint puts it.
fn target(a: &ArmRun, part: &Pose) -> V3 {
    match (&a.active, a.targets.back()) {
        (Some(Active { step: Step::Path { .. }, .. }), Some(x)) => part.apply(x.1),
        _ => frames(&a.model, &a.q).tcp.t,
    }
}

/// Whether the table must move to go from `a` to `b`.
fn moved(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).abs() > TABLE_TOL || (a[1] - b[1]).abs() > TABLE_TOL
}

/// Minimum jerk at `u` (0..1) of a move taking `dur` s: the share done, its rate and its
/// rate's rate (1/s, 1/s²); speed and acceleration both start and end at zero.
fn min_jerk(u: f64, dur: f64) -> (f64, f64, f64) {
    let u = u.clamp(0.0, 1.0);
    let (u2, u3) = (u * u, u * u * u);
    let h = u3 * (10.0 - 15.0 * u + 6.0 * u2);
    let hd = 30.0 * u2 * (1.0 - u) * (1.0 - u) / dur;
    let hdd = 60.0 * u * (1.0 - u) * (1.0 - 2.0 * u) / (dur * dur);
    (h, hd, hdd)
}

/// A positioner's move: minimum jerk on both axes together, as long as the slower needs within
/// 0.8 of its speed and acceleration (as the arms' joint moves), at least 0.2 s.
fn table_time(p: &Positioner, from: &[f64; 2], to: &[f64; 2]) -> f64 {
    let mut t = 0.2f64;
    for k in 0..2 {
        let d = (to[k] - from[k]).abs();
        t = t.max(1.875 * d / (0.8 * p.vmax[k])).max((5.77 * d / (0.8 * p.accel[k])).sqrt());
    }
    t
}

/// Where the TCP was sent to be at `t`, if a path was running then (a joint move sends none).
fn target_at(targets: &VecDeque<(f64, V3)>, t: f64) -> Option<V3> {
    let i = targets.iter().rposition(|x| x.0 <= t)?;
    (t - targets[i].0 < 0.01).then_some(targets[i].1)
}

/// The wire feed sent for `t`.
fn feed_at(feeds: &VecDeque<(f64, f64)>, t: f64) -> f64 {
    feeds.iter().rfind(|f| f.0 <= t + 1e-9).map_or(0.0, |f| f.1)
}

/// How fast the tracker saw the TCP move over the part about `t` (mm/s), from its poses `half`
/// s either side.
fn seen_speed(seen: &VecDeque<(f64, Pose)>, part_inv: &Pose, t: f64, half: f64) -> Option<f64> {
    let i = seen.iter().rposition(|x| x.0 <= t - half)?;
    let j = seen.iter().rposition(|x| x.0 <= t + half)?;
    let dt = seen[j].0 - seen[i].0;
    (dt > 1e-6).then(|| dist(part_inv.apply(seen[i].1.t), part_inv.apply(seen[j].1.t)) / dt)
}

/// The tracker's pose nearest before `t`.
fn pose_at(seen: &VecDeque<(f64, Pose)>, t: f64) -> Option<Pose> {
    let i = seen.iter().rposition(|x| x.0 <= t + 1e-9).or(if seen.is_empty() { None } else { Some(0) })?;
    Some(seen[i].1)
}

/// A joint move's duration: the slowest joint within 0.8 of its speed and acceleration.
fn move_time(m: &ArmModel, from: &Q, to: &Q) -> f64 {
    let mut t = 0.2f64;
    for j in 0..DOF {
        let d = (to[j] - from[j]).abs();
        let v = 0.8 * m.joints[j].vmax;
        let a = 0.8 * m.controller.accel[j];
        // Minimum jerk peaks at 1.875 d / T in speed and 5.77 d / T² in acceleration.
        t = t.max(1.875 * d / v).max((5.77 * d / a).sqrt());
    }
    t
}

/// The drive setpoint for link pose `q` moving with `qd`, `qdd`, and the torque the drives must
/// give for it (the gears' share, the friction and the rotors'), whoever feeds it forward.
fn joint_cmd(m: &ArmModel, mode: LoopMode, q: &Q, qd: &Q, qdd: &Q, gravity: V3) -> (JointCmd, Q) {
    let kp: Q = std::array::from_fn(|j| m.joints[j].drive.kp);
    let kd: Q = std::array::from_fn(|j| m.joints[j].drive.kd);
    let f = frames(m, q);
    let mut wrench = Wrench::NONE;
    if let Some(c) = m.tool().cable {
        let at = f.flange.apply(c.attach);
        let force = c.force(at, m.base.apply(c.anchor));
        wrench = Wrench { force, moment: cross(sub(at, f.tcp.t), force) };
    }
    let link = rnea(m, &f, qd, qdd, gravity, &wrench);
    let mut th = [0.0; DOF];
    let mut tau = [0.0; DOF];
    for j in 0..DOF {
        let d = &m.joints[j].drive;
        // The spring bent onto the path: the motor ahead of the link by the torque over K
        // (what the link needs less what its preload pushes).
        let gear = link[j] - d.preload;
        th[j] = q[j] + gear / d.stiffness;
        let fric = d.coulomb_total() * (qd[j] / FRICTION_EPS).tanh() + d.viscous * qd[j];
        tau[j] = d.reflected() * qdd[j] + gear + fric;
    }
    let cmd = match (mode, m.controller.servo) {
        // A rigid plan sends the pose alone: the drives give the torque all the same.
        (LoopMode::Rigid, _) => JointCmd { th: *q, thd: *qd, tau: [0.0; DOF], kp, kd },
        (_, ServoLoop::Own) => JointCmd { th, thd: *qd, tau, kp, kd },
        (_, ServoLoop::Vendor { .. }) => {
            // The maker's loop holds gravity with its own model and tracks with kp, kd: lead
            // its setpoint by what it would otherwise lag.
            let g = static_torque(m, &frames(m, &th), gravity, &Wrench::NONE);
            let lead: Q = std::array::from_fn(|j| th[j] + (tau[j] - g[j]) / kp[j].max(1e-9));
            JointCmd { th: lead, thd: *qd, tau: [0.0; DOF], kp, kd }
        }
    };
    (cmd, tau)
}

/// Arm `a`'s RMS torque so far per joint, as a share of its continuous rating.
fn rms_use(a: &ArmRun) -> Q {
    std::array::from_fn(|j| (a.heat[j] / a.heat_time.max(1e-9)).sqrt() / a.model.joints[j].drive.torque_rated.max(1e-9))
}
