//! Routes: what to do next, decided from the design and the as-built field as they are now.
//!
//! A route hands out one [`Task`] at a time; the planner asks for the next only when an arm is
//! free, so every task is planned from the latest observations (nothing is laid out ahead for
//! the whole build). [`DedTrim`]: an additive pass (a few layers with an oversize margin on the
//! faces a cut can reach), a scan, a laser cut along the design surface, a scan; repeated to the
//! top.

use crate::design::{tracks, Design};
use crate::path::{Motion, Waypoint};
use fab_robot::arm::*;
use fab_robot::cell::{Positioner, Substrate};
use fab_robot::collide::{capsules, part_gap};
use fab_robot::field::BuildField;
use fab_robot::kin::{frames, ik, manipulability, IkGoal, IkOpts};
use fab_robot::math::*;
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq)]
pub enum TaskKind {
    /// Lay layer `layer` (its top at `top`).
    Deposit { layer: usize, top: f64 },
    /// Measure the tops along the walls.
    Scan,
    /// Cut the margin off the band `z0`..`z1` along the design surface.
    Cut { z0: f64, z1: f64 },
    /// What a craftsman's loop adds (later): fill a gap, trim an excess, re-melt.
    Repair(Repair),
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskKind::Deposit { layer, top } => write!(f, "deposit layer {} (top {top:.1} mm)", layer + 1),
            TaskKind::Scan => write!(f, "scan"),
            TaskKind::Cut { z0, z1 } => write!(f, "cut {z0:.1}..{z1:.1} mm"),
            TaskKind::Repair(r) => write!(f, "repair {r:?}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Repair {
    Fill { at: V3, volume: f64 },
    Trim { at: V3, depth: f64 },
    Remelt { at: V3 },
}

#[derive(Clone, Debug)]
pub struct Task {
    pub arm: ArmId,
    pub head: usize,
    pub kind: TaskKind,
    /// The path, approach and retract included (part frame).
    pub path: Vec<Waypoint>,
    /// The bead each deposit segment lays: (width, height), one per waypoint.
    pub bead: Vec<(f64, f64)>,
    /// Live: the tool rests and waits at each process start until the tracker sees it on the
    /// path, and the process pauses while it is off it (a weld or a cut; plastic prints on).
    pub hold: bool,
    /// The positioner's axes (tilt, turn; rad) the path runs at: the part frame is its table's
    /// there. A cell with none ignores them.
    pub table: [f64; 2],
    /// Where the table may instead turn while the path runs, each waypoint's face toward the
    /// arm (the head nearly still): the turn at each waypoint, the tilt staying `table`'s. The
    /// planner times both and takes the faster.
    pub turns: Option<Vec<f64>>,
}

/// What a route sees when it plans.
pub struct Ctx<'a> {
    pub design: &'a Design,
    /// The as-built field as the planner knows it.
    pub field: &'a BuildField,
    /// The arms as the planner plans them (inside their limits by its margin).
    pub arms: &'a [ArmModel],
    /// Joint poses to start IK from, per arm: where it is, where it parks.
    pub seeds: Vec<Vec<Q>>,
    pub substrate: Substrate,
    /// Plan from observations (heights from the field) or from the nominal layer stack.
    pub observed: bool,
    /// The positioner under the part, if the cell has one; the part frame on its table (or in
    /// the cell frame, the arms' bases' frame, without one).
    pub positioner: Option<&'a Positioner>,
    pub plate: Pose,
    /// Where the positioner's axes (tilt, turn) stand now.
    pub table: [f64; 2],
    /// How the planner moves paths (a route timing its own tasks uses the same).
    pub motion: Motion,
}

