//! The planner as a dataflow of small tasks. A task makes one stage's result
//! for one layer of a region stream (or one island of it, a block of layers,
//! or the whole part) from the results of other tasks at fixed layer offsets.
//! A layer's plan therefore never depends on which other layers are planned
//! with it, and N layers plan at once on N threads with no barrier between
//! passes.
//!
//! [`PartPlan`] holds a part's inputs and its results as they are made; a
//! result is dropped as soon as the last task that reads it has finished.
//! [`Sched`] is a passive scheduler over the tasks a set of targets needs
//! (their cone): the caller pops a task with `next`, runs it with
//! [`PartPlan::run`] and reports it `done`. A layer's contours the caller put before
//! the scheduler was made ([`PartPlan::put_trace`]) are taken as made; the others it
//! cuts itself, as tasks like the others.
//!
//! [`Plate`] runs several parts' schedulers on one worker set in one claim order:
//! the most urgent ready task of any part runs next, so parts, layers and stages
//! share the threads with no barrier between them. [`plan`] is a plate of one
//! part, behind `plan_one` and `plan_window`.

use crate::*;
use crate::given::{GivenConcentric, GivenIsland, GivenLayer, GivenUse};
use makepad_fab_slicer::par::tick;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Condvar, Mutex, OnceLock};

/// What a task makes. Per layer unless noted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// The layer's contours: put by the caller before the scheduler is made
    /// ([`PartPlan::put_trace`]), else cut from the part's source ([`PartPlan::cut`]).
    Trace,
    /// The part's outline: XY compensation and the non-planar cap clip.
    Outline,
    /// Paint's region streams over the whole part (a painted part, not paint by position). Part.
    Regions,
    /// One island's wall rings or beads and the fill region inside them. Island; per layer a group.
    Walls,
    /// The wall paths: seams, speeds, texture, overhang split, arc clip.
    Paths,
    /// What the layers above and below leave uncovered.
    Faces,
    /// Those clipped to the fill, their shell projections, the grown external bridges.
    Skin,
    /// Internal solid shells and the sparse rest.
    Shells,
    /// Narrow vertical strips and internal bridges.
    Bridges,
    /// The final classes, with the floating vertical shells under bridges (or the source's).
    Classes,
    /// A block of layers' shared data for cross-layer sparse infill. Block.
    Aux,
    /// One island's fills. Island; per layer a group.
    Infill,
    /// The finished layer (paint by position: of every filament). Per stream; for every stream a group.
    Layer,
    /// A vase's ramp over every layer. Part.
    Vase,
    /// Tree and normal supports. Part.
    Supports,
    /// The finished part: supports merged, raft, brim keep-out, non-planar blocks. Part.
    Finish,
}

impl Stage {
    /// The stage's name in the bench's spans.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Trace => "trace",
            Stage::Outline => "outline",
            Stage::Regions => "regions",
            Stage::Walls => "walls",
            Stage::Paths => "paths",
            Stage::Faces => "faces",
            Stage::Skin => "skin",
            Stage::Shells => "shells",
            Stage::Bridges => "bridges",
            Stage::Classes => "classes",
            Stage::Aux => "aux",
            Stage::Infill => "infill",
            Stage::Layer => "layer",
            Stage::Vase => "vase",
            Stage::Supports => "supports",
            Stage::Finish => "finish",
        }
    }
}

/// One task: a stage at a stream, layer (a block for [`Stage::Aux`]) and island.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Task {
    pub stage: Stage,
    pub stream: u16,
    pub layer: u32,
    pub island: u32,
}

/// The island of a group over every island of a layer.
pub const EVERY: u32 = u32::MAX;
/// The stream of a group over every stream.
pub const EVERY_STREAM: u16 = u16::MAX;

impl Task {
    pub fn layer(stage: Stage, stream: u16, k: usize) -> Self {
        Task { stage, stream, layer: k as u32, island: 0 }
    }
    pub fn part(stage: Stage) -> Self {
        Task { stage, stream: 0, layer: 0, island: 0 }
    }
    fn island(stage: Stage, stream: u16, k: usize, i: usize) -> Self {
        Task { stage, stream, layer: k as u32, island: i as u32 }
    }
    /// Layer `k`'s contours.
    pub fn trace(k: usize) -> Self {
        Task::layer(Stage::Trace, 0, k)
    }
    /// Layer `k`'s finished layers of every stream.
    pub fn finished_layer(k: usize) -> Self {
        Task { stage: Stage::Layer, stream: EVERY_STREAM, layer: k as u32, island: 0 }
    }
    fn is_group(self) -> bool {
        matches!(self.stage, Stage::Walls | Stage::Infill) && self.island == EVERY || self.stage == Stage::Layer && self.stream == EVERY_STREAM
    }
    fn k(self) -> usize {
        self.layer as usize
    }
    /// The layer its span names: none for a part's or a block's task.
    fn span_layer(self) -> u32 {
        match self.stage {
            Stage::Regions | Stage::Vase | Stage::Supports | Stage::Finish | Stage::Aux => makepad_fab_slicer::par::NO_LAYER,
            _ => self.layer,
        }
    }
}

/// A result, shared by the tasks that read it until it is dropped.
struct Slot<T>(Mutex<Option<Arc<T>>>);

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot(Mutex::new(None))
    }
}

impl<T> Slot<T> {
    fn put(&self, v: T) {
        self.put_arc(Arc::new(v));
    }
    fn put_arc(&self, v: Arc<T>) {
        *self.0.lock().unwrap() = Some(v);
    }
    fn get(&self) -> Arc<T> {
        self.0.lock().unwrap().clone().expect("a result read before it is made or after it is dropped")
    }
    fn is_set(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
    fn clear(&self) {
        self.0.lock().unwrap().take();
    }
}

impl<T: Clone> Slot<T> {
    /// The result moved out, for its last reader.
    fn take(&self) -> T {
        let v = self.0.lock().unwrap().take().expect("a result taken before it is made or twice");
        Arc::try_unwrap(v).unwrap_or_else(|v| (*v).clone())
    }
}

/// A traced layer, and what its GPU stages already made of it.
struct Traced {
    contours: Vec<ExPolygon>,
    given: Option<Arc<GivenLayer>>,
}

/// A stream's regions on a layer, and what its walls are made from (on the
/// first layer, what elephant-foot compensation leaves of them).
struct Base {
    raw: Arc<Vec<ExPolygon>>,
    walls_in: Arc<Vec<ExPolygon>>,
}

/// What the layer above (`top`) and below (`bottom`) leave uncovered.
#[derive(Default)]
struct Faces {
    top: Vec<ExPolygon>,
    bottom: Vec<ExPolygon>,
}

#[derive(Default)]
struct Skin {
    /// The fill regions inside the walls, every island's.
    fill: Vec<ExPolygon>,
    /// The faces clipped to the fill; `bottom` before its bridges grow.
    top: Vec<ExPolygon>,
    bottom: Vec<ExPolygon>,
    /// The faces as the shells of the layers below / above take them.
    top_projected: Vec<ExPolygon>,
    bottom_projected: Vec<ExPolygon>,
    /// Whether the faces were empty before clipping.
    top_face: bool,
    bottom_face: bool,
    /// The bottom grown into the supported area (external bridges).
    grown: Vec<ExPolygon>,
}

#[derive(Default)]
struct Shells {
    solid: Vec<ExPolygon>,
    sparse: Vec<ExPolygon>,
}

#[derive(Default)]
struct Bridges {
    vertical: Vec<ExPolygon>,
    solid: Vec<ExPolygon>,
    bridge: Vec<ExPolygon>,
}

/// A layer's islands: the fill region inside the walls and the finished walls.
#[derive(Clone)]
struct Paths {
    /// Per island the fill region, the walls and the gap fill.
    islands: Vec<(Vec<ExPolygon>, Vec<ExtrusionPath>, Vec<ExtrusionPath>)>,
    /// Arc overhangs and where their fronts anchor.
    arc_ov: Vec<ExPolygon>,
    arc_supported: Vec<ExPolygon>,
}

/// An island's fills, and its ironing (after any arc fronts it owns).
#[derive(Clone)]
struct Fills {
    fills: Vec<ExtrusionPath>,
    ironing: Vec<ExtrusionPath>,
    /// The island closes its part's inside in on every side: its fills are core, its inner
    /// walls their lining (`ExtrusionPath::core`).
    closed: bool,
}

#[derive(Default)]
struct LayerSlots {
    base: Slot<Base>,
    walls: OnceLock<Vec<Slot<IslandWalls>>>,
    paths: Slot<Paths>,
    faces: Slot<Faces>,
    skin: Slot<Skin>,
    shells: Slot<Shells>,
    bridges: Slot<Bridges>,
    classes: Slot<Classes>,
    infill: OnceLock<Vec<Slot<Fills>>>,
    layer: Slot<Vec<ObjectLayer>>,
}

/// One region stream: a filament's regions planned through every per-layer stage.
struct Stream {
    filament: usize,
    p: Params,
    layers: Vec<LayerSlots>,
    /// Per block: its first layer, and the part's own sparse's cross-layer data, then each of its
    /// regions' fills' ([`PartPlan::fills`]).
    aux: Vec<Slot<(usize, Vec<infill::Aux>)>>,
}

impl Stream {
    fn new(filament: usize, p: Params, n: usize) -> Self {
        Stream { filament, p, layers: (0..n).map(|_| LayerSlots::default()).collect(), aux: (0..n / AUX_BLOCK + 1).map(|_| Slot::default()).collect() }
    }
}

/// One part's plan as a dataflow: its inputs, and every result while a task still reads it.
pub struct PartPlan<'a> {
    oi: usize,
    o: &'a ObjectInput,
    /// The job as given, and with the part's overrides.
    source_job: Job,
    job: Job,
    params: Params,
    heights: Vec<(f64, f64)>,
    center: Pt,
    radius: f64,
    painted: bool,
    paint_at: Option<makepad_fab_slicer::source::PaintAt>,
    /// Its regions with print settings of their own (`m.settings`), in order; a vase has none.
    zones: Vec<Zone>,
    /// The sparse fills its regions may print but its own and solid ([`region_fills`]): what
    /// [`Classes::zoned`] indexes.
    fills: Vec<infill::SparseFill>,
    /// Per layer, the regions' cells at its cut ([`Self::cells_at`]): made with the layer's
    /// outline, when the part has regions.
    zone_at: Vec<Slot<Vec<(Vec<ExPolygon>, ZoneSet)>>>,
    /// A vase ramps every layer once all are planned.
    vase: bool,
    xy: XyComp,
    np: Option<Arc<makepad_fab_slicer::nonplanar::NpPlan>>,
    lift: f64,
    traces: Vec<Slot<Traced>>,
    /// Every layer's classes are given ([`Self::with_given_classes`]): no class stage runs.
    given_classes: bool,
    /// What each layer's stages made that a GPU can give ([`Self::with_capture`]).
    capture: Option<Vec<Mutex<GivenLayer>>>,
    used: Mutex<GivenUse>,
    outline: Vec<Slot<Vec<ExPolygon>>>,
    streams: OnceLock<Vec<Stream>>,
    /// Finished layers put before the streams are known (stream, layer, layers): they go in
    /// when [`Stage::Regions`] splits the part. Its lock orders a put against that split.
    finished_early: Mutex<Vec<(usize, usize, Vec<ObjectLayer>)>>,
    ramp: Slot<Vec<(f64, f64, ObjectLayer)>>,
    supports: Slot<PlannedSupports>,
    finished: Slot<ObjectPlan>,
}

