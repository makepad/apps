//! The volumes (R14 §3): may a circle of radius r have its centre here, on layer L?
//!
//! Each volume is a polygon set per layer, per rung (the rounded radii of
//! [`Settings::rung`]), all at xy_min; a branch keeping the full side gap asks for the rung at
//! its radius plus the xy delta ([`Settings::rung_for`]). Every volume is built on its first
//! ask from the bottom up, as high as it is asked for, and kept (the wall restriction a layer at
//! a time): the descent comes down from the top, so its first ask at a rung is the highest it
//! makes there.
//!
//! - Collision (§3.1): where a centre may not be. The part's outline (its slices simplified at
//!   the resolution) on the layer and on the bottom-gap layers under it, grown by r + xy_min; the
//!   top-gap layers over it, grown by r plus a side gap falling towards the face; mitred at 1.2,
//!   which the engine takes as 2 (§1.5).
//!   Blocked areas never reach it (§3.1 item 4: the reference's test for them is never true),
//!   and it is not simplified.
//! - Hole-free collision (§3.1), for the rungs under r_br + the xy delta: the collision at the
//!   rung that rounds up to shrunk back to the asked one, so holes narrower than a branch close.
//! - Placeable areas (§3.2), only when supports may rest on the model.
//! - Avoidance (§3.3): where a centre may not be if it is to get down to the bed (or a placeable
//!   area) moving at most m a layer, built bottom-up as a [`Reach`] is: slow (the slow move,
//!   hole-free), fast (the fast move, plain) and fast hole-free (the fast move, hole-free). Layer
//!   0 is the plain collision, for every kind. Each layer over it is the one under it shrunk round
//!   at the resolution, with the layer's own collision (hole-free for the hole-free kinds), to the
//!   model less the placeable areas, and then simplified at the resolution: what the layer keeps
//!   and what the one over it starts from. The collision and the placeable areas are never
//!   simplified; the hole-free collision and the wall restriction are.
//! - Wall restriction (§3.4): what an area may not flow through going a layer down.
//!
//! Fab's keep-out, which is not the reference's ([`Volumes::keeping_out`]): per layer, regions
//! that are taken as the part on that layer. The collision grows them as it grows the layer's
//! outline, so every volume built from it keeps off them, and nothing in them is placeable.
//! A branch leaving the keep-out ([`Element::leaving`]) sees the part alone ([`Volumes::free`])
//! and keeps to where it still gets out by its exit ([`Volumes::leave_by`]). With none, the
//! volumes are the reference's.
//!
//! D28, Fab's: a first layer gap wider than xy_min ([`Settings::first_gap`]) is in the collision
//! on layer 0, from the part as the pad keeps off it: its first layer and those within the top
//! gap over it, and the keep-out. Every volume built from it leads a branch out of that ring on
//! the way down, and a roof reaching the bed stops short of it. A branch with no way out of it has
//! no way to the bed.
//!
//! Fab's too: an edit's volumes take from the run's before every layer that reads only what the
//! edit left ([`Volumes::taking`]), shared: the collision, the hole-free collision, the placeable
//! areas and the wall restriction off the layers the edit reached, and each avoidance's layers
//! from the bottom up while what they stand on is the run's before. Each is as a fresh build
//! makes it, ring for ring.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::Join;

use super::clip::{difference_ex, in_reference_order, intersection_ex, union_ex};
use super::element::{simplified, Element};
use super::offset::offset;
use super::settings::Settings;
use crate::coral::reach;
use crate::par::{Cancelled, Ctl};

/// The collision's and the placeable areas' corners (§3.1, §3.2): mitred to 1.2, which the engine
/// takes as 2 (§1.5): a full mitre up to 2 times the offset, squared at the offset past it.
const MITRE: Join = Join::Miter(1.2);
/// The lower layer a placeable area stands on is grown with this mitre (§3.2).
const PLACE_MITRE: Join = Join::Miter(3.0);
/// What a hole-free collision's shrink falls short of the rung difference by (mm): 5 nm.
const HOLE_FREE_SHORT: f64 = 5e-6;

/// An avoidance's kind (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// The slow move over the hole-free collision: the preferred, gentle path.
    Slow,
    /// The fast move over the plain collision: the fallback, steeper path.
    Fast,
    /// The fast move over the hole-free collision: fast, but never into a pocket.
    FastHoleFree,
}

/// What an avoidance gets down to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum To {
    Bed,
    /// The bed or a placeable area on the model.
    Model,
}

const KINDS: [Kind; 3] = [Kind::Slow, Kind::Fast, Kind::FastHoleFree];

/// A value built on its first ask and kept; a cancelled build keeps nothing.
struct Lazy<T> {
    lock: Mutex<()>,
    cell: OnceLock<T>,
}

impl<T> Lazy<T> {
    fn new() -> Self {
        Lazy { lock: Mutex::new(()), cell: OnceLock::new() }
    }

    fn get(&self, build: impl FnOnce() -> Result<T, Cancelled>) -> Result<&T, Cancelled> {
        if let Some(v) = self.cell.get() {
            return Ok(v);
        }
        let _one = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = self.cell.get() {
            return Ok(v);
        }
        let v = timed(build)?;
        Ok(self.cell.get_or_init(|| v))
    }
}

/// A volume's layers, built from the bottom up only as high as they have been asked for, and on
/// up from there on a higher ask. A layer stands on itself or on the layers under it alone, so
/// what is built is the whole build's as far as it goes, and stays right kept. A layer whose own
/// inputs are the run's before is that run's, shared, as it rises ([`Volumes::taking`]).
struct Rising {
    layers: Vec<OnceLock<Arc<Vec<ExPolygon>>>>,
    /// How many layers from the bottom are built, and whether the top one of them is the run's
    /// before (with none built, the bed's, which is); held while they are raised.
    built: Mutex<(usize, bool)>,
    /// The run before's layers, from the bottom up as high as it built them, and on over them the
    /// layers it had of the run before it ([`Volumes::taking`]).
    was: Vec<Arc<Vec<ExPolygon>>>,
    /// Per layer, whether what it reads of the part may not be what `was`'s layer was built from:
    /// every layer with no run before.
    changed: Vec<bool>,
    /// How many layers it built, not taking them.
    made: AtomicUsize,
}

impl Rising {
    fn new(layers: usize) -> Self {
        Rising { layers: (0..layers).map(|_| OnceLock::new()).collect(), built: Mutex::new((0, true)), was: Vec::new(), changed: Vec::new(), made: AtomicUsize::new(0) }
    }

    fn many(n: usize, layers: usize) -> Vec<Self> {
        (0..n).map(|_| Rising::new(layers)).collect()
    }

    /// Whether layer `l` is the run before's: that run built it from what this one reads there.
    fn takes(&self, l: usize) -> bool {
        l < self.was.len() && !self.changed[l]
    }

    /// Layer `l`, the layers up to it built first where they are not, each on its own by
    /// `build`, side by side; a cancelled raise keeps none of them.
    fn each(&self, ctl: Ctl, l: usize, build: impl Fn(usize) -> Result<Vec<ExPolygon>, Cancelled> + Sync) -> Result<&[ExPolygon], Cancelled> {
        self.rise(l, |at| crate::par::try_map(ctl, at.len(), |i| build(at[i]))?.into_iter().collect())
    }

    /// Layer `l`, the layers up to it built first where they are not: those that are the run
    /// before's taken ([`Rising::takes`]), the rest (`at`, from the bottom up) all at once by
    /// `build`; a cancelled raise keeps none of them.
    fn rise(&self, l: usize, build: impl FnOnce(&[usize]) -> Result<Vec<Vec<ExPolygon>>, Cancelled>) -> Result<&[ExPolygon], Cancelled> {
        if let Some(b) = self.layers[l].get() {
            return Ok(b.as_slice());
        }
        let mut built = self.built.lock().unwrap_or_else(|e| e.into_inner());
        if built.0 <= l {
            let at: Vec<usize> = (built.0..=l).filter(|&k| !self.takes(k)).collect();
            let mut out = if at.is_empty() { Vec::new() } else { timed(|| build(&at))? }.into_iter();
            self.made.fetch_add(at.len(), Ordering::Relaxed);
            for k in built.0..=l {
                let set = if self.takes(k) { self.was[k].clone() } else { Arc::new(out.next().expect("built for the layer")) };
                self.layers[k].get_or_init(|| set);
            }
            built.0 = l + 1;
        }
        Ok(self.layers[l].get().expect("raised to the layer").as_slice())
    }

    /// The first layer from where it stands up to `top` that an avoidance builds rather than
    /// takes ([`Volumes::raise_one`]); past `top` when it takes them all.
    fn first_built(&self, top: usize) -> usize {
        let (from, same) = *self.built.lock().unwrap_or_else(|e| e.into_inner());
        if !same {
            return from;
        }
        (from..=top).find(|&l| !self.takes(l)).unwrap_or(top + 1)
    }
}

/// An avoidance of [`Volumes::avoidance`] (§3.3), built as high as it is asked for.
#[derive(Clone, Copy)]
pub struct Avoidance<'v, 'c> {
    v: &'v Volumes,
    ctl: Ctl<'c>,
    kind: Kind,
    to: To,
    rung: usize,
}

impl<'v> Avoidance<'v, '_> {
    /// Where a centre on layer `l` must not be, to get down from there.
    pub fn blocked(&self, l: usize) -> Result<&'v [ExPolygon], Cancelled> {
        self.v.avoidance_layer(self.ctl, self.kind, self.to, self.rung, l)
    }

    /// Whether a centre at `p` on layer `l` gets down.
    pub fn reached(&self, p: Pt, l: usize) -> Result<bool, Cancelled> {
        Ok(!reach::inside(self.blocked(l)?, p))
    }

    /// The point on layer `l` nearest `p` within `max` mm of it that gets down: `p` itself, or a
    /// point just off the edge of where none does.
    pub fn nearest(&self, p: Pt, l: usize, max: f64) -> Result<Option<Pt>, Cancelled> {
        let b = self.blocked(l)?;
        Ok(if reach::inside(b, p) { reach::nearest_off(b, p, max, |q| !reach::inside(b, q)) } else { Some(p) })
    }
}

thread_local! {
    /// Whether this thread is inside a volume's build already: a build that asks for another
    /// volume is timed once.
    static BUILDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `build`, timed as the volumes' stage unless a build on this thread already is.
fn timed<T>(build: impl FnOnce() -> T) -> T {
    if BUILDING.with(|b| b.replace(true)) {
        return build();
    }
    let out = crate::prof::scope_fanned(crate::prof::Stage::TreeVolumes, build);
    BUILDING.with(|b| b.set(false));
    out
}

thread_local! {
    /// The volume layers the step or merge this thread is making reads ([`reading`]): none
    /// outside one.
    static READS: RefCell<Option<Vec<Read>>> = const { RefCell::new(None) };
}

/// A volume layer a step or a merge of the descent reads: which volume (and whether of the
/// part's own volumes without the keep-out, [`Volumes::free`]) and which layer of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Read {
    free: bool,
    of: Of,
    l: u32,
}

