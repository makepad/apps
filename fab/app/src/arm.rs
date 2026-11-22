//! The Arm workspace's lab (experimental). A job from `fab_arm_lab` steps through the planner
//! and the virtual cell in chunks on the app's pool while the workspace shows and the run is
//! on: each chunk covers the cell time the speed asks for since the last, and its end becomes
//! the cell view's scene and the panel's figures. Away from the workspace the run waits. The
//! checks run on the pool one after another. A plastic job is sliced by Fab's own planner for
//! the arm's extruder, timed by the planner's dry run, and set against Fab's own estimate for
//! the same part on the built-in H2D.

use fab::api::*;
use fab_arm_lab::checks::{self, Check};
use fab_arm_lab::compare::{self, Group, WORK};
use fab_arm_lab::fdm::{plan, printer_time, FdmCell, Part, PrinterTime};
use fab_arm_lab::lab::{Cell, Job, Session, Setup};
use fab_arm_lab::printer::{h2d, H2D_LABEL};
use fab_arm_lab::realtime::{CellLoop, RtArmed, RtPath};
use fab_robot::safety::Mode;
use fab_robot::presets::{FDM_ARMS, HEADS};
use fab_arms::planner::{Frame, LoopMode};
use fab_arms::stats::{deviation, Deviation, Faces};
use fab_cell_sim::Errors;
use fab_robot::arm::{ArmId, DOF};
use fab_robot::collide::capsules;
use fab_robot::dynamics::{payload_margin, torque_use, Wrench};
use fab_robot::io::CellLink;
use fab_robot::kin::frames;
use fab_robot::math::{dist, Pose, V3};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle};
use makepad_widgets::*;
use std::collections::VecDeque;
use std::sync::Arc;

/// The most cell time one chunk covers (s).
const CHUNK_MAX: f64 = 0.25;
/// How often the part's surface is sampled again (cell s).
const SURFACE_EVERY: f64 = 0.5;
/// Recorded frames (25 a second) a trail keeps.
const TRAIL: usize = 100;
/// The seed every run uses: the checks' own.
const SEED: u64 = 11;
const MAX_TIME: f64 = 600.0;
/// A plastic print may run the virtual cell this long (the arm's whole time is the dry run's).
const PLASTIC_MAX_TIME: f64 = 4.0 * 3600.0;

const _: () = assert!(checks::PHYSICS.len() + checks::CELL.len() <= fab::ui::arm::CHECK_ROWS, "more checks than the panel has rows");

fn f3(p: V3) -> [f32; 3] {
    [p[0] as f32, p[1] as f32, p[2] as f32]
}

/// What stays put through a run: what the part stands on, the design and its box (part frame:
/// a positioner moves them), what the arms reach (cell frame).
#[derive(Clone)]
struct Statics {
    stand: Vec<Vec<V3>>,
    design: Vec<Vec<V3>>,
    cell: ([f32; 3], [f32; 3]),
    part: (V3, V3),
}

fn circle(c: V3, r: f64) -> Vec<V3> {
    (0..48).map(|k| {
        let a = k as f64 / 48.0 * std::f64::consts::TAU;
        [c[0] + r * a.cos(), c[1] + r * a.sin(), c[2]]
    }).collect()
}

fn statics(s: &Session) -> Statics {
    let info = s.sim.info();
    let d = s.planner.design();
    // The plate's top edge, and the stub on it.
    let s = &info.substrate;
    let ([hx, hy], z) = (s.half, s.plate_top());
    let mut stand = vec![vec![[-hx, -hy, z], [hx, -hy, z], [hx, hy, z], [-hx, hy, z]]];
    if let Some(st) = s.stub {
        stand.push(circle([0.0, 0.0, 0.0], st.radius));
        stand.push(circle([0.0, 0.0, -st.height], st.radius));
    }
    let mut design = Vec::new();
    let (z0, z1) = (d.lo[2].max(0.0), d.hi[2]);
    for z in [z0 + 0.02, 0.5 * (z0 + z1), z1 - 0.02] {
        for region in d.layer(z, 0.0) {
            for poly in std::iter::once(&region.contour).chain(region.holes.iter()) {
                design.push(poly.pts.iter().map(|q| [q.x_mm(), q.y_mm(), z]).collect());
            }
        }
    }
    // Where the arms work: round their bases and up from them, within most of their reach.
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for a in &info.arms {
        let r = a.reach();
        let (out, up) = (0.6 * r, 0.75 * r);
        for k in 0..3 {
            lo[k] = lo[k].min(a.base.t[k] - if k == 2 { 0.0 } else { out });
            hi[k] = hi[k].max(a.base.t[k] + if k == 2 { up } else { out });
        }
    }
    Statics { stand, design, cell: (f3(lo), f3(hi)), part: (d.lo, d.hi) }
}