impl<'a> PartPlan<'a> {
    /// Object `oi` (`o`) over `heights` (its [`plan_heights`]) under `job`, before its overrides.
    pub fn new(oi: usize, o: &'a ObjectInput, job: &Job, heights: Vec<(f64, f64)>) -> Self {
        let own = if o.overrides.is_empty() { job.clone() } else { job.with_overrides(&o.overrides) };
        let params = Params::from_job(&own, o.filament);
        let (min, max) = o.source.pivot_box();
        let center = Pt::from_mm((min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0);
        let radius = 0.5 * (max[0] - min[0]).max(max[1] - min[1]).max(0.0);
        let painted = o.source.painted();
        let paint_at = o.source.paint_at();
        let n = heights.len();
        let streams = OnceLock::new();
        if !painted || paint_at.is_some() {
            let _ = streams.set(vec![Stream::new(o.filament, params.clone(), n)]);
        }
        let xy = XyComp::of(&own, o);
        let lift = if own.cfg.i64_at("raft_layers", 0) > 0 { params.raft_lift } else { 0.0 };
        let zones: Vec<Zone> = if params.exp.vase.is_some() { Vec::new() } else { o.source.settings_zones().into_iter().map(|z| Zone { set: ZoneSet::of(&z.values), region: z.region }).collect() };
        let fills = region_fills(&params, &zones);
        let zone_at = (0..if zones.is_empty() { 0 } else { n }).map(|_| Slot::default()).collect();
        PartPlan {
            oi,
            vase: params.exp.vase.is_some() && !painted,
            np: o.source.np_plan(),
            o,
            source_job: job.clone(),
            job: own,
            params,
            center,
            radius,
            painted,
            paint_at,
            zones,
            fills,
            zone_at,
            xy,
            lift,
            traces: (0..n).map(|_| Slot::default()).collect(),
            given_classes: false,
            capture: None,
            used: Mutex::default(),
            outline: (0..n).map(|_| Slot::default()).collect(),
            streams,
            finished_early: Mutex::default(),
            heights,
            ramp: Slot::default(),
            supports: Slot::default(),
            finished: Slot::default(),
        }
    }

    /// The part's region streams, once known (a painted part's after [`Stage::Regions`]).
    pub fn streams(&self) -> Option<usize> {
        self.streams.get().map(Vec::len)
    }

    /// Every layer's classes come with its trace ([`GivenLayer::classes`]): the class stages do
    /// not run, and a layer put without them is an error. Classes reach across layers, so a
    /// part gives all of them or none.
    pub fn with_given_classes(mut self, given: bool) -> Self {
        self.given_classes = given;
        self
    }

    /// Records what each layer's stages make that a GPU can give ([`Self::captured`]).
    pub fn with_capture(mut self) -> Self {
        self.capture = Some((0..self.heights.len()).map(|_| Mutex::default()).collect());
        self
    }

    /// Per layer, what its stages made that a GPU can give, as they ran ([`Self::with_capture`]).
    /// Put back with the traces, it plans the part as it was planned.
    pub fn captured(&self) -> Vec<GivenLayer> {
        self.capture.as_ref().expect("a capturing plan").iter().map(|l| l.lock().unwrap().clone()).collect()
    }

    /// The given pieces the stages took so far.
    pub fn given_use(&self) -> GivenUse {
        *self.used.lock().unwrap()
    }

    /// Layer `k`'s contours (cut at its [`cut_z`]), and what its GPU stages made of it. A contour
    /// that touches itself is split there first ([`split_pinches`]): no step sees a zero-width
    /// neck. Given islands are those the walls are made from, in order: a part whose stages
    /// change its islands (paint, a non-planar cap) or that has regions' settings takes none.
    pub fn put_trace(&self, k: usize, contours: Vec<ExPolygon>, given: Option<Arc<GivenLayer>>) {
        assert!(given.is_none() || !self.region_paint() && self.np.is_none() && self.zones.is_empty(), "layer {k}: given islands of a part whose islands the planner changes");
        assert!(!self.given_classes || given.as_ref().is_some_and(|g| g.classes.is_some()), "layer {k} has no classes and its part gives every layer's");
        self.traces[k].put(Traced { contours: split_pinches(contours), given });
    }

    /// What layer `k`'s GPU stages made of it, when its trace came with that.
    fn given(&self, k: usize) -> Option<Arc<GivenLayer>> {
        self.traces[k].get().given.clone()
    }

    /// Records into layer `k`'s capture.
    fn capture(&self, k: usize, f: impl FnOnce(&mut GivenLayer)) {
        if let Some(c) = &self.capture {
            f(&mut c[k].lock().unwrap());
        }
    }

    fn count(&self, f: impl FnOnce(&mut GivenUse)) {
        f(&mut self.used.lock().unwrap());
    }

    /// Layer `k`'s contours cut from the part's source, for a scheduler that cuts its traces
    /// ([`Sched::cutting`]); [`Self::put_trace`] takes them.
    pub fn cut(&self, k: usize) -> Vec<ExPolygon> {
        let _t = prof::scope(prof::Stage::Contours);
        let (z, h) = self.heights[k];
        self.o.source.contours(&[cut_z(&self.params, z, h)]).pop().expect("one plane cut")
    }

    /// The part's region streams as soon as they are known without planning: paint by
    /// position makes one per filament of the job, an unpainted part one.
    pub fn regions_known(&self) -> Option<usize> {
        if self.paint_at.is_some() {
            let nf = self.job.filaments().max(1);
            return Some(std::iter::once(self.o.filament).chain((0..nf).filter(|&f| f != self.o.filament)).count());
        }
        self.streams()
    }

    /// What makes model layers `layers` (none: nothing): a vase's ramp (it plans every layer at
    /// once), else each layer's finished layers.
    pub fn layer_targets(&self, layers: impl IntoIterator<Item = usize>) -> Vec<Task> {
        let mut layers = layers.into_iter().peekable();
        if self.vase {
            return layers.peek().map(|_| Task::part(Stage::Vase)).into_iter().collect();
        }
        layers.map(Task::finished_layer).collect()
    }

    /// A finished layer of stream `s` (every filament's, by paint by position), once its
    /// [`Stage::Layer`] task is done and until it is taken.
    pub fn finished_layers(&self, s: usize, k: usize) -> Arc<Vec<ObjectLayer>> {
        self.stream(s).layers[k].layer.get()
    }

    /// Stream `s`'s finished layers of layer `k` (what [`Self::finished_layers`] gives), made
    /// elsewhere: a band planned on its own. A scheduler made after takes them as done and runs
    /// nothing that only they read; what the layers beside them read of layer `k` is still
    /// planned. Nor does a plate emit them. A region-painted part's streams are known only once
    /// its [`Stage::Regions`] ran: layers put before go in then. A vase ramps its layers whole,
    /// so it takes none.
    pub fn put_finished(&self, s: usize, k: usize, layers: Vec<ObjectLayer>) {
        assert!(!self.vase, "layer {k}: finished layers put into a vase, which ramps them whole");
        let mut early = self.finished_early.lock().unwrap();
        match self.streams.get() {
            Some(streams) => {
                assert!(s < streams.len(), "layer {k}: stream {s} of {}", streams.len());
                streams[s].layers[k].layer.put(layers);
            }
            None => early.push((s, k, layers)),
        }
    }

    /// [`Self::finished_layers`], taken (once).
    pub fn take_layer(&self, s: usize, k: usize) -> Vec<ObjectLayer> {
        self.stream(s).layers[k].layer.take()
    }

    /// The part's supports, from its [`Emit::Supports`] until its [`Stage::Finish`] takes them.
    pub fn supports(&self) -> Arc<PlannedSupports> {
        self.supports.get()
    }

    /// The finished part, after [`Stage::Finish`].
    pub fn take_finished(&self) -> ObjectPlan {
        self.finished.take()
    }

    fn stream(&self, s: usize) -> &Stream {
        &self.streams.get().expect("the streams are known")[s]
    }

    fn region_paint(&self) -> bool {
        self.painted && self.paint_at.is_none()
    }

    /// What task `t` reads: every result it takes, so that each is kept until it ran.
    fn deps(&self, t: Task) -> Vec<Task> {
        let n = self.heights.len();
        let k = t.k();
        let s = t.stream;
        let outline = |j: usize| Task::layer(Stage::Outline, 0, j);
        let trace = |j: usize| Task::layer(Stage::Trace, 0, j);
        let base = |j: usize| if self.region_paint() { Task::part(Stage::Regions) } else { outline(j) };
        let at = |stage: Stage, j: usize| Task::layer(stage, s, j);
        let group = |stage: Stage, j: usize| Task::island(stage, s, j, EVERY as usize);
        let mut d = Vec::new();
        match t.stage {
            Stage::Trace => {}
            Stage::Outline => d.push(trace(k)),
            Stage::Supports => d.extend((0..n).map(outline)),
            // The streams' tasks it adds read the traces and outlines it holds.
            Stage::Regions => d.extend((0..n).flat_map(|j| [outline(j), trace(j)])),
            Stage::Walls if t.island == EVERY => {
                d.extend([base(k), trace(k)]);
                if k + 1 < n {
                    d.push(outline(k + 1));
                }
                // the regions' wall counts
                if !self.zones.is_empty() {
                    d.push(outline(k));
                }
            }
            Stage::Walls | Stage::Infill => {}
            Stage::Paths => {
                let p = &self.stream(s as usize).p;
                let seams = if p.seam_mode == SeamMode::Random { k } else { k.saturating_sub(SEAM_WINDOW) };
                d.extend((seams..=k).map(|j| group(Stage::Walls, j)));
                // and the given labels of its layer
                d.extend([base(seams), base(k), trace(k)]);
                if k > 0 {
                    d.push(outline(k - 1));
                }
            }
            Stage::Faces => {
                d.push(base(k));
                if k > 0 {
                    d.push(outline(k - 1));
                }
                if k + 1 < n {
                    d.push(outline(k + 1));
                }
            }
            Stage::Skin => d.extend([at(Stage::Faces, k), group(Stage::Walls, k)]),
            Stage::Shells => {
                let p = &self.stream(s as usize).p;
                let lo = k.saturating_sub(p.bottom_layers.saturating_sub(1));
                let hi = (k + p.top_layers.max(1) - 1).min(n - 1);
                d.extend((lo..=hi).map(|j| at(Stage::Skin, j)));
                // the regions that print solid
                if !self.zones.is_empty() {
                    d.push(outline(k));
                }
            }
            Stage::Bridges => {
                d.push(at(Stage::Shells, k));
                if k > 0 {
                    d.push(at(Stage::Shells, k - 1));
                }
            }
            Stage::Classes if self.given_classes => d.push(trace(k)),
            Stage::Classes => {
                d.extend([at(Stage::Bridges, k), at(Stage::Shells, k), at(Stage::Skin, k), trace(k)]);
                if k + 1 < n {
                    d.push(at(Stage::Bridges, k + 1));
                }
                // the regions' sparse
                if !self.zones.is_empty() {
                    d.push(outline(k));
                }
            }
            Stage::Aux => {
                let (a, b) = aux_range(k, n);
                // the fill as the skin holds it, or (no class stages) as the walls left it
                let fill = |j: usize| if self.given_classes { group(Stage::Walls, j) } else { at(Stage::Skin, j) };
                d.extend((a..b).flat_map(|j| [at(Stage::Classes, j), fill(j)]));
                // the regions' cells
                if !self.zones.is_empty() {
                    d.extend((a..b).map(outline));
                }
            }
            Stage::Layer if s == EVERY_STREAM => {
                if self.region_paint() {
                    d.push(Task::part(Stage::Regions));
                }
            }
            Stage::Layer => {
                d.extend([group(Stage::Infill, k), at(Stage::Paths, k), base(k)]);
                if k == 0 {
                    d.push(outline(0));
                }
                // a painted layer's inside: the outlines within its skin above and below
                if let Some((lo, hi)) = self.paint_window(k) {
                    d.extend((lo..=hi).map(outline));
                }
            }
            Stage::Vase => d.extend((0..n).map(Task::finished_layer)),
            Stage::Finish => {
                if self.vase {
                    d.push(Task::part(Stage::Vase));
                } else {
                    d.extend((0..n).map(Task::finished_layer));
                }
                d.push(Task::part(Stage::Supports));
                if n > 0 {
                    d.push(outline(0));
                }
            }
        }
        if t.stage == Stage::Infill && t.island == EVERY {
            let p = &self.stream(s as usize).p;
            d.extend([at(Stage::Classes, k), at(Stage::Paths, k), trace(k)]);
            if k + 1 < n {
                // the final bridges above: the given classes carry them
                d.extend([at(if self.given_classes { Stage::Classes } else { Stage::Bridges }, k + 1), trace(k + 1)]);
            }
            if k > 0 {
                d.push(outline(k - 1));
            }
            // the regions that print solid: a wall to the sparse round them
            if !self.zones.is_empty() {
                d.push(outline(k));
            }
            if p.sparse_pattern.cross_layer() {
                d.push(Task::layer(Stage::Aux, s, k / AUX_BLOCK));
            }
        }
        d.sort();
        d.dedup();
        d
    }

    /// The tasks a group stands for, once what it reads is made.
    fn children(&self, t: Task) -> Vec<Task> {
        let k = t.k();
        match t.stage {
            Stage::Walls => {
                let base = self.stream(t.stream as usize).layers[k].base.get();
                let n = base.walls_in.len();
                if let Some(g) = self.given(k) {
                    assert_eq!(g.islands.len(), n, "layer {k}: one given island per island its walls are made from");
                }
                self.capture(k, |c| c.islands.resize(n, GivenIsland::default()));
                let _ = self.stream(t.stream as usize).layers[k].walls.set((0..n).map(|_| Slot::default()).collect());
                (0..n).map(|i| Task::island(Stage::Walls, t.stream, k, i)).collect()
            }
            Stage::Infill => {
                let n = self.stream(t.stream as usize).layers[k].paths.get().islands.len();
                let _ = self.stream(t.stream as usize).layers[k].infill.set((0..n).map(|_| Slot::default()).collect());
                (0..n).map(|i| Task::island(Stage::Infill, t.stream, k, i)).collect()
            }
            Stage::Layer => (0..self.streams.get().expect("the streams are known").len()).map(|s| Task::layer(Stage::Layer, s as u16, k)).collect(),
            _ => unreachable!("{t:?} is no group"),
        }
    }

    /// Drop what `t` made (its last reader finished).
    fn release(&self, t: Task) {
        let k = t.k();
        let layer = |s: u16| &self.stream(s as usize).layers[k];
        match t.stage {
            Stage::Trace => self.traces[k].clear(),
            Stage::Outline => {
                self.outline[k].clear();
                if let Some(z) = self.zone_at.get(k) {
                    z.clear();
                }
                if !self.region_paint() {
                    layer(0).base.clear();
                }
            }
            Stage::Regions => {
                for st in self.streams.get().into_iter().flatten() {
                    for l in &st.layers {
                        l.base.clear();
                    }
                }
            }
            Stage::Walls => layer(t.stream).walls.get().into_iter().flatten().for_each(Slot::clear),
            Stage::Paths => layer(t.stream).paths.clear(),
            Stage::Faces => layer(t.stream).faces.clear(),
            Stage::Skin => layer(t.stream).skin.clear(),
            Stage::Shells => layer(t.stream).shells.clear(),
            Stage::Bridges => layer(t.stream).bridges.clear(),
            Stage::Classes => layer(t.stream).classes.clear(),
            Stage::Aux => self.stream(t.stream as usize).aux[k].clear(),
            Stage::Infill => layer(t.stream).infill.get().into_iter().flatten().for_each(Slot::clear),
            Stage::Layer => {
                for st in self.streams.get().into_iter().flatten() {
                    st.layers[k].layer.clear();
                }
            }
            Stage::Vase => self.ramp.clear(),
            Stage::Supports => self.supports.clear(),
            Stage::Finish => self.finished.clear(),
        }
    }

    /// Whether task `t`'s result is in without it running: a trace, or finished layers, put
    /// ([`Self::put_trace`], [`Self::put_finished`]); a layer of every stream once each is.
    fn has(&self, t: Task) -> bool {
        let k = t.k();
        match t.stage {
            Stage::Trace => self.traces[k].is_set(),
            Stage::Layer => self.streams.get().is_some_and(|streams| match t.stream {
                EVERY_STREAM => streams.iter().all(|st| st.layers[k].layer.is_set()),
                s => streams[s as usize].layers[k].layer.is_set(),
            }),
            _ => false,
        }
    }

    /// Run task `t`; what it reads is made. `cancel` is asked between its parts.
    pub fn run(&self, t: Task, cancel: &(dyn Fn() -> bool + Sync)) -> Result<(), par::Cancelled> {
        if cancel() {
            return Err(par::Cancelled);
        }
        let k = t.k();
        let s = t.stream as usize;
        match t.stage {
            Stage::Trace => unreachable!("traces are put, not run"),
            Stage::Outline => self.outline_at(k),
            Stage::Regions => self.regions(cancel)?,
            Stage::Walls => {
                let _t = prof::scope(prof::Stage::Walls);
                self.walls(s, k, t.island as usize);
            }
            Stage::Paths => {
                let _t = prof::scope(prof::Stage::Walls);
                self.paths(s, k);
            }
            Stage::Faces | Stage::Skin | Stage::Shells | Stage::Bridges | Stage::Classes => {
                let _t = prof::scope(prof::Stage::Classes);
                self.classes(t.stage, s, k);
            }
            Stage::Aux => {
                let _t = prof::scope(prof::Stage::Infill);
                self.aux(s, k);
            }
            Stage::Infill => {
                let _t = prof::scope(prof::Stage::Infill);
                self.infill(s, k, t.island as usize);
            }
            Stage::Layer => {
                let _t = prof::scope(prof::Stage::Infill);
                self.layer(s, k);
            }
            Stage::Vase => {
                let mut layers = self.collect(0, self.heights.len());
                ramp::vase(&mut layers, &self.params, self.params.exp.vase.as_ref().unwrap(), self.center);
                self.ramp.put(layers);
            }
            Stage::Supports => {
                let outline: Vec<Vec<ExPolygon>> = self.outline.iter().map(|o| o.get().to_vec()).collect();
                let ctl = par::Ctl { cancel: Some(cancel), ..Default::default() };
                self.supports.put(plan_supports(ctl, self.oi, &self.o, &self.source_job, &outline, &self.heights, &mut tree::Cache::default())?);
            }
            Stage::Finish => {
                let layers = if self.vase { self.ramp.take() } else { self.collect(0, self.heights.len()) };
                let first = self.outline[0].get();
                let plan = finish_plan(&self.o, &self.source_job, &self.heights, std::slice::from_ref(&*first), layers, self.supports.take());
                self.finished.put(plan);
            }
        }
        Ok(())
    }

    fn outline_at(&self, k: usize) {
        let tr = self.traces[k].get();
        let compensated = tr.given.as_ref().is_some_and(|g| g.compensated);
        let xy = if compensated { XyComp::default() } else { self.xy };
        let out = Arc::new(outline_layer(xy, self.np.as_deref(), self.lift, self.heights[k].0, tr.contours.clone()));
        self.outline[k].put_arc(out.clone());
        if !self.zones.is_empty() {
            self.zone_at[k].put(self.cells_at(k));
        }
        if !self.region_paint() {
            let st = self.stream(0);
            let walls_in = if compensated { out.clone() } else { self.eroded(&st.p, k, &out) };
            st.layers[k].base.put(Base { raw: out, walls_in });
        }
    }

    /// The regions' cells at layer `k`'s cut, within the part's box: disjoint, each with what
    /// holds there, a later region's value of a key over an earlier's. Each region's own outline,
    /// cut exactly as the support paint is.
    fn cells_at(&self, k: usize) -> Vec<(Vec<ExPolygon>, ZoneSet)> {
        let (z, h) = self.heights[k];
        let cut = cut_z(&self.params, z, h);
        let part = self.o.source.bounds();
        let reach = ([part.0[0], part.0[1]], [part.1[0], part.1[1]]);
        let mut cells: Vec<(Vec<ExPolygon>, ZoneSet)> = Vec::new();
        for zone in &self.zones {
            let r = crate::support_paint::outline_at(&zone.region, cut, part, reach);
            if r.is_empty() {
                continue;
            }
            let mut next = Vec::with_capacity(2 * cells.len() + 1);
            let mut fresh = r.clone();
            for (area, set) in cells {
                let inside = intersection_ex(&area, &r);
                if inside.is_empty() {
                    next.push((area, set));
                    continue;
                }
                fresh = difference_ex(&fresh, &area);
                let outside = difference_ex(&area, &r);
                if !outside.is_empty() {
                    next.push((outside, set));
                }
                next.push((inside, set.then(zone.set)));
            }
            if !fresh.is_empty() {
                next.push((fresh, zone.set));
            }
            cells = next;
        }
        cells
    }

    /// Layer `k`'s regions that print solid, as one region.
    fn solid_zones(&self, k: usize) -> Vec<ExPolygon> {
        if self.zones.is_empty() {
            return Vec::new();
        }
        let cells = self.zone_at[k].get();
        let solid: Vec<ExPolygon> = cells.iter().filter(|(_, set)| set.fill(&self.params).density >= 1.0).flat_map(|(a, _)| a.iter().cloned()).collect();
        if solid.len() > 1 { union_ex(&solid) } else { solid }
    }

    /// Layer `k`'s regions by the sparse fill they print but the part's own and solid: each fill's
    /// index in [`Self::fills`] and where it holds, in that order.
    fn fill_cells(&self, k: usize) -> Vec<(usize, Vec<ExPolygon>)> {
        if self.fills.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(usize, Vec<ExPolygon>)> = Vec::new();
        for (area, set) in self.zone_at[k].get().iter() {
            let Some(fi) = self.fills.iter().position(|f| *f == set.fill(&self.params)) else { continue };
            match out.iter_mut().find(|(i, _)| *i == fi) {
                Some((_, at)) => *at = union_ex(&[std::mem::take(at), area.clone()].concat()),
                None => out.push((fi, area.clone())),
            }
        }
        out.sort_by_key(|(fi, _)| *fi);
        out
    }

    /// Layer `k`'s sparse infill (`sparse`) its regions fill otherwise: each fill's share (the
    /// regions that print solid took theirs into the solid already).
    fn zoned(&self, k: usize, sparse: &[ExPolygon]) -> Vec<(usize, Vec<ExPolygon>)> {
        if sparse.is_empty() {
            return Vec::new();
        }
        self.fill_cells(k).into_iter().map(|(fi, at)| (fi, intersection_ex(sparse, &at))).filter(|(_, a)| !a.is_empty()).collect()
    }

    /// Elephant-foot compensation belongs to the part's first layer.
    fn eroded(&self, p: &Params, k: usize, raw: &Arc<Vec<ExPolygon>>) -> Arc<Vec<ExPolygon>> {
        if k == 0 && p.first(0) && p.elephant_foot > 0.0 {
            Arc::new(foot_compensated(p, raw))
        } else {
            raw.clone()
        }
    }

    /// Paint discovers the part's region streams in first-occurrence order, so it splits
    /// every layer before any is planned; each region then plans like a part of its own.
    fn regions(&self, cancel: &(dyn Fn() -> bool + Sync)) -> Result<(), par::Cancelled> {
        let (job, params, o) = (&self.job, &self.params, &*self.o);
        let n = self.heights.len();
        let zs: Vec<f64> = self.heights.iter().map(|&(z, h)| cut_z(params, z, h)).collect();
        let slices: Vec<Vec<ExPolygon>> = self.outline.iter().map(|o| o.get().to_vec()).collect();
        let pp = makepad_fab_slicer::paint::PaintParams {
            top_layers: job.cfg.i64_at("top_color_penetration_layers", 0).max(0) as usize,
            bottom_layers: job.cfg.i64_at("bottom_color_penetration_layers", 0).max(0) as usize,
            shrink: params.w_outer + flow::spacing(params.w_inner, params.layer_height),
            max_width: job.f("mmu_segmented_region_max_width").max(0.0),
            interlocking_depth: job.f("mmu_segmented_region_interlocking_depth").max(0.0),
        };
        // The layers split on fan-out workers inside the call: the stage counts their time too.
        let mut split = prof::scope_fanned(prof::Stage::Paint, || o.source.paint_regions(&zs, &slices, &pp));
        if let Some(il) = &params.exp.interlock {
            split = par::try_map(par::Ctl { cancel: Some(cancel), ..Default::default() }, split.len(), |i| {
                let mut l = split[i].clone();
                interlock::apply(&mut l, i, il);
                l
            })?;
        }
        let nf = job.filaments().max(1);
        let fil_of = |st: u32| if st == 0 || st as usize > nf { o.filament } else { st as usize - 1 };
        let mut fils = vec![o.filament];
        for l in &split {
            for (st, _) in l {
                let f = fil_of(*st);
                if !fils.contains(&f) {
                    fils.push(f);
                }
            }
        }
        fils.retain(|&f| f == o.filament || split.iter().any(|l| l.iter().any(|(st, r)| fil_of(*st) == f && !r.is_empty())));
        let streams: Vec<Stream> = fils.into_iter().map(|f| {
            let st = Stream::new(f, Params::from_job(job, f), n);
            for (k, l) in split.iter().enumerate() {
                let parts: Vec<ExPolygon> = l.iter().filter(|(s, _)| fil_of(*s) == f).flat_map(|(_, r)| r.iter().cloned()).collect();
                let raw = Arc::new(if l.iter().filter(|(s, _)| fil_of(*s) == f).count() > 1 { union_ex(&parts) } else { parts });
                let walls_in = self.eroded(&st.p, k, &raw);
                st.layers[k].base.put(Base { raw, walls_in });
            }
            st
        }).collect();
        let mut early = self.finished_early.lock().unwrap();
        assert!(self.streams.set(streams).is_ok(), "the regions are split once");
        for (s, k, layers) in early.drain(..) {
            let streams = self.streams.get().unwrap();
            assert!(s < streams.len(), "layer {k}: stream {s} of {}", streams.len());
            streams[s].layers[k].layer.put(layers);
        }
        Ok(())
    }

    fn walls(&self, s: usize, k: usize, i: usize) {
        let st = self.stream(s);
        let p = &st.p;
        let out = &st.layers[k].walls.get().expect("the walls group expanded")[i];
        if let Some(w) = self.given(k).and_then(|g| g.islands[i].walls.clone()) {
            self.count(|u| u.walls += 1);
            self.capture(k, |c| c.islands[i].walls = Some(w.clone()));
            out.put_arc(w);
            return;
        }
        let base = st.layers[k].base.get();
        let island = &base.walls_in[i];
        let first = p.first(k);
        let n = self.heights.len();
        let above = if k + 1 < n { Some(self.outline[k + 1].get()) } else { None };
        let above = above.as_deref().map(Vec::as_slice);
        let cap = loops_cap(p, island, above, first);
        let loops = p.wall_loops.min(cap);
        let h = self.heights[k].1;
        let cells = (!self.zones.is_empty()).then(|| self.zone_at[k].get());
        let counts: Vec<(&[ExPolygon], usize)> = cells.iter().flat_map(|c| c.iter()).filter_map(|(a, set)| set.walls.map(|n| (a.as_slice(), n.min(cap)))).collect();
        let w = Arc::new(if counts.is_empty() { island_walls(p, std::slice::from_ref(island), loops, first, h) } else { zoned_walls(p, island, loops, &counts, first, h) });
        self.capture(k, |c| c.islands[i].walls = Some(w.clone()));
        out.put_arc(w);
    }

    fn island_walls(&self, s: usize, k: usize) -> Vec<Arc<IslandWalls>> {
        self.stream(s).layers[k].walls.get().expect("the walls group expanded").iter().map(Slot::get).collect()
    }

    /// Each layer restarts its own short seam chain from the layers below, then makes
    /// its walls' paths and finishes them.
    fn paths(&self, s: usize, k: usize) {
        let st = self.stream(s);
        let p = &st.p;
        let (z, h) = self.heights[k];
        let first = p.first(k);
        let (w_ext, w_in) = if first { (p.w_first, p.w_first) } else { (p.w_outer, p.w_inner) };
        let center = self.center;
        let mut seam_target = center;
        if p.seam_mode != SeamMode::Random {
            let s0 = k.saturating_sub(SEAM_WINDOW);
            seam_target = st.layers[s0].base.get().walls_in.iter().fold(None::<BBox>, |acc, e| {
                let mut b = acc.unwrap_or_else(BBox::empty);
                b.merge(&e.bbox());
                Some(b)
            }).map(|b| b.max).unwrap_or(center);
            for j in s0..k {
                let width = if p.first(j) { p.w_first } else { p.w_outer };
                for island in self.island_walls(s, j) {
                    if let Some(seam) = walls::shape_seam(&island.shape, seam_target, p.seam_mode, width, p.nozzle) {
                        seam_target = seam;
                    }
                }
            }
        }
        let tex_frame = p.exp.texture.as_ref().map(|t| texture::Frame::new(t, center.x_mm(), center.y_mm(), self.radius));
        let raw = st.layers[k].base.get().raw.clone();
        let lower = if k > 0 { Some(self.outline[k - 1].get()) } else { None };
        // Arc overhangs: what hangs out past the layer below gets fronts, owned by the nearest island.
        let arc_ov: Vec<ExPolygon> = match (p.exp.arc.as_ref(), &lower) {
            (Some(a), Some(lower)) => arcfront::overhang(&raw, lower, a, p.w_outer),
            _ => Vec::new(),
        };
        let arc_supported = match (p.exp.arc.as_ref(), &lower, arc_ov.is_empty()) {
            (Some(a), Some(lower), false) => arcfront::anchor(&raw, lower, a, p.w_outer),
            _ => Vec::new(),
        };
        let mut below = lower.as_deref().map(|l| walls::Lower::new(l, &p.overhang));
        if p.arachne.is_some() {
            below = below.map(|b| b.for_beads(w_ext, w_in));
        }
        // The first layer prints inner walls first for inner-outer-inner.
        let order = if first && p.wall_order == walls::WallOrder::InnerOuterInner { walls::WallOrder::InnerOuter } else { p.wall_order };
        let given = self.given(k);
        let mut islands = Vec::new();
        for (j, island) in self.island_walls(s, k).into_iter().enumerate() {
            let (placed, seam) = walls::shape_paths(&island.shape, w_ext, w_in, h, p.seam_gap, seam_target, order, p.seam_mode, p.nozzle, z);
            if let Some(s) = seam {
                seam_target = s;
            }
            let (mut walls, along): (Vec<ExtrusionPath>, Vec<walls::Along>) = placed.into_iter().unzip();
            // A texture moves points off their line: each keeps its arc position on it.
            let mut moved: Vec<Option<Vec<f64>>> = vec![None; walls.len()];
            if let (Some(t), Some(f), false) = (p.exp.texture.as_ref(), tex_frame.as_ref(), first) {
                for (w, m) in walls.iter_mut().zip(&mut moved) {
                    *m = texture::apply(w, z, h, t, f);
                }
            }
            for w in walls.iter_mut() {
                w.height = h;
                w.area = flow::cross_section(w.width, h);
                w.speed = p.speed(w.role, w.area, first);
                if let (Some(t), Role::OuterWall, false) = (p.exp.texture.as_ref(), w.role, first) {
                    if let Some(cap) = texture::speed_cap(w, t) {
                        w.speed = w.speed.min(cap);
                    }
                }
            }
            // S3 labels each path's line (the ring before seams and texture, or as given), then the path.
            if let (Some(below), true) = (below.as_mut(), p.overhang.enabled) {
                let _t = prof::scope(prof::Stage::Overhangs);
                let given = given.as_ref().and_then(|g| g.islands[j].labels.as_ref());
                if given.is_some() {
                    self.count(|u| u.labels += 1);
                }
                let lines = match &island.shape {
                    WallShape::Rings(rings) => rings.iter().map(Vec::len).sum(),
                    WallShape::Beads(lines) => lines.len(),
                };
                let mut made: Vec<Option<walls::Labels>> = vec![None; if self.capture.is_some() { lines } else { 0 }];
                walls = walls.into_iter().zip(along).zip(moved).flat_map(|((w, a), m)| {
                    let labelled;
                    let labels = match given {
                        Some(l) => &l[a.line],
                        None => {
                            let (line, closed) = walls::shape_line(&island.shape, a.line);
                            labelled = walls::label_line(line, closed, w.role, w.width, below, &p.overhang);
                            &labelled
                        }
                    };
                    if let Some(at) = made.get_mut(a.line) {
                        assert!(at.as_ref().is_none_or(|l| l == labels), "the paths of one line label it alike");
                        *at = Some(labels.clone());
                    }
                    walls::apply_labels(w, labels, a.start, m.as_deref(), &p.overhang)
                }).collect();
                self.capture(k, |c| c.islands[j].labels = Some(made.into_iter().map(Option::unwrap_or_default).collect()));
            }
            if !island.pieces.is_empty() {
                // The loops only some places have (a region's own wall count): their pieces there,
                // first when the inner walls print first.
                let ahead = order == walls::WallOrder::InnerOuter;
                let from = if ahead { walls.first().and_then(|w| w.pts.first()) } else { walls.last().and_then(|w| w.pts.last()) }.copied().unwrap_or(center);
                let mut cut = Vec::new();
                for (role, outer) in [(Role::OuterWall, true), (Role::InnerWall, false)] {
                    let lines: Vec<BeadLine> = island.pieces.iter().filter(|l| (l.inset == 0) == outer).cloned().collect();
                    cut.extend(walls::line_paths(&lines, role, h, from));
                }
                for c in cut.iter_mut() {
                    c.height = h;
                    c.area = flow::cross_section(c.width, h);
                    c.speed = p.speed(c.role, c.area, first);
                }
                if ahead {
                    cut.append(&mut walls);
                    walls = cut;
                } else {
                    walls.append(&mut cut);
                }
            }
            if !island.thin.is_empty() {
                let thin: Vec<BeadLine> = match &lower {
                    Some(l) => {
                        // As the layer below printed it: the first layer is compensated for its foot.
                        let held = walls::Held::new(&self.eroded(p, k - 1, l), &island.thin, p.gap_fill.min_bead_width, p.overhang.nozzle);
                        island.thin.iter().filter(|t| held.holds(t)).cloned().collect()
                    }
                    None => island.thin.clone(),
                };
                let from = walls.last().and_then(|w| w.pts.last().copied()).unwrap_or(center);
                // Inside the support margin of the layer below, by the rule above: no overhang.
                for mut t in walls::line_paths(&thin, Role::OuterWall, h, from) {
                    t.height = h;
                    t.area = flow::cross_section(t.width, h);
                    t.speed = p.speed(Role::OuterWall, t.area, first);
                    walls.push(t);
                }
            }
            if !arc_ov.is_empty() {
                walls = arcfront::clip_walls(walls, &arc_ov);
            }
            let from = walls.last().and_then(|w| w.pts.last().copied()).unwrap_or(center);
            let mut gaps = walls::gap_paths(&island.gaps, h, from);
            for g in gaps.iter_mut() {
                g.height = h;
                g.area = flow::cross_section(g.width, h);
                g.speed = p.speed(g.role, g.area, first);
            }
            islands.push((island.fill.clone(), walls, gaps));
        }
        st.layers[k].paths.put(Paths { islands, arc_ov, arc_supported });
    }

    /// The class stages of layer `k`: what the layers above and below leave uncovered,
    /// clipped to the fill, propagated into shells, bridges, floating shells.
    fn classes(&self, stage: Stage, s: usize, k: usize) {
        let st = self.stream(s);
        let p = &st.p;
        let n = self.heights.len();
        let l = &st.layers[k];
        let r = ClassRules::of(p, self.heights[k].1);
        match stage {
            Stage::Faces => {
                let above = if k + 1 < n { Some(self.outline[k + 1].get()) } else { None };
                let below = if k > 0 { Some(self.outline[k - 1].get()) } else { None };
                l.faces.put(faces(&r, &l.base.get().raw, above.as_deref().map(Vec::as_slice), below.as_deref().map(Vec::as_slice)));
            }
            Stage::Skin => {
                let fill: Vec<ExPolygon> = self.island_walls(s, k).iter().flat_map(|w| w.fill.iter().cloned()).collect();
                l.skin.put(skin(p, &r, k, fill, &l.faces.get()));
            }
            Stage::Shells => l.shells.put(shells(p, &r, k, n, &|j| st.layers[j].skin.get(), &self.solid_zones(k))),
            Stage::Bridges => {
                let below = if k > 0 { Some(st.layers[k - 1].shells.get()) } else { None };
                l.bridges.put(bridges(p, &r, k, &l.shells.get(), below.as_deref()));
            }
            Stage::Classes => {
                let c = if self.given_classes {
                    self.count(|u| u.classes += 1);
                    self.given(k).and_then(|g| g.classes.clone()).expect("a part that gives its classes gives every layer's")
                } else {
                    let above = if k + 1 < n { Some(st.layers[k + 1].bridges.get()) } else { None };
                    let mut c = classes(p, &r, &l.skin.get(), &l.shells.get(), &l.bridges.get(), above.as_deref());
                    c.zoned = self.zoned(k, &c.sparse);
                    Arc::new(c)
                };
                self.capture(k, |g| g.classes = Some(c.clone()));
                l.classes.put_arc(c);
            }
            _ => unreachable!(),
        }
    }

    /// Every block sees exactly its own sparse and fill window: the part's own sparse, and each
    /// regions' fill that takes the part's cross-layer pattern its share (its fill within it).
    fn aux(&self, s: usize, b: usize) {
        let st = self.stream(s);
        let p = &st.p;
        let (a, end) = aux_range(b, self.heights.len());
        let classes: Vec<Arc<Classes>> = (a..end).map(|j| st.layers[j].classes.get()).collect();
        let sparse: Vec<Vec<ExPolygon>> = classes.iter().map(|c| own_sparse(c)).collect();
        let fill: Vec<Vec<ExPolygon>> = (a..end).map(|j| if self.given_classes {
            self.island_walls(s, j).iter().flat_map(|w| w.fill.iter().cloned()).collect()
        } else {
            st.layers[j].skin.get().fill.clone()
        }).collect();
        let spacing = flow::spacing(p.w_sparse, p.layer_height);
        let heights = &self.heights[a..end];
        let mut data = vec![infill::prepare(p.sparse_pattern, &sparse, &fill, heights, spacing, p.density, self.center)];
        let cells: Vec<Vec<(usize, Vec<ExPolygon>)>> = if self.fills.iter().any(|f| f.pattern.cross_layer()) { (a..end).map(|j| self.fill_cells(j)).collect() } else { Vec::new() };
        for (fi, f) in self.fills.iter().enumerate() {
            data.push(if f.pattern.cross_layer() {
                let sparse: Vec<Vec<ExPolygon>> = classes.iter().map(|c| c.zoned.iter().find(|(i, _)| *i == fi).map_or_else(Vec::new, |(_, a)| a.clone())).collect();
                let fill: Vec<Vec<ExPolygon>> = cells.iter().zip(&fill).map(|(c, f)| c.iter().find(|(i, _)| *i == fi).map_or_else(Vec::new, |(_, at)| intersection_ex(f, at))).collect();
                infill::prepare(f.pattern, &sparse, &fill, heights, spacing, f.density, self.center)
            } else {
                infill::Aux::None
            });
        }
        st.aux[b].put((a, data));
    }

    fn infill(&self, s: usize, k: usize, i: usize) {
        let st = self.stream(s);
        let p = &st.p;
        let n = self.heights.len();
        let l = &st.layers[k];
        let (z, h) = self.heights[k];
        let index = k;
        let first = p.first(index);
        let center = self.center;
        let paths = l.paths.get();
        let (region, _, gaps) = &paths.islands[i];
        let arc_ov = &paths.arc_ov;
        let cls = l.classes.get();
        // The island's given pieces: its share of the classes and its concentric fills, read
        // with the layer's given classes.
        let given = if self.given_classes { self.given(k) } else { None };
        let gi = given.as_ref().map(|g| &g.islands[i]);
        let ic = gi.and_then(|g| g.classes.as_ref());
        let cc = gi.and_then(|g| g.concentric.as_ref()).filter(|_| ic.is_some());
        if ic.is_some() {
            self.count(|u| u.island_classes += 1);
        }
        if cc.is_some() {
            self.count(|u| u.concentric += 1);
        }
        let mut made = self.capture.is_some().then(|| {
            let share = |c: &Vec<ExPolygon>| intersection_ex(region, c);
            let zoned = cls.zoned.iter().map(|(zi, a)| (*zi, share(a))).filter(|(_, a)| !a.is_empty()).collect();
            let classes = Classes { top: share(&cls.top), bottom: share(&cls.bottom), solid: share(&cls.solid), sparse: share(&cls.sparse), bridge: share(&cls.bridge), vertical: share(&cls.vertical), band: share(&cls.band), zoned };
            (classes, None::<Vec<bool>>, GivenConcentric::default())
        });
        // The final bridges of the layer above (its given classes carry them).
        let above: Option<Vec<ExPolygon>> = (k + 1 < n).then(|| if self.given_classes {
            st.layers[k + 1].classes.get().bridge.clone()
        } else {
            st.layers[k + 1].bridges.get().bridge.clone()
        });
        let aux_held = p.sparse_pattern.cross_layer().then(|| st.aux[k / AUX_BLOCK].get());
        let no_aux = infill::Aux::None;
        let aux_start = aux_held.as_ref().map_or(0, |a| a.0);
        // the part's own sparse's (0), or a regions' fill's (its index + 1)
        let aux_of = |slot: usize| aux_held.as_ref().and_then(|a| a.1.get(slot)).unwrap_or(&no_aux);
        let lower = if k > 0 { Some(self.outline[k - 1].get()) } else { None };
        let solid_angle = p.infill_direction + if index % 2 == 1 { std::f64::consts::FRAC_PI_2 } else { 0.0 };
        let mut fills = Vec::new();
        let own_vertical;
        let vertical = match ic {
            Some(c) => &c.vertical,
            None => {
                own_vertical = { let _t = prof::scope(prof::Stage::InfillBooleans); intersection_ex(region, &cls.vertical) };
                &own_vertical
            }
        };
        if let Some(cc) = cc {
            assert_eq!(cc.vertical.len(), vertical.len(), "layer {k} island {i}: one given fill per vertical-shell piece");
        }
        for (j, e) in vertical.iter().enumerate() {
            let mut paths = {
                let _t = prof::scope(prof::Stage::InfillConcentric);
                match cc {
                    Some(cc) => bead_paths(p, Role::FloatingVerticalShell, &cc.vertical[j], h, first),
                    None => {
                        let beads = concentric_beads(p, e, h, first);
                        let paths = bead_paths(p, Role::FloatingVerticalShell, &beads, h, first);
                        if let Some(m) = made.as_mut() {
                            m.2.vertical.push(beads);
                        }
                        paths
                    }
                }
            };
            let left = left_beads(p, Role::FloatingVerticalShell, e, &paths, gaps, h, first);
            paths.extend(left);
            // Sloping shells use vertical_shell_speed. The anchor loop
            // immediately under an internal bridge retains bridge speed.
            let under_bridge = { let _t = prof::scope(prof::Stage::InfillBooleans); above.as_ref().is_some_and(|b| !intersection_ex(std::slice::from_ref(e), b).is_empty()) };
            if !under_bridge {
                for path in &mut paths {
                    let v = p.speed(Role::InternalSolidInfill, path.area, first);
                    path.speed = if p.vertical_shell_speed > 0.0 { v.min(p.vertical_shell_speed) } else { v };
                }
            }
            fills.extend(paths);
        }
        // The band and its loop's beads (a loop may reach past a thin band).
        let mut shell_keep: Vec<ExPolygon> = Vec::new();
        if !cls.band.is_empty() {
            let _t = prof::scope(prof::Stage::InfillConcentric);
            let own_band;
            let mine = match ic {
                Some(c) => &c.band,
                None => {
                    own_band = intersection_ex(region, &cls.band);
                    &own_band
                }
            };
            let shells = floating_shell(p, mine, gaps, h, first);
            shell_keep = cls.band.clone();
            for sp in &shells {
                let mut pts = sp.pts.clone();
                if sp.closed && pts.first() != pts.last() {
                    if let Some(&f) = pts.first() {
                        pts.push(f);
                    }
                }
                shell_keep.extend(makepad_fab_slicer::offset::offset_polyline(&pts, sp.width / 2.0 * SCALE));
            }
            shell_keep = union_ex(&shell_keep);
            fills.extend(shells);
        }
        // The part's own sparse, less its regions' shares: each region fill's prints next at its
        // own density and pattern, meeting the part's as a neighbouring class does.
        let own = infill::SparseFill { density: p.density, pattern: p.sparse_pattern };
        let own_area = (!cls.zoned.is_empty()).then(|| own_sparse(&cls));
        let cores = self.solid_zones(k);
        let mut classes: Vec<(Role, &Vec<ExPolygon>, Option<&Vec<ExPolygon>>, infill::SparseFill, usize)> = vec![
            // The raft contact keeps normal flow, even across a gap.
            (if index == 0 { Role::BottomSurface } else { Role::Bridge }, &cls.bottom, ic.map(|c| &c.bottom), own, 0),
            (Role::Bridge, &cls.bridge, ic.map(|c| &c.bridge), own, 0),
            (Role::InternalSolidInfill, &cls.solid, ic.map(|c| &c.solid), own, 0),
            (Role::SparseInfill, own_area.as_ref().unwrap_or(&cls.sparse), ic.map(|c| &c.sparse).filter(|_| own_area.is_none()), own, 0),
        ];
        classes.extend(cls.zoned.iter().map(|(fi, area)| (Role::SparseInfill, area, None, self.fills[*fi], fi + 1)));
        classes.push((Role::TopSurface, &cls.top, ic.map(|c| &c.top), own, 0));
        // The floating-vertical-shell band is filled by its own loop:
        // classes stop at it as at a neighbouring fill (half a spacing
        // off, no wall overlap), so no line end runs over the loop.
        let clips = prof::scope(prof::Stage::InfillBooleans);
        let boundary = if shell_keep.is_empty() { region.clone() } else { difference_ex(region, &shell_keep) };
        let boundary = if arc_ov.is_empty() { boundary } else { difference_ex(&boundary, arc_ov) };
        drop(clips);
        // Nothing stops the island's classes short of its fill: its share is what it fills.
        let whole = shell_keep.is_empty() && arc_ov.is_empty();
        let mut surfaces = Vec::new();
        for (role, area, own, sparse, aux) in classes {
            if area.is_empty() {
                continue;
            }
            let mut mine = match own.filter(|_| whole) {
                Some(own) => own.clone(),
                None => { let _t = prof::scope(prof::Stage::InfillBooleans); intersection_ex(&boundary, area) }
            };
            if mine.is_empty() {
                continue;
            }
            if role == Role::InternalSolidInfill && p.narrow_solid {
                // Inferred from the output: regions narrower than about
                // twelve solid spacings use loops, including their core.
                let _t = prof::scope(prof::Stage::InfillConcentric);
                let radius = ClassRules::of(p, h).narrow * SCALE;
                let mut flags = gi.and_then(|g| g.narrow.as_ref()).filter(|_| whole && ic.is_some()).map(|v| v.iter());
                if flags.is_some() {
                    self.count(|u| u.narrow += 1);
                }
                let mut beads = cc.filter(|_| whole).map(|c| c.narrow.iter());
                let (mut made_flags, mut made_beads) = (Vec::new(), Vec::new());
                mine.retain(|e| {
                    let is = match flags.as_mut() {
                        Some(v) => *v.next().expect("one given narrow flag per solid piece"),
                        None => makepad_fab_slicer::offset::inset_is_empty(e, radius),
                    };
                    made_flags.push(is);
                    if is {
                        let paths = match beads.as_mut() {
                            Some(b) => bead_paths(p, role, b.next().expect("one given fill per narrow piece"), h, first),
                            None => {
                                let b = concentric_beads(p, e, h, first);
                                let paths = bead_paths(p, role, &b, h, first);
                                made_beads.push(b);
                                paths
                            }
                        };
                        fills.extend(left_beads(p, role, e, &paths, gaps, h, first));
                        fills.extend(paths);
                    }
                    !is
                });
                if let Some(m) = made.as_mut().filter(|_| whole) {
                    m.1 = Some(made_flags);
                    m.2.narrow = made_beads;
                }
            }
            // A first-layer region that is also top is still the bottom.
            let mine = if role == Role::TopSurface && !cls.bottom.is_empty() { let _t = prof::scope(prof::Stage::InfillBooleans); difference_ex(&mine, &cls.bottom) } else { mine };
            if mine.is_empty() {
                continue;
            }
            let internal = role == Role::Bridge && std::ptr::eq(area, &cls.bridge);
            let lower = if internal { None } else { lower.as_deref() };
            let mut paths = crate::infill(p, role, &mine, region, index, h, solid_angle, center, first, z, lower, !cls.band.is_empty(), aux_start, aux_of(aux), sparse, &cores);
            // Bridges over the sparse infill below are internal bridges when the job has their
            // speed: the same lines, their own role, speed and volume factor.
            if internal && p.speeds.internal_bridge.is_some() {
                for path in paths.iter_mut().filter(|path| path.role == Role::Bridge) {
                    path.role = Role::InternalBridge;
                    path.area *= p.internal_bridge_flow;
                    path.speed = p.speed(Role::InternalBridge, path.area, first);
                }
            }
            if role == Role::BottomSurface && p.raft_layers > 0 && p.raft_gap > 0.0 {
                for path in &mut paths {
                    path.speed = p.speed(Role::Bridge, path.area, false);
                }
            }
            if is_surface(role) {
                surfaces.extend_from_slice(&mine);
            }
            fills.extend(paths);
        }
        if !surfaces.is_empty() {
            let _t = prof::scope(prof::Stage::InfillBooleans);
            let gaps = surface_gaps(p, &surfaces, &fills, h, first, center);
            fills.extend(gaps);
        }
        let mut ironing = Vec::new();
        if let (Some(ir), false) = (p.exp.ironing.as_ref(), first) {
            if !ir.only_topmost || index + 1 == n {
                let mut surf = intersection_ex(&boundary, &cls.top);
                if ir.solid {
                    surf = union_ex(&[surf, intersection_ex(&boundary, &cls.solid)].concat());
                }
                if !surf.is_empty() {
                    ironing.extend(ironing::paths(&surf, ir, h, p.w_top, center));
                }
            }
        }
        if let Some((classes, narrow, concentric)) = made {
            self.capture(k, |c| {
                let g = &mut c.islands[i];
                g.classes = Some(classes);
                g.narrow = narrow;
                g.concentric = Some(concentric);
            });
        }
        // Two wall loops around (in every region of the layer), skins above and below and no vase
        // close the part's inside in on every side: its fills are its core, which the preview
        // leaves out while the part shows whole (its inner walls their lining, `layer`).
        let zone_walls = (!self.zones.is_empty()).then(|| self.zone_at[k].get()).iter().flat_map(|c| c.iter()).filter_map(|(_, set)| set.walls).min();
        let walls = zone_walls.map_or(p.wall_loops, |z| z.min(p.wall_loops));
        let closed = p.exp.vase.is_none() && walls >= 2 && p.top_layers > 0 && p.bottom_layers > 0;
        if closed {
            for f in fills.iter_mut().filter(|f| f.role.fills_inside()) {
                f.core = true;
            }
        }
        l.infill.get().expect("the infill group expanded")[i].put(Fills { fills, ironing, closed });
    }

    /// The layer's islands joined: walls, fills, the arc fronts each owns, ironing; then
    /// the brim, adaptive marks and scarf seams (paint by position: per filament).
    fn layer(&self, s: usize, k: usize) {
        let st = self.stream(s);
        let p = &st.p;
        let params = &self.params;
        let (z, h) = self.heights[k];
        let l = &st.layers[k];
        let paths = l.paths.take();
        let fills = l.infill.get().expect("the infill group expanded");
        let mut ol = ObjectLayer { object: self.oi, filament: st.filament, islands: Vec::new(), slices: l.base.get().raw.clone(), support_only: false };
        let mut arc_owned = vec![false; paths.arc_ov.len()];
        for ((region, walls, gaps), f) in paths.islands.into_iter().zip(fills) {
            let f = f.take();
            let mut island = Island { walls, fills: f.fills };
            if f.closed {
                for w in island.walls.iter_mut().filter(|w| w.role == Role::InnerWall) {
                    w.core = true;
                }
            }
            island.fills.extend(gaps);
            if let Some(a) = p.exp.arc.as_ref().filter(|_| !paths.arc_ov.is_empty()) {
                let reach = offset_ex(&region, 2.0 * p.w_outer * p.wall_loops.max(1) as f64 * SCALE, DEFAULT_MITER);
                for (j, piece) in paths.arc_ov.iter().enumerate() {
                    if !arc_owned[j] && !intersection_ex(std::slice::from_ref(piece), &reach).is_empty() {
                        arc_owned[j] = true;
                        island.fills.extend(arcfront::fronts(piece, &paths.arc_supported, a, p.w_outer, h));
                    }
                }
            }
            island.fills.extend(f.ironing);
            ol.islands.push(Arc::new(island));
        }
        // Overhangs next to no island (a floating ledge): fronts of their own.
        if let Some(a) = p.exp.arc.as_ref() {
            for (j, piece) in paths.arc_ov.iter().enumerate() {
                if !arc_owned[j] {
                    let fronts = arcfront::fronts(piece, &paths.arc_supported, a, p.w_outer, h);
                    if !fronts.is_empty() {
                        ol.islands.push(Arc::new(Island { fills: fronts, ..Default::default() }));
                    }
                }
            }
        }
        let brim = if k == 0 && params.first(0) { self.brim() } else { Vec::new() };
        let finish = |stream: usize, layer: &mut ObjectLayer| {
            if stream == 0 && params.first(k) && !brim.is_empty() {
                layer.islands.insert(0, Arc::new(Island { walls: brim.clone(), ..Default::default() }));
            }
            mark_adaptive(params, layer);
            if !self.vase && (!params.first(k) || stream > 0) {
                if let Some(sc) = &params.exp.scarf {
                    ramp::scarf(layer, h, sc);
                }
            }
        };
        let mut out = match &self.paint_at {
            Some(paint) => {
                // Every filament of the job is a stream (the own first): the same inventory on every layer.
                let o = &*self.o;
                let nf = self.job.filaments().max(1);
                let fils: Vec<usize> = std::iter::once(o.filament).chain((0..nf).filter(|&f| f != o.filament)).collect();
                let fil_of = |st: u32| if st == 0 || st as usize > nf { o.filament } else { st as usize - 1 };
                // On the surface a colour changes where the paint does, down to a run one bead wide:
                // a shorter one is no run of its own in the print. Inside the part a run shows nowhere
                // and costs its retract and travel, unless the paint is to be exact all through (a
                // part cut open, a picture whose inner layers show through the face): then the
                // fills inside keep the surface's runs. A run shorter than a step can be missed by
                // the march, but would take its neighbour's colour anyway: steps of half never
                // change the result.
                let min_run = params.w_outer;
                // a blend dithers at the print's resolution: a bead across, a layer up
                let rule = paint_split::SplitRule { min_run, min_run_inside: 3.0 * params.w_outer, exact_fills: params.paint_inside_cell.is_none(), step: 0.5 * min_run, bead: params.w_outer, height: params.layer_height, min_swap: 20.0 * params.w_outer };
                // the first layer prints in the part's own filament whatever its paint (as painted
                // meshes do), unless the part asks for its paint there (a picture face down)
                let mut out = if params.first(k) && !params.paint_first_layer {
                    let empty = |f: usize| ObjectLayer { object: ol.object, filament: f, islands: Vec::new(), slices: Arc::default(), support_only: ol.support_only };
                    let rest: Vec<ObjectLayer> = fils[1..].iter().map(|&f| empty(f)).collect();
                    std::iter::once(ol).chain(rest).collect()
                } else {
                    let _t = prof::scope(prof::Stage::Paint);
                    let inside = self.paint_inside(k);
                    paint_split::split_layer(ol, cut_z(params, z, h), paint, &fil_of, &fils, &rule, inside.as_ref())
                };
                for (stream, layer) in out.iter_mut().enumerate() {
                    finish(stream, layer);
                }
                out
            }
            None => {
                finish(s, &mut ol);
                vec![ol]
            }
        };
        // The layer's islands are made here, held by it alone: what the plan keeps of them is exact.
        for island in out.iter_mut().flat_map(|l| l.islands.iter_mut()) {
            Arc::get_mut(island).expect("a layer's islands are its own until it is put").exact();
        }
        l.layer.put(out);
    }

    /// The layers within the paint's skin of layer `k`'s middle, below and above, when the
    /// layer's fills take the paint coarsely and the part reaches past its skin both ways (else
    /// none: all of the layer is skin).
    fn paint_window(&self, k: usize) -> Option<(usize, usize)> {
        let params = &self.params;
        params.paint_inside_cell?;
        if self.paint_at.is_none() || params.first(k) {
            return None;
        }
        let mid = |j: usize| self.heights[j].0 - 0.5 * self.heights[j].1;
        let (n, d) = (self.heights.len(), params.paint_skin);
        let (bottom, top) = (self.heights[0].0 - self.heights[0].1, self.heights[n - 1].0);
        let z = mid(k);
        if z - d <= bottom || z + d >= top {
            return None;
        }
        let lo = (0..k).rev().take_while(|&j| mid(j) >= z - d).last().unwrap_or(k);
        let hi = (k + 1..n).take_while(|&j| mid(j) <= z + d).last().unwrap_or(k);
        Some((lo, hi))
    }

    /// Layer `k`'s inside, deeper under the part's surface than the paint's skin: its outline
    /// where the layers within the skin above and below hold it too, shrunk by the skin.
    fn paint_inside(&self, k: usize) -> Option<paint_split::Inside> {
        let (lo, hi) = self.paint_window(k)?;
        let cell = self.params.paint_inside_cell?;
        let mut core = self.outline[lo].get().to_vec();
        for j in lo + 1..=hi {
            core = intersection_ex(&core, &self.outline[j].get());
            if core.is_empty() {
                return None;
            }
        }
        let core = makepad_fab_slicer::offset::offset_mm(&core, -self.params.paint_skin);
        (!core.is_empty()).then_some(paint_split::Inside { core, cell })
    }

    fn brim(&self) -> Vec<ExtrusionPath> {
        let params = &self.params;
        let first_outline = self.outline[0].get();
        let (min, max) = self.o.source.bounds();
        let brim_width = if params.brim_kind == BrimKind::AutoBrim { brim::auto_brim_width(&first_outline, max[2] - min[2]) } else { params.brim_width };
        let mut paths = brim::rings(&first_outline, params.brim_kind, brim_width, params.brim_gap, params.w_first, self.heights[0].1, &[]);
        for path in &mut paths {
            path.speed = params.speed(Role::Brim, path.area, true);
        }
        paths
    }

    /// Layers `lo..hi` of every stream, taken: by stream (paint by position: by layer), then by height.
    fn collect(&self, lo: usize, hi: usize) -> Vec<(f64, f64, ObjectLayer)> {
        let mut out = Vec::new();
        let at = |s: usize, k: usize| self.take_layer(s, k).into_iter().map(move |l| (self.heights[k].0, self.heights[k].1, l));
        if self.paint_at.is_some() {
            out.extend((lo..hi).flat_map(|k| at(0, k)));
        } else {
            for s in 0..self.streams().expect("the streams are known") {
                out.extend((lo..hi).flat_map(|k| at(s, k)));
            }
        }
        if self.painted {
            out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        }
        out
    }
}

/// The aux block `b`'s layers of `n`: the block with context on either side.
/// A region of a part with print settings of its own (`m.settings`): where, and what it sets.
struct Zone {
    region: makepad_fab_slicer::source::SupportRegion,
    set: ZoneSet,
}

/// What a region's settings set (`fab_plate::REGION_SETTINGS`), each key the part's own where
/// `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ZoneSet {
    density: Option<f64>,
    pattern: Option<pattern::Sparse>,
    walls: Option<usize>,
}

