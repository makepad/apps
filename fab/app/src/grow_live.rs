//! A grown part's live run (`m.grow`): a thread per part holds the part's sessions on the AI hub. One
//! holds the coarse levels, which answer each change as it is made. Once those settle and nothing
//! is being dragged, the other refines from where they stand, and takes each change that is let go
//! from where it stands. While a change has no refined answer the part shows its refined shape moved
//! as the coarse shape moved, so the shape follows the hand and its detail stays. A shape from
//! another level fades in, so a level taking over never pops, and only a level that is done asks
//! for a slice. While the model is busy a newer shape takes an older one's place, so the model only
//! ever builds the newest.

use crate::model_host::HostAction;
use fab_force::hub::Hub;
use fab_force::study::Load;
use fab_model::grown::{inputs_hash, Stored};
use fab_optimise::grow;
use fab_optimise::grown::{preview, Fade, Posed};
use makepad_ai_hub::program_client::{RemoteConfig, RemoteSession};
use makepad_compute_program::{Class, ProgramError, Updates};
use makepad_model_script::lang::csg_sdf::{SdfGrid, SdfNode};
use makepad_model_script::lang::GrowRequest;
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Thread;
use std::time::{Duration, Instant};

/// How long after a look for the AI hub that found none it looks again.
const LOOK_AGAIN: Duration = Duration::from_secs(2);
/// How long a shape takes to turn into one from another level.
const FADE: Duration = Duration::from_millis(150);
/// A fade's steps come no closer than this, and only as the model takes them: however slow it is, a
/// fade shows this many.
const FADE_STEP: Duration = Duration::from_millis(30);
const FADE_STEPS: u32 = 4;
/// How long a run with something else to watch looks at one thing.
const LOOK: Duration = Duration::from_millis(4);

/// How far from solved a solve may stop (its residual's square over the load's) and the shape still
/// answer its forces. Solid parts stop under 1e-3; slender ones with few cells across stop at 10 and
/// up, their forces not found at all.
const UNSOLVED: f64 = 1e-2;

/// What a part says while no AI hub answers: it goes on as soon as one does.
const NO_HUB: &str = "No AI hub is running: this part grows on the AI hub. Start the AI Hub app and it goes on.";

/// What a part's force study lacks to grow.
pub fn study_missing(study: Option<&fab_model::study::Study>) -> &'static str {
    let holds = study.is_some_and(|s| s.loads.iter().any(|l| l.kind == fab_model::study::Kind::Fixed));
    let loads = study.is_some_and(|s| s.loads.iter().any(|l| l.kind != fab_model::study::Kind::Fixed));
    match (holds, loads) {
        (false, false) => "grows once its force study holds it and loads it (m.fixed, m.force); until then it is its whole space",
        (false, true) => "grows once its force study holds it (m.fixed); until then it is its whole space",
        _ => "grows once its force study loads it (m.force); until then it is its whole space",
    }
}

/// The AI hub the runs share, once one answered.
#[derive(Clone, Default)]
pub struct SharedHub(Arc<Mutex<Option<Arc<Hub>>>>);

impl SharedHub {
    fn open(&self) -> Option<Arc<Hub>> {
        let mut known = self.0.lock().unwrap();
        if !known.as_ref().is_some_and(|h| h.is_open()) {
            let hub = Hub::find();
            *known = hub.is_open().then(|| Arc::new(hub));
        }
        known.clone()
    }
}

/// What a run is to answer: the newest asked.
struct Want {
    loads: Vec<Load>,
    mass: f64,
    /// Counts the changes, and when the last came.
    count: u64,
    at: Instant,
    /// A hand is on it: the coarse levels follow, and the rest waits for it to let go.
    dragging: bool,
}

struct Shared {
    want: Mutex<Want>,
    stop: AtomicBool,
    /// Its thread has returned: it answers nothing more.
    ended: AtomicBool,
    /// The session, for waking the thread that waits on it.
    session: Mutex<Option<Arc<RemoteSession>>>,
    thread: Mutex<Option<Thread>>,
}

/// The newest shape a run has shown, until the model takes it: the grid, and whether it is a level
/// done, which a slice follows.
pub struct Newest {
    pub grid: Arc<SdfGrid>,
    pub settled: bool,
    /// The shape a fade was on its way to.
    pub faded: bool,
}

pub type Slot = Arc<Mutex<Option<Newest>>>;

