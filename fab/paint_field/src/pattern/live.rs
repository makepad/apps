//! The pattern simulations the process runs: one per lineage (the part's shape and the spec
//! but its steps), held by the paint that shows it, and the last few to reach their steps a
//! while after, so more steps carry on from there.
//!
//! - [`resolve`] gives a mix its pattern on a part: the finished state when one is kept, else
//!   its simulation's latest, the simulation stepping on toward the asked steps: more carries
//!   on, fewer replays from the seed. A part never shows unpatterned: a new simulation steps a
//!   moment first, unless that first step builds its fine lattice (a preset that starts on it,
//!   the longest step there is). Then [`work`] takes that step, and the part shows the state its
//!   pattern showed before until then. A caller that must have the print's state (a slice, a
//!   test, a picture) waits for it there.
//! - [`work`] steps the simulations that want it, on the caller's workers, a slice of time each,
//!   publishing a state about every [`FRAME`] (or [`DRAWN`] times what drawing the last took)
//!   for the views and the finished one for every sampler; [`serial`] moves with each and the
//!   installed wake rings.
//! - A simulation pauses and resumes ([`Sim::set_paused`]); the step it is at and its rate
//!   show while it runs.

use super::band::{Distances, Level, Surface, SKIN};
use super::rd::{self, Rates, State};
use super::{fnv, Grid, Grown, Model, Spec};
use crate::bake::Fan;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// How often a running simulation publishes its state for the views, at most.
pub const FRAME: Duration = Duration::from_millis(33);
/// A simulation publishes its next state for the views no sooner than this many times what
/// drawing the last took after it, so they take at most a quarter of its time.
const DRAWN: u32 = 3;
/// How long a new simulation steps where it is asked for (its first state shows a pattern
/// already forming).
pub const WARM: Duration = Duration::from_millis(60);
/// How long [`work`] steps one simulation before the next.
const TURN: Duration = Duration::from_millis(150);
/// The finished states kept, at most (bytes).
const FINALS_BYTES: usize = 256 << 20;
/// The finished simulations kept after their paint lets them go, newest first: steps raised
/// on one carry on from where it stopped.
const KEPT_SIMS: usize = 4;
/// A pattern has settled once its places move less per step, on average over its cells, than
/// this share of what they moved [`SETTLE_AFTER`] steps into the fine lattice (where the
/// pattern was forming, past the refinement's first shake), and until they move twice that
/// again. The patterns drift on slowly for long after: this marks where they turned from
/// forming to drifting (spots near 2600 of their 3000 steps, dots near 1300 of 2000). Scales
/// settle once no cell changes, flow lines once no line is left to draw.
pub const SETTLED: f64 = 0.2;
/// The steps on the fine lattice whose change [`SETTLED`] is a share of.
pub const SETTLE_AFTER: u32 = 20;

struct Run {
    step: u32,
    fine: bool,
    now: State,
    next: State,
    /// The change [`SETTLED`] is a share of (0 before it is measured), and the step it settled
    /// at while it stays settled.
    forming: f64,
    settled: Option<u32>,
    /// When it last published a state for the views, and what that took.
    shown: Option<Instant>,
    drawing: Duration,
}

impl Run {
    fn seeded(state: State) -> Run {
        Run { step: 0, fine: false, next: state.clone(), now: state, forming: 0.0, settled: None, shown: None, drawing: Duration::ZERO }
    }

    /// Whether a state for the views is due: a [`FRAME`] on from the last, or [`DRAWN`] times
    /// what drawing the last took, so the views take at most a share of the stepping.
    fn due(&self) -> bool {
        self.shown.is_none_or(|t| t.elapsed() >= FRAME.max(self.drawing * DRAWN))
    }
}

/// One pattern growing on one part's surface.
pub struct Sim {
    pub lineage: u64,
    pub spec: Spec,
    distances: Distances,
    coarse: Level,
    fine: OnceLock<Level>,
    run: Mutex<Run>,
    target: AtomicU32,
    paused: AtomicBool,
    /// Stepping the fine lattice now.
    on_fine: AtomicBool,
    latest: Mutex<Arc<Grid>>,
    /// Steps a second over its last turn (f64 bits).
    rate: AtomicU64,
    /// How much its places moved in its last fine step, on average over its cells (f64 bits).
    change: AtomicU64,
    /// When it was made, and how long its first state took (f64 bits, seconds; NaN before it).
    pub made: Instant,
    first: AtomicU64,
}