impl ZoneSet {
    /// A region's values, as the model checked them ("100%", "gyroid", "4").
    fn of(values: &[(String, String)]) -> ZoneSet {
        let mut set = ZoneSet::default();
        for (k, v) in values {
            let number = || v.trim().trim_end_matches('%').parse::<f64>().unwrap_or_else(|_| panic!("region setting {k} \"{v}\": the model checks it"));
            match k.as_str() {
                "sparse_infill_density" => set.density = Some(number() / 100.0),
                "sparse_infill_pattern" => set.pattern = Some(pattern::Sparse::parse(v).unwrap_or_else(|| panic!("region setting {k} \"{v}\": the model checks it"))),
                "wall_loops" => set.walls = Some(number().max(0.0) as usize),
                _ => panic!("region setting {k}: the model takes only a region's"),
            }
        }
        set
    }

    /// This, with each key `later` sets taken from it.
    fn then(self, later: ZoneSet) -> ZoneSet {
        ZoneSet { density: later.density.or(self.density), pattern: later.pattern.or(self.pattern), walls: later.walls.or(self.walls) }
    }

    /// The sparse fill it prints under the part's parameters `p`.
    fn fill(&self, p: &Params) -> infill::SparseFill {
        infill::SparseFill { density: self.density.unwrap_or(p.density), pattern: self.pattern.unwrap_or(p.sparse_pattern) }
    }
}