/// A volume, by its rung (an avoidance by its kind and target too, a wall restriction by its xy
/// kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Of {
    Collision(u32),
    HoleFree(u32),
    Placeable(u32),
    Avoidance(Kind, To, u32),
    Wall(u32, bool),
}

/// What `f` makes, and the volume layers it reads outside the volumes' own builds, each once:
/// what a step or a merge of the descent reads of them ([`Volumes::now`]).
pub fn reading<T>(f: impl FnOnce() -> T) -> (T, Vec<Read>) {
    /// Closes the thread's log however `f` ends.
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            READS.with(|r| r.borrow_mut().take());
        }
    }
    READS.with(|r| *r.borrow_mut() = Some(Vec::new()));
    let close = Close;
    let out = f();
    let reads = READS.with(|r| r.borrow_mut().take()).unwrap_or_default();
    drop(close);
    (out, reads)
}

/// The volume layers a step or a merge read, each as it read it ([`Volumes::now`]): none where
/// there was none to read (the placeable areas from the bed only, the wall restriction on layer
/// 0). It holds each layer, so none of them can be another layer.
#[derive(Clone)]
pub struct Reads(Arc<[(Read, Option<Arc<Vec<ExPolygon>>>)]>);

impl Default for Reads {
    fn default() -> Self {
        Reads(Arc::from(Vec::new()))
    }
}

impl Reads {
    /// Every layer of `each`, once, in the order of their names.
    pub fn all<'a>(each: impl IntoIterator<Item = &'a Reads>) -> Reads {
        let mut have = std::collections::HashSet::new();
        let mut out = Vec::new();
        for reads in each {
            for (r, layer) in reads.0.iter() {
                if have.insert(*r) {
                    out.push((*r, layer.clone()));
                }
            }
        }
        out.sort_unstable_by_key(|(r, _)| *r);
        Reads(out.into())
    }
}

type Layers = Vec<Vec<ExPolygon>>;

/// What a rung's avoidances keep off and stand on, a layer, united once for them all
/// ([`Volumes::raise`]): its collision, its hole-free collision (none on layer 0 or past the
/// hole-free rungs, where the collision stands for it) and its placeable areas (none from the bed
/// only).
struct Common {
    plain: Vec<ExPolygon>,
    hole_free: Option<Vec<ExPolygon>>,
    rests: Vec<ExPolygon>,
}

/// The volumes of one part. They own what they are built from, so a run's volumes can be kept
/// for the next run on the same inputs ([`super::Cache`]).
pub struct Volumes {
    s: Settings,
    /// Per layer, the part's outline: its slices simplified at the resolution.
    outline: Layers,
    blocked: Layers,
    /// Per layer, what is kept out as the part is ([`Volumes::keeping_out`]): none past its end.
    keep_out: Layers,
    /// Per rung.
    collision: Vec<Rising>,
    hole_free: Vec<Rising>,
    placeable: Vec<Rising>,
    /// Per (parent rung, xy kind), per layer.
    walls: Vec<Vec<Lazy<Arc<Vec<ExPolygon>>>>>,
    /// How many wall restrictions it built, not taking them.
    walls_made: AtomicUsize,
    /// How many layers of its avoidances were raised on an ask past what was built: the layers a
    /// branch on a layer waited on ([`Volumes::raise_ahead`]).
    asked: AtomicUsize,
    /// Per (rung, kind, target).
    avoidance: Vec<Rising>,
    /// Per rung, held while its avoidances rise ([`Volumes::raise`]).
    raising: Vec<Mutex<()>>,
    /// With a keep-out, the same part's volumes without it: what a branch leaving it sees.
    free: Option<Box<Volumes>>,
    /// Per (rung, exit, layer), what a branch leaving the keep-out may not be in
    /// ([`Volumes::leave_by`]).
    leave: Mutex<HashMap<(usize, usize, usize), Arc<Vec<ExPolygon>>>>,
    /// Every volume's layers under this read only what is the run before's ([`Volumes::taking`]):
    /// none with no run before.
    same_below: usize,
    /// Whether these are a part's own volumes without the keep-out ([`Volumes::free`]).
    is_free: bool,
}

fn grow(set: &[ExPolygon], by: f64, join: Join) -> Vec<ExPolygon> {
    if set.is_empty() || by == 0.0 { set.to_vec() } else { offset(set, by * SCALE, join) }
}

fn union_of(set: Vec<ExPolygon>) -> Vec<ExPolygon> {
    if set.len() > 1 { union_ex(&set) } else { set }
}

impl Volumes {
    /// For the part's slices, one per layer, as §2.0 leaves them ([`super::overhangs::slices`]),
    /// and the areas blocked per layer (none past their end).
    pub fn new(ctl: Ctl, s: &Settings, part: &[Vec<ExPolygon>], blocked: &[Vec<ExPolygon>]) -> Result<Self, Cancelled> {
        // §3.1: the slices simplified at the resolution by the per-ring kind, each ring from the
        // first point §2.0 left it (every ring by Rule A, §1.6).
        let outline = crate::par::try_map(ctl, part.len(), |l| simplified(&part[l], s.keys.resolution))?;
        Ok(Self::of_outline(s, outline, blocked))
    }

    /// Unbuilt volumes over `outline`, with no keep-out.
    fn of_outline(s: &Settings, outline: Layers, blocked: &[Vec<ExPolygon>]) -> Self {
        let rungs = s.rungs();
        let layers = outline.len();
        Volumes {
            s: s.clone(),
            outline,
            blocked: blocked.to_vec(),
            keep_out: Vec::new(),
            collision: Rising::many(rungs, layers),
            hole_free: Rising::many(rungs, layers),
            placeable: Rising::many(rungs, layers),
            walls: (0..rungs * 2).map(|_| (0..layers).map(|_| Lazy::new()).collect()).collect(),
            walls_made: AtomicUsize::new(0),
            asked: AtomicUsize::new(0),
            avoidance: Rising::many(rungs * KINDS.len() * 2, layers),
            raising: (0..rungs).map(|_| Mutex::new(())).collect(),
            free: None,
            leave: Mutex::new(HashMap::new()),
            same_below: 0,
            is_free: false,
        }
    }

    /// These volumes, with `keep_out` taken as the part on each layer (none past its end): in
    /// the collision, grown as the layer's outline is, and so in every volume built from it; and
    /// never placeable. Fab's, not the reference's ([`super::Input::keep_out`]). Without it, the
    /// part's own volumes stay at hand for the branches leaving it ([`Volumes::free`]).
    pub fn keeping_out(self, keep_out: &[Vec<ExPolygon>]) -> Self {
        if keep_out.iter().all(Vec::is_empty) {
            return self;
        }
        let free = Volumes { is_free: true, ..Self::of_outline(&self.s, self.outline.clone(), &self.blocked) };
        Volumes { keep_out: keep_out.to_vec(), free: Some(Box::new(free)), ..self }
    }

    /// These volumes taking from `old`, the run's before on the same settings (its part edited),
    /// every layer that reads only what is the same in both: the outlines, blocked areas and
    /// keep-out of the layers it reads (§3.1 to §3.4), shared as each volume rises; and each
    /// avoidance's layers while what it stands on is `old`'s ([`Volumes::raise_one`]). A layer `old`
    /// did not build, cut short, is the run before it's, on the same terms: what it reads is the
    /// same in that run and this one. A layer one run has and the other has not is read by one
    /// alone, an edited one. Every layer is as a fresh build over these slices makes it, ring for
    /// ring.
    pub fn taking(mut self, old: &Volumes) -> Self {
        fn on(v: &Layers, l: usize) -> &[ExPolygon] {
            v.get(l).map(Vec::as_slice).unwrap_or_default()
        }
        let (n, had) = (self.layers(), old.layers());
        let edited: Vec<bool> = (0..n.max(had)).map(|l| l >= n.min(had) || self.outline[l] != old.outline[l]).collect();
        let kept: Vec<bool> = (0..n).map(|l| on(&self.keep_out, l) != on(&old.keep_out, l)).collect();
        let blocked: Vec<bool> = (0..n).map(|l| on(&self.blocked, l) != on(&old.blocked, l)).collect();
        let (bottom, top) = (self.s.bottom_gap_layers, self.s.top_gap_layers);
        // §3.1: the outlines of the layer, of the bottom gap under it and the top gap over it, and
        // its keep-out (the hole-free collision is a wider rung's on the layer).
        let collision: Vec<bool> = (0..n).map(|l| kept[l] || (l.saturating_sub(bottom)..=l + top).any(|k| edited.get(k) == Some(&true))).collect();
        // §3.2: the outlines of the layer and of the one a bottom gap under it, that one's
        // blockers, and its keep-out.
        let placeable: Vec<bool> = (0..n).map(|l| edited[l] || kept[l] || l.checked_sub(bottom + 1).is_some_and(|k| edited[k] || blocked[k])).collect();
        // §3.3: what the layer keeps off and stands on.
        let rests: Vec<bool> = (0..n).map(|l| collision[l] || placeable[l]).collect();
        // §3.4: the collisions of the layer and of the one under it.
        let walls: Vec<bool> = (0..n).map(|l| collision[l] || (l > 0 && collision[l - 1])).collect();
        // Each volume's layers `old` built, and over them the run before it's, which a run cut short
        // leaves as it found them. Those read what changed for `old` as well, and the first of them
        // stands on that run's layer, which `old`'s top one may not be.
        let share = |new: &mut [Rising], old: &[Rising], changed: &[bool]| {
            for (a, b) in new.iter_mut().zip(old) {
                let (built, same) = *b.built.lock().unwrap_or_else(|e| e.into_inner());
                a.was = b.layers[..built].iter().map(|l| l.get().expect("built under its top").clone()).chain(b.was.iter().skip(built).cloned()).take(n).collect();
                a.changed = (0..n).map(|l| changed[l] || (l >= built && !b.takes(l)) || (l == built && !same)).collect();
            }
        };
        share(&mut self.collision, &old.collision, &collision);
        share(&mut self.hole_free, &old.hole_free, &collision);
        share(&mut self.placeable, &old.placeable, &placeable);
        share(&mut self.avoidance, &old.avoidance, &rests);
        for (a, b) in self.walls.iter().zip(&old.walls) {
            for l in (0..n.min(had)).filter(|&l| !walls[l]) {
                if let Some(set) = b[l].cell.get() {
                    a[l].cell.get_or_init(|| set.clone());
                }
            }
        }
        // The part's own volumes take from the run before's: its own, or itself with no keep-out.
        self.free = self.free.take().map(|free| Box::new((*free).taking(old.free())));
        let own = rests.iter().position(|&r| r).unwrap_or(n);
        self.same_below = self.free.as_ref().map_or(own, |f| own.min(f.same_below));
        self
    }

    /// The layers under which every volume, the part's own too ([`Volumes::free`]), reads only what
    /// is the run before's that these take from ([`Volumes::taking`]), and so is as that run's:
    /// none with no run before.
    pub fn same_below(&self) -> usize {
        self.same_below
    }