/// What the scene shows of the part, in the cell frame as it stands now.
#[derive(Clone, Default)]
struct Shown {
    part: Option<Pose>,
    stand: Arc<Vec<Vec<[f32; 3]>>>,
    design: Arc<Vec<Vec<[f32; 3]>>>,
    surface: Arc<Vec<([f32; 3], f32)>>,
    part_box: ([f32; 3], [f32; 3]),
}

impl Shown {
    /// The statics and the sampled surface (part frame) with the part frame at `part`.
    fn at(st: &Statics, surface: &[(V3, f32)], part: Pose) -> Shown {
        let lines = |ls: &[Vec<V3>]| Arc::new(ls.iter().map(|l| l.iter().map(|p| f3(part.apply(*p))).collect()).collect());
        let (lo, hi) = st.part;
        let m = 1.0;
        let (mut plo, mut phi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for k in 0..8 {
            let c = f3(part.apply([[lo[0], hi[0]][k & 1], [lo[1], hi[1]][(k >> 1) & 1], [lo[2], hi[2]][k >> 2]]));
            for j in 0..3 {
                plo[j] = plo[j].min(c[j] - m);
                phi[j] = phi[j].max(c[j] + m);
            }
        }
        Shown {
            part: Some(part),
            stand: lines(&st.stand),
            design: lines(&st.design),
            surface: Arc::new(surface.iter().map(|(p, d)| (f3(part.apply(*p)), *d)).collect()),
            part_box: (plo, phi),
        }
    }
}

/// What a chunk brings back.
struct Chunk {
    session: Box<Session>,
    /// A fresh plastic job's times.
    times: Option<Arc<Times>>,
    statics: Option<Statics>,
    ended: bool,
    time: f64,
    arms: Vec<ArmShape>,
    horizon: Vec<[f32; 3]>,
    frames: Vec<Frame>,
    /// The part frame in the cell as the cell truly has it at the chunk's end.
    part: Pose,
    /// The surface sampled (part frame), when it was.
    surface: Option<(Vec<(V3, f32)>, Faces)>,
    stats: Vec<(String, String)>,
    events: Vec<String>,
    end: Option<String>,
}

fn arm_name(i: usize) -> String {
    match i {
        0 => "W".into(),
        1 => "C".into(),
        _ => format!("{i}"),
    }
}

/// Each arm now: where it truly is, its setpoint's rigid pose, its links' masses.
fn shapes(s: &Session) -> Vec<ArmShape> {
    (0..s.sim.info().arms.len()).map(|i| {
        let truth = &s.sim.truth_arms()[i];
        let now = s.planner.arm_now(i);
        let tf = s.sim.true_frames(ArmId(i as u8));
        let sf = frames(truth, &now.q);
        let caps = |f: &fab_robot::kin::Frames| capsules(truth, f).iter().map(|c| (f3(c.a), f3(c.b), c.r as f32)).collect();
        ArmShape {
            name: arm_name(i),
            links: caps(&tf),
            sent: caps(&sf),
            masses: (0..DOF).map(|k| {
                let b = &truth.bodies[k];
                (f3(tf.link[k].apply(b.com)), f3(sf.link[k].apply(b.com)), b.mass as f32)
            }).collect(),
            trail: Vec::new(),
            doing: now.doing.into(),
            on: now.on,
            tcp: f3(tf.tcp.t),
            axis: f3(tf.tcp.z()),
            target: f3(now.target),
        }
    }).collect()
}

fn joined(per_arm: impl Iterator<Item = String>) -> String {
    per_arm.enumerate().map(|(i, v)| format!("{} {v}", arm_name(i))).collect::<Vec<_>>().join(", ")
}

fn deviation_line(d: &Deviation) -> String {
    if d.points == 0 { "nothing there yet".into() } else { format!("p95 {:.2} mm, worst {:.2} mm", d.p95, d.max) }
}

/// A plastic job's times: the arm's (the planner's dry run of the whole print) and Fab's own
/// estimate for the H2D.
struct Times {
    arm: f64,
    printing: f64,
    layers: usize,
    printer: Result<PrinterTime, String>,
}

fn clock(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, sec) => format!("{sec} s"),
        (0, m, sec) => format!("{m} min {sec:02} s"),
        (h, m, _) => format!("{h} h {m:02} min"),
    }
}

/// The plastic job's time rows: the arm's whole print, the printer's, and the ratio.
fn time_rows(t: &Times) -> Vec<(String, String)> {
    let mut rows = vec![("Job time, arm".into(), format!("{} ({} layers, {} printing; its motion limits, setpoint ticks, travels)", clock(t.arm), t.layers, clock(t.printing)))];
    match &t.printer {
        Ok(p) => {
            rows.push(("On the H2D".into(), format!("{} printing, {} with its start ({H2D_LABEL}; Fab's estimate)", clock(p.model), clock(p.total))));
            rows.push(("Arm : H2D".into(), format!("{:.1}× the H2D's time", t.arm / p.model.max(1e-9))));
        }
        Err(e) => rows.push(("On the H2D".into(), format!("no estimate: {e}"))),
    }
    rows
}