/// [`Sim::first`] before its first state.
const UNSHOWN: u64 = 0x7ff8_0000_0000_0000;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sim {
    fn new(spec: &Spec, surface: &Surface, fan: Fan) -> Option<Sim> {
        let made = Instant::now();
        let coarse_pitch = 2.0 * spec.pitch();
        let reach = super::band::thickness(coarse_pitch, spec.depth).max(SKIN) + 3.0 * coarse_pitch;
        let distances = Distances::new(surface, coarse_pitch, reach, fan);
        let coarse = Level::new(&distances, coarse_pitch, spec.depth, fan);
        if coarse.band == 0 {
            return None;
        }
        let state = rd::seed(spec, &coarse, fan);
        let lineage = spec.lineage(surface.key);
        let grid = Grid::new(coarse.layout.clone(), &rd::places(spec, &coarse, &state, fan), 0, spec.steps == 0, grid_key(lineage, 0, false));
        Some(Sim {
            lineage,
            spec: spec.clone(),
            distances,
            coarse,
            fine: OnceLock::new(),
            run: Mutex::new(Run::seeded(state)),
            target: AtomicU32::new(spec.steps),
            paused: AtomicBool::new(false),
            on_fine: AtomicBool::new(false),
            latest: Mutex::new(Arc::new(grid)),
            rate: AtomicU64::new(0),
            change: AtomicU64::new(f64::INFINITY.to_bits()),
            made,
            first: AtomicU64::new(UNSHOWN),
        })
    }

    /// A simulation outside the registry (a test's).
    #[cfg(test)]
    pub(crate) fn alone(spec: &Spec, surface: &Surface, fan: Fan) -> Option<Sim> {
        Sim::new(spec, surface, fan)
    }

    /// The fine lattice and the state on it now (a test's look inside).
    #[cfg(test)]
    pub(crate) fn fine_now(&self) -> (&Level, State) {
        (self.fine.get().expect("refined"), lock(&self.run).now.clone())
    }

    /// The lattice it steps and its state there now.
    pub(crate) fn now(&self) -> (&Level, State) {
        let run = lock(&self.run);
        let level = if run.fine { self.fine.get().expect("a fine run has its level") } else { &self.coarse };
        (level, run.now.clone())
    }

    /// Grow to `steps` from here on: more carries on, fewer replays from the seed.
    pub fn set_target(&self, steps: u32) {
        self.target.store(steps, Ordering::Relaxed);
        published();
    }

    /// The steps its latest published state has grown.
    pub fn step(&self) -> u32 {
        lock(&self.latest).step
    }
    /// The steps it grows to.
    pub fn target(&self) -> u32 {
        self.target.load(Ordering::Relaxed)
    }
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
    /// Pause it where it is, or let it carry on.
    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::Relaxed);
        published();
    }
    pub fn latest(&self) -> Arc<Grid> {
        lock(&self.latest).clone()
    }
    /// Steps a second over its last turn.
    pub fn rate(&self) -> f64 {
        f64::from_bits(self.rate.load(Ordering::Relaxed))
    }
    /// How much its places moved in its last step on the fine lattice, on average over its
    /// cells (0..1 a step; infinite before one).
    pub fn change(&self) -> f64 {
        f64::from_bits(self.change.load(Ordering::Relaxed))
    }
    /// How long it took to show its first state (seconds after it was asked for; NaN before it).
    pub fn first_shown(&self) -> f64 {
        f64::from_bits(self.first.load(Ordering::Relaxed))
    }
    /// The cells it steps now.
    pub fn cells(&self) -> usize {
        match self.fine.get() {
            Some(f) if self.on_fine.load(Ordering::Relaxed) => f.band,
            _ => self.coarse.band,
        }
    }
    /// How many distance samples the band was built from.
    pub fn distance_samples(&self) -> usize {
        self.distances.samples()
    }

    fn wants(&self) -> bool {
        !self.paused() && !lock(&self.latest).done_at(self.target())
    }

    /// Whether its next step builds its fine lattice first: the longest a step takes, which a
    /// preset that starts on the fine lattice (`coarse` 0) takes first.
    fn refines_next(&self) -> bool {
        let run = lock(&self.run);
        !run.fine && run.step >= self.spec.preset.coarse && run.step < self.target()
    }

    fn publish(&self, run: &mut Run, done: bool, fan: Fan) {
        let drawing = Instant::now();
        let level = if run.fine { self.fine.get().expect("a fine run has its level") } else { &self.coarse };
        let mut grid = Grid::new(level.layout.clone(), &rd::places(&self.spec, level, &run.now, fan), run.step, done, grid_key(self.lineage, run.step, run.fine));
        grid.settled = run.settled;
        let grid = Arc::new(grid);
        if done {
            keep_final(self.lineage, grid.clone());
        }
        *lock(&self.latest) = grid;
        let _ = self.first.compare_exchange(UNSHOWN, self.made.elapsed().as_secs_f64().to_bits(), Ordering::Relaxed, Ordering::Relaxed);
        published();
        (run.shown, run.drawing) = (Some(Instant::now()), drawing.elapsed());
    }

    /// Steps toward its target until it is there (true), or it is paused, `stop` says so or
    /// `budget` has gone by (after one step at least); its state is published as it goes and
    /// where it stops.
    pub fn advance(&self, fan: Fan, stop: &dyn Fn() -> bool, budget: Option<Duration>) -> bool {
        let mut run = lock(&self.run);
        let started = Instant::now();
        let mut stepped = 0u32;
        let end = |stepped: u32| {
            if stepped > 0 {
                let rate = stepped as f64 / started.elapsed().as_secs_f64().max(1e-6);
                self.rate.store(rate.to_bits(), Ordering::Relaxed);
            }
        };
        loop {
            let target = self.target();
            if run.step > target {
                // fewer steps than it has grown: from the seed again
                *run = Run::seeded(rd::seed(&self.spec, &self.coarse, fan));
                self.on_fine.store(false, Ordering::Relaxed);
            }
            if run.step == target {
                if !lock(&self.latest).done_at(target) {
                    self.publish(&mut run, true, fan);
                }
                end(stepped);
                return true;
            }
            // a call steps at least once: one that yields as soon as asked still moves on
            if self.paused() || (stepped > 0 && (stop() || budget.is_some_and(|b| started.elapsed() >= b))) {
                if stepped > 0 && (self.paused() || run.due()) {
                    self.publish(&mut run, false, fan);
                }
                end(stepped);
                return false;
            }
            if !run.fine && run.step >= self.spec.preset.coarse {
                let fine = self.fine.get_or_init(|| Level::new(&self.distances, self.spec.pitch(), self.spec.depth, fan));
                let now = rd::refine(&self.spec, &self.coarse, &run.now, fine, fan);
                *run = Run { step: run.step, fine: true, next: now.clone(), now, forming: 0.0, settled: None, shown: run.shown, drawing: run.drawing };
                self.on_fine.store(true, Ordering::Relaxed);
            }
            let (level, coarser) = if run.fine { (self.fine.get().expect("refined"), 1.0) } else { (&self.coarse, 2.0) };
            let rates = Rates::of(&self.spec, coarser);
            let at = run.step;
            let Run { now, next, .. } = &mut *run;
            let moved = rd::step(&self.spec, &rates, level, now, next, at, fan);
            run.step += 1;
            if run.fine {
                let p = self.spec.preset;
                let change = moved / (level.band.max(1) as f64 * (p.hi - p.lo));
                self.change.store(change.to_bits(), Ordering::Relaxed);
                if run.step == p.coarse + SETTLE_AFTER {
                    run.forming = change;
                }
                let drawn = matches!(self.spec.model, Model::Flow { .. });
                if change == 0.0 && (drawn || matches!(self.spec.model, Model::Scales { .. })) {
                    // scales grown till they meet change no more, flow lines once no start is
                    // left
                    let at = run.step;
                    run.settled.get_or_insert(at);
                } else if run.forming > 0.0 && !drawn {
                    if change < SETTLED * run.forming {
                        let at = run.step;
                        run.settled.get_or_insert(at);
                    } else if change > 2.0 * SETTLED * run.forming {
                        run.settled = None;
                    }
                }
            }
            stepped += 1;
            if run.due() {
                self.publish(&mut run, false, fan);
            }
        }
    }
}