/// The sparse fills a part's regions may print but its own and solid: each density one of them
/// sets or the part's, with each pattern one sets or the part's (a region may set the one and
/// a later one over it the other).
fn region_fills(p: &Params, zones: &[Zone]) -> Vec<infill::SparseFill> {
    let (mut densities, mut patterns) = (vec![p.density], vec![p.sparse_pattern]);
    for z in zones {
        if let Some(d) = z.set.density.filter(|d| !densities.contains(d)) {
            densities.push(d);
        }
        if let Some(q) = z.set.pattern.filter(|q| !patterns.contains(q)) {
            patterns.push(q);
        }
    }
    let own = infill::SparseFill { density: p.density, pattern: p.sparse_pattern };
    densities.iter().flat_map(|&density| patterns.iter().map(move |&pattern| infill::SparseFill { density, pattern })).filter(|f| *f != own && f.density < 1.0).collect()
}

/// A layer's sparse infill less its regions' shares: what prints at the part's own density and
/// pattern.
fn own_sparse(c: &Classes) -> Vec<ExPolygon> {
    if c.zoned.is_empty() {
        return c.sparse.clone();
    }
    let zoned: Vec<ExPolygon> = c.zoned.iter().flat_map(|(_, a)| a.iter().cloned()).collect();
    difference_ex(&c.sparse, &zoned)
}

