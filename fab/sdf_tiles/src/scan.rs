//! The scan: with nothing else to do, each part is looked at from a fixed set of views over the
//! hemisphere above the bed, so an orbit finds it there already. Rays only, as any view's: a
//! lattice's inside gets geometry only where a ray from outside lands. The light's view goes first:
//! the shadow draws from the mesh, and an edit's look is only from the camera.
//!
//! One queue for the parts, one low-priority job on it, one round of one part at a time, its looks
//! spread over the pool at the job's class. Any input, camera move, edit or slice stops it at once
//! and what it was doing is owed again; an edit starts that part's scan over and leaves the
//! others'. With every part done nothing runs at all.

use super::*;
use makepad_draw::makepad_platform::SignalToUI;
use std::sync::Weak;
use std::time::Duration;

/// Views over the hemisphere. By what an orbit to a side not seen yet still finds missing: 26 leave
/// a quarter to two thirds of a percent bare on a vase, a knob and lattices, against 18-34% with one
/// settled view and no scan. 42 take half as long again and leave 0.2-0.5%.
const VIEWS: usize = 26;
/// How many pixels across a part is in those views. 160 takes about two thirds of the time and
/// leaves about twice as much bare.
const ACROSS: f64 = 240.0;
const WIDE: f64 = 1000.0;
const HIGH: f64 = 640.0;
/// No input for this long and it goes on.
pub const REST: Duration = Duration::from_millis(300);
/// A part's scan ends here, finished or not: 96 bytes a quad.
const MOST_QUADS: usize = 2_000_000;
/// What a scan's views find goes out when it ends, and before that at most this often: the view
/// records again for each batch, so a long scan shows what it finds as it goes and a short one
/// once.
const SEND_EVERY: Duration = Duration::from_millis(250);

/// One part's scan.
pub(super) struct Scan {
    views: Vec<Cam>,
    at: usize,
    gaze: Gaze,
    began: Instant,
    evals: u64,
    /// What its whole views found, each looked over again by the view on screen, waiting for its
    /// batch; the view in hand's finds are in the part's `unsent` till then.
    ready: Vec<(usize, Option<Chunk>)>,
    /// Its last batch went out then (or it began).
    sent: Instant,
}

impl Scan {
    /// What its whole views found goes out to the view (the frames that draw it stop with any
    /// input), unless the shape is no longer the one it scanned.
    fn send_ready(&mut self, part: &Shared, key: [u8; 32], version: u64) {
        if !self.ready.is_empty() && part.version.load(Ordering::Acquire) == version {
            part.send(last_of_each(std::mem::take(&mut self.ready)), key, false);
            self.sent = Instant::now();
        }
    }
}

/// The views over the hemisphere around `up` that the box `lo`..`hi` is seen from, nearest to where
/// `from` looks from first. Before them the light's, along its parallel rays, when it is above the
/// bed: `to_light` is the way to it.
fn views(lo: V, hi: V, from: &Cam, up: V, to_light: Option<V>) -> Vec<Cam> {
    let size = (0..3).map(|i| hi[i] - lo[i]).fold(0.0, f64::max);
    let at = mul(add(lo, hi), 0.5);
    let half = 20f64.to_radians().tan();
    let dist = size / (ACROSS / HIGH * 2.0 * half);
    let up = norm(up);
    let side = norm(cross(up, if up[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] }));
    let other = cross(up, side);
    let mut dirs: Vec<V> = (0..VIEWS)
        .map(|i| {
            let height = (i as f64 + 0.5) / VIEWS as f64;
            let round = (1.0 - height * height).sqrt();
            let turn = i as f64 * 2.399963229728653;
            add(add(mul(side, round * turn.cos()), mul(other, round * turn.sin())), mul(up, height))
        })
        .collect();
    let back = mul(from.f, -1.0);
    dirs.sort_by(|a, b| dot(*b, back).total_cmp(&dot(*a, back)));
    let view = |d: V, ortho: bool| {
        let f = mul(d, -1.0);
        let r = norm(cross(f, if dot(d, up).abs() > 0.99 { side } else { up }));
        let half = if ortho { half * dist } else { half };
        Cam { eye: add(at, mul(d, dist)), f, r, u: cross(r, f), half: [half * WIDE / HIGH, half], off: [0.0; 2], ortho, w: WIDE, h: HIGH }
    };
    let light = to_light.map(norm).filter(|l| dot(*l, up) > 0.0).map(|l| view(l, true));
    light.into_iter().chain(dirs.into_iter().map(|d| view(d, false))).collect()
}