/// The run's figures, as the panel lists them.
fn stats(s: &Session, faces: Option<&Faces>, field: ArmField, times: Option<&Times>) -> Vec<(String, String)> {
    let r = &s.planner.report;
    let log = s.sim.log();
    let info = s.sim.info();
    let n = info.arms.len();
    let time = s.time().max(1e-9);
    let mut rows = vec![
        ("Cell time".into(), format!("{:.1} s", s.time())),
        ("Tasks".into(), format!("{} (deposit {:.1} s, scan {:.1} s, cut {:.1} s){}", r.tasks, r.deposit_time, r.scan_time, r.cut_time, compare::leaned(r).map_or_else(String::new, |l| format!("; leaned {l}")))),
        ("Arms busy".into(), joined(r.busy.iter().map(|b| format!("{:.0} %", 100.0 * b / time)))),
        ("TCP off its path".into(), joined((0..n).map(|i| {
            let now = s.planner.arm_now(i);
            match now.doing {
                "deposit" | "scan" | "cut" | "repair" => format!("{:.2} mm", dist(s.sim.true_frames(ArmId(i as u8)).tcp.t, now.target)),
                other => other.to_string(),
            }
        }))),
        ("Deposited".into(), format!("{:.0} mm³ in {} beads, {} unfused", log.deposited, log.beads.len(), log.lack_of_fusion)),
    ];
    match times {
        Some(t) => {
            rows.extend(time_rows(t));
            rows.push(("Ooze".into(), format!("{} pieces laid with the drive stopped", log.strings)));
        }
        None => rows.push(("Cut".into(), format!("{:.0} mm, {} plate hits", log.cut_length, log.plate_hits))),
    }
    if info.positioner.is_some() {
        let [tilt, turn] = s.planner.table();
        rows.push(("Positioner".into(), format!("{} moves, {:.1} s, turning under {} cuts; tilt {:.0}°, turn {:.0}°", r.table_moves, r.table_time, r.coordinated, tilt.to_degrees(), turn.to_degrees())));
    }
    if let Some(f) = faces {
        let which = field.label().to_lowercase();
        rows.push((format!("Outer faces, {which}"), deviation_line(&f.outer)));
        rows.push((format!("Tops, {which}"), deviation_line(&f.top)));
    }
    let gravity = info.gravity;
    let margins = (0..n).map(|i| {
        let truth = &s.sim.truth_arms()[i];
        let f = s.sim.true_frames(ArmId(i as u8));
        (payload_margin(truth, &f, gravity, &Wrench::NONE), torque_use(truth, &f, gravity, &Wrench::NONE).iter().fold(0.0f64, |m, u| m.max(*u)))
    }).collect::<Vec<_>>();
    rows.push(("Payload margin".into(), joined(margins.iter().map(|m| format!("{:.1} kg", m.0)))));
    rows.push(("Holding torque".into(), joined(margins.iter().map(|m| format!("{:.0} % of rated", 100.0 * m.1)))));
    // The drives' heat: the run's RMS torque against the continuous rating.
    let rms = r.over_rating_line().unwrap_or_else(|| {
        joined(r.rms_use.iter().map(|u| {
            let (j, w) = u.iter().enumerate().fold((0, 0.0f64), |b, (j, u)| if *u > b.1 { (j, *u) } else { b });
            format!("{:.0} % of rated (J{})", 100.0 * w, j + 1)
        }))
    });
    rows.push(("Torque, RMS".into(), rms));
    // Near a singularity first (the park, a kind of task), then the points left undone.
    let undone = r.flags.first().map(|(_, why)| format!("{}: {why}", r.flags.len()));
    let flags = match (r.low_line(), undone) {
        (None, None) => "none".to_string(),
        (Some(l), None) | (None, Some(l)) => l,
        (Some(l), Some(u)) => format!("{l}; {u}"),
    };
    rows.push(("Flags".into(), flags));
    rows.push(("Setpoints".into(), format!("{} dropped, {} out of reach, {} ms at a torque limit", log.dropped, r.unreachable, log.saturated)));
    rows.push(("Faults".into(), if r.faults.is_empty() { "none".into() } else { r.faults.join(", ") }));
    assert!(rows.len() <= fab::ui::arm::STAT_ROWS, "more figures than the panel has rows");
    rows
}

/// The real-time path is compiled: the panel's line follows.
extern "C" fn rt_wake() {
    SignalToUI::set_ui_signal();
}

