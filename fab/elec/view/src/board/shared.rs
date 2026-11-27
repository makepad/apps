//! One board's drawing, built once on the task pool and shared by every view of it.
//!
//! The board panes, the 3D decal pass and the art raster all read the same `Arc<BoardDraw>`
//! (`BoardDraws::current`): one build and one upload per board, however many views show it.
//! The owner is the board (its unit id), never a pane, the same rule as the host's segment rows.
//!
//! An edit is incremental: the job diffs the new board against the drawn one (`BoardDelta`, plus
//! the zone fills that changed), builds the scene of the edited items only and re-batches the
//! tiles their old and new rows touch (`rebatch`). Fill-only edits scene just the changed batch
//! region; every other tile's rows and every untouched
//! stream's publication are the previous ones, so the frame builder uploads only the new tiles.
//! The scene, item table and pick index of the whole new board follow in a second job
//! (`BoardGeom`): until it is in, hover and semantic nodes answer from the board before the edit.
//! A global change (stackup, outline, nets, classes, rules), a new lowering or the first board
//! is a full build.
//!
//! With a live bench, a wire-graph job precedes the picture job. `poll` registers its trees with
//! the host on the UI thread; the picture job builds the live row index and instance rows. The wire
//! job compares the trees with the drawing's (on the pool): an edit that keeps the copper's
//! connectivity, such as a drag, keeps its rows and does no registration work on the UI thread.
//! Cost on the UI thread: `set_board` and `poll` are O(boards) plus, when a job finishes, O(streams)
//! to wrap its result and the measured host tree registration. Other O(items) work runs on the pool,
//! freeing included: a drawing a view lets go of goes back through `BoardDraws::retire`, and its
//! last reference is dropped on the pool (a board's scene, pick index and fills are large;
//! freeing them on the UI thread is a visible stall).
//!
//! The wires, picture and geometry jobs run as `Class::Interactive`: they are what a person waits
//! for on screen (the first picture, an edit's feedback), and a Visible job is FIFO behind every
//! queued zone-fill tile and connectivity job. On a 100 000-segment board the first picture
//! otherwise waited 25 s behind the fills. Dropping retired drawings stays Background.

use super::batch::{batch_scene, rebatch, BoardBatch, KindBatch, RegionEdit, Templates, BATCH_LOD};
use super::checks::CheckIndex;
use super::flow::{net_wires, NetWires};
use super::liveix::{trees_in, LiveIx};
use super::inst::{Common, PadInst, PadTemplate};
use super::nodes::{ItemIndex, NameIndex};
use super::pick::ScenePick;
use super::scene::{scene_of, scene_of_fill_region, scene_of_items, DrawScene};
use super::tiles::TileId;
use crate::live::segs::{TreeIn, TreeOwner};
use crate::snapshot::{DesignSnapshot, ElecLive, ProposalId};
use fab_elec_bench::ValueIndex;
use fab_elec_circuit::Circuit;
use fab_elec_board::{zone, Board, BoardDelta, ZoneFill};
use fab_elec_core::{BBox, BoardId, ItemId, LayerId, Nm};
use makepad_widgets_core::{makepad_platform::retained_instances::{InstanceSegment, RetainedInstances}, *};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

/// One stream's publication (the instance rows the frame builder uploads), if it has rows.
pub type Publication = Option<RetainedInstances>;

/// A layer's three streams.
#[derive(Clone)]
pub struct LayerPubs {
    pub layer: LayerId,
    pub tris: Publication,
    pub strokes: Publication,
    pub pads: Publication,
}

/// Every stream of a board as publications, in the batch's layer order.
#[derive(Clone, Default)]
pub struct BoardPubs {
    pub layers: Vec<LayerPubs>,
    pub substrate: Publication,
    /// One oriented, bounded glow per numbered placement. Shared by all board panes.
    pub glows: Publication,
    pub holes: Publication,
}

/// The scene, item table, pick index and check index of one board.
pub struct BoardGeom {
    pub board: Arc<Board>,
    pub scene: Arc<DrawScene>,
    pub items: Arc<ItemIndex>,
    /// Item names and stable semantic keys, built with the geometry on the pool.
    pub names: Arc<NameIndex>,
    pub pick: Arc<ScenePick>,
    /// Finding selections resolved against this board's scene, built on the pool.
    pub checks: Arc<CheckIndex>,
    /// Worker time to build it, milliseconds.
    pub ms: f64,
}

/// How the current picture came about.
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawTiming {
    /// UI clock when the board was handed in (`Cx::monotonic_now`).
    pub requested: f64,
    /// UI clock when the picture was installed.
    pub installed: f64,
    /// Worker time of the picture job (live index, diff, partial scene, rows, publications),
    /// milliseconds.
    pub picture_ms: f64,
    /// Worker time of the wire-graph job, milliseconds (0 without a live bench).
    pub wires_ms: f64,
    /// UI time of the host's tree registration, milliseconds (0 without a live bench).
    pub trees_ms: f64,
    /// Rebatched from the previous picture (false: a full build).
    pub incremental: bool,
    /// Items the edit touched (0 for a full build).
    pub edited: usize,
}

/// Picture and geometry revisions within one slot. A delta is valid only against the
/// preceding picture of that slot. Crossing from a proposal to Live invalidates all tiles.
#[derive(Clone)]
pub struct DrawRevision {
    /// Rises with each new picture.
    pub gen: u64,
    /// Rises when the pick and check geometry changes.
    pub geom_gen: u64,
    /// Board-nm boxes changed since the previous picture. None invalidates the whole board.
    pub changed: Option<Arc<[BBox]>>,
}