/// Each chunk's last version alone, in their order: the view sets them in order, the last
/// standing, so it draws the same; the ones it would only replace are dropped here, not on the UI
/// thread.
fn last_of_each(chunks: Vec<(usize, Option<Chunk>)>) -> Vec<(usize, Option<Chunk>)> {
    let mut seen = vec![false; CHUNKS];
    let mut last: Vec<(usize, Option<Chunk>)> = chunks.into_iter().rev().filter(|(i, _)| !std::mem::replace(&mut seen[*i], true)).collect();
    last.reverse();
    last
}

pub(super) struct Queue {
    parts: Mutex<Vec<Weak<Shared>>>,
    /// Moves with each input: a round begun before it stops.
    input: AtomicU64,
    /// When the last input came; none once its rest was ended by hand (a test's).
    last_input: Mutex<Option<Instant>>,
    /// How long an input rests the scan: [`REST`] (a test's queue rests until it ends the rest).
    rest: Duration,
    /// A slice is running: nothing else may scan.
    blocked: AtomicBool,
    /// A job is on the pool.
    running: AtomicBool,
    /// It would have gone on but the input has not rested: the UI's timer brings it back.
    waiting: AtomicBool,
    /// Rounds run, since the start: the scan's work, counted where a clock would vary (what a
    /// test's wait for the job is bounded by).
    rounds: AtomicU64,
}

/// The parts' scans, one queue a viewport.
#[derive(Clone)]
pub struct ScanQueue(Arc<Queue>);

impl Default for ScanQueue {
    fn default() -> Self {
        ScanQueue(Arc::new(Queue::new(REST)))
    }
}

impl ScanQueue {
    /// The user did something: what is running stops, and the scan rests a while.
    pub fn input(&self) {
        self.0.input_now();
    }

    /// While a slice runs nothing is scanned; when it ends the scan goes on.
    pub fn set_blocked(&self, blocked: bool, pool: &TaskPool) {
        let was = self.0.blocked.swap(blocked, Ordering::AcqRel);
        if blocked && !was {
            self.0.input.fetch_add(1, Ordering::AcqRel);
        }
        if was && !blocked {
            self.0.kick(pool);
        }
    }

    /// How long until the input has rested, none: it has. The caller's timer then calls [`ScanQueue::kick`].
    pub fn rest_left(&self) -> Option<Duration> {
        self.0.resting_for().map(|el| self.0.rest - el)
    }

    /// Starts on the queue if there is anything on it.
    pub fn kick(&self, pool: &TaskPool) {
        self.0.kick(pool);
    }

    /// The queue wanted to go on before the input had rested: the caller's timer is owed.
    pub fn take_waiting(&self) -> bool {
        self.0.waiting.swap(false, Ordering::AcqRel)
    }

    pub(super) fn add(&self, part: &Arc<Shared>) {
        self.0.parts.lock().unwrap().push(Arc::downgrade(part));
    }

    pub(super) fn queue(&self) -> Arc<Queue> {
        self.0.clone()
    }
}

impl Queue {
    /// Resting from its start, as if an input came then.
    fn new(rest: Duration) -> Queue {
        Queue {
            parts: Mutex::new(Vec::new()),
            input: AtomicU64::new(0),
            last_input: Mutex::new(Some(Instant::now())),
            rest,
            blocked: AtomicBool::new(false),
            running: AtomicBool::new(false),
            waiting: AtomicBool::new(false),
            rounds: AtomicU64::new(0),
        }
    }

    pub(super) fn input_now(&self) {
        *self.last_input.lock().unwrap() = Some(Instant::now());
        self.input.fetch_add(1, Ordering::AcqRel);
    }

    /// The time since the last input, while it has not rested.
    fn resting_for(&self) -> Option<Duration> {
        self.last_input.lock().unwrap().map(|t| t.elapsed()).filter(|el| *el < self.rest)
    }