/// A run settled: the shape to keep ([`crate::model_host::Job::GrownSettled`]).
#[derive(Clone)]
pub struct Settled {
    pub file: String,
    pub part: String,
    pub run: u64,
    pub stored: Arc<Stored>,
}

impl std::fmt::Debug for Settled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settled").field("file", &self.file).field("part", &self.part).field("run", &self.run).finish()
    }
}

/// A run has a shape for the model ([`crate::model_host::Job::Grown`]), in its slot.
#[derive(Clone)]
pub struct Shown {
    pub file: String,
    pub part: String,
    pub run: u64,
    pub slot: Slot,
}

impl std::fmt::Debug for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shown").field("file", &self.file).field("part", &self.part).field("run", &self.run).finish()
    }
}

/// A running grow of one part, for one shape of what it grows in. Dropped, it ends on the hub.
pub struct Run {
    pub id: u64,
    /// The part it is a feature of, whose panel says how it goes.
    pub object: String,
    /// What it grows in: another means another run.
    pub space: u64,
    loads: Vec<Load>,
    mass: f64,
    dragging: bool,
    /// What the model holds, for a drag called off.
    held: (Vec<Load>, f64),
    shared: Arc<Shared>,
}

impl Run {
    #[allow(clippy::too_many_arguments)]
    pub fn start(id: u64, hub: SharedHub, file: String, object: String, request: GrowRequest, root: SdfNode, space: u64, loads: Vec<Load>, stored: Option<(Arc<Stored>, Arc<SdfGrid>)>) -> Run {
        let shared = Arc::new(Shared {
            want: Mutex::new(Want { loads: loads.clone(), mass: request.mass, count: 0, at: Instant::now(), dragging: false }),
            stop: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            session: Mutex::new(None),
            thread: Mutex::new(None),
        });
        let mass = request.mass;
        let them = shared.clone();
        let line = Line { object: object.clone(), part: request.part.clone() };
        let said = line.clone();
        let spawned = std::thread::Builder::new().name("fab grow".into()).spawn(move || {
            *them.thread.lock().unwrap() = Some(std::thread::current());
            if let Err(e) = grow_on(&them, &hub, id, &file, &request, &root, &line, space, stored) {
                log!("fab: {} could not grow: {e}", request.part);
                line.say(format!("{} could not grow: {e}", request.part));
            }
            them.ended.store(true, Ordering::Relaxed);
        });
        if let Err(e) = spawned {
            said.say(format!("{} could not start growing: {e}", said.part));
        }
        Run { id, object, space, held: (loads.clone(), mass), loads, mass, dragging: false, shared }
    }

    /// Whether its thread gave up (no answer from the study or the AI hub): the next build starts
    /// another.
    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::Relaxed)
    }

    /// The loads or the mass a build has: the shape answers from where it stands. While a hand is on
    /// it, it answers the hand until that lets go (a build of its own new shape says nothing new).
    pub fn ask(&mut self, loads: Vec<Load>, mass: f64) {
        self.held = (loads.clone(), mass);
        if !self.dragging && (self.loads != loads || self.mass != mass) {
            self.change(loads, mass, false);
        }
    }

    /// The loads or the mass at a step of a drag: the coarse levels follow.
    pub fn drag(&mut self, loads: Vec<Load>, mass: f64) {
        if self.loads != loads || self.mass != mass {
            self.change(loads, mass, true);
        }
    }

    /// A drag is over, wherever it ended.
    fn release(&mut self) {
        if self.dragging {
            self.change(self.loads.clone(), self.mass, false);
        }
    }

    /// A drag called off: back to what the model holds.
    fn cancel(&mut self) {
        if self.dragging {
            let (loads, mass) = self.held.clone();
            self.change(loads, mass, false);
        }
    }

    fn change(&mut self, loads: Vec<Load>, mass: f64, dragging: bool) {
        self.loads = loads.clone();
        self.mass = mass;
        self.dragging = dragging;
        {
            let mut want = self.shared.want.lock().unwrap();
            want.loads = loads;
            want.mass = mass;
            want.dragging = dragging;
            want.count += 1;
            want.at = Instant::now();
        }
        self.wake();
    }

    fn wake(&self) {
        if let Some(session) = self.shared.session.lock().unwrap().as_ref() {
            session.wake();
        }
        if let Some(thread) = self.shared.thread.lock().unwrap().as_ref() {
            thread.unpark();
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.wake();
    }
}

/// A part's line in its panel.
#[derive(Clone)]
struct Line {
    object: String,
    part: String,
}