/// One board's drawing (module docs).
#[derive(Clone)]
pub struct BoardDraw {
    pub unit: BoardId,
    built: DrawRevision,
    /// Set once on adoption so every holder keeps the exact drawing and Live consumers see
    /// a new generation even when the two slots happened to have the same generation.
    adopted: OnceLock<DrawRevision>,
    pub board: Arc<Board>,
    pub fills: Arc<Vec<ZoneFill>>,
    /// The simulation rows used by these instances; empty without a live bench.
    pub live: Arc<LiveIx>,
    /// The lowering that numbered the live rows; `None` without a live bench.
    pub index: Option<Arc<ValueIndex>>,
    /// Kept with the drawing so the wire job's large result is retired on the pool too.
    _wires: Option<Arc<Vec<NetWires>>>,
    /// The copper trees registered for these rows; `None` without a live bench.
    trees: Option<Arc<TreeRec>>,
    /// The instance rows per (layer, stream, tile) and the item-to-rows table.
    pub batch: Arc<BoardBatch>,
    pub pubs: BoardPubs,
    /// Of `geom.board`: `board`, or for a moment after an incremental edit the board before it.
    pub geom: Arc<BoardGeom>,
    pub timing: DrawTiming,
}

impl std::ops::Deref for BoardDraw {
    type Target = DrawRevision;
    fn deref(&self) -> &DrawRevision { self.adopted.get().unwrap_or(&self.built) }
}

/// A board's copper trees as registered with the host's segment rows, and the rows they got.
struct TreeRec {
    trees: Arc<Vec<TreeIn>>,
    rows: Arc<Vec<Vec<u32>>>,
    /// `SegRows::version` right after the registration: while it is unchanged, the registration
    /// stands as made.
    version: u64,
}

struct Want {
    board: Arc<Board>,
    fills: Arc<Vec<ZoneFill>>,
    at: f64,
    /// The picture on which this job based its tile delta.
    base_gen: Option<u64>,
}

impl Want {
    fn new(board: Arc<Board>, fills: Arc<Vec<ZoneFill>>) -> Self {
        Self { board, fills, at: Cx::monotonic_now(), base_gen: None }
    }
}

struct Picture {
    batch: BoardBatch,
    pubs: BoardPubs,
    geom: Option<BoardGeom>,
    live: Arc<LiveIx>,
    index: Option<Arc<ValueIndex>>,
    wires: Option<Arc<Vec<NetWires>>>,
    trees: Option<Arc<TreeRec>>,
    ms: f64,
    wires_ms: f64,
    trees_ms: f64,
    edited: Option<usize>,
    /// `BoardDraw::changed`.
    changed: Option<Vec<BBox>>,
}

/// A wire job's result. `kept` is the previous drawing's trees when `same` (equal to `trees`), else
/// a copy of `trees` made on the pool: `trees` goes to the host, `kept` stays with the drawing.
struct Wired {
    wires: Arc<Vec<NetWires>>,
    trees: Vec<TreeIn>,
    kept: Arc<Vec<TreeIn>>,
    same: bool,
    ms: f64,
}