fn aux_range(b: usize, n: usize) -> (usize, usize) {
    let block = b * AUX_BLOCK;
    (block.saturating_sub(AUX_REACH), (block + AUX_BLOCK + AUX_REACH).min(n))
}

/// The layer's outline: XY compensation, then the non-planar cap clip.
pub(crate) fn outline_layer(xy: XyComp, np: Option<&makepad_fab_slicer::nonplanar::NpPlan>, lift: f64, z: f64, contours: Vec<ExPolygon>) -> Vec<ExPolygon> {
    let slices = if xy == XyComp::default() { contours } else { xy.apply(&contours) };
    match np {
        Some(np) => np_clip(np, z - lift, slices),
        None => slices,
    }
}

/// The class chain's distances (mm) and areas (mm²) on a layer of height `h`: one source for the
/// planner's own classes and for a field source's (its plan spec).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClassRules {
    /// Faces and skins drop pieces of at most this area.
    pub speck: f64,
    /// A face clear of the fill (wholly in the wall band) grows by this into the shells.
    pub face_grow: f64,
    /// The bottom (external bridge) grows by this into its anchor.
    pub bottom_grow: f64,
    /// Shells open by this.
    pub shell_open: f64,
    pub min_sparse_area: f64,
    /// A solid piece with nothing this far from its edge, over sparse, is a vertical shell.
    pub vertical: f64,
    /// Internal bridges open by this, keep pieces over `bridge_area` and grow by `bridge_grow`.
    pub bridge_open: f64,
    pub bridge_area: f64,
    pub bridge_grow: f64,
    /// The bridge above grows by this to find the sparse under it.
    pub under_grow: f64,
    /// The anchor band is the sparse under a bridge within this of the fill's edge, in pieces over `band_area`.
    pub band_core: f64,
    pub band_area: f64,
    /// A solid piece with nothing this far from its edge, beside a band, is a vertical shell.
    pub adjacent: f64,
    /// A solid piece with nothing this far from its edge is narrow (concentric fill).
    pub narrow: f64,
}