impl Line {
    fn say(&self, text: String) {
        Cx::post_action(HostAction::GrowLine(fab::params::GrowLine { object: self.object.clone(), part: self.part.clone(), text }));
    }
}

/// The runs of the model's grown parts, by file and part: what the model asks by each build, and a
/// live step by the value it was compiled at.
#[derive(Default)]
pub struct Runs {
    pub runs: Mutex<HashMap<(String, String), Run>>,
    /// The live step last let go: a step compiled before that no longer speaks for the hand. Read
    /// and raised only under `runs`' lock, so a step lands before the release that lets it go, or
    /// not at all: one that passed this check and took the lock after the release would hold the
    /// part as dragged with no hand on it, and nothing would let it go again.
    released: AtomicU64,
    look_again: AtomicBool,
}

impl Runs {
    /// A live step `gen`'s loads and mass for a part. Steps may come after the hand let go.
    pub fn drag(&self, gen: u64, key: &(String, String), loads: Vec<Load>, mass: f64) {
        let mut runs = self.runs.lock().unwrap();
        if gen <= self.released.load(Ordering::Relaxed) {
            return;
        }
        if let Some(run) = runs.get_mut(key) {
            run.drag(loads, mass);
        }
    }

    /// The hand let go after live step `gen`. The build of its commit says what the model holds,
    /// though it holds what it held (a commit refused).
    pub fn release(&self, gen: u64) {
        let mut runs = self.runs.lock().unwrap();
        self.released.fetch_max(gen, Ordering::Relaxed);
        for run in runs.values_mut() {
            run.release();
        }
        drop(runs);
        self.look_again.store(true, Ordering::Relaxed);
    }

    /// The hand's drag was called off after live step `gen`.
    pub fn cancel(&self, gen: u64) {
        let mut runs = self.runs.lock().unwrap();
        self.released.fetch_max(gen, Ordering::Relaxed);
        for run in runs.values_mut() {
            run.cancel();
        }
    }

    /// Whether a release wants the runs asked again, once.
    pub fn take_look_again(&self) -> bool {
        self.look_again.swap(false, Ordering::Relaxed)
    }
}

/// A grown part's loads in its body's frame, from the plate's own frames, so a move or a turn on the
/// plate changes nothing. None when the part has no one frame.
pub fn loads_of(field: &fab_model::eval::PartField, to_plate: &[[f64; 4]; 3], part: &str, study: &fab_model::study::Study) -> Option<Vec<Load>> {
    let body = fab_optimise::grown::body_to_object(&field.shape.root, part).ok()?;
    let to_shape = crate::paint_edit::inverse(&field.to_printer)?;
    let to_body = fab_model::weave::compose(&crate::paint_edit::inverse(&body)?, &fab_model::weave::compose(&to_shape, to_plate));
    let frame = crate::study_edit::Frame::new(&to_body)?;
    Some(crate::study_edit::loads(study, &frame).into_iter().map(|(l, _)| l).collect())
}

/// One of a run's sessions and what it has said.
struct Held {
    posed: Posed,
    session: Arc<RemoteSession>,
    seen: Updates,
    /// The shape last made into a grid.
    drawn: Option<(usize, u64)>,
    /// The rev of the edit last sent, and the change it was.
    sent: usize,
    answers: u64,
    loads: Vec<Load>,
    mass: f64,
}

impl Held {
    /// A session of `posed` on the hub. While the hub has it wait for room (it runs as many
    /// sessions as it may, and opens this one as one of them ends), `waits` hears why, once.
    #[allow(clippy::too_many_arguments)]
    fn open(shared: &Shared, hub: &Hub, posed: Posed, class: Class, loads: &[Load], mass: f64, answers: u64, waits: &mut dyn FnMut(&str)) -> Result<Held, String> {
        let (_, stream) = hub.first()?;
        let config = grow::config(posed.job(), None);
        let remote = RemoteConfig { params: config.params, inputs: config.inputs, limits: config.limits, class, ..RemoteConfig::default() };
        let mut heard = false;
        let session = RemoteSession::open_waiting(stream, &grow::program(), remote, &mut |queued| {
            if let Some(why) = queued.filter(|_| !std::mem::replace(&mut heard, true)) {
                waits(why);
            }
            shared.stop.load(Ordering::Relaxed)
        })
        .map_err(|e| format!("the AI hub did not take it: {e}"))?;
        let session = Arc::new(session.ok_or("stopped")?);
        Ok(Held { posed, session, seen: Updates::default(), drawn: None, sent: 0, answers, loads: loads.to_vec(), mass })
    }