impl Ctx<'_> {
    /// Whether `arm` at `q` keeps clear of the part (as the field knows it, the design, the
    /// substrate) with the part frame at `part`, its working end at the work.
    pub fn clear(&self, arm: &ArmModel, q: &Q, part: &Pose) -> bool {
        let inv = part.inv();
        let field = |p: V3| {
            let q = inv.apply(p);
            self.field.clearance(q).min(self.design.distance(q)).min(self.substrate.distance(q))
        };
        part_gap(&capsules(arm, &frames(arm, q)), &field, WORKING_END).0 > 0.0
    }

    /// The part frame in the cell frame, the positioner at `table`.
    pub fn part(&self, table: [f64; 2]) -> Pose {
        match self.positioner {
            Some(p) => p.table_frame(table).mul(&self.plate),
            None => self.plate,
        }
    }

    /// The table level, turned as it stands: where deposits run (the pool flat) and scans.
    pub fn level(&self) -> [f64; 2] {
        [0.0, self.table[1]]
    }

    /// The tilts a cut may stand the table at: level first, then further either way, within
    /// its range by `TILT_MARGIN`; level alone without a positioner.
    fn tilts(&self) -> Vec<f64> {
        let Some(p) = self.positioner else { return vec![0.0] };
        let t: Vec<f64> = [0.0, 0.3, -0.3, 0.6, -0.6, 0.9, -0.9].into_iter().filter(|t| *t >= p.tilt[0] + TILT_MARGIN && *t <= p.tilt[1] - TILT_MARGIN).collect();
        // A range too narrow for any of them: its middle.
        if t.is_empty() {
            vec![0.5 * (p.tilt[0] + p.tilt[1])]
        } else {
            t
        }
    }

    /// The table's turn that points a face whose outward normal is `normal` (part frame) at the
    /// base `base` (cell frame), nearest the turn `now` (it is endless): on a level table, whose
    /// turn axis stands upright.
    fn facing(&self, normal: V3, base: V3, now: f64) -> f64 {
        let level = self.part([0.0, 0.0]);
        let d = level.rotate(normal);
        let to = sub(base, level.t);
        let want = to[1].atan2(to[0]) - d[1].atan2(d[0]);
        now + (want - now + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI
    }
}

/// How far inside its tilt range a cut stands the table (rad).
const TILT_MARGIN: f64 = 0.05;

/// The IK every plan uses.
pub const IK: IkOpts = IkOpts { iters: 40, tol_mm: 1e-3, tol_rad: 1e-5, damping: 0.06, w0: 0.02, centre: 0.05 };

/// The least manipulability a process pose may have: nearer a singularity a small correction
/// of the tool asks for large, fast joint motion (the outer loop's gain grows without bound).
pub const W_MIN: f64 = 0.02;

/// The working head's last this many mm are at the work: a pose keeps the rest of the arm and
/// its heads clear of the part.
pub const WORKING_END: f64 = 40.0;

pub trait Route: Send {
    fn next(&mut self, ctx: &Ctx) -> Option<Task>;
    fn label(&self) -> &'static str;
    /// The points it could not plan since the last call (a face the beam cannot leave, a pose
    /// out of reach): work left undone, for the report.
    fn take_flags(&mut self) -> Vec<(V3, &'static str)> {
        Vec::new()
    }
}

/// A cut point no reachable pose serves (or only one near a singularity, or one that meets the
/// part with the arm or a head).
pub const CUT_UNREACHED: &str = "the cut is out of reach (or near a singularity, or blocked) here";
/// A cut point no beam inclination can leave without meeting the design or the plate.
pub const BEAM_BLOCKED: &str = "the beam cannot leave this face";