/// The comparison table as the tab shows it: each row's figures as text, its hint, its sheet.
fn compare_view(t: &compare::Table, key: ArmCompareKey) -> ArmCompare {
    let title = format!("{}, against the {H2D_LABEL}", t.part.label());
    let mut last = None;
    let rows = t
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let group = if last == Some(r.group) {
                String::new()
            } else {
                match r.group {
                    Group::Printer => "Reference".to_string(),
                    Group::Plastic => format!("Plastic: {}", t.head),
                    Group::Metal => "Metal: the part laid in steel".to_string(),
                }
            };
            last = Some(r.group);
            let ratio = t.ratio(r);
            let (time, limit, mut hint) = match &r.time {
                Ok(time) => {
                    let limit = r.limit().map_or_else(|| "none found".to_string(), |(l, saves)| format!("{} ({:.0} %)", l.label(), 100.0 * saves / time.max(1e-9)));
                    let mut hint = String::from("What lifting each limit would save:");
                    for (l, saves) in &r.weighed {
                        hint.push_str(&format!("\n• {}: {} ({:.0} % of {})", l.label(), clock(*saves), 100.0 * saves / time.max(1e-9), clock(*time)));
                    }
                    (clock(*time), limit, hint)
                }
                Err(e) => (e.clone(), String::new(), String::new()),
            };
            for n in &r.notes {
                if !hint.is_empty() {
                    hint.push('\n');
                }
                hint.push_str(n);
            }
            ArmCompareRow {
                group,
                name: r.name.clone(),
                time,
                ratio: match (i, ratio) {
                    (0, _) => "1×".into(),
                    (_, Some(x)) => format!("{x:.2}×"),
                    (_, None) => String::new(),
                },
                faster: ratio.is_some_and(|x| x < 1.0),
                mode: format!("{:.1} Hz", r.mode_hz),
                limit,
                hint,
                sheet: r.sheet.iter().map(|(k, v, tag)| (k.clone(), v.clone(), tag.letter().to_string())).collect(),
            }
        })
        .collect();
    ArmCompare { key, title, rows }
}

/// A run to set up: a metal job, or a plastic part.
#[derive(Clone)]
enum Make {
    Metal(Setup, Job),
    Plastic(Setup, Part),
}

/// A fresh plastic session: Fab's plan of the part for the arm's extruder (the H2D's process),
/// the dry run's time of the whole print, and Fab's estimate for the H2D (on the pool).
fn plastic(setup: Setup, part: Part) -> Result<(Session, Times), String> {
    let Cell::Fdm(cell) = setup.cell else { return Err("not a plastic cell".into()) };
    let js = h2d();
    let design = part.design();
    let p = plan(&js, &design, &cell.extruder())?;
    let mut dry = Session::fdm(setup, part, p.clone());
    let arm = dry.dry_run();
    let r = &dry.planner.report;
    let printer = printer_time(&js, &design);
    let times = Times { arm, printing: r.deposit_time, layers: r.tasks, printer };
    Ok((Session::fdm(setup, part, p), times))
}

/// Step `session` (or a new one from `make`) `advance` cell seconds on (on the cell's loop
/// thread when the run is armed, else here on the pool), and gather what the views show; the
/// surface again when `sample` or at the end (else the figures keep `last`).
fn chunk(session: Option<Box<Session>>, make: Option<Make>, times: Option<Arc<Times>>, advance: f64, sample: bool, last: Option<Faces>, field: ArmField, cell_loop: Option<CellLoop>) -> Result<Chunk, String> {
    let fresh = session.is_none();
    let (mut s, times, new_times) = match (session, make) {
        (Some(s), _) => (s, times, None),
        (None, None) => return Err("no job to set up".into()),
        (None, Some(Make::Metal(setup, job))) => (Box::new(Session::job(setup, job)), None, None),
        (None, Some(Make::Plastic(setup, part))) => {
            let (s, t) = plastic(setup, part)?;
            let t = Arc::new(t);
            (Box::new(s), Some(t.clone()), Some(t))
        }
    };
    let statics = fresh.then(|| statics(&s));
    let until = s.time() + advance;
    let ended = match (advance > 0.0, cell_loop) {
        (false, _) => s.planner.finished(),
        (true, None) => s.run_until(until),
        (true, Some(l)) => {
            let (back, ended, _) = l.run(s, until)?;
            s = back;
            ended
        }
    };
    let frames = std::mem::take(&mut s.planner.frames);
    let part = s.sim.true_part_frame();
    let horizon = frames.last().map(|f| f.horizon.iter().map(|p| f3(part.apply(*p))).collect()).unwrap_or_default();
    let surface = (sample || fresh || ended).then(|| {
        let f = match field {
            ArmField::Built => s.sim.truth_field(),
            ArmField::Seen => s.planner.field(),
        };
        let mut pts = Vec::new();
        f.surface_points(&mut pts);
        let d = s.planner.design();
        let points = pts.iter().filter_map(|(p, _)| {
            let off = d.distance(*p);
            (off.abs() < 3.0).then_some((*p, off as f32))
        }).collect();
        (points, deviation(f, d, 0.3, 3.0))
    });
    let faces = surface.as_ref().map(|s| s.1).or(last);
    let stats = stats(&s, faces.as_ref(), field, times.as_deref());
    let r = &s.planner.report;
    let events = r.events.iter().rev().take(6).rev().map(|(t, e)| format!("{t:6.1} s  {e}")).collect();
    let end = ended.then(|| {
        if !r.faults.is_empty() {
            format!("Stopped by a fault at {:.1} s", s.time())
        } else if r.done {
            format!("Done in {:.1} s", s.time())
        } else {
            format!("Stopped at the {MAX_TIME:.0} s limit")
        }
    });
    let arms = shapes(&s);
    let time = s.time();
    Ok(Chunk { session: s, times: new_times, statics, ended, time, arms, horizon, frames, part, surface, stats, events, end })
}