    /// Sends a change: false when it holds or loads other nodes (the cells kept solid change, so
    /// the shape grows anew).
    fn ask(&mut self, loads: &[Load], mass: f64, change: u64) -> bool {
        let Some(edit) = self.posed.edit(loads, mass) else { return false };
        self.sent = self.session.send("edit", edit.value()) as usize;
        self.answers = change;
        self.loads = loads.to_vec();
        self.mass = mass;
        true
    }

    fn settled(&self) -> bool {
        grow::settled(&self.seen, self.sent).is_some()
    }

    fn level(&self) -> usize {
        grow::latest(&self.seen).map_or(0, |i| i.level)
    }

    /// Its level's iterations are done, as far as it has said.
    fn level_done(&self) -> bool {
        self.settled() || grow::latest(&self.seen).is_some_and(|i| self.posed.level_done(i.level, i.it))
    }

    /// The shape it stands at, as a project keeps it.
    fn stored(&self, whole_level: usize, space: u64) -> Option<Stored> {
        let at = grow::latest(&self.seen)?;
        let xp = self.seen.output(&format!("xp{}", at.level))?.data.f32s()?;
        let (n, h, origin, solid) = self.posed.cells(at.level, self.mass);
        Some(Stored::new(inputs_hash(space, &self.loads, self.mass), space, whole_level as u32, n, h, origin, solid, at.compliance, &xp))
    }

    /// Its last solve never found the forces, so the shape does not answer them.
    fn unsolved(&self) -> bool {
        grow::latest(&self.seen).is_some_and(|i| i.residual > UNSOLVED)
    }

    /// The newest shape it has not made into a grid, for the change last sent.
    fn shape(&mut self) -> Option<(usize, Arc<SdfGrid>)> {
        let level = self.level();
        let output = self.seen.output(&format!("xp{level}"))?;
        if self.drawn == Some((level, output.version)) || (output.rev as usize) < self.sent {
            return None;
        }
        self.drawn = Some((level, output.version));
        let xp = output.data.f32s()?;
        Some((level, Arc::new(self.posed.grid(level, &xp, self.mass))))
    }

    /// The level the hub had no room for, by its place among this session's levels.
    fn refused(&self) -> Option<usize> {
        grow::no_room(&self.seen)
    }
}

/// What the part shows, and how the next shape gets there.
struct Screen {
    file: String,
    part: String,
    run: u64,
    slot: Slot,
    shown: Option<Arc<SdfGrid>>,
    /// The level it shows (a level by its place among all of the part's).
    level: usize,
    fade: Option<Fading>,
    stepped: Option<Instant>,
    /// What shows is a level done, and has been told so.
    sliced: bool,
}

struct Fading {
    mix: Fade,
    to: Arc<SdfGrid>,
    settled: bool,
    at: Instant,
    steps: u32,
}

impl Screen {
    /// A shape of `level`. One from another level, or that a preview gives way to (`fades`), fades
    /// in; another of the level showing replaces it, or what a fade is on the way to.
    fn show(&mut self, grid: Arc<SdfGrid>, level: usize, settled: bool, fades: bool) {
        let new = fades || level != self.level;
        self.level = level;
        if let Some(shown) = self.shown.as_ref().filter(|_| new) {
            self.fade = Some(Fading { mix: Fade::new(shown, &grid), to: grid, settled, at: Instant::now(), steps: 0 });
            self.stepped = None;
        } else if let Some(fade) = &mut self.fade {
            fade.mix.retarget(&grid);
            fade.to = grid;
            fade.settled |= settled;
        } else {
            self.present(grid, settled);
        }
    }

    /// Takes a fade a step on, when the model took the last.
    fn step(&mut self) {
        let Some(fade) = &mut self.fade else { return };
        if self.slot.lock().unwrap().is_some() || self.stepped.is_some_and(|s| s.elapsed() < FADE_STEP) {
            return;
        }
        // as far as the time says, but in steps a slow model can show
        let t = (fade.at.elapsed().as_secs_f32() / FADE.as_secs_f32()).min((fade.steps + 1) as f32 / FADE_STEPS as f32);
        fade.steps += 1;
        if t >= 1.0 {
            let fade = self.fade.take().expect("a fade");
            self.present_as(fade.to, fade.settled, true);
        } else {
            let grid = Arc::new(fade.mix.at(t));
            self.stepped = Some(Instant::now());
            self.present(grid, false);
        }
    }