    /// A part came to rest before the input did, with nothing to wake this again.
    fn wait_for_rest(&self) {
        self.waiting.store(true, Ordering::Release);
        SignalToUI::set_ui_signal();
    }

    pub(super) fn kick(self: &Arc<Self>, pool: &TaskPool) {
        if self.blocked.load(Ordering::Acquire) {
            return;
        }
        if self.resting_for().is_some() {
            self.wait_for_rest();
            return;
        }
        // an empty queue costs nothing, not even a job to find it empty
        if self.next().is_some() && !self.running.swap(true, Ordering::AcqRel) {
            self.submit(pool);
        }
    }

    fn submit(self: &Arc<Self>, pool: &TaskPool) {
        let (q, worker_pool) = (self.clone(), pool.clone());
        if pool.submit_named(Class::Background, "sdf scan", move || q.slice(worker_pool)).is_err() {
            self.running.store(false, Ordering::Release);
        }
    }

    /// The part to go on with: the selected one, then those in view, then the rest.
    fn next(&self) -> Option<Arc<Shared>> {
        let mut parts = self.parts.lock().unwrap();
        parts.retain(|p| p.strong_count() > 0);
        let mut best: Option<(u8, Arc<Shared>)> = None;
        for part in parts.iter().filter_map(Weak::upgrade) {
            let rank = {
                let w = part.want.lock().unwrap();
                if !w.scan_wanted() {
                    continue;
                }
                let kept = w.kept.as_ref().and_then(|k| k.field.as_ref());
                match (w.selected, w.cam, kept) {
                    (true, _, _) => 0,
                    (_, Some(cam), Some(f)) if cam.sees(mul(add(f.lo, f.hi), 0.5), len(sub(f.hi, f.lo)) * 0.5) => 1,
                    _ => 2,
                }
            };
            if best.as_ref().is_none_or(|b| rank < b.0) {
                best = Some((rank, part));
            }
        }
        best.map(|b| b.1)
    }