impl ClassRules {
    pub fn of(p: &Params, h: f64) -> Self {
        let s_layer = flow::spacing(p.w_solid, p.layer_height);
        let s = flow::spacing(p.w_solid, h);
        ClassRules {
            speck: 0.05,
            face_grow: 1.5 * s_layer + p.infill_overlap * flow::spacing((p.w_outer + p.w_inner) / 2.0, p.layer_height),
            bottom_grow: 2.0 * p.w_solid,
            shell_open: 0.55 * s,
            min_sparse_area: p.min_sparse_area,
            vertical: 2.5 * s_layer,
            bridge_open: s,
            bridge_area: 1.0,
            bridge_grow: 2.0 * s,
            under_grow: 0.01,
            band_core: s_layer,
            band_area: 0.5,
            adjacent: 1.5 * s_layer,
            narrow: 6.0 * s,
        }
    }
}

fn speck(mut v: Vec<ExPolygon>, r: &ClassRules) -> Vec<ExPolygon> {
    v.retain(|e| e.area_mm2() > r.speck);
    v
}

/// What the layer above (`above`, None on the top layer) and below leave of the regions `raw`.
fn faces(r: &ClassRules, raw: &[ExPolygon], above: Option<&[ExPolygon]>, below: Option<&[ExPolygon]>) -> Faces {
    let top = match above {
        Some(a) => difference_ex(raw, a),
        None => raw.to_vec(),
    };
    let bottom = match below {
        Some(b) => difference_ex(raw, b),
        None => raw.to_vec(),
    };
    Faces { top: speck(top, r), bottom: speck(bottom, r) }
}

/// More than this many units apart, the edges of two regions take no part in each other's
/// boolean: nothing crosses, touches or snaps across (a hot pixel reaches under a unit from its
/// point; clip's `near` rule, per edge).
const CLEAR: f64 = 3.0;

/// Whether `face` misses `fill` (its edges indexed in `grid`, payload the region): the answer of
/// `intersection_ex(&[face], fill).is_empty()`. When every edge of the face keeps more than CLEAR
/// from every fill edge, each ring of either lies wholly inside or outside the other's regions,
/// so they meet exactly when a point of one lies in the other. Closer, the boolean answers.
fn misses(face: &ExPolygon, fill: &[ExPolygon], grid: &makepad_fab_slicer::grid::SegGrid<u32>) -> bool {
    let near = |a: Pt, b: Pt, c: Pt, d: Pt| {
        let side = |o: i128| o.signum();
        let crossing = side(orient(a, b, c)) * side(orient(a, b, d)) < 0 && side(orient(c, d, a)) * side(orient(c, d, b)) < 0;
        crossing || dist_point_segment(a, c, d) <= CLEAR || dist_point_segment(b, c, d) <= CLEAR || dist_point_segment(c, a, b) <= CLEAR || dist_point_segment(d, a, b) <= CLEAR
    };
    let reach = CLEAR as i64 + 1;
    let clear = face.rings().all(|r| {
        let n = r.pts.len();
        (0..n).all(|i| {
            let (a, b) = (r.pts[i], r.pts[(i + 1) % n]);
            let mut bb = BBox::of(&[a, b]);
            bb.min = Pt::new(bb.min.x - reach, bb.min.y - reach);
            bb.max = Pt::new(bb.max.x + reach, bb.max.y + reach);
            let mut clear = true;
            grid.visit_box(bb, |c, d, _| clear = clear && !near(a, b, c, d));
            clear
        })
    });
    let Some(&first) = face.contour.pts.first() else { return intersection_ex(std::slice::from_ref(face), fill).is_empty() };
    if !clear {
        return intersection_ex(std::slice::from_ref(face), fill).is_empty();
    }
    let fb = face.bbox();
    !grid.contains_any(first) && !fill.iter().filter(|e| e.bbox().overlaps(&fb)).flat_map(|e| e.rings()).any(|r| r.pts.first().is_some_and(|&q| face.contains(q)))
}

/// Top and bottom are found on the outlines and then clipped to the fill
/// regions, so shells reach the full infill area of every layer.
fn skin(p: &Params, r: &ClassRules, k: usize, fill: Vec<ExPolygon>, f: &Faces) -> Skin {
    let top = speck(intersection_ex(&fill, &f.top), r);
    let bottom = speck(intersection_ex(&fill, &f.bottom), r);
    let shell_margin = r.face_grow;
    // R1 §4.2 regularises projected vertical shells. Output on sloping
    // faces implies roughly 1.5 solid spacings plus wall overlap where the
    // exposed skin is wholly inside the wall band. Classify each component:
    // a horizontal top on another island must not suppress a sloped shell.
    // the fill's edges, indexed once for every face's test
    let grid = (p.vertical_shells && (p.top_layers > 1 && !f.top.is_empty() || p.bottom_layers > 1 && !f.bottom.is_empty()))
        .then(|| makepad_fab_slicer::grid::SegGrid::of_regions(&fill, |k| k as u32));
    let projected = |faces: &[ExPolygon]| -> Vec<ExPolygon> {
        faces.iter().flat_map(|face| {
            let src = std::slice::from_ref(face);
            if grid.as_ref().is_some_and(|g| misses(face, &fill, g)) {
                grown_face(src, shell_margin)
            } else {
                vec![face.clone()]
            }
        }).collect()
    };
    let top_projected = if p.top_layers > 1 { projected(&f.top) } else { Vec::new() };
    let bottom_projected = if p.bottom_layers > 1 { projected(&f.bottom) } else { Vec::new() };
    // External bridges grow into the supported area for anchoring (R1 §4.2).
    // The cap output implies about two solid widths, with the same footprint
    // on the layer immediately above the bridge. Later shell layers keep
    // their original projection.
    let grown = if k >= 1 && !bottom.is_empty() { intersection_ex(&fill, &offset_ex(&bottom, r.bottom_grow * SCALE, DEFAULT_MITER)) } else { bottom.clone() };
    Skin { top_face: !f.top.is_empty(), bottom_face: !f.bottom.is_empty(), fill, top, bottom, top_projected, bottom_projected, grown }
}

/// Shell propagation: the faces projected from the layers above and below
/// (`skin(j)`), the bridge below reaching up, and small sparse areas made solid, as are the
/// regions that print solid (`solid_zones`): what rests on the sparse below them is an
/// internal bridge, as under a top's shells.
fn shells(p: &Params, r: &ClassRules, k: usize, n: usize, skin: &dyn Fn(usize) -> Arc<Skin>, solid_zones: &[ExPolygon]) -> Shells {
    let own = skin(k);
    let fill = &own.fill;
    let mut acc: Vec<ExPolygon> = Vec::new();
    for m in 1..p.top_layers {
        if k + m < n {
            let s = skin(k + m);
            if s.top_face {
                acc.extend(s.top_projected.iter().cloned());
            }
        }
    }
    for m in 1..p.bottom_layers {
        if k >= m {
            let s = skin(k - m);
            if s.bottom_face {
                acc.extend(s.bottom_projected.iter().cloned());
            }
        }
    }
    let mut solid = if acc.is_empty() {
        Vec::new()
    } else {
        let acc = union_ex(&acc);
        let s = intersection_ex(fill, &acc);
        let s = difference_ex(&s, &own.top);
        let s = difference_ex(&s, &own.bottom);
        // Regularised (R1 §4.2): a sliver no wider than one solid line is no shell.
        let half = r.shell_open * SCALE;
        makepad_fab_slicer::offset::opening(&s, half, half, DEFAULT_MITER)
    };
    // The bridge below (layer k - 1) reaches up into this layer...
    if k >= 2 && p.bottom_layers > 1 {
        let below = skin(k - 1);
        if !below.bottom.is_empty() {
            let up = intersection_ex(fill, &below.grown);
            solid = difference_ex(&union_ex(&[solid, up].concat()), &union_ex(&[own.top.clone(), own.bottom.clone()].concat()));
        }
    }
    // ...and this layer's own grown bridge takes its share.
    if k >= 1 && !own.bottom.is_empty() {
        solid = difference_ex(&solid, &own.grown);
    }
    // Small sparse areas become solid.
    let taken = union_ex(&[own.top.clone(), own.grown.clone(), solid.clone()].concat());
    let mut sparse = difference_ex(fill, &taken);
    if !solid_zones.is_empty() && !sparse.is_empty() {
        let cored = intersection_ex(&sparse, solid_zones);
        if !cored.is_empty() {
            sparse = difference_ex(&sparse, solid_zones);
            solid = union_ex(&[solid, cored].concat());
        }
    }
    let mut small = Vec::new();
    sparse.retain(|e| {
        if e.area_mm2() <= r.min_sparse_area {
            small.push(e.clone());
            false
        } else {
            true
        }
    });
    let solid = if small.is_empty() { solid } else { union_ex(&[solid, small].concat()) };
    Shells { solid, sparse }
}

/// Narrow vertical shells and internal bridges over the sparse infill of the layer below.
fn bridges(p: &Params, r: &ClassRules, k: usize, own: &Shells, below: Option<&Shells>) -> Bridges {
    let mut solid = own.solid.clone();
    let mut vertical = Vec::new();
    // Narrow vertical shells can project over sparse infill on a sloping
    // face. Keep the whole strip together instead of making an unprintable
    // sliver of bridge. The reference uses concentric paths on these strips.
    // Strips up to five solid lines wide count: on the bracket's sloped faces
    // that leaves the internal solid infill at the reference's (87.8 vs 88.0 m;
    // three lines left 91.1 m, seven 86.6 m).
    if let (true, Some(below)) = (p.floating_shells, below) {
        let radius = r.vertical * SCALE;
        vertical = solid.iter().filter(|e| {
            let src = std::slice::from_ref(*e);
            makepad_fab_slicer::offset::inset_is_empty(e, radius) && !intersection_ex(src, &below.sparse).is_empty()
        }).cloned().collect();
        solid = difference_ex(&solid, &vertical);
    }
    // Internal bridges: solid resting on sparse infill.
    let Some(below) = below.filter(|b| k > 0 && !solid.is_empty() && !b.sparse.is_empty()) else {
        return Bridges { vertical, solid, bridge: Vec::new() };
    };
    let b = intersection_ex(&solid, &below.sparse);
    // A sliver narrower than two solid lines (a sloped shell creeping out over the
    // infill one layer at a time) cannot span anything: it stays solid infill.
    let s2 = r.bridge_open * SCALE;
    let b = makepad_fab_slicer::offset::opening(&b, s2, s2, DEFAULT_MITER);
    let b: Vec<ExPolygon> = b.into_iter().filter(|e| e.area_mm2() > r.bridge_area).collect();
    // Let the bridge reach neighbouring solid anchors. This also keeps
    // narrow residual strips from being filled with a different pattern.
    let b = intersection_ex(&solid, &offset_ex(&b, r.bridge_grow * SCALE, DEFAULT_MITER));
    if b.is_empty() {
        Bridges { vertical, solid, bridge: Vec::new() }
    } else {
        Bridges { vertical, solid: difference_ex(&solid, &b), bridge: b }
    }
}

/// The final classes. Floating vertical shells: under an internal bridge of the
/// layer above (`above`), a one-line solid band along the walls carries the
/// bridge's ends; the sparse infill keeps clear of it.
fn classes(p: &Params, r: &ClassRules, sk: &Skin, sh: &Shells, br: &Bridges, above: Option<&Bridges>) -> Classes {
    let mut c = Classes { top: sk.top.clone(), bottom: sk.grown.clone(), solid: br.solid.clone(), sparse: sh.sparse.clone(), bridge: br.bridge.clone(), vertical: br.vertical.clone(), band: Vec::new(), zoned: Vec::new() };
    let Some(above) = above.filter(|a| p.floating_shells && !a.bridge.is_empty() && !c.sparse.is_empty()) else { return c };
    let under = intersection_ex(&c.sparse, &offset_ex(&above.bridge, r.under_grow * SCALE, DEFAULT_MITER));
    if under.is_empty() {
        return c;
    }
    let core = offset_ex(&sk.fill, -r.band_core * SCALE, DEFAULT_MITER);
    let band: Vec<ExPolygon> = difference_ex(&under, &core).into_iter().filter(|e| e.area_mm2() > r.band_area).collect();
    if band.is_empty() {
        return c;
    }
    c.sparse = difference_ex(&c.sparse, &band);
    // Existing thin solid strips beside this anchor band are
    // part of the same floating shell on the bridge's underside.
    let mut adjacent = Vec::new();
    c.solid.retain(|e| {
        let src = std::slice::from_ref(e);
        if makepad_fab_slicer::offset::inset_is_empty(e, r.adjacent * SCALE) && !intersection_ex(src, &above.bridge).is_empty() {
            adjacent.push(e.clone());
            false
        } else {
            true
        }
    });
    c.vertical.extend(adjacent);
    c.band = band;
    c
}

