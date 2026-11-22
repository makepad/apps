//! The Mechanics panel's joint check, run only when asked: the design check of every joint (the
//! closest gap, the slicer's layers, the range sweep) on a worker. Its progress goes on the job
//! board, where the panel's busy line shows it: what it is doing in words, a bar of measured
//! progress, the time left at the rate so far, and Cancel. What it finds as it goes is kept after:
//! the marks the view draws (each joint's region, the closest gap, the layers a bead could tack
//! the parts together in, where each turn stops and the range between) and the moving part
//! ghosted at the angle being tested, then at the angles it stops at. When it is done the sim
//! takes the measured ranges.

use crate::mechanics::{apply, mul, M4};
use fab::api::{MechCheckView, MechMark, MechMarkKind};
use fab::model::{ElementId, Scene};
use fab_loose::mesh::PieceMesh;
use fab_model::eval::Evaluated;
use fab_model::mech::Mech;
use makepad_csg_math::Vec3d;
use makepad_csg_mech::geom::{add, cross, dot, len, norm, rotate, scale, sub};
use makepad_csg_mech::sim::JointKind;
use makepad_csg_mech::{Bind, CheckSink, Found, JointCheck, JointGeom, Profile, Tack, Turned, P3};
use makepad_widgets::jobs::{JobHandle, JobId, JobReporter, JobSpec};
use makepad_widgets::makepad_platform::thread::Class;
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The sweep's time, before it starts, as a share of the layer check's: the jewellery box's
/// hinge 0.64, a chain's hinges 0.5 to 1.3, a ball joint 0.5, a slider 1.2 (its own rate takes
/// over once it turns).
const SWEEP_PER_LAYERS: f64 = 0.65;
/// A gap search, a layer's rows and a sweep's angle tell of their progress this often at most
/// (the view and the bar move no faster).
const GAP_EVERY: Duration = Duration::from_millis(20);
/// The ghost of the angle being tested, and of one it stopped at.
const TESTING: [f32; 4] = [0.35, 0.8, 1.0, 1.0];
const STOPPED: [f32; 4] = [0.45, 0.85, 0.5, 1.0];

/// A joint check's phases, in the order they run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Waiting,
    /// A few layers sliced first, to time the layer check by.
    Ahead,
    Gap,
    Layers,
    Sweep,
}

/// How far one joint's check has got, in its time (s since the check began).
#[derive(Clone, Debug, Default)]
struct JointProgress {
    words: String,
    /// When its first step came and its last, if it is done.
    began: Option<f64>,
    ended: Option<f64>,
    phase: Phase,
    /// When the phase began, and its share done by its own measure (the share of the region
    /// searched, of the layers sliced, of the angles turned).
    phase_began: f64,
    share: f64,
    /// The share at the last whole layer of the phase, and a layer's (the rows of the next
    /// count in it).
    base: f64,
    unit: f64,
    /// The layers ahead: (sliced, of, the box's layers, their seconds).
    ahead: (usize, usize, usize, f64),
    /// Each layer ahead's seconds (sliced from the samples of the layer below it, as the
    /// check's layers are), in the order sliced.
    each: Vec<f64>,
}

impl JointProgress {
    /// Seconds a layer takes, by those sliced so far: the layers ahead by their median, as a
    /// layer along a flat face takes many times another.
    fn per_layer(&self, now: f64) -> Option<f64> {
        let (sliced, of, n, _) = self.ahead;
        let el = now - self.phase_began;
        let mut each = self.each.clone();
        each.sort_by(f64::total_cmp);
        // the lower middle: of two, one along a flat face does not set it
        let typical = each.get(each.len().saturating_sub(1) / 2).copied();
        match self.phase {
            // until one is timed: half a pair's time (a layer ahead and the one below it)
            Phase::Ahead if sliced == 0 && self.share > 0.0 => Some(el / (self.share * of as f64) / 2.0),
            // the layers sliced in turn at their own rate (the layers ahead come free, spread
            // among them), counting in its share of the time they should take, the layers ahead's
            // the rest
            Phase::Layers if self.share > 0.0 && sliced > 0 => {
                let typical = typical.unwrap_or(0.0);
                let turn = n.saturating_sub(Self::below(of, n)).max(1) as f64;
                let w = if el + turn * typical > 0.0 { el / (el + turn * typical) } else { 1.0 };
                Some(w * el / (self.share * turn) + (1.0 - w) * typical)
            }
            _ if sliced > 0 => typical,
            _ => None,
        }
    }

    /// The layers sliced ahead: each of `of` with the layer below it, of the box's `n`.
    fn below(of: usize, n: usize) -> usize {
        (2 * of).min(n)
    }

    /// Its time so far and the time it has left (None: nothing to tell it by yet).
    fn times(&self, now: f64) -> Option<(f64, f64)> {
        let began = self.began?;
        if let Some(end) = self.ended {
            return Some((end - began, 0.0));
        }
        let el = now - self.phase_began;
        // the phase's own rate, once it has one
        let rate = |share: f64| (share > 0.0).then(|| el * (1.0 - share) / share);
        // a box with no layers (flat along up) takes them no time; else a layer's time is unknown
        // until a few of its rows are sliced
        let per = match self.per_layer(now) {
            Some(per) => per,
            None if self.ahead.2 == 0 && self.phase != Phase::Ahead => 0.0,
            None => return None,
        };
        let (_, of, n, _) = self.ahead;
        let layers = n.saturating_sub(Self::below(of, n)) as f64 * per;
        let sweep = SWEEP_PER_LAYERS * n as f64 * per;
        let left = match self.phase {
            Phase::Waiting => return None,
            // the rest ahead at the pairs' own rate; the gap search is unknown until it starts:
            // as long as the layers, say
            Phase::Ahead => (1.0 - self.share) * el / self.share + 2.0 * layers + sweep,
            // a gap search's first roots take longest: its rate counts in its share of the time
            // it should take, the forecast the rest
            Phase::Gap => {
                let prior = layers * (1.0 - self.share);
                let w = if el + layers > 0.0 { el / (el + layers) } else { 1.0 };
                w * rate(self.share).unwrap_or(prior) + (1.0 - w) * prior + layers + sweep
            }
            Phase::Layers => layers * (1.0 - self.share) + sweep,
            // a sweep's first angles are its largest steps: its rate counts in its share of the
            // time it should take, the forecast the rest
            Phase::Sweep => {
                let prior = sweep * (1.0 - self.share);
                let w = if el + sweep > 0.0 { el / (el + sweep) } else { 1.0 };
                w * rate(self.share).unwrap_or(prior) + (1.0 - w) * prior
            }
        };
        Some((now - began, left.max(0.0)))
    }
}

/// How far a check has got, from the steps it takes: the bar's share (in the time the joints
/// take: each joint's time so far, of that and the time it has left, each phase at its own
/// rate once it runs and the layers' and sweep's before that by the layers sliced ahead), what
/// it is doing in words and the time left at the rate so far.
pub struct Progress {
    kinds: Vec<&'static str>,
    joints: Vec<JointProgress>,
    started: Instant,
    /// The share told last, and the joints' time so far then.
    told: Mutex<(f64, f64)>,
}