/// The settings of the deposit-and-trim route.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DedTrimOpts {
    pub layer: f64,
    pub bead_width: f64,
    /// Bead centre lines apart (mm).
    pub spacing: f64,
    /// Each ring stops this far short of where it started (mm): its bead's round end lands
    /// beside the round start, overlapping it a little, not up on it.
    pub seam_gap: f64,
    /// Deposited outside the design surface on the faces a cut reaches (mm).
    pub margin: f64,
    pub layers_per_cut: usize,
    pub cut_speed: f64,
    pub scan_speed: f64,
    /// How far above the work approaches and retracts start (mm); along the beam for a cut.
    pub approach: f64,
    pub cut_approach: f64,
    /// The beam must pass finished material and the plate by this much (mm).
    pub beam_clearance: f64,
    /// A cut face's tolerance: a beam reaches down the band as far as its kerf's inner edge
    /// stays within this of the design (mm), and the passes step by that reach.
    pub finish: f64,
    /// Trim at all (a deposit-only build for comparison).
    pub trim: bool,
}

impl DedTrimOpts {
    /// How fast `head` lays its tracks: its wire rate fills tracks `spacing` apart and `layer`
    /// high (mm/s); none for a head that lays no wire.
    pub fn deposit_speed(&self, head: &Tool) -> Option<f64> {
        head.wire_rate().map(|r| r / (self.spacing * self.layer))
    }
}