enum Built {
    Wires(Wired),
    Picture(Picture),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoardSource {
    Live,
    Proposal(ProposalId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DrawKey {
    source: BoardSource,
    unit: BoardId,
}

struct Slot {
    key: DrawKey,
    want: Option<Want>,
    current: Option<Arc<BoardDraw>>,
    job: Option<(TaskHandle<Built>, Want)>,
    geom_job: Option<TaskHandle<BoardGeom>>,
}

impl Slot {
    fn finish_picture(&self, want: &Want, picture: Picture) -> Arc<BoardDraw> {
        let now = Cx::monotonic_now();
        let timing = DrawTiming {
            requested: want.at,
            installed: now,
            picture_ms: picture.ms,
            wires_ms: picture.wires_ms,
            trees_ms: picture.trees_ms,
            incremental: picture.edited.is_some(),
            edited: picture.edited.unwrap_or(0),
        };
        let (gen, geom_gen) = self.current.as_ref().map_or((0, 0), |c| (c.gen, c.geom_gen));
        let need_geom = picture.geom.is_none();
        let geom = match (picture.geom, self.current.as_ref()) {
            (Some(g), _) => Arc::new(g),
            (None, Some(cur)) => cur.geom.clone(),
            (None, None) => unreachable!("an incremental picture has a previous drawing"),
        };
        Arc::new(BoardDraw {
            unit: self.key.unit,
            built: DrawRevision { gen: gen + 1, geom_gen: geom_gen + (!need_geom) as u64,
                changed: picture.changed.filter(|_| want.base_gen == Some(gen)).map(Arc::from) },
            adopted: OnceLock::new(),
            board: want.board.clone(),
            fills: want.fills.clone(),
            live: picture.live,
            index: picture.index,
            _wires: picture.wires,
            trees: picture.trees,
            batch: Arc::new(picture.batch),
            pubs: picture.pubs,
            geom,
            timing,
        })
    }

    fn publish(&mut self, draw: Arc<BoardDraw>, changed: &mut Vec<BoardId>) -> Option<Arc<BoardDraw>> {
        if self.key.source == BoardSource::Live { changed.push(self.key.unit); }
        self.current.replace(draw)
    }
}

/// The shared board drawings of this process (a `Cx` global; module docs).
#[derive(Default)]
pub struct BoardDraws {
    slots: Vec<Slot>,
    changed: Vec<BoardId>,
    templates: Option<Templates>,
    /// Drawings let go of, waiting until this is their last reference to be dropped on the pool.
    retired: Vec<Arc<BoardDraw>>,
    /// Replaced wants, removed slots and stale geometries: they may hold a board's last references,
    /// so they are dropped on the pool with the retired drawings.
    discarded: Vec<Box<dyn Send>>,
}

impl BoardDraws {
    /// The instance layouts every build uses; the first pane to capture them hands them in.
    pub fn set_templates(cx: &mut Cx, templates: Templates) {
        let me = cx.global::<BoardDraws>();
        if me.templates.is_none() {
            me.templates = Some(templates);
            Self::pump(cx);
        }
    }

    pub fn has_templates(cx: &mut Cx) -> bool {
        cx.global::<BoardDraws>().templates.is_some()
    }

    /// The captured instance layouts (plain data), for views that batch a few items themselves.
    pub fn templates(cx: &mut Cx) -> Option<Templates> {
        cx.global::<BoardDraws>().templates.clone()
    }

    /// Draw this board (keyed by `board.unit`). Handing in the board and fills that are already
    /// wanted or drawn (the same `Arc`s) does nothing, so every view of a board may call it.
    pub fn set_board(cx: &mut Cx, board: Arc<Board>, fills: Arc<Vec<ZoneFill>>) {
        Self::set_source(cx, BoardSource::Live, board, fills);
    }

    pub(super) fn set_source(cx: &mut Cx, source: BoardSource, board: Arc<Board>, fills: Arc<Vec<ZoneFill>>) {
        let me = cx.global::<BoardDraws>();
        let key = DrawKey { source, unit: board.unit };
        let slot = Self::slot(&mut me.slots, key);
        let same = |b: &Arc<Board>, f: &Arc<Vec<ZoneFill>>| Arc::ptr_eq(b, &board) && Arc::ptr_eq(f, &fills);
        let known = slot.want.as_ref().map(|w| same(&w.board, &w.fills))
            .or_else(|| slot.job.as_ref().map(|(_, w)| same(&w.board, &w.fills)))
            .or_else(|| slot.current.as_ref().map(|c| same(&c.board, &c.fills)))
            .unwrap_or(false);
        if !known {
            if let Some(old) = slot.want.replace(Want::new(board, fills)) {
                me.discarded.push(Box::new(old));
            }
            Self::pump(cx);
        }
    }

    fn slot(slots: &mut Vec<Slot>, key: DrawKey) -> &mut Slot {
        if let Some(i) = slots.iter().position(|s| s.key == key) { return &mut slots[i]; }
        slots.push(Slot { key, want: None, current: None, job: None, geom_job: None });
        slots.last_mut().unwrap()
    }

    /// Forget a board (its unit went away).
    pub fn remove(cx: &mut Cx, unit: BoardId) {
        Self::remove_key(cx, DrawKey { source: BoardSource::Live, unit });
    }

    /// End other comparisons. An exact picture moves into the Live slot immediately.
    /// The next poll refreshes its simulation index, wires and trees as for any Live picture.
    pub fn keep_preview(cx: &mut Cx, id: Option<ProposalId>, snap: &DesignSnapshot) {
        if !cx.has_global::<BoardDraws>() { return; }
        let me = cx.get_global::<BoardDraws>();
        for i in (0..me.slots.len()).rev() {
            let key = me.slots[i].key;
            if !matches!(key.source, BoardSource::Proposal(old) if Some(old) != id) { continue; }
            let mut proposal = me.slots.remove(i);
            if let Some((job, _)) = &proposal.job { job.cancel(); }
            if let Some(draw) = proposal.current.take() {
                let matches = snap.live().unit(key.unit).and_then(|u| u.board.as_ref())
                    .is_some_and(|b| Arc::ptr_eq(b, &draw.board))
                    && snap.of(&snap.fills, key.unit).is_some_and(|f| Arc::ptr_eq(&f.value, &draw.fills));
                if matches {
                    me.adopt(draw, proposal.geom_job.take());
                } else {
                    me.retired.push(draw);
                }
            }
            if let Some(job) = &proposal.geom_job { job.cancel(); }
            me.discarded.push(Box::new(proposal));
        }
        Self::sweep(cx);
    }

    fn adopt(&mut self, draw: Arc<BoardDraw>, geom_job: Option<TaskHandle<BoardGeom>>) {
        let live = Self::slot(&mut self.slots, DrawKey { source: BoardSource::Live, unit: draw.unit });
        // Every Live request comes from a snapshot no newer than the one this drawing matches
        // (the host hands its boards over after the drain that ends the comparison), so any
        // other build is stale: finishing it would put the earlier design back over this one.
        if let Some((job, want)) = live.job.take() {
            job.cancel();
            self.discarded.push(Box::new((job, want)));
        }
        if let Some(want) = live.want.take() {
            self.discarded.push(Box::new(want));
        }
        if let Some(job) = live.geom_job.take() {
            job.cancel();
            self.discarded.push(Box::new(job));
        }
        live.geom_job = geom_job;
        let (gen, geom_gen) = live.current.as_ref().map_or((1, 1), |c| (c.gen + 1, c.geom_gen + 1));
        draw.adopted.get_or_init(|| DrawRevision { gen, geom_gen, changed: None });
        self.retired.extend(live.publish(draw, &mut self.changed));
        SignalToUI::set_ui_signal();
    }

    fn remove_key(cx: &mut Cx, key: DrawKey) {
        if key.source == BoardSource::Live && cx.has_global::<ElecLive>() {
            cx.get_global::<ElecLive>().sim.trees.remove(TreeOwner::Copper(key.unit));
        }
        let me = cx.global::<BoardDraws>();
        if let Some(i) = me.slots.iter().position(|s| s.key == key) {
            let mut slot = me.slots.remove(i);
            me.retired.extend(slot.current.take());
            me.discarded.push(Box::new(slot));
        }
        Self::sweep(cx);
    }

    /// Hand back a drawing a view no longer draws (instead of dropping it): the last reference to
    /// any drawing is dropped on the pool, never on the UI thread.
    pub fn retire(cx: &mut Cx, draw: Arc<BoardDraw>) {
        let me = cx.global::<BoardDraws>();
        if !me.slots.iter().any(|s| s.current.as_ref().is_some_and(|current| Arc::ptr_eq(current, &draw)))
            && !me.retired.iter().any(|old| Arc::ptr_eq(old, &draw)) {
            me.retired.push(draw);
        } else {
            drop(draw);
        }
        Self::sweep(cx);
    }

    /// Free replaced pane data on the pool alongside discarded board inputs and results.
    pub(super) fn discard(cx: &mut Cx, data: impl Send + 'static) {
        cx.global::<BoardDraws>().discarded.push(Box::new(data));
        Self::sweep(cx);
    }

    /// Sends the retired drawings nobody else holds, and the discarded inputs and results, to the
    /// pool to be dropped. O(retired + discarded).
    fn sweep(cx: &mut Cx) {
        let me = cx.global::<BoardDraws>();
        if me.retired.is_empty() && me.discarded.is_empty() {
            return;
        }
        let (last, kept): (Vec<_>, Vec<_>) = me.retired.drain(..).partition(|d| Arc::strong_count(d) == 1);
        me.retired = kept;
        let discarded = std::mem::take(&mut me.discarded);
        if last.is_empty() && discarded.is_empty() {
            return;
        }
        // A closed pool (shutdown) hands the job back and it drops here: nothing else is left to run.
        if let Err(e) = cx.task_pool().submit(Class::Background, move || drop((last, discarded))) {
            log!("fab.board: retired drawings were not queued for freeing ({e:?})");
        }
    }

    /// A drawing is on its way: a board is wanted, being drawn or having its geometry built, once
    /// a view has handed in the templates every build needs (before that no view draws boards).
    /// O(boards).
    pub fn drawing(&self) -> bool {
        self.templates.is_some() && self.slots.iter().any(|s| s.want.is_some() || s.job.is_some() || s.geom_job.is_some())
    }

    /// The board's current drawing, if one is in.
    pub fn current(cx: &mut Cx, unit: BoardId) -> Option<Arc<BoardDraw>> {
        Self::current_source(cx, BoardSource::Live, unit)
    }

    pub(super) fn current_source(cx: &mut Cx, source: BoardSource, unit: BoardId) -> Option<Arc<BoardDraw>> {
        let key = DrawKey { source, unit };
        cx.global::<BoardDraws>().slots.iter().find(|s| s.key == key).and_then(|s| s.current.clone())
    }

    /// Takes finished jobs and starts the next ones; returns the boards whose drawing changed.
    /// Every view calls it on `Event::Signal` (the pool signals once a result is stored); a
    /// second caller in the same event finds nothing left to take.
    pub fn poll(cx: &mut Cx) -> Vec<BoardId> {
        let live = cx.has_global::<ElecLive>();
        let mut geoms = Vec::new();
        let mut wired = Vec::new();
        let me = cx.global::<BoardDraws>();
        for slot in &mut me.slots {
            if let Some(job) = slot.geom_job.as_mut() {
                if let Some(result) = job.try_take() {
                    slot.geom_job = None;
                    match result {
                        Ok(geom) => {
                            if let Some(cur) = slot.current.as_ref().filter(|cur| Arc::ptr_eq(&geom.board, &cur.board)) {
                                let mut next = BoardDraw::clone(cur);
                                next.geom = Arc::new(geom);
                                next.adopted.get_mut().unwrap_or(&mut next.built).geom_gen += 1;
                                me.retired.extend(slot.publish(Arc::new(next), &mut me.changed));
                            } else {
                                // An older geometry finishing after a newer full build is freed on the pool.
                                me.discarded.push(Box::new(geom));
                            }
                        }
                        Err(e) => log!("fab.board: the board geometry build failed: {e:?}"),
                    }
                }
            }
            let Some((job, _)) = slot.job.as_mut() else { continue };
            let Some(result) = job.try_take() else { continue };
            let (_, want) = slot.job.take().unwrap();
            match result {
                Ok(Built::Wires(w)) => {
                    wired.push((slot.key, want, slot.current.clone(), w));
                }
                Ok(Built::Picture(picture)) => {
                    let need_geom = picture.geom.is_none();
                    let draw = slot.finish_picture(&want, picture);
                    me.retired.extend(slot.publish(draw, &mut me.changed));
                    if need_geom {
                        geoms.push((slot.key, want.board, want.fills));
                    }
                }
                Err(e) => {
                    me.discarded.push(Box::new(want));
                    log!("fab.board: the board build failed: {e:?}");
                }
            }
        }
        for (key, mut want, old, Wired { wires, trees, kept, same, ms: wires_ms }) in wired {
            let unit = key.unit;
            // A wires job starts only with this global, which stays installed in Cx.
            let sim = &mut cx.get_global::<ElecLive>().sim;
            let t0 = Cx::monotonic_now();
            let standing = old.as_ref().and_then(|o| o.trees.clone())
                .filter(|rec| same && Arc::ptr_eq(&rec.trees, &kept) && rec.version == sim.trees.version());
            let (rec, unused) = match standing {
                // Equal trees, and nothing registered since: their rows stand, nothing to do here.
                Some(rec) => (rec, Some(trees)),
                None => {
                    let rows = sim.trees.set(TreeOwner::Copper(unit), trees);
                    (Arc::new(TreeRec { trees: kept, rows: Arc::new(rows), version: sim.trees.version() }), None)
                }
            };
            let trees_ms = (Cx::monotonic_now() - t0) * 1e3;
            let index = sim.index.clone();
            if trees_ms > 2.0 {
                log!("fab.board: copper tree registration took {trees_ms:.2} ms on the UI thread");
            }
            let me = cx.global::<BoardDraws>();
            if let Some(unused) = unused {
                me.discarded.push(Box::new(unused));
            }
            let templates = me.templates.clone().unwrap();
            let circuit = cx.get_global::<ElecLive>().snap.live().unit(unit).map(|u| u.circuit.clone());
            want.base_gen = old.as_ref().map(|d| d.gen);
            let (board, fills) = (want.board.clone(), want.fills.clone());
            let job = move || {
                let t0 = Cx::monotonic_now();
                let live = Arc::new(LiveIx::new(&board, &index, &wires, &rec.rows));
                let previous = old.as_ref().and_then(|d| d.pubs.glows.as_ref());
                let glows = footprint_glows(&board, circuit.as_deref(), &live, &templates.pad, previous);
                let mut picture = picture(old, board, fills, &templates, live, Some(index));
                picture.pubs.glows = glows;
                picture.ms = (Cx::monotonic_now() - t0) * 1e3;
                picture.wires = Some(wires);
                picture.trees = Some(rec);
                picture.wires_ms = wires_ms;
                picture.trees_ms = trees_ms;
                Built::Picture(picture)
            };
            let submitted = cx.task_pool().submit(Class::Interactive, job);
            let me = cx.global::<BoardDraws>();
            let slot = me.slots.iter_mut().find(|s| s.key == key).unwrap();
            match submitted {
                Ok(handle) => slot.job = Some((handle, want)),
                Err(e) => {
                    log!("fab.board: the board build was not queued ({e:?}); retrying");
                    // Keep a newer edit that arrived while the wires were being built.
                    if slot.want.is_none() {
                        slot.want = Some(want);
                    } else {
                        me.discarded.push(Box::new(want));
                    }
                }
            }
        }
        for (key, board, fills) in geoms {
            let job = move || geom_of(board, &fills);
            match cx.task_pool().submit(Class::Interactive, job) {
                Ok(handle) => {
                    let me = cx.global::<BoardDraws>();
                    if let Some(slot) = me.slots.iter_mut().find(|s| s.key == key) {
                        if let Some(old) = slot.geom_job.replace(handle) {
                            me.discarded.push(Box::new(old));
                        }
                    }
                }
                Err(e) => log!("fab.board: the board geometry build was not queued ({e:?}); the next edit rebuilds it"),
            }
        }
        if live {
            let index = cx.get_global::<ElecLive>().sim.index.clone();
            for slot in &mut cx.global::<BoardDraws>().slots {
                if slot.key.source == BoardSource::Live && slot.want.is_none() && slot.job.is_none() {
                    if let Some(cur) = &slot.current {
                        if !cur.index.as_ref().is_some_and(|old| Arc::ptr_eq(old, &index)) {
                            // Bypass set_board's same-Arc shortcut: this lowering renumbered rows.
                            slot.want = Some(Want::new(cur.board.clone(), cur.fills.clone()));
                        }
                    }
                }
            }
        }
        Self::pump(cx);
        Self::sweep(cx);
        std::mem::take(&mut cx.global::<BoardDraws>().changed)
    }

    /// Starts a wires or picture job for every wanted board with no job running.
    fn pump(cx: &mut Cx) {
        let live = cx.has_global::<ElecLive>();
        let me = cx.global::<BoardDraws>();
        let Some(templates) = me.templates.clone() else { return };
        let mut start = Vec::new();
        for slot in &mut me.slots {
            if slot.job.is_none() {
                if let Some(want) = slot.want.take() {
                    start.push((slot.key, want, slot.current.clone()));
                }
            }
        }
        for (key, mut want, old) in start {
            want.base_gen = old.as_ref().map(|d| d.gen);
            let (board, fills, t) = (want.board.clone(), want.fills.clone(), templates.clone());
            let prev = old.as_ref().and_then(|o| o.trees.as_ref().map(|rec| rec.trees.clone()));
            let job = move || {
                if live && key.source == BoardSource::Live {
                    let t0 = Cx::monotonic_now();
                    let wires = Arc::new(net_wires(&board, &fills));
                    let trees = trees_in(&board, &wires);
                    let (kept, same) = match prev {
                        Some(prev) if *prev == trees => (prev, true),
                        _ => (Arc::new(trees.clone()), false),
                    };
                    Built::Wires(Wired { wires, trees, kept, same, ms: (Cx::monotonic_now() - t0) * 1e3 })
                } else {
                    Built::Picture(picture(old, board, fills, &t, Arc::new(LiveIx::none()), None))
                }
            };
            let submitted = cx.task_pool().submit(Class::Interactive, job);
            let Some(slot) = cx.global::<BoardDraws>().slots.iter_mut().find(|s| s.key == key) else { continue };
            match submitted {
                Ok(handle) => slot.job = Some((handle, want)),
                Err(e) => {
                    // kept wanted: the next poll or set_board tries again; the old picture stays up
                    log!("fab.board: the board build was not queued ({e:?}); retrying");
                    slot.want = Some(want);
                }
            }
        }
    }
}

/// Build the underlay on the picture worker, using the same placement and numbering
/// as its pad stream. A mirrored footprint's centre follows the placement transform.
fn footprint_glows(board: &Board, circuit: Option<&Circuit>, live: &LiveIx, template: &PadTemplate,
    previous: Option<&RetainedInstances>) -> Publication {
    use fab_elec_core::{geom::NM_PER_MM, to_mm};
    let mut rows = Vec::new();
    for placed in &board.placed {
        let row = live.part(placed.id);
        if row < 0.0 { continue; }
        let mut bounds = placed.footprint.courtyard.bbox();
        if bounds.is_empty() { bounds = placed.footprint.bbox(); }
        if bounds.is_empty() { continue; }
        let center = placed.to_board(bounds.center());
        let mut c = Common::rest(if placed.id.0 < (1 << 24) { placed.id.0 as f32 } else { -1.0 });
        c.live_part = row;
        c.pose_a[0] = to_mm(placed.at.x) as f32;
        c.pose_a[1] = to_mm(placed.at.y) as f32;
        if let Some(emitter) = circuit.and_then(|c| c.comps.get(placed.comp.ix())).and_then(|comp| comp.part.emitter.as_ref()) {
            c.ink[0] = crate::canvas::emitter::emitter_ix(&emitter.light);
        }
        template.append(&mut rows, &PadInst {
            c, rect: [to_mm(center.x) as f32, to_mm(center.y) as f32,
                (0.5 * bounds.width() as f64 / NM_PER_MM) as f32, (0.5 * bounds.height() as f64 / NM_PER_MM) as f32],
            rot: placed.rot.radians() as f32, corner: 0.0, hole: [0.0; 4], hole_rot: 0.0,
        });
    }
    if rows.is_empty() { return None; }
    if let Some(prev) = previous {
        if matches!(prev.segments(), [only] if only.data() == rows.as_slice()) { return Some(prev.clone()); }
    }
    match RetainedInstances::from_segments(template.slots(), vec![InstanceSegment::new(rows.into())]) {
        Ok(publication) => Some(publication),
        Err(e) => { log!("fab.board: cannot publish part underlays: {e}"); None }
    }
}

/// The picture of `board`: incremental from `old` when the lowering is unchanged and only items
/// changed, else a full build (which also yields the geometry). Runs on the pool.
fn picture(
    old: Option<Arc<BoardDraw>>,
    board: Arc<Board>,
    fills: Arc<Vec<ZoneFill>>,
    t: &Templates,
    live: Arc<LiveIx>,
    index: Option<Arc<ValueIndex>>,
) -> Picture {
    let t0 = Cx::monotonic_now();
    let same_index = |old: &BoardDraw| match (&old.index, &index) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    if let Some(old) = old.filter(|old| same_index(old)) {
        if let Some((mut edited, regions)) = edited_items(&old.board, &old.fills, &board, &fills) {
            edited.extend(live.changed_items(&old.live));
            edited.sort_unstable();
            edited.dedup();
            let mut partial = scene_of_items(&board, &fills, &edited);
            let by_item: BTreeMap<_, _> = regions.iter().map(|r| (r.item, r.region)).collect();
            for fill in fills.iter() {
                if let Some(&region) = by_item.get(&fill.zone) {
                    merge_into(&mut partial, scene_of_fill_region(&board, fill, region));
                }
            }
            let batch = rebatch(&old.batch, &edited, &regions, &partial, t, &live);
            let pubs = publications(&batch, Some((&old.batch, &old.pubs)));
            // Where the drawing may differ: the edited items as the old board drew them (one
            // more scene of the same items, O(their size)), as the new one does, and the regions.
            let before = scene_of_items(&old.board, &old.fills, &edited).bbox;
            let changed = [before, partial.bbox].into_iter().chain(regions.iter().map(|r| r.region))
                .filter(|b| !b.is_empty()).collect();
            return Picture { batch, pubs, geom: None, live, index, wires: None, trees: None,
                ms: (Cx::monotonic_now() - t0) * 1e3, wires_ms: 0.0, trees_ms: 0.0,
                edited: Some(edited.len() + regions.len()), changed: Some(changed) };
        }
    }
    let geom = geom_of(board, &fills);
    let batch = batch_scene(&geom.scene, t, &live);
    let pubs = publications(&batch, None);
    Picture { batch, pubs, geom: Some(geom), live, index, wires: None, trees: None,
        ms: (Cx::monotonic_now() - t0) * 1e3, wires_ms: 0.0, trees_ms: 0.0, edited: None, changed: None }
}

fn geom_of(board: Arc<Board>, fills: &[ZoneFill]) -> BoardGeom {
    let t0 = Cx::monotonic_now();
    let scene = scene_of(&board, fills);
    let checks = CheckIndex::new(&board, &scene);
    let mut items = ItemIndex::new(&board, &scene);
    let names = NameIndex::new(&board, &items);
    let pick = ScenePick::new(&scene, items.take_bodies());
    BoardGeom {
        board, scene: Arc::new(scene), items: Arc::new(items), names: Arc::new(names), pick: Arc::new(pick),
        checks: Arc::new(checks), ms: (Cx::monotonic_now() - t0) * 1e3,
    }
}

fn merge_into(scene: &mut DrawScene, other: DrawScene) {
    scene.bbox = scene.bbox.union(other.bbox);
    for (layer, mut source) in other.layers {
        match scene.layers.binary_search_by_key(&layer, |(id, _)| *id) {
            Ok(i) => {
                scene.layers[i].1.shapes.append(&mut source.shapes);
                scene.layers[i].1.polys.append(&mut source.polys);
            }
            Err(i) => scene.layers.insert(i, (layer, source)),
        }
    }
    scene.drills.extend(other.drills);
}

/// Grow changed fill tiles to whole batch tiles; count old polygons without cloning geometry.
pub(crate) fn fill_region_edit(before: &ZoneFill, after: &ZoneFill) -> Option<RegionEdit> {
    let changed = zone::changed_box(before, after);
    if changed.is_empty() {
        return None;
    }
    let span = TileId::span(BATCH_LOD);
    let first = |n: Nm| (n as i64).div_euclid(span) as i32;
    let last = |n: Nm| (n as i64 - 1).div_euclid(span) as i32;
    let region = TileId { lod: BATCH_LOD, x: first(changed.min.x), y: first(changed.min.y) }.rect_nm()
        .union(TileId { lod: BATCH_LOD, x: last(changed.max.x), y: last(changed.max.y) }.rect_nm());
    let old_polys = before.layers.iter().filter_map(|layer| {
        let count = layer.kept().filter(|(poly_ref, poly)| {
            let b = before.lattice.tile_box(poly_ref.tile);
            b.min.x < region.max.x && region.min.x < b.max.x
                && b.min.y < region.max.y && region.min.y < b.max.y && !poly.is_empty()
        }).count();
        (count != 0).then_some((layer.layer, count))
    }).collect();
    Some(RegionEdit { item: after.zone, region, old_polys })
}

/// Whole-item and regional fill edits, sorted by item. Global changes require a full build.
fn edited_items(old: &Board, old_fills: &Arc<Vec<ZoneFill>>, new: &Board, new_fills: &Arc<Vec<ZoneFill>>) -> Option<(Vec<ItemId>, Vec<RegionEdit>)> {
    let delta = BoardDelta::between(old, new);
    // Cutouts also change the substrate mesh, which belongs to the whole board.
    if delta.global || old.cutouts != new.cutouts {
        return None;
    }
    let mut edited: Vec<ItemId> = delta.added.into_iter().chain(delta.removed).chain(delta.changed).collect();
    edited.sort_unstable();
    edited.dedup();
    let board_edits = edited.len();
    let mut regions = Vec::new();
    if !Arc::ptr_eq(old_fills, new_fills) {
        let before: BTreeMap<ItemId, &ZoneFill> = old_fills.iter().map(|f| (f.zone, f)).collect();
        let after: BTreeMap<ItemId, &ZoneFill> = new_fills.iter().map(|f| (f.zone, f)).collect();
        // `same` compares tiles by content hash: O(tiles), no geometry.
        for (zone, fill) in &after {
            if edited[..board_edits].binary_search(zone).is_ok() {
                continue;
            }
            if let Some(before) = before.get(zone) {
                if before.same(fill) { continue; }
                if before.lattice == fill.lattice {
                    regions.extend(fill_region_edit(before, fill));
                    continue;
                }
            }
            edited.push(*zone);
        }
        edited.extend(before.keys().filter(|z| !after.contains_key(z)));
    }
    edited.sort_unstable();
    edited.dedup();
    Some((edited, regions))
}

/// The batch's streams as publications; a stream whose rows are the previous batch's (the same
/// tile map) keeps the previous publication, so its draw lists stay recorded and nothing uploads.
fn publications(batch: &BoardBatch, old: Option<(&BoardBatch, &BoardPubs)>) -> BoardPubs {
    let reuse = |kind: &KindBatch, before: Option<(&KindBatch, &Publication)>| -> Publication {
        if let Some((k, p)) = before {
            if k.tiles.same(&kind.tiles) {
                return p.clone();
            }
        }
        publish(kind)
    };
    let old_layer = |layer: LayerId| {
        old.and_then(|(b, p)| Some((b.layers.iter().find(|l| l.layer == layer)?, p.layers.iter().find(|l| l.layer == layer)?)))
    };
    let layers = batch.layers.iter().map(|lb| {
        let before = old_layer(lb.layer);
        LayerPubs {
            layer: lb.layer,
            tris: reuse(&lb.tris, before.map(|(b, p)| (&b.tris, &p.tris))),
            strokes: reuse(&lb.strokes, before.map(|(b, p)| (&b.strokes, &p.strokes))),
            pads: reuse(&lb.pads, before.map(|(b, p)| (&b.pads, &p.pads))),
        }
    }).collect();
    BoardPubs {
        glows: None,
        layers,
        substrate: reuse(&batch.substrate, old.map(|(b, p)| (&b.substrate, &p.substrate))),
        holes: reuse(&batch.holes, old.map(|(b, p)| (&b.holes, &p.holes))),
    }
}

fn publish(kind: &KindBatch) -> Publication {
    if kind.is_empty() {
        return None;
    }
    match kind.publication() {
        Ok(p) => Some(p),
        Err(e) => {
            log!("fab.board: a stream of {} instances is not drawn: {e}", kind.count);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{tests::snapshot, StateOf, Tagged};
    use super::super::inst::{StrokeTemplate, TriTemplate};

    #[test]
    fn proposal_slot_retires_independently_of_the_live_board() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut design = fab_elec_fixtures::ledctl();
        let unit = design.units[0].id;
        let board = Arc::new(Board::new(unit, "power"));
        design.units[0].board = Some(board.clone());
        let fills: Arc<Vec<ZoneFill>> = Arc::default();
        let mut snap = snapshot(Arc::new(design));
        snap.fills.push(Tagged { unit: Some(unit), of: StateOf::Committed, generation: 0, value: fills.clone() });
        let templates = Templates { stroke: StrokeTemplate::packed(), pad: PadTemplate::packed(), tri: TriTemplate::packed() };
        let finish = |cx: &mut Cx, source, b: &Arc<Board>, f: &Arc<Vec<ZoneFill>>| {
            BoardDraws::set_source(cx, source, b.clone(), f.clone());
            let me = cx.get_global::<BoardDraws>();
            let slot = BoardDraws::slot(&mut me.slots, DrawKey { source, unit });
            let want = slot.want.take().unwrap();
            let pic = picture(None, b.clone(), f.clone(), &templates, Arc::new(LiveIx::none()), None);
            let draw = slot.finish_picture(&want, pic);
            me.retired.extend(slot.publish(draw.clone(), &mut me.changed));
            draw
        };
        let old_board = Arc::new((*board).clone());
        let old = finish(&mut cx, BoardSource::Live, &old_board, &fills);
        BoardDraws::poll(&mut cx);
        BoardDraws::set_board(&mut cx, board.clone(), fills.clone());
        let id = ProposalId(4);
        let source = BoardSource::Proposal(id);
        for (b, f) in [(&old_board, &fills), (&board, &Arc::default())] {
            let different = finish(&mut cx, source, b, f);
            BoardDraws::keep_preview(&mut cx, Some(id), &snap);
            assert_eq!(cx.global::<BoardDraws>().slots.len(), 2, "both pictures coexist while the comparison is selected");
            BoardDraws::keep_preview(&mut cx, None, &snap);
            let me = cx.global::<BoardDraws>();
            let [live] = me.slots.as_slice() else { panic!("ending the comparison removes its slot") };
            let want = live.want.as_ref().unwrap();
            assert!(Arc::ptr_eq(live.current.as_ref().unwrap(), &old) && Arc::ptr_eq(&want.board, &board)
                && Arc::ptr_eq(&want.fills, &fills), "a board or fills mismatch leaves the Live picture and its request intact");
            assert!(me.retired.iter().any(|d| Arc::ptr_eq(d, &different)) && me.changed.is_empty(),
                "a mismatched comparison retires without publishing a Live change");
        }
        let ready = finish(&mut cx, source, &board, &fills);
        BoardDraws::set_templates(&mut cx, templates.clone());
        let token = cx.get_global::<BoardDraws>().slots.iter().find(|s| s.key.source == BoardSource::Live)
            .unwrap().job.as_ref().unwrap().0.token();
        BoardDraws::keep_preview(&mut cx, None, &snap);
        assert!(Arc::ptr_eq(&BoardDraws::current(&mut cx, unit).unwrap(), &ready), "Live adopts the finished drawing itself");
        let [live] = cx.get_global::<BoardDraws>().slots.as_slice() else { panic!("adoption leaves only the Live slot") };
        assert!(token.is_cancelled() && live.want.is_none() && live.job.is_none(), "the adopted picture cancels its duplicate Live build");
        assert!(ready.gen == old.gen + 1 && ready.geom_gen == old.geom_gen + 1 && ready.changed.is_none(),
            "adoption is a full Live publication even when the source slots had the same generation");
        assert_eq!(BoardDraws::poll(&mut cx), [unit], "poll reports the adopted unit through the Live completion path");

        // A request from an earlier snapshot is queued when a second comparison ends. Pause
        // submission so the test can inspect the queue and the real index refresh without racing a worker.
        cx.get_global::<BoardDraws>().templates = None;
        let next = finish(&mut cx, source, &board, &fills);
        BoardDraws::set_board(&mut cx, old_board.clone(), fills.clone());
        cx.set_global(ElecLive::new(Arc::new(snap.clone())));
        BoardDraws::keep_preview(&mut cx, None, &snap);
        let live = &cx.get_global::<BoardDraws>().slots[0];
        assert!(Arc::ptr_eq(live.current.as_ref().unwrap(), &next) && live.want.is_none(),
            "a stale Live request is dropped: building it would put the earlier design back over the adopted one");
        BoardDraws::poll(&mut cx);
        let refresh = cx.get_global::<BoardDraws>().slots[0].want.as_ref().unwrap();
        assert!(Arc::ptr_eq(&refresh.board, &board) && Arc::ptr_eq(&refresh.fills, &fills),
            "the Live index refresh requests wires and trees for the adopted picture");

        let last = finish(&mut cx, source, &board, &fills);
        BoardDraws::set_board(&mut cx, old_board.clone(), fills.clone());
        BoardDraws::set_templates(&mut cx, templates.clone());
        let token = cx.get_global::<BoardDraws>().slots.iter().find(|s| s.key.source == BoardSource::Live)
            .unwrap().job.as_ref().unwrap().0.token();
        BoardDraws::keep_preview(&mut cx, None, &snap);
        let live = &cx.get_global::<BoardDraws>().slots[0];
        assert!(Arc::ptr_eq(live.current.as_ref().unwrap(), &last) && token.is_cancelled()
            && live.job.is_none() && live.want.is_none(),
            "a stale Live build in flight is cancelled: finishing it would put the earlier design back over the adopted one");
    }
}