impl Progress {
    pub fn new(mech: &Mech) -> Progress {
        let kinds = mech.plan.joints.iter().map(kind_of).collect::<Vec<_>>();
        Progress { joints: vec![JointProgress::default(); kinds.len()], kinds, started: Instant::now(), told: Mutex::new((0.0, 0.0)) }
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Joint `j` took this step.
    pub fn step(&mut self, j: usize, f: &Found) {
        let now = self.now();
        let kind = self.kinds.get(j).copied().unwrap_or("joint");
        let Some(p) = self.joints.get_mut(j) else { return };
        p.began.get_or_insert(now);
        let enter = |p: &mut JointProgress, phase: Phase| {
            if p.phase != phase {
                p.phase = phase;
                p.phase_began = now;
                p.share = 0.0;
            }
        };
        let (share, words) = match f {
            Found::Ahead { sliced, of, layers, secs } => {
                enter(p, Phase::Ahead);
                if *sliced > p.each.len() {
                    p.each.push(secs - p.ahead.3);
                }
                p.ahead = (*sliced, *of, *layers, *secs);
                (p.base, p.unit) = (*sliced as f64 / (*of).max(1) as f64, 1.0 / (*of).max(1) as f64);
                (p.base, format!("slicing a few layers first: {} of {of}", (sliced + 1).min(*of)))
            }
            Found::Rows { share } => {
                // rows after the gap search are the first layer's: the layers have begun
                if p.phase == Phase::Gap {
                    enter(p, Phase::Layers);
                    (p.base, p.unit) = (0.0, 1.0 / p.ahead.2.max(1) as f64);
                }
                (p.base + p.unit * share, p.words.clone())
            }
            Found::Gap { searched, gap, .. } => {
                enter(p, Phase::Gap);
                let words = if *searched >= 1.0 { closest(*gap) } else { format!("finding the closest gap: {:.0}% searched", searched * 100.0) };
                (*searched, words)
            }
            Found::Layer { k, n, .. } => {
                enter(p, Phase::Layers);
                (p.base, p.unit) = (*k as f64 / (*n).max(1) as f64, 1.0 / (*n).max(1) as f64);
                (p.base, format!("slicing layer {} of {n}", (k + 1).min(*n)))
            }
            Found::Tack(t) => (p.share, format!("a bead over the other part at layer {}", t.layer)),
            Found::Cells { .. } => (p.share, p.words.clone()),
            Found::Turn { turn, turns, angle, max, slide, .. } => {
                enter(p, Phase::Sweep);
                ((*turn as f64 + (angle.abs() / max).min(1.0)) / (*turns).max(1) as f64, format!("sweeping {}: {}", turn_name(kind, *turn), amount(*angle, *slide)))
            }
            Found::Stop(t) => {
                enter(p, Phase::Sweep);
                if t.turn + 1 >= t.turns {
                    p.ended = Some(now);
                }
                let words = match t.contact {
                    Some(_) => format!("{} stops at {}", turn_name(kind, t.turn), amount(t.angle, t.slide)),
                    None => format!("{} {} all {}", turn_name(kind, t.turn), if t.slide { "slides" } else { "turns" }, amount(t.angle, t.slide)),
                };
                ((t.turn + 1) as f64 / t.turns.max(1) as f64, words)
            }
        };
        // a phase's share never goes back (a bisection tests angles below the one that touched)
        p.share = p.share.max(share.min(1.0));
        p.words = words;
    }

    /// The share of the whole check done, 0 to 1: it grows with the joints' time by its share of
    /// the time they have left, so it never goes back, and it moves whenever they work.
    pub fn fraction(&self) -> f64 {
        if self.joints.iter().all(|j| j.ended.is_some()) {
            return 1.0;
        }
        let now = self.now();
        let times: Vec<(f64, f64)> = self.joints.iter().filter_map(|j| j.times(now)).collect();
        let mut told = self.told.lock().unwrap();
        if times.is_empty() {
            return told.0;
        }
        // a joint not started yet takes as long as those started do, on average
        let unknown = self.joints.len() - times.len();
        let mean = times.iter().map(|t| t.0 + t.1).sum::<f64>() / times.len() as f64;
        let done: f64 = times.iter().map(|t| t.0).sum();
        let left = times.iter().map(|t| t.1).sum::<f64>() + unknown as f64 * mean;
        let (share, then) = *told;
        let since = (done - then).max(0.0);
        let share = if since + left > 0.0 { share + (1.0 - share) * since / (since + left) } else { share };
        // or the share the forecast now gives, where that is more: a forecast too long early (a
        // slow layer ahead, a gap search's first roots) is made up once a phase's own rate shows
        let share = if done + left > 0.0 { share.max(done / (done + left)) } else { share };
        *told = (share.min(1.0), done);
        told.0
    }

    /// The seconds left at the rate so far, once there is a rate to go by.
    pub fn eta(&self) -> Option<f64> {
        let (f, t) = (self.fraction(), self.now());
        (f >= 0.02 && t >= 1.0 && f < 1.0).then(|| t * (1.0 - f) / f)
    }

    /// What it is doing: the time left, once there is a rate to go by, then the joint furthest
    /// behind ("about 2 min left · hinge 1 of 2 · sweeping open: +84°"); a line too narrow for
    /// it cuts the joint's words, which its row shows whole.
    pub fn words(&self) -> String {
        let n = self.joints.len();
        let now = self.now();
        let behind = |j: &JointProgress| j.times(now).map_or(0.0, |(el, left)| if el + left > 0.0 { el / (el + left) } else { 0.0 });
        let at = self.joints.iter().enumerate().filter(|(_, j)| j.ended.is_none()).min_by(|a, b| behind(a.1).total_cmp(&behind(b.1)));
        let what = match at {
            Some((j, p)) if !p.words.is_empty() => format!("{} {} of {n} · {}", self.kinds[j], j + 1, p.words),
            Some((j, _)) => format!("{} {} of {n} · starting", self.kinds[j], j + 1),
            None => "finishing".into(),
        };
        match self.eta() {
            Some(s) if s < 60.0 => format!("about {:.0} s left · {what}", s.max(1.0)),
            Some(s) => format!("about {:.0} min left · {what}", (s / 60.0).ceil()),
            None => what,
        }
    }

    /// One joint's words, for its row while the check runs.
    fn joint_words(&self, j: usize) -> Option<&str> {
        self.joints.get(j).map(|p| p.words.as_str()).filter(|w| !w.is_empty())
    }
}

fn kind_of(g: &JointGeom) -> &'static str {
    match g {
        JointGeom::Hinge { .. } => "hinge",
        JointGeom::Ball { .. } => "ball joint",
        JointGeom::Slider(_) => "slider",
        JointGeom::Bearing(_) => "bearing",
    }
}

/// A turn by its place in the sweep: a hinge opens one way and closes the other, a slider slides
/// on and back, a bearing turns forward and back; a ball tilts each way about two axes.
fn turn_name(kind: &str, turn: usize) -> &'static str {
    match (kind, turn) {
        ("hinge", 0) => "open",
        ("hinge", _) => "closed",
        ("slider", 0) => "on",
        ("slider", _) => "back",
        ("bearing", 0) => "forward",
        ("bearing", _) => "back",
        (_, 0) => "tilt 1",
        (_, 1) => "tilt 1 back",
        (_, 2) => "tilt 2",
        _ => "tilt 2 back",
    }
}

/// How far a joint moves: a slide in millimetres, a turn in degrees.
fn amount(x: f64, slide: bool) -> String {
    if slide {
        format!("{x:+.1} mm")
    } else {
        format!("{:+.0}°", x.to_degrees())
    }
}

/// What the check has found of one joint so far (the parts' frame, mm).
#[derive(Clone, Debug)]
pub struct JointFound {
    pub name: String,
    /// The fixed part (the marks ride on it) and the moving one (its ghost turns).
    pub a: usize,
    pub b: usize,
    /// "hinge", "ball joint", "slider" or "bearing".
    pub kind: &'static str,
    pub region: (P3, P3),
    pub pivot: P3,
    pub turns: usize,
    pub started: bool,
    /// The smallest gap so far and where; whether the search is over.
    pub gap: Option<(f64, P3)>,
    pub gap_done: bool,
    pub tacks: Vec<Tack>,
    /// The turn being tested: (turn, axis, angle).
    pub turning: Option<(usize, P3, f64)>,
    pub stops: Vec<Turned>,
    pub check: Option<JointCheck>,
}

/// A mark the view draws: the body it rides on, where (the parts' frame, mm), its words, its kind.
pub type Mark = (usize, P3, String, MechMarkKind);
/// A line the view draws: the body it rides on, its points, its kind.
pub type Line = (usize, Vec<P3>, MechMarkKind);