    /// One round of one part, then the queue again.
    fn slice(self: Arc<Self>, pool: TaskPool) {
        let input = self.input.load(Ordering::Acquire);
        let resting = self.resting_for().is_none();
        let part = if resting && !self.blocked.load(Ordering::Acquire) { self.next() } else { None };
        let Some(part) = part else {
            self.running.store(false, Ordering::Release);
            if !resting {
                self.wait_for_rest();
            } else if self.next().is_some() && !self.blocked.load(Ordering::Acquire) && !self.running.swap(true, Ordering::AcqRel) {
                // a part that came to rest as this found none, whose kick saw this still running
                self.submit(&pool);
            }
            return;
        };
        self.rounds.fetch_add(1, Ordering::Relaxed);
        let stopped = !self.round(&part, input, &pool);
        if stopped && LOG_PASSES.load(Ordering::Relaxed) {
            makepad_draw::makepad_platform::log!("sdf scan stopped {:.1} ms after the input", self.last_input.lock().unwrap().map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0));
        }
        // an input the pool sees again after its rest; nothing else sets this job going
        if stopped {
            self.running.store(false, Ordering::Release);
            self.wait_for_rest();
            return;
        }
        self.submit(&pool);
    }

    /// One round of `part`'s scan, its looks spread over `pool` at the job's class (Background: any
    /// other work goes first). Spread or not, a look finds the same, piece for piece. False:
    /// something stopped it.
    fn round(&self, part: &Arc<Shared>, input: u64, pool: &TaskPool) -> bool {
        let (mut kept, version, key, up, to_light, cam, eyes) = {
            let mut w = part.want.lock().unwrap();
            if !w.scan_wanted() {
                return true;
            }
            w.scanning = true;
            (w.kept.take().unwrap(), part.version.load(Ordering::Acquire), w.key, w.up, w.to_light, w.cam.unwrap(), w.eyes)
        };
        let field = kept.field.clone().unwrap();
        let stale = || self.input.load(Ordering::Relaxed) != input || part.version.load(Ordering::Relaxed) != version || self.blocked.load(Ordering::Relaxed);
        let scan = kept.scan.get_or_insert_with(|| Scan { views: views(field.lo, field.hi, &cam, up, to_light), at: 0, gaze: Gaze::default(), began: Instant::now(), evals: field.evals.load(Ordering::Relaxed), ready: Vec::new(), sent: Instant::now() });
        let step = scan.gaze.step(kept.surface.as_mut().unwrap(), &field, pool, scan.views[scan.at], &MOVING, &stale);
        let mut went_on = step.is_some();
        let mut finished = false;
        if let Some(step) = step {
            kept.unsent.extend(step.chunks);
            // a view's worth goes out once the view on screen has looked again over it, to its end:
            // a cell the scan split moves the mesh's edges round it, which can stride over a wall
            // thinner than the cells beside it where the view had it whole
            if step.done && !kept.unsent.is_empty() {
                let looks: Vec<(&mut Gaze, Cam)> = match eyes {
                    Some(e) => kept.eye_gazes.iter_mut().zip(e).collect(),
                    None => vec![(&mut kept.gaze, cam)],
                };
                'looks: for (gaze, at) in looks {
                    loop {
                        let Some(again) = gaze.step(kept.surface.as_mut().unwrap(), &field, pool, at, &STILL, &stale) else {
                            went_on = false;
                            break 'looks;
                        };
                        kept.unsent.extend(again.chunks);
                        if again.done {
                            break;
                        }
                    }
                }
            }
            // stopped, the round goes again: the scan's look at this view finds it done
            if step.done && went_on {
                scan.at += 1;
                scan.ready.append(&mut kept.unsent);
                let held = kept.surface.as_ref().unwrap().held();
                finished = scan.at == scan.views.len() || held.quads >= MOST_QUADS;
                if finished && LOG_PASSES.load(Ordering::Relaxed) {
                    makepad_draw::makepad_platform::log!(
                        "sdf scan done: {} of {} views, {:.1} ms, {} evals; {} cells {} quads, cpu {:.1} MB, gpu {:.1} MB",
                        scan.at,
                        scan.views.len(),
                        scan.began.elapsed().as_secs_f64() * 1000.0,
                        field.evals.load(Ordering::Relaxed) - scan.evals,
                        held.cells,
                        held.quads,
                        held.cpu_bytes as f64 / 1e6,
                        held.gpu_bytes as f64 / 1e6
                    );
                }
                // whole views' worth at a time, which wakes the view to record it: when the scan
                // ends, and before that at most every SEND_EVERY
                if finished || scan.sent.elapsed() >= SEND_EVERY {
                    scan.send_ready(part, key, version);
                }
            }
        }
        // stopped: what its whole views found shows now, as it would have before the stop; the
        // view in hand's goes on with it, or with the view's own next look
        if !went_on {
            scan.send_ready(part, key, version);
        }
        if finished {
            kept.scan = None;
        }
        let mut w = part.want.lock().unwrap();
        w.scanning = false;
        w.scan_done |= finished && part.version.load(Ordering::Acquire) == version;
        w.kept = Some(kept);
        // what the view wanted while this held the part
        if std::mem::take(&mut w.owed) && !w.cancelled && !w.failed {
            w.running = true;
            let mesh = w.quiet;
            drop(w);
            if part.start(&pool, mesh).is_err() {
                let mut w = part.want.lock().unwrap();
                w.running = false;
                w.failed = true;
            }
        }
        went_on
    }
}