/// The planner's classes of every layer, from each layer's regions (`raw`), the
/// object's outline and the fill regions inside the walls (`fill`). The reference a field
/// source's classes are checked against.
pub fn planner_classes(p: &Params, heights: &[(f64, f64)], raw: &[Vec<ExPolygon>], outline: &[Vec<ExPolygon>], fill: &[Vec<ExPolygon>]) -> Vec<Classes> {
    let n = raw.len();
    let skins: Vec<Arc<Skin>> = (0..n).map(|k| {
        let r = ClassRules::of(p, heights[k].1);
        let f = faces(&r, &raw[k], outline.get(k + 1).map(Vec::as_slice), k.checked_sub(1).map(|j| &outline[j][..]));
        Arc::new(skin(p, &r, k, fill[k].clone(), &f))
    }).collect();
    let rules = |k: usize| ClassRules::of(p, heights[k].1);
    let shells: Vec<Shells> = (0..n).map(|k| shells(p, &rules(k), k, n, &|j| skins[j].clone(), &[])).collect();
    let bridges: Vec<Bridges> = (0..n).map(|k| bridges(p, &rules(k), k, &shells[k], k.checked_sub(1).map(|j| &shells[j]))).collect();
    (0..n).map(|k| classes(p, &rules(k), &skins[k], &shells[k], &bridges[k], bridges.get(k + 1))).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Waiting for what it reads.
    Waiting,
    Ready,
    Running,
    /// A group waiting for its tasks.
    Expanded,
    Done,
}

struct Node<P> {
    state: State,
    /// Unfinished tasks it waits for.
    remaining: usize,
    deps: Vec<Task>,
    /// A group's tasks.
    children: Vec<Task>,
    /// Who waits for it.
    dependents: Vec<Task>,
    /// Readers not yet finished: at zero its result is dropped.
    readers: usize,
    released: bool,
    /// Its own priority, or that of the most wanted task that waits for it.
    rank: P,
    /// Its own priority: among equal ranks, the higher goes first.
    own: P,
    /// When it became ready, and the span that made it (a group's: its last child's), while
    /// spans are recorded.
    ready_ns: u64,
    span: u64,
}

/// A passive scheduler over the cone of a part's targets. It holds the dependency counts
/// and the ready tasks by rank, highest first; the caller runs the tasks. A task ranks by
/// its own priority (`prio`) or, when higher, by that of the most wanted task waiting for it:
/// what the focus layer reads goes with the focus layer, wherever it lies. Among equal ranks
/// its own priority decides. A group (every island of a layer's walls or infill, every stream of a layer) expands
/// into its tasks once what it reads is made. A result put into the plan before it is made
/// (a trace, finished layers) is done without its cone; a trace not put is cut from the part's
/// source by a [`Stage::Trace`] task ([`PartPlan::cut`]), ranked like the tasks that read it.
/// A result is dropped when its last reader is done, unless it is a target's.
pub struct Sched<P: Ord + Clone> {
    nodes: HashMap<Task, Node<P>>,
    ready: BinaryHeap<(P, P, Reverse<Task>)>,
    prio: Box<dyn Fn(Task) -> P + Send>,
    targets: HashSet<Task>,
    open: usize,
}

impl<P: Ord + Clone> Sched<P> {
    /// Over the cone of `targets` in `plan`, its traces not put cut by tasks.
    pub fn cutting(plan: &PartPlan, targets: &[Task], prio: impl Fn(Task) -> P + Send + 'static) -> Self {
        let mut s = Sched { nodes: HashMap::new(), ready: BinaryHeap::new(), prio: Box::new(prio), targets: targets.iter().copied().collect(), open: 0 };
        for &t in targets {
            s.add(plan, t, None);
        }
        s
    }

    /// Rank the tasks by `prio` from now on (the focus moved).
    pub fn set_prio(&mut self, prio: impl Fn(Task) -> P + Send + 'static) {
        self.prio = Box::new(prio);
        let prio = &self.prio;
        let mut open: Vec<(P, Task)> = self.nodes.iter_mut().filter(|(_, n)| n.state != State::Done).map(|(t, n)| {
            n.own = prio(*t);
            n.rank = n.own.clone();
            (n.rank.clone(), *t)
        }).collect();
        // The most wanted first: each task's cone takes its rank once.
        open.sort_by(|a, b| b.cmp(a));
        for (rank, t) in open {
            let n = &self.nodes[&t];
            if n.rank != rank { continue; }
            let below: Vec<Task> = n.deps.iter().chain(&n.children).copied().collect();
            for d in below {
                self.lift(d, rank.clone());
            }
        }
        self.ready = self.nodes.iter().filter(|(_, n)| n.state == State::Ready).map(|(t, n)| (n.rank.clone(), n.own.clone(), Reverse(*t))).collect();
    }

    /// Task `t` and what it waits for rank at least `rank`.
    fn lift(&mut self, t: Task, rank: P) {
        let mut todo = vec![t];
        while let Some(t) = todo.pop() {
            let n = self.nodes.get_mut(&t).unwrap();
            if n.state == State::Done || n.rank >= rank { continue; }
            n.rank = rank.clone();
            if n.state == State::Ready { self.ready.push((rank.clone(), n.own.clone(), Reverse(t))); }
            todo.extend(n.deps.iter().chain(&n.children).copied());
        }
    }

    /// The rank of the best ready task.
    pub fn peek(&mut self) -> Option<&P> {
        // Entries a lift outranked stay behind in the heap: dropped here.
        loop {
            let stale = match self.ready.peek() {
                None => return None,
                Some((r, _, Reverse(t))) => { let n = &self.nodes[t]; n.state != State::Ready || n.rank != *r }
            };
            if !stale { return self.ready.peek().map(|(p, _, _)| p); }
            self.ready.pop();
        }
    }

    /// The targets done so far.
    pub fn done_targets(&self) -> Vec<Task> {
        let mut v: Vec<Task> = self.targets.iter().copied().filter(|t| self.nodes.get(t).is_some_and(|n| n.state == State::Done)).collect();
        v.sort();
        v
    }

    /// Running task `t`'s span inputs: when it became ready, and the spans of what it read.
    fn span_inputs(&self, t: Task) -> (u64, Vec<u64>) {
        let n = &self.nodes[&t];
        (n.ready_ns, n.deps.iter().map(|d| self.nodes[d].span).filter(|&s| s != 0).collect())
    }

    /// Running task `t` ran as span `span`.
    fn ran(&mut self, t: Task, span: u64) {
        self.nodes.get_mut(&t).unwrap().span = span;
    }

    /// The best ready task, now running.
    pub fn next(&mut self) -> Option<Task> {
        loop {
            let (r, _, Reverse(t)) = self.ready.pop()?;
            let n = self.nodes.get_mut(&t).unwrap();
            if n.state != State::Ready || n.rank != r { continue; }
            n.state = State::Running;
            return Some(t);
        }
    }

    /// Task `t` ran: release what waits for it.
    pub fn done(&mut self, plan: &PartPlan, t: Task) {
        let state = self.nodes[&t].state;
        assert_eq!(state, State::Running, "{t:?} done while {state:?}");
        self.complete(plan, t);
    }

    /// Every task of the cone is done.
    pub fn finished(&self) -> bool {
        self.open == 0
    }

    /// Adds `t` and its cone, ranked at least `inherit` (what waits for it).
    fn add(&mut self, plan: &PartPlan, t: Task, inherit: Option<P>) {
        if self.nodes.contains_key(&t) {
            if let Some(r) = inherit { self.lift(t, r); }
            return;
        }
        // A result already put reads nothing.
        let deps = if plan.has(t) { Vec::new() } else { plan.deps(t) };
        let own = (self.prio)(t);
        let rank = match inherit { Some(r) if r > own => r, _ => own.clone() };
        self.nodes.insert(t, Node { state: State::Waiting, remaining: 0, deps: deps.clone(), children: Vec::new(), dependents: Vec::new(), readers: 0, released: false, rank: rank.clone(), own, ready_ns: 0, span: 0 });
        self.open += 1;
        let mut remaining = 0;
        for d in deps {
            self.add(plan, d, Some(rank.clone()));
            let dn = self.nodes.get_mut(&d).unwrap();
            assert!(!dn.released, "{t:?} reads {d:?} after it was dropped");
            dn.readers += 1;
            if dn.state != State::Done {
                dn.dependents.push(t);
                remaining += 1;
            }
        }
        self.nodes.get_mut(&t).unwrap().remaining = remaining;
        if remaining == 0 {
            self.start(plan, t);
        }
    }

    /// Everything `t` reads is made.
    fn start(&mut self, plan: &PartPlan, t: Task) {
        if plan.has(t) {
            self.complete(plan, t);
        } else if t.is_group() {
            let n = self.nodes.get_mut(&t).unwrap();
            n.state = State::Expanded;
            let rank = n.rank.clone();
            let children = plan.children(t);
            n.children.clone_from(&children);
            let mut remaining = 0;
            for c in children {
                self.add(plan, c, Some(rank.clone()));
                let cn = self.nodes.get_mut(&c).unwrap();
                if cn.state != State::Done {
                    cn.dependents.push(t);
                    remaining += 1;
                }
            }
            self.nodes.get_mut(&t).unwrap().remaining = remaining;
            if remaining == 0 {
                self.complete(plan, t);
            }
        } else {
            let n = self.nodes.get_mut(&t).unwrap();
            n.state = State::Ready;
            n.ready_ns = ready_ns();
            self.ready.push((n.rank.clone(), n.own.clone(), Reverse(t)));
        }
    }

    fn complete(&mut self, plan: &PartPlan, t: Task) {
        let node = self.nodes.get_mut(&t).unwrap();
        node.state = State::Done;
        self.open -= 1;
        let dependents = std::mem::take(&mut node.dependents);
        let deps = std::mem::take(&mut node.deps);
        node.children = Vec::new();
        let span = node.span;
        // Waiters first: a group expanding here adds readers before anything is dropped.
        for w in dependents {
            let wn = self.nodes.get_mut(&w).unwrap();
            wn.remaining -= 1;
            if wn.remaining == 0 {
                if wn.state == State::Expanded {
                    wn.span = span;
                    self.complete(plan, w);
                } else {
                    self.start(plan, w);
                }
            }
        }
        for d in deps {
            let dn = self.nodes.get_mut(&d).unwrap();
            dn.readers -= 1;
            if dn.readers == 0 && !self.targets.contains(&d) {
                dn.released = true;
                plan.release(d);
            }
        }
    }
}

/// Now on the spans' clock, while spans are recorded.
fn ready_ns() -> u64 {
    if makepad_fab_slicer::par::recording() { makepad_fab_slicer::par::now_ns() } else { 0 }
}

/// A plate run's priority of a task (highest first): lower urgency; then its part's focus
/// layer, the layers below it down to the first, then those above it up to the last; then
/// earlier stages, streams and islands; then lower parts.
pub type PlatePrio = Reverse<(u8, bool, usize, u32, Stage, u16, u32, usize)>;

fn plate_prio(part: usize, urgency: u8, focus: Option<usize>) -> impl Fn(Task) -> PlatePrio + Send + 'static {
    move |t: Task| {
        let (above, dist) = focus.map_or((false, 0), |f| (t.k() > f, t.k().abs_diff(f)));
        Reverse((urgency, above, dist, t.layer, t.stage, t.stream, t.island, part))
    }
}

/// One part of a [`Plate`]: its plan, what is wanted of it and how it ranks.
pub struct PlatePart<'p, 'a> {
    pub plan: &'p PartPlan<'a>,
    /// What the run makes of it: finished layers, a vase's ramp, or the finished part. The
    /// traces they read that its plan was not given ([`PartPlan::put_trace`]) are cut by tasks.
    pub targets: Vec<Task>,
    /// Its tasks rank before those of parts with a higher value.
    pub urgency: u8,
    /// Its tasks nearest this layer go first.
    pub focus: Option<usize>,
}

/// Cancellation and results of a [`Plate::run`].
#[derive(Clone, Copy, Default)]
pub struct PlateCtl<'c> {
    /// True once the work is no longer wanted: no task starts after.
    pub cancel: Option<&'c (dyn Fn() -> bool + Sync)>,
    /// Told about a part's results as they are made: (part, result).
    pub emit: Option<&'c (dyn Fn(usize, Emit) + Sync)>,
    /// A layer the run cut from its part's source: (part, layer, its contours before pinch splitting).
    pub traced: Option<&'c (dyn Fn(usize, usize, &[ExPolygon]) + Sync)>,
    /// Asked by a helper before it takes a task: moves focuses ([`Plate::set_focus`]).
    pub poll: Option<&'c (dyn Fn() + Sync)>,
    /// Told how many units of work just finished: one per layer cut.
    pub tick: Option<&'c (dyn Fn(usize) + Sync)>,
    /// The revision its spans name (the bench's, [`makepad_fab_slicer::par::record_spans`]).
    pub revision: u64,
}

impl PlateCtl<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|c| c())
    }
}

/// How a [`Plate::run`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlateRun {
    /// Every target of every part is made.
    Finished,
    /// The cancel said stop.
    Cancelled,
}

struct PlateState {
    scheds: Vec<Sched<PlatePrio>>,
    /// Helpers recruited and not yet gone, and those of them not started yet.
    workers: usize,
    starting: usize,
    /// Parts whose region count was emitted.
    announced: Vec<bool>,
}

/// A plate's state and its workers' wake.
struct PlateSync {
    state: Mutex<PlateState>,
    wake: Condvar,
}

/// Several parts' dataflows on one worker set: whichever part's most urgent ready task
/// ranks highest runs next, so no part waits for another and no stage for a barrier. A
/// part's traces not put before the plate is made are cut here as tasks. The results never
/// depend on the thread count or the order tasks ran in.
pub struct Plate<'p, 'a> {
    parts: Vec<PlatePart<'p, 'a>>,
    sync: Arc<PlateSync>,
    /// Per part: nanoseconds its tasks ran.
    busy: Vec<AtomicU64>,
}

impl<'p, 'a> Plate<'p, 'a> {
    pub fn new(parts: Vec<PlatePart<'p, 'a>>) -> Self {
        let scheds = parts.iter().enumerate().map(|(i, p)| {
            Sched::cutting(p.plan, &p.targets, plate_prio(i, p.urgency, p.focus))
        }).collect();
        let announced = vec![false; parts.len()];
        let busy = parts.iter().map(|_| AtomicU64::new(0)).collect();
        Plate { parts, sync: Arc::new(PlateSync { state: Mutex::new(PlateState { scheds, workers: 0, starting: 0, announced }), wake: Condvar::new() }), busy }
    }

    /// Seconds part `part`'s tasks ran so far (summed over the threads).
    pub fn busy_s(&self, part: usize) -> f64 {
        self.busy[part].load(Relaxed) as f64 * 1e-9
    }

    /// Part `part`'s tasks nearest `focus` go first from now on.
    pub fn set_focus(&self, part: usize, focus: Option<usize>) {
        let p = &self.parts[part];
        self.sync.state.lock().unwrap().scheds[part].set_prio(plate_prio(part, p.urgency, focus));
    }

    /// Part `part`'s targets made so far.
    pub fn done_targets(&self, part: usize) -> Vec<Task> {
        self.sync.state.lock().unwrap().scheds[part].done_targets()
    }