/// What a check found and said.
pub struct CheckState {
    /// The piece (its instance), the evaluation and the profile it is of.
    pub instance: String,
    pub of: ([u8; 32], Profile),
    pub joints: Vec<JointFound>,
    pub running: bool,
    /// It ended before every joint was checked.
    pub cancelled: bool,
}

impl CheckState {
    pub fn new(mech: &Mech, of: ([u8; 32], Profile)) -> CheckState {
        let joints = mech
            .plan
            .joints
            .iter()
            .zip(&mech.plan.decls)
            .enumerate()
            .map(|(i, (g, d))| {
                let (a, b) = d.parts();
                JointFound {
                    name: mech.joint_ids.get(i).cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("joint {}", i + 1)),
                    a,
                    b,
                    kind: kind_of(g),
                    region: g.region(),
                    pivot: g.pivot().0,
                    turns: if matches!(g, JointGeom::Ball { .. }) { 4 } else { 2 },
                    started: false,
                    gap: None,
                    gap_done: false,
                    tacks: Vec::new(),
                    turning: None,
                    stops: Vec::new(),
                    check: None,
                }
            })
            .collect();
        CheckState { instance: mech.instance.clone(), of, joints, running: true, cancelled: false }
    }

    pub fn apply(&mut self, j: usize, f: &Found) {
        let Some(s) = self.joints.get_mut(j) else { return };
        s.started = true;
        match f {
            Found::Gap { searched, gap, at } => {
                if gap.is_finite() {
                    s.gap = Some((*gap, *at));
                }
                s.gap_done = *searched >= 1.0;
            }
            Found::Ahead { .. } | Found::Rows { .. } | Found::Layer { .. } | Found::Cells { .. } => {}
            Found::Tack(t) => s.tacks.push(*t),
            Found::Turn { turn, axis, angle, .. } => s.turning = Some((*turn, *axis, *angle)),
            Found::Stop(t) => {
                s.turning = None;
                s.stops.push(*t);
            }
        }
    }

    /// The check ended: its answers (a cancelled check leaves out the joints it did not finish).
    pub fn finish(&mut self, checks: Vec<JointCheck>) {
        self.running = false;
        self.cancelled = checks.len() < self.joints.len();
        for c in checks {
            if let Some(s) = self.joints.get_mut(c.joint) {
                // what the view shows is what it measured
                s.gap = Some((c.gap.gap, c.gap.at));
                s.gap_done = true;
                s.tacks = c.layers.tacks.clone();
                s.stops = c.turns.clone();
                s.turning = None;
                s.check = Some(c);
            }
        }
        // a joint it did not finish shows no angle as being tested
        for s in self.joints.iter_mut().filter(|s| s.check.is_none()) {
            s.turning = None;
        }
    }

    /// It is of this piece, evaluation and print settings.
    pub fn is_of(&self, instance: &str, content: [u8; 32], profile: &Profile) -> bool {
        self.instance == instance && self.of.0 == content && self.of.1 == *profile
    }

    /// The checked joints' answers.
    pub fn checks(&self) -> impl Iterator<Item = &JointCheck> {
        self.joints.iter().filter_map(|j| j.check.as_ref())
    }

    /// What it said in a line (empty while it runs).
    pub fn line(&self) -> String {
        let n = self.joints.len();
        let s = if n == 1 { "" } else { "s" };
        if self.running {
            return String::new();
        }
        if self.cancelled {
            return format!("Cancelled: {} of {n} joint{s} checked", self.checks().count());
        }
        let free = self.checks().filter(|c| c.ok()).count();
        let worst = self.checks().map(|c| c.risk).max().map_or("low", |r| r.name());
        format!("{free} of {n} joint{s} free · worst fuse risk {worst}")
    }

    /// Per joint, what the check says of it.
    pub fn rows(&self, progress: &Progress) -> Vec<String> {
        self.joints
            .iter()
            .enumerate()
            .map(|(i, j)| match &j.check {
                Some(c) if c.ok() => {
                    let (lo, hi) = c.effective;
                    let range = if j.kind == "slider" {
                        format!("slides {} to {}", amount(lo, true), amount(hi, true))
                    } else if hi - lo >= 2.0 * std::f64::consts::PI - 1e-6 {
                        "turns all the way round".to_string()
                    } else {
                        format!("turns {} to {}", amount(lo, false), amount(hi, false))
                    };
                    format!("{}: free · fuse risk {} · gap {:.2} mm · {range}", j.name, c.risk.name(), c.gap.gap)
                }
                Some(c) => format!("{}: not free · {}", j.name, c.binds.iter().map(|b| why(b, c)).collect::<Vec<_>>().join("; ")),
                None if self.running => format!("{}: {}", j.name, progress.joint_words(i).unwrap_or("waiting")),
                None => format!("{}: not checked (cancelled)", j.name),
            })
            .collect()
    }

    /// The marks the view draws, each on the body it rides on.
    pub fn marks(&self) -> Vec<Mark> {
        let mut out = Vec::new();
        for j in self.joints.iter().filter(|j| j.started) {
            let binds = j.check.as_ref().map_or(&[][..], |c| &c.binds[..]);
            if let Some((gap, at)) = j.gap {
                let (label, kind) = match binds.iter().find(|b| matches!(b, Bind::Fused { .. } | Bind::Tight { .. })) {
                    Some(Bind::Fused { .. }) => (format!("fused here: {}", gap_words(gap)), MechMarkKind::Bind),
                    Some(_) => (format!("too close: gap {gap:.2} mm"), MechMarkKind::Bind),
                    None if j.gap_done => (closest(gap), MechMarkKind::Gap),
                    None => (format!("gap {gap:.2} mm so far"), MechMarkKind::Gap),
                };
                out.push((j.a, at, label, kind));
            }
            // the tacks that weigh most, and every one that binds
            let mut tacks: Vec<&Tack> = j.tacks.iter().collect();
            tacks.sort_by(|a, b| tack_weight(b).total_cmp(&tack_weight(a)));
            for (k, t) in tacks.iter().enumerate() {
                let binding = binds.iter().any(|b| matches!(b, Bind::Tacks(x) if x == *t));
                if k >= 3 && !binding {
                    continue;
                }
                let label = match t.empty {
                    0 => format!("fused at layer {}", t.layer),
                    1 => format!("tacks at layer {} (1 empty layer)", t.layer),
                    e => format!("may tack at layer {} ({e} empty layers)", t.layer),
                };
                out.push((j.a, t.at, label, if binding { MechMarkKind::Bind } else { MechMarkKind::Tack }));
            }
            // where each turn stops; a hinge's turn free all the way, at its end of the range's arc,
            // a slide's at its end along the way
            let arc = if j.turns == 2 && j.stops.len() == 2 && j.kind != "slider" { range_arc(j.pivot, &j.stops) } else { None };
            for (k, s) in j.stops.iter().enumerate() {
                let (name, by) = (turn_name(j.kind, s.turn), amount(s.angle, s.slide));
                match (s.contact, &arc) {
                    (Some(c), _) => out.push((j.a, c, format!("{name} stops at {by}"), MechMarkKind::Stop)),
                    (None, _) if s.slide => out.push((j.a, slid(j.pivot, s), format!("{name} free to {by}"), MechMarkKind::Stop)),
                    // the arc runs from the second stop to the first
                    (None, Some(arc)) => out.push((j.a, if k == 0 { arc[arc.len() - 1] } else { arc[0] }, format!("{name} free to {by}"), MechMarkKind::Stop)),
                    (None, None) => {}
                }
            }
        }
        out
    }

    /// The lines the view draws: each joint's region (once it is looked at), a hinge's measured
    /// range as an arc through the point that touched, and a slider's along its way.
    pub fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        for j in self.joints.iter().filter(|j| j.started) {
            let (lo, hi) = j.region;
            let corner = |c: usize| [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }];
            for (p, q) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
                out.push((j.a, vec![corner(p), corner(q)], MechMarkKind::Region));
            }
            if j.turns == 2 && j.stops.len() == 2 {
                if j.kind == "slider" {
                    out.push((j.a, vec![slid(j.pivot, &j.stops[1]), slid(j.pivot, &j.stops[0])], MechMarkKind::Stop));
                } else if let Some(arc) = range_arc(j.pivot, &j.stops) {
                    out.push((j.a, arc, MechMarkKind::Stop));
                }
            }
        }
        out
    }

    /// The ghosts to show: per (joint, turn), the angle the moving part is turned to (the one
    /// being tested, or the one it stopped at: true), about the joint's pivot and the turn's
    /// axis. A turn not reached shows none.
    pub fn ghosts(&self) -> Vec<((usize, usize), P3, f64, bool)> {
        let mut out = Vec::new();
        for (i, j) in self.joints.iter().enumerate() {
            for s in &j.stops {
                out.push(((i, s.turn), s.axis, s.angle, true));
            }
            if let Some((turn, axis, angle)) = j.turning {
                out.push(((i, turn), axis, angle, false));
            }
        }
        out
    }

    /// The sim's joints with the measured ranges, from their kinds as the sim has them.
    pub fn limits(&self, kinds: &[JointKind]) -> Vec<(usize, JointKind)> {
        self.checks().filter_map(|c| Some((c.joint, kinds.get(c.joint)?.measured(c.effective)))).collect()
    }
}