    /// The level showing is done, though its last shape may already have gone: a slice follows it.
    fn settle(&mut self) {
        if let Some(fade) = &mut self.fade {
            fade.settled = true;
        } else if let Some(shown) = self.shown.clone().filter(|_| !self.sliced) {
            self.present(shown, true);
        }
    }

    fn present(&mut self, grid: Arc<SdfGrid>, settled: bool) {
        self.present_as(grid, settled, false);
    }

    fn present_as(&mut self, grid: Arc<SdfGrid>, settled: bool, faded: bool) {
        self.shown = Some(grid.clone());
        self.sliced = settled;
        let was = self.slot.lock().unwrap().replace(Newest { grid, settled, faded });
        match was {
            None => Cx::post_action(HostAction::Grown(Box::new(Shown { file: self.file.clone(), part: self.part.clone(), run: self.run, slot: self.slot.clone() }))),
            // what it replaces was done and not sliced yet: this one is what gets sliced
            Some(was) if was.settled => {
                if let Some(newest) = self.slot.lock().unwrap().as_mut() {
                    newest.settled = true;
                }
            }
            Some(_) => {}
        }
    }
}

/// A cell's size as a row says it.
fn mm(h: f64) -> String {
    format!("{} mm", (h * 10.0).round() / 10.0)
}

/// The run's thread: until it stops, grows the part on the AI hub, and while none answers (or the
/// one it had quit) says so and looks for one.
#[allow(clippy::too_many_arguments)]
fn grow_on(shared: &Shared, hub: &SharedHub, run: u64, file: &str, request: &GrowRequest, root: &SdfNode, line: &Line, space: u64, stored: Option<(Arc<Stored>, Arc<SdfGrid>)>) -> Result<(), String> {
    // A shape kept that answers what is asked is the part: nothing runs, and no hub is needed, until
    // something else is asked.
    let (asked, answers) = {
        let want = shared.want.lock().unwrap();
        (want.count, stored.as_ref().is_some_and(|(s, _)| s.inputs == inputs_hash(space, &want.loads, want.mass)))
    };
    if let Some((s, _)) = stored.as_ref().filter(|_| answers) {
        line.say(format!("{}: settled {}", request.part, mm(s.h as f64)));
        while !shared.stop.load(Ordering::Relaxed) && shared.want.lock().unwrap().count == asked {
            std::thread::park();
        }
    }
    let regrowing = !answers && stored.is_some();
    if regrowing {
        log!("fab: {} inputs changed: regrowing", request.part);
    }
    let stored = stored.filter(|(s, _)| s.space == space);
    let mut waiting = false;
    while !shared.stop.load(Ordering::Relaxed) {
        let Some(open) = hub.open() else {
            if !waiting {
                line.say(NO_HUB.into());
                waiting = true;
            }
            std::thread::park_timeout(LOOK_AGAIN);
            continue;
        };
        if waiting {
            line.say(String::new());
            waiting = false;
        }
        match grow_with(shared, &open, run, file, request, root, line, space, stored.as_ref().map(|(s, g)| (s.level as usize, g.clone())), regrowing) {
            Err(_) if !open.is_open() => continue,
            done => return done,
        }
    }
    Ok(())
}

/// Why a run has no refined levels.
#[derive(Clone, PartialEq)]
enum Unrefined {
    /// Not (yet): the coarse levels are still going, or a hand is on it.
    Waiting,
    /// The hub has no room for the first refined level.
    NoRoom,
    Failed(String),
}

/// The refined shape last shown, the level it is of and the change it answers.
struct Kept {
    grid: Arc<SdfGrid>,
    level: usize,
    answers: u64,
}