impl Grid {
    /// Whether it is the finished state at `steps`.
    fn done_at(&self, steps: u32) -> bool {
        self.done && self.step == steps
    }
}

fn grid_key(lineage: u64, step: u32, fine: bool) -> u64 {
    fnv(&[lineage.to_le_bytes(), (step as u64 | (fine as u64) << 40).to_le_bytes()].concat())
}

struct Registry {
    sims: Mutex<Vec<Weak<Sim>>>,
    /// What each caller's slot showed last ([`resolve`]), while its paint holds it.
    slots: Mutex<Vec<(u64, Shown)>>,
    kept: Mutex<VecDeque<Arc<Sim>>>,
    finals: Mutex<VecDeque<(u64, Arc<Grid>)>>,
    serial: AtomicU64,
    wake: OnceLock<fn()>,
}

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| Registry { sims: Mutex::new(Vec::new()), slots: Mutex::new(Vec::new()), kept: Mutex::new(VecDeque::new()), finals: Mutex::new(VecDeque::new()), serial: AtomicU64::new(0), wake: OnceLock::new() })
}

/// What rings when a simulation publishes (the app's UI wake). The first call wins.
pub fn install_wake(wake: fn()) {
    let _ = registry().wake.set(wake);
}

/// Moves with every state published, paused or resumed.
pub fn serial() -> u64 {
    registry().serial.load(Ordering::Relaxed)
}