    /// How many layers of its volumes it built since this was last asked (its part's own without
    /// the keep-out too), not taking them ([`Volumes::taking`]).
    pub fn take_made(&self) -> usize {
        let risings = self.collision.iter().chain(&self.hole_free).chain(&self.placeable).chain(&self.avoidance);
        risings.map(|r| r.made.swap(0, Ordering::Relaxed)).sum::<usize>() + self.walls_made.swap(0, Ordering::Relaxed) + self.free.as_ref().map_or(0, |f| f.take_made())
    }

    /// How many layers of its avoidances were raised on asks past what was built since this was
    /// last asked (its part's own without the keep-out too).
    pub fn take_asked(&self) -> usize {
        self.asked.swap(0, Ordering::Relaxed) + self.free.as_ref().map_or(0, |f| f.take_asked())
    }

    /// Whether it keeps anything out ([`Volumes::keeping_out`]).
    pub fn keeps_out(&self) -> bool {
        self.free.is_some()
    }

    /// The part's own volumes, without the keep-out: these when there is none.
    pub fn free(&self) -> &Volumes {
        self.free.as_deref().unwrap_or(self)
    }

    /// The volumes `e` sees: the part's own while it is leaving the keep-out, else these.
    pub fn of(&self, e: &Element) -> &Volumes {
        if e.leaving() { self.free() } else { self }
    }