/// Poses the part, grows it on this hub, and answers each change.
#[allow(clippy::too_many_arguments)]
fn grow_with(shared: &Shared, hub: &Hub, run: u64, file: &str, request: &GrowRequest, root: &SdfNode, line: &Line, space: u64, stored: Option<(usize, Arc<SdfGrid>)>, mut regrowing: bool) -> Result<(), String> {
    let (mut answered, mut asked, loads, mass, mut dragging) = {
        let want = shared.want.lock().unwrap();
        (want.count, want.at, want.loads.clone(), want.mass, want.dragging)
    };
    let started = Instant::now();
    let requested = |mass: f64| GrowRequest { mass, ..request.clone() };
    let mut screen = Screen { file: file.to_string(), part: request.part.clone(), run, slot: Slot::default(), shown: None, level: 0, fade: None, stepped: None, sliced: false };
    let mut coarse = Held::open(shared, hub, Posed::new(root, &requested(mass), &loads)?, Class::Interactive, &loads, mass, answered, &mut |why| line.say(waits_for_room(&request.part, why)))?;
    *shared.session.lock().unwrap() = Some(coarse.session.clone());
    let last_coarse = coarse.posed.levels() - 1;
    let mut fine: Option<Held> = None;
    // the refined levels posed, between their sessions, or on their way
    let mut spare: Option<Posed> = None;
    let mut posing: Option<std::thread::JoinHandle<Result<Option<Posed>, String>>> = None;
    let mut unrefined = Unrefined::Waiting;
    // the newest coarse shape; the one it settled at, by the change it answers; the refined shape
    // showing; and the coarse shape of the change that one answers, which a preview moves it from
    let mut coarse_shape: Option<(usize, Arc<SdfGrid>)> = None;
    let mut rest: Option<(u64, Arc<SdfGrid>)> = None;
    // a shape kept stands until a refined one comes, or a hand wants it to follow (from what the
    // coarse levels never answered, nothing can move it)
    let mut kept: Option<Kept> = stored.map(|(level, grid)| {
        screen.shown = Some(grid.clone());
        screen.level = level;
        screen.sliced = true;
        Kept { grid, level, answers: u64::MAX }
    });
    let mut base: Option<Arc<SdfGrid>> = None;
    // the change whose shape is kept
    let mut settled_for = None;
    let mut said = String::new();
    // for the log: a change waits on its first shape, and a session has said it settled
    let (mut waiting, mut coarse_said, mut fine_said) = (true, false, false);
    let part = request.part.as_str();

    while !shared.stop.load(Ordering::Relaxed) {
        let (change, loads, mass, at, drag) = {
            let want = shared.want.lock().unwrap();
            (want.count, want.loads.clone(), want.mass, want.at, want.dragging)
        };
        if change != answered {
            answered = change;
            asked = at;
            dragging = drag;
            waiting = true;
            coarse_said = false;
            fine_said = false;
            if !coarse.ask(&loads, mass, change) {
                // other nodes hold or load it: the cells kept solid change, so it grows anew
                let Held { posed, .. } = coarse;
                coarse = Held::open(shared, hub, posed.with_loads(&loads, mass)?, Class::Interactive, &loads, mass, change, &mut |why| {
                    said = waits_for_room(part, why);
                    line.say(said.clone());
                })?;
                *shared.session.lock().unwrap() = Some(coarse.session.clone());
                coarse_shape = None;
                rest = None;
                base = None;
                kept = None;
                if let Some(f) = fine.take() {
                    spare = Some(f.posed);
                }
            }
            if matches!(unrefined, Unrefined::Failed(_)) {
                unrefined = Unrefined::Waiting;
            }
            // a shape still climbing its levels starts again from what the coarse levels settle
            // at; a settled one takes a change when it is let go
            if fine.as_ref().is_some_and(|f| f.sent == 0 && !f.settled()) {
                spare = fine.take().map(|f| f.posed);
            }
            if !drag {
                if let Some(f) = &mut fine {
                    if f.loads == loads && f.mass == mass {
                        // The refined levels were last asked these loads, so what they stand at, or
                        // the answer still on its way, is this change's. Only one they stand at is
                        // said here; one on its way is said and kept when it settles (changes can
                        // come back to what was asked before it settles, past ones never seen here).
                        f.answers = change;
                        if f.settled() {
                            // let go where it began: the shape it had is the answer
                            if let Some(k) = &mut kept {
                                k.answers = change;
                                screen.show(k.grid.clone(), k.level, false, true);
                            }
                            fine_said = true;
                            settled_for = Some(change);
                        }
                    } else if !f.ask(&loads, mass, change) {
                        spare = fine.take().map(|f| f.posed);
                    }
                }
            }
        }

        // One wait, on what is coming next.
        let fine_busy = fine.as_ref().is_some_and(|f| !f.settled());
        let timed = screen.fade.is_some() || posing.is_some();
        match &mut fine {
            Some(f) if fine_busy => {
                let u = f.session.wait_within(LOOK);
                f.seen.merge(u);
                coarse.seen.merge(coarse.session.poll());
            }
            _ => {
                let u = if timed { coarse.session.wait_within(LOOK) } else { coarse.session.wait() };
                coarse.seen.merge(u);
            }
        }
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(end) = &coarse.seen.end {
            return Err(format!("its session ended: {end:?}"));
        }
        if let Some(end) = fine.as_ref().and_then(|f| f.seen.end.clone()) {
            match end {
                Err(ProgramError::Lost(why)) => return Err(why),
                Ok(_) => unrefined = Unrefined::NoRoom,
                Err(e) => unrefined = Unrefined::Failed(e.to_string()),
            }
            spare = fine.take().map(|f| f.posed);
        }

        // The coarse levels grew anew: nothing refined speaks for this shape.
        if coarse.sent > 0 && coarse.level() == 0 && kept.is_some() {
            kept = None;
            base = None;
            rest = None;
            spare = fine.take().map(|f| f.posed).or(spare);
        }

        if posing.as_ref().is_some_and(|p| p.is_finished()) {
            match posing.take().expect("a posing").join() {
                Ok(Ok(posed)) => spare = posed,
                Ok(Err(e)) => unrefined = Unrefined::Failed(e),
                Err(_) => unrefined = Unrefined::Failed("posing its finer levels panicked".into()),
            }
        }

        // The coarse levels' shape: the shape itself until there is a refined one, then what moves
        // the refined one, until a refined shape answers the change.
        if let Some((level, grid)) = coarse.shape() {
            if std::mem::take(&mut waiting) {
                log!("fab: {part} shape for its change after {:.1} ms", asked.elapsed().as_secs_f64() * 1e3);
            }
            coarse_shape = Some((level, grid.clone()));
            if dragging && base.is_none() {
                kept = None;
            }
            match (&kept, &base) {
                (Some(k), Some(b)) if k.answers != answered && level == last_coarse && b.dims == grid.dims => {
                    screen.show(Arc::new(preview(&k.grid, b, &grid)), k.level, false, false);
                }
                (Some(_), _) => {}
                (None, _) => screen.show(grid, level, !dragging && coarse.level_done(), false),
            }
        }
        if kept.is_none() && !dragging && coarse.level_done() {
            screen.settle();
        }
        if coarse.settled() {
            if !std::mem::replace(&mut coarse_said, true) {
                log!("fab: {part} coarse settled at {} after {:.1} ms", mm(coarse.posed.h(last_coarse)), started.elapsed().as_secs_f64() * 1e3);
            }
            if rest.as_ref().is_none_or(|r| r.0 != coarse.answers) {
                if let Some((_, grid)) = coarse_shape.as_ref().filter(|(level, _)| *level == last_coarse) {
                    rest = Some((coarse.answers, grid.clone()));
                }
            }
        }

        // The refined levels start once the coarse ones settled what was last asked.
        if !dragging && fine.is_none() && posing.is_none() && unrefined == Unrefined::Waiting && coarse.settled() && coarse.answers == answered {
            match spare.take() {
                Some(posed) => match coarse.seen.output("x").and_then(|o| o.data.f32s()) {
                    Some(x) => {
                        let opened = posed.refining(&coarse.posed, &loads, mass, &x).and_then(|posed| {
                            Held::open(shared, hub, posed, Class::Visible, &loads, mass, answered, &mut |why| {
                                said = waits_for_room(part, why);
                                line.say(said.clone());
                            })
                        });
                        match opened {
                            Ok(f) => {
                                log!("fab: {part} refining from {} after {:.1} ms", mm(coarse.posed.h(last_coarse)), started.elapsed().as_secs_f64() * 1e3);
                                fine = Some(f);
                            }
                            Err(e) if !hub.is_open() => return Err(e),
                            Err(e) => unrefined = Unrefined::Failed(e),
                        }
                    }
                    None => unrefined = Unrefined::Failed("its coarse levels gave no density".into()),
                },
                None if coarse.posed.refines() => {
                    let (root, request, loads) = (root.clone(), requested(mass), loads.clone());
                    let spawned = std::thread::Builder::new().name("fab grow pose".into()).spawn(move || Posed::refinement(&root, &request, &loads));
                    match spawned {
                        Ok(handle) => posing = Some(handle),
                        Err(e) => unrefined = Unrefined::Failed(e.to_string()),
                    }
                }
                None => {}
            }
        }

        // The refined levels' shape: every look while they climb, and once they stand, only what
        // they settle at for a change (their first looks at it are behind what the preview shows).
        if let Some(f) = fine.as_mut().filter(|f| f.answers == answered) {
            let warm = f.sent > 0;
            let settled = f.settled();
            if !warm || settled {
                if let Some((level, grid)) = f.shape() {
                    let whole_level = f.posed.first_level() + level;
                    kept = Some(Kept { grid: grid.clone(), level: whole_level, answers: f.answers });
                    screen.show(grid, whole_level, f.level_done(), warm);
                }
                if !warm && f.level_done() {
                    screen.settle();
                }
            }
            if settled && !std::mem::replace(&mut fine_said, true) {
                log!("fab: {part} settled at {} after {:.1} ms", mm(f.posed.h(f.level())), started.elapsed().as_secs_f64() * 1e3);
            }
        }
        if let (Some((c, grid)), Some(k)) = (&rest, &kept) {
            if *c == k.answers {
                base = Some(grid.clone());
            }
        }

        screen.step();

        // What it stands at, once the screen shows it, is what a project keeps and what its panel
        // calls settled: while the screen still fades to it, what shows (and the size the plate
        // gives) is a frame of the fade.
        let standing = match &fine {
            Some(f) if f.settled() => Some((f, f.posed.first_level() + f.level())),
            None if coarse.settled() && (!coarse.posed.refines() || unrefined != Unrefined::Waiting) => Some((&coarse, coarse.level())),
            _ => None,
        };
        let stands = standing.filter(|(held, _)| !dragging && held.answers == answered);
        let shown = screen.fade.is_none();
        if let Some((held, whole_level)) = stands.filter(|_| shown) {
            regrowing = false;
            if settled_for != Some(answered) {
                settled_for = Some(answered);
                if let Some(stored) = held.stored(whole_level, space) {
                    Cx::post_action(HostAction::GrownSettled(Box::new(Settled { file: file.to_string(), part: part.to_string(), run, stored: Arc::new(stored) })));
                }
            }
        }

        let row = if regrowing && !dragging { format!("{part}: inputs changed: regrowing") } else { row_of(part, &coarse, fine.as_ref(), &unrefined, dragging, posing.is_some()) };
        if row != said && (shown || stands.is_none()) {
            line.say(row.clone());
            said = row;
        }
    }
    Ok(())
}