impl Want {
    /// Its view is done and its scan is not.
    fn scan_wanted(&self) -> bool {
        self.converged && self.scannable && !self.running && !self.scanning && !self.failed && !self.cancelled && !self.scan_done && self.kept.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The views are all above the bed, all look at the part, and the nearest to the eye is first.
    #[test]
    fn the_views_cover_the_hemisphere_nearest_first() {
        let (lo, hi) = ([-10.0, -20.0, 0.0], [10.0, 20.0, 30.0]);
        let from = Cam::looking([100.0, 0.0, 5.0], [0.0, 0.0, 15.0], 800.0, 500.0);
        let up = [0.0, 0.0, 1.0];
        let v = views(lo, hi, &from, up, None);
        assert_eq!(v.len(), VIEWS);
        let at = mul(add(lo, hi), 0.5);
        let back = mul(from.f, -1.0);
        let closeness = |c: &Cam| dot(norm(sub(c.eye, at)), back);
        assert!(v.windows(2).all(|w| closeness(&w[0]) >= closeness(&w[1])));
        for c in &v {
            assert!(c.eye[2] > at[2], "a view from below the bed");
            assert!(dot(norm(sub(at, c.eye)), c.f) > 0.999);
        }
        // no direction above the bed is far from a view
        for (a, e) in [(0.0f64, 5.0f64), (90.0, 30.0), (200.0, 60.0), (300.0, 85.0)] {
            let (a, e) = (a.to_radians(), e.to_radians());
            let d = [a.cos() * e.cos(), a.sin() * e.cos(), e.sin()];
            let best = v.iter().map(|c| dot(mul(c.f, -1.0), d)).fold(-1.0, f64::max);
            assert!(best > 30f64.to_radians().cos(), "{best}");
        }
    }

    /// The light's view comes first, along its own rays with the whole part in it; straight
    /// overhead it still has a frame, and under the bed it has no view.
    #[test]
    fn the_lights_view_comes_first() {
        let (lo, hi) = ([-10.0, -20.0, 0.0], [10.0, 20.0, 30.0]);
        let from = Cam::looking([100.0, 0.0, 5.0], [0.0, 0.0, 15.0], 800.0, 500.0);
        let up = [0.0, 0.0, 1.0];
        let to_light = norm([0.35, -0.45, 0.8]);
        let v = views(lo, hi, &from, up, Some(to_light));
        assert_eq!(v.len(), VIEWS + 1);
        assert!(v[0].ortho && dot(v[0].f, to_light) < -0.999999);
        assert!(!v[1].ortho);
        for bits in 0..8 {
            let corner = [0, 1, 2].map(|i| if bits >> i & 1 == 0 { lo[i] } else { hi[i] });
            let (x, y, z) = v[0].proj(corner).unwrap();
            assert!(x > 0.0 && x < WIDE && y > 0.0 && y < HIGH && z > 0.0, "{x} {y} {z}");
        }
        assert!(views(lo, hi, &from, up, Some(up))[0].r.iter().all(|x| x.is_finite()));
        assert_eq!(views(lo, hi, &from, up, Some([0.35, -0.45, -0.8])).len(), VIEWS);
    }

    /// A look from the camera leaves what only the light sees bare, and the light's view covers it.
    #[test]
    fn the_first_view_leaves_nothing_bare_for_the_light() {
        let closed = TaskPool::closed();
        let root = SdfNode::Sphere { radius: 10.0 };
        let field = Field::new(&root, false, &closed).unwrap();
        let cam = Cam::looking([60.0, 0.0, 0.0], [0.0; 3], 800.0, 500.0);
        let light = views(field.lo, field.hi, &cam, [0.0, 0.0, 1.0], Some(norm([-1.0, 0.0, 0.5])))[0];
        let look = |s: &mut Surface, c: Cam| {
            let mut gaze = Gaze::default();
            while !gaze.step(s, &field, &closed, c, &MOVING, &|| false).unwrap().done {}
        };
        let at: Vec<(f64, f64)> = (0..HIGH as usize / 2).flat_map(|y| (0..WIDE as usize / 2).map(move |x| (x as f64 * 2.0 + 0.5, y as f64 * 2.0 + 0.5))).collect();
        let truth = super::truth::exact(&root, field.lo, field.hi, &light, 1.0, &at);
        let bare = |s: &Surface| {
            let mut cover = crate::surface::Cover::default();
            cover.fill(&light, 1.0, s, &closed);
            at.iter().zip(&truth).filter(|((x, y), t)| t.is_finite() && !cover.at(*x as usize, *y as usize).is_finite()).count()
        };
        let mut surface = Surface::new(&field);
        look(&mut surface, cam);
        let before = bare(&surface);
        look(&mut surface, light);
        let after = bare(&surface);
        assert!(before > 1000 && after * 100 < before, "{before} bare before, {after} after");
    }

    /// The app's pool, made on a thread of its own: the test's is not its UI thread, so a round
    /// the test runs spreads over it as the queue's job does.
    fn pool() -> TaskPool {
        std::thread::spawn(|| makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool()).join().unwrap()
    }

    /// A queue an input rests until the test ends the rest (`end_rest`): resting is a state,
    /// not a time a loaded box can run out.
    fn queue() -> ScanQueue {
        ScanQueue(Arc::new(Queue::new(Duration::MAX)))
    }

    fn end_rest(q: &ScanQueue) {
        *q.0.last_input.lock().unwrap() = None;
    }

    fn ball(r: f64) -> Arc<SdfShape> {
        Arc::new(SdfShape { root: SdfNode::Sphere { radius: r }, min: [-r; 3], max: [r; 3], materials: Vec::new() })
    }

    /// Frames at a still view until the part's view is done.
    fn rest(tiles: &mut Tiles, pool: &TaskPool, cam: Cam, key: [u8; 32], shape: &Arc<SdfShape>) {
        tiles.frame(pool, cam, key, shape);
        while tiles.busy() {
            tiles.frame(pool, cam, key, shape);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn scanned(tiles: &Tiles) -> bool {
        tiles.shared.want.lock().unwrap().scan_done
    }

    /// A test's queue never runs this many rounds: a ball's scan takes 190-195, two balls' 393
    /// (measured).
    const MOST_ROUNDS: u64 = 2_000;

    /// Waits for the queue's job to end: it ran out of parts to scan, or something stopped it.
    /// That is an event, not a time a loaded box can run out (the two balls' job took 3.2 s on
    /// one box and 12.9 s on a busier one); a job that never ends is caught by its work, counted.
    fn job_ends(q: &ScanQueue) {
        while q.0.running.load(Ordering::Acquire) {
            let rounds = q.0.rounds.load(Ordering::Relaxed);
            assert!(rounds < MOST_ROUNDS, "the scan never ended: {rounds} rounds");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A scan goes all the way round, what it found gets drawn, and then nothing runs.
    #[test]
    fn a_scan_shows_the_far_side_and_then_stops() {
        let (pool, q) = (pool(), queue());
        let cam = Cam::looking([60.0, 0.0, 0.0], [0.0; 3], 800.0, 500.0);
        let (mut tiles, shape, key) = (Tiles::in_queue(&q), ball(10.0), [1; 32]);
        rest(&mut tiles, &pool, cam, key, &shape);
        let before = tiles.count();
        end_rest(&q);
        q.kick(&pool);
        job_ends(&q);
        assert!(scanned(&tiles), "the job ended before the scan");
        tiles.frame(&pool, cam, key, &shape);
        assert!(tiles.count() as f64 > before as f64 * 1.3, "{} tiles against {before}", tiles.count());
        assert!(!tiles.busy() && tiles.look_due_in().is_none() && !tiles.take_woke(), "the scan keeps asking for looks");
        // nothing is left on the queue: a kick runs nothing
        let held = tiles.shared.want.lock().unwrap().kept.as_ref().unwrap().surface.as_ref().unwrap().held().cells;
        q.kick(&pool);
        assert!(!q.0.running.load(Ordering::Acquire), "a kick with the scan done ran");
        assert_eq!(tiles.shared.want.lock().unwrap().kept.as_ref().unwrap().surface.as_ref().unwrap().held().cells, held);
    }

    /// A thin wall the view shows stays whole through its scan. A cell a scan's view splits moves
    /// the mesh's edges round it, which can dent a wall thinner than the cells beside it where the
    /// view had it whole, so the view looks again at what the scan changed before any of it goes
    /// out. A hollow vase with a 0.8 wall, stretched to about twice its width along one axis as a
    /// drag of its scale left it, from above and in front: no ray that meets its wall shows a dent
    /// in it or what is behind it, before the scan nor after any of its views. Without the second
    /// look, 25 did after its second view, about four pixels deep, where the app showed specks.
    #[test]
    fn a_scan_leaves_the_views_thin_wall_whole() {
        let pool = pool();
        let root = super::truth::thin_vase_stretched();
        let (min, max) = makepad_csg_sdf::node_bounds(&root).unwrap();
        let shape = Arc::new(SdfShape { root: root.clone(), min, max, materials: Vec::new() });
        let cam = super::truth::app_view();
        let q = queue();
        let mut tiles = Tiles::in_queue(&q);
        tiles.set_scan([0.0, 1.0, 0.0], true, true);
        rest(&mut tiles, &pool, cam, [1; 32], &shape);
        let (w, h) = (cam.w as usize, cam.h as usize);
        let px: Vec<(f64, f64)> = (0..w * h).map(|i| ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5)).collect();
        let truth: Vec<f64> = super::truth::exact(&root, min, max, &cam, 2.0, &px).iter().zip(&px).map(|(t, (x, y))| if t.is_finite() { cam.depth(add(cam.ray(*x, *y).0, mul(cam.ray(*x, *y).1, *t))) } else { f64::INFINITY }).collect();
        // where a ray meets the wall, away from its edges, the mesh shows it, not a dent into it
        // nor what is behind it: no more than a pixel off along the wall's normal. One that only
        // grazes an edge, less than a quarter of a pixel in, meets no wall there
        let through = |surface: &Surface| {
            let mut cover = crate::surface::Cover::default();
            cover.fill(&cam, 1.0, surface, &TaskPool::closed());
            let (mut met, mut bad) = (0, Vec::new());
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    let z = truth[y * w + x];
                    let pw = cam.pw(z);
                    if !z.is_finite() || (y - 1..y + 2).any(|j| (x - 1..x + 2).any(|i| !((truth[j * w + i] - z).abs() <= pw * 8.0))) {
                        continue;
                    }
                    met += 1;
                    let shown = cover.at(x, y) as f64;
                    if shown - z <= pw {
                        continue;
                    }
                    let (o, d) = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                    let (t0, t1) = (cam.along(d, z), if shown.is_finite() { cam.along(d, shown) } else { cam.along(d, z) + 20.0 });
                    let p = add(o, mul(d, t0));
                    let e = pw * 0.05;
                    let grad: Vec<f64> = (0..3).map(|a| crate::trace::raw(&root, &[0, 1].map(|s| { let mut q = p; q[a] += if s == 0 { e } else { -e }; q }))).map(|v| v[0] - v[1]).collect();
                    let facing = dot(norm([grad[0], grad[1], grad[2]]), d).abs();
                    let n = ((t1 - t0) / (pw * 0.25)).ceil().max(1.0) as usize;
                    let pts: Vec<V> = (1..=n).map(|k| add(o, mul(d, t0 + (t1 - t0) * k as f64 / n as f64))).collect();
                    if (shown - z) * facing > pw && crate::trace::raw(&root, &pts).iter().any(|v| *v < -pw * 0.25) {
                        bad.push((x, y));
                    }
                }
            }
            (met, bad)
        };
        let surface = |tiles: &Tiles, look: &dyn Fn(&Surface) -> (u32, Vec<(usize, usize)>)| look(tiles.shared.want.lock().unwrap().kept.as_ref().unwrap().surface.as_ref().unwrap());
        let (met, before) = surface(&tiles, &through);
        assert!(met > 10_000, "{met} rays");
        assert!(before.is_empty(), "{} rays dent the wall or see past it before the scan, at {:?}", before.len(), &before[..before.len().min(8)]);
        // the scan's rounds one by one, as the queue runs them: each whole view, whose worth can
        // go out, in its batch or at a stop
        end_rest(&q);
        let (mut views, mut batches) = (0, 0);
        while !scanned(&tiles) {
            let before = views_done(&tiles);
            assert!(q.0.round(&tiles.shared, q.0.input.load(Ordering::Relaxed), &pool), "a round stopped");
            batches += tiles.shared.out.lock().unwrap().take().is_some() as usize;
            if !scanned(&tiles) && views_done(&tiles) == before {
                continue;
            }
            views += 1;
            let (_, after) = surface(&tiles, &through);
            assert!(after.is_empty(), "after {views} of the scan's views {} rays dent the wall or see past it, at {:?}", after.len(), &after[..after.len().min(8)]);
        }
        assert!(views > 20, "{views} views");
        assert!(batches >= 1, "nothing the scan found went out");
    }

    /// The views a part's scan has done.
    fn views_done(tiles: &Tiles) -> usize {
        tiles.shared.want.lock().unwrap().kept.as_ref().unwrap().scan.as_ref().map_or(0, |s| s.at)
    }

    /// A scan's looks spread over the pool find what they find on one thread, piece for piece: the
    /// view draws the same after them, so a finished picture is the same either way. Its first
    /// four views go through every spread step of a look (rays, checks, cones, cells placed and
    /// settled, chunks emitted, the cover drawn).
    #[test]
    fn a_scan_over_the_pool_finds_what_it_finds_on_one_thread() {
        let pool = pool();
        let cam = Cam::looking([60.0, 0.0, 0.0], [0.0; 3], 800.0, 500.0);
        let scanned_on = |on: &TaskPool| {
            let q = queue();
            let (mut tiles, shape, key) = (Tiles::in_queue(&q), ball(10.0), [1; 32]);
            rest(&mut tiles, &pool, cam, key, &shape);
            end_rest(&q);
            while views_done(&tiles) < 4 {
                assert!(q.0.round(&tiles.shared, q.0.input.load(Ordering::Relaxed), on), "a round stopped");
                // (the view takes each batch as it comes, as a frame would)
                tiles.latest();
            }
            // what the view draws once the views' worth still waiting for its batch goes out
            let mut drawn = tiles.drawn.clone();
            for (i, c) in &tiles.shared.want.lock().unwrap().kept.as_ref().unwrap().scan.as_ref().unwrap().ready {
                drawn[*i] = c.clone();
            }
            drawn.iter().map(|c| c.as_ref().map(|c| c.data_slices(0..c.float_len()).flat_map(|(_, s)| s.iter().map(|f| f.to_bits())).collect())).collect::<Vec<Option<Vec<u32>>>>()
        };
        let (alone, spread) = (scanned_on(&TaskPool::closed()), scanned_on(&pool));
        let floats = alone.iter().flatten().map(Vec::len).sum::<usize>();
        assert!(floats > 0, "nothing drawn");
        let differ: Vec<usize> = (0..alone.len()).filter(|&i| alone[i] != spread[i]).collect();
        assert!(differ.is_empty(), "spread over the pool the scan drew {} of {} chunks otherwise: {:?}", differ.len(), alone.iter().flatten().count(), &differ[..differ.len().min(8)]);
    }

    /// An input stops a scan until the input has rested; an edit starts over that part's scan and
    /// leaves the other's.
    /// Nothing else wakes the queue if a part settles sooner than the input rests.
    #[test]
    fn a_part_at_rest_before_the_input_asks_for_the_timer() {
        let (pool, q) = (pool(), queue());
        q.input();
        q.kick(&pool);
        assert!(q.take_waiting());
        assert!(!q.take_waiting());
    }

    #[test]
    fn input_stops_it_and_an_edit_starts_only_that_part_over() {
        let (pool, q) = (pool(), queue());
        let cam = Cam::looking([100.0, 0.0, 0.0], [0.0; 3], 800.0, 500.0);
        let (mut a, mut b) = (Tiles::in_queue(&q), Tiles::in_queue(&q));
        let (ball_a, ball_b) = (ball(10.0), ball(14.0));
        rest(&mut a, &pool, cam, [1; 32], &ball_a);
        rest(&mut b, &pool, cam, [2; 32], &ball_b);
        // the input just made it rest: a kick does nothing yet
        q.input();
        q.kick(&pool);
        assert!(!q.0.running.load(Ordering::Acquire), "it ran during the rest");
        end_rest(&q);
        q.kick(&pool);
        job_ends(&q);
        assert!(scanned(&a) && scanned(&b), "the job ended before the scans");
        // an edit of one
        let edited = ball(11.0);
        a.frame(&pool, cam, [3; 32], &edited);
        assert!(!scanned(&a), "the edit left its scan done");
        assert!(scanned(&b), "the edit touched the other part's");
        // its look rests once the edit is quiet, and the queue, resting, starts nothing meanwhile
        rest(&mut a, &pool, cam, [3; 32], &edited);
        assert_eq!(a.drawn_key(), Some([3; 32]));
        end_rest(&q);
        q.kick(&pool);
        job_ends(&q);
        assert!(scanned(&a), "the edit did not start that part's scan over");
    }
}