    /// Where a branch at `rung` on layer `l`, leaving the keep-out to be out of it by layer
    /// `exit` (under `l`), may not be: where the fast avoidance to the bed at `rung`, keep-out
    /// and all, comes within the fast move a layer of it on each layer down to `exit`; there, the
    /// avoidance itself. From anywhere else a branch moving out at the fast move gets out in
    /// time, the part aside.
    pub fn leave_by(&self, ctl: Ctl, rung: usize, exit: usize, l: usize) -> Result<Arc<Vec<ExPolygon>>, Cancelled> {
        // Read on every ask, kept or not: the layer it is made from ([`reading`]).
        let on = self.avoidance(ctl, Kind::Fast, To::Bed, rung).blocked(exit)?;
        let key = (rung, exit, l);
        if let Some(set) = self.leave.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Ok(set.clone());
        }
        let set = Arc::new(reach::beyond_of(on, l.saturating_sub(exit) as f64 * self.s.fast));
        Ok(self.leave.lock().unwrap_or_else(|e| e.into_inner()).entry(key).or_insert(set).clone())
    }

    pub fn layers(&self) -> usize {
        self.outline.len()
    }

    pub fn settings(&self) -> &Settings {
        &self.s
    }

    /// The part's outline on layer `l`: its slices simplified at the resolution (§3.1).
    pub fn outline(&self, l: usize) -> &[ExPolygon] {
        &self.outline[l]
    }

    /// Whether `rung` gets a hole-free collision of its own: under r_br + the xy delta.
    fn has_hole_free(&self, rung: usize) -> bool {
        self.s.rung(rung) < self.s.r_br + self.s.xy_delta
    }

    /// §3.1 on layers `at` (from the bottom up) at `rung`, side by side.
    fn collisions(&self, ctl: Ctl, at: &[usize], rung: usize) -> Result<Vec<Vec<ExPolygon>>, Cancelled> {
        let (s, n) = (&self.s, self.outline.len());
        let r = s.rung(rung);
        let f = s.xy_min;
        // Items 1 and 2: the layer and the bottom-gap layers under it, each layer's outline grown
        // by r + xy_min on its own (one offset of that layer's united outline), united last. A
        // layer's grown outline is the same in each layer it counts in: it is grown once.
        let gap = s.bottom_gap_layers;
        let mut under: Vec<usize> = at.iter().flat_map(|&l| l.saturating_sub(gap)..=l).collect();
        under.sort_unstable();
        under.dedup();
        let grown = crate::par::try_map(ctl, under.len(), |i| grow(&self.outline[under[i]], r + f, MITRE))?;
        let grown_on = |k: usize| &grown[under.binary_search(&k).expect("grown for the layer")];
        crate::par::try_map(ctl, at.len(), |i| {
            let l = at[i];
            let mut out: Vec<ExPolygon> = (l.saturating_sub(gap)..=l).flat_map(|k| grown_on(k).iter().cloned()).collect();
            // Fab's keep-out on the layer, grown as the layer's outline is.
            if let Some(keep) = self.keep_out.get(l) {
                out.extend(grow(keep, r + f, MITRE));
            }
            // Item 3: the top-gap layers over it, the side gap falling a step a layer.
            let gap = s.top_gap_layers;
            for k in 1..=gap {
                if l + k >= n {
                    break;
                }
                let side = if gap == 1 { f / 2.0 } else { f * (1.0 - k as f64 / gap as f64) };
                out.extend(grow(&self.outline[l + k], r + side, MITRE));
            }
            if l == 0 && s.first_gap > 0.0 {
                out.extend(self.outline[..n.min(gap + 1)].iter().chain(self.keep_out.first()).flat_map(|o| grow(o, r + s.first_gap, MITRE)));
            }
            union_of(out)
        })
    }

    /// Where a centre of radius `rung` may not be on layer `l` (§3.1).
    pub fn collision(&self, ctl: Ctl, l: usize, rung: usize) -> Result<&[ExPolygon], Cancelled> {
        self.note(Of::Collision(rung as u32), l);
        self.collision_layer(ctl, l, rung)
    }

    fn collision_layer(&self, ctl: Ctl, l: usize, rung: usize) -> Result<&[ExPolygon], Cancelled> {
        self.collision[rung].rise(l, |at| self.collisions(ctl, at, rung))
    }

    /// The collision at `rung` with holes narrower than a branch closed (§3.1): for a rung under
    /// r_br + the xy delta, the collision at the rung that rounds up to, shrunk back to this one;
    /// past it, the collision itself.
    pub fn hole_free(&self, ctl: Ctl, l: usize, rung: usize) -> Result<&[ExPolygon], Cancelled> {
        if !self.has_hole_free(rung) {
            return self.collision(ctl, l, rung);
        }
        self.note(Of::HoleFree(rung as u32), l);
        let s = &self.s;
        let wide = s.rung_for(s.r_br, true);
        if self.hole_free[rung].layers[l].get().is_none() {
            self.collision_layer(ctl, l, wide)?;
        }
        let back = (s.rung(wide) - s.rung(rung) - HOLE_FREE_SHORT) * SCALE;
        let arc = Join::Round(s.keys.resolution * SCALE);
        self.hole_free[rung].each(ctl, l, |k| {
            let big = self.collision(ctl, k, wide)?;
            Ok(if big.is_empty() { Vec::new() } else { simplified(&offset(big, -back, arc), s.keys.resolution) })
        })
    }

    /// Where a centre of radius `rung` may stand on the model on layer `l` (§3.2): the outline
    /// one bottom gap and a layer under it, grown by xy_min, less the outline on the layer grown by
    /// xy_min and less that lower layer's blockers; past rung 0, shrunk by the radius plus the full
    /// side gap. None from the bed only.
    pub fn placeable(&self, ctl: Ctl, l: usize, rung: usize) -> Result<&[ExPolygon], Cancelled> {
        self.note(Of::Placeable(rung as u32), l);
        self.placeable_layer(ctl, l, rung)
    }

    fn placeable_layer(&self, ctl: Ctl, l: usize, rung: usize) -> Result<&[ExPolygon], Cancelled> {
        let s = &self.s;
        if s.keys.bed_only {
            return Ok(&[]);
        }
        if rung != 0 {
            if self.placeable[rung].layers[l].get().is_none() {
                self.placeable_layer(ctl, l, 0)?;
            }
            let by = (s.rung(rung) + s.keys.xy_distance) * SCALE;
            return self.placeable[rung].each(ctl, l, |k| {
                let base = self.placeable(ctl, k, 0)?;
                Ok(if base.is_empty() { Vec::new() } else { offset(&union_ex(base), -by, MITRE) })
            });
        }
        let f = s.xy_min;
        self.placeable[0].each(ctl, l, |l| {
            let Some(k) = l.checked_sub(s.bottom_gap_layers + 1) else { return Ok(Vec::new()) };
            if self.outline[k].is_empty() {
                return Ok(Vec::new());
            }
            let mut off = grow(&self.outline[l], f, MITRE);
            if let Some(b) = self.blocked.get(k) {
                off.extend(b.iter().cloned());
            }
            // Nothing stands in Fab's keep-out: less it on the layer, grown as the outline is.
            if let Some(keep) = self.keep_out.get(l) {
                off.extend(grow(keep, f, MITRE));
            }
            Ok(difference_ex(&grow(&self.outline[k], f, PLACE_MITRE), &off))
        })
    }

    /// The avoidance of `kind` to `to` at `rung` (§3.3): where a centre on a layer must not be
    /// to get down from there. Fast hole-free past the hole-free rungs is fast: holes no longer
    /// matter there.
    pub fn avoidance<'c>(&self, ctl: Ctl<'c>, kind: Kind, to: To, rung: usize) -> Avoidance<'_, 'c> {
        let (kind, to) = self.avoidance_of(kind, to, rung);
        Avoidance { v: self, ctl, kind, to, rung }
    }

    /// Layer `l` of the avoidance `kind` to `to` at `rung`, as [`Volumes::avoidance_of`] leaves
    /// them. Each is built from the bottom up as high as it is asked for: the descent comes down
    /// from the top, so its first ask at a rung is the highest it makes there.
    ///
    /// The first ask past what is built raises every avoidance of the rung to it side by side:
    /// the descent asks for them close together, each is a pass up the layers, and every branch
    /// on the layer waits for the one it asks for. A rung on the ramp ([`Settings::ramp_rungs`])
    /// raises every ramp rung's: each branch comes down the ramp within its first layers, so they
    /// are all asked for near the top. The ramp's are raised before the tips ([`Volumes::ramp`])
    /// and the rest beside them as high as a branch can ask for them ([`Volumes::raise_ahead`]),
    /// so the descent asks for none past what is built.
    fn avoidance_layer(&self, ctl: Ctl, kind: Kind, to: To, rung: usize, l: usize) -> Result<&[ExPolygon], Cancelled> {
        self.note(Of::Avoidance(kind, to, rung as u32), l);
        let rising = &self.avoidance[self.avoidance_at(kind, to, rung)];
        if let Some(b) = rising.layers[l].get() {
            return Ok(b.as_slice());
        }
        let ramp = self.s.ramp_rungs();
        self.raise_rungs(ctl, if ramp.contains(&rung) { &ramp } else { std::slice::from_ref(&rung) }, l, true)?;
        Ok(rising.layers[l].get().expect("raised to the layer").as_slice())
    }

    /// Builds every avoidance of the ramp's rungs on every layer, as the first ask on the ramp
    /// would as high as it asks: the tips ask for them first ([`super::tips`]).
    pub fn ramp(&self, ctl: Ctl) -> Result<(), Cancelled> {
        match self.layers().checked_sub(1) {
            Some(top) => self.raise_rungs(ctl, &self.s.ramp_rungs(), top, false),
            None => Ok(()),
        }
    }

    /// Builds every avoidance of each rung past the ramp as high as a branch under tips no higher
    /// than layer `top` can ask for it ([`Settings::rung_tops`]), the rungs side by side, before
    /// the descent asks for them: no branch on a layer then waits on a rung's pass up the layers.
    /// Each layer is the same raised ahead as on an ask: it stands on its inputs and the layer
    /// under it alone.
    pub fn raise_ahead(&self, ctl: Ctl, top: usize) -> Result<(), Cancelled> {
        let Some(last) = self.layers().checked_sub(1) else { return Ok(()) };
        let tops = self.s.rung_tops(top.min(last));
        crate::par::try_map(ctl, tops.len(), |i| self.raise(ctl, tops[i].0, tops[i].1, false))?.into_iter().collect()
    }

    /// Raises every avoidance of `rungs` to layer `top`, the rungs side by side; `asked`, for an
    /// ask past what was built.
    fn raise_rungs(&self, ctl: Ctl, rungs: &[usize], top: usize, asked: bool) -> Result<(), Cancelled> {
        crate::par::try_map(ctl, rungs.len(), |i| self.raise(ctl, rungs[i], top, asked))?.into_iter().collect()
    }

    /// The avoidance `kind` to `to` at `rung` stands for: fast hole-free past the hole-free
    /// rungs is fast, and from the bed only every avoidance is to the bed.
    fn avoidance_of(&self, kind: Kind, to: To, rung: usize) -> (Kind, To) {
        let kind = if kind == Kind::FastHoleFree && !self.has_hole_free(rung) { Kind::Fast } else { kind };
        (kind, if self.s.keys.bed_only { To::Bed } else { to })
    }

    fn avoidance_at(&self, kind: Kind, to: To, rung: usize) -> usize {
        let k = KINDS.iter().position(|&x| x == kind).unwrap_or(0);
        (rung * KINDS.len() + k) * 2 + (to == To::Model) as usize
    }

    /// Builds every avoidance of `rung` ([`Volumes::avoidance_of`]'s) on up from where it stands
    /// to layer `top`. Several keep off the same collision and stand on the same placeable areas:
    /// each layer's of those is united once for them all, from the first layer one of them builds
    /// rather than takes ([`Volumes::raise_one`]), then each avoidance goes up the layers on its
    /// own, side by side.
    fn raise(&self, ctl: Ctl, rung: usize, top: usize, asked: bool) -> Result<(), Cancelled> {
        let mut all: Vec<(Kind, To)> = Vec::new();
        for (k, t) in KINDS.iter().flat_map(|&k| [To::Bed, To::Model].map(|t| self.avoidance_of(k, t, rung))) {
            if !all.contains(&(k, t)) {
                all.push((k, t));
            }
        }
        // Under the rung's lock no avoidance of it rises but here.
        let _one = self.raising[rung].lock().unwrap_or_else(|e| e.into_inner());
        let rising = |&(k, t): &(Kind, To)| &self.avoidance[self.avoidance_at(k, t, rung)];
        let from = all.iter().map(|a| rising(a).built.lock().unwrap_or_else(|e| e.into_inner()).0).min().unwrap_or(top + 1);
        if from > top {
            return Ok(());
        }
        if asked {
            self.asked.fetch_add(top + 1 - from, Ordering::Relaxed);
        }
        timed(|| {
            // Layer 0 is the plain collision for every kind; slow and fast hole-free keep off the
            // hole-free collision from layer 1 up (§3.3), which past the hole-free rungs is the
            // collision itself.
            let hole_free = self.has_hole_free(rung) && all.iter().any(|&(k, _)| k != Kind::Fast);
            let placed = all.iter().any(|&(_, t)| t == To::Model);
            let first = all.iter().map(|a| rising(a).first_built(top)).min().unwrap_or(top + 1);
            let shared = if first > top {
                Vec::new()
            } else {
                // What they keep off and stand on, up to the layer, first.
                self.collision(ctl, top, rung)?;
                if hole_free {
                    self.hole_free(ctl, top, rung)?;
                }
                if placed {
                    self.placeable(ctl, top, rung)?;
                }
                crate::par::try_map(ctl, top + 1 - first, |i| -> Result<Common, Cancelled> {
                    let l = first + i;
                    let plain = reach::keep_of(self.collision(ctl, l, rung)?);
                    let hole_free = if hole_free && l > 0 { Some(reach::keep_of(self.hole_free(ctl, l, rung)?)) } else { None };
                    let rests = if placed { reach::rests_of(self.placeable(ctl, l, rung)?, &[], true) } else { Vec::new() };
                    Ok(Common { plain, hole_free, rests })
                })?
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?
            };
            crate::par::try_map(ctl, all.len(), |j| self.raise_one(ctl, all[j], rung, top, (first, &shared)))?.into_iter().collect()
        })
    }

    /// Builds the avoidance `kind` to `to` at `rung` on up from where it stands to layer `top`,
    /// from what [`Volumes::raise`] united for each layer from `common.0` up. A layer whose own
    /// inputs are the run's before ([`Volumes::taking`]) over a layer that is that run's is that
    /// run's, taken; any other is built, and is that run's again when it comes out ring for ring
    /// as that run's did.
    fn raise_one(&self, ctl: Ctl, (kind, to): (Kind, To), rung: usize, top: usize, common: (usize, &[Common])) -> Result<(), Cancelled> {
        let rising = &self.avoidance[self.avoidance_at(kind, to, rung)];
        let mut built = rising.built.lock().unwrap_or_else(|e| e.into_inner());
        let (from, mut same) = *built;
        let placed = to == To::Model;
        let m = if kind == Kind::Slow { self.s.avoid_slow } else { self.s.avoid_fast };
        // Each layer's lean comes off the avoidance under it in a round shrink at the resolution,
        // its close points dropped first as every offset drops them (§1.5). Every layer over the
        // bottom one, to the bed and to the model alike, its placeable areas out, is then
        // simplified at the resolution, and the layer over it starts from that (Q13). The
        // simplify fixes each ring's first point where the layer's last boolean left it (Q16d):
        // the reach makes that union, or to the model the placeable areas' difference, without
        // the reference's order, so its rings take that order first (§1.6).
        let res = self.s.keys.resolution;
        let arc = Join::Round(res * SCALE);
        let erode = |set: &[ExPolygon], by: f64| offset(set, -by, arc);
        let mut below: &[ExPolygon] = match from.checked_sub(1) {
            Some(b) => rising.layers[b].get().expect("built under").as_slice(),
            None => &[],
        };
        for l in from..=top {
            if ctl.cancelled() {
                return Err(Cancelled);
            }
            let set = if same && rising.takes(l) {
                rising.was[l].clone()
            } else {
                let at = &common.1[l - common.0];
                let keep = if kind == Kind::Fast || l == 0 { &at.plain } else { at.hole_free.as_ref().unwrap_or(&at.plain) };
                let rests: &[ExPolygon] = if placed { &at.rests } else { &[] };
                let here = reach::layer_of(keep, rests, below, m, placed, &erode);
                let here = Arc::new(if l == 0 { here } else { simplified(&in_reference_order(here), res) });
                same = rising.was.get(l).is_some_and(|w| **w == *here);
                rising.made.fetch_add(1, Ordering::Relaxed);
                here
            };
            below = rising.layers[l].get_or_init(|| set).as_slice();
            *built = (l + 1, same);
        }
        Ok(())
    }

    /// What an area of a branch at `rung` may not flow through going from layer `l` to `l` − 1
    /// (§3.4): the collision on `l` at radius 0 of the xy kind, where the collision on `l` − 1 at
    /// `rung` also is, simplified at the resolution. Built a layer at a time: the branches on a
    /// layer ask for its own alone, and a build of every layer on the first ask at a rung held
    /// every branch asking after it.
    pub fn wall_restriction(&self, ctl: Ctl, l: usize, rung: usize, full_xy: bool) -> Result<&[ExPolygon], Cancelled> {
        self.note(Of::Wall(rung as u32, full_xy), l);
        if l == 0 {
            return Ok(&[]);
        }
        let wall = self.walls[rung * 2 + full_xy as usize][l].get(|| {
            let zero = self.collision(ctl, l, self.s.rung_for(0.0, full_xy))?;
            let wall = simplified(&intersection_ex(zero, self.collision(ctl, l - 1, rung)?), self.s.keys.resolution);
            self.walls_made.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(wall))
        })?;
        Ok(wall.as_slice())
    }

    /// Notes that layer `l` of `of` is read, when a step or a merge is ([`reading`]); not inside
    /// a build of a volume, whose layer stands for all it was built from.
    fn note(&self, of: Of, l: usize) {
        if BUILDING.with(Cell::get) {
            return;
        }
        READS.with(|r| {
            if let Some(reads) = r.borrow_mut().as_mut() {
                let read = Read { free: self.is_free, of, l: l as u32 };
                if !reads.contains(&read) {
                    reads.push(read);
                }
            }
        });
    }

    /// The layer `r` names, asked for as a step asks for it (so built if it is not yet): none
    /// where there is none to read.
    fn layer(&self, ctl: Ctl, r: Read) -> Result<Option<&Arc<Vec<ExPolygon>>>, Cancelled> {
        let v = if r.free { self.free() } else { self };
        let l = r.l as usize;
        if l >= v.layers() {
            return Ok(None);
        }
        Ok(match r.of {
            Of::Collision(k) => {
                v.collision(ctl, l, k as usize)?;
                v.collision[k as usize].layers[l].get()
            }
            Of::HoleFree(k) => {
                v.hole_free(ctl, l, k as usize)?;
                v.hole_free[k as usize].layers[l].get()
            }
            Of::Placeable(k) => {
                v.placeable(ctl, l, k as usize)?;
                v.placeable[k as usize].layers[l].get()
            }
            Of::Avoidance(kind, to, k) => {
                v.avoidance_layer(ctl, kind, to, k as usize, l)?;
                v.avoidance[v.avoidance_at(kind, to, k as usize)].layers[l].get()
            }
            Of::Wall(k, full) => {
                v.wall_restriction(ctl, l, k as usize, full)?;
                v.walls[k as usize * 2 + usize::from(full)][l].cell.get()
            }
        })
    }

    /// The layers `reads` names as these volumes have them, for [`Volumes::holds`].
    pub fn now(&self, ctl: Ctl, reads: Vec<Read>) -> Result<Reads, Cancelled> {
        let mut out = Vec::with_capacity(reads.len());
        for r in reads {
            out.push((r, self.layer(ctl, r)?.cloned()));
        }
        Ok(Reads(out.into()))
    }

    /// Whether these volumes have every layer `reads` read as it read it: the very layer (built
    /// from the same as it was, [`Volumes::taking`]), or none where there was none.
    pub fn holds(&self, ctl: Ctl, reads: &Reads) -> Result<bool, Cancelled> {
        for (r, was) in reads.0.iter() {
            let same = match (self.layer(ctl, *r)?, was) {
                (None, None) => true,
                (Some(now), Some(was)) => Arc::ptr_eq(now, was),
                _ => false,
            };
            if !same {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::settings::tests::r14;
    use super::super::settings::Keys;
    use super::*;
    use crate::coral::reach::Reach;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)))
    }

    fn inside(set: &[ExPolygon], x: f64, y: f64) -> bool {
        set.iter().any(|e| e.contains(Pt::from_mm(x, y)))
    }

    fn ctl() -> Ctl<'static> {
        Ctl::default()
    }

    fn volumes(s: &Settings, part: &[Vec<ExPolygon>], blocked: &[Vec<ExPolygon>]) -> Volumes {
        Volumes::new(ctl(), s, part, blocked).unwrap()
    }

    /// §3.1 items 1-4 on a 10 mm post from the bed to layer 9, a block from layer 20 up (its face
    /// on layer 20), and a blocked square on layer 5.
    #[test]
    fn collision_is_the_part_grown_and_the_gaps() {
        let s = r14(2.0);
        let part: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l < 10 { vec![rect(0.0, 0.0, 10.0, 10.0)] } else if l >= 20 { vec![rect(20.0, 0.0, 30.0, 10.0)] } else { Vec::new() }).collect();
        let blocked: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l == 5 { vec![rect(40.0, 0.0, 42.0, 2.0)] } else { Vec::new() }).collect();
        let v = volumes(&s, &part, &blocked);
        let c = |l: usize, rung: usize| v.collision(ctl(), l, rung).unwrap().to_vec();
        // Item 1: grown by r + xy_min = 1.5 + 0.21 at rung 2. A square corner's mitre is √2 times
        // that: past the limit of 1.2 as written, but the engine takes the limit as 2 (§1.5), so
        // the corner is mitred in full, out to (11.71, 11.71).
        assert!(inside(&c(5, 2), 11.70, 5.0) && !inside(&c(5, 2), 11.72, 5.0), "item 1: the side grown by r + xy_min");
        assert!(inside(&c(5, 2), 11.70, 11.70) && !inside(&c(5, 2), 11.72, 11.72), "item 1: the corner mitred in full");
        assert!(inside(&c(5, 0), 10.20, 5.0) && !inside(&c(5, 0), 10.22, 5.0), "item 1: at radius 0, grown by xy_min alone");
        // Item 2: the post's top layer 9 is in the bottom gap under layer 10, not under 11.
        assert!(inside(&c(10, 1), 5.0, 5.0), "item 2: the part in the bottom gap under the layer");
        assert!(!inside(&c(11, 1), 5.0, 5.0), "item 2: past the bottom gap");
        // Item 3: the face on layer 20 is in the top gap over layer 19: under it, and beside it
        // grown by r + xy_min / 2; nothing of it over layer 18.
        assert!(inside(&c(19, 0), 25.0, 5.0), "item 3: under the face within the top gap");
        assert!(inside(&c(19, 0), 19.90, 5.0) && !inside(&c(19, 0), 19.89, 5.0), "item 3: beside the face, half of xy_min");
        assert!(inside(&c(19, 1), 19.50, 5.0) && !inside(&c(19, 1), 19.49, 5.0), "item 3: with the radius, r + xy_min / 2");
        assert!(!inside(&c(18, 0), 25.0, 5.0), "item 3: the tip layer under a one-layer gap is free under the face");
        // Item 4: blockers never reach the collision.
        assert!(!inside(&c(5, 1), 41.0, 1.0), "item 4: a blocked area is no collision");
        // Item 3 with a two-layer gap: the layer under the face gets no side gap, the one under
        // that half of xy_min.
        let two = Settings::new(Keys { top_distance: 0.4, ..r14(2.0).keys });
        let v2 = volumes(&two, &part, &[]);
        let c2 = |l: usize| v2.collision(ctl(), l, 0).unwrap().to_vec();
        assert!(inside(&c2(19), 19.90, 5.0) && !inside(&c2(19), 19.89, 5.0), "item 3, n = 2: a layer under the face, xy_min · (1 − 1/2)");
        assert!(!inside(&c2(18), 19.99, 5.0) && inside(&c2(18), 20.01, 5.0), "item 3, n = 2: n layers under the face its side gap is 0");
    }

    /// §3.1 items 1 and 2: each layer's outline is grown on its own and the grown outlines are
    /// united. A diamond on layer 1 with its right corner 0.1 mm inside layer 0's square still
    /// mitres out past the square's growth: its 90° corner goes √2 (r + xy_min) out, the square's
    /// side only r + xy_min. Growing the two layers' united outline once would lose the corner,
    /// the diamond being inside the square.
    #[test]
    fn collision_unites_each_layers_own_growth() {
        let s = r14(2.0);
        let diamond = ExPolygon::new(Polygon::new(vec![Pt::from_mm(9.9, 5.0), Pt::from_mm(8.0, 6.9), Pt::from_mm(6.1, 5.0), Pt::from_mm(8.0, 3.1)]));
        let v = volumes(&s, &[vec![rect(0.0, 0.0, 10.0, 10.0)], vec![diamond]], &[]);
        let c = v.collision(ctl(), 1, 1).unwrap();
        // At rung 1, r + xy_min = 0.4 + 0.21: the square's side grows to 10.61, the diamond's
        // corner to 9.9 + 0.61 √2 = 10.763.
        assert!(inside(c, 10.60, 2.0) && !inside(c, 10.62, 2.0), "the square's side grown by r + xy_min");
        assert!(inside(c, 10.75, 5.0) && !inside(c, 10.78, 5.0), "the diamond's corner mitred out past it");
    }

    /// §3.1 (§1.6): the outline's simplify runs each slice ring from the first point §2.0 left
    /// it. A hole 0.03 mm wide and 0.008 mm deep, its apex at the bottom: stored by Rule A, from
    /// its top left corner, the 0.012 mm simplify drops the apex and the hole with it; stored from
    /// the apex, all three corners stay.
    #[test]
    fn the_outline_runs_each_slice_ring_from_its_first_point() {
        let s = r14(2.0);
        let (left, right, apex) = (Pt::from_mm(5.0, 5.008), Pt::from_mm(5.03, 5.008), Pt::from_mm(5.015, 5.0));
        let holed = |hole: Vec<Pt>| {
            let mut slab = rect(0.0, 0.0, 10.0, 10.0);
            slab.holes = vec![Polygon::new(hole)];
            volumes(&s, &[vec![slab]], &[]).outline(0).to_vec()
        };
        let by_rule_a = holed(vec![left, right, apex]);
        assert_eq!((by_rule_a.len(), by_rule_a[0].holes.len()), (1, 0), "from its top left corner the hole loses its apex and goes");
        let from_apex = holed(vec![apex, left, right]);
        assert_eq!((from_apex.len(), from_apex[0].holes.len()), (1, 1), "from the apex it stays");
    }

    /// §3.1 hole-free: a 3 mm slot in a wall is open to a tip-radius centre and closed by the
    /// hole-free collision, which is that of r_br at full xy shrunk back.
    #[test]
    fn hole_free_collision_closes_holes_narrower_than_a_branch() {
        let s = r14(2.0);
        let wall = difference_ex(&[rect(0.0, 0.0, 20.0, 20.0)], &[rect(5.0, 5.0, 8.0, 15.0)]);
        let part: Vec<Vec<ExPolygon>> = (0..5).map(|_| wall.clone()).collect();
        let v = volumes(&s, &part, &[]);
        assert_eq!(s.rung(s.rung_for(s.r_br, true)), 3.9, "r_br + 1.79 rounds up to the 3.9 rung");
        assert!(v.has_hole_free(3) && !v.has_hole_free(4), "the rungs strictly under 2.79 mm get one: 0.4, 1.5, 2.6");
        let plain = v.collision(ctl(), 2, 1).unwrap();
        let free = v.hole_free(ctl(), 2, 1).unwrap();
        assert!(!inside(plain, 6.5, 10.0), "the slot is open to a tip");
        assert!(inside(free, 6.5, 10.0), "the hole-free collision closes it");
        assert!(inside(free, -0.60, 10.0) && !inside(free, -0.62, 10.0), "outside, the hole-free collision is the tip's own: r + xy_min");
        assert!(std::ptr::eq(v.hole_free(ctl(), 2, 4).unwrap(), v.collision(ctl(), 2, 4).unwrap()), "past the hole-free rungs it is the collision");
    }

    /// §3.2: on a slab's top, one bottom gap and a layer under, grown by xy_min, less the post on
    /// it grown by xy_min and less the lower layer's blockers; past rung 0 shrunk by r and the
    /// full side gap; none from the bed only.
    #[test]
    fn placeable_areas_are_the_part_top_under_the_bottom_gap() {
        let s = r14(0.35);
        let part: Vec<Vec<ExPolygon>> = (0..20).map(|l| if l < 10 { vec![rect(0.0, 0.0, 20.0, 20.0)] } else { vec![rect(8.0, 8.0, 12.0, 12.0)] }).collect();
        let blocked: Vec<Vec<ExPolygon>> = (0..20).map(|l| if l == 9 { vec![rect(15.0, 15.0, 17.0, 17.0)] } else { Vec::new() }).collect();
        let v = volumes(&s, &part, &blocked);
        let p0 = v.placeable(ctl(), 11, 0).unwrap();
        assert!(inside(p0, 3.0, 3.0), "the slab's top beside the post");
        assert!(inside(p0, -0.20, 10.0) && !inside(p0, -0.22, 10.0), "the lower layer grown by xy_min");
        assert!(inside(p0, 7.78, 10.0) && !inside(p0, 7.80, 10.0), "less the post grown by xy_min");
        assert!(!inside(p0, 16.0, 16.0), "less the lower layer's blockers");
        let p1 = v.placeable(ctl(), 11, 1).unwrap();
        assert!(inside(p1, 0.56, 10.0) && !inside(p1, 0.52, 10.0), "past rung 0 shrunk by r + xy: 0.4 + 0.35 from the grown edge");
        assert!(v.placeable(ctl(), 1, 0).unwrap().is_empty(), "nothing under the bottom gap of layer 1");
        let bed = Settings::new(Keys { bed_only: true, ..r14(0.35).keys });
        assert!(volumes(&bed, &part, &[]).placeable(ctl(), 11, 0).unwrap().is_empty(), "none from the bed only");
    }

    /// Fab's keep-out: a 5 mm square kept out on layers 11-15 over a slab on layers 0-9 is taken as
    /// the part there. It is collision grown by r + xy_min, the avoidance keeps off it, nothing on
    /// the slab's top under it is placeable, and an empty keep-out leaves the volumes as they are.
    #[test]
    fn the_keep_out_is_taken_as_the_part() {
        let s = r14(0.35);
        let slab: Vec<Vec<ExPolygon>> = (0..20).map(|l| if l < 10 { vec![rect(0.0, 0.0, 20.0, 20.0)] } else { Vec::new() }).collect();
        let keep: Vec<Vec<ExPolygon>> = (0..20).map(|l| if (11..16).contains(&l) { vec![rect(5.0, 5.0, 10.0, 10.0)] } else { Vec::new() }).collect();
        let (plain, kept) = (volumes(&s, &slab, &[]), volumes(&s, &slab, &[]).keeping_out(&keep));
        // At rung 1, r + xy_min = 0.4 + 0.21: the square's side grows to 10.61.
        let c = |l: usize| kept.collision(ctl(), l, 1).unwrap().to_vec();
        assert!(inside(&c(12), 7.5, 7.5) && inside(&c(12), 10.60, 7.5) && !inside(&c(12), 10.62, 7.5), "kept out on its layers, grown by r + xy_min");
        assert!(!inside(&c(17), 7.5, 7.5) && !inside(plain.collision(ctl(), 12, 1).unwrap(), 7.5, 7.5), "nothing kept out past its layers, or without it");
        // To the model, from the square's middle over it the slab is out of reach: it would have to
        // lean 3.11 mm off it, at 0.2 mm a layer.
        let down = |v: &Volumes, l: usize, x: f64| v.avoidance(ctl(), Kind::Fast, To::Model, 1).reached(Pt::from_mm(x, 7.5), l).unwrap();
        assert!(down(&plain, 17, 7.5) && !down(&kept, 17, 7.5), "over the square the avoidance keeps off it");
        assert!(down(&kept, 13, 15.0), "beside it a centre still gets down to the slab");
        // The slab's top under the square, grown by xy_min, is no place to stand.
        let p = kept.placeable(ctl(), 11, 0).unwrap();
        assert!(inside(plain.placeable(ctl(), 11, 0).unwrap(), 7.5, 7.5) && !inside(p, 7.5, 7.5), "the slab's top under the square is not placeable");
        assert!(!inside(p, 10.20, 7.5) && inside(p, 10.22, 7.5), "less the square grown by xy_min");
        // Kept out, but empty on every layer: the volumes as they are.
        let none = vec![Vec::new(); 20];
        let same = volumes(&s, &slab, &[]).keeping_out(&none);
        assert!(!same.keeps_out() && kept.keeps_out(), "an empty keep-out keeps nothing out");
        for rung in 0..3 {
            assert!((0..20).all(|l| same.collision(ctl(), l, rung).unwrap() == plain.collision(ctl(), l, rung).unwrap() && same.placeable(ctl(), l, rung).unwrap() == plain.placeable(ctl(), l, rung).unwrap()), "rung {rung}: an empty keep-out changes nothing");
        }
    }

    /// A branch leaving the keep-out sees the part alone, and may not be where it can no longer
    /// get out by its exit at the fast move: a 10 mm square kept out on every layer of 40.
    #[test]
    fn a_branch_leaving_the_keep_out_sees_the_part_alone_until_its_exit() {
        let s = r14(0.35);
        let part: Vec<Vec<ExPolygon>> = vec![Vec::new(); 40];
        let keep: Vec<Vec<ExPolygon>> = vec![vec![rect(0.0, 0.0, 10.0, 10.0)]; 40];
        let v = volumes(&s, &part, &[]).keeping_out(&keep);
        assert!(inside(v.collision(ctl(), 20, 1).unwrap(), 5.0, 5.0) && v.free().collision(ctl(), 20, 1).unwrap().is_empty(), "the part alone keeps nothing out");
        let mut e = super::super::element::Element::tip(&s, 30, Pt::from_mm(9.0, 5.0));
        assert!(std::ptr::eq(v.of(&e), &v), "a tip out of the keep-out by its own layer keeps out");
        e.exit = 20;
        assert!(std::ptr::eq(v.of(&e), v.free()), "one leaving it sees the part alone");
        e.layer = 20;
        assert!(std::ptr::eq(v.of(&e), &v), "and from its exit down keeps out");
        // On its exit, where the fast avoidance to the bed is; ten layers over it, that less ten
        // fast moves round: at rung 1 the square grown by 0.61 mm, then shrunk by 10 · 0.2 mm.
        let at = |l: usize, x: f64| inside(&v.leave_by(ctl(), 1, 20, l).unwrap(), x, 5.0);
        let avoid = v.avoidance(ctl(), Kind::Fast, To::Bed, 1);
        assert!(at(20, 10.5) && !at(20, 10.7) && avoid.reached(Pt::from_mm(10.7, 5.0), 20).unwrap(), "on the exit, the avoidance itself");
        let edge = 10.61 - 10.0 * s.fast;
        assert!(at(30, edge - 0.05) && !at(30, edge + 0.05), "ten layers over it, ten fast moves less: edge at {edge:.2} mm");
    }

    /// §3.3: over a 20 mm slab on layers 0-9, a centre leans off it at the kind's move a layer;
    /// in a 2 mm tube only the plain collision lets it down; to the model it lands on the slab,
    /// inside the bottom gap's collision too.
    #[test]
    fn avoidance_kinds_lean_at_their_move_and_hole_free_ones_skip_pockets() {
        let s = r14(0.35);
        let slab: Vec<Vec<ExPolygon>> = (0..40).map(|l| if l < 10 { vec![rect(0.0, 0.0, 20.0, 20.0)] } else { Vec::new() }).collect();
        let v = volumes(&s, &slab, &[]);
        let at = |kind, to, l, x| v.avoidance(ctl(), kind, to, 1).reached(Pt::from_mm(x, 10.0), l).unwrap();
        // Layer 29 is 19 layers over the bottom gap's layer 10; at rung 1, 0.4 + 0.21 off the side.
        let fast_in = 19.0 * s.avoid_fast - 0.61;
        let slow_in = 19.0 * s.avoid_slow - 0.61;
        assert!(at(Kind::Fast, To::Bed, 29, fast_in - 0.01) && !at(Kind::Fast, To::Bed, 29, fast_in + 0.01), "fast leans 0.199997 mm a layer");
        assert!(at(Kind::Slow, To::Bed, 29, slow_in - 0.01) && !at(Kind::Slow, To::Bed, 29, slow_in + 0.01), "slow leans 0.093259 mm a layer");
        assert!(!at(Kind::Fast, To::Bed, 29, 10.0) && at(Kind::Fast, To::Model, 29, 10.0), "over the middle it gets to the model, not the bed");
        assert!(at(Kind::Fast, To::Model, 10, 10.0), "to the model the placeable area is taken out last, inside the bottom gap's collision too");
        // A tube on the bed, its hole 2 mm across.
        let tube = difference_ex(&[rect(0.0, 0.0, 10.0, 10.0)], &[rect(4.0, 4.0, 6.0, 6.0)]);
        let tubes: Vec<Vec<ExPolygon>> = (0..10).map(|_| tube.clone()).collect();
        let t = volumes(&s, &tubes, &[]);
        let down = |kind| t.avoidance(ctl(), kind, To::Bed, 1).reached(Pt::from_mm(5.0, 5.0), 8).unwrap();
        assert!(down(Kind::Fast), "fast goes down the tube's hole");
        assert!(!down(Kind::FastHoleFree) && !down(Kind::Slow), "fast hole-free and slow never go into it");
        let on = |kind, l| t.avoidance(ctl(), kind, To::Bed, 1).reached(Pt::from_mm(5.0, 5.0), l).unwrap();
        assert!(on(Kind::Slow, 0) && on(Kind::FastHoleFree, 0), "on layer 0 every kind keeps off the plain collision, so the hole is open there");
        assert!(!on(Kind::Slow, 1) && !on(Kind::FastHoleFree, 1), "from layer 1 up the hole-free collision closes it");
        let fast = |kind| t.avoidance(ctl(), kind, To::Bed, 5).blocked(3).unwrap();
        assert!(std::ptr::eq(fast(Kind::FastHoleFree), fast(Kind::Fast)), "fast hole-free past the hole-free rungs is fast");
    }

    /// A volume is built from the bottom up as high as it is asked for, and on up from there on a
    /// higher ask: the collision, the hole-free collision and the placeable areas, each the same on
    /// every layer as asked for in any order.
    #[test]
    fn volumes_are_built_as_high_as_they_are_asked_for() {
        let s = r14(0.35);
        let part: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l < 6 { vec![rect(0.0, 0.0, 10.0, 10.0)] } else if l < 12 { vec![rect(0.0, 0.0, 30.0, 10.0)] } else { Vec::new() }).collect();
        let v = volumes(&s, &part, &[]);
        let rung = 1;
        assert!(v.has_hole_free(rung), "rung {rung} has a hole-free collision of its own");
        let built = |r: &[Rising], l: usize| r[rung].layers[l].get().is_some();
        v.collision(ctl(), 7, rung).unwrap();
        assert!(built(&v.collision, 7) && !built(&v.collision, 8), "the collision up to layer 7 alone");
        v.hole_free(ctl(), 12, rung).unwrap();
        v.placeable(ctl(), 15, rung).unwrap();
        assert!(built(&v.hole_free, 12) && !built(&v.hole_free, 13) && built(&v.placeable, 15) && !built(&v.placeable, 16), "the hole-free collision up to 12, the placeable areas up to 15");
        assert!(!built(&v.collision, 8), "the hole-free collision stands on a wider rung's");
        let fresh = volumes(&s, &part, &[]);
        for l in (0..30).rev() {
            assert_eq!(v.collision(ctl(), l, rung).unwrap(), fresh.collision(ctl(), l, rung).unwrap(), "collision on layer {l}");
            assert_eq!(v.hole_free(ctl(), l, rung).unwrap(), fresh.hole_free(ctl(), l, rung).unwrap(), "hole-free on layer {l}");
            assert_eq!(v.placeable(ctl(), l, rung).unwrap(), fresh.placeable(ctl(), l, rung).unwrap(), "placeable on layer {l}");
        }
        assert!(!v.placeable(ctl(), 11 + s.bottom_gap_layers + 1, rung).unwrap().is_empty(), "the ledge's top is somewhere to stand");
    }

    /// The first ask at a rung on the ramp raises every ramp rung's avoidances to its layer: each
    /// kind to the bed and to the model, and no higher. A rung past the ramp raises its own
    /// alone, fast hole-free past the hole-free rungs being fast.
    #[test]
    fn the_first_ask_on_the_ramp_raises_every_ramp_rungs_avoidances_to_it() {
        let s = r14(0.35);
        let slab: Vec<Vec<ExPolygon>> = (0..20).map(|l| if l < 5 { vec![rect(0.0, 0.0, 20.0, 20.0)] } else { Vec::new() }).collect();
        let v = volumes(&s, &slab, &[]);
        let raw = |kind, to, rung, l: usize| v.avoidance[v.avoidance_at(kind, to, rung)].layers[l].get().is_some();
        let built = |kind, to, rung, l| {
            let (kind, to) = v.avoidance_of(kind, to, rung);
            raw(kind, to, rung, l)
        };
        let ramp = s.ramp_rungs();
        let past = ramp[ramp.len() - 1] + 1;
        assert!(ramp.len() > 1 && past < s.zero_full(), "rungs {ramp:?} on the ramp, {past} past it");
        v.avoidance(ctl(), Kind::Slow, To::Bed, ramp[ramp.len() - 1]).blocked(12).unwrap();
        for kind in KINDS {
            for to in [To::Bed, To::Model] {
                assert!(ramp.iter().all(|&r| built(kind, to, r, 12) && !built(kind, to, r, 13)), "{kind:?} to {to:?} on every ramp rung, up to layer 12");
                assert!(!built(kind, to, past, 0), "{kind:?} to {to:?} past the ramp waits for its own ask");
            }
        }
        v.avoidance(ctl(), Kind::Fast, To::Model, past).blocked(7).unwrap();
        assert!(KINDS.iter().all(|&kind| built(kind, To::Bed, past, 7) && built(kind, To::Model, past, 7)), "a rung past the ramp raises its own");
        assert!(!built(Kind::Slow, To::Bed, past + 1, 0), "and only its own");
        assert!(!v.has_hole_free(past) && !raw(Kind::FastHoleFree, To::Bed, past, 0), "past the hole-free rungs fast hole-free is fast");
        v.ramp(ctl()).unwrap();
        assert!(ramp.iter().all(|&r| built(Kind::Fast, To::Model, r, 19)), "the ramp's build takes them to the top");
    }

    /// An avoidance raised a few layers at a time, in any order of asks, is the one built up every
    /// layer at once ([`crate::coral::reach::Reach`]'s, simplified past the bottom layer), to the
    /// bed and to the model: over a post with a ledge, hole-free and plain.
    #[test]
    fn an_avoidance_raised_by_its_asks_is_the_one_built_at_once() {
        let s = r14(0.35);
        let ledge = vec![rect(0.0, 0.0, 30.0, 10.0)];
        let part: Vec<Vec<ExPolygon>> = (0..40).map(|l| if l < 8 { vec![rect(0.0, 0.0, 10.0, 10.0)] } else if l < 12 { ledge.clone() } else { Vec::new() }).collect();
        let asks = [9, 3, 26, 26, 14, 39, 0];
        let res = s.keys.resolution;
        let arc = Join::Round(res * SCALE);
        let erode = |set: &[ExPolygon], by: f64| offset(set, -by, arc);
        let finish = |set: Vec<ExPolygon>| simplified(&in_reference_order(set), res);
        for (kind, to) in [(Kind::Fast, To::Bed), (Kind::Slow, To::Model), (Kind::FastHoleFree, To::Model)] {
            let v = volumes(&s, &part, &[]);
            let a = v.avoidance(ctl(), kind, to, 1);
            for &l in &asks {
                a.blocked(l).unwrap();
            }
            let mut keep: Vec<Vec<ExPolygon>> = (0..40).map(|l| if kind == Kind::Fast { v.collision(ctl(), l, 1) } else { v.hole_free(ctl(), l, 1) }.unwrap().to_vec()).collect();
            keep[0] = v.collision(ctl(), 0, 1).unwrap().to_vec();
            let tops: Vec<Vec<ExPolygon>> = (0..40).map(|l| v.placeable(ctl(), l, 1).unwrap().to_vec()).collect();
            let m = if kind == Kind::Slow { s.avoid_slow } else { s.avoid_fast };
            let whole = match to {
                To::Bed => Reach::eroded_by(ctl(), &keep, &[], &[m; 40], false, &erode, &finish),
                To::Model => Reach::eroded_by(ctl(), &keep, &tops, &[m; 40], true, &erode, &finish),
            }
            .unwrap();
            assert!((0..40).all(|l| a.blocked(l).unwrap() == whole.blocked(l)), "{kind:?} to {to:?}: raised by its asks, the same on every layer");
        }
    }

    /// §3.3 with §1.5: over an L-shaped post, layer 0's avoidance is its collision, and each layer
    /// over it is the one under it shrunk by the move, round at the resolution (0.012 mm), its close
    /// points dropped first, then simplified at the resolution; so the inner corner, sharp on layer
    /// 10, rounds on layer 11 in fewer, longer chords than a 5 µm arc would give.
    #[test]
    fn avoidance_shrinks_round_at_the_resolution() {
        let s = r14(0.35);
        let post = union_ex(&[rect(0.0, 0.0, 20.0, 5.0), rect(0.0, 0.0, 5.0, 20.0)]);
        let part: Vec<Vec<ExPolygon>> = (0..20).map(|l| if l < 10 { post.clone() } else { Vec::new() }).collect();
        let v = volumes(&s, &part, &[]);
        let a = |l| v.avoidance(ctl(), Kind::Fast, To::Bed, 1).blocked(l).unwrap();
        let (res, m) = (s.keys.resolution, s.avoid_fast * SCALE);
        assert_eq!(a(0), &reach::keep_of(v.collision(ctl(), 0, 1).unwrap())[..], "layer 0 is its collision, united, not simplified");
        for l in 11..14 {
            assert!(v.collision(ctl(), l, 1).unwrap().is_empty(), "no collision over the bottom gap's layer 10");
            let want = simplified(&union_ex(&offset(a(l - 1), -m, Join::Round(res * SCALE))), res);
            assert_eq!(a(l), &want[..], "layer {l}: the one under it shrunk round at the resolution, then simplified");
        }
        // Layer 10 is the collision (the post in its bottom gap), its inner corner sharp; layer 11, the first shrink, rounds it.
        let fine = total_area_mm2(&simplified(&union_ex(&offset(a(10), -m, Join::Round(0.005 * SCALE))), res));
        let got = total_area_mm2(a(11));
        assert!((got - fine).abs() > 1e-4, "layer 11: {got} mm2, not the 5 µm arc's {fine}");
    }

    /// §3.3 (Q13): every avoidance layer over the bottom one, to the bed and to the model alike,
    /// is simplified at the resolution, and the layer over it starts from that. Over a triangular
    /// post whose top is blocked, so that nothing is placeable, to the model is to the bed. At side
    /// gap 0.202 mm the collision grows the post by 0.602 mm, 2 µm more than three avoidance
    /// moves. The post's two 45° corners are past the engine's mitre limit of 2 (§1.5), so the
    /// collision squares each 0.602 mm out from it: an edge 0.804 mm long, which loses 0.267 mm a
    /// layer over the post. On layer 13 it is 2.7 µm long, under the resolution, so one of its two
    /// corners goes, where the same avoidance left as it comes keeps both.
    #[test]
    fn every_avoidance_layer_is_simplified_and_the_next_starts_from_it() {
        let s = r14(0.202);
        let post = vec![ExPolygon::new(Polygon::new(vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(40.0, 0.0), Pt::from_mm(0.0, 40.0)]))];
        let part: Vec<Vec<ExPolygon>> = (0..40).map(|l| if l < 10 { post.clone() } else { Vec::new() }).collect();
        let lid = vec![rect(-1.0, -1.0, 41.0, 41.0)];
        let blocked: Vec<Vec<ExPolygon>> = (0..40).map(|l| if l == 8 || l == 9 { lid.clone() } else { Vec::new() }).collect();
        let v = volumes(&s, &part, &blocked);
        assert!((0..40).all(|l| v.placeable(ctl(), l, 1).unwrap().is_empty()), "the post's top is blocked, so nothing is placeable at the rung");
        let (bed, model) = (v.avoidance(ctl(), Kind::Fast, To::Bed, 1), v.avoidance(ctl(), Kind::Fast, To::Model, 1));
        assert!((0..40).all(|l| bed.blocked(l).unwrap() == model.blocked(l).unwrap()), "with nothing placeable, to the model is to the bed");
        let arc = Join::Round(s.keys.resolution * SCALE);
        let erode = |set: &[ExPolygon], by: f64| offset(set, -by, arc);
        let collision: Vec<Vec<ExPolygon>> = (0..40).map(|l| v.collision(ctl(), l, 1).unwrap().to_vec()).collect();
        let raw = Reach::eroded_by(ctl(), &collision, &[], &[s.avoid_fast; 40], false, &erode, &|set| set).unwrap();
        let points = |set: &[ExPolygon]| set.iter().flat_map(|e| e.rings()).map(Polygon::len).sum::<usize>();
        assert_eq!(raw.blocked(0), bed.blocked(0).unwrap(), "layer 0 is the collision either way");
        let (r, b) = (raw.blocked(13), bed.blocked(13).unwrap());
        assert!(!b.is_empty() && points(b) + 2 <= points(r), "layer 13: {} points simplified, {} as it comes", points(b), points(r));
        let (ar, ab) = (total_area_mm2(r), total_area_mm2(b));
        assert!((ar - ab).abs() < 1e-3 * ar, "layer 13: {ab} mm2 simplified, {ar} as it comes");
    }

    /// An edit's volumes taken from the run's before, over a 10 mm column a millimetre wider on two
    /// bands of layers apart: every layer of every volume is as built afresh over the edited
    /// column. A layer that reads only layers the edit left is the run's before, shared: the
    /// collision, the hole-free collision and the wall restriction off the bands' reach, and an
    /// avoidance under the lower band, and again over each band from where it comes back to the
    /// run's before (the wider column shrunk off it) up to the next band. On the bands they are
    /// built again.
    #[test]
    fn an_edits_volumes_take_the_layers_it_left_from_the_run_before() {
        let s = r14(0.35);
        let n = 60;
        let bands = [12..15, 36..39];
        let column = |wide: bool| vec![rect(0.0, 0.0, if wide { 11.0 } else { 10.0 }, 10.0)];
        let before: Vec<Vec<ExPolygon>> = (0..n).map(|_| column(false)).collect();
        let after: Vec<Vec<ExPolygon>> = (0..n).map(|l| column(bands.iter().any(|b| b.contains(&l)))).collect();
        let rungs = 0..3;
        let kinds = [(Kind::Slow, To::Bed), (Kind::Slow, To::Model), (Kind::Fast, To::Bed), (Kind::Fast, To::Model), (Kind::FastHoleFree, To::Bed), (Kind::FastHoleFree, To::Model)];
        let old = volumes(&s, &before, &[]);
        for rung in rungs.clone() {
            for &(kind, to) in &kinds {
                old.avoidance(ctl(), kind, to, rung).blocked(n - 1).unwrap();
            }
            old.hole_free(ctl(), n - 1, rung).unwrap();
            old.placeable(ctl(), n - 1, rung).unwrap();
            for l in 1..n {
                old.wall_restriction(ctl(), l, rung, false).unwrap();
            }
        }
        let new = volumes(&s, &after, &[]).taking(&old);
        let fresh = volumes(&s, &after, &[]);
        let shared = |a: &[ExPolygon], b: &[ExPolygon]| !a.is_empty() && std::ptr::eq(a, b);
        // The layers a band's outline reaches: the collision's bottom gap over it and top gap under it.
        let (gb, gt) = (s.bottom_gap_layers, s.top_gap_layers);
        let reached = |l: usize| bands.iter().any(|b| l + gt >= b.start && l < b.end + gb);
        for rung in rungs.clone() {
            for l in (0..n).rev() {
                let (c, h) = (new.collision(ctl(), l, rung).unwrap(), new.hole_free(ctl(), l, rung).unwrap());
                assert!(c == fresh.collision(ctl(), l, rung).unwrap() && h == fresh.hole_free(ctl(), l, rung).unwrap() && new.placeable(ctl(), l, rung).unwrap() == fresh.placeable(ctl(), l, rung).unwrap(), "rung {rung} layer {l}: as built afresh");
                assert_eq!(shared(c, old.collision(ctl(), l, rung).unwrap()), !reached(l), "rung {rung} layer {l}: the collision the run's before off the bands' reach alone");
                assert_eq!(shared(h, old.hole_free(ctl(), l, rung).unwrap()), !reached(l), "rung {rung} layer {l}: the hole-free collision too");
                if l > 0 {
                    let w = new.wall_restriction(ctl(), l, rung, false).unwrap();
                    assert!(w == fresh.wall_restriction(ctl(), l, rung, false).unwrap(), "rung {rung} layer {l}: the wall restriction as built afresh");
                    assert_eq!(shared(w, old.wall_restriction(ctl(), l, rung, false).unwrap()), !reached(l) && !reached(l - 1), "rung {rung} layer {l}: the wall restriction the run's before off the bands' reach");
                }
            }
            for &(kind, to) in &kinds {
                let (a, f, o) = (new.avoidance(ctl(), kind, to, rung), fresh.avoidance(ctl(), kind, to, rung), old.avoidance(ctl(), kind, to, rung));
                let taken: Vec<bool> = (0..n).map(|l| shared(a.blocked(l).unwrap(), o.blocked(l).unwrap())).collect();
                assert!((0..n).all(|l| a.blocked(l).unwrap() == f.blocked(l).unwrap()), "{kind:?} to {to:?} at rung {rung}: as built afresh on every layer");
                let low = bands[0].start - gt;
                assert!((0..low).all(|l| taken[l]), "{kind:?} to {to:?} at rung {rung}: the run's before under the lower band: {taken:?}");
                for (k, b) in bands.iter().enumerate() {
                    let end = bands.get(k + 1).map_or(n, |next| next.start - gt);
                    let back = (b.start..end).find(|&l| taken[l]);
                    assert!(back.is_some_and(|c| c > b.end && (c..end).all(|l| taken[l])) && !taken[b.start], "{kind:?} to {to:?} at rung {rung}: built again over band {k} and the run's before from where it comes back up to the next: {taken:?}");
                }
            }
        }
        let (made, afresh) = (new.take_made(), fresh.take_made());
        assert!(made > 0 && made * 2 < afresh, "{made} layers built against {afresh} afresh");
    }

    /// An edit that adds layers on top of a 10 mm column, 30 layers to 40: every layer of its
    /// volumes is as built afresh, and each layer the new ones do not reach (the collision's top
    /// gap under them) is the run before's, shared: the collision, and each avoidance from the
    /// bottom up.
    #[test]
    fn an_edit_that_adds_layers_on_top_takes_the_volumes_under_them() {
        let s = r14(0.35);
        let (had, n) = (30, 40);
        let column = |n: usize| -> Vec<Vec<ExPolygon>> { (0..n).map(|_| vec![rect(0.0, 0.0, 10.0, 10.0)]).collect() };
        let kinds = [(Kind::Slow, To::Bed), (Kind::Slow, To::Model), (Kind::Fast, To::Bed), (Kind::Fast, To::Model), (Kind::FastHoleFree, To::Bed), (Kind::FastHoleFree, To::Model)];
        let old = volumes(&s, &column(had), &[]);
        for rung in 0..3 {
            for &(kind, to) in &kinds {
                old.avoidance(ctl(), kind, to, rung).blocked(had - 1).unwrap();
            }
        }
        let new = volumes(&s, &column(n), &[]).taking(&old);
        let fresh = volumes(&s, &column(n), &[]);
        let shared = |a: &[ExPolygon], b: &[ExPolygon]| !a.is_empty() && std::ptr::eq(a, b);
        let under = had - s.top_gap_layers;
        for rung in 0..3 {
            for l in (0..n).rev() {
                let c = new.collision(ctl(), l, rung).unwrap();
                assert!(c == fresh.collision(ctl(), l, rung).unwrap(), "rung {rung} layer {l}: the collision as built afresh");
                assert!(l >= had || shared(c, old.collision(ctl(), l, rung).unwrap()) == (l < under), "rung {rung} layer {l}: the collision the run's before under the new layers' reach alone");
            }
            for &(kind, to) in &kinds {
                let (a, f, o) = (new.avoidance(ctl(), kind, to, rung), fresh.avoidance(ctl(), kind, to, rung), old.avoidance(ctl(), kind, to, rung));
                assert!((0..n).all(|l| a.blocked(l).unwrap() == f.blocked(l).unwrap()), "{kind:?} to {to:?} at rung {rung}: as built afresh on every layer");
                let taken: Vec<bool> = (0..had).map(|l| shared(a.blocked(l).unwrap(), o.blocked(l).unwrap())).collect();
                assert!((0..had).all(|l| taken[l] == (l < under)), "{kind:?} to {to:?} at rung {rung}: the run's before under the new layers' reach: {taken:?}");
            }
        }
    }

    /// What a step of the descent reads holds only as it was read ([`Volumes::holds`]): the very
    /// layer, so on volumes that take it from these, and not on a fresh build of the same part,
    /// whose layer is another though the same, nor on volumes too short to have it; and a layer
    /// there was none of, the wall restriction on layer 0, none again. What builds a layer read is
    /// not read.
    #[test]
    fn a_read_holds_only_as_it_was_read() {
        let s = r14(0.35);
        let column = |n: usize| -> Vec<Vec<ExPolygon>> { (0..n).map(|_| vec![rect(0.0, 0.0, 10.0, 10.0)]).collect() };
        let v = volumes(&s, &column(40), &[]);
        let ((), reads) = reading(|| {
            v.avoidance(ctl(), Kind::Slow, To::Model, 1).blocked(30).unwrap();
            v.collision(ctl(), 30, 1).unwrap();
            v.collision(ctl(), 30, 1).unwrap();
            v.wall_restriction(ctl(), 0, 1, true).unwrap();
        });
        assert_eq!(reads.len(), 3, "each layer read once, and none of what built them: {reads:?}");
        let read = v.now(ctl(), reads).unwrap();
        assert_eq!(read.0.iter().filter(|(_, l)| l.is_none()).count(), 1, "the wall restriction on layer 0 is none");
        assert!(v.holds(ctl(), &read).unwrap(), "on the volumes it read");
        assert!(volumes(&s, &column(40), &[]).taking(&v).holds(ctl(), &read).unwrap(), "on volumes that take its layers");
        assert!(!volumes(&s, &column(40), &[]).holds(ctl(), &read).unwrap(), "not on a fresh build of the same part");
        assert!(!volumes(&s, &column(20), &[]).taking(&v).holds(ctl(), &read).unwrap(), "not on volumes with no layer 30");
    }

    /// A run cut short leaves the layers it did not build to the run after it as the run before it
    /// built them, as far as neither edit changed what they read. What the first run read high up a
    /// column holds on the third's volumes, which come out as a fresh build's for under half its
    /// work; what it read on the edited layer does not. For an edit of the top layer, under which
    /// nothing changes; for two edits of a low layer, over whose reach the layers are the first's;
    /// and for an edit of a low layer whose run is cut while its avoidance's top layer is not the
    /// first's yet, over which the third builds until it is.
    #[test]
    fn a_run_cut_short_leaves_the_layers_it_did_not_build_as_they_were() {
        let s = r14(0.35);
        // A column 40 layers high, its layer `at` `w` mm wide.
        let column = |at: usize, w: f64| -> Vec<Vec<ExPolygon>> { (0..40).map(|l| vec![rect(0.0, 0.0, if l == at { w } else { 10.0 }, 10.0)]).collect() };
        let first = volumes(&s, &column(0, 10.0), &[]);
        let read = |l: usize| {
            let ((), reads) = reading(|| {
                for &kind in &KINDS {
                    for to in [To::Bed, To::Model] {
                        first.avoidance(ctl(), kind, to, 1).blocked(l).unwrap();
                    }
                }
                first.collision(ctl(), l, 1).unwrap();
            });
            first.now(ctl(), reads).unwrap()
        };
        let top = read(39);
        for (what, cut_part, raised, part, under, edited) in [
            ("the top layer edited", column(39, 12.0), None, column(39, 12.0), 20, &top),
            ("a low layer edited twice", column(2, 10.4), None, column(2, 10.8), 30, &read(2)),
            ("cut short over a low edit", column(2, 12.0), Some(10), column(2, 12.0), 30, &read(2)),
        ] {
            let cut = volumes(&s, &cut_part, &[]).taking(&first);
            if let Some(l) = raised {
                cut.avoidance(ctl(), Kind::Slow, To::Bed, 1).blocked(l).unwrap();
                assert!(!cut.avoidance[cut.avoidance_at(Kind::Slow, To::Bed, 1)].built.lock().unwrap().1, "{what}: its top layer is not the first's");
            }
            let third = volumes(&s, &part, &[]).taking(&cut);
            assert!(third.holds(ctl(), &read(under)).unwrap(), "{what}: the first run's layers high up");
            assert!(!third.holds(ctl(), edited).unwrap(), "{what}: the edited layer built afresh");
            let fresh = volumes(&s, &part, &[]);
            for l in 0..40 {
                assert_eq!(third.collision(ctl(), l, 1).unwrap(), fresh.collision(ctl(), l, 1).unwrap(), "{what}: layer {l}'s collision as a fresh build's");
                for &kind in &KINDS {
                    for to in [To::Bed, To::Model] {
                        assert_eq!(third.avoidance(ctl(), kind, to, 1).blocked(l).unwrap(), fresh.avoidance(ctl(), kind, to, 1).blocked(l).unwrap(), "{what}: layer {l}'s {kind:?} avoidance to {to:?} as a fresh build's");
                    }
                }
            }
            let (made, afresh) = (third.take_made(), fresh.take_made());
            assert!(2 * made < afresh, "{what}: {made} layers built, a fresh build {afresh}");
        }
    }

    /// §3.4: the restriction going from layer l to l − 1 is the collision on l at radius 0 of the
    /// xy kind where the collision on l − 1 at the parent's rung also is.
    #[test]
    fn the_wall_restriction_is_the_collision_on_both_layers() {
        let s = r14(0.35);
        // A post on layers 0-1, nothing on 2-3, a block from layer 4 up.
        let part: Vec<Vec<ExPolygon>> = (0..8).map(|l| if l < 2 { vec![rect(0.0, 0.0, 10.0, 10.0)] } else if l >= 4 { vec![rect(20.0, 0.0, 30.0, 10.0)] } else { Vec::new() }).collect();
        let v = volumes(&s, &part, &[]);
        let w = v.wall_restriction(ctl(), 4, 1, false).unwrap();
        assert!(inside(w, 25.0, 5.0), "where both layers' collisions are");
        assert!(inside(v.collision(ctl(), 3, 1).unwrap(), 30.30, 5.0) && !inside(w, 30.30, 5.0), "not past xy_min of the upper layer's part");
        assert!(inside(v.wall_restriction(ctl(), 4, 1, true).unwrap(), 30.30, 5.0), "at full xy radius 0 is the xy delta: 0.14 + 0.21");
        assert!(v.wall_restriction(ctl(), 0, 1, false).unwrap().is_empty(), "no layer under the bed");
    }
}