fn published() {
    let r = registry();
    r.serial.fetch_add(1, Ordering::Relaxed);
    if let Some(w) = r.wake.get() {
        w();
    }
}

/// A simulation that reached its steps, held a while after its paint lets it go.
fn keep_sim(sim: &Arc<Sim>) {
    let mut kept = lock(&registry().kept);
    kept.retain(|k| !Arc::ptr_eq(k, sim));
    kept.push_front(sim.clone());
    kept.truncate(KEPT_SIMS);
}

fn keep_final(lineage: u64, grid: Arc<Grid>) {
    let mut finals = lock(&registry().finals);
    finals.retain(|(l, g)| !(*l == lineage && g.step == grid.step));
    finals.push_front((lineage, grid));
    let mut bytes = 0;
    let mut keep = 0;
    for (_, g) in finals.iter() {
        bytes += g.bytes();
        if bytes > FINALS_BYTES && keep > 0 {
            break;
        }
        keep += 1;
    }
    finals.truncate(keep);
}

fn final_of(lineage: u64, steps: u32) -> Option<Arc<Grid>> {
    let mut finals = lock(&registry().finals);
    let i = finals.iter().position(|(l, g)| *l == lineage && g.step == steps)?;
    let hit = finals.remove(i)?;
    finals.push_front(hit.clone());
    Some(hit.1)
}

/// What a slot shows: a simulation's latest state, or a state nothing grows on.
enum Shown {
    Sim(Weak<Sim>),
    Grid(Weak<Grid>),
}

impl Shown {
    fn of(g: &Grown) -> Shown {
        match (&g.sim, &g.grid) {
            (Some(sim), _) => Shown::Sim(Arc::downgrade(sim)),
            (None, Some(grid)) => Shown::Grid(Arc::downgrade(grid)),
            (None, None) => Shown::Grid(Weak::new()),
        }
    }

    /// The state it shows, while its paint holds it.
    fn grid(&self) -> Option<Arc<Grid>> {
        match self {
            Shown::Sim(sim) => sim.upgrade().map(|s| s.latest()),
            Shown::Grid(grid) => grid.upgrade(),
        }
    }

    fn held(&self) -> bool {
        match self {
            Shown::Sim(sim) => sim.strong_count() > 0,
            Shown::Grid(grid) => grid.strong_count() > 0,
        }
    }
}

/// The state `slot` shows now, while its paint holds it.
fn shown_at(slot: u64) -> Option<Arc<Grid>> {
    lock(&registry().slots).iter().find(|(s, _)| *s == slot).and_then(|(_, shown)| shown.grid())
}

/// `slot` shows `g` from now on; the slots no paint holds any more are let go.
fn remember(slot: u64, g: &Grown) {
    let mut slots = lock(&registry().slots);
    slots.retain(|(s, shown)| *s != slot && shown.held());
    slots.push((slot, Shown::of(g)));
}

/// The simulations still held by some paint.
fn alive() -> Vec<Arc<Sim>> {
    let mut sims = lock(&registry().sims);
    sims.retain(|w| w.strong_count() > 0);
    sims.iter().filter_map(Weak::upgrade).collect()
}

/// A mix's pattern grown on a part: `spec` on the surface `surface` makes (asked for only when
/// no state of its lineage is kept; `None`: the part has no surface there), by its shape's
/// `shape` key, for the caller's `slot` (where it shows: a part's pattern; a new simulation's
/// part shows what its slot showed until its first state, when its first step is a long one).
/// With `wait`, the state at its steps (unless `stop` ends it first); else the latest there is,
/// its simulation going on in [`work`].
pub fn resolve(spec: &Spec, shape: u64, slot: u64, surface: &dyn Fn() -> Option<Surface>, wait: bool, fan: Fan, stop: &dyn Fn() -> bool) -> Grown {
    let grown = resolved(spec, shape, slot, surface, wait, fan, stop);
    remember(slot, &grown);
    grown
}