impl Default for DedTrimOpts {
    fn default() -> Self {
        DedTrimOpts { layer: 1.0, bead_width: 3.0, spacing: 2.1, seam_gap: 0.75, margin: 0.4, layers_per_cut: 2, cut_speed: 20.0, scan_speed: 30.0, approach: 20.0, cut_approach: 5.0, beam_clearance: 0.05, finish: 0.1, trim: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Deposit(usize),
    ScanDeposit(usize),
    Cut(usize),
    ScanCut(usize),
    Done,
}

/// Deposit with a margin, scan, cut the margin off along the design, scan; to the top.
pub struct DedTrim {
    pub opts: DedTrimOpts,
    step: Step,
    layers: usize,
    /// The current band's cut, one piece per task, and whether it is planned.
    pieces: VecDeque<Task>,
    planned: bool,
    /// Cut points left undone, until the planner takes them.
    flags: Vec<(V3, &'static str)>,
}

impl DedTrim {
    pub fn new(design: &Design, opts: DedTrimOpts) -> DedTrim {
        let layers = ((design.hi[2] - design.lo[2].max(0.0)) / opts.layer).round().max(1.0) as usize;
        DedTrim { opts, step: Step::Deposit(0), layers, pieces: VecDeque::new(), planned: false, flags: Vec::new() }
    }

    pub fn layers(&self) -> usize {
        self.layers
    }

    /// The route from the cut of the block that ends with layer `layer` on: for a part laid up
    /// to there already.
    pub fn from_cut(mut self, layer: usize) -> DedTrim {
        self.step = Step::Cut(layer);
        self
    }

    fn arm_with(arms: &[ArmModel], f: impl Fn(&Tool) -> bool) -> Option<(ArmId, usize)> {
        arms.iter().enumerate().find_map(|(i, a)| a.heads.iter().position(&f).map(|h| (ArmId(i as u8), h)))
    }

    fn deposit(&self, ctx: &Ctx, layer: usize) -> Option<Task> {
        let (arm, head) = Self::arm_with(ctx.arms, |t| t.wire_rate().is_some())?;
        let o = &self.opts;
        let speed = o.deposit_speed(&ctx.arms[arm.index()].heads[head])?;
        let top = (layer + 1) as f64 * o.layer;
        let z_mid = top - 0.5 * o.layer;
        let margin = if o.trim { o.margin } else { 0.0 };
        let mut path = Vec::new();
        let mut bead = Vec::new();
        let down = [0.0, 0.0, -1.0];
        for region in ctx.design.layer(z_mid, margin) {
            for (ring, width) in tracks(&region, o.bead_width, o.spacing) {
                if ring.len() < 3 {
                    continue;
                }
                // Each ring closed, starting where the last ended nearest (the first point).
                let z_of = |x: f64, y: f64| {
                    let floor = ctx.substrate.top(x, y).max(0.0);
                    if ctx.observed {
                        ctx.field.top_below([x, y, top + 2.0], 4.0 * o.layer).unwrap_or(floor).max(floor)
                    } else {
                        top - o.layer
                    }
                };
                let run = short_of_start(&ring, o.seam_gap);
                let (first, last) = (run[0], run[run.len() - 1]);
                let z0 = z_of(first[0], first[1]);
                path.push(Waypoint { p: [first[0], first[1], z0 + o.approach], z: down, x: None, speed: speed * 4.0, on: false });
                bead.push((0.0, 0.0));
                for (k, q) in run.iter().enumerate() {
                    let z = z_of(q[0], q[1]);
                    path.push(Waypoint { p: [q[0], q[1], z], z: down, x: None, speed: if k == 0 { speed * 2.0 } else { speed }, on: k > 0 });
                    bead.push((width, top - z));
                }
                path.push(Waypoint { p: [last[0], last[1], z_of(last[0], last[1]) + o.approach], z: down, x: None, speed: speed * 4.0, on: false });
                bead.push((0.0, 0.0));
            }
        }
        (!path.is_empty()).then(|| Task { arm, head, kind: TaskKind::Deposit { layer, top }, path, bead, hold: true, table: ctx.level(), turns: None })
    }

    /// Straight lanes over the part, the scanner's line across them: its roll stays put all
    /// the way (a pass around the walls would wind the wrist a full turn).
    fn scan(&self, ctx: &Ctx, top: f64) -> Option<Task> {
        let (arm, head) = Self::arm_with(ctx.arms, |t| t.scans())?;
        let ToolKind::Scanner { width, .. } = ctx.arms[arm.index()].heads[head].kind else { return None };
        let o = &self.opts;
        let d = ctx.design;
        let pad = o.margin + 2.0;
        let (x0, x1, y0, y1) = (d.lo[0] - pad, d.hi[0] + pad, d.lo[1] - pad, d.hi[1] + pad);
        let lanes = ((x1 - x0) / (0.8 * width)).ceil().max(1.0) as usize;
        let pitch = (x1 - x0) / lanes as f64;
        let (down, line) = ([0.0, 0.0, -1.0], Some([1.0, 0.0, 0.0]));
        let mut path = Vec::new();
        for k in 0..lanes {
            let x = x0 + (k as f64 + 0.5) * pitch;
            let (a, b) = if k % 2 == 0 { (y0, y1) } else { (y1, y0) };
            if k == 0 {
                path.push(Waypoint { p: [x, a, top + o.approach], z: down, x: line, speed: o.scan_speed, on: false });
            }
            path.push(Waypoint { p: [x, a, top], z: down, x: line, speed: o.scan_speed, on: false });
            path.push(Waypoint { p: [x, b, top], z: down, x: line, speed: o.scan_speed, on: true });
        }
        let last = *path.last()?;
        path.push(Waypoint { p: add(last.p, [0.0, 0.0, o.approach]), on: false, ..last });
        let bead = vec![(0.0, 0.0); path.len()];
        Some(Task { arm, head, kind: TaskKind::Scan, path, bead, hold: true, table: ctx.level(), turns: None })
    }

    /// The band `z0`..`z1`'s cut, one task per run, and the points no run could serve: passes
    /// along the design's outer contours, one above the other as the beam's reach down the band
    /// needs, each contour in arcs of at most a quarter turn of the face (the head's roll stays
    /// in range) with the beam in the face's tangent plane and the head on the cutting arm's
    /// side; split where the beam cannot leave the face. A positioner stands each arc where it
    /// faces the arm, tilted where the arc's beams reach furthest down the band (a tilt lays a
    /// steep beam flatter in the cell, its pose clear of the wrist's singularity).
    fn cut(&self, ctx: &Ctx, z0: f64, z1: f64) -> (VecDeque<Task>, Vec<(V3, &'static str)>) {
        let mut out = VecDeque::new();
        let mut flags = Vec::new();
        let Some((arm, head)) = Self::arm_with(ctx.arms, |t| matches!(t.kind, ToolKind::LaserCut { .. })) else { return (out, flags) };
        let model = &ctx.arms[arm.index()];
        let tool = &model.heads[head];
        let ToolKind::LaserCut { kerf, defocus, .. } = tool.kind else { return (out, flags) };
        let mut arm_model = model.clone();
        arm_model.set_head(head);
        let mid_range: Q = std::array::from_fn(|j| 0.5 * (model.joints[j].lo + model.joints[j].hi));
        let seeds: Vec<Q> = ctx.seeds[arm.index()].iter().copied().chain([mid_range]).collect();
        let reach = Reach { arm: &arm_model, ctx, lever: len(tool.tcp.t).max(50.0), seeds: &seeds };
        let o = &self.opts;
        let d = ctx.design;
        let mut table = ctx.table;
        let mut z = z1;
        let mut passes = 0;
        while z > z0 + 1e-6 && passes < 32 {
            passes += 1;
            let mut cover = f64::MAX;
            for region in d.layer(z - 1e-3, 0.0) {
                let ring: Vec<[f64; 2]> = region.contour.pts.iter().map(|p| [p.x_mm(), p.y_mm()]).collect();
                if ring.len() < 3 {
                    continue;
                }
                for arc in arcs(d, &ring, z, std::f64::consts::FRAC_PI_2) {
                    let points: Vec<CutPoint> = arc
                        .iter()
                        .map(|&(on, normal)| {
                            let along = [-normal[1], normal[0], 0.0];
                            let focus = add(on, scale(normal, 0.5 * kerf));
                            let beams = [1.0, -1.0].map(|sg| beams(d, &ctx.substrate, focus, normal, along, sg, z0, z1, kerf, defocus, o.finish, o.beam_clearance));
                            CutPoint { on, normal, focus, beams }
                        })
                        .collect();
                    table = reach.table(&points, table);
                    let part = ctx.part(table);
                    let sign = reach.side(&points, &part);
                    let mut run: Vec<Waypoint> = Vec::new();
                    let mut close = |run: &mut Vec<Waypoint>| {
                        if run.len() >= 2 {
                            let mut path = Vec::with_capacity(run.len() + 2);
                            let (first, last) = (run[0], run[run.len() - 1]);
                            path.push(Waypoint { p: sub(first.p, scale(first.z, o.cut_approach)), on: false, ..first });
                            path.extend(run.iter().copied());
                            path.push(Waypoint { p: sub(last.p, scale(last.z, o.cut_approach)), on: false, ..last });
                            let bead = vec![(0.0, 0.0); path.len()];
                            let turns = reach.turns(&path, table);
                            out.push_back(Task { arm, head, kind: TaskKind::Cut { z0, z1 }, path, bead, hold: true, table, turns });
                        }
                        run.clear();
                    };
                    // Each point's beam, (direction, inclination): the last one while it still
                    // serves (a change turns the head, so the cut breaks there), else the
                    // reachable one that reaches furthest down the band, the arc's direction
                    // before the other (a point the arc's direction cannot reach, its corner
                    // say, the other may).
                    let mut prev: Option<((f64, f64), Q)> = None;
                    for p in &points {
                        let options = p.options(sign);
                        let keep = prev.and_then(|(beam, q)| {
                            let (_, axis, c) = *options.iter().find(|o| o.0 == beam)?;
                            reach.ik(p, axis, &part, &[q]).map(|q| (beam, axis, c, q))
                        });
                        let pick = keep.or_else(|| {
                            let seed: Vec<Q> = prev.map(|p| p.1).into_iter().chain(seeds.iter().copied()).collect();
                            options.iter().find_map(|&(beam, axis, c)| reach.ik(p, axis, &part, &seed).map(|q| (beam, axis, c, q)))
                        });
                        match pick {
                            Some((beam, axis, c, q)) => {
                                if prev.is_some_and(|p| p.0 != beam) {
                                    let last = run.last().copied();
                                    close(&mut run);
                                    run.extend(last.map(|w| Waypoint { z: axis, on: false, ..w }));
                                }
                                cover = cover.min(c);
                                let on = !run.is_empty();
                                run.push(Waypoint { p: p.focus, z: axis, x: Some(p.normal), speed: o.cut_speed, on });
                                prev = Some((beam, q));
                            }
                            None => {
                                flags.push((p.on, if options.is_empty() { BEAM_BLOCKED } else { CUT_UNREACHED }));
                                close(&mut run);
                                prev = None;
                            }
                        }
                    }
                    close(&mut run);
                }
            }
            if cover == f64::MAX {
                break;
            }
            z -= cover.max(kerf);
        }
        (out, flags)
    }
}

/// A cut point on a face: where, its outward normal, the beam's focus (half a kerf out), and
/// the beams that leave it either way along the face ([`beams`], +1 then -1).
struct CutPoint {
    on: V3,
    normal: V3,
    focus: V3,
    beams: [Vec<(f64, V3, f64)>; 2],
}

impl CutPoint {
    /// Its beams as ((direction, inclination), axis, reach): those along `sign` first.
    fn options(&self, sign: f64) -> Vec<((f64, f64), V3, f64)> {
        let order = if sign > 0.0 { [(0, 1.0), (1, -1.0)] } else { [(1, -1.0), (0, 1.0)] };
        order.into_iter().flat_map(|(k, sg)| self.beams[k].iter().map(move |&(deg, axis, c)| ((sg, deg), axis, c))).collect()
    }
}

/// The cutting arm's reach over a cut's points.
struct Reach<'a> {
    arm: &'a ArmModel,
    ctx: &'a Ctx<'a>,
    /// How far the head's body sits back from its focus (mm).
    lever: f64,
    seeds: &'a [Q],
}

impl Reach<'_> {
    /// A pose holding beam `axis` on `p` with the part frame at `part`, from the first of
    /// `seeds` that reaches it clear of a singularity, the arm and its heads clear of the part.
    fn ik(&self, p: &CutPoint, axis: V3, part: &Pose, seeds: &[Q]) -> Option<Q> {
        let g = IkGoal { p: part.apply(p.focus), z: part.rotate(axis), x: Some(part.rotate(p.normal)) };
        seeds.iter().find_map(|s| ik(self.arm, &g, s, &IK).ok().filter(|q| manipulability(self.arm, q) >= W_MIN && self.ctx.clear(self.arm, q, part)))
    }

    /// The table's turns for `path` cut with the table turning (its tilt `table`'s): each
    /// waypoint's face (its x) toward the arm, where the arm reaches every waypoint there clear
    /// of singularities and the part; none where it does not, or the turn would not change.
    fn turns(&self, path: &[Waypoint], table: [f64; 2]) -> Option<Vec<f64>> {
        self.ctx.positioner?;
        let mut now = table[1];
        let mut seeds: Vec<Q> = self.seeds.to_vec();
        let mut turns = Vec::with_capacity(path.len());
        for w in path {
            now = self.ctx.facing(w.x?, self.arm.base.t, now);
            let part = self.ctx.part([table[0], now]);
            let g = IkGoal { p: part.apply(w.p), z: part.rotate(w.z), x: w.x.map(|x| part.rotate(x)) };
            let q = seeds.iter().find_map(|s| ik(self.arm, &g, s, &IK).ok().filter(|q| manipulability(self.arm, q) >= W_MIN && self.ctx.clear(self.arm, q, &part)))?;
            // On from the last pose first: the arm follows the path through.
            seeds.insert(0, q);
            seeds.truncate(self.seeds.len() + 1);
            turns.push(now);
        }
        turns.iter().any(|t| (t - turns[0]).abs() > 1e-6).then_some(turns)
    }

    /// Which way along an arc the beams run, its middle's: the head's body on the side nearer
    /// the arm's base.
    fn side(&self, points: &[CutPoint], part: &Pose) -> f64 {
        let base = part.inv().apply(self.arm.base.t);
        let p = &points[points.len() / 2];
        let tangent = [-p.normal[1], p.normal[0], 0.0];
        let side = |s: f64| dist(sub(p.on, scale(tangent, s * self.lever)), base);
        if side(1.0) <= side(-1.0) {
            1.0
        } else {
            -1.0
        }
    }

    /// Where the positioner stands for an arc: its middle turned to face the arm (the wrist
    /// works round the same pose on every arc), at the tilt whose beams at the arc's ends and
    /// middle serve the most of them, then reach furthest down the band (the shallowest of
    /// those three); of equals the least tilt. As it stands without a positioner.
    fn table(&self, points: &[CutPoint], now: [f64; 2]) -> [f64; 2] {
        if self.ctx.positioner.is_none() {
            return now;
        }
        let mid = &points[points.len() / 2];
        let turn = self.ctx.facing(mid.normal, self.arm.base.t, now[1]);
        let probes = [&points[0], mid, &points[points.len() - 1]];
        let score = |table: [f64; 2]| -> (usize, f64) {
            let part = self.ctx.part(table);
            let sign = self.side(points, &part);
            let (mut served, mut reach) = (0, f64::MAX);
            for p in probes {
                if let Some(c) = p.options(sign).iter().find(|o| self.ik(p, o.1, &part, self.seeds).is_some()).map(|o| o.2) {
                    served += 1;
                    reach = reach.min(c);
                }
            }
            (served, if served == 0 { 0.0 } else { reach })
        };
        let mut best: Option<([f64; 2], (usize, f64))> = None;
        for tilt in self.ctx.tilts() {
            let t = [tilt, turn];
            let s = score(t);
            if best.is_none_or(|b| s.0 > b.1 .0 || (s.0 == b.1 .0 && s.1 > b.1 .1 + 1e-9)) {
                best = Some((t, s));
            }
        }
        best.expect("a tilt to try").0
    }
}

/// A closed ring walked from its first point round to `gap` (mm, along it) short of it.
fn short_of_start(ring: &[[f64; 2]], gap: f64) -> Vec<[f64; 2]> {
    let mut run: Vec<[f64; 2]> = ring.iter().chain(&ring[..1]).copied().collect();
    let mut left = gap;
    while run.len() > 2 {
        let n = run.len();
        let (a, b) = (run[n - 2], run[n - 1]);
        let l = (b[0] - a[0]).hypot(b[1] - a[1]);
        if l > left {
            let t = 1.0 - left / l;
            run[n - 1] = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            break;
        }
        left -= l;
        run.pop();
    }
    run
}

/// A closed contour at height `z` in arcs over which the face's normal turns at most `turn`,
/// each with its points and their outward normals; neighbours share their end point.
fn arcs(d: &Design, ring: &[[f64; 2]], z: f64, turn: f64) -> Vec<Vec<(V3, V3)>> {
    let pts: Vec<(V3, V3)> = ring
        .iter()
        .map(|q| {
            let on = [q[0], q[1], z];
            let n = d.normal(on);
            (on, norm([n[0], n[1], 0.0]))
        })
        .collect();
    let n = pts.len();
    let mut out = Vec::new();
    let mut cur = vec![pts[0]];
    for k in 1..=n {
        let p = pts[k % n];
        if dot(cur[0].1, p.1) < turn.cos() {
            let last = cur[cur.len() - 1];
            out.push(std::mem::replace(&mut cur, vec![last]));
        }
        cur.push(p);
    }
    out.push(cur);
    out
}

/// The beams for a cut focused at `focus` on a face with outward `normal`, along `sign` ×
/// `along` in the tangent plane inclined down from horizontal, that pass finished material
/// below `z0`, the design and the substrate by `clear` for `defocus` mm past the focus: each
/// inclination (degrees), axis and reach down the band (mm of z: as far as the kerf's inner
/// edge stays within `finish` of the design), the furthest reaching first.
#[allow(clippy::too_many_arguments)]
pub fn beams(d: &Design, substrate: &Substrate, focus: V3, normal: V3, along: V3, sign: f64, z0: f64, z1: f64, kerf: f64, defocus: f64, finish: f64, clear: f64) -> Vec<(f64, V3, f64)> {
    let up = cross(normal, along);
    let up = if up[2] < 0.0 { scale(up, -1.0) } else { up };
    let mut out = Vec::new();
    for deg in [0.0f64, 15.0, 30.0, 45.0, 60.0, 75.0] {
        let b = deg.to_radians();
        let axis = norm(add(scale(along, sign * b.cos()), scale(up, -b.sin())));
        let mut ok = true;
        let mut reach = 0.0f64;
        let mut s = 0.0;
        let step = 0.1;
        while s <= defocus {
            let p = add(focus, scale(axis, s));
            let dd = d.distance(p);
            if substrate.distance(p) < clear {
                ok = false;
                break;
            }
            let in_band = p[2] >= z0 - 1e-9 && p[2] <= z1 + 1e-9;
            if dd < 0.5 * kerf - 1e-3 {
                // Into the design: never.
                ok = false;
                break;
            }
            if !in_band && p[2] < z0 && dd < 0.5 * kerf + clear {
                // Grazing the finished face below.
                ok = false;
                break;
            }
            if in_band && dd <= 0.5 * kerf + finish {
                reach = reach.max(focus[2] - p[2]);
            }
            s += step;
        }
        if ok {
            out.push((deg, axis, reach.max(kerf)));
        }
    }
    // The furthest reaching first; of equals, the flatter.
    out.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.total_cmp(&b.0)));
    out
}