#[derive(Default)]
pub struct ArmLab {
    /// The run between chunks, and the chunk under way.
    session: Option<Box<Session>>,
    chunk: Option<TaskHandle<Result<Chunk, String>>>,
    /// The plastic job's times, once its first chunk has them.
    times: Option<Arc<Times>>,
    statics: Option<Statics>,
    /// The surface as last sampled (part frame), and the part as the scene shows it.
    surface: Vec<(V3, f32)>,
    shown: Shown,
    faces: Option<Faces>,
    surface_at: f64,
    /// The field to sample changed: sample on the next chunk.
    resample: bool,
    /// Where each arm's tracker saw its TCP, newest last.
    trails: Vec<VecDeque<[f32; 3]>>,
    /// The run was set up anew while a chunk was under way: drop what it brings.
    stale: bool,
    /// When the last chunk went out (wall s).
    sent_at: f64,
    check: Option<(usize, f64, TaskHandle<Check>)>,
    /// The comparison under way: its key, when it started (wall s), its pieces running and
    /// done.
    compare: Option<(ArmCompareKey, f64)>,
    compare_running: Vec<TaskHandle<compare::Done>>,
    compare_done: Vec<compare::Done>,
    /// The cell loop marked as real-time code once the workspace first shows (compiled ahead
    /// under the JIT), and its guard while a dry run or a real build runs.
    rt: Option<RtPath>,
    armed: Option<RtArmed>,
    frame: NextFrame,
    /// The run could not be set up: no new one until the panel changes (no retry on every event).
    failed: bool,
}

/// The Arm workspace's tabs: what the lab's state draws.
pub const TABS: &[&[LiveId]] = &[ids!(tab_arm), ids!(tab_arm_cell), ids!(tab_arm_compare)];

impl ArmLab {
    fn setup(state: &AppState) -> Make {
        let a = &state.arm;
        let mode = match a.mode {
            ArmLoop::Rigid => LoopMode::Rigid,
            ArmLoop::Feedforward => LoopMode::Feedforward,
            ArmLoop::Live => LoopMode::Live,
        };
        let errors = match a.errors {
            ArmErrors::Exact => Errors::NONE,
            ArmErrors::Typical => Errors::TYPICAL,
        };
        if a.plastic {
            let cell = FdmCell { arm: a.cell.min(FDM_ARMS.len() - 1) as u8, head: a.head.min(HEADS.len() - 1) as u8, nozzle: (a.nozzle * 10.0).round() as u8 };
            let setup = Setup { cell: Cell::Fdm(cell), mode, errors, seed: SEED, max_time: PLASTIC_MAX_TIME };
            return Make::Plastic(setup, Part::ALL[a.job.min(Part::ALL.len() - 1)]);
        }
        let cell = Cell::ALL[a.cell.min(Cell::ALL.len() - 1)];
        Make::Metal(Setup { cell, mode, errors, seed: SEED, max_time: MAX_TIME }, Job::ALL[a.job.min(Job::ALL.len() - 1)])
    }

    /// The cells, jobs and heads the panel offers for metal or plastic, and the chosen head's
    /// nozzles.
    fn offer(state: &mut AppState) {
        let a = &mut state.arm;
        a.parts = Part::ALL.iter().map(|p| p.label().to_string()).collect();
        a.compare_part = a.compare_part.min(a.parts.len() - 1);
        if a.plastic {
            a.cells = FDM_ARMS.iter().map(|c| c.label.to_string()).collect();
            a.jobs = Part::ALL.iter().map(|p| p.label().to_string()).collect();
            a.heads = HEADS.iter().map(|h| h.label.to_string()).collect();
            let h = &HEADS[a.head.min(HEADS.len() - 1)];
            a.nozzles = (h.nozzles[0], h.nozzles[1]);
            a.nozzle = a.nozzle.clamp(a.nozzles.0, a.nozzles.1);
        } else {
            a.cells = Cell::ALL.iter().map(|c| c.label()).collect();
            a.jobs = Job::ALL.iter().map(|j| j.label().to_string()).collect();
            a.heads.clear();
        }
        a.cell = a.cell.min(a.cells.len() - 1);
        a.job = a.job.min(a.jobs.len() - 1);
    }