fn resolved(spec: &Spec, shape: u64, slot: u64, surface: &dyn Fn() -> Option<Surface>, wait: bool, fan: Fan, stop: &dyn Fn() -> bool) -> Grown {
    let lineage = spec.lineage(shape);
    if let Some(grid) = final_of(lineage, spec.steps) {
        return Grown { spec: spec.clone(), grid: Some(grid), sim: None };
    }
    let found = alive().into_iter().find(|s| s.lineage == lineage);
    let (sim, fresh) = match found {
        Some(s) => (s, false),
        None => {
            let Some(s) = surface() else { return Grown::new(spec.clone()) };
            let Some(sim) = Sim::new(spec, &s, fan) else { return Grown::new(spec.clone()) };
            let sim = Arc::new(sim);
            lock(&registry().sims).push(Arc::downgrade(&sim));
            (sim, true)
        }
    };
    sim.target.store(spec.steps, Ordering::Relaxed);
    if wait {
        if sim.advance(fan, stop, None) {
            keep_sim(&sim);
        }
    } else if fresh {
        // a first step that builds the fine lattice is left to [`work`]: the state the slot
        // showed shows till then, as a state on the way (step 0); with none, it steps here
        match shown_at(slot).filter(|_| sim.refines_next()) {
            Some(before) => *lock(&sim.latest) = Arc::new(Grid { step: 0, done: false, settled: None, ..(*before).clone() }),
            None => {
                sim.advance(fan, stop, Some(WARM));
            }
        }
    }
    let grid = sim.latest();
    if grid.done_at(spec.steps) {
        return Grown { spec: spec.clone(), grid: Some(grid), sim: None };
    }
    published();
    Grown { spec: spec.clone(), grid: Some(grid), sim: Some(sim) }
}

/// The simulation of `spec` on the surface `surface` makes (by its shape's `shape` key) grown to
/// its steps, for what reads its state itself (a slime mould's veins): its lineage's running one
/// when that grows to the same steps and is not paused, else a new one. None: `stop` ended it
/// first, or the part has no surface there.
pub(crate) fn grown_sim(spec: &Spec, shape: u64, surface: &dyn Fn() -> Option<Surface>, fan: Fan, stop: &dyn Fn() -> bool) -> Option<Arc<Sim>> {
    let lineage = spec.lineage(shape);
    let sim = match alive().into_iter().find(|s| s.lineage == lineage && s.target() == spec.steps && !s.paused()) {
        Some(s) => s,
        None => Arc::new(Sim::new(spec, &surface()?, fan)?),
    };
    sim.advance(fan, stop, None).then_some(sim)
}

/// Whether a simulation wants stepping.
pub fn has_work() -> bool {
    alive().iter().any(|s| s.wants())
}

/// Steps every simulation that wants it, a turn each, until none does or `stop` says so.
pub fn work(fan: Fan, stop: &dyn Fn() -> bool) {
    loop {
        let sims: Vec<Arc<Sim>> = alive().into_iter().filter(|s| s.wants()).collect();
        if sims.is_empty() || stop() {
            return;
        }
        for s in sims {
            if s.advance(fan, stop, Some(TURN)) {
                keep_sim(&s);
            }
            if stop() {
                return;
            }
        }
    }
}

/// The simulation of `lineage`, while one runs.
pub fn sim(lineage: u64) -> Option<Arc<Sim>> {
    alive().into_iter().find(|s| s.lineage == lineage)
}

/// The finished state of `lineage` at `steps`, when one is kept.
pub fn finished(lineage: u64, steps: u32) -> Option<Arc<Grid>> {
    final_of(lineage, steps)
}

/// `g` as its simulation stands now, for the views: its latest state (`g` itself when nothing
/// grows it).
pub fn shown(g: &Grown) -> Grown {
    match &g.sim {
        Some(sim) => Grown { spec: g.spec.clone(), grid: Some(sim.latest()), sim: Some(sim.clone()) },
        None => g.clone(),
    }
}

/// `g` with the state that prints, once its simulation has grown it; `None` while it has not
/// (or nothing grows it).
pub fn grown(g: &Grown) -> Option<Grown> {
    let sim = g.sim.as_ref()?;
    let grid = finished(sim.lineage, g.spec.steps).or_else(|| Some(sim.latest()).filter(|l| l.done_at(g.spec.steps)))?;
    Some(Grown { spec: g.spec.clone(), grid: Some(grid), sim: None })
}

/// How far `g` has grown toward the steps it prints at: (steps, of).
pub fn progress(g: &Grown) -> (u32, u32) {
    let step = match &g.sim {
        Some(sim) => sim.step().min(g.spec.steps),
        None => g.grid.as_ref().map_or(0, |grid| grid.step),
    };
    (step, g.spec.steps)
}