impl Route for DedTrim {
    fn label(&self) -> &'static str {
        "Deposit and trim"
    }

    fn take_flags(&mut self) -> Vec<(V3, &'static str)> {
        std::mem::take(&mut self.flags)
    }

    fn next(&mut self, ctx: &Ctx) -> Option<Task> {
        loop {
            let o = self.opts;
            let block_end = |l: usize| ((l + 1) % o.layers_per_cut == 0) || l + 1 == self.layers;
            let (task, after) = match self.step {
                Step::Deposit(l) => (self.deposit(ctx, l), if block_end(l) { Step::ScanDeposit(l) } else { Step::Deposit(l + 1) }),
                Step::ScanDeposit(l) => (self.scan(ctx, (l + 1) as f64 * o.layer), if o.trim { Step::Cut(l) } else if l + 1 < self.layers { Step::Deposit(l + 1) } else { Step::Done }),
                Step::Cut(l) => {
                    if !self.planned {
                        let first = l - l % o.layers_per_cut;
                        let z0 = if first == 0 { 0.5 * o.layer } else { first as f64 * o.layer - 0.2 };
                        let (pieces, flags) = self.cut(ctx, z0, (l + 1) as f64 * o.layer);
                        self.pieces = pieces;
                        self.flags.extend(flags);
                        self.planned = true;
                    }
                    match self.pieces.pop_front() {
                        Some(t) => return Some(t),
                        None => {
                            self.planned = false;
                            (None, Step::ScanCut(l))
                        }
                    }
                }
                Step::ScanCut(l) => (self.scan(ctx, (l + 1) as f64 * o.layer), if l + 1 < self.layers { Step::Deposit(l + 1) } else { Step::Done }),
                Step::Done => return None,
            };
            self.step = after;
            if task.is_some() {
                return task;
            }
        }
    }
}