    /// Start the job over: the next chunk sets it up as the panel now has it.
    fn start_over(&mut self, state: &mut AppState) {
        self.session = None;
        self.times = None;
        self.statics = None;
        self.surface.clear();
        self.shown = Shown::default();
        self.faces = None;
        self.trails.clear();
        self.stale = self.chunk.is_some();
        self.failed = false;
        let a = &mut state.arm;
        a.scene = None;
        a.ended = false;
        a.stats.clear();
        a.events.clear();
        a.status = if a.running { "Starting...".into() } else { "Run starts the job in the virtual cell.".into() };
    }

    /// The comparison tab asks for its table: time every option for the part on the pool, its
    /// pieces side by side (one already under way finishes first; the tab asks again). True when
    /// it started (or failed to).
    fn start_compare(&mut self, cx: &mut Cx, state: &mut AppState) -> bool {
        let a = &mut state.arm;
        let key = a.compare_key();
        if a.compare_asked == Some(key) || self.compare.is_some() {
            return false;
        }
        let part = Part::ALL[key.0.min(Part::ALL.len() - 1)];
        let (head, nozzle) = (key.1.min(HEADS.len() - 1) as u8, key.2);
        a.compare_asked = Some(key);
        self.compare_done.clear();
        for w in WORK {
            match cx.task_pool().submit_named(Class::Visible, "fab arm compare", move || {
                let d = compare::run(w, part, head, nozzle);
                SignalToUI::set_ui_signal();
                d
            }) {
                Ok(h) => self.compare_running.push(h),
                Err(e) => {
                    a.compare_status = format!("Could not start the comparison: {e:?}");
                    self.compare_running.clear();
                    return true;
                }
            }
        }
        self.compare = Some((key, Cx::time_now()));
        a.compare_status = format!("Timing every option for the {}...", part.label().to_lowercase());
        true
    }

    /// Gather the comparison's finished pieces; the table once all are in. True when it changed.
    fn poll_compare(&mut self, state: &mut AppState) -> bool {
        let Some((key, t0)) = self.compare else { return false };
        let mut failed = None;
        let done = &mut self.compare_done;
        self.compare_running.retain_mut(|h| match h.try_take() {
            None => true,
            Some(Ok(d)) => {
                done.push(d);
                false
            }
            Some(Err(e)) => {
                failed = Some(format!("{e:?}"));
                false
            }
        });
        let a = &mut state.arm;
        if let Some(e) = failed {
            self.compare_running.clear();
            self.compare = None;
            a.compare_status = format!("The comparison stopped: {e}");
            return true;
        }
        if !self.compare_running.is_empty() {
            return false;
        }
        self.compare = None;
        let part = Part::ALL[key.0.min(Part::ALL.len() - 1)];
        let table = compare::assemble(part, key.1 as u8, key.2, std::mem::take(&mut self.compare_done));
        a.compare = Some(Arc::new(compare_view(&table, key)));
        a.compare_status = format!("Timed in {:.1} s on the pool.", Cx::time_now() - t0);
        true
    }

    /// A panel's action: true when it changed what the tabs show (the caller redraws them, at
    /// once: never by another action from inside the action handling).
    pub fn action(&mut self, cx: &mut Cx, action: &ArmAction, state: &mut AppState) -> bool {
        let a = &mut state.arm;
        let changed = match action {
            ArmAction::Plastic(p) if *p != a.plastic => {
                a.plastic = *p;
                a.cell = 0;
                a.job = 0;
                Self::offer(state);
                self.start_over(state);
                true
            }
            ArmAction::Head(i) if *i != a.head => {
                a.head = *i;
                Self::offer(state);
                self.start_over(state);
                true
            }
            ArmAction::Nozzle(n) if (*n - a.nozzle).abs() > 1e-9 => {
                a.nozzle = n.clamp(a.nozzles.0, a.nozzles.1);
                self.start_over(state);
                true
            }
            ArmAction::Cell(i) if *i != a.cell => {
                a.cell = *i;
                self.start_over(state);
                true
            }
            ArmAction::Job(i) if *i != a.job => {
                a.job = *i;
                self.start_over(state);
                true
            }
            ArmAction::Mode(m) if *m != a.mode => {
                a.mode = *m;
                self.start_over(state);
                true
            }
            ArmAction::Errors(e) if *e != a.errors => {
                a.errors = *e;
                self.start_over(state);
                true
            }
            ArmAction::Target(t) if t.available() && *t != a.target => {
                a.target = *t;
                true
            }
            ArmAction::Field(f) if *f != a.field => {
                a.field = *f;
                self.resample = true;
                true
            }
            ArmAction::Play => {
                let mode = match a.target {
                    ArmTarget::Sim => Mode::Sim,
                    ArmTarget::DryRun => Mode::DryRun,
                    ArmTarget::Real => Mode::Real,
                };
                match self.rt.as_ref().map_or_else(|| Err("The real-time path is not marked".to_string()), |rt| rt.arm_for(mode)) {
                    Err(e) => a.status = e,
                    Ok(armed) => {
                        self.armed = armed;
                        a.running = true;
                        if a.ended {
                            self.start_over(state);
                        }
                    }
                }
                true
            }
            ArmAction::Pause => {
                a.running = false;
                a.status = match &a.scene {
                    Some(s) => format!("Paused at {:.1} s", s.time),
                    None => "Paused".into(),
                };
                true
            }
            ArmAction::Reset => {
                self.start_over(state);
                true
            }
            ArmAction::Speed(v) => {
                a.speed = *v;
                true
            }
            ArmAction::Flex(f) => {
                a.flex = *f;
                true
            }
            ArmAction::Compare => self.start_compare(cx, state),
            ArmAction::ComparePart(i) if *i != a.compare_part => {
                a.compare_part = *i;
                true
            }
            ArmAction::CompareRow(i) if *i != a.compare_row => {
                a.compare_row = *i;
                true
            }
            ArmAction::RunChecks if self.check.is_none() => {
                a.checks = checks::all().map(|(name, _)| ArmCheckRow { name: name.into(), ..Default::default() }).collect();
                self.next_check(cx, state, 0);
                true
            }
            _ => false,
        };
        if changed {
            self.frame = cx.new_next_frame();
        }
        changed
    }