/// Where a slide ended: the pivot moved along its way.
fn slid(pivot: P3, s: &Turned) -> P3 {
    add(pivot, scale(norm(s.axis), s.angle))
}

/// How much a tack weighs in the fuse score (as [`makepad_csg_mech::check::risk`] weighs it).
fn tack_weight(t: &Tack) -> f64 {
    [3.0, 1.0, 0.3, 0.05][t.empty.min(3)] * t.area
}

/// A hinge's measured range: an arc about its pin from one stop to the other, through the point
/// that touched (at the hinge's rest, on the moving part).
fn range_arc(pivot: P3, stops: &[Turned]) -> Option<Vec<P3>> {
    let axis = norm(stops[0].axis);
    let (s, c) = stops.iter().find_map(|s| Some((s, s.contact?)))?;
    let rest = rotate(sub(c, pivot), axis, -s.angle);
    let along = scale(axis, dot(rest, axis));
    let off = sub(rest, along);
    if len(off) < 1e-6 {
        return None;
    }
    let (from, to) = (stops[1].angle, stops[0].angle);
    let n = (((to - from).abs().to_degrees() / 5.0).ceil() as usize).max(2);
    Some((0..=n).map(|k| add(pivot, add(along, rotate(off, axis, from + (to - from) * k as f64 / n as f64)))).collect())
}

/// A gap the check found, in words: an overlap's depth is a bound (the gap search stops at the
/// first overlap), so it says at least.
fn gap_words(gap: f64) -> String {
    if gap < 0.0 { format!("overlap at least {:.2} mm", -gap) } else { format!("gap {gap:.2} mm") }
}

/// The closest gap the check found, in words (see [`gap_words`]).
fn closest(gap: f64) -> String {
    if gap < 0.0 { gap_words(gap) } else { format!("closest gap {gap:.2} mm") }
}

/// Why a joint is not free, in words.
fn why(b: &Bind, c: &JointCheck) -> String {
    let at = |p: P3| format!("[{:.1}, {:.1}, {:.1}] mm", p[0], p[1], p[2]);
    match b {
        Bind::Fused { gap, at: p } => format!("fused at {} ({})", at(*p), gap_words(*gap)),
        Bind::Tight { gap, at: p } => format!("too close at {}: {gap:.2} mm, under half its {:.2} mm clearance", at(*p), c.clearance),
        Bind::Tacks(t) if t.empty == 0 => format!("fused at layer {} (beads print straight onto the other part)", t.layer),
        Bind::Tacks(t) => format!("tacks at layer {} ({} empty layer{} between)", t.layer, t.empty, if t.empty == 1 { "" } else { "s" }),
        Bind::Sections { gap, z } => format!("its parts' sections come {gap:.2} mm apart at {z:.1} mm up"),
        Bind::Range { range } if c.kind == "slider" => format!("it slides only {range:.2} mm"),
        Bind::Range { range } => format!("it turns only {:.1}°", range.to_degrees()),
        Bind::Loose { retention } => format!("the pin holds it by only {retention:.2} mm"),
        Bind::Supports => "support prints between the parts".into(),
    }
}

/// A joint's move as a 4x4 (the parts' frame, mm): a slide along `axis` by `amount` mm, or a
/// turn about it through `pivot` by `amount` radians.
pub fn move_matrix(pivot: P3, axis: P3, amount: f64, slide: bool) -> M4 {
    if !slide {
        return turn_matrix(pivot, axis, amount);
    }
    let d = scale(norm(axis), amount);
    [[1.0, 0.0, 0.0, d[0]], [0.0, 1.0, 0.0, d[1]], [0.0, 0.0, 1.0, d[2]], [0.0, 0.0, 0.0, 1.0]]
}

/// The turn about `axis` through `pivot` by `angle` as a 4x4 (the parts' frame, mm).
pub fn turn_matrix(pivot: P3, axis: P3, angle: f64) -> M4 {
    let axis = norm(axis);
    let turned = |p: P3| add(pivot, rotate(sub(p, pivot), axis, angle));
    let o = turned([0.0; 3]);
    let col = |e: P3| sub(turned(e), o);
    let (x, y, z) = (col([1.0, 0.0, 0.0]), col([0.0, 1.0, 0.0]), col([0.0, 0.0, 1.0]));
    [[x[0], y[0], z[0], o[0]], [x[1], y[1], z[1], o[1]], [x[2], y[2], z[2], o[2]], [0.0, 0.0, 0.0, 1.0]]
}

/// What the worker sends the UI.
enum Msg {
    Found(usize, Found),
    Done(Vec<JointCheck>),
}

/// The worker's sink: each step to the progress (and the job board) and to the UI; cancelled by
/// the busy line's Cancel, or by the piece changing.
struct Sink {
    progress: Mutex<Progress>,
    tx: Mutex<Sender<Msg>>,
    job: JobReporter,
    stop: Arc<AtomicBool>,
    /// Per joint, when its gap search last told the UI.
    told: Mutex<Vec<Instant>>,
}

impl CheckSink for Sink {
    fn found(&self, joint: usize, found: Found) {
        // a gap search and a sweep's angle step every few cells and a layer every row of blocks:
        // the UI hears of them every so often, and of a gap search's end
        if matches!(found, Found::Gap { searched, .. } if searched < 1.0) || matches!(found, Found::Rows { .. } | Found::Cells { .. }) {
            let mut told = self.told.lock().unwrap();
            if told.get(joint).is_some_and(|t| t.elapsed() < GAP_EVERY) {
                return;
            }
            if let Some(t) = told.get_mut(joint) {
                *t = Instant::now();
            }
        }
        let (fraction, words) = {
            let mut p = self.progress.lock().unwrap();
            p.step(joint, &found);
            (p.fraction(), p.words())
        };
        self.job.stage(words, None, Some(fraction as f32));
        let _ = self.tx.lock().unwrap().send(Msg::Found(joint, found));
        SignalToUI::set_ui_signal();
    }

    fn cancelled(&self) -> bool {
        self.job.is_cancelled() || self.stop.load(Ordering::Relaxed)
    }
}