/// What a grown part's panel says of how it goes.
/// A part's row while the hub has its open wait for room: it grows once the hub opens it, it is no
/// failure (`why`, the hub's word on it, goes to the log).
fn waits_for_room(part: &str, why: &str) -> String {
    log!("fab: {part} waits for the AI hub: {why}");
    format!("{part}: waiting for room on the AI hub")
}

fn row_of(part: &str, coarse: &Held, fine: Option<&Held>, unrefined: &Unrefined, dragging: bool, posing: bool) -> String {
    let coarsest = coarse.posed.levels() - 1;
    let finished = mm(coarse.posed.h(coarsest));
    let next = mm(0.5 * coarse.posed.h(coarsest));
    let state = match (unrefined, fine) {
        _ if dragging => format!("following at {finished}"),
        (Unrefined::NoRoom, _) => format!("settled {finished}: the AI hub has no room for {next}"),
        (Unrefined::Failed(e), _) => format!("settled {finished}: could not refine: {e}"),
        (_, Some(f)) if f.settled() => {
            let level = f.level();
            match f.refused() {
                Some(refused) => format!("settled {}: the AI hub has no room for {}", mm(f.posed.h(level)), mm(f.posed.h(refused))),
                None => format!("settled {}", mm(f.posed.h(level))),
            }
        }
        (_, Some(f)) if f.sent > 0 => format!("refining {}", mm(f.posed.h(f.level()))),
        (_, Some(f)) => {
            let level = f.level();
            let from = if level == 0 { finished } else { mm(f.posed.h(level - 1)) };
            format!("refining {from} → {}", mm(f.posed.h(level)))
        }
        (_, None) if coarse.settled() && (posing || coarse.posed.refines()) => format!("refining {finished} → {next}"),
        (_, None) if coarse.settled() => format!("settled {finished}"),
        (_, None) => match coarse.level() {
            0 => format!("growing {}", mm(coarse.posed.h(0))),
            level => format!("refining {} → {}", mm(coarse.posed.h(level - 1)), mm(coarse.posed.h(level))),
        },
    };
    let standing = match fine {
        Some(f) if f.settled() => Some(f),
        None if coarse.settled() => Some(coarse),
        _ => None,
    };
    if standing.is_some_and(Held::unsolved) {
        return format!("{part}: {state}. Its force solve did not finish, so do not rely on this shape.");
    }
    format!("{part}: {state}")
}