    fn next_check(&mut self, cx: &mut Cx, state: &mut AppState, i: usize) {
        let Some((_, run)) = checks::all().nth(i) else { return };
        match cx.task_pool().submit_named(Class::Visible, "fab arm check", move || {
            let c = run();
            SignalToUI::set_ui_signal();
            c
        }) {
            Ok(h) => {
                state.arm.checks[i].state = ArmCheckState::Running;
                self.check = Some((i, Cx::time_now(), h));
            }
            Err(e) => state.arm.checks[i].figures = format!("could not start: {e:?}"),
        }
    }

    /// Keep the run going while the workspace shows; call on every event. True when what the
    /// tabs show changed (the caller redraws them, at once: an action here would come back on
    /// the next round of the action handling, and again, a feedback loop).
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState) -> bool {
        if state.arm.cells.is_empty() {
            Self::offer(state);
            state.arm.status = "Run starts the job in the virtual cell.".into();
        }
        let mut changed = false;
        if state.ui.shows(Family::Arm) {
            let rt = self.rt.get_or_insert_with(|| RtPath::mark(Some(rt_wake)));
            let line = rt.line();
            if state.arm.realtime != line {
                state.arm.realtime = line;
                changed = true;
            }
        }
        if !state.arm.running {
            self.armed = None;
        }
        if let Some((i, t0, h)) = &mut self.check {
            if let Some(r) = h.try_take() {
                let (i, ms) = (*i, (Cx::time_now() - *t0) * 1000.0);
                self.check = None;
                let row = &mut state.arm.checks[i];
                row.ms = ms;
                match r {
                    Ok(c) => {
                        row.state = if c.pass { ArmCheckState::Pass } else { ArmCheckState::Fail };
                        row.figures = c.figures;
                    }
                    Err(e) => {
                        row.state = ArmCheckState::Fail;
                        row.figures = format!("it panicked ({e:?})");
                    }
                }
                self.next_check(cx, state, i + 1);
                changed = true;
            }
        }
        if let Some(h) = &mut self.chunk {
            if let Some(r) = h.try_take() {
                self.chunk = None;
                // A run set up anew meanwhile drops what the old one brings.
                let stale = std::mem::take(&mut self.stale);
                match r {
                    Ok(Ok(c)) => {
                        if !stale {
                            self.take(c, state);
                        }
                    }
                    Ok(Err(e)) if !stale => {
                        state.arm.running = false;
                        state.arm.status = format!("Could not set the job up: {e}");
                        self.failed = true;
                    }
                    Ok(Err(_)) => {}
                    Err(e) => {
                        state.arm.running = false;
                        state.arm.status = format!("The cell stopped: {e:?}");
                        self.failed = true;
                    }
                }
                changed = true;
            }
        }
        changed |= self.poll_compare(state);
        let shown = state.ui.shows(Family::Arm);
        let first = shown && state.arm.scene.is_none() && self.session.is_none() && !self.failed;
        if self.chunk.is_none() && shown && (state.arm.running || self.resample || first) {
            self.submit(cx, state);
        }
        if self.frame.is_event(event).is_some() || changed {
            if self.chunk.is_some() || self.check.is_some() || self.compare.is_some() || (shown && state.arm.running) {
                self.frame = cx.new_next_frame();
            }
        }
        changed
    }

    fn submit(&mut self, cx: &mut Cx, state: &mut AppState) {
        let now = Cx::time_now();
        let a = &state.arm;
        let advance = if !a.running {
            0.0
        } else if a.speed <= 0.0 {
            CHUNK_MAX
        } else {
            let wall = if self.sent_at > 0.0 { (now - self.sent_at).clamp(0.0, 0.1) } else { 1.0 / 60.0 };
            (wall * a.speed).min(CHUNK_MAX)
        };
        self.sent_at = now;
        let time = a.scene.as_ref().map_or(0.0, |s| s.time);
        let sample = std::mem::take(&mut self.resample) || time + advance >= self.surface_at + SURFACE_EVERY;
        let session = self.session.take();
        // A new run is set up from the panel as it is.
        let make = session.is_none().then(|| Self::setup(state));
        let (field, last, times) = (a.field, self.faces, self.times.clone());
        // An armed run (a dry run or a real build) drives the cell from its loop thread.
        let cell_loop = self.armed.as_ref().and(self.rt.as_ref()).and_then(|rt| rt.loop_thread());
        if matches!(make, Some(Make::Plastic(..))) {
            state.arm.status = "Slicing the part for the arm's extruder and for the H2D...".into();
        }
        match cx.task_pool().submit_named(Class::Visible, "fab arm lab", move || {
            let c = chunk(session, make, times, advance, sample, last, field, cell_loop);
            SignalToUI::set_ui_signal();
            c
        }) {
            Ok(h) => {
                self.chunk = Some(h);
                self.frame = cx.new_next_frame();
            }
            Err(e) => {
                state.arm.running = false;
                state.arm.status = format!("Could not run the cell: {e:?}");
            }
        }
    }

    /// A chunk's end: the run back, and what it shows.
    fn take(&mut self, c: Chunk, state: &mut AppState) {
        if let Some(s) = c.statics {
            self.statics = Some(s);
        }
        if let Some(t) = c.times {
            self.times = Some(t);
        }
        let Some(st) = self.statics.as_ref() else { return };
        let sampled = c.surface.is_some();
        if let Some((points, faces)) = c.surface {
            self.surface = points;
            self.faces = Some(faces);
            self.surface_at = c.time;
        }
        // A positioner moves the part: the scene follows it.
        if sampled || self.shown.part != Some(c.part) {
            self.shown = Shown::at(st, &self.surface, c.part);
        }
        let cell = st.cell;
        self.trails.resize(c.arms.len(), VecDeque::new());
        for f in &c.frames {
            for (i, seen) in f.seen.iter().enumerate() {
                if let (Some(t), Some(seen)) = (self.trails.get_mut(i), seen) {
                    t.push_back(f3(*seen));
                    if t.len() > TRAIL {
                        t.pop_front();
                    }
                }
            }
        }
        let mut arms = c.arms;
        for (arm, trail) in arms.iter_mut().zip(&self.trails) {
            arm.trail = trail.iter().copied().collect();
        }
        let a = &mut state.arm;
        a.scene = Some(Arc::new(ArmScene {
            time: c.time,
            arms,
            cell_lo: cell.0,
            cell_hi: cell.1,
            part_lo: self.shown.part_box.0,
            part_hi: self.shown.part_box.1,
            stand: self.shown.stand.clone(),
            design: self.shown.design.clone(),
            surface: self.shown.surface.clone(),
            horizon: c.horizon,
        }));
        a.stats = c.stats;
        a.events = c.events;
        if c.ended {
            a.running = false;
            a.ended = true;
            a.status = c.end.unwrap_or_default();
        } else if a.running {
            a.status = if a.speed <= 0.0 { "Running as fast as it goes".into() } else { format!("Running at {}x", a.speed) };
        }
        self.session = Some(c.session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice settling sends a round of actions while the Arm workspace shows and its run goes
    /// at 1x; with the pool free, each chunk is back before the next round. Rounds go on as the
    /// platform's action handling runs them, while the last round sent any: the lab reports
    /// its change and sends none, so the first round is the last (a redraw sent as an action
    /// came back every round, and the platform stopped it at 100 as a feedback loop).
    #[test]
    fn a_settle_beside_the_running_lab_is_one_round_of_actions() {
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut state = AppState::default();
        state.ui.arm_shown = true;
        state.ui.enter(Workspace::Arm);
        let mut lab = ArmLab::default();
        lab.handle_event(&mut cx, &Event::Startup, &mut state);
        state.arm.running = true;
        let mut actions: ActionsBuf = vec![Box::new(())];
        let (mut rounds, mut changes) = (0, 0);
        while !actions.is_empty() && rounds < 8 {
            rounds += 1;
            // The pool is free: the chunk under way is back first.
            while lab.chunk.as_ref().is_some_and(|h| !h.is_finished()) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let event = Event::Actions(std::mem::take(&mut actions));
            actions = cx.capture_actions(|cx| changes += lab.handle_event(cx, &event, &mut state) as usize);
        }
        assert_eq!(rounds, 1, "the lab sent actions from its poll, round after round");
        assert!(changes == 1 && state.arm.scene.is_some(), "the chunk's scene came back and was reported once ({changes})");
    }
}