    /// Every target of part `part` is made.
    pub fn finished(&self, part: usize) -> bool {
        self.sync.state.lock().unwrap().scheds[part].finished()
    }

    /// Helpers recruited and not yet gone.
    #[cfg(test)]
    pub(crate) fn helpers(&self) -> usize {
        self.sync.state.lock().unwrap().workers
    }

    /// Runs ready tasks on up to `par::threads()` helpers until every target is made or the
    /// cancel stops it.
    ///
    /// The caller leads and works: it runs ready tasks itself, and recruits helpers
    /// ([`par::crew`]) for more, one at a time: each that starts with more ready recruits the
    /// next. A crew so never waits on a helper that has not started. A helper with no ready task
    /// leaves at once, so a plate whose ready work is narrower than its helpers holds no thread or
    /// CPU permit it does not use; the lead, with none ready for it, waits without its permit for
    /// the helpers that run.
    pub fn run(&self, ctl: PlateCtl) -> PlateRun {
        let regions: Vec<(usize, usize)> = {
            let mut s = self.sync.state.lock().unwrap();
            (0..self.parts.len()).filter_map(|i| {
                let count = self.parts[i].plan.regions_known().filter(|_| !s.announced[i])?;
                s.announced[i] = true;
                Some((i, count))
            }).collect()
        };
        if let Some(e) = ctl.emit {
            for (i, count) in regions {
                e(i, Emit::Regions { count });
            }
        }
        let stop = std::sync::atomic::AtomicBool::new(false);
        let threads = par::threads();
        // The part of the best ready task (none once the run is stale).
        let best = |s: &mut PlateState, stale: bool| {
            if stale { return None }
            (0..self.parts.len()).filter_map(|i| s.scheds[i].peek().map(|p| (*p, i))).max().map(|(_, i)| i)
        };
        // One more helper, unless one is still starting or every core has one.
        let recruit = |s: &mut PlateState| {
            let more = s.starting == 0 && s.workers < threads;
            if more {
                s.starting += 1;
                s.workers += 1;
            }
            more
        };
        // Ready tasks, one after another, until none is ready for this worker, or (a helper that
        // `may_leave`) a higher-class job waits for its thread.
        let work = |crew: &dyn par::Crew, may_leave: bool| loop {
            if let Some(poll) = ctl.poll {
                poll();
            }
            let stale = ctl.cancelled();
            // a higher-class job waits for this helper's thread: it leaves at this task boundary
            let leave = may_leave && crew.leave();
            let mut s = self.sync.state.lock().unwrap();
            let next = if stop.load(Relaxed) || leave { None } else { best(&mut s, stale) };
            let Some(i) = next else {
                s.workers -= 1;
                drop(s);
                self.sync.wake.notify_all();
                return;
            };
            let t = s.scheds[i].next().expect("a ready task");
            let more = best(&mut s, stale).is_some() && recruit(&mut s);
            let inputs = makepad_fab_slicer::par::recording().then(|| s.scheds[i].span_inputs(t));
            drop(s);
            if more {
                crew.recruit();
            }
            tick();
            let part = &self.parts[i];
            let began = std::time::Instant::now();
            let start_ns = if inputs.is_some() { makepad_fab_slicer::par::now_ns() } else { 0 };
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), par::Cancelled> {
                if t.stage == Stage::Trace {
                    let raw = part.plan.cut(t.k());
                    if let Some(f) = ctl.traced {
                        f(i, t.k(), &raw);
                    }
                    part.plan.put_trace(t.k(), raw, None);
                    if let Some(f) = ctl.tick {
                        f(1);
                    }
                    return Ok(());
                }
                let cancel = || ctl.cancelled();
                part.plan.run(t, &cancel)
            }));
            self.busy[i].fetch_add(began.elapsed().as_nanos() as u64, Relaxed);
            if let (Some(e), Ok(Ok(()))) = (ctl.emit, &ran) {
                let plan = part.plan;
                match t.stage {
                    Stage::Regions => {
                        self.sync.state.lock().unwrap().announced[i] = true;
                        e(i, Emit::Regions { count: plan.streams().unwrap() });
                    }
                    Stage::Supports => e(i, Emit::Supports),
                    Stage::Vase => {
                        for (index, (z, h, layer)) in plan.ramp.get().iter().enumerate() {
                            e(i, Emit::Layer { region: 0, index, z: *z, h: *h, layer });
                        }
                    }
                    Stage::Layer if !plan.vase => {
                        let (z, h) = plan.heights[t.k()];
                        for (r, layer) in plan.finished_layers(t.stream as usize, t.k()).iter().enumerate() {
                            let region = if plan.paint_at.is_some() { r } else { t.stream as usize };
                            e(i, Emit::Layer { region, index: t.k(), z, h, layer });
                        }
                    }
                    _ => {}
                }
            }
            let span = inputs.map(|(ready_ns, after)| {
                let id = makepad_fab_slicer::par::span_id();
                let lane = makepad_fab_slicer::par::worker();
                makepad_fab_slicer::par::push_span(makepad_fab_slicer::par::Span { id, lane, ready_ns, start_ns, end_ns: makepad_fab_slicer::par::now_ns(), after, stage: t.stage.name(), part: part.plan.oi as u32, layer: t.span_layer(), revision: ctl.revision });
                id
            });
            let mut s = self.sync.state.lock().unwrap();
            match ran {
                Ok(Ok(())) => {
                    if let Some(id) = span {
                        s.scheds[i].ran(t, id);
                    }
                    s.scheds[i].done(part.plan, t)
                }
                // Stale between its start and its first step: the part stops here.
                Ok(Err(par::Cancelled)) => {}
                Err(payload) => {
                    stop.store(true, Relaxed);
                    s.workers -= 1;
                    drop(s);
                    self.sync.wake.notify_all();
                    std::panic::resume_unwind(payload);
                }
            }
        };
        let lead = |crew: &dyn par::Crew| loop {
            let stale = ctl.cancelled();
            let mut s = self.sync.state.lock().unwrap();
            if stop.load(Relaxed) {
                return;
            }
            // A ready task: the lead runs it itself, the crew's first worker. A crew so never waits
            // on a helper that has not started (in the app, a pool whose every worker is busy, some
            // perhaps other crews' leads).
            if best(&mut s, stale).is_some() {
                s.workers += 1;
                drop(s);
                work(crew, false);
                continue;
            }
            // Nothing is ready and no helper runs (one still starting finds nothing and leaves):
            // the run ends.
            if s.workers == s.starting {
                return;
            }
            // A helper leaving wakes it. It takes its permit again unlocked.
            let mut s = Some(s);
            crew.wait(&mut || drop(self.sync.wake.wait(s.take().unwrap()).unwrap()));
        };
        let helper = |crew: &dyn par::Crew| {
            self.sync.state.lock().unwrap().starting -= 1;
            work(crew, true);
        };
        par::crew(&lead, &helper);
        let s = self.sync.state.lock().unwrap();
        let open = |i: usize| !s.scheds[i].finished();
        if !(0..self.parts.len()).any(open) {
            return PlateRun::Finished;
        }
        // Every trace is cut here when not put: only the cancel leaves work undone.
        assert!(ctl.cancelled(), "a plate stopped with work it could run");
        PlateRun::Cancelled
    }
}

/// Plan object `oi` whole (`window` None: with supports, finished) or model layers
/// `lo..hi` of `heights` on `par::threads()` threads, N layers at once: a [`Plate`] of
/// this one part. Each window reads only its [`plan_reads`] contours.
pub(crate) fn plan(ctl: par::Ctl, oi: usize, o: &ObjectInput, job: &Job, heights: &[(f64, f64)], window: Option<(usize, usize)>) -> Result<(ObjectPlan, Vec<(f64, f64, ObjectLayer)>), par::Cancelled> {
    let plan = PartPlan::new(oi, o, job, heights.to_vec());
    let n = heights.len();
    let empty = || {
        ObjectPlan { info: ObjectInfo::of(o, plan.params.raft_lift), pivot: o.source.pivot_box(), layers: Vec::new(), brim_keep_out: Vec::new(), np_blocks: Vec::new(), skeleton: Default::default() }
    };
    let params = &plan.params;
    let whole = params.exp.vase.is_some() || window.is_none();
    let (lo, hi) = if whole { (0, n) } else { window.map(|(lo, hi)| (lo, hi.min(n))).unwrap() };
    if lo >= hi {
        return Ok((empty(), Vec::new()));
    }
    let targets: Vec<Task> = if window.is_none() { vec![Task::part(Stage::Finish)] } else { plan.layer_targets(lo..hi) };
    let forward = |_: usize, e: Emit| {
        if let Some(f) = ctl.emit {
            f(e);
        }
    };
    let plate = Plate::new(vec![PlatePart { plan: &plan, targets, urgency: 0, focus: ctl.focus }]);
    if plate.run(PlateCtl { cancel: ctl.cancel, emit: Some(&forward), traced: None, poll: None, tick: ctl.tick, revision: 0, }) == PlateRun::Cancelled {
        return Err(par::Cancelled);
    }
    drop(plate);
    if window.is_none() {
        return Ok((plan.take_finished(), Vec::new()));
    }
    let layers = if plan.vase { plan.ramp.take() } else { plan.collect(lo, hi) };
    Ok((empty(), layers))
}

/// A face's shell past its edges, by the margin all round: a mitre would carry a sharp corner
/// three margins out, through what the shell has no reason to cover. Far below a bead's width,
/// the chords' tolerance only decides how many points the fill's clips go through.
fn grown_face(face: &[ExPolygon], margin: f64) -> Vec<ExPolygon> {
    offset_ex(face, margin * SCALE, Join::Round(GROWN_FACE_TOLERANCE * SCALE))
}
const GROWN_FACE_TOLERANCE: f64 = 0.05;

#[cfg(test)]
mod grown_face_tests {
    use super::*;

    #[test]
    fn a_sharp_corners_shell_reaches_the_margin_and_no_further() {
        let at = |x: f64, y: f64| Pt::new((x * SCALE) as i64, (y * SCALE) as i64);
        let face = [ExPolygon::new(Polygon::new(vec![at(0.0, 0.0), at(20.0, 0.0), at(0.0, 1.0)]))];
        let margin = 0.6;
        let grid = makepad_fab_slicer::grid::SegGrid::of_regions(&face, |k| k as u32);
        let reach = grown_face(&face, margin).iter().flat_map(|e| e.rings()).flat_map(|r| r.pts.iter().copied())
            .map(|p| grid.nearest(p, |k| k).map_or(0.0, |d| d.0) / SCALE).fold(0.0, f64::max);
        assert!(reach <= margin + 1e-5 && reach >= margin - GROWN_FACE_TOLERANCE, "reach {reach} mm for a margin of {margin} mm");
    }
}

#[cfg(test)]
mod zone_tests {
    use super::*;
    use makepad_fab_slicer::source::{SettingsZone, SupportRegion};

    const C: [f64; 3] = [10.0, 10.0, 10.0];
    const R: f64 = 5.0;

    /// A 20 mm cube with a solid region round a sphere in its middle, and a later one over its +x
    /// half that sets its pattern and wall count only.
    struct Zoned;

    impl ContourSource for Zoned {
        fn bounds(&self) -> ([f64; 3], [f64; 3]) {
            ([0.0; 3], [20.0; 3])
        }
        fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
            let square = ExPolygon::new(Polygon::new(vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 0.0), Pt::from_mm(20.0, 20.0), Pt::from_mm(0.0, 20.0)]));
            zs.iter().map(|&z| if (0.0..20.0).contains(&z) { vec![square.clone()] } else { Vec::new() }).collect()
        }
        fn settings_zones(&self) -> Vec<SettingsZone> {
            let sphere = SupportRegion::new(Arc::new(|x, y, z| ((x - C[0]).powi(2) + (y - C[1]).powi(2) + (z - C[2]).powi(2)).sqrt() - R), Some((C.map(|v| v - R), C.map(|v| v + R))));
            let (lo, hi) = ([10.0, 0.0, 0.0], [20.0, 20.0, 20.0]);
            let half = SupportRegion::new(Arc::new(move |x, y, z| (0..3).map(|k| (lo[k] - [x, y, z][k]).max([x, y, z][k] - hi[k])).fold(f64::MIN, f64::max)), Some((lo, hi)));
            let values = |v: &[(&str, &str)]| v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            vec![
                SettingsZone { region: sphere, values: values(&[("sparse_infill_density", "100%")]) },
                SettingsZone { region: half, values: values(&[("sparse_infill_pattern", "grid"), ("wall_loops", "3")]) },
            ]
        }
    }

    /// Each layer's regions are their cuts at its plane, exactly, and a later region overrides
    /// only the keys it sets: the sphere's whole circle prints solid at that height, the later
    /// box's pattern over its half of it too, and the box's half square takes its pattern and wall
    /// count.
    #[test]
    fn a_region_is_its_exact_cut_and_a_later_one_overrides_only_its_keys() {
        let o = ObjectInput { name: "cube".into(), source: Box::new(Zoned), filament: 0, overrides: Vec::new() };
        let job = crate::plate_tests::job("classic");
        let heights = plan_heights(&o, &job);
        let plan = PartPlan::new(0, &o, &job, heights.clone());
        let mut through = 0;
        for (k, &(z, h)) in heights.iter().enumerate() {
            plan.put_trace(k, plan.cut(k), None);
            plan.run(Task::layer(Stage::Outline, 0, k), &|| false).unwrap();
            let cells = plan.zone_at[k].get();
            let area = |pick: &dyn Fn(&ZoneSet) -> bool| cells.iter().filter(|(_, s)| pick(s)).map(|(a, _)| total_area_mm2(a)).sum::<f64>();
            let grid = area(&|s| s.pattern == Some(pattern::Sparse::Grid) && s.walls == Some(3));
            assert!((grid - 200.0).abs() < 0.01, "layer {k}: the box's half square, {grid:.3} mm²");
            let solid = plan.solid_zones(k);
            let dz = cut_z(&plan.params, z, h) - C[2];
            if dz.abs() > R {
                assert!(solid.is_empty(), "layer {k}: the plane misses the sphere");
                continue;
            }
            let r = (R * R - dz * dz).sqrt();
            // the disc's area as a radius
            let seen = (total_area_mm2(&solid) / std::f64::consts::PI).sqrt();
            assert!((seen - r).abs() < 0.01, "layer {k}: the solid disc is {seen:.4} mm round, the sphere's circle {r:.4}");
            for q in solid.iter().flat_map(|e| e.rings()).flat_map(|ring| ring.pts.iter()) {
                let (x, y) = (q.x_mm(), q.y_mm());
                let d = (x - C[0]).hypot(y - C[1]);
                assert!((d - r).abs() < 0.01 || (x - 10.0).abs() < 1e-3 && d <= r + 0.01, "layer {k}: ({x:.3}, {y:.3}) is off the circle (r {r:.3})");
            }
            through += 1;
        }
        assert!(through >= 45, "{through} layers through the sphere");
    }
}