/// The ghosts' scene: the plate's, with a copy of each joint's moving part per turn (each a copy
/// of that part's element: see [`crate::plate_scene::build_ghosts`]).
fn ghost_scene(ev: &Evaluated, name: &str, mech: &Mech, refs: &[(String, [[f64; 4]; 3])], slots: &[((usize, usize), String)]) -> Arc<Scene> {
    let mut copies: Vec<(String, String)> = Vec::new();
    let meshes: Vec<(String, PieceMesh)> = slots
        .iter()
        .filter_map(|((j, _), name)| {
            let b = mech.plan.decls.get(*j)?.parts().1;
            let m = mech.finals.get(b)?;
            let (object, to_plate) = refs.get(b)?;
            copies.push((name.clone(), object.clone()));
            Some((name.clone(), piece_mesh(&m.vertices, &m.triangles, &m.corner_normals, &mut (0..m.triangles.len()), to_plate)))
        })
        .collect();
    let mut pieces: Vec<(String, &PieceMesh, [f32; 4])> = meshes.iter().map(|(n, m)| (n.clone(), m, TESTING)).collect();
    // a flexible part shows as its pieces, bent with the parts as the sim bends them
    let riders = crate::mech_flex::riders(mech, refs);
    if let Some(r) = &riders {
        pieces.extend(crate::mech_flex::scene_pieces(ev, &r.pieces));
    }
    crate::plate_scene::build_ghosts(ev, name, &pieces, &copies, riders.as_ref().map_or(&[], |r| &r.spines))
}

/// Some of a part's triangles (`which`; its mesh in the model frame, mm) where the plate has it
/// (plate mm).
pub(crate) fn piece_mesh(vertices: &[Vec3d], triangles: &[[u32; 3]], corner_normals: &[Vec3d], which: &mut dyn Iterator<Item = usize>, to_plate: &[[f64; 4]; 3]) -> PieceMesh {
    let at = |v: P3| -> [f32; 3] { std::array::from_fn(|k| (to_plate[k][0] * v[0] + to_plate[k][1] * v[1] + to_plate[k][2] * v[2] + to_plate[k][3]) as f32) };
    let turn = |v: P3| -> [f32; 3] { norm(std::array::from_fn(|k| to_plate[k][0] * v[0] + to_plate[k][1] * v[1] + to_plate[k][2] * v[2])).map(|c| c as f32) };
    let corners = corner_normals.len() == triangles.len() * 3;
    let mut out = PieceMesh { positions: Vec::new(), normals: Vec::new(), indices: Vec::new() };
    for t in which {
        let p = triangles[t].map(|i| vertices[i as usize]).map(|v| [v.x, v.y, v.z]);
        let face = cross(sub(p[1], p[0]), sub(p[2], p[0]));
        for (k, v) in p.iter().enumerate() {
            let normal = if corners {
                let c = corner_normals[t * 3 + k];
                [c.x, c.y, c.z]
            } else {
                face
            };
            out.indices.push(out.positions.len() as u32);
            out.positions.push(at(*v));
            out.normals.push(turn(normal));
        }
    }
    out
}

/// The worker: check every joint (`check`: [`Mech::check`]), telling `tx` of each step and the
/// end, and `job` how far it has got; `stop` (or the job's Cancel) stops it within a step.
fn run(mech: &Mech, job: JobHandle, stop: Arc<AtomicBool>, tx: Sender<Msg>, check: &dyn Fn(&dyn CheckSink) -> Vec<JointCheck>) {
    let (job, whole) = run_steps(mech, job, stop, tx, check);
    end_job(job, whole);
}

/// The check's steps, all but ending its job: the job comes back still running, and whether the
/// check was whole.
fn run_steps(mech: &Mech, job: JobHandle, stop: Arc<AtomicBool>, tx: Sender<Msg>, check: &dyn Fn(&dyn CheckSink) -> Vec<JointCheck>) -> (JobHandle, bool) {
    let n = mech.plan.joints.len();
    let sink = Sink { progress: Mutex::new(Progress::new(mech)), tx: Mutex::new(tx), job: job.reporter(), stop, told: Mutex::new(vec![Instant::now() - GAP_EVERY; n]) };
    let checks = check(&sink);
    // a check cancelled after its last step is whole
    let whole = checks.len() == n;
    let _ = sink.tx.lock().unwrap().send(Msg::Done(checks));
    SignalToUI::set_ui_signal();
    (job, whole)
}

/// End the job as the check ended: done when whole, else cancelled. Says which.
fn end_job(job: JobHandle, whole: bool) -> makepad_widgets::jobs::JobEnd {
    if whole {
        job.done();
        makepad_widgets::jobs::JobEnd::Done { next: None }
    } else {
        job.cancelled();
        makepad_widgets::jobs::JobEnd::Cancelled
    }
}

/// A check running: its worker, the job board's job and what arrives from it. Dropping it stops
/// the worker within a step.
struct CheckRun {
    job: JobId,
    rx: Receiver<Msg>,
    stop: Arc<AtomicBool>,
}

impl Drop for CheckRun {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl CheckRun {
    fn start(cx: &mut Cx, mech: Arc<Mech>) -> Result<CheckRun, String> {
        let job: JobHandle = JobSpec::new("Checking the joints").stage("starting").start();
        let id = job.id();
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let task = cx.task_pool().submit_internal(Class::Visible, move || run(&mech, job, worker_stop, tx, &|s| mech.check(s)));
        match task {
            Ok(task) => {
                task.detach();
                Ok(CheckRun { job: id, rx, stop })
            }
            Err(e) => Err(format!("Could not start the joint check: {e:?}")),
        }
    }
}

/// What builds the ghosts' scene: the plate, the document's name, the piece and per body its
/// plate part's object and model -> plate frame.
type GhostInput = (Arc<Evaluated>, String, Arc<Mech>, Vec<(String, [[f64; 4]; 3])>);

/// A joint check and what it found, kept after it ends; the sim shows it while it is of the
/// piece the sim is of.
pub struct Check {
    pub found: CheckState,
    progress: Progress,
    run: Option<CheckRun>,
    /// The piece or its print settings changed since: what it found is out of date.
    pub stale: bool,
    /// Per ghost (joint, turn): its object's name in the ghosts' scene.
    slots: Vec<((usize, usize), String)>,
    ghost_input: GhostInput,
    building: Option<Receiver<Arc<Scene>>>,
}

/// What a poll of the check brought.
#[derive(Default)]
pub struct Polled {
    /// It found more: the view and the panel change.
    pub news: bool,
    /// It ended (done or cancelled): the sim takes the measured ranges.
    pub ended: bool,
    /// The scene with its ghosts arrived.
    pub scene: Option<Arc<Scene>>,
}

impl Check {
    /// Check every joint of `mech` (of `ev`, at its profile); `refs`: per body, its plate part's
    /// object and model -> plate frame; `name`: the document's.
    pub fn start(cx: &mut Cx, mech: Arc<Mech>, ev: Arc<Evaluated>, name: String, refs: Vec<(String, [[f64; 4]; 3])>) -> Result<Check, String> {
        let found = CheckState::new(&mech, (ev.content, ev.profile.clone()));
        let slots = found.joints.iter().enumerate().flat_map(|(j, f)| (0..f.turns).map(move |t| ((j, t), format!("{}mech-ghost-{j}-{t}", fab::api::CONTEXT_PREFIX)))).collect();
        let run = CheckRun::start(cx, mech.clone())?;
        let mut c = Check { progress: Progress::new(&mech), found, run: Some(run), stale: false, slots, ghost_input: (ev, name, mech, refs), building: None };
        c.build_ghosts(cx);
        Ok(c)
    }

    /// Build the ghosts' scene (again, when another scene took its place).
    pub fn build_ghosts(&mut self, cx: &mut Cx) {
        if self.building.is_some() || self.slots.is_empty() {
            return;
        }
        let (ev, name, mech, refs) = self.ghost_input.clone();
        let slots = self.slots.clone();
        let (tx, rx) = channel();
        if let Ok(task) = cx.task_pool().submit_internal(Class::Visible, move || {
            let _ = tx.send(ghost_scene(&ev, &name, &mech, &refs, &slots));
            SignalToUI::set_ui_signal();
        }) {
            task.detach();
            self.building = Some(rx);
        }
    }

    /// The ghosts' scene is on its way.
    pub fn building(&self) -> bool {
        self.building.is_some()
    }

    /// The running check's job.
    pub fn job(&self) -> Option<JobId> {
        self.run.as_ref().map(|r| r.job)
    }

    /// Stop a running check (what it found so far stays).
    pub fn stop(&mut self) {
        if let Some(r) = &self.run {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Take what arrived.
    pub fn poll(&mut self) -> Polled {
        let mut out = Polled::default();
        if let Some(run) = &self.run {
            loop {
                match run.rx.try_recv() {
                    Ok(Msg::Found(j, f)) => {
                        out.news = true;
                        self.progress.step(j, &f);
                        self.found.apply(j, &f);
                    }
                    Ok(Msg::Done(checks)) => {
                        out.news = true;
                        out.ended = true;
                        self.found.finish(checks);
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    // a worker that ends without saying so (the app closing its pool) ended it
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        out.news = true;
                        out.ended = true;
                        self.found.finish(Vec::new());
                        break;
                    }
                }
            }
        }
        if out.ended {
            self.run = None;
        }
        if let Some(rx) = &self.building {
            match rx.try_recv() {
                Ok(scene) => {
                    out.scene = Some(scene);
                    self.building = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.building = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        out
    }

    /// The ghosts' objects are in this scene.
    pub fn ghosts_in(&self, scene: &Scene) -> bool {
        self.slots.iter().all(|(_, n)| scene.elements.iter().any(|e| e.name == *n))
    }

    /// The panel's and the view's joint check: `place(i)` takes body `i` from the parts' frame
    /// (mm) to where it is now (the printer frame, metres), `rest(i)` from where its part rests
    /// on the plate (metres) to the parts' frame. Also each ghost's element and its move (none:
    /// hidden).
    pub fn view(&self, scene: &Scene, place: &dyn Fn(usize) -> Option<M4>, rest: &dyn Fn(usize) -> Option<M4>) -> (MechCheckView, Vec<(ElementId, Option<M4>)>) {
        let f = &self.found;
        let mut v = MechCheckView { joints: f.joints.len(), job: self.job(), line: f.line(), stale: self.stale, rows: f.rows(&self.progress), ..Default::default() };
        let mut moves = Vec::new();
        let at = |m: &M4, p: P3| apply(m, p).map(|c| c as f32);
        if !self.stale {
            for (body, pts, kind) in f.lines() {
                if let Some(m) = place(body) {
                    v.lines.push((pts.iter().map(|p| at(&m, *p)).collect(), kind));
                }
            }
            for (body, p, label, kind) in f.marks() {
                if let Some(m) = place(body) {
                    v.marks.push(MechMark { at: at(&m, p), label, kind });
                }
            }
        }
        let shown = if self.stale { Vec::new() } else { f.ghosts() };
        for (slot, name) in &self.slots {
            let Some(e) = scene.elements.iter().find(|e| e.name == *name).map(|e| e.id) else { continue };
            let j = &f.joints[slot.0];
            let w = shown.iter().find(|(s, ..)| s == slot).and_then(|(_, axis, angle, stopped)| {
                let m = mul(&mul(&place(j.a)?, &move_matrix(j.pivot, *axis, *angle, j.kind == "slider")), &rest(j.b)?);
                v.ghosts.push((e, if *stopped { STOPPED } else { TESTING }));
                Some(m)
            });
            moves.push((e, w));
        }
        (v, moves)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mechanics::{m4, IDENTITY};
    use makepad_widgets::jobs::{board, JobEnd};
    use std::collections::BTreeMap;

    /// The jewellery box, or the same box with its hinge built with no clearance (fused).
    fn jewellery_box(fused: bool) -> Arc<Evaluated> {
        let d = crate::demo_part::demo("jewellery_box").expect("the jewellery box demo");
        let project = format!("{}.{}", d.key, fab_model::project::EXTENSION);
        let text = d.files.iter().find(|(f, _)| *f == project).expect("its project").1;
        // Progress, marks and ghosts need every phase, but not the demo's full width.
        // Keep the real box at its smallest supported width.
        let text = text.replace("script(\"jewellery_box.cad.splash\")", "script(\"jewellery_box.cad.splash\", params: {box_width: 40mm})");
        let hinge = "plate: plate_len})";
        let scripts: BTreeMap<String, String> = d
            .files
            .iter()
            .filter(|(f, _)| *f != project)
            .map(|(f, s)| {
                assert!(!fused || !f.ends_with(".cad.splash") || s.contains(hinge), "the hinge's options end with its plate");
                (f.to_string(), if fused { s.replace(hinge, "plate: plate_len clearance: 0mm})") } else { s.to_string() })
            })
            .collect();
        let model = fab_model::FabModel::load(d.title, &text, scripts).unwrap_or_else(|f| panic!("{f}"));
        let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        Arc::new(model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1)
    }

    /// Two short links exercise the complete check and its UI messages without a box's
    /// long barrel. Both variants use the same evaluated-model path as the demo.
    fn hinge_pair(fused: bool) -> Arc<Evaluated> {
        let clearance = if fused { "clearance: 0mm clearance_axial: 0mm" } else { "" };
        let source = format!(
            "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\
             let a = m.box({{size: [12mm 6mm 5mm] at: [6mm 3mm 0]}})\n\
             let b = m.box({{size: [12mm 6mm 5mm] at: [18mm 3mm 0]}})\n\
             let hinge = m.hinge(a, b, {{at: [12mm 0 0] axis: [0 1 0] {clearance}}})\n\
             object(a)\n"
        );
        let scripts = BTreeMap::from([("hinge.cad.splash".to_string(), source)]);
        let model = fab_model::FabModel::load("hinge", "use mod.fab.*\nscript(\"hinge.cad.splash\")\nplace(\"hinge\", at: [100mm 100mm])\n", scripts).unwrap_or_else(|f| panic!("{f}"));
        let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        Arc::new(model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1)
    }

    /// Per body, its plate part's object and model -> plate frame, as the sim takes them.
    fn refs(ev: &Evaluated, mech: &Mech) -> Vec<(String, [[f64; 4]; 3])> {
        mech.names
            .iter()
            .map(|n| ev.objects.iter().find(|o| o.name == *n).and_then(|o| o.part))
            .map(|p| p.and_then(|p| Some((ev.plate.parts.get(p)?.object.clone(), *ev.model_to_plate.get(p)?))).unwrap_or((String::new(), IDENTITY)))
            .collect()
    }

    /// When a test's check is cancelled, through the job board's Cancel as the busy line does.
    #[derive(Clone, Copy, PartialEq)]
    enum Cancel {
        Never,
        Before,
        /// As it slices its first layer.
        FirstLayer,
    }

    /// The worker's sink, cancelling its job when told to; it counts the steps after that, and
    /// keeps the bar's share at each step.
    struct CancelAt<'a> {
        inner: &'a dyn CheckSink,
        cancel: Cancel,
        job: JobId,
        pressed: AtomicBool,
        after: std::sync::atomic::AtomicUsize,
        trace: Mutex<(Progress, Vec<f64>)>,
    }

    impl CheckSink for CancelAt<'_> {
        fn found(&self, joint: usize, found: Found) {
            if self.pressed.load(Ordering::SeqCst) {
                self.after.fetch_add(1, Ordering::SeqCst);
            }
            {
                let mut t = self.trace.lock().unwrap();
                t.0.step(joint, &found);
                let share = t.0.fraction();
                t.1.push(share);
            }
            let press = self.cancel == Cancel::FirstLayer && matches!(found, Found::Layer { .. }) && !self.pressed.load(Ordering::SeqCst);
            self.inner.found(joint, found);
            if press {
                board().cancel(self.job);
                self.pressed.store(true, Ordering::SeqCst);
            }
        }
        fn cancelled(&self) -> bool {
            self.inner.cancelled()
        }
    }

    /// The check as the panel runs it, its job on the board: every message, in order, the
    /// steps it took after its Cancel was pressed, and the bar's share at every step.
    fn run_check(mech: &Arc<Mech>, cancel: Cancel) -> (Vec<Msg>, JobEnd, Option<f32>, usize, Vec<f64>) {
        let job = JobSpec::new("Checking the joints").stage("starting").start();
        let id = job.id();
        if cancel == Cancel::Before {
            board().cancel(id);
        }
        let after = std::sync::atomic::AtomicUsize::new(0);
        let trace = Mutex::new(Vec::new());
        let (tx, rx) = channel();
        // the board's history drops ended jobs (the newest dozen stay, a quick success not even
        // that), so the test reads its job's bar while it still runs, and takes how it ended
        // from ending it itself
        let (job, whole) = run_steps(mech, job, Arc::new(AtomicBool::new(false)), tx, &|sink| {
            let w = CancelAt { inner: sink, cancel, job: id, pressed: AtomicBool::new(cancel == Cancel::Before), after: Default::default(), trace: Mutex::new((Progress::new(mech), Vec::new())) };
            let checks = mech.check(&w);
            after.store(w.after.load(Ordering::SeqCst), Ordering::SeqCst);
            *trace.lock().unwrap() = std::mem::take(&mut w.trace.lock().unwrap().1);
            checks
        });
        let fraction = board().get(id).expect("a running job is on the board").fraction;
        let end = end_job(job, whole);
        (rx.try_iter().collect(), end, fraction, after.load(Ordering::SeqCst), trace.into_inner().unwrap())
    }

    /// What the panel keeps of a run: its progress and what it found, fed the messages in order.
    /// The bar's share after each step.
    /// Also what it showed of each joint just before the end.
    fn replay(mech: &Mech, ev: &Evaluated, msgs: Vec<Msg>) -> (Progress, CheckState, Vec<f64>, Vec<String>, Vec<JointFound>) {
        let mut p = Progress::new(mech);
        let mut s = CheckState::new(mech, (ev.content, ev.profile.clone()));
        let (mut bar, mut words, mut shown) = (Vec::new(), Vec::new(), Vec::new());
        for m in msgs {
            match m {
                Msg::Found(j, f) => {
                    p.step(j, &f);
                    s.apply(j, &f);
                    bar.push(p.fraction());
                    words.push(p.words());
                }
                Msg::Done(checks) => {
                    shown = s.joints.clone();
                    s.finish(checks);
                }
            }
        }
        (p, s, bar, words, shown)
    }

    fn near(a: P3, b: P3) -> bool {
        len(sub(a, b)) < 1e-9
    }

    /// The ghosts draw in a pass of their own: at rest each lies on its part, yet the overlap
    /// pass pairs none of them, and each draws with its part's priority and group.
    #[test]
    fn the_ghosts_draw_in_a_pass_of_their_own_with_their_parts_priority() {
        let ev = jewellery_box(false);
        let mech = ev.mech.first().expect("the box and lid are jointed").clone();
        let refs = refs(&ev, &mech);
        let slots: Vec<((usize, usize), String)> = (0..2).map(|t| ((0, t), format!("{}mech-ghost-0-{t}", fab::api::CONTEXT_PREFIX))).collect();
        let scene = ghost_scene(&ev, "box", &mech, &refs, &slots);
        let ghost = |name: &str| slots.iter().any(|(_, g)| g == name);
        assert!(scene.overlap_report.pairs.iter().all(|r| !ghost(&r.element_a_name) && !ghost(&r.element_b_name)), "{:?}", scene.overlap_report.pairs.iter().map(|r| (&r.element_a_name, &r.element_b_name)).collect::<Vec<_>>());
        let drawn = |name: &str| {
            let e = scene.elements.iter().position(|e| e.name == name).expect("an element of the scene");
            scene.parts.iter().filter(|p| p.element.index() == e).map(|p| (p.draw_priority, p.coplanar_group)).max().expect("its parts")
        };
        let moving = &refs[mech.plan.decls[0].parts().1].0;
        for (_, g) in &slots {
            assert_eq!(drawn(g), drawn(moving), "{g} draws as {moving} does");
        }
    }

    /// The jewellery box's check, run as the panel runs it: its steps come in order, gap search
    /// then layers then the sweep's two turns; the bar grows at every step and ends full; it says what
    /// it is doing; and what it finds is what the check finds run alone. Its marks sit where it
    /// measured (the closest gap, the tacks, the stops), the ghosts are turned to the angles it
    /// stopped at, and the sim gets the measured range.
    #[test]
    fn the_jewellery_box_check_shows_its_progress_and_marks_what_it_measured() {
        let ev = jewellery_box(false);
        let mech = ev.mech.first().expect("the box and lid are jointed").clone();
        let t = Instant::now();
        let (msgs, end, fraction, _, trace) = run_check(&mech, Cancel::Never);
        let took = t.elapsed().as_secs_f64();
        let steps = msgs.len() - 1;
        assert!(matches!(end, JobEnd::Done { .. }), "{end:?}");
        assert_eq!(fraction, Some(1.0), "the board's bar ends full");
        // phases in order: a joint's phase never goes back
        // (a layer's rows come in the phase they slice for)
        let phase = |f: &Found, last: usize| match f {
            Found::Ahead { .. } => 0,
            Found::Rows { .. } => last,
            Found::Gap { .. } => 1,
            Found::Layer { .. } | Found::Tack(_) => 2,
            Found::Turn { turn, .. } | Found::Cells { turn, .. } => 3 + 2 * turn,
            Found::Stop(t) => 4 + 2 * t.turn,
        };
        let mut last = 0;
        for m in &msgs[..steps] {
            let Msg::Found(0, f) = m else { panic!("one joint's steps, then the end") };
            assert!(phase(f, last) >= last, "{f:?} after phase {last}");
            last = phase(f, last);
        }
        assert_eq!(last, 6, "it ends with the second turn's stop");
        let (p, s, bar, words, shown) = replay(&mech, &ev, msgs);
        assert!(bar.windows(2).all(|w| w[1] >= w[0]), "the bar only grows");
        assert_eq!(bar.last().copied(), Some(1.0), "and ends full");
        let moving = bar.windows(2).filter(|w| w[1] > w[0]).count();
        println!("{steps} steps in {took:.2} s, the bar moved at {moving} of them; e.g. {:?}", [&words[0], &words[words.len() / 2], &words[words.len() - 1]]);
        assert!(words.iter().any(|w| w.contains("hinge 1 of 1 · slicing a few layers first: ")));
        assert!(words.iter().any(|w| w.contains("hinge 1 of 1 · finding the closest gap: ")));
        assert!(words.iter().any(|w| w.contains("hinge 1 of 1 · slicing layer ")));
        assert!(words.iter().any(|w| w.contains("hinge 1 of 1 · sweeping open: +") || w.contains("hinge 1 of 1 · sweeping open: -")), "{words:?}");
        assert_eq!(p.fraction(), 1.0);
        // it moves at every step the check takes, from the first it has a rate for until it is
        // full: it never sits still while the check works
        let from = trace.iter().position(|s| *s > 0.0).expect("the bar moves");
        let still = trace[from..].windows(2).filter(|w| w[0] < 1.0 && w[1] <= w[0]).count();
        assert_eq!(still, 0, "the bar sat still at {still} of the {} steps from its first move", trace.len() - from);
        // what it showed as it went ends on what the check measured (the check with a sink
        // answers as without one: makepad-csg-mech's mechanisms test)
        assert!(!s.running && !s.cancelled);
        let c = &s.joints[0].check.clone().expect("the hinge is checked");
        println!("the check: {:.0} ms (gap {:.0}, layers {:.0}, sweep {:.0})", c.millis, c.phases.0, c.phases.1, c.phases.2);
        assert_eq!(shown[0].gap, Some((c.gap.gap, c.gap.at)), "the closest gap");
        assert!(shown[0].gap_done);
        assert_eq!(shown[0].tacks, c.layers.tacks, "the tacks");
        assert_eq!(shown[0].stops, c.turns, "the stops");
        assert_eq!(shown[0].turning, None, "no angle under test at the end");
        println!("{}; {}", s.line(), s.rows(&p).join("; "));
        assert_eq!(s.line(), format!("{} of 1 joint free · worst fuse risk {}", c.ok() as usize, c.risk.name()));
        // the marks sit where it measured
        let marks = s.marks();
        let at = |kind: MechMarkKind| marks.iter().filter(move |m| m.3 == kind).map(|m| m.1);
        assert!(marks.iter().any(|m| near(m.1, c.gap.at) && m.2 == closest(c.gap.gap)), "{marks:?}");
        // a mark per turn: where it touched, else (free all the way) at its end of the range's arc
        assert_eq!(at(MechMarkKind::Stop).count(), c.turns.len());
        for t in &c.turns {
            let label = format!("{} {} {}", turn_name("hinge", t.turn), if t.contact.is_some() { "stops at" } else { "free to" }, amount(t.angle, false));
            let m = marks.iter().find(|m| m.3 == MechMarkKind::Stop && m.2 == label).unwrap_or_else(|| panic!("{label}: {marks:?}"));
            assert!(t.contact.map_or(true, |p| near(p, m.1)), "{label} where it touched");
        }
        assert!(at(MechMarkKind::Tack).all(|m| c.layers.tacks.iter().any(|t| near(t.at, m))));
        assert!(marks.iter().all(|m| m.0 == c.a), "they ride on the fixed part");
        // the ghosts at the angles it stopped at
        let ghosts = s.ghosts();
        assert_eq!(ghosts.len(), 2);
        for (k, t) in c.turns.iter().enumerate() {
            assert!(ghosts.iter().any(|g| g.0 == (0, k) && g.2 == t.angle && g.3), "{ghosts:?}");
        }
        // the sim takes the measured range
        let kinds: Vec<JointKind> = mech.assembly(&[], 1.0).unwrap().joints.iter().map(|j| j.kind.clone()).collect();
        let limits = s.limits(&kinds);
        assert_eq!(limits, vec![(0, kinds[0].measured(c.effective))]);
        println!("the sim's hinge: {:?} -> {:?}", kinds[0], limits[0].1);

        // in the view: each mark where its part is, each ghost its part turned about the pin
        let refs = refs(&ev, &mech);
        let slots: Vec<((usize, usize), String)> = (0..2).map(|t| ((0, t), format!("{}mech-ghost-0-{t}", fab::api::CONTEXT_PREFIX))).collect();
        let scene = ghost_scene(&ev, "box", &mech, &refs, &slots);
        let printed = |i: usize| mul(&[[0.001, 0.0, 0.0, 0.0], [0.0, 0.001, 0.0, 0.0], [0.0, 0.0, 0.001, 0.0], [0.0, 0.0, 0.0, 1.0]], &m4(&refs[i].1));
        let place = |i: usize| (i < refs.len()).then(|| printed(i));
        let rest = |i: usize| (i < refs.len()).then(|| crate::mechanics::inv(&printed(i)));
        let check = Check { found: s, progress: p, run: None, stale: false, slots: slots.clone(), ghost_input: (ev.clone(), "box".into(), mech.clone(), refs.clone()), building: None };
        assert!(check.ghosts_in(&scene), "the ghosts' scene has a ghost per turn");
        let (view, moves) = check.view(&scene, &place, &rest);
        assert_eq!(view.marks.len(), marks.len());
        for (m, (body, p, ..)) in view.marks.iter().zip(&marks) {
            let w = apply(&printed(*body), *p);
            assert!((0..3).all(|k| (m.at[k] as f64 - w[k]).abs() < 1e-6), "{:?} at {w:?}", m.at);
        }
        assert_eq!(view.ghosts.len(), 2);
        assert_eq!(moves.len(), 2);
        let j = &check.found.joints[0];
        let v = mech.finals[j.b].vertices[0];
        let v = [v.x, v.y, v.z];
        for (k, (e, w)) in moves.iter().enumerate() {
            assert!(view.ghosts.iter().any(|g| g.0 == *e && g.1 == STOPPED));
            let w = w.expect("a stopped turn's ghost shows");
            let t = &c.turns[k];
            let shown = apply(&w, apply(&printed(j.b), v));
            let want = apply(&printed(j.a), add(j.pivot, rotate(sub(v, j.pivot), norm(t.axis), t.angle)));
            assert!(len(sub(shown, want)) < 1e-9, "the ghost of turn {k} is the moving part turned {:.1}°", t.angle.to_degrees());
        }
        // and out of date after an edit: no marks or ghosts, the line says so
        let edited = jewellery_box(true);
        assert!(!check.found.is_of(&mech.instance, edited.content, &edited.profile), "another shape is another check");
        assert!(check.found.is_of(&mech.instance, ev.content, &ev.profile));
        let stale = Check { stale: true, ..check };
        let (view, moves) = stale.view(&scene, &place, &rest);
        assert!(view.stale && view.marks.is_empty() && view.lines.is_empty() && view.ghosts.is_empty() && moves.iter().all(|(_, w)| w.is_none()));
        assert!(fab::ui::mechanics::check_line(&view).ends_with("out of date: the part or its settings changed"), "{}", fab::ui::mechanics::check_line(&view));
    }

    /// Two links with a hinge built with no clearance: the check says it is fused, and where; the
    /// view marks the spot and the panel's row says why it is not free.
    #[test]
    fn a_fused_hinge_says_fused_at_the_spot() {
        let ev = hinge_pair(true);
        let mech = ev.mech.first().expect("jointed").clone();
        let (msgs, _, _, _, _) = run_check(&mech, Cancel::Never);
        let (p, s, _, _, _) = replay(&mech, &ev, msgs);
        let c = s.joints[0].check.as_ref().expect("checked");
        println!("{}; {}", s.line(), s.rows(&p).join("; "));
        assert!(!c.ok());
        let Some(Bind::Fused { at, .. }) = c.binds.iter().find(|b| matches!(b, Bind::Fused { .. })).copied() else { panic!("fused: {:?}", c.binds) };
        assert!(near(at, c.gap.at));
        assert!(s.marks().iter().any(|m| m.3 == MechMarkKind::Bind && near(m.1, at) && m.2.starts_with("fused here")));
        let row = &s.rows(&p)[0];
        assert!(row.contains(": not free · fused at ["), "{row}");
        assert_eq!(s.line(), format!("0 of 1 joint free · worst fuse risk {}", c.risk.name()));
    }

    /// Cancel stops the check within a step: pressed before it starts, the joint takes its one
    /// step and stops; pressed as it slices, it stops long before the sweep, and the panel says
    /// it was cancelled.
    #[test]
    fn cancel_stops_the_check_within_a_step() {
        let ev = hinge_pair(false);
        let mech = ev.mech.first().expect("jointed").clone();
        let (msgs, end, _, after, _) = run_check(&mech, Cancel::Before);
        assert!(after <= 1, "cancelled before it starts: {after} steps");
        assert!(matches!(end, JobEnd::Cancelled), "{end:?}");
        let (_, s, _, _, _) = replay(&mech, &ev, msgs);
        assert!(s.cancelled && !s.running);
        assert_eq!(s.line(), "Cancelled: 0 of 1 joint checked");
        let (msgs, end, _, after, _) = run_check(&mech, Cancel::FirstLayer);
        println!("cancelled as it sliced its first layer: {after} steps after");
        assert_eq!(after, 0, "the step it was cancelled at is its last");
        assert!(matches!(end, JobEnd::Cancelled), "{end:?}");
        assert!(matches!(msgs.last(), Some(Msg::Done(c)) if c.is_empty()));
        assert!(matches!(msgs[msgs.len() - 2], Msg::Found(_, Found::Layer { k: 1, .. })), "the last step is the first layer");
        assert!(!msgs.iter().any(|m| matches!(m, Msg::Found(_, Found::Stop(_)))), "it never got to the sweep's end");
    }
}
