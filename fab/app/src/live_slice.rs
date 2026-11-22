//! The live slice: what the headset, the desktop's Slice render mode and the Print path get from Fab's
//! slicing engine ([`makepad_fab_slice::live`]). A grab sends each part's transform here
//! every headset frame ([`LiveSlice::set_transform`], coalescing: only the newest one per
//! part is ever worked on) and the views draw the newest progressive result
//! ([`LiveSlice::latest`], never blocking) at their own frame rate. A release asks for the
//! refined result ([`LiveSlice::release`]); the committed edit then reslices the plate.
//!
//! One engine slices for every view: the Print editor's Slice asks it for the exact result
//! ([`settle`]) and the live views share its contours and toolpaths (a plate and settings
//! already sliced live settle without slicing again). A second engine answers the model
//! worker's printability checks, so a check never cancels the user's slice.
//!
//! What streams in (each part kept by content, so a part that only moves costs nothing):
//! - `Contours`: every part's layer contours;
//! - then, released: final model layers (`Refined` with `preview`, `layers_done` of
//!   `layers_total`), followed by support additions, plate checks and the merged plan;
//! - sliced printability layer by layer (`checking`: done, total) stays last.
//! The desktop settles that same input only when Print, Export or Slice asks for it.
//!
//! [`cancel`]: nothing to slice any more (no view wants one, a plate that cannot print): idle, zero CPU.
//!
//! Main-loop cost: `set_transform` stores the transform in the engine's state under a mutex
//! held for the store; `latest` clones an `Arc` under a `try_lock`. Every result is built on
//! the engine's jobs. Every lock here is held for a pointer move or a few field updates, and
//! none is taken while another is held ([`Leaf`], [`Shot`]).

use crate::print::{CheckSetup, PrintEvent};
use fab::print::{issue_markers, Bead, Contours, LiveChunk, PreviewBlock, PreviewLayer, PrintPreview, PREVIEW_SLOTS};
use fab_model::eval::Evaluated;
use fab_plate::Plate;
use makepad_fab_gcode::record::MoveRecord;
use makepad_fab_slice::gcode_text::SliceCache;
use makepad_fab_slice::live_paths::LivePaths;
use makepad_fab_slice::live::{Affine, CutLayers, Estimate, Frame, LiveEngine, LiveEvent, LiveInput, PartField, RecordTaker, Sink, Spawn, TreeShape, Urgency};
use makepad_fab_gcode::record::MoveKind;
use makepad_fab_slicer::geom::{ExPolygon, Pt};
use makepad_fab_toolpath::par::Crew;
use makepad_widgets::makepad_platform::retained_instances::{InstanceSegment, RetainedInstances};
use makepad_widgets::makepad_platform::thread::{checkpoint, without_permit, worker_wanted, Class, TaskPool, TaskScope, Ticket, Version};
use makepad_widgets::*;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Row-major affine in printer mm (Z up): `p' = m * p`.
pub type PartTransform = Affine;

pub const IDENTITY: PartTransform = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

/// How far a result has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LiveStage {
    /// Contours plus the moved plate's printability, during the grab.
    Issues,
    /// Released: the toolpaths (`preview`, as far as they are) and the printability.
    Refined,
}

/// One progressive result.
pub struct LiveResult {
    /// The engine's input revision; a result of an older one is never newer.
    pub revision: u64,
    pub stage: LiveStage,
    /// One bead per contour edge, [`PREVIEW_SLOTS`] floats each, on its layer: per
    /// part and layer, shared with the slots (and their GPU buffers) while unchanged.
    pub contours: Contours,
    pub layer_height: f32,
    /// The plate's report so far and its markers.
    pub report: Option<Arc<fab_printability::Report>>,
    pub markers: Option<RetainedInstances>,
    /// `Refined`: the toolpaths as print preview beads (draw with `draw_print_placed`).
    pub preview: Option<Arc<PrintPreview>>,
    /// Toolpath layers complete, of all (the preview's progress).
    pub layers_done: u32,
    pub layers_total: u32,
    /// All parts' layers and supports have reached the assembled plate.
    pub complete: bool,
    /// Printability on the toolpaths: layers checked, of all (`done == total`: checked).
    #[allow(dead_code)] // FABUI's "checking… N/total"
    pub checking: (u32, u32),
}

impl std::fmt::Debug for LiveResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveResult").field("revision", &self.revision).field("stage", &self.stage).field("cuts", &self.contours.all.count()).field("preview", &self.preview.is_some()).field("layers", &(self.layers_done, self.layers_total)).finish()
    }
}

/// What a settle for the Print editor needs to turn the slice into its result.
pub struct PrintJob {
    pub gen: u64,
    pub settings: fab::print::PrintSettings,
    /// The colour of each job tool as Sliced and Print draw it (`print::tool_colours`).
    pub colours: Vec<[f32; 4]>,
    pub machine: fab_plate::machine::Machine,
    pub name: String,
    /// The plate's printed objects, the beads' part indices.
    pub objects: Arc<[String]>,
    /// `Some(id)`: the model worker's printability check (answered as `Checked`).
    pub check: Option<u64>,
    /// The check's report when the plate cannot be sliced.
    pub fallback: Option<Arc<dyn Fn() -> Arc<fab_printability::Report> + Send + Sync>>,
    /// The installed profiles when the job slices with them (the safety check reads the
    /// printer's system preset and templates from them); None: Fab defaults.
    pub catalog: Option<Arc<crate::print::Catalog>>,
    /// A test plate's input: its pieces say whose each of the slice's moves are.
    pub tests: Option<makepad_fab_slice::live::MakeInput>,
    /// The plate's heat-set inserts to melt in after the print ([`crate::print::melt_steps`]),
    /// each with the job tool (0-based) its part prints in: the iron takes that filament's print
    /// temperature.
    pub melts: Vec<(fab::print::PreviewMelt, usize)>,
}

/// A part slot as the views know it: its content's contours and planned layers.
#[derive(Default)]
struct Slot {
    key: u64,
    cut: Option<Arc<CutLayers>>,
    /// Layers of `cut` that are the previous content's until they are traced (end exclusive).
    stale: Vec<(u32, u32)>,
    /// Its plan's region streams (0: not known yet) and layers.
    regions: u32,
    total: u32,
    /// (layer index, region) → its plan's paths, complete: their records (times from 0) are
    /// made only as a tile is built from them.
    layers: BTreeMap<(u32, u32), Arc<LivePaths>>,
    /// Who planned each of `layers`.
    by: BTreeMap<(u32, u32), PlannedBy>,
    /// The newest engine revision whose plan this slot (its content) has.
    revision: u64,
    /// Support islands by model layer, with support-only indices after `total`.
    supports: BTreeMap<u32, Arc<LivePaths>>,
    supports_done: bool,
    /// The trees of its supports' plan, in its frame (none while they are planned anew), and
    /// which of the trees the engine sent they are ([`TREES_SENT`]; 0: none).
    trees: (u64, Arc<[TreeShape]>),
    /// The last complete content, its beads shown where neither this content nor a `newer` ghost
    /// has a layer yet (it goes once this content is complete).
    good: Option<Box<Slot>>,
    /// The superseded incomplete contents with a layer, newest first, each holding only the
    /// layers no newer one has: their beads are the last drawn of those layers, so they are shown
    /// where this content has no layer yet (they go with `good`). Keeping only the newest one
    /// showed a layer an older content's plan again once two such contents were superseded.
    newer: Vec<Slot>,
    /// Eight-layer bead blocks (their layers' beads by reference) and their runs, kept while
    /// their layer identities stay the same.
    tiles: BTreeMap<u32, (u64, RetainedInstances, Arc<[PreviewBlock]>)>,
    /// Each shown layer's toolpath beads, the identity they were built for and the identity of
    /// the layers either side their walls' steps were taken from, and its runs
    /// ([`PreviewBlock::of_layer`]): a block whose identity changed builds only its changed
    /// layers, and a layer whose neighbours changed only takes new steps.
    layer_tiles: BTreeMap<u32, (u64, u64, RetainedInstances, Arc<[PreviewBlock]>)>,
    /// The contour beads of each layer of `cut` at `beads_at` (None: not built, a layer still
    /// to trace): a cut of the same content keeps them, only its newly traced layers are built.
    beads: Vec<Option<RetainedInstances>>,
    beads_at: Option<Pt>,
    /// The contours this slot shows (its stale layers left out) and their top layer, until
    /// its cut, stale layers or place change.
    contours: Option<(Arc<[Option<RetainedInstances>]>, u32)>,
}

impl Slot {
    /// Keep the last complete content across any number of partial revisions.
    fn renew(&mut self, key: u64) {
        if self.key != key {
            let mut was = std::mem::take(self);
            let revision = was.revision;
            was.beads = Vec::new();
            was.contours = None;
            let tiles = std::mem::take(&mut was.tiles);
            let layer_tiles = std::mem::take(&mut was.layer_tiles);
            // A content shown again while a ghost of it is still kept: those layers are its plan.
            let back = if was.good.as_ref().is_some_and(|g| g.key == key) {
                was.good.take().map(|g| *g)
            } else {
                was.newer.iter().position(|g| g.key == key).map(|k| was.newer.remove(k))
            };
            let (good, newer) = if was.complete() {
                was.good = None;
                was.newer.clear();
                (Some(Box::new(was)), Vec::new())
            } else {
                let good = was.good.take();
                let mut newer = std::mem::take(&mut was.newer);
                if was.top() > 0 {
                    Slot::supersede(&mut newer, was);
                }
                (good, newer)
            };
            *self = match back {
                Some(back) => Slot { revision, good, newer, tiles, layer_tiles, ..back },
                None => Slot { key, revision, good, newer, tiles, layer_tiles, ..Default::default() },
            };
        }
    }

    /// `ghost`, the newest superseded content, goes first in `newer`: the older ghosts keep only
    /// the layers it has not, and one left with none goes.
    fn supersede(newer: &mut Vec<Slot>, ghost: Slot) {
        for old in newer.iter_mut() {
            old.layers.retain(|&(i, _), _| !ghost.has(i));
            old.by.retain(|&(i, _), _| !ghost.has(i));
        }
        newer.retain(|g| g.top() > 0);
        newer.insert(0, ghost);
    }

    /// The contours of `cut` at offset `d` (the slot's part `part`): only layers not built for
    /// this content and place yet are built, the stale ones are left out.
    fn build_contours(&mut self, part: usize, d: Pt) -> usize {
        let cut = self.cut.clone().expect("a slot with contours has a cut");
        if self.beads_at != Some(d) || self.beads.len() != cut.len() {
            self.beads = vec![None; cut.len()];
            self.beads_at = Some(d);
        }
        let (mut top, mut built) = (0u32, 0usize);
        let mut shown = Vec::with_capacity(cut.len());
        for (li, (z, h, layer)) in cut.iter().enumerate() {
            let regions = &layer.regions;
            // A layer still to trace has no contours of the edited part yet.
            if self.stale.iter().any(|&(a, b)| (a..b).contains(&(li as u32))) {
                shown.push(None);
                continue;
            }
            if !regions.is_empty() {
                top = top.max(li as u32);
            }
            let beads = self.beads[li].get_or_insert_with(|| {
                built += 1;
                contour_layer_beads(part, li as u32, *z, *h, regions, d)
            });
            shown.push(Some(beads.clone()));
        }
        self.contours = Some((shown.into(), top));
        built
    }

    /// One past this content's highest complete layer (0: none).
    fn top(&self) -> u32 {
        self.layers.keys().rev().map(|&(i, _)| i).find(|&i| self.has(i)).map_or(0, |i| i + 1)
    }

    /// Every model region and the support pass are complete.
    fn complete(&self) -> bool {
        self.supports_done && self.regions > 0 && (0..self.total).all(|i| self.has(i))
    }

    /// A model layer is complete in every region; a support-only layer stands alone.
    fn has(&self, i: u32) -> bool {
        if i >= self.total { return self.supports.contains_key(&i); }
        self.regions > 0 && (0..self.regions).all(|r| self.layers.contains_key(&(i, r)))
    }

    /// The layer count shown: the plan's, else the last complete content's and the newer
    /// incomplete contents' layers above it.
    fn shown_total(&self) -> u32 {
        let own = self.supports.last_key_value().map_or(self.total, |(&i, _)| self.total.max(i + 1));
        if self.regions != 0 {
            return own;
        }
        let newer = self.newer.iter().map(Slot::top).max().unwrap_or(0);
        self.good.as_ref().map_or(own, |o| o.shown_total()).max(newer)
    }

    /// This content's layer when all regions have it, else the one drawn last before it: the
    /// newest superseded incomplete content's that has it, else the last complete content's.
    fn shown(&self, i: u32) -> Option<&Slot> {
        if self.has(i) {
            return Some(self);
        }
        self.newer.iter().find(|o| o.has(i)).or_else(|| self.good.as_deref().filter(|o| o.has(i)))
    }

    /// The layers of this content's stack: its cut's and those shown above them.
    fn stack_len(&self) -> u32 {
        self.shown_total().max(self.cut.as_ref().map_or(0, |c| c.len() as u32))
    }

    /// The print z of layer `i` of this content's stack: its cut's (traced or not), else the
    /// shown layer's (a support-only layer, a ghost above the cut).
    fn stack_z(&self, i: u32) -> Option<f32> {
        self.cut.as_ref().and_then(|c| c.get(i as usize)).map(|c| c.0 as f32).or_else(|| self.layer_z(i))
    }

    /// The print z of shown layer `i` (its records', else its cut's).
    fn layer_z(&self, i: u32) -> Option<f32> {
        let slot = self.shown(i)?;
        let paths = (0..slot.regions).filter_map(|r| slot.layers.get(&(i, r))).chain(slot.supports.get(&i));
        paths.filter_map(|p| p.z()).next().or_else(|| slot.cut.as_ref().and_then(|c| c.get(i as usize)).map(|c| c.0 as f32))
    }

    /// The plate's `blocks` eight-layer blocks of this part: a block whose shown layers or plate
    /// layer numbers changed is joined anew from its layers' beads, building only the layers
    /// that changed; the others keep theirs (and their GPU buffers). A block with no layer shown
    /// is empty but keeps its place, so the blocks after it keep theirs. A tile's beads are in
    /// the part's own frame (the preview's `part_offsets` place them, so a part that only moves
    /// rebuilds nothing) and carry the plate's layer number (by print z, as the settled slice
    /// numbers them), so the slider cuts every part at the same height.
    fn rebuild_tiles(&mut self, machine: &fab_plate::machine::Machine, part: usize, plate_layer: &dyn Fn(f32) -> u32, blocks: u32, built: &mut usize) -> Vec<ShownTile> {
        let total = self.shown_total();
        self.tiles.retain(|&block, _| block < blocks);
        let mut shown = Vec::new();
        let mut lines = BTreeMap::new();
        for block in 0..blocks {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            let first = shown.len();
            for i in block * TILE_BLOCK..((block + 1) * TILE_BLOCK).min(total) {
                let Some(tile) = self.tile_at(part, i, plate_layer) else { continue };
                (i, tile.hash, tile.stack).hash(&mut hash);
                shown.push(tile);
            }
            let hash = hash.finish();
            if self.tiles.get(&block).is_some_and(|t| t.0 == hash) {
                continue;
            }
            let (mut segments, mut runs, mut at) = (Vec::new(), Vec::new(), 0);
            for tile in &shown[first..] {
                let (beads, layer_runs) = match self.layer_tiles.get(&tile.layer).filter(|t| t.0 == tile.hash) {
                    Some((_, stack, beads, layer_runs)) if *stack == tile.stack => (beads.clone(), layer_runs.clone()),
                    // Only the layers either side changed: the same beads (and runs) take their
                    // new steps.
                    Some((_, _, beads, layer_runs)) => {
                        let mut data = beads.data().to_vec();
                        let layer_runs = layer_runs.clone();
                        self.stack_tile(&mut data, tile.layer, &mut lines);
                        let beads = RetainedInstances::new(PREVIEW_SLOTS, data.into()).expect("whole layer instances");
                        self.layer_tiles.insert(tile.layer, (tile.hash, tile.stack, beads.clone(), layer_runs.clone()));
                        (beads, layer_runs)
                    }
                    None => {
                        let (beads, layer_runs) = self.tile_beads(machine, tile, &mut lines);
                        self.layer_tiles.insert(tile.layer, (tile.hash, tile.stack, beads.clone(), layer_runs.clone()));
                        *built += 1;
                        (beads, layer_runs)
                    }
                };
                runs.extend(layer_runs.iter().map(|b| PreviewBlock { first: b.first + at, end: b.end + at, ..*b }));
                at += beads.count() as u32;
                segments.extend(beads.segments().iter().cloned());
            }
            self.tiles.insert(block, (hash, RetainedInstances::from_segments(PREVIEW_SLOTS, segments).expect("whole tile instances"), runs.into()));
        }
        // Beads of layers no longer shown (or shown otherwise) go.
        self.layer_tiles.retain(|i, (id, _, _, _)| shown.binary_search_by_key(i, |t| t.layer).is_ok_and(|k| shown[k].hash == *id));
        shown
    }

    /// Layer `i` as it shows now (none: no content has it yet): which content shows it, its
    /// identity (the part, that content, its paths and its plate layer) and its steps' (the
    /// walls either side).
    fn tile_at(&self, part: usize, i: u32, plate_layer: &dyn Fn(f32) -> u32) -> Option<ShownTile> {
        let slot = self.shown(i)?;
        let ghost = !std::ptr::eq(slot, self);
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        (part, slot.key).hash(&mut identity);
        let z = self.layer_z(i).unwrap_or(0.0);
        for paths in (0..slot.regions).filter_map(|r| slot.layers.get(&(i, r))).chain(slot.supports.get(&i)) {
            paths.identity(&mut identity);
        }
        let global = plate_layer(z);
        global.hash(&mut identity);
        let hash = identity.finish();
        let mut stack = std::collections::hash_map::DefaultHasher::new();
        for n in [i.checked_sub(1), i.checked_add(1)] {
            for paths in n.into_iter().flat_map(|n| self.wall_paths(n)) {
                paths.identity(&mut stack);
            }
            0u8.hash(&mut stack);
        }
        let stack = stack.finish();
        let revision = (0..slot.regions).filter_map(|r| slot.by.get(&(i, r))).map(|b| b.revision).max().unwrap_or(slot.revision);
        Some(ShownTile { part, layer: i, global, hash, stack, z, ghost, revision })
    }

    /// A settled slice's beads of this part (by plate layer, in the part's frame, `beads`) as the
    /// tiles of its layers this content shows, in place of their plan's: one set of beads for
    /// the slice shown, the headset's view while a part is held and the next edit's unchanged
    /// layers (their identity stays, so no tile is built for them). A layer goes on with its
    /// plan's beads where the slice's plate layer of that number is at another height
    /// (`layer_z`, the slice's layers) or where two of the part's layers share it. How many
    /// layers took the slice's beads. `beads`: per part and plate layer, its segments and runs.
    fn adopt(&mut self, part: usize, plate_layer: &dyn Fn(f32) -> u32, beads: &BTreeMap<(usize, u32), (Vec<InstanceSegment>, Arc<[PreviewBlock]>)>, layer_z: &BTreeMap<u32, f32>) -> usize {
        let tiles: Vec<ShownTile> = (0..self.shown_total()).filter_map(|i| self.tile_at(part, i, plate_layer)).filter(|t| !t.ghost).collect();
        let mut per_layer: BTreeMap<u32, usize> = BTreeMap::new();
        for t in &tiles {
            *per_layer.entry(t.global).or_default() += 1;
        }
        let um = |z: f32| (z * 1000.0).round() as i64;
        let (mut adopted, mut elsewhere) = (0, Vec::new());
        for t in tiles {
            let Some((segments, runs)) = beads.get(&(part, t.global)) else { continue };
            if per_layer[&t.global] > 1 || layer_z.get(&t.global).is_none_or(|&z| um(z) != um(t.z)) {
                elsewhere.push(t.layer);
                continue;
            }
            let instances = RetainedInstances::from_segments(PREVIEW_SLOTS, segments.clone()).expect("whole layer instances");
            self.layer_tiles.insert(t.layer, (t.hash, t.stack, instances, runs.clone()));
            // Its block is made again of the layer beads it holds.
            self.tiles.remove(&(t.layer / TILE_BLOCK));
            adopted += 1;
        }
        if !elsewhere.is_empty() {
            log!("fab live tiles: part {part}: {} layers keep their plans' beads, the slice numbers them otherwise (first {})", elsewhere.len(), elsewhere[0]);
        }
        adopted
    }

    /// The beads of one shown layer, in the part's frame, numbered with the plate's layer, and
    /// their runs.
    fn tile_beads(&self, machine: &fab_plate::machine::Machine, tile: &ShownTile, lines: &mut BTreeMap<u32, crate::print::WallLines>) -> (RetainedInstances, Arc<[PreviewBlock]>) {
        let slot = self.shown(tile.layer).expect("shown layer");
        let records = (0..slot.regions).filter_map(|region| slot.layers.get(&(tile.layer, region)))
            .chain(slot.supports.get(&tile.layer)).map(|paths| crate::print::StreamRecords {
                moves: paths.records(),
                shift: [0.0, 0.0],
                object: tile.part as u16,
            }).collect();
        let (mut beads, counts) = crate::print::layer_beads(machine, records);
        for bead in beads.chunks_mut(PREVIEW_SLOTS) {
            fab::print::bead::set_layer(bead, tile.global as f32);
        }
        self.stack_tile(&mut beads, tile.layer, lines);
        let runs = PreviewBlock::of_layer(beads.chunks_exact(PREVIEW_SLOTS), counts);
        (RetainedInstances::new(PREVIEW_SLOTS, beads.into()).expect("whole layer instances"), runs.into())
    }

    /// The toolpaths of shown layer `i` that print its walls (its regions', no supports).
    fn wall_paths(&self, i: u32) -> impl Iterator<Item = &Arc<LivePaths>> {
        self.shown(i).into_iter().flat_map(move |slot| (0..slot.regions).filter_map(move |r| slot.layers.get(&(i, r))))
    }

    /// One shown layer's wall beads take their steps to the walls of the shown layers either
    /// side, as they are now: a layer not shown yet has none, and the step comes when it is.
    /// `lines` holds the walls of the layers this pass gridded: the tiles come up the stack, so
    /// a layer's are gridded once for the tiles below and above it and go past them.
    fn stack_tile(&self, beads: &mut [f32], layer: u32, lines: &mut BTreeMap<u32, crate::print::WallLines>) {
        lines.retain(|&n, _| n + 1 >= layer);
        let (below, above) = (layer.checked_sub(1), layer.checked_add(1));
        for n in [below, above].into_iter().flatten() {
            lines.entry(n).or_insert_with(|| {
                let records: Vec<MoveRecord> = self.wall_paths(n).flat_map(|p| p.records()).collect();
                crate::print::WallLines::of_moves(records.iter())
            });
        }
        let side = |n: Option<u32>| n.and_then(|n| lines.get(&n));
        crate::print::stack_walls(beads.chunks_exact_mut(PREVIEW_SLOTS), side(below), side(above));
    }
}

/// The pass and engine revision that planned a slot layer.
#[derive(Clone, Copy, Debug)]
struct PlannedBy {
    revision: u64,
    pass: &'static str,
}

/// A layer of one content planned twice with different geometry: both plans, as they came.
#[derive(Clone)]
pub(crate) struct Replan {
    pub part: usize,
    pub layer: u32,
    pub region: u32,
    pub key: u64,
    pub before: (u64, &'static str, Arc<LivePaths>),
    pub after: (u64, &'static str, Arc<LivePaths>),
}

/// The last few replans with different geometry (`makepad inspect <fab> eval
/// in=makepad_fab::live_slice 'replans()'` summarises them, `REPLANS` has their records).
pub(crate) static REPLANS: Leaf<std::collections::VecDeque<Replan>> = Leaf::new(std::collections::VecDeque::new());
const REPLANS_KEPT: usize = 8;

/// The geometry of a layer's moves: where material goes, in which role and bead.
fn same_geometry(a: &[MoveRecord], b: &[MoveRecord]) -> bool {
    let laid = |r: &&MoveRecord| r.kind == MoveKind::Extrude;
    a.iter().filter(laid).map(|r| (r.role, r.p0, r.p1, r.width, r.height)).eq(b.iter().filter(laid).map(|r| (r.role, r.p0, r.p1, r.width, r.height)))
}

/// A layer's extrusions per role and their XY bounds (one log field).
fn plan_summary(records: &[MoveRecord]) -> String {
    let mut roles: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for r in records.iter().filter(|r| r.kind == MoveKind::Extrude) {
        *roles.entry(r.role.name()).or_default() += 1;
        for p in [r.p0, r.p1] {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
    }
    let roles: Vec<String> = roles.iter().map(|(n, c)| format!("{n} {c}")).collect();
    format!("{} [{:.2} {:.2} .. {:.2} {:.2}]", roles.join(", "), lo[0], lo[1], hi[0], hi[1])
}

/// Every plan of one content's layer is the same geometry, whichever pass or revision made it:
/// a layer that already has one (this slot's, or a ghost's of the same content) and gets a
/// different one is logged with both, marked in the flight recording and kept in [`REPLANS`].
fn check_replan(slot: &Slot, part: usize, layer: u32, region: u32, paths: &Arc<LivePaths>, by: PlannedBy) {
    let at = (layer, region);
    let before = [Some(slot), slot.good.as_deref()].into_iter().flatten().chain(&slot.newer)
        .filter(|s| s.key == slot.key)
        .find_map(|s| Some((s.layers.get(&at)?.clone(), s.by.get(&at).copied()?)));
    let Some((old, was)) = before else { return };
    // The same paths make the same records: only other paths need theirs compared.
    if old.same_paths(paths) {
        return;
    }
    let (old_records, records) = (old.records(), paths.records());
    if same_geometry(&old_records, &records) {
        return;
    }
    let text = format!("layer {layer} of part {part} replanned at revision {} with different geometry ({} r{} -> {} r{})", by.revision, was.pass, was.revision, by.pass, by.revision);
    log!("fab live: {text}: before {} | after {}", plan_summary(&old_records), plan_summary(&records));
    makepad_error_log::flight::mark_ab("fab.replan", &text, part as u64, layer as u64);
    let mut kept = REPLANS.lock();
    if kept.len() == REPLANS_KEPT {
        kept.pop_front();
    }
    kept.push_back(Replan { part, layer, region, key: slot.key, before: (was.revision, was.pass, old), after: (by.revision, by.pass, paths.clone()) });
}

/// The kept replans, one line each.
#[allow(dead_code)] // called by `makepad inspect .. eval`
pub(crate) fn replans() -> Vec<String> {
    let kept: Vec<Replan> = REPLANS.lock().iter().cloned().collect();
    kept.iter().map(|r| format!("part {} layer {} region {} key {:x}: {} r{} {} | {} r{} {}", r.part, r.layer, r.region, r.key,
        r.before.1, r.before.0, plan_summary(&r.before.2.records()), r.after.1, r.after.0, plan_summary(&r.after.2.records()))).collect()
}

/// Layers per tile block: a block is one draw (and one GPU buffer) of the live preview.
const TILE_BLOCK: u32 = 8;

struct ShownTile {
    part: usize,
    /// The part's own layer index, and the plate's layer number (by print z).
    layer: u32,
    global: u32,
    hash: u64,
    /// The identity of the layers either side, whose walls' steps the layer's beads take.
    stack: u64,
    z: f32,
    /// An older content's beads, until this content draws the layer.
    ghost: bool,
    /// The newest revision that planned the layer (its slot's, for a layer with no plan of its own).
    revision: u64,
}

/// The live state: what the engine's events built, as the views get it. Only its owner works on
/// it ([`LiveOwner`]), with no lock held.
#[derive(Default)]
struct LiveState {
    slots: Vec<Slot>,
    /// (content key, offset) per slot, newest revision.
    placement: Vec<(u64, Pt)>,
    /// The object per slot, the same revision.
    objects: Arc<[String]>,
    revision: u64,
    /// Every slot's contours as last joined (by reference, see [`refresh_contours`]).
    contours: Option<Contours>,
    /// What `contours` shows: (content key or 0, offset) per slot.
    contours_of: Vec<(u64, Pt)>,
    report: Option<(Arc<fab_printability::Report>, RetainedInstances)>,
    checking: (u32, u32),
    preview: Option<Arc<PrintPreview>>,
    /// Layer identities in the last publication.
    shown: BTreeMap<(usize, u32), u64>,
    layers: (u32, u32),
    complete: bool,
    /// What the Sliced view's trees were made of: per slot, which trees and its place.
    trees_of: Vec<(u64, Pt)>,
    /// What the events applied since the last publication ask it to show ([`publish_applied`]).
    unpublished: Unpublished,
    /// The Print editor's newest settled slice whose preview it has: (engine revision, slice
    /// generation). Its printability streams in after it as `Issues` of that revision.
    settled: Option<(u64, u64)>,
    /// The Print editor's settled slice whose preview is still being made: (engine revision, slice
    /// generation, the newest of its printability that came meanwhile). The Print editor takes a
    /// slice's checks only once it has its preview, so they go after it ([`Msg::Sliced`]).
    settling: Option<(u64, u64, Option<(Arc<fab_printability::Report>, u32, u32)>)>,
    /// Insert findings for the settled revision, added to each fresh streamed report once.
    insert_issues: Option<(u64, Vec<fab_printability::Issue>)>,
    /// The Print editor's slice in flight: its generation, printer and
    /// filament colours ([`Msg::PrintJob`]), until it is answered.
    print_job: Option<(u64, fab_plate::machine::Machine, Vec<[f32; 4]>)>,
    /// What the estimate's preview was last told of the plate ([`Hub::plate`]).
    plate: Option<Arc<PlateShot>>,
    /// The publications' tiles, logged per two-second window ([`publish_tiles`]).
    tile_stats: BeadWindow,
    /// The contour joins, logged per two-second window ([`refresh_contours`]).
    bead_stats: BeadWindow,
}

/// What applied events ask the next publication to show.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Unpublished {
    /// The tiles, and with them the joined contours (any event that may have changed the shown
    /// layers, and every contours event).
    tiles: bool,
    /// Any event: the trees ([`post_trees`] posts them when they changed).
    trees: bool,
}

/// What the live state's owner is asked to do ([`LiveOwner`]), handled in the order it was asked.
enum Msg {
    /// One of the engine's events for the live state (a part's cut, regions, layers, supports or
    /// trees, the plate's contours): applied, and shown by the next publication that is due.
    Part(LiveEvent),
    /// The slice's checks of `revision` so far.
    Issues { revision: u64, report: Arc<fab_printability::Report>, done: u32, total: u32 },
    /// The plate's toolpaths of `revision` are whole: where each part is.
    Toolpaths { revision: u64, parts: Vec<(u64, Pt)> },
    /// The slice of `revision` failed.
    Failed { revision: u64, error: String },
    /// The Print editor asked for the slice `gen` on `machine` with these filament colours: the live
    /// tiles stream to it until it is answered.
    PrintJob { gen: u64, machine: fab_plate::machine::Machine, colours: Vec<[f32; 4]> },
    /// The Print editor's slice settled ([`settle`]).
    Settled { revision: u64, job: PrintJob, result: Result<Arc<makepad_fab_slice::live::Settled>, String>, seconds: f64 },
    /// Its preview is made ([`Msg::Settled`]), and its beads for the live tiles.
    Sliced { revision: u64, gen: u64, sliced: Result<crate::print::Sliced, String>, beads: Option<SliceBeads>, insert_issues: Vec<fab_printability::Issue>, seconds: f64 },
    /// The beads of the estimate's preview of `revision`, for the live tiles.
    Adopt { revision: u64, beads: SliceBeads },
    /// A view started: what the state holds is published.
    StartView,
    /// The preview goes, unless a view or a Print editor slice wants it.
    Release,
    /// A view took the newest publication: what is held goes out.
    Took,
    /// A test's panic in the owner.
    #[cfg(test)]
    Panic,
}

impl Msg {
    /// The owner handles it as soon as it is free. The others (part events, and a view's take)
    /// wait for a publication to be due: they go out together in it.
    fn now(&self) -> bool {
        !matches!(self, Msg::Part(_) | Msg::Took)
    }
}

/// The live state's one owner. The state is parked in the inbox while nobody works on it: whoever
/// takes it out is the owner (an engine worker that posted a message, or a pool job a UI post
/// started), works on it with no lock held, and parks it again. Messages are handled strictly in
/// the order they were posted. A shown slice publishes at the pace its views take the publications
/// ([`LiveSlice::latest`]): the part events that come meanwhile wait in the inbox and go out
/// together in the next. Before it, each part event took the live state's lock and published the
/// Sliced view's tiles (about 2 ms each, 4700 an edit of a figure), and 13 planners slept on that
/// lock: 90 s of their time an edit, the lock held 9 of the edit's 10 s. Then it was a lock that
/// anyone could take to read the state, and a reader that waited on it while holding another was
/// how the UI hung (#3610, #3620): nothing takes the state now but its owner.
struct LiveOwner {
    inbox: Mutex<Inbox>,
    /// A view took the newest publication, or none waits for one: the next may go.
    taken: AtomicBool,
    /// Events waiting or applied and not published yet: the view that takes the newest
    /// publication asks for the next.
    held: AtomicBool,
}

struct Inbox {
    msgs: Vec<Msg>,
    /// The live state while no owner works on it.
    state: Option<Box<LiveState>>,
    /// An owner job is on the pool and has not begun ([`LiveOwner::post`]).
    submitted: bool,
}

/// What an owner does: the app's handlers, or a test's.
struct Work<'a> {
    handle: &'a (dyn Fn(&mut LiveState, Msg) + Sync),
    publish: &'a (dyn Fn(&mut LiveState) + Sync),
    /// The state was lost to a panic (why): its slice fails, so the next sends every part again.
    lost: &'a (dyn Fn(String) + Sync),
}

/// The app's owner work.
static WORK: Work<'static> = Work { handle: &handle, publish: &publish_applied, lost: &state_lost };

impl LiveOwner {
    fn new() -> Self {
        LiveOwner {
            inbox: Mutex::new(Inbox { msgs: Vec::new(), state: Some(Box::default()), submitted: false }),
            taken: AtomicBool::new(true),
            held: AtomicBool::new(false),
        }
    }

    /// `msg` posted. When it asks for the owner now or a publication is due (`paced`: only once a
    /// view took the newest), and the state is parked, this thread takes it and is the owner until
    /// nothing more is due; otherwise it returns at once. For the engine's workers.
    fn post_run(&self, msg: Msg, paced: bool, work: &Work) {
        holds_no_lock("posts to the live state's owner");
        let claimed = {
            let _depth = shot_depth::enter();
            let mut inbox = self.inbox.lock().unwrap();
            let due = self.due(&msg, paced);
            inbox.msgs.push(msg);
            match inbox.state.take() {
                Some(state) if due => Some((state, std::mem::take(&mut inbox.msgs))),
                state => {
                    inbox.state = state;
                    None
                }
            }
        };
        if let Some((state, msgs)) = claimed {
            self.run(state, msgs, paced, work);
        }
    }

    /// `msg` posted, from a thread that never works on the state (the UI): when it asks for the
    /// owner now or a publication is due and nobody owns the state, an owner job goes on the pool.
    fn post(&'static self, msg: Msg) {
        let submit = {
            let _depth = shot_depth::enter();
            let mut inbox = self.inbox.lock().unwrap();
            let due = self.due(&msg, live_views());
            inbox.msgs.push(msg);
            let submit = due && inbox.state.is_some() && !inbox.submitted;
            inbox.submitted |= submit;
            submit
        };
        if submit {
            let submitted = Submitted(Some(self));
            run_slice_work(Class::Visible, "fab live owner", move || {
                if let Some((state, msgs)) = submitted.begin() {
                    self.run(state, msgs, live_views(), &WORK);
                }
            });
        }
    }

    /// Whether `msg` has the owner run: it asks for it now, or a publication is due. A message
    /// that waits holds a publication for the view's next take.
    fn due(&self, msg: &Msg, paced: bool) -> bool {
        let due = msg.now() || !paced || self.taken.load(Ordering::Acquire);
        if !due {
            self.held.store(true, Ordering::Release);
        }
        due
    }

    /// The owner's loop: `msgs` handled in order, then what they applied published when a
    /// publication is due; again while more came that are due. It ends with the state parked and
    /// what waits for a publication held for the next.
    fn run(&self, mut state: Box<LiveState>, mut msgs: Vec<Msg>, paced: bool, work: &Work) {
        holds_no_lock("owns the live state");
        loop {
            for msg in msgs.drain(..) {
                guarded(&mut state, work, |l| (work.handle)(l, msg));
            }
            if state.unpublished != Unpublished::default() {
                if !paced || self.taken.load(Ordering::Acquire) {
                    self.held.store(false, Ordering::Release);
                    guarded(&mut state, work, work.publish);
                } else {
                    self.held.store(true, Ordering::Release);
                }
            }
            let _depth = shot_depth::enter();
            let mut inbox = self.inbox.lock().unwrap();
            let due = !paced || self.taken.load(Ordering::Acquire);
            if inbox.msgs.iter().any(Msg::now) || (due && !inbox.msgs.is_empty()) {
                msgs = std::mem::take(&mut inbox.msgs);
                continue;
            }
            if !inbox.msgs.is_empty() {
                self.held.store(true, Ordering::Release);
            }
            inbox.state = Some(state);
            return;
        }
    }

    /// A view took the newest publication: true when one is held for it (its caller posts
    /// [`Msg::Took`]).
    fn took(&self) -> bool {
        !self.taken.swap(true, Ordering::AcqRel) && self.held.load(Ordering::Acquire)
    }
}

/// `f` on the state. A panic in it loses the state: a fresh one takes its place and its slice
/// fails ([`Work::lost`]), so the engine's next sends every part again.
fn guarded(state: &mut Box<LiveState>, work: &Work, f: impl FnOnce(&mut LiveState)) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(state)));
    if let Err(p) = r {
        let why = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "a panic".into());
        log!("fab live: the live state was lost to a panic ({why}): its slice fails, and the next sends every part again");
        *state = Box::default();
        (work.lost)(why);
    }
}

/// The thread that posts to the live state's owner (and may become it) or owns it holds no lock: the
/// owner's work takes the app's leaf locks, and a lock held around it would order those after it.
/// Under the JIT, which counts each thread's held locks, this checks that none is: a test fails,
/// a run says so once.
fn holds_no_lock(what: &str) {
    #[cfg(relax)]
    {
        let (held, at) = std::relax::held_lock();
        if held == 0 {
            return;
        }
        let at = at.map_or(String::new(), |l| format!(", the newest taken at {}:{}", l.file(), l.line()));
        #[cfg(test)]
        panic!("a thread {what} holding {held} lock(s){at}");
        #[cfg(not(test))]
        {
            static SAID: AtomicBool = AtomicBool::new(false);
            if !SAID.swap(true, Ordering::Relaxed) {
                log!("fab live: a thread {what} holding {held} lock(s){at}");
            }
        }
    }
    #[cfg(not(relax))]
    let _ = what;
}

/// An owner job submitted and not begun: it lets the next be submitted when it begins, or when
/// the pool drops it unrun.
struct Submitted(Option<&'static LiveOwner>);

impl Submitted {
    /// The job begins: the state and the messages, unless an owner has them.
    fn begin(mut self) -> Option<(Box<LiveState>, Vec<Msg>)> {
        let owner = self.0.take()?;
        let _depth = shot_depth::enter();
        let mut inbox = owner.inbox.lock().unwrap();
        inbox.submitted = false;
        let state = inbox.state.take()?;
        Some((state, std::mem::take(&mut inbox.msgs)))
    }
}

impl Drop for Submitted {
    fn drop(&mut self) {
        if let Some(owner) = self.0 {
            let _depth = shot_depth::enter();
            owner.inbox.lock().unwrap().submitted = false;
        }
    }
}

/// The state was lost to a panic in its owner: the engine fails its newest revision as it does
/// for a panic of its own, and its next slice sends every part again.
fn state_lost(why: String) {
    hub().engine.fail(format!("the live view's state was lost: {why}"));
}

/// A queued event applied: [`apply`], and what it asks the next publication to show.
fn apply_queued(l: &mut LiveState, e: LiveEvent) {
    let contours = matches!(e, LiveEvent::Contours { .. });
    let shown = apply(l, e);
    l.unpublished.tiles |= shown || contours;
    l.unpublished.trees = true;
}

/// What the applied events ask for, published: the tiles, in one publication with the joined
/// contours, and the trees. Contours never go out ahead of their tiles: the toolpaths' events
/// complete the slice, and a view that took its contours alone would show it whole with only
/// the layers it had.
fn publish_applied(l: &mut LiveState) {
    #[cfg(test)]
    gates::reach("publication");
    let t = std::time::Instant::now();
    let u = std::mem::take(&mut l.unpublished);
    if u.tiles {
        publish_tiles(l);
    }
    post_trees(l);
    if u != Unpublished::default() {
        SINK[3].fetch_add(1, Ordering::Relaxed);
        SINK[2].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// `msg` to the live state's owner from an engine worker, which is the owner when it is free and
/// the message is due ([`LiveOwner::post_run`]).
fn post_live(msg: Msg) {
    let part = matches!(msg, Msg::Part(_));
    let t = std::time::Instant::now();
    hub().owner.post_run(msg, live_views(), &WORK);
    if part {
        SINK[0].fetch_add(1, Ordering::Relaxed);
        SINK[1].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// A view took the newest publication ([`LiveSlice::latest`]): one held for it goes out.
fn took_publication() {
    let owner = &hub().owner;
    if owner.took() {
        owner.post(Msg::Took);
    }
}

/// What the estimate's preview reads of the plate ([`records_taker`]): the owner shares it
/// whenever the revision, the parts' places or their objects change.
struct PlateShot {
    revision: u64,
    /// Where each part's frame sits on the plate (mm).
    offsets: Arc<[[f32; 2]]>,
    objects: Arc<[String]>,
}

/// The evaluated plate behind a live input, with its insert indices (no mesh copies).
/// Replacing it forgets the old plate; an already settling worker keeps its own reference.
struct InsertPlate {
    id: u64,
    evaluated: Arc<Evaluated>,
    inserts: Vec<usize>,
}

/// The plate as the state has it, shared when it changed ([`PlateShot`]).
fn share_plate(l: &mut LiveState) {
    let offsets = offsets(l);
    let same = l.plate.as_ref().is_some_and(|p| p.revision == l.revision && p.offsets == offsets && Arc::ptr_eq(&p.objects, &l.objects));
    if same {
        return;
    }
    let plate = Arc::new(PlateShot { revision: l.revision, offsets, objects: l.objects.clone() });
    l.plate = Some(plate.clone());
    drop(hub().plate.set(plate));
}

/// Where each part sits on the plate (mm): a slice's preview holds its parts' beads in their own
/// frames at these, as the live tiles do.
fn offsets(l: &LiveState) -> Arc<[[f32; 2]]> {
    l.placement.iter().map(|(_, d)| [d.x_mm() as f32, d.y_mm() as f32]).collect()
}

/// What the views set, as a whole ([`Hub::view`]): the UI stores a new one, the live state's owner
/// and the estimate preview read the newest.
#[derive(Default)]
struct ViewSettings {
    layer_height: f64,
    machine: Option<fab_plate::machine::Machine>,
    colours: Vec<[f32; 4]>,
    /// A test plate's input: its pieces say whose each of the slice's moves are.
    tests: Option<makepad_fab_slice::live::MakeInput>,
    /// The heat-set inserts the job melts in after the print ([`PrintJob::melts`]).
    melts: Vec<(fab::print::PreviewMelt, usize)>,
}

/// A value threads share whole: [`Shot::set`] puts a new one in, [`Shot::get`] hands out the
/// newest. Its lock is held for one pointer move and never leaves these methods, so nothing is
/// ever taken while it is held and nobody waits on work behind it.
pub(crate) struct Shot<T>(Mutex<Option<Arc<T>>>);

impl<T> Shot<T> {
    pub(crate) const fn new() -> Self {
        Shot(Mutex::new(None))
    }

    /// `value` is the newest; the one it replaces comes back, for the caller to let go of.
    pub(crate) fn set(&self, value: Arc<T>) -> Option<Arc<T>> {
        let _depth = shot_depth::enter();
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replace(value)
    }

    /// Empty the shot; its value comes back, for the caller to let go of.
    pub(crate) fn take(&self) -> Option<Arc<T>> {
        let _depth = shot_depth::enter();
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    /// The newest value.
    pub(crate) fn get(&self) -> Option<Arc<T>> {
        let _depth = shot_depth::enter();
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// The newest value for a view's frame, unless the lock is held this instant.
    pub(crate) fn try_get(&self) -> Option<Arc<T>> {
        let _depth = shot_depth::enter();
        match self.0.try_lock() {
            Ok(value) => value.clone(),
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().clone(),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

/// A lock of the live slice held for a few field updates, a push or a take, never for work, and
/// never taken while another of its locks (a [`Leaf`], a [`Shot`] or the owner's inbox) is held:
/// in tests, taking one inside another fails ([`shot_depth`]).
pub(crate) struct Leaf<T>(Mutex<T>);

impl<T> Leaf<T> {
    pub(crate) const fn new(value: T) -> Self {
        Leaf(Mutex::new(value))
    }

    pub(crate) fn lock(&self) -> LeafGuard<'_, T> {
        let depth = shot_depth::enter();
        LeafGuard { guard: self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner), _depth: depth }
    }
}

impl<T: Default> Default for Leaf<T> {
    fn default() -> Self {
        Leaf::new(T::default())
    }
}

/// A held [`Leaf`]: let go of before its depth (fields drop in order).
pub(crate) struct LeafGuard<'a, T> {
    guard: std::sync::MutexGuard<'a, T>,
    _depth: shot_depth::Depth,
}

impl<'a, T> LeafGuard<'a, T> {
    /// Sleeps on `cv` with the lock let go, for at most `timeout`; held again after.
    fn wait_timeout(self, cv: &Condvar, timeout: std::time::Duration) -> Self {
        let LeafGuard { guard, _depth } = self;
        let guard = cv.wait_timeout(guard, timeout).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        LeafGuard { guard, _depth }
    }
}

impl<T> std::ops::Deref for LeafGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> std::ops::DerefMut for LeafGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

/// Test check: none of the live slice's locks (a [`Leaf`], a [`Shot`], the live state's inbox
/// [`LiveOwner`]) is taken inside another: each is held for a pointer move or a few field updates.
mod shot_depth {
    #[cfg(test)]
    thread_local! {
        static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    pub(super) struct Depth(());

    pub(super) fn enter() -> Depth {
        #[cfg(test)]
        DEPTH.with(|d| {
            assert_eq!(d.get(), 0, "one of the live slice's locks is taken while another is held");
            d.set(d.get() + 1);
        });
        Depth(())
    }

    impl Drop for Depth {
        fn drop(&mut self) {
            #[cfg(test)]
            DEPTH.with(|d| d.set(d.get() - 1));
        }
    }
}

struct Hub {
    main_version: Arc<EngineVersion>,
    check_version: Arc<EngineVersion>,
    engine: LiveEngine,
    checks: LiveEngine,
    /// The live state's owner: every event and call that changes the live state is a message to
    /// it ([`LiveOwner`]).
    owner: LiveOwner,
    /// What the views set: the UI stores a whole new one, readers take the newest.
    view: Shot<ViewSettings>,
    /// The plate as the live state has it, for the estimate's preview ([`PlateShot`]).
    plate: Shot<PlateShot>,
    /// Inserts excluded from the slicer's input, keyed by that input's plate id.
    insert_plate: Shot<InsertPlate>,
    /// Live views exist (a headset, the desktop's Slice render mode): build their results.
    on: std::sync::atomic::AtomicU32,
    /// The plate's toolpaths are waited for where no view shows them ([`ToolpathsWanted`]).
    waits: std::sync::atomic::AtomicU32,
    /// Print editor slices asked and not answered yet ([`settle`]), for [`streaming`].
    print_jobs: std::sync::atomic::AtomicU32,
    /// Model workers waiting for the committed plate's live slice ([`AiSlices`]).
    ai_waits: std::sync::atomic::AtomicU32,
    /// No part is held by the hands.
    released: std::sync::atomic::AtomicBool,
    /// The newest publication, set by the live state's owner; views take it with `try_get`.
    latest: Shot<LiveResult>,
    jobs: Leaf<HashMap<u64, PrintJob>>,
    /// Check jobs settled but waiting for their sliced report (it streams
    /// after the settle): (revision, check id, the report so far).
    pending_checks: Leaf<Vec<(u64, u64, Arc<fab_printability::Report>)>>,
    /// The newest complete merged plan, its revision, when it was first made (Unix seconds: a
    /// later revision that reuses it whole keeps the time) and the input it was made from
    /// (plate and settings ids; none when a newer input superseded it as it came).
    toolpaths: Shot<Sliced>,
    /// What those waits hear besides the toolpaths; its lock also orders their checks with the
    /// toolpaths' publication ([`Hub::slice_ready`]).
    ai: Leaf<AiWaits>,
    slice_ready: Condvar,
    /// The Print editor's settings, for live inputs.
    setup: Leaf<Option<(u64, CheckSetup)>>,
}

static HUB: OnceLock<Hub> = OnceLock::new();
static POOL: OnceLock<TaskPool> = OnceLock::new();
/// A slice's jobs submitted and not ended yet: none means no slice's work runs or waits.
static SLICE_JOBS: AtomicUsize = AtomicUsize::new(0);

/// One of a slice's jobs, counted from its submission until it ends or is dropped unrun.
struct SliceJob(());

impl SliceJob {
    fn begin() -> SliceJob {
        SLICE_JOBS.fetch_add(1, Ordering::AcqRel);
        SliceJob(())
    }
}

impl Drop for SliceJob {
    fn drop(&mut self) {
        SLICE_JOBS.fetch_sub(1, Ordering::AcqRel);
        trim_when_quiet();
    }
}

/// `work` as a slice's job at `class`, counted until it ends ([`quiet`]).
fn run_slice_work(class: Class, name: &'static str, work: impl FnOnce() + Send + 'static) {
    let job = SliceJob::begin();
    run_while_open(POOL.get().expect("slice runtime initialized"), class, name, move || {
        let _job = job;
        work();
    });
}

/// Slicer layer workers inherit the engine job's class.
struct PoolFanOut;

impl makepad_fab_toolpath::par::FanOut for PoolFanOut {
    fn run(&self, len: usize, f: &(dyn Fn(usize) + Sync)) {
        POOL.get().expect("slice runtime initialized").fan_out(len, |i| {
            let _ = checkpoint();
            f(i);
        });
    }
    /// Each helper is a job of the caller's class, in the caller's scope: it holds a worker and
    /// a permit only while it runs. The lead works on its own worker and waits for the helpers
    /// without its permit.
    fn crew(&self, lead: &(dyn Fn(&dyn Crew) + Sync), helper: &(dyn Fn(&dyn Crew) + Sync)) {
        POOL.get().expect("slice runtime initialized").scope(|scope| lead(&PoolCrew { scope, helper }));
    }
}

struct PoolCrew<'s, 'e> {
    scope: &'s TaskScope<'s, 'e>,
    helper: &'s (dyn Fn(&dyn Crew) + Sync),
}

impl Crew for PoolCrew<'_, '_> {
    fn recruit(&self) {
        let (scope, helper) = (self.scope, self.helper);
        scope.spawn(move || {
            let _ = checkpoint();
            helper(&PoolCrew { scope, helper });
        });
    }
    fn wait(&self, f: &mut dyn FnMut()) {
        without_permit(f)
    }
    fn leave(&self) -> bool {
        worker_wanted()
    }
}

/// The process's memory budget (0, no stop, until [`init`] arms it).
static MEMORY_BUDGET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Evaluating a model stops at the process's memory budget, with the reason.
pub fn model_memory_guard() -> fab_model::eval::MemoryGuard {
    Arc::new(|| {
        let budget = MEMORY_BUDGET.load(Ordering::Relaxed);
        let used = process_footprint_bytes()?;
        (budget > 0 && used > budget).then(|| format!("building this model reached the {:.0} GB memory limit and stopped: simplify it or lower its resolution", budget as f64 / (1u64 << 30) as f64))
    })
}

/// Tests: the slice runtime as the app starts it, once per process, on a pool no test closes (a
/// test runtime's Cx closes its own pool when it quits, and [`POOL`] is the first one set), with
/// no memory stop ([`start`]).
#[cfg(test)]
pub(crate) fn init_for_tests() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| start(&Cx::new(Box::new(|_, _| {}))));
}

/// Start the engines on the runtime's task pool (once, at startup), and arm the process's memory
/// stops at its budget ([`arm_memory_stops`]).
pub fn init(cx: &Cx) {
    start(cx);
    arm_memory_stops(cx.memory_budget() as u64);
}

/// The engines on the runtime's task pool, without the process's memory stops. A test process
/// starts them so: there the footprint the stops read is every test's that runs at once, and a
/// bare Cx's budget is the platform's 1.5 GB placeholder, not the process envelope its startup
/// reads. `init_for_tests` armed the stops at that, and a plate picture's build stopped "at the
/// 2 GB memory limit" while 150 sibling tests held 8 GB (11-18).
pub(crate) fn start(cx: &Cx) {
    fab::print::set_drawn_hook(drawn);
    let pool = cx.task_pool();
    makepad_fab_toolpath::par::set_threads(pool.worker_count());
    // The pool first: the fan-out is global, so a slice already running on another thread can
    // use it as soon as it is set. One pool for the process, the first Cx's: another Cx's init
    // keeps it.
    let pool = POOL.get_or_init(|| pool).clone();
    makepad_fab_toolpath::par::set_fan_out(Arc::new(PoolFanOut));
    set_resume(&hub().engine, hub().main_version.clone(), pool.clone(), true);
    set_resume(&hub().checks, hub().check_version.clone(), pool, false);
}

/// A model's build and a slice stop at the process's memory budget, whatever the model or plate,
/// with the reason.
fn arm_memory_stops(budget: u64) {
    MEMORY_BUDGET.store(budget, Ordering::Relaxed);
    let guard: makepad_fab_slice::live::MemoryGuard = Arc::new(move || {
        let used = process_footprint_bytes()?;
        (used > budget).then(|| format!("slicing this plate reached the {:.0} GB memory limit and stopped: make the parts smaller or simpler", budget as f64 / 1e9))
    });
    hub().engine.set_memory_guard(guard.clone());
    hub().checks.set_memory_guard(guard);
}

/// A heavy job ended since the last trim: freed pages wait to go back to the system.
static TRIM_PENDING: AtomicBool = AtomicBool::new(false);
/// A trim is queued and has not started.
static TRIM_QUEUED: AtomicBool = AtomicBool::new(false);
/// Runs of pictures painting ([`Painting`]).
static PICTURES: AtomicUsize = AtomicUsize::new(0);

/// A heavy job ended: a replan once every part's plan is complete, a settle (cancelled ones too),
/// a run of pictures. What it freed goes back to the system off the UI thread once no slice's work
/// and no run of pictures goes on, so a burst of them is trimmed once, at its end.
pub(crate) fn after_heavy_job() {
    TRIM_PENDING.store(true, Ordering::Release);
    trim_when_quiet();
}

/// No slice's work runs or waits, and no run of pictures goes on.
fn quiet() -> bool {
    SLICE_JOBS.load(Ordering::Acquire) == 0 && PICTURES.load(Ordering::Acquire) == 0
}

/// Queues the one trim when one is pending and all is quiet. It trims if all is still quiet when it
/// starts; else the end of what started meanwhile asks again.
fn trim_when_quiet() {
    if !TRIM_PENDING.load(Ordering::Acquire) || !quiet() || TRIM_QUEUED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(pool) = POOL.get() else {
        TRIM_QUEUED.store(false, Ordering::Release);
        log!("fab: cannot return freed memory: slice pool is not initialized");
        return;
    };
    let trim = || {
        TRIM_QUEUED.store(false, Ordering::Release);
        if quiet() && TRIM_PENDING.swap(false, Ordering::AcqRel) {
            makepad_fab_slicer::clip::release_scratch();
            release_free_memory();
        }
    };
    match pool.submit_named(Class::Background, "fab-trim", trim) {
        Ok(handle) => handle.detach(),
        Err(error) => {
            TRIM_QUEUED.store(false, Ordering::Release);
            log!("fab: cannot return freed memory: {error}");
        }
    }
}

/// Pictures paint, one after another, until this drops: its end is a heavy job's.
pub(crate) struct Painting(());

impl Painting {
    pub(crate) fn begin() -> Painting {
        PICTURES.fetch_add(1, Ordering::AcqRel);
        Painting(())
    }
}

impl Drop for Painting {
    fn drop(&mut self) {
        PICTURES.fetch_sub(1, Ordering::AcqRel);
        after_heavy_job();
    }
}

/// One pool Version per engine; its checkpoints travel with the input snapshot.
#[derive(Default)]
pub(crate) struct EngineVersion {
    tickets: Leaf<Vec<(makepad_fab_slice::live::Checkpoint, Ticket)>>,
    version: Version,
    current: Leaf<Option<Ticket>>,
    changed: Leaf<Option<std::time::Instant>>,
}

impl EngineVersion {
    pub(crate) fn ticket(&self, checkpoint: &makepad_fab_slice::live::Checkpoint) -> Ticket {
        self.tickets.lock().iter().find(|(c, _)| Arc::ptr_eq(c, checkpoint)).expect("input ticket retained by checkpoint").1.clone()
    }
    pub(crate) fn next(self: &Arc<Self>) -> makepad_fab_slice::live::Next {
        let this = self.clone();
        Arc::new(move || {
            let ticket = this.version.next();
            *this.current.lock() = Some(ticket.clone());
            let check = ticket.clone();
            let checkpoint: makepad_fab_slice::live::Checkpoint = Arc::new(move || check.is_stale() || checkpoint().is_err());
            let mut tickets = this.tickets.lock();
            tickets.retain(|(check, _)| Arc::strong_count(check) > 1);
            tickets.push((checkpoint.clone(), ticket));
            checkpoint
        })
    }

    fn bump(&self) {
        self.version.bump();
        self.changed.lock().get_or_insert_with(std::time::Instant::now);
    }

    fn stale(&self) -> bool {
        self.current.lock().as_ref().is_some_and(Ticket::is_stale)
    }
}

/// A shown slice's viewed layers (and the start of each revision) are Interactive, the rest of
/// it Visible; checks and unseen slices are Background, and so is every slice while another
/// document is on its way to the bed ([`loading`]): the person waits on that one, and the slice
/// is of the document it replaces.
pub(crate) fn class_of(main: bool, urgency: Urgency) -> Class {
    if main && streaming() && LOADING.load(Ordering::Acquire) == 0 { shown_class(urgency) } else { Class::Background }
}

/// The call of the load the person waits on (0: none).
static LOADING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Load `call` puts another document on the bed in place of the one there (queued now, it
/// takes over from one still on its way): until it is in ([`load_over`]), what slices is of the
/// document it replaces.
pub fn loading(call: u64) {
    LOADING.store(call, Ordering::Release);
}

/// Call `call` came back (the app took its answer, after the document it loaded is on the bed,
/// or as its failure or cancel): when it is the load the person waits on, slices are what the
/// person looks at again. An older load's answer leaves the newer one waited on.
pub fn load_over(call: u64) {
    let _ = LOADING.compare_exchange(call, 0, Ordering::AcqRel, Ordering::Acquire);
}

/// The class of a shown slice's work (a live view or a Print editor slice in flight).
pub(crate) fn shown_class(urgency: Urgency) -> Class {
    match urgency {
        Urgency::Rest => Class::Visible,
        Urgency::Viewed | Urgency::Focus => Class::Interactive,
    }
}

fn spawn(main: bool, version: Arc<EngineVersion>) -> Spawn {
    Arc::new(move |urgency, job: Box<dyn FnOnce() + Send>| {
        let class = class_of(main, urgency);
        let version = version.clone();
        run_slice_work(class, "fab slice", move || {
            job();
            let changed = version.changed.lock().take();
            if let Some(t) = changed {
                on_event(!main, LiveEvent::Stopped { ms: t.elapsed().as_secs_f64() * 1e3 });
            }
        });
    })
}

/// `work` on `pool`, dropped once the pool closed (the app ends): nobody wants its answer then.
fn run_while_open(pool: &TaskPool, class: Class, name: &'static str, work: impl FnOnce() + Send + 'static) {
    if let Ok(handle) = pool.submit_named(class, name, work) {
        handle.detach();
    }
}

/// A view lets go of a live result ([`LiveSlice::latest`]). One holding the last reference to its
/// tiles preview, whose beads the settled slice's replaced ([`adopt`]), frees it on the
/// slicing pool: hundreds of MB, which took a headset frame 13 to 37 ms on the UI thread. Any other
/// result only lets go of what newer ones share, here.
pub fn let_go(result: Arc<LiveResult>) {
    if POOL.get().is_some() && Arc::strong_count(&result) == 1 && result.preview.as_ref().is_some_and(|p| Arc::strong_count(p) == 1) {
        run_slice_work(Class::Visible, "fab live tiles freed", move || drop(result));
    }
}

/// Work nobody waits for (weighing other nozzles for a plate's colours), on the slicing pool
/// below every slice. Nothing runs when the pool is not up (tests run such work themselves).
pub fn run_background(name: &'static str, work: impl FnOnce() + Send + 'static) {
    if POOL.get().is_some() {
        run_slice_work(Class::Background, name, work);
    }
}

/// Fab's slice cache ([`SliceCache`]), opened when the app starts ([`open_slice_cache`]).
static SLICE_CACHE: OnceLock<SliceCache> = OnceLock::new();

/// Opens the slice cache under `root` (once, as the app starts), removing what the processes
/// that ended left in it. Without one a settled slice keeps its G-code in memory.
pub fn open_slice_cache(root: &std::path::Path) {
    match SliceCache::open(root) {
        Ok(cache) => {
            let _ = SLICE_CACHE.set(cache);
        }
        Err(e) => log!("fab slice cache: {e}: settled slices keep their G-code in memory"),
    }
}

/// A shown settled slice's G-code goes to the slice cache, on a worker below every slice: once
/// its hash and the print safety check have read it, only a package reads it again. A slice
/// that goes before its turn needs no file.
fn spill_gcode(settled: &Arc<makepad_fab_slice::live::Settled>) {
    if SLICE_CACHE.get().is_none() {
        return;
    }
    let settled = Arc::downgrade(settled);
    run_background("fab slice G-code to disk", move || {
        let (Some(cache), Some(settled)) = (SLICE_CACHE.get(), settled.upgrade()) else { return };
        match settled.gcode.spill(cache) {
            // What it held goes back to the system once all is quiet.
            Ok(()) => after_heavy_job(),
            Err(e) => log!("fab slice cache: {e}: the slice keeps its G-code in memory"),
        }
    });
}

/// Work the person waits for besides a slice (a printer package for a send or an export), on
/// the slicing pool in a shown slice's class.
pub fn run_shown(name: &'static str, work: impl FnOnce() + Send + 'static) {
    run_while_open(POOL.get().expect("slice runtime initialized"), Class::Visible, name, work);
}

type Job = Arc<Leaf<Option<Box<dyn FnOnce() + Send>>>>;
struct PendingJob { pool: TaskPool, class: Class, ticket: Ticket, job: Job }
/// Continuations the pool refused, resubmitted by [`SliceHost::event`].
static JOBS: Leaf<VecDeque<PendingJob>> = Leaf::new(VecDeque::new());

fn submit(job: PendingJob, wake: bool) {
    let pending = job.job.clone();
    let counted = SliceJob::begin();
    match job.pool.submit_at(job.class, "sdf plan continuation", job.ticket.clone(), move || {
        let _counted = counted;
        // Taken out first: the work runs with the cell let go (it calls the sink, and a pool
        // checkpoint yields only a thread that holds no lock).
        let job = pending.lock().take();
        if let Some(job) = job { job(); }
    }) {
        Ok(handle) => handle.detach(),
        Err(e) => {
            log!("sdf plan continuation queued: {:?}", e);
            JOBS.lock().push_back(job);
            if wake { SignalToUI::set_ui_signal(); }
        }
    }
}

/// Each run's polls go to the pool with its input's ticket, so a newer input skips them; an
/// edited part's viewed layer continues even once a newer input came. A poll the pool refuses
/// waits in [`JOBS`] for the next signal.
pub(crate) fn set_resume(engine: &LiveEngine, version: Arc<EngineVersion>, pool: TaskPool, main: bool) {
    engine.set_resume(Arc::new(move |checkpoint: makepad_fab_slice::live::Checkpoint| {
        let ticket = version.ticket(&checkpoint);
        let pool = pool.clone();
        Arc::new(move |urgency: Urgency, job| {
            let ticket = if urgency == Urgency::Viewed { Ticket::none() } else { ticket.clone() };
            submit(PendingJob { pool: pool.clone(), class: class_of(main, urgency), ticket, job: Arc::new(Leaf::new(Some(job))) }, true);
        }) as Spawn
    }));
}

/// The live slicer's side of the app's events: it resubmits the continuations the pool refused,
/// and follows the live patches the app takes in.
#[derive(Default)]
pub struct SliceHost {
    /// The live patches taken in when last asked ([`Cx::live_patches`]).
    patches: u64,
}

impl SliceHost {
    /// One call site in App::handle_event. True when a live patch just taken in changed the
    /// slicing code: the shown slice is made again by it ([`code_patched`]).
    pub fn event(&mut self, cx: &Cx, event: &Event) -> bool {
        if matches!(event, Event::Draw(_) | Event::NextFrame(_) | Event::Signal) {
            // Pool completions wake Signal. A failed retry stays queued until that wake, rather
            // than spinning or asking for paint.
            let jobs = std::mem::take(&mut *JOBS.lock());
            for job in jobs { submit(job, false); }
        }
        let patches = cx.live_patches();
        if patches == self.patches {
            return false;
        }
        self.patches = patches;
        code_patched()
    }
}

/// A live patch landed. True when it changed code the shown slice comes from: that slice runs its
/// input again and makes anew only what that code makes (the support code: the supports alone),
/// and a slice settled before it is the old code's. The checks' engine answers each check once,
/// by the code loaded when it runs.
pub fn code_patched() -> bool {
    let Some(h) = HUB.get() else { return false };
    let again = h.engine.code_patched();
    if again {
        log!("fab live: a live patch changed the slicing code ({:?}): the slice runs again by it", makepad_fab_slice::live::loaded_code());
    }
    again
}

fn hub() -> &'static Hub {
    HUB.get_or_init(|| {
        let sink: Sink = Arc::new(|e| on_event(false, e));
        let check_sink: Sink = Arc::new(|e| on_event(true, e));
        // The engines run on worker threads and never block the UI: the main one at the
        // priority of whoever waits for it, the checks' in the background.
        let main_version = Arc::new(EngineVersion::default());
        let check_version = Arc::new(EngineVersion::default());
        let engine = LiveEngine::new(spawn(true, main_version.clone()), sink, main_version.next());
        let checks = LiveEngine::new(spawn(false, check_version.clone()), check_sink, check_version.next());
        // Estimates share the pool, but only a slice driver reports cancel latency.
        engine.set_background(Arc::new(|_, job| {
            run_slice_work(Class::Background, "fab estimate", job);
        }));
        // The estimate's records build its preview on the estimate's own thread.
        engine.set_records(Arc::new(records_taker));
        Hub {
            main_version, check_version,
            engine,
            checks,
            owner: LiveOwner::new(),
            view: Shot::new(),
            plate: Shot::new(),
            insert_plate: Shot::new(),
            on: std::sync::atomic::AtomicU32::new(0),
            waits: std::sync::atomic::AtomicU32::new(0),
            print_jobs: std::sync::atomic::AtomicU32::new(0),
            ai_waits: std::sync::atomic::AtomicU32::new(0),
            released: std::sync::atomic::AtomicBool::new(true),
            latest: Shot::new(),
            jobs: Leaf::new(HashMap::new()),
            pending_checks: Leaf::new(Vec::new()),
            toolpaths: Shot::new(),
            ai: Leaf::new(AiWaits::default()),
            slice_ready: Condvar::new(),
            setup: Leaf::new(None),
        }
    })
}

fn hash64(bytes: &[u8]) -> u64 {
    // FNV-1a: identities only, never security.
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3))
}

/// The app shuts down: both engines finish their focus chunk and stop, taking no new work.
pub fn stop() {
    if let Some(h) = HUB.get() {
        h.engine.set_enabled(false);
        h.checks.set_enabled(false);
    }
}

/// A shape changed (an edit, a parameter's live value, an undo...): the slices of the old shape
/// stop at their next chunk, their cores go to the shape's update, and slicing waits for the new
/// shape ([`shape_published`], or the new plate's input).
pub fn shape_changed() {
    if let Some(h) = HUB.get() {
        h.main_version.bump();
        h.check_version.bump();
    }
}

/// The new shape is published (its plate reached the views): slicing resumes on it. A new input
/// resumed it already; this resumes it for an unchanged or failed plate.
pub fn shape_published() {
    if let Some(h) = HUB.get() {
        if h.main_version.stale() { h.engine.release(); }
        if h.check_version.stale() { h.checks.release(); }
    }
}

/// The shown slice's engine revisions: (its newest input's, the one its slice was served for).
pub fn revisions() -> Option<(u64, u64)> {
    HUB.get().map(|h| h.engine.revisions())
}

/// The whole live slice's print times once edits rest: the time model without G-code text,
/// in the background, answered as `PrintEvent::Estimated` (a preview that plays). A shape
/// change stops it.
pub fn estimate() {
    if let Some(h) = HUB.get() {
        h.engine.estimate();
    }
}

/// Whether the whole live slice's print time is kept: a step back to a slice made whole before,
/// which [`estimate`] answers at once.
pub fn estimate_kept() -> bool {
    HUB.get().is_some_and(|h| h.engine.estimate_kept())
}

/// Stop the slice in progress and forget its input: the engine idles at zero CPU until the
/// next slice asks.
pub fn cancel() {
    let h = hub();
    h.engine.cancel();
    drop(h.insert_plate.take());
    h.owner.post(Msg::Release);
}

/// Release the live preview when no view needs it. Slots keep their bead blocks for the
/// next edit, so untouched parts never rebuild them.
fn release_preview(l: &mut LiveState) {
    if live_views() || l.print_job.is_some() {
        return;
    }
    l.preview = None;
}

/// The Print editor's settings changed: live inputs slice with them from now on.
pub fn set_print_setup(setup: CheckSetup) {
    let id = setup.id();
    *hub().setup.lock() = Some((id, setup));
}

/// An input for `plate` with the Print editor's settings (Fab defaults until it has some).
pub fn input_for(plate_id: u64, setup: &CheckSetup, plate: Arc<Plate>, machine: fab_plate::machine::Machine, frames: Option<&[fab_model::eval::PartFrame]>, fields: Option<&[Option<fab_model::eval::PartField>]>) -> LiveInput {
    let resolve = setup.clone();
    let (p, m) = (plate.clone(), machine.clone());
    // A part that is one SDF solid slices from its field (its mesh is for display); its paint
    // is on the part (by position).
    let frames = frames.map(|f| {
        Arc::new(f.iter().enumerate().map(|(i, f)| {
            let field = fields.and_then(|fs| fs.get(i)).and_then(Option::as_ref).and_then(slicer_field);
            Frame { local: f.local.clone(), offset: f.offset, field }
        }).collect())
    });
    // Non-planar tops plan with the installed head, else the placeholder: they slice and
    // preview either way, and the print check refuses them until a measured head is installed.
    let np_head = Some(Arc::new(crate::head::boxes(&crate::head::for_slicing())));
    LiveInput { plate_id, settings_id: setup.id_for(&plate), setup: Arc::new(move |kept| resolve.job_setup_of(&p, &m, kept)), plate, paint: Arc::new(Vec::new()), frames, machine, np_head, pieces: None }
}

/// A model part's field as the slicer's, in the part's frame (its mesh before the plate
/// offset): its field as a distance and the map to the part (a uniform scale goes into the
/// shape, as a stretch already has), and the solid's box in that frame (what the layout placed
/// it by). Both are the layout's own, so a part that only moves keeps its content key.
fn slicer_field(pf: &fab_model::eval::PartField) -> Option<PartField> {
    let (shape, _, _, to_part) = pf.distance();
    PartField::placed(shape.clone(), *to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max))))
}

/// The plate id of an evaluated model plate: its sources, and what each part and paint colour
/// prints with (mapped onto the same sources without a rebuild).
pub fn plate_id(ev: &Evaluated) -> u64 {
    let tools: Vec<u32> = ev.plate.parts.iter().flat_map(|p| {
        let paint = p.paint.iter().flat_map(|sp| sp.colors.iter().map(|c| c.filament.map_or(0, |f| f + 1)));
        [p.filament, p.nozzle.map_or(0, |n| n as u32 + 1)].into_iter().chain(paint)
    }).collect();
    let tools = format!("{tools:?}");
    hash64(&ev.content) ^ hash64(&(ev.plate.parts.len() as u64).to_le_bytes()) ^ hash64(tools.as_bytes()).rotate_left(1)
}

/// A distance-field part's field at a dragged value: part `part` of the plate is sliced from
/// it in place of its committed field.
#[derive(Clone)]
pub struct FieldSwap {
    pub part: usize,
    pub sdf: fab::api::PartSdf,
}

/// The plate id of `ev` with `swaps` in place: the committed plate's when there are none.
pub fn edited_plate_id(ev: &Evaluated, swaps: &[FieldSwap]) -> u64 {
    swaps.iter().fold(plate_id(ev), |h, s| (h ^ hash64(&s.sdf.key) ^ s.part as u64).wrapping_mul(0x100_0000_01b3))
}

/// `ev`'s part fields with `swaps` in place: a dragged shape at its committed part's place,
/// over the box the model's evaluation finds for it there (the same search), so a live field
/// slices as its commit will and the commit's slice is the live one's (on the engine's thread).
pub fn edited_fields(ev: &Evaluated, swaps: &[FieldSwap]) -> Vec<Option<fab_model::eval::PartField>> {
    let mut fields = ev.fields.clone();
    for s in swaps {
        if let Some(Some(f)) = fields.get_mut(s.part) {
            f.shape = s.sdf.shape.clone();
            f.key = s.sdf.key;
            f.bounds = fab_model::eval::field_extent(&f.shape, f.key, &f.to_part);
        }
    }
    fields
}

fn send_live_plate(ev: &Arc<Evaluated>) {
    let setup = hub().setup.lock().as_ref().map(|(_, s)| s.clone()).unwrap_or_else(CheckSetup::fab_defaults);
    let (id, settings, parts) = (plate_id(ev), setup.id_for(&ev.plate), ev.plate.parts.len());
    let inserts = ev.plate.excluded.iter().enumerate().filter_map(|(i, p)| p.insert.as_ref().map(|_| i)).collect();
    drop(hub().insert_plate.set(Arc::new(InsertPlate { id, evaluated: ev.clone(), inserts })));
    let ev = ev.clone();
    hub().engine.set_input_with(id, settings, parts, Arc::new(move || input_for(plate_id(&ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields))));
}

/// A live edit or settle's input: its identity, part count, and how to build it (on the
/// engine's thread).
pub struct SliceInput {
    pub plate_id: u64,
    pub settings_id: u64,
    pub parts: usize,
    pub make: makepad_fab_slice::live::MakeInput,
}

/// Another view may have sent the same plate; a different input or a grab supersedes it.
pub fn input_revision(input: &SliceInput) -> Option<u64> {
    hub().engine.input_revision(input.plate_id, input.settings_id)
}

/// What the live view reads a job's slices with.
fn view_job(job: &PrintJob) {
    let view = ViewSettings { layer_height: job.settings.layer_height, machine: Some(job.machine.clone()), colours: job.colours.clone(), tests: job.tests.clone(), melts: job.melts.clone() };
    drop(hub().view.set(Arc::new(view)));
}

/// The view settings now (none stored yet: no machine, no colours).
fn view() -> Arc<ViewSettings> {
    hub().view.get().unwrap_or_default()
}

/// A slice made earlier stands again: `input` is the engine's newest with these toolpaths of it,
/// and nothing slices. Its revision.
pub fn hold_input(input: &SliceInput, job: &PrintJob, mut toolpaths: Sliced) -> u64 {
    view_job(job);
    let h = hub();
    let revision = h.engine.hold_input(input.plate_id, input.settings_id, input.parts, input.make.clone());
    toolpaths.revision = revision;
    drop(h.toolpaths.set(Arc::new(toolpaths)));
    log!("fab live input: revision={revision} plate={} brought back", input.plate_id);
    revision
}

/// Send an edit without requesting G-code. Keep its input until Print, Export or Slice asks to settle.
pub fn set_input(input: &SliceInput, job: &PrintJob, edited: std::time::Instant) -> u64 {
    view_job(job);
    let h = hub();
    h.released.store(true, Ordering::Relaxed);
    // Include the model's work in both the viewed-layer and full-reslice clocks.
    {
        let mut ledger = LEDGER.lock();
        let edited = ledger.edit_of(input.plate_id).unwrap_or(edited);
        ledger.newest_edit = Some(edited);
    }
    log!("fab live edit: plate {}", input.plate_id);
    h.engine.stop_settle();
    let revision = h.engine.set_input_with(input.plate_id, input.settings_id, input.parts, input.make.clone());
    LEDGER.lock().sent(revision, input.plate_id);
    log!("fab live input: revision={revision} plate={}", input.plate_id);
    revision
}

/// The Print editor's Slice: the exact slice of `input`, answered as `PrintEvent::Sliced`
/// (or `Checked` for a check job).
pub fn settle(job: PrintJob, input: SliceInput) {
    let h = hub();
    let tag = if let Some(id) = job.check { id } else { job.gen };
    let engine = if job.check.is_some() { &h.checks } else { &h.engine };
    // Check tags and slice generations live in separate engines.
    let key = if job.check.is_some() { tag | 1 << 63 } else { tag };
    let print = job.check.is_none();
    if print {
        h.owner.post(Msg::PrintJob { gen: job.gen, machine: job.machine.clone(), colours: job.colours.clone() });
    }
    let new = h.jobs.lock().insert(key, job).is_none();
    if print && new {
        h.print_jobs.fetch_add(1, Ordering::Relaxed);
    }
    engine.settle_with(tag, input.plate_id, input.settings_id, input.parts, input.make);
}

#[derive(Clone)]
struct EditTiming {
    asked: std::time::Instant,
    model_ms: f64,
}
static DRAG_LIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn drag_live() -> bool { DRAG_LIVE.load(Ordering::Relaxed) }

pub fn set_drag_live(on: bool) {
    let was = DRAG_LIVE.swap(on, Ordering::Relaxed);
    if on && !was { LEDGER.lock().models.clear(); }
}

pub fn model_built(plate: u64, asked: std::time::Instant, model_ms: f64) {
    LEDGER.lock().model_built(plate, asked, model_ms);
}

struct ViewedTiming {
    revision: u64,
    part: usize,
    layer: u32,
    edit: EditTiming,
    field_ms: f64,
    slice_ms: f64,
    plan_ms: f64,
    published: std::time::Instant,
    preview: Option<u64>,
    /// The tile that showed it first: a later preview, or the settled slice, that shows the same
    /// tile there shows it too.
    tile: Option<u64>,
}

struct DrawnPreview {
    edited: Option<std::time::Instant>,
    tiles: Vec<ShownTile>,
    focus: Vec<(u64, usize, u32)>,
    /// The revision it shows; none for the Print editor's slice.
    revision: Option<u64>,
    /// That revision's slice is whole in it.
    whole: bool,
}

/// A tile a frame drew first.
#[derive(Clone)]
pub struct DrawnTile {
    pub part: usize,
    pub layer: u32,
    /// The newest revision that planned its layer.
    pub revision: u64,
    /// When, after the edit (ms).
    pub ms: f64,
}

/// What frames drew of one revision's previews: the one log line a run writes, and what the slice
/// tool tells of it on demand ([`drawn_runs`]).
#[derive(Clone, Default)]
pub struct DrawnRun {
    /// The frames that first drew one of its previews.
    pub frames: usize,
    /// An older content's tiles it drew, until this content draws those layers.
    pub ghosts: usize,
    /// How many of its own tiles were on screen before its whole slice was in.
    pub before_whole: usize,
    pub tiles: Vec<DrawnTile>,
    /// How many tiles its last line told.
    told: usize,
}

impl DrawnRun {
    /// Its line, once it has drawn a tile its last line did not tell.
    fn line(&mut self, revision: u64) -> Option<String> {
        let tiles = self.tiles.len() + self.ghosts;
        if tiles == self.told {
            return None;
        }
        self.told = tiles;
        let parts: std::collections::BTreeSet<usize> = self.tiles.iter().map(|t| t.part).collect();
        let at = |tile: Option<&DrawnTile>| tile.map_or(0.0, |t| t.ms);
        Some(format!("fab live drawn: revision={revision} {} new tiles in parts {parts:?}, {} ghost, {} frames, {} before the whole slice, {:.0}..{:.0} ms after the edit", self.tiles.len(), self.ghosts, self.frames, self.before_whole, at(self.tiles.first()), at(self.tiles.last())))
    }
}

/// The revisions whose runs are kept, newest last.
const DRAWN_RUNS: usize = 64;

/// The viewed-layer ledger: the edit each revision answers, the viewed layers the engine planned,
/// the previews that hold them and the frame that first draws each. Its lines are the latency of
/// an edit to its viewed layer on screen, and one for each revision, of what frames drew of it, once
/// its whole slice or a later revision's is in ([`DrawnRun`]). A viewed layer is
/// drawn by the first frame that shows its tile: a live preview of its revision, a later one that
/// kept the tile, or the Print editor's slice while the live view shows it.
struct Ledger {
    /// When the newest live input was sent, for the timeline of tiles actually drawn.
    newest_edit: Option<std::time::Instant>,
    /// Each plate's model, by plate id: when it was asked for and how long it took.
    models: BTreeMap<u64, EditTiming>,
    /// The edit each revision answers.
    inputs: BTreeMap<u64, EditTiming>,
    viewed: Vec<ViewedTiming>,
    previews: BTreeMap<u64, DrawnPreview>,
    runs: BTreeMap<u64, DrawnRun>,
    /// The newest revision a frame drew whole.
    whole: u64,
}

static LEDGER: Leaf<Ledger> = Leaf::new(Ledger::new());

impl Ledger {
    const fn new() -> Self {
        Ledger { newest_edit: None, models: BTreeMap::new(), inputs: BTreeMap::new(), viewed: Vec::new(), previews: BTreeMap::new(), runs: BTreeMap::new(), whole: 0 }
    }

    fn drawn_tiles(&self, revision: u64, from: usize) -> (Vec<DrawnTile>, usize) {
        let Some(tiles) = self.runs.get(&revision).and_then(|run| run.tiles.get(from..)) else {
            return (Vec::new(), 0);
        };
        let count = tiles.len().min(DRAWN_PAGE);
        (tiles[..count].to_vec(), tiles.len() - count)
    }

    fn model_built(&mut self, plate: u64, asked: std::time::Instant, model_ms: f64) {
        // The final model may finish after pointer release, and a step's input may go to the
        // engine after it: kept until the engine starts the revision that answers it.
        self.models.retain(|_, t| t.asked >= asked);
        self.models.insert(plate, EditTiming { asked, model_ms });
    }

    /// When the edit that built `plate`'s model asked for it.
    fn edit_of(&self, plate: u64) -> Option<std::time::Instant> {
        self.models.get(&plate).map(|t| t.asked)
    }

    /// `plate` went to the engine as `revision`: it answers the edit that built the plate's model.
    fn sent(&mut self, revision: u64, plate: u64) {
        if let Some(t) = self.models.get(&plate) {
            self.inputs.insert(revision, t.clone());
        }
    }

    /// The engine began `revision`, of `plate`: an older revision plans nothing more, and the
    /// model's timing is spent once this revision answers it (an input of the plate sent again
    /// without a new model answers no edit).
    fn started(&mut self, revision: u64, plate: u64) {
        self.inputs.retain(|r, _| *r >= revision);
        let asked = self.inputs.get(&revision).map(|t| t.asked);
        if let Some(asked) = asked.filter(|a| self.models.get(&plate).is_some_and(|m| m.asked == *a)) {
            self.models.remove(&plate);
            self.models.retain(|_, m| m.asked > asked);
        }
    }

    /// The engine planned `revision`'s viewed layer `layer` of `part` (kept when an edit times it:
    /// a plate sent with no edit, as the headset's view sends it, has no line).
    fn viewed(&mut self, revision: u64, part: usize, layer: u32, field_ms: f64, slice_ms: f64, plan_ms: f64) {
        if let Some(edit) = self.inputs.get(&revision).cloned() {
            self.viewed.push(ViewedTiming { revision, part, layer, edit, field_ms, slice_ms, plan_ms, published: std::time::Instant::now(), preview: None, tile: None });
        }
    }

    /// Live preview `id` of `revision` went to the views: the layers each part `has`, the tiles
    /// `shown` (part and layer to tile), the newest edit then and the tiles it `added`.
    fn published(&mut self, id: u64, revision: u64, has: impl Fn(usize, u32) -> bool, shown: &BTreeMap<(usize, u32), u64>, edited: Option<std::time::Instant>, added: Vec<ShownTile>) {
        let focus = self.viewed.iter_mut().filter_map(|v| {
            let at = shown.get(&(v.part, v.layer)).copied();
            // its revision's own layer, or a later revision's preview that kept its tile
            let kept = v.tile.is_some() && at == v.tile;
            if !kept && (v.revision != revision || !has(v.part, v.layer)) { return None; }
            v.preview.get_or_insert(id);
            v.tile = v.tile.or(at);
            Some((v.revision, v.part, v.layer))
        }).collect();
        self.previews.insert(id, DrawnPreview { edited, tiles: added, focus, revision: Some(revision), whole: false });
        while self.previews.len() > 64 {
            self.previews.pop_first();
        }
    }

    /// Live preview `id` holds its revision's whole slice.
    fn whole(&mut self, id: u64) {
        if let Some(p) = self.previews.get_mut(&id) {
            p.whole = true;
        }
    }

    /// The Print editor's slice went to the views as preview `id`, with the live tiles `shown`
    /// then.
    fn settled(&mut self, id: u64, shown: &BTreeMap<(usize, u32), u64>) {
        // The slice is of the content the live view shows: a viewed layer whose tile is on show
        // there is in it. Its frame draws no live tile.
        let focus = self.viewed.iter_mut().filter(|v| v.tile.is_some() && shown.get(&(v.part, v.layer)).copied() == v.tile).map(|v| {
            v.preview.get_or_insert(id);
            (v.revision, v.part, v.layer)
        }).collect();
        self.previews.insert(id, DrawnPreview { edited: None, tiles: Vec::new(), focus, revision: None, whole: false });
        while self.previews.len() > 64 {
            self.previews.pop_first();
        }
    }

    /// A frame drew preview `id` for the first time: the ledger's lines for it.
    fn drawn(&mut self, id: u64) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.previews.contains_key(&id) {
            return lines;
        }
        // Previews published since the last drawn one were never on screen: their new tiles
        // show first in this frame.
        let newer = self.previews.split_off(&(id + 1));
        let previews: Vec<DrawnPreview> = std::mem::replace(&mut self.previews, newer).into_values().collect();
        let (completed, pending): (Vec<_>, Vec<_>) = self.viewed.drain(..).partition(|v| v.preview.is_some_and(|p| p <= id));
        self.viewed = pending;
        for v in completed {
            if previews.last().is_some_and(|p| p.focus.contains(&(v.revision, v.part, v.layer))) {
                let total_ms = v.edit.asked.elapsed().as_secs_f64() * 1e3;
                let upload_draw_ms = v.published.elapsed().as_secs_f64() * 1e3;
                let wait_ms = (total_ms - v.edit.model_ms - v.field_ms - v.slice_ms - v.plan_ms - upload_draw_ms).max(0.0);
                lines.push(format!("fab: viewed layer revision={} part={} layer={} total_ms={total_ms:.1} model_ms={:.1} field_ms={:.1} slice_ms={:.1} plan_ms={:.1} upload_draw_ms={upload_draw_ms:.1} wait_ms={wait_ms:.1}",
                    v.revision, v.part, v.layer, v.edit.model_ms, v.field_ms, v.slice_ms, v.plan_ms));
            }
        }
        // The Print editor's slice follows every run: they are over.
        let Some(edited) = previews.last().and_then(|p| p.edited) else {
            lines.extend(self.runs.iter_mut().filter_map(|(r, run)| run.line(*r)));
            return lines;
        };
        let ms = edited.elapsed().as_secs_f64() * 1e3;
        self.whole = previews.iter().filter(|p| p.whole).filter_map(|p| p.revision).fold(self.whole, u64::max);
        let revisions: std::collections::BTreeSet<u64> = previews.iter().filter_map(|p| p.revision).collect();
        for r in revisions {
            self.runs.entry(r).or_default().frames += 1;
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &previews {
            let Some(revision) = p.revision else { continue };
            let run = self.runs.entry(revision).or_default();
            for tile in p.tiles.iter().filter(|t| seen.insert((t.part, t.layer))) {
                if tile.ghost {
                    run.ghosts += 1;
                    continue;
                }
                run.before_whole += usize::from(tile.revision == revision && self.whole < revision);
                run.tiles.push(DrawnTile { part: tile.part, layer: tile.layer, revision: tile.revision, ms });
            }
        }
        // A run is over once its whole slice or a later one's is on screen; what drops off the end
        // says what it drew all the same.
        lines.extend(self.runs.range_mut(..=self.whole).filter_map(|(r, run)| run.line(*r)));
        while self.runs.len() > DRAWN_RUNS {
            if let Some((r, mut run)) = self.runs.pop_first() {
                lines.extend(run.line(r));
            }
        }
        lines
    }
}

/// What frames drew of the last revisions, oldest first ([`DrawnRun`]).
pub fn drawn_runs() -> Vec<(u64, DrawnRun)> {
    LEDGER.lock().runs.iter().map(|(r, run)| (*r, run.clone())).collect()
}

/// How many tiles one reply of the slice tool tells: its reply is cut at 16 KiB.
pub const DRAWN_PAGE: usize = 256;

/// Revision `revision`'s tiles from the `from`th, a page of them, and how many are left after it.
pub fn drawn_tiles(revision: u64, from: usize) -> (Vec<DrawnTile>, usize) {
    LEDGER.lock().drawn_tiles(revision, from)
}

/// The newest Print slice answer the engine gave: (generation, result, seconds). The Print
/// editor's status is reconciled from it, so an answer is never lost on the way to the UI.
static SLICED: Shot<(u64, Result<Arc<crate::print::Sliced>, String>, f64, std::time::Instant)> = Shot::new();

/// The newest Print slice answer (see [`SLICED`]), once it is older than any action still on its
/// way to the UI.
pub fn latest_sliced() -> Option<(u64, Result<Arc<crate::print::Sliced>, String>, f64)> {
    let s = SLICED.get()?;
    let (gen, result, seconds, at) = &*s;
    (at.elapsed().as_secs_f64() > 0.5).then(|| (*gen, result.clone(), *seconds))
}

/// The first frame that draws a published preview, rather than its arrival at the sink.
fn drawn(id: u64) {
    let lines = LEDGER.lock().drawn(id);
    for line in lines {
        log!("{line}");
    }
}

/// The revision of the newest complete plate toolpaths, for consumers that judge the sliced
/// result ([`toolpaths_of`] has them).
pub fn latest_toolpaths_revision() -> Option<u64> {
    hub().toolpaths.get().map(|s| s.revision)
}

/// The complete toolpaths still belonging to the live input.
pub fn latest_toolpaths_key() -> Option<(u64, u64)> {
    let h = hub();
    let (key, revision) = h.toolpaths.get().and_then(|s| s.key.map(|key| (key, s.revision)))?;
    (h.engine.input_revision(key.0, key.1) == Some(revision)).then_some(key)
}

/// A plate's toolpaths as the hub keeps them.
#[derive(Clone)]
pub struct Sliced {
    pub revision: u64,
    pub at: i64,
    pub key: Option<(u64, u64)>,
    pub toolpaths: Arc<makepad_fab_slice::live::PlatePaths>,
}

/// The Print editor's settings (Fab defaults until it has some).
pub fn print_setup() -> CheckSetup {
    hub().setup.lock().as_ref().map(|(_, s)| s.clone()).unwrap_or_else(CheckSetup::fab_defaults)
}

/// The Print editor's settings by their id (none until it has some): a change is another input.
pub fn print_setup_id() -> Option<u64> {
    hub().setup.lock().as_ref().map(|(id, _)| *id)
}

/// What a live slice of `ev` at the Print editor's settings is made from: its plate id and
/// settings id (reads the settings' text: not every frame).
pub fn slice_key(ev: &Evaluated) -> (u64, u64) {
    (plate_id(ev), print_setup().id_for(&ev.plate))
}

/// The newest plan while it is the slice of the input `key` ([`slice_key`]).
pub fn toolpaths_of(key: (u64, u64)) -> Option<Sliced> {
    hub().toolpaths.get().filter(|s| s.key == Some(key)).map(|s| Sliced::clone(&s))
}

/// The AI session reads the same committed slice as the Slice view and Mechanics.
pub struct AiSlices {
    setup: CheckSetup,
}

impl AiSlices {
    pub fn new(setup: CheckSetup) -> AiSlices {
        AiSlices { setup }
    }
}

impl fab_ai::supports::SliceSource for AiSlices {
    fn input(&self, ev: &Evaluated) -> Result<(LiveInput, String), String> {
        let input = input_for(plate_id(ev), &self.setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields));
        let name = format!("the app's print setup: {} / {}", self.setup.calibration_names().0, self.setup.process());
        Ok((input, name))
    }

    fn sliced(&self, input: &LiveInput, cancelled: Option<&dyn Fn() -> bool>) -> Result<Arc<makepad_fab_slice::live::PlatePaths>, String> {
        let key = (input.plate_id, input.settings_id);
        // The toolpaths, with what their supports tell (it follows them).
        let told = |s: &Sliced| s.toolpaths.plans().iter().all(|(plan, _)| plan.skeleton.told().is_some());
        if let Some(sliced) = toolpaths_of(key).filter(told) {
            return Ok(sliced.toolpaths);
        }
        let wait = AiWait::start();
        let h = hub();
        let cancelled = || cancelled.is_some_and(|cancelled| cancelled());
        // This waits on the model worker, and an edit queued there behind it holds the slice
        // (its shape changes): neither these toolpaths nor what their supports tell come before
        // the edit has run, so a hold ends the wait.
        let another = || "the app now slices another version of the plate (an edit or a drag came first): ask again".to_string();
        // Only the model worker waits, leaving its pool permit to the slice workers. Nothing it
        // runs moves on meanwhile, so the wait ends whenever these toolpaths cannot come: the main
        // loop slices nothing, slices another input, or holds for a shape change.
        // What the wait has seen (`AiWaits::ready`), read before it looks.
        let mut seen = h.ai.lock().ready;
        let sliced = without_permit(|| loop {
            if cancelled() {
                return Err("cancelled".to_string());
            }
            if h.main_version.stale() {
                return Err(another());
            }
            if let Some(sliced) = toolpaths_of(key) {
                return Ok(sliced);
            }
            let sliced_here = h.engine.input_revision(key.0, key.1).is_some();
            let ai = h.ai.lock();
            if let Some((_, error)) = ai.failure.as_ref().filter(|(failed, _)| *failed == key) {
                return Err(format!("the plate does not slice: {error}"));
            }
            if ai.answered >= wait.asked {
                if let Some(why) = &ai.refused {
                    return Err(format!("the plate is not sliced: {why}"));
                }
                if !sliced_here {
                    return Err(another());
                }
            }
            seen = if ai.ready == seen { ai.wait_timeout(&h.slice_ready, std::time::Duration::from_millis(100)).ready } else { ai.ready };
        })?;
        // What their supports tell: the live slice measures it once the toolpaths are out, and a
        // hold stops that measure. One it stopped is measured here (one measure at a time: this
        // waits for the slice's own to end), and stops at a hold too.
        let stop = || h.main_version.stale() || latest_toolpaths_key() != Some(key);
        for (plan, _) in sliced.toolpaths.plans() {
            if cancelled() {
                return Err("cancelled".to_string());
            }
            plan.skeleton.tell(makepad_fab_toolpath::par::Ctl { cancel: Some(&stop), ..Default::default() }).map_err(|_| another())?;
        }
        drop(wait);
        Ok(sliced.toolpaths)
    }

    fn made(&self, ev: &Evaluated) -> Option<Arc<makepad_fab_slice::live::PlatePaths>> {
        toolpaths_of((plate_id(ev), self.setup.id_for(&ev.plate))).map(|s| s.toolpaths)
    }
}

/// What the waits for the plate's toolpaths hear besides them.
#[derive(Default)]
struct AiWaits {
    /// A failed slice's key and why.
    failure: Option<((u64, u64), String)>,
    /// Waits started, and how many of them the main loop has seen: it slices (or refuses to)
    /// what each asked for by then.
    asked: u64,
    answered: u64,
    /// Why the main loop slices nothing now.
    refused: Option<String>,
    /// Counts what a wait may be waiting for: toolpaths kept, a failure, an answer. A wait reads it
    /// before it looks (with this lock let go) and sleeps only while it has not moved.
    ready: u64,
}

/// An AI wait asks the main loop to start and stop slicing just as Mechanics does.
struct AiWait {
    asked: u64,
}

impl AiWait {
    fn start() -> Self {
        let h = hub();
        h.ai_waits.fetch_add(1, Ordering::Relaxed);
        let asked = {
            let mut ai = h.ai.lock();
            ai.asked += 1;
            ai.asked
        };
        Cx::post_action(LiveAction::AiWait);
        AiWait { asked }
    }
}

/// The main loop has followed the AI waits so far: it slices the plate it holds, or says why not.
pub fn ai_answer(refused: Option<String>) {
    let h = hub();
    let mut ai = h.ai.lock();
    ai.answered = ai.asked;
    ai.refused = refused;
    ai.ready += 1;
    drop(ai);
    h.slice_ready.notify_all();
}

impl Drop for AiWait {
    fn drop(&mut self) {
        hub().ai_waits.fetch_sub(1, Ordering::Relaxed);
        Cx::post_action(LiveAction::AiWait);
    }
}

pub fn ai_waiting() -> bool {
    hub().ai_waits.load(Ordering::Relaxed) > 0
}

/// Focus: the layer height the viewer looks at slices first.
pub fn set_focus_z(z: Option<f64>) {
    hub().engine.set_focus_z(z);
}

/// Tests that drive the hub (its live state, view and engines are the process's) run one at a time.
#[cfg(test)]
pub(crate) static HUB_TESTS: Mutex<()> = Mutex::new(());

/// Test hook: places where a test stops a thread it marked, and lets it on.
#[cfg(test)]
pub(crate) mod gates {
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Mutex;

    /// An armed place: where, who hears the arrival, and the release the thread waits for.
    type Gate = (&'static str, Sender<()>, Receiver<()>);

    static ARMED: Mutex<Vec<Gate>> = Mutex::new(Vec::new());

    thread_local! {
        static MARKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The next marked thread to reach `at` stops there: the first receiver hears it come, the
    /// sender lets it on (dropping it does too).
    pub(crate) fn arm(at: &'static str) -> (Receiver<()>, Sender<()>) {
        let (came, arrived) = channel();
        let (go, wait) = channel();
        ARMED.lock().unwrap().push((at, came, wait));
        (arrived, go)
    }

    /// This thread stops at the armed places (no other thread does).
    pub(crate) fn mark() {
        MARKED.with(|m| m.set(true));
    }

    /// A thread at `at`: it waits there when it is marked and a test armed the place.
    pub(crate) fn reach(at: &'static str) {
        if !MARKED.with(std::cell::Cell::get) {
            return;
        }
        let gate = {
            let mut armed = ARMED.lock().unwrap();
            armed.iter().position(|g| g.0 == at).map(|k| armed.remove(k))
        };
        if let Some((_, came, wait)) = gate {
            let _ = came.send(());
            let _ = wait.recv();
        }
    }
}

/// Test hook: every event the app's sink gets, with when it got it (and a streamed
/// preview, as the Print editor gets it).
#[cfg(test)]
pub(crate) static OBSERVE: Mutex<Option<std::sync::mpsc::Sender<(std::time::Instant, String, Option<Arc<PrintPreview>>, Option<Arc<LiveResult>>)>>> = Mutex::new(None);

/// Test counters: preview rebuilds and their summed time (ns).
#[cfg(test)]
pub(crate) static REBUILDS: [std::sync::atomic::AtomicU64; 2] = [std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0)];

fn on_event(check: bool, e: LiveEvent) {
    let h = hub();
    #[cfg(test)]
    if !check {
        let what = match &e {
            LiveEvent::Progress { progress, stage, .. } => format!("progress {progress} {stage}"),
            LiveEvent::Settled { result, seconds, .. } => format!("settled {} {seconds}", result.is_ok()),
            LiveEvent::PartLayers { layers, .. } => format!("layers {}", layers.len()),
            LiveEvent::PartCut { .. } => "cut".into(),
            LiveEvent::Failed { error, .. } => format!("failed {error}"),
            _ => String::new(),
        };
        if let Some(tx) = OBSERVE.lock().unwrap().as_ref() {
            let _ = tx.send((std::time::Instant::now(), what, None, None));
        }
    }
    match e {
        LiveEvent::Progress { tag, progress, stage } => {
            if !check {
                Cx::post_action(PrintEvent::Progress { gen: tag, progress: progress * 0.9, stage });
            }
        }
        LiveEvent::Running { revision, progress, stage } => {
            if !check {
                Cx::post_action(PrintEvent::Running { revision, progress, stage });
            }
        }
        LiveEvent::Settled { tag, revision, result, seconds } => {
            let key = if check { tag | 1 << 63 } else { tag };
            if check {
                // An older check whose report stream was cut short by this
                // one: answered with what it had.
                let cut_short = std::mem::take(&mut *h.pending_checks.lock());
                for (_, id, report) in cut_short {
                    Cx::post_action(PrintEvent::Checked { id, report, error: None });
                }
            }
            let job = h.jobs.lock().remove(&key);
            if job.as_ref().is_some_and(|j| j.check.is_none()) {
                h.print_jobs.fetch_sub(1, Ordering::Relaxed);
            }
            match job {
                None => drop(result),
                Some(job) => match job.check {
                    Some(id) => {
                        // A check's preview is not shown: its beads stay on the plate.
                        let sliced = result.map(|r| crate::print::finish_sliced(&job, &r, Arc::from([])));
                        observe_sliced(sliced.is_ok());
                        match sliced {
                            // The sliced report follows as `Issues`.
                            Ok(s) => h.pending_checks.lock().push((revision, id, s.preview.printability.clone())),
                            Err(e) => Cx::post_action(PrintEvent::Checked { id, report: job.fallback.as_ref().map(|f| f()).unwrap_or_default(), error: Some(e) }),
                        }
                    }
                    None => {
                        // The live state's owner answers the Print editor ([`settled`]). The answer
                        // to a settle an input call superseded comes on that call's thread (the
                        // UI's), which only posts.
                        let ok = result.is_ok();
                        let msg = Msg::Settled { revision, job, result, seconds };
                        if ok { post_live(msg) } else { h.owner.post(msg) }
                    }
                },
            }
            after_heavy_job();
        }
        LiveEvent::Estimated { revision, estimate, streamed } => {
            // The time model's run: its records made the preview as they came to their taker.
            if streamed {
                return;
            }
            // A kept print time: its records follow once the time model has run again.
            log!("fab live estimate: {:.0} s print time kept, its records follow", estimate.stats.total_time);
            let s = &estimate.stats;
            let totals = fab::print::EstimateTotals { seconds: s.total_time, prepare_seconds: s.prepare_time, grams: s.filament_g.iter().sum() };
            Cx::post_action(PrintEvent::Totals { revision, totals });
        }
        LiveEvent::Started { revision, plate_id } => {
            if !check {
                LEDGER.lock().started(revision, plate_id);
            }
        }
        LiveEvent::Viewed { revision, part, layer, field_ms, slice_ms, plan_ms } => {
            if !check {
                LEDGER.lock().viewed(revision, part, layer, field_ms, slice_ms, plan_ms);
            }
        }
        LiveEvent::Stopped { ms } => log!("fab live cancel: shape change to slice idle {ms:.1} ms ({})", if check { "checks" } else { "view" }),
        LiveEvent::Failed { revision, error } => {
            log!("fab: live slice: {error}");
            if check {
                let failed = {
                    let mut pending = h.pending_checks.lock();
                    pending.iter().position(|e| e.0 == revision).map(|k| pending.remove(k))
                };
                if let Some((_, id, report)) = failed {
                    Cx::post_action(PrintEvent::Checked { id, report, error: Some(error) });
                }
            } else {
                if let Some(key) = h.engine.key_of(revision) {
                    let mut ai = h.ai.lock();
                    ai.failure = Some((key, error.clone()));
                    ai.ready += 1;
                }
                h.slice_ready.notify_all();
                post_live(Msg::Failed { revision, error });
            }
        }
        LiveEvent::Issues { revision, report, done, total } if check => {
            if done == total {
                let checked = {
                    let mut pending = h.pending_checks.lock();
                    pending.iter().position(|e| e.0 == revision).map(|k| pending.remove(k))
                };
                if let Some((_, id, _)) = checked {
                    Cx::post_action(PrintEvent::Checked { id, report, error: None });
                }
            }
        }
        _ if check => {}
        LiveEvent::Contours { revision, parts, objects, age } => {
            stats("contours", age);
            post_live(Msg::Part(LiveEvent::Contours { revision, parts, objects, age }));
        }
        LiveEvent::Issues { revision, report, done, total } => post_live(Msg::Issues { revision, report, done, total }),
        LiveEvent::Toolpaths { revision, parts, age, toolpaths } => {
            // The replan is complete.
            after_heavy_job();
            let key = h.engine.key_of(revision);
            let at = h.toolpaths.get().filter(|s| Arc::ptr_eq(&s.toolpaths, &toolpaths)).map_or_else(makepad_civil_time::now_secs, |s| s.at);
            drop(h.toolpaths.set(Arc::new(Sliced { revision, at, key, toolpaths })));
            {
                // After the toolpaths are kept: a wait that looked before them sees `ready` move.
                let mut ai = h.ai.lock();
                if ai.failure.as_ref().is_some_and(|(failed, _)| Some(*failed) == key) {
                    ai.failure = None;
                }
                ai.ready += 1;
            }
            h.slice_ready.notify_all();
            Cx::post_action(LiveAction::Toolpaths { revision });
            // What each part cost since the last whole slice (an untouched part: nothing).
            static WORK: Leaf<Vec<(u64, u64, f64, u64)>> = Leaf::new(Vec::new());
            let kept = h.engine.part_kept();
            let now: Vec<(u64, u64, f64, u64)> = h.engine.part_work().into_iter().zip(h.engine.part_plan_ms()).zip(&kept).map(|(((t, p), ms), k)| (t, p, ms, k.0)).collect();
            let was = std::mem::replace(&mut *WORK.lock(), now.clone());
            for (i, w) in now.iter().enumerate() {
                let (t, p, ms, k) = was.get(i).copied().unwrap_or_default();
                // an edit that planned layers again and kept none of the last plan's says why
                let none = kept.get(i).map(|k| k.1.as_str()).filter(|why| w.3 == k && w.1 > p && !why.is_empty()).map_or(String::new(), |why| format!(" (kept none: {why})"));
                log!("fab live work: part {i} traced {} planned {} kept {}{none} plan_ms={:.1} revision={revision}", w.0 - t, w.1 - p, w.3 - k, w.2 - ms);
            }
            let [events, sent, publishing, publications] = SINK.each_ref().map(|s| s.swap(0, Ordering::Relaxed));
            log!("fab live sink: {events} part events, {:.0} ms of their senders' time, {publications} publications in {:.0} ms, revision={revision}", sent as f64 / 1e6, publishing as f64 / 1e6);
            stats("toolpaths", age);
            post_live(Msg::Toolpaths { revision, parts });
        }
        e => post_live(Msg::Part(e)),
    }
}

/// Test hook: the Print editor's slice (or a check's) is answered, `ok` or not.
fn observe_sliced(ok: bool) {
    #[cfg(test)]
    if let Some(tx) = OBSERVE.lock().unwrap().as_ref() {
        let _ = tx.send((std::time::Instant::now(), format!("sliced {ok}"), None, None));
    }
    #[cfg(not(test))]
    let _ = ok;
}

/// A message on the live state, by its owner ([`LiveOwner`]).
fn handle(l: &mut LiveState, msg: Msg) {
    match msg {
        Msg::Part(e) => {
            let contours = matches!(e, LiveEvent::Contours { .. });
            apply_queued(l, e);
            if contours {
                share_plate(l);
            }
        }
        Msg::Issues { revision, report, done, total } => issues(l, revision, report, done, total),
        Msg::Toolpaths { revision, parts } => {
            if revision < l.revision {
                return;
            }
            l.placement = parts;
            l.revision = revision;
            l.complete = true;
            share_plate(l);
            // the whole plate's tiles, whatever a view took: with what was held for one
            l.unpublished.tiles = true;
            publish_applied(l);
        }
        Msg::Failed { revision, error } => {
            // A failure of an older revision is that run's: the newer one stands.
            if revision < l.revision {
                return;
            }
            // Part streams can have been published before a worker panicked. The retry may have
            // the same content keys, so discard their partial layers as well as the engine caches.
            *l = LiveState { revision, print_job: l.print_job.take(), ..Default::default() };
            // The views stay started: their joined contours (empty), which a view's start made
            // and every publication waits for, so the next revision's slice is shown.
            refresh_contours(l);
            share_plate(l);
            let h = hub();
            drop(h.latest.take());
            drop(h.toolpaths.take());
            Cx::post_action(LiveAction::Failed { revision, error });
        }
        Msg::PrintJob { gen, machine, colours } => l.print_job = Some((gen, machine, colours)),
        Msg::Settled { revision, job, result, seconds } => settled(l, revision, job, result, seconds),
        Msg::Sliced { revision, gen, sliced, beads, insert_issues, seconds } => {
            // A report may not have streamed yet. Deliver the insert findings after the
            // preview even then, with the ordinary checks still pending.
            let initial = sliced.as_ref().ok().filter(|_| !insert_issues.is_empty())
                .map(|s| (s.preview.printability.clone(), 0, s.preview.layers.len() as u32));
            if let Ok(s) = &sliced {
                // What the views showed when it came, for the drawn-layer timings.
                LEDGER.lock().settled(s.preview.id, &l.shown);
                let adopted = beads.as_ref().map_or(0, |beads| adopt(l, revision, beads));
                #[cfg(test)]
                if let Some(tx) = OBSERVE.lock().unwrap().as_ref() {
                    let _ = tx.send((std::time::Instant::now(), format!("beads adopted {adopted}"), None, None));
                }
                #[cfg(not(test))]
                let _ = adopted;
            }
            let made = sliced.is_ok();
            print_sliced(l, gen, sliced, seconds);
            // Its checks so far, after its preview: from here on they go to the Print editor as
            // they come.
            if let Some((revision, gen, held)) = l.settling.take_if(|s| s.0 == revision && s.1 == gen) {
                if made {
                    l.settled = Some((revision, gen));
                    l.insert_issues = Some((revision, insert_issues));
                    if let Some((report, done, total)) = held.or(initial) {
                        issues(l, revision, report, done, total);
                    }
                }
            }
        }
        Msg::Adopt { revision, beads } => {
            adopt(l, revision, &beads);
        }
        Msg::StartView => {
            refresh_contours(l);
            l.unpublished.tiles = true;
            publish_applied(l);
        }
        Msg::Release => release_preview(l),
        Msg::Took => {}
        #[cfg(test)]
        Msg::Panic => panic!("a test's panic in the live state's owner"),
    }
}

/// The slice's checks of `revision` so far: to the Print editor when they are its slice's, and
/// into the live state (published to the views) unless a newer revision stands.
fn issues(l: &mut LiveState, revision: u64, report: Arc<fab_printability::Report>, done: u32, total: u32) {
    // The Print editor's slice: its sliced checks, as they stream; the newest waits while its
    // preview is being made. Hold the unmerged report so replaying it cannot double-count.
    if let Some(s) = l.settling.as_mut().filter(|s| s.0 == revision) {
        s.2 = Some((report.clone(), done, total));
    }
    let report = if let Some((_, inserts)) = l.insert_issues.as_ref().filter(|s| s.0 == revision && !s.1.is_empty()) {
        let mut report = (*report).clone();
        report.issues.extend(inserts.iter().cloned());
        report.sort();
        Arc::new(report)
    } else {
        report
    };
    if let Some((_, gen)) = l.settled.filter(|s| s.0 == revision) {
        Cx::post_action(PrintEvent::Issues { gen, report: report.clone(), done, total });
    }
    if revision < l.revision {
        return;
    }
    // The sliced checks whole: they read what the supports tell, so it is in.
    if done == total && total > 0 {
        log!("fab live checked: revision={revision}");
    }
    log_kinds(&report);
    let markers = issue_markers(&report);
    l.report = Some((report, markers));
    l.checking = (done, total);
    // Kept for a view that starts later ([`LiveSlice::start`]).
    if !live_views() {
        return;
    }
    let stage = if hub().released.load(Ordering::Relaxed) { LiveStage::Refined } else { LiveStage::Issues };
    publish(l, stage);
}

/// The Print editor's slice settled. Its preview is made on the pool, its parts' beads in their
/// frames when the live state is at its revision, and comes back as [`Msg::Sliced`]: the owner
/// goes on meanwhile. Its printability streams in as `Issues` of its revision, and goes to the
/// Print editor after its preview ([`LiveState::settling`]).
fn settled(l: &mut LiveState, revision: u64, job: PrintJob, result: Result<Arc<makepad_fab_slice::live::Settled>, String>, seconds: f64) {
    if l.print_job.as_ref().is_some_and(|p| p.0 == job.gen) {
        l.print_job = None;
    }
    if result.is_ok() {
        l.settled = None;
        l.settling = Some((revision, job.gen, None));
        l.insert_issues = None;
    }
    // A slice of a plate that moved after the request is not the requested one (the move's
    // commit asks again); never hand it to Print or Export as that.
    let result = result.and_then(|r| if r.transformed { Err("a part moved while the plate was sliced".to_string()) } else { Ok(r) });
    let r = match result {
        Ok(r) => r,
        Err(e) => {
            observe_sliced(false);
            print_sliced(l, job.gen, Err(e), seconds);
            return;
        }
    };
    let offsets = if l.revision == revision { offsets(l) } else { Arc::from([]) };
    let insert_plate = hub().engine.key_of(revision)
        .and_then(|(id, _)| hub().insert_plate.get().filter(|p| p.id == id));
    run_slice_work(Class::Visible, "fab sliced", move || {
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // O(insert triangles + layers spanned x section edges + moves inside the inserts'
            // boxes), once per settled slice on the slice worker, never per frame or per edit.
            let insert_issues = if let Some(p) = insert_plate.as_ref().filter(|p| !p.inserts.is_empty()) {
                let inserts: Vec<_> = p.inserts.iter().filter_map(|&i| {
                    let excluded = &p.evaluated.plate.excluded[i];
                    let insert = excluded.insert.as_ref()?;
                    // As in Plate::inserts, only inserts with a printed host are checked.
                    p.evaluated.plate.parts.iter().any(|host| host.object == insert.into).then_some((&excluded.part, insert))
                }).collect();
                // The settled timeline's moves have been packed into its layer chunks.
                fab_printability::insert::bead_issues(&inserts, &mut r.layers.records())
            } else {
                Vec::new()
            };
            (crate::print::finish_sliced(&job, &r, offsets), insert_issues)
        }));
        let (sliced, insert_issues) = match made {
            Ok((sliced, inserts)) => (Ok(sliced), inserts),
            Err(p) => (Err(p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "its preview failed".into())), Vec::new()),
        };
        observe_sliced(sliced.is_ok());
        let beads = sliced.as_ref().ok().and_then(|s| slice_beads(&s.preview));
        post_live(Msg::Sliced { revision, gen: job.gen, sliced, beads, insert_issues, seconds });
    });
}

/// The Print editor's slice answered: kept as the newest ([`latest_sliced`]), its G-code to disk,
/// and the preview let go of when nothing shows it.
fn print_sliced(l: &mut LiveState, gen: u64, sliced: Result<crate::print::Sliced, String>, seconds: f64) {
    release_preview(l);
    let sliced = sliced.map(Arc::new);
    drop(SLICED.set(Arc::new((gen, sliced.clone(), seconds, std::time::Instant::now()))));
    if let Ok(s) = &sliced {
        // Its hash and the safety check have read its G-code: it goes to disk.
        spill_gcode(&s.settled);
    }
    Cx::post_action(PrintEvent::Sliced { gen, result: sliced, seconds });
}

/// What the engine's part events (a part's cut, regions, layers, supports, trees) and its
/// contours cost, since the last whole slice: the events queued, the time their senders spent in
/// the sink (ns: posting, and owning the state when it fell to them), the time publications took
/// (ns) and how many went out ([`LiveOwner`]).
static SINK: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// Counts the trees the engine sent ([`LiveEvent::PartTrees`]): each slot's are told apart by it.
static TREES_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The plate's trees to the Sliced view when a part's trees or place changed: each part's as
/// its slot has them, on the plate ([`fab::print::PlateTree`]).
fn post_trees(l: &mut LiveState) {
    let slot = |part: usize, key: u64| l.slots.get(part).filter(|s| s.key == key);
    let of: Vec<(u64, Pt)> = l.placement.iter().enumerate().map(|(part, &(key, d))| (slot(part, key).map_or(0, |s| s.trees.0), d)).collect();
    if l.trees_of == of {
        return;
    }
    let mut trees = Vec::new();
    for (part, &(key, d)) in l.placement.iter().enumerate() {
        let (Some(slot), Some(object)) = (slot(part, key), l.objects.get(part)) else { continue };
        let (dx, dy) = (d.x_mm() as f32, d.y_mm() as f32);
        trees.extend(slot.trees.1.iter().map(|t| {
            let discs = t.discs.iter().map(|c| [c.at[0] + dx, c.at[1] + dy, c.z, c.h, c.r]).collect();
            fab::print::PlateTree::new(object.clone(), t.tips.clone(), t.tip_discs.clone(), discs, &t.links)
        }));
    }
    l.trees_of = of;
    Cx::post_action(PrintEvent::Trees(trees.into()));
}

/// A part slot event of the engine on the live state (the engine sends them in order, from its
/// driver): true when the shown layers may have changed. The live view publishes them; the
/// parity guard drives this alone.
fn apply(l: &mut LiveState, e: LiveEvent) -> bool {
    match e {
        LiveEvent::Contours { revision, parts, objects, .. } => {
            l.slots.truncate(parts.len());
            l.placement = parts;
            l.objects = objects;
            if l.revision != revision { l.complete = false; }
            l.revision = revision;
        }
        LiveEvent::PartCut { part, key, layers, stale } => {
            if l.slots.len() <= part {
                l.slots.resize_with(part + 1, Slot::default);
            }
            if l.slots[part].key != key {
                l.slots[part].renew(key);
            }
            if let Some(p) = l.placement.get_mut(part) { p.0 = key; }
            let slot = &mut l.slots[part];
            slot.cut = Some(layers);
            slot.stale = stale;
            // The next publication shows this cut: its newly traced layers are built then.
            slot.contours = None;
        }
        LiveEvent::PartRegions { part, key, regions, total } => {
            if l.slots.len() <= part {
                l.slots.resize_with(part + 1, Slot::default);
            }
            let slot = &mut l.slots[part];
            if slot.key != key {
                slot.renew(key);
            }
            // A new stream of the content the slot holds (a cancelled revision resumed, or the
            // content back after another) with the same layers keeps them: they are its plan,
            // and the stream's same records keep them shown. Another inventory starts over.
            if (slot.regions, slot.total) != (regions, total) {
                if slot.complete() {
                    let mut was = std::mem::take(slot);
                    let cut = was.cut.clone();
                    let stale = was.stale.clone();
                    let tiles = std::mem::take(&mut was.tiles);
                    let layer_tiles = std::mem::take(&mut was.layer_tiles);
                    let beads = std::mem::take(&mut was.beads);
                    let beads_at = was.beads_at;
                    let contours = was.contours.take();
                    was.good = None;
                    was.newer.clear();
                    *slot = Slot { key, revision: was.revision, cut, stale, tiles, layer_tiles, beads, beads_at, contours, good: Some(Box::new(was)), ..Default::default() };
                } else if slot.top() > 0 {
                    // The layers this content's cancelled stream drew stay shown until the new
                    // stream draws them.
                    let layers = std::mem::take(&mut slot.layers);
                    let by = std::mem::take(&mut slot.by);
                    let ghost = Slot { key, revision: slot.revision, regions: slot.regions, total: slot.total, layers, by, cut: slot.cut.clone(), ..Default::default() };
                    Slot::supersede(&mut slot.newer, ghost);
                }
                slot.layers.clear();
                slot.by.clear();
                slot.supports.clear();
                slot.supports_done = false;
                slot.trees = Default::default();
            }
            slot.regions = regions;
            slot.total = total;
            if let Some(p) = l.placement.get_mut(part) { p.0 = key; }
        }
        LiveEvent::PartLayers { revision, pass, part, key, region, layers, total } => {
            if l.slots.len() <= part {
                l.slots.resize_with(part + 1, Slot::default);
            }
            if older(&l.slots[part], key, revision, part) { return false; }
            let slot = &mut l.slots[part];
            if slot.key != key {
                slot.renew(key);
            }
            slot.revision = slot.revision.max(revision);
            slot.total = total;
            let by = PlannedBy { revision, pass: pass.name() };
            for (index, mut paths) in layers {
                // This content's plan of the layer as shown already, path for path, stays the
                // shown one: a revision that plans it again draws and uploads nothing new. It
                // holds the newer plan's islands (the engine's), the older ones go.
                let at = (index, region);
                let by = match slot.layers.get(&at).filter(|old| old.same_paths(&paths)) {
                    Some(old) => {
                        paths.keep_identity(old);
                        slot.by.get(&at).copied().unwrap_or(by)
                    }
                    None => by,
                };
                let paths = Arc::new(paths);
                check_replan(slot, part, index, region, &paths, by);
                slot.layers.insert((index, region), paths);
                slot.by.insert((index, region), by);
            }
            if let Some(p) = l.placement.get_mut(part) { p.0 = key; }
        }
        LiveEvent::Settings => {
            // Other settings: other layers. Nothing of the old ones is shown for the new, so the
            // plate is no longer whole (this comes ahead of the new revision's contours).
            l.complete = false;
            for slot in &mut l.slots {
                slot.good = None;
                slot.newer.clear();
                slot.layers.clear();
                slot.by.clear();
                slot.supports.clear();
                slot.supports_done = false;
                slot.trees = Default::default();
                slot.regions = 0;
            }
        }
        LiveEvent::PartTrees { part, key, trees } => {
            let Some(slot) = l.slots.get_mut(part).filter(|s| s.key == key) else { return false };
            slot.trees = (TREES_SENT.fetch_add(1, Ordering::Relaxed) + 1, trees);
            return false;
        }
        LiveEvent::PartSupports { part, key, layers, done } => {
            let Some(slot) = l.slots.get_mut(part).filter(|s| s.key == key) else { return false };
            if done {
                slot.supports = layers.into_iter().map(|(i, paths)| (i, Arc::new(paths))).collect();
            } else if layers.is_empty() {
                // A plan of other supports begins on the part's layers as they are.
                slot.supports.clear();
            } else {
                slot.supports.extend(layers.into_iter().map(|(i, paths)| (i, Arc::new(paths))));
            }
            slot.supports_done = done;
        }
        _ => return false,
    }
    // A complete content shows no layer of the last one: its ghost goes now, not at the next
    // edit (until then it kept a second slice of the part's records).
    for slot in l.slots.iter_mut().filter(|s| (s.good.is_some() || !s.newer.is_empty()) && s.complete()) {
        slot.good = None;
        slot.newer.clear();
    }
    true
}

/// A plan of another content than the slot's, from a revision older than one the slot has
/// shown: it never takes the slot back (the engine sends in order, so this is a fault to see).
fn older(slot: &Slot, key: u64, revision: u64, part: usize) -> bool {
    let older = slot.key != key && revision < slot.revision;
    if older {
        log!("fab live: part {part} dropped revision {revision}'s plan: the slot shows revision {}", slot.revision);
    }
    older
}

fn live_views() -> bool {
    hub().on.load(Ordering::Relaxed) > 0
}

/// Whoever shows the stream or waits for it: a headset's live view, a Print editor slice in
/// flight, or a check of the plate's toolpaths.
fn streaming() -> bool {
    live_views() || hub().waits.load(Ordering::Relaxed) > 0 || hub().print_jobs.load(Ordering::Relaxed) > 0
}

/// Publish independently ready layers, sharing every unchanged bead block.
fn publish_tiles(l: &mut LiveState) {
    let job = l.print_job.clone();
    if !live_views() && job.is_none() {
        return;
    }
    let v = view();
    let machine = job.as_ref().map(|j| j.1.clone()).or_else(|| v.machine.clone());
    let Some(machine) = machine else { return };
    let colours = match &job {
        Some((_, _, c)) => c.clone(),
        None => v.colours.clone(),
    };
    #[cfg(test)]
    let _t = RebuildTimer(std::time::Instant::now());
    let mut layers = BTreeMap::new();
    let mut chunks = Vec::new();
    let mut shown = BTreeMap::new();
    let mut added = Vec::new();
    let (mut done, mut total) = (0, 0);
    let mut totals = vec![0u32; l.placement.len()];
    // Per part, its lowest and highest plate layer where all it shows is one content, whole: only
    // then is it closed in on every side, its core hidden.
    let mut part_layers = vec![[f32::INFINITY; 2]; l.placement.len()];
    let plate_zs = plate_zs(l);
    let plate_layer = |z: f32| plate_zs.partition_point(|&q| q < (z * 1000.0).round() as i64) as u32;
    // Every part has a block for each block of the plate's tallest part: a part whose layer
    // count changes moves no other part's blocks (each block keeps its draw and GPU buffer).
    let blocks = l.placement.iter().enumerate()
        .filter_map(|(part, &(key, _))| l.slots.get(part).filter(|s| s.key == key))
        .map(|s| s.shown_total().div_ceil(TILE_BLOCK)).max().unwrap_or(0);
    let started = std::time::Instant::now();
    let mut built = 0;
    for (part, &(key, _)) in l.placement.iter().enumerate() {
        let Some(slot) = l.slots.get_mut(part).filter(|s| s.key == key) else { continue };
        totals[part] = slot.shown_total();
        total += slot.total;
        done += (0..slot.total).filter(|&i| slot.has(i)).count() as u32;
        let whole = slot.complete();
        let mut reach = [f32::INFINITY, f32::NEG_INFINITY];
        for tile in slot.rebuild_tiles(&machine, part, &plate_layer, blocks, &mut built) {
            reach = [reach[0].min(tile.global as f32), reach[1].max(tile.global as f32)];
            layers.entry(tile.global).or_insert(PreviewLayer { layer: tile.global, z: tile.z, t_start: 0.0, t_end: 0.0, starts: [0; 3] });
            let at = (part, tile.layer);
            shown.insert(at, tile.hash);
            if l.shown.get(&at) != Some(&tile.hash) {
                added.push(tile);
            }
        }
        if whole && reach[0] <= reach[1] {
            part_layers[part] = reach;
        }
        chunks.extend(slot.tiles.values().map(|t| LiveChunk { beads: t.1.clone(), part, runs: t.2.clone() }));
    }
    l.layers = (done, total);
    if live_views() {
        let beads = chunks.iter().map(|c| c.beads.count()).sum();
        if let Some(w) = l.tile_stats.add(started.elapsed().as_secs_f64(), built, beads) {
            log!("fab live tiles: {} publications (avg {:.2} ms, worst {:.2} ms), {} layers built, {} beads", w.n, w.avg_ms(), w.worst * 1e3, w.layers, w.beads);
        }
    }
    // Never blank: a layer shown before (new or ghost) stays shown while the part still has it.
    for &(part, layer) in l.shown.keys().filter(|at| !shown.contains_key(at)) {
        if layer < totals.get(part).copied().unwrap_or(0) {
            log!("fab live gap: part {part} layer {layer}");
        }
    }
    l.shown = shown;
    // Where each part's beads go on the plate (they are built in its frame).
    let offsets: Arc<[[f32; 2]]> = l.placement.iter().map(|(_, d)| [d.x_mm() as f32, d.y_mm() as f32]).collect();
    let mut preview = crate::print::live_preview(&machine, colours, layers.into_values().collect(), chunks, part_layers, offsets, l.objects.clone());
    if let (Some(pv), Some((report, markers))) = (Arc::get_mut(&mut preview), l.report.as_ref()) {
        pv.markers = markers.clone();
        pv.printability = report.clone();
    }
    let mut ledger = LEDGER.lock();
    let edited = ledger.newest_edit;
    ledger.published(preview.id, l.revision, |part, layer| l.slots.get(part).is_some_and(|s| s.has(layer)), &l.shown, edited, added);
    if l.complete {
        ledger.whole(preview.id);
    }
    drop(ledger);
    l.preview = Some(preview.clone());
    if let Some((gen, _, _)) = job {
        #[cfg(test)]
        if let Some(tx) = OBSERVE.lock().unwrap().as_ref() {
            let _ = tx.send((std::time::Instant::now(), format!("streamed {} {}", l.layers.0, preview.layers.len()), Some(preview.clone()), None));
        }
        Cx::post_action(PrintEvent::Streamed { gen, preview });
    }
    if live_views() {
        publish(l, LiveStage::Refined);
    }
}

/// The plate's layer heights (µm), in order: a layer's number is its height's place here. Every
/// part's layer stack is in, its cut's, shown or not, so the numbers stay put while its layers
/// stream in: a layer arriving below others renumbers none of them (each renumbered layer
/// rebuilt its block, a new GPU buffer). The settled slice numbers its layers by print z alike.
fn plate_zs(l: &LiveState) -> Vec<i64> {
    let mut zs = std::collections::BTreeSet::new();
    for (part, &(key, _)) in l.placement.iter().enumerate() {
        let Some(slot) = l.slots.get(part).filter(|s| s.key == key) else { continue };
        zs.extend((0..slot.stack_len()).filter_map(|i| slot.stack_z(i)).map(|z| (z * 1000.0).round() as i64));
    }
    zs.into_iter().collect()
}

/// The preview of revision `revision`'s print time, built as the time model makes its records
/// ([`RecordTaker`]) and played once the estimate is done. A kept print time (`kept`) plays it as
/// it grows: a growing preview goes to the view once it has taken the last ([`crate::print::TimedPreview::grown`]).
/// A newer input stops the estimate: its records end without it, and nothing more is made.
fn records_taker(revision: u64, kept: Option<Arc<Estimate>>) -> Option<RecordTaker> {
    use makepad_fab_slice::live::Streamed;
    let v = view();
    let (machine, tests, melts) = (v.machine.clone(), v.tests.clone(), v.melts.clone());
    let machine = machine?;
    // Where the parts of `revision` sit: none (the beads on the plate) once the plate is at
    // another. The estimate is asked for once a view took the whole slice of its revision, which
    // its owner published after it shared that revision's plate. A newer revision's parts come
    // with its Contours; this preview is then dropped as stale.
    let part_offsets = hub().plate.get().filter(|p| p.revision == revision).map_or_else(|| Arc::from([]), |p| p.offsets.clone());
    let mut timed = Some(crate::print::TimedPreview::new(&machine, tests.is_some(), part_offsets));
    let mut fed = std::time::Duration::ZERO;
    // The view's colours and the plate's objects, read once a preview is made.
    let mut shown: Option<(Vec<[f32; 4]>, Arc<[String]>)> = None;
    let taken = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut grew = 0;
    Some(Box::new(move |streamed| {
        let mut look = || {
            shown
                .get_or_insert_with(|| {
                    let colours = view().colours.clone();
                    #[cfg(test)]
                    gates::reach("estimate look");
                    (colours, hub().plate.get().map_or_else(|| Arc::from([]), |p| p.objects.clone()))
                })
                .clone()
        };
        match streamed {
            Streamed::Part { records: part, temps } => {
                let t = std::time::Instant::now();
                let timed = timed.as_mut().expect("the estimate is not done");
                let buffer = timed.feed(part, temps);
                if let Some(kept) = kept.as_ref().filter(|_| taken.load(Ordering::Acquire)) {
                    let (colours, objects) = look();
                    if let Some(preview) = timed.grown(kept, &colours, &objects) {
                        taken.store(false, Ordering::Release);
                        grew += 1;
                        Cx::post_action(PrintEvent::Growing { revision, preview, taken: taken.clone() });
                    }
                }
                fed += t.elapsed();
                buffer
            }
            Streamed::Done { estimate } => {
                let t0 = std::time::Instant::now();
                let (colours, objects) = look();
                let mut preview = timed.take().expect("the estimate is not done").preview(&estimate, colours, objects, tests.as_ref());
                // The heat-set inserts to melt in after the print, as the settled slice says them,
                // the iron at the print temperature of their part's filament.
                if let Some(pv) = Arc::get_mut(&mut preview).filter(|_| !melts.is_empty()) {
                    crate::print::add_melts(pv, &melts, |tool| estimate.tools.iter().position(|&t| t == tool).and_then(|k| estimate.filament_temps.get(k).copied()));
                }
                log!("fab live estimate: {:.0} s print time in {:.0} ms, its preview in {:.0} ms ({:.0} ms built as its records came, {grew} growing previews)", estimate.stats.total_time, estimate.seconds * 1e3, t0.elapsed().as_secs_f64() * 1e3, fed.as_secs_f64() * 1e3);
                Cx::post_action(PrintEvent::Estimated { revision, preview: preview.clone(), changes: estimate.changes.clone() });
                if let Some(beads) = slice_beads(&preview) {
                    post_live(Msg::Adopt { revision, beads });
                }
                None
            }
        }
    }))
}

/// A slice's beads for the live tiles ([`Slot::adopt`]): by part and plate layer, its segments
/// and their runs, and its layers' heights.
struct SliceBeads {
    beads: BTreeMap<(usize, u32), (Vec<InstanceSegment>, Arc<[PreviewBlock]>)>,
    layer_z: BTreeMap<u32, f32>,
}

/// The beads of a slice's preview for the live tiles, grouped where it was made: none when the
/// preview holds them on the plate (made while the live state was at another revision).
fn slice_beads(preview: &PrintPreview) -> Option<SliceBeads> {
    if preview.part_offsets.is_empty() {
        return None;
    }
    // Per part and layer, its segments (a segment is one part's on one layer), the surface's,
    // the lining's, then the core's, and how many beads are in each stream; then their runs.
    let mut layers: BTreeMap<(usize, u32), (Vec<InstanceSegment>, [u32; 3])> = BTreeMap::new();
    for (s, stream) in preview.beads.0.iter().enumerate() {
        for segment in stream.segments() {
            let Some(first) = segment.data().get(..PREVIEW_SLOTS) else { continue };
            let Some(part) = fab::print::bead_part(first) else { continue };
            let (segments, counts) = layers.entry((part, fab::print::bead::layer(first) as u32)).or_default();
            segments.push(segment.clone());
            counts[s] += (segment.data().len() / PREVIEW_SLOTS) as u32;
        }
    }
    let beads = layers.into_iter().map(|(at, (segments, counts))| {
        let runs = PreviewBlock::of_layer(segments.iter().flat_map(|s| s.data().chunks_exact(PREVIEW_SLOTS)), counts);
        (at, (segments, runs.into()))
    }).collect();
    let layer_z = preview.layers.iter().map(|l| (l.layer, l.z)).collect();
    Some(SliceBeads { beads, layer_z })
}

/// The beads of a slice of `revision` (its parts' in their frames) become the live tiles of the
/// layers they print unchanged, in place of their plans' beads ([`Slot::adopt`]): the preview
/// the desktop shows, the headset's view while a part is held and the next edit's untouched
/// layers then share one set of beads. Each block that changed is joined anew and published, so
/// the plan's beads go. Returns how many layers took them.
fn adopt(l: &mut LiveState, revision: u64, s: &SliceBeads) -> usize {
    if l.revision != revision {
        return 0;
    }
    let t = std::time::Instant::now();
    let plate_zs = plate_zs(l);
    let plate_layer = |z: f32| plate_zs.partition_point(|&q| q < (z * 1000.0).round() as i64) as u32;
    let LiveState { placement, slots, .. } = l;
    let mut adopted = 0;
    for (part, &(key, _)) in placement.iter().enumerate() {
        let Some(slot) = slots.get_mut(part).filter(|s| s.key == key) else { continue };
        adopted += slot.adopt(part, &plate_layer, &s.beads, &s.layer_z);
    }
    log!("fab live tiles: {adopted} layers took the slice's beads in {:.1} ms, revision {revision}", t.elapsed().as_secs_f64() * 1e3);
    if adopted > 0 {
        l.unpublished.tiles = true;
        publish_applied(l);
    }
    adopted
}

#[cfg(test)]
struct RebuildTimer(std::time::Instant);

#[cfg(test)]
impl Drop for RebuildTimer {
    fn drop(&mut self) {
        REBUILDS[0].fetch_add(1, Ordering::Relaxed);
        REBUILDS[1].fetch_add(self.0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// Nothing is published before the engine's first contours; after them every publication
/// carries the slots' newest cuts (only their changed layers are built, see [`refresh_contours`]).
fn publish(l: &mut LiveState, stage: LiveStage) {
    if l.contours.is_none() {
        return;
    }
    refresh_contours(l);
    let Some(contours) = l.contours.clone() else { return };
    let r = LiveResult {
        revision: l.revision,
        stage,
        contours,
        layer_height: view().layer_height as f32,
        report: l.report.as_ref().map(|r| r.0.clone()),
        markers: l.report.as_ref().map(|r| r.1.clone()),
        preview: if stage == LiveStage::Refined {
            l.preview.as_ref().filter(|p| p.has_beads()).cloned()
        } else { None },
        layers_done: l.layers.0,
        layers_total: l.layers.1,
        complete: l.complete,
        checking: l.checking,
    };
    let r = Arc::new(r);
    // the next publication waits for a view to take this one (before it is out: a view that
    // takes it at once lets the next go)
    if live_views() {
        hub().owner.taken.store(false, Ordering::Release);
    }
    drop(hub().latest.set(r.clone()));
    #[cfg(test)]
    if let Some(tx) = OBSERVE.lock().unwrap().as_ref() {
        let _ = tx.send((std::time::Instant::now(), "live".into(), None, Some(r)));
    }
    Cx::post_action(LiveAction::Updated);
}

/// Posted when a newer live result exists (the views redraw).
#[derive(Clone, Debug)]
pub enum LiveAction {
    Updated,
    /// An AI started or finished waiting for the committed plate's slice.
    AiWait,
    Failed { revision: u64, error: String },
    /// The plate's toolpaths of `revision` are whole ([`toolpaths_of`] has them).
    Toolpaths { revision: u64 },
}

/// Bring the joined contours up to every slot's cut and place: a slot whose cut, stale layers
/// or place changed builds only the layers it has no beads for yet, and the join shares every
/// layer's beads by reference (the cost is the changed layers', not the plate's).
fn refresh_contours(l: &mut LiveState) {
    let started = std::time::Instant::now();
    let made_from: Vec<(u64, Pt)> = l.placement.iter().zip(&l.slots).map(|(p, s)| (if s.cut.is_some() && s.key == p.0 { p.0 } else { 0 }, p.1)).collect();
    let mut changed = l.contours.is_none() || l.contours_of != made_from;
    let mut built = 0;
    for (part, (slot, &(key, d))) in l.slots.iter_mut().zip(&l.placement).enumerate() {
        if key == slot.key && slot.cut.is_some() && (slot.contours.is_none() || slot.beads_at != Some(d)) {
            built += slot.build_contours(part, d);
            changed = true;
        }
    }
    if !changed {
        return;
    }
    let mut top = 0;
    let parts: Vec<Arc<[Option<RetainedInstances>]>> = l.slots.iter().zip(&made_from).map(|(slot, &(key, _))| {
        match slot.contours.as_ref().filter(|_| key != 0) {
            Some((layers, t)) => {
                top = top.max(*t);
                layers.clone()
            }
            None => Arc::from([]),
        }
    }).collect();
    let segments = parts.iter().flat_map(|p| p.iter().flatten()).flat_map(|b| b.segments().iter().cloned()).collect();
    let all = RetainedInstances::from_segments(PREVIEW_SLOTS, segments).expect("whole instances");
    if let Some(w) = l.bead_stats.add(started.elapsed().as_secs_f64(), built, all.count()) {
        log!("fab live beads: {} joins (avg {:.2} ms, worst {:.2} ms), {} layers built, {} beads", w.n, w.avg_ms(), w.worst * 1e3, w.layers, w.beads);
    }
    l.contours = Some(Contours { parts, all, top_layer: top });
    l.contours_of = made_from;
}

/// One layer's contours at offset `d`, one bead per edge in the print preview's format, at the
/// layer's middle, on the part's layer index `li`.
fn contour_layer_beads(part: usize, li: u32, z: f64, h: f64, regions: &[ExPolygon], d: Pt) -> RetainedInstances {
    let (dx, dy) = (d.x_mm() as f32, d.y_mm() as f32);
    let zc = (z - h / 2.0) as f32;
    let height = (h as f32 * 0.6).max(0.05);
    let mut data: Vec<f32> = Vec::with_capacity(regions.iter().flat_map(|e| e.rings()).map(|r| r.pts.len()).sum::<usize>() * PREVIEW_SLOTS);
    for e in regions {
        for r in e.rings() {
            let n = r.pts.len();
            for i in 0..n {
                let (a, b) = (r.pts[i], r.pts[(i + 1) % n]);
                data.extend_from_slice(&Bead {
                    a: [a.x_mm() as f32 + dx, a.y_mm() as f32 + dy, zc],
                    b: [b.x_mm() as f32 + dx, b.y_mm() as f32 + dy, zc],
                    width: 0.45,
                    height,
                    layer: li as f32,
                    role: 3.0,
                    tag: part as f32,
                    kind: 3.0,
                    ..Bead::default()
                }.pack());
            }
        }
    }
    RetainedInstances::new(PREVIEW_SLOTS, data.into()).expect("whole layer instances")
}

/// Two seconds of one kind of bead work: how often, how long, the layers it built and the
/// beads there were after the last.
struct BeadWindow {
    since: Option<std::time::Instant>,
    n: u32,
    layers: usize,
    sum: f64,
    worst: f64,
    beads: usize,
}

impl BeadWindow {
    const fn new() -> Self {
        BeadWindow { since: None, n: 0, layers: 0, sum: 0.0, worst: 0.0, beads: 0 }
    }

    /// One more; the window, once two seconds are full (and a new one starts).
    fn add(&mut self, secs: f64, built: usize, beads: usize) -> Option<BeadWindow> {
        let since = *self.since.get_or_insert_with(std::time::Instant::now);
        self.n += 1;
        self.layers += built;
        self.sum += secs;
        self.worst = self.worst.max(secs);
        self.beads = beads;
        (since.elapsed().as_secs_f64() >= 2.0).then(|| std::mem::replace(self, BeadWindow::new()))
    }

    fn avg_ms(&self) -> f64 {
        self.sum / self.n as f64 * 1e3
    }
}

impl Default for BeadWindow {
    fn default() -> Self {
        BeadWindow::new()
    }
}

/// One line every two seconds while live results flow: how many, and how old their
/// input was when each was ready (newest input to result).
fn stats(level: &'static str, age: f64) {
    struct S {
        since: Option<std::time::Instant>,
        n: [u32; 2],
        worst: [f64; 2],
        sum: [f64; 2],
    }
    static STATS: Leaf<S> = Leaf::new(S { since: None, n: [0; 2], worst: [0.0; 2], sum: [0.0; 2] });
    let k = if level == "contours" { 0 } else { 1 };
    // The window, once two seconds are full (and a new one starts).
    let full = {
        let mut s = STATS.lock();
        let since = *s.since.get_or_insert_with(std::time::Instant::now);
        s.n[k] += 1;
        s.worst[k] = s.worst[k].max(age);
        s.sum[k] += age;
        let t = since.elapsed().as_secs_f64();
        (t >= 2.0).then(|| (t, std::mem::replace(&mut *s, S { since: None, n: [0; 2], worst: [0.0; 2], sum: [0.0; 2] })))
    };
    if let Some((t, s)) = full {
        let avg = |i: usize| if s.n[i] > 0 { s.sum[i] / s.n[i] as f64 * 1e3 } else { 0.0 };
        log!("fab live: {:.1} s: contours {} (age avg {:.0} ms, worst {:.0} ms), toolpaths {} (age avg {:.0} ms, worst {:.0} ms)", t, s.n[0], avg(0), s.worst[0] * 1e3, s.n[1], avg(1), s.worst[1] * 1e3);
    }
}

/// One line whenever the set of live issue kinds changes.
fn log_kinds(report: &fab_printability::Report) {
    static LAST: Leaf<String> = Leaf::new(String::new());
    let mut kinds: Vec<&str> = report.issues.iter().map(|i| i.kind).collect();
    kinds.sort();
    kinds.dedup();
    let line = kinds.join(" ");
    let changed = {
        let mut last = LAST.lock();
        let changed = *last != line;
        if changed {
            last.clone_from(&line);
        }
        changed
    };
    if changed {
        log!("fab live issues: {line}");
    }
}

/// Test hook: an engine on std threads whose continuations skip a stale input's work, as the
/// app's pool tickets do: every part is cut on the cores. The first one of the process gives the
/// slicer's maps the machine's cores, or the run's CPU budget when it has one
/// ([`makepad_fab_toolpath::par::budget_threads`]), as the app's [`init`] does (at most 8: tests
/// run side by side); until then a map runs on its caller alone.
#[cfg(test)]
pub(crate) fn cpu_engine() -> (LiveEngine, std::sync::mpsc::Receiver<LiveEvent>) {
    static CORES: std::sync::Once = std::sync::Once::new();
    CORES.call_once(|| makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map_or(4, |n| n.get())).min(8)));
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let spawn: Spawn = Arc::new(|_, job| { std::thread::spawn(job); });
    let version = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let next: makepad_fab_slice::live::Next = Arc::new(move || {
        let mine = version.fetch_add(1, Ordering::AcqRel) + 1;
        let version = version.clone();
        Arc::new(move || version.load(Ordering::Acquire) != mine)
    });
    let engine = LiveEngine::new(spawn.clone(), Arc::new(move |e| { let _ = tx.lock().unwrap().send(e); }), next);
    engine.set_resume(Arc::new(move |checkpoint| {
        let spawn = spawn.clone();
        Arc::new(move |urgency, job| {
            let checkpoint = checkpoint.clone();
            spawn(urgency, Box::new(move || { if urgency == Urgency::Viewed || !checkpoint() { job(); } }));
        })
    }));
    (engine, rx)
}

/// Test hook: the whole toolpaths of `ev`'s plate at Fab's default settings, as a live slice
/// makes them (on a [`cpu_engine`]).
#[cfg(test)]
pub(crate) fn sliced(ev: &Evaluated) -> Arc<makepad_fab_slice::live::PlatePaths> {
    let (engine, rx) = cpu_engine();
    engine.set_input(input_for(plate_id(ev), &CheckSetup::fab_defaults(), Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields)));
    rx.iter().find_map(|e| match e {
        LiveEvent::Toolpaths { toolpaths, .. } => Some(toolpaths),
        LiveEvent::Failed { error, .. } => panic!("the plate does not slice: {error}"),
        _ => None,
    }).expect("the engine answers")
}

/// A wait for the plate's toolpaths where no view shows them (the Mechanics check of its
/// supports): while one is held the plate slices as shown work does, and builds no tiles.
pub struct ToolpathsWanted {
    _private: (),
}

impl Drop for ToolpathsWanted {
    fn drop(&mut self) {
        hub().waits.fetch_sub(1, Ordering::Relaxed);
    }
}

impl ToolpathsWanted {
    pub fn start() -> ToolpathsWanted {
        hub().waits.fetch_add(1, Ordering::Relaxed);
        ToolpathsWanted { _private: () }
    }
}

/// A live view's handle on the live slice (the headset, the desktop's Slice render mode).
pub struct LiveSlice {
    _private: (),
}

impl Drop for LiveSlice {
    fn drop(&mut self) {
        hub().on.fetch_sub(1, Ordering::Relaxed);
    }
}

impl LiveSlice {
    pub fn start() -> LiveSlice {
        hub().on.fetch_add(1, Ordering::Relaxed);
        // A view that starts late gets every part again, and at once what the live state holds
        // (a slice made while no view showed it): its contours and tiles are built on the
        // slicing pool.
        let h = hub();
        h.engine.resend_all();
        h.owner.post(Msg::StartView);
        LiveSlice { _private: () }
    }

    /// The plate edits start from (each rebuilt plate); transforms reset to none.
    pub fn set_plate(&self, plate: Arc<Evaluated>, layer_height: f64, _materials: fab_printability::Materials) {
        // The UI is the views' only writer: a test plate's input stays as the last job set it.
        let nf = plate.plate.parts.iter().map(|p| p.filament as usize).max().unwrap_or(1).max(1);
        let colours = (0..nf).map(|f| plate.plate.parts.iter().find(|p| p.filament as usize == f + 1).map(|p| p.color).unwrap_or([0.8; 4])).collect();
        let view = ViewSettings { layer_height: layer_height.clamp(0.04, 1.0), machine: Some(plate.machine.clone()), colours, tests: view().tests.clone(), melts: crate::print::melt_steps(&plate.plate) };
        drop(hub().view.set(Arc::new(view)));
        hub().released.store(true, Ordering::Relaxed);
        send_live_plate(&plate);
    }

    /// A part's transform now, on the part as the plate has it (coalescing).
    pub fn set_transform(&self, part: usize, xf: PartTransform) {
        hub().released.store(false, Ordering::Relaxed);
        hub().engine.set_transform(part, xf);
    }

    /// The hands let go: refine what is shown (the committed edit reslices after).
    pub fn release(&self) {
        hub().released.store(true, Ordering::Relaxed);
        hub().engine.release();
    }

    /// The newest result, if the engine is not publishing one this instant.
    /// Taking it lets the next publication go ([`LiveOwner`]).
    pub fn latest(&self) -> Option<Arc<LiveResult>> {
        let r = hub().latest.try_get();
        if r.is_some() {
            took_publication();
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settled_inserts_hit_survives_streamed_report_replacement_once() {
        let _hub = HUB_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let _cx = crate::cx_owned();
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let seen = delivered.clone();
        let mut cx = Cx::new(Box::new(move |_, event| {
            if let Event::Actions(actions) = event {
                for action in actions {
                    if let Some(PrintEvent::Issues { report, .. }) = action.downcast_ref::<PrintEvent>() {
                        seen.lock().unwrap().push(report.clone());
                    }
                }
            }
        }));
        let part = fab_plate::Part {
            object: "insert".into(), name: "Insert".into(), filament: 1, color: [1.0; 4], nozzle: None,
            mesh: fab_plate::PartMesh {
                positions: (0..8).map(|i| [2.0 + (i & 1) as f64, -1.0 + 2.0 * ((i >> 1) & 1) as f64, ((i >> 2) & 1) as f64]).collect(),
                triangles: vec![[0,2,1], [1,2,3], [4,5,6], [5,7,6], [0,1,4], [1,5,4], [2,6,3], [3,6,7], [0,4,2], [2,4,6], [1,3,5], [3,7,5]],
            },
            field_bounds: None, field_floor: None, print: Vec::new(), print_mesh: None, paint: None, body_of: None,
            ..Default::default()
        };
        let insert = fab_plate::Insert { into: "host".into(), clearance: 0.0, embed: false, melt: None };
        let hits = fab_printability::insert::bead_issues(&[(&part, &insert)], &mut line(0, 0.2, 0.0, 0.0, 1.0).records().into_iter());
        let mut live = LiveState { revision: 7, settling: Some((7, 5, None)), ..Default::default() };
        let report = Arc::new(fab_printability::Report::default());
        handle(&mut live, Msg::Issues { revision: 7, report: report.clone(), done: 0, total: 1 });
        // The worker finishes after the first report: replay what was held, then replace it
        // with the final streamed report twice. Each delivery has exactly one insert hit.
        let (_, _, held) = live.settling.take().unwrap();
        live.settled = Some((7, 5));
        live.insert_issues = Some((7, hits));
        let (held, done, total) = held.unwrap();
        issues(&mut live, 7, held, done, total);
        for _ in 0..2 {
            handle(&mut live, Msg::Issues { revision: 7, report: report.clone(), done: 1, total: 1 });
        }
        // A later revision must not inherit this slice's findings.
        live.settled = Some((8, 6));
        handle(&mut live, Msg::Issues { revision: 8, report, done: 1, total: 1 });
        cx.handle_action_receiver();
        cx.handle_actions();
        let got: Vec<Vec<_>> = delivered.lock().unwrap().iter().map(|r| r.issues.iter()
            .map(|i| (i.kind, i.object.clone())).collect()).collect();
        let hit = vec![("insert_hit", Some("insert".to_string()))];
        assert_eq!(got, vec![hit.clone(), hit.clone(), hit, Vec::new()], "the held and replacement reports deliver the hit once, only for its slice");
    }

    /// Whether the ledger wrote revision `revision`'s viewed layer line among `lines`.
    fn viewed_line(lines: &[String], revision: u64) -> bool {
        lines.iter().any(|l| l.starts_with(&format!("fab: viewed layer revision={revision} ")))
    }

    /// The vase's viewed layer 5 (its part 1) planned by a drag's last step, revision 10, and on
    /// show as tile 70 in its preview 100.
    fn step_viewed(l: &mut Ledger) -> BTreeMap<(usize, u32), u64> {
        let shown = BTreeMap::from([((1, 5), 70)]);
        l.model_built(7, std::time::Instant::now(), 2.0);
        l.sent(10, 7);
        l.started(10, 7);
        l.viewed(10, 1, 5, 1.0, 1.0, 1.0);
        l.published(100, 10, |_, _| true, &shown, Some(std::time::Instant::now()), Vec::new());
        shown
    }

    /// The step's slice is whole, and the commit after it (the same content, revision 11) does
    /// no work: its preview keeps the step's tile. A frame that first draws the commit's preview
    /// draws the step's viewed layer; one whose preview holds another tile there does not.
    #[test]
    fn a_frame_that_first_draws_the_commits_preview_draws_the_steps_viewed_layer() {
        for (tile, drawn) in [(70, true), (71, false)] {
            let mut l = Ledger::new();
            let mut shown = step_viewed(&mut l);
            shown.insert((1, 5), tile);
            l.model_built(7, std::time::Instant::now(), 2.0);
            l.sent(11, 7);
            l.started(11, 7);
            l.published(101, 11, |_, _| true, &shown, Some(std::time::Instant::now()), Vec::new());
            let lines = l.drawn(101);
            assert_eq!(viewed_line(&lines, 10), drawn, "tile {tile}: {lines:?}");
        }
    }

    /// The commit's model (the step's plate) is built before the engine starts the drag's last
    /// step, and the commit comes while that step plans: the commit's viewed layer is the step's,
    /// said with no work, and its revision slices whole. Its line is written, timed from the
    /// commit's edit.
    #[test]
    fn a_commit_built_before_its_step_starts_writes_its_viewed_layer() {
        let mut l = Ledger::new();
        let shown = BTreeMap::from([((1, 5), 70)]);
        l.model_built(7, std::time::Instant::now(), 2.0);
        l.sent(10, 7);
        l.model_built(7, std::time::Instant::now(), 2.0);
        l.started(10, 7);
        l.sent(11, 7);
        l.viewed(10, 1, 5, 1.0, 1.0, 1.0);
        l.published(100, 10, |_, _| true, &shown, Some(std::time::Instant::now()), Vec::new());
        l.started(11, 7);
        l.viewed(11, 1, 5, 1.0, 0.0, 0.0);
        l.published(101, 11, |_, _| true, &shown, Some(std::time::Instant::now()), Vec::new());
        let lines = l.drawn(101);
        assert!(viewed_line(&lines, 11), "{lines:?}");
    }

    /// The frames lag: the Print editor's slice comes before any frame drew the live preview that
    /// holds the viewed layer, and the first frame draws the slice. With the viewed layer's tile
    /// still the live one there, it draws the viewed layer; with another tile there it does not.
    #[test]
    fn a_frame_that_first_draws_the_settled_slice_draws_the_viewed_layer() {
        for (tile, drawn) in [(70, true), (71, false)] {
            let mut l = Ledger::new();
            let mut shown = step_viewed(&mut l);
            shown.insert((1, 5), tile);
            l.settled(101, &shown);
            let lines = l.drawn(101);
            assert_eq!(viewed_line(&lines, 10), drawn, "tile {tile}: {lines:?}");
        }
    }

    /// Revision `revision`'s tile of `layer`.
    fn tile(layer: u32, revision: u64, ghost: bool) -> ShownTile {
        ShownTile { part: 0, layer, global: layer, hash: 0, stack: 0, z: 0.0, ghost, revision }
    }

    /// However many layers a revision draws, over however many frames, it writes one line, once
    /// its whole slice is on screen: what it drew, and how much before that. A later tile tells it
    /// again; a revision whose whole slice never comes is told by a later one's; and the Print
    /// editor's slice tells what is left.
    #[test]
    fn a_run_writes_one_line_however_many_layers_it_draws() {
        let drawn = |lines: &[String]| lines.iter().filter(|l| l.starts_with("fab live drawn:")).cloned().collect::<Vec<_>>();
        let mut l = Ledger::new();
        let (shown, now) = (BTreeMap::new(), Some(std::time::Instant::now()));
        l.published(1, 10, |_, _| true, &shown, now, (0..300).map(|i| tile(i, 10, false)).collect());
        assert!(drawn(&l.drawn(1)).is_empty(), "the first frame writes nothing yet");
        l.published(2, 10, |_, _| true, &shown, now, (300..600).map(|i| tile(i, 10, false)).chain([tile(600, 9, true), tile(601, 3, false)]).collect());
        assert!(drawn(&l.drawn(2)).is_empty(), "nor the second");
        l.published(3, 10, |_, _| true, &shown, now, vec![tile(602, 10, false)]);
        l.whole(3);
        let lines = drawn(&l.drawn(3));
        assert_eq!(lines.len(), 1, "{lines:?}");
        let parts = "fab live drawn: revision=10 602 new tiles in parts {0}, 1 ghost, 3 frames, 600 before the whole slice, ";
        assert!(lines[0].starts_with(parts) && lines[0].ends_with(" ms after the edit"), "{lines:?}");
        let runs = &l.runs[&10];
        assert_eq!((runs.tiles.len(), runs.tiles[601].revision, runs.tiles[601].layer), (602, 10, 602), "the slice tool has every tile");
        let mut joined = Vec::new();
        loop {
            let (page, left) = l.drawn_tiles(10, joined.len());
            assert!(!page.is_empty() && page.len() <= DRAWN_PAGE, "each page advances within the reply's limit");
            joined.extend(page);
            assert_eq!(left, runs.tiles.len() - joined.len(), "left counts the tiles after the page");
            if left == 0 { break; }
        }
        let fields = |t: &DrawnTile| (t.part, t.layer, t.revision, t.ms);
        assert_eq!(joined.iter().map(fields).collect::<Vec<_>>(), runs.tiles.iter().map(fields).collect::<Vec<_>>(), "pages keep every tile in order");
        for (revision, from) in [(99, 0), (10, runs.tiles.len()), (10, usize::MAX)] {
            let (page, left) = l.drawn_tiles(revision, from);
            assert!(page.is_empty() && left == 0, "an unknown revision or an exhausted range has no tiles left");
        }
        l.published(4, 10, |_, _| true, &shown, now, Vec::new());
        assert!(drawn(&l.drawn(4)).is_empty(), "a frame that draws nothing new tells nothing");
        l.published(5, 10, |_, _| true, &shown, now, vec![tile(603, 10, false)]);
        assert_eq!(drawn(&l.drawn(5)).len(), 1, "a later tile tells it again");

        l.published(6, 11, |_, _| true, &shown, now, vec![tile(0, 11, false)]);
        assert!(drawn(&l.drawn(6)).is_empty(), "an edit under way");
        l.published(7, 12, |_, _| true, &shown, now, vec![tile(0, 12, false)]);
        assert!(drawn(&l.drawn(7)).is_empty(), "another on top of it");
        l.published(8, 12, |_, _| true, &shown, now, Vec::new());
        l.whole(8);
        let lines = drawn(&l.drawn(8));
        assert!(lines.len() == 2 && lines[0].contains("revision=11 ") && lines[1].contains("revision=12 "), "the later whole slice tells the edit it superseded: {lines:?}");

        l.published(9, 13, |_, _| true, &shown, now, vec![tile(0, 13, false)]);
        assert!(drawn(&l.drawn(9)).is_empty());
        l.settled(10, &shown);
        let lines = drawn(&l.drawn(10));
        assert!(lines.len() == 1 && lines[0].contains("revision=13 "), "the Print editor's slice tells what is left: {lines:?}");

        for r in 20..100 {
            l.published(r, r, |_, _| true, &shown, now, vec![tile(0, r, false)]);
            l.drawn(r);
        }
        assert_eq!(l.runs.len(), DRAWN_RUNS, "the last revisions are kept");
    }

    /// An estimate asked for as the app ends finds the pool closed: it is dropped, and nothing panics.
    #[test]
    fn work_for_a_closed_pool_is_dropped() {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let job = ran.clone();
        run_while_open(&TaskPool::closed(), Class::Background, "fab estimate", move || job.store(true, Ordering::Relaxed));
        assert!(!ran.load(Ordering::Relaxed));
    }

    fn layer(slot: &mut Slot, i: u32) {
        slot.layers.insert((i, 0), Arc::default());
    }

    /// One part layer's paths for a test: a 10 mm line from (x, y) at print z `z`, at 10 mm/s,
    /// laying `e` mm of filament.
    fn line(index: u32, z: f64, x: f64, y: f64, e: f64) -> LivePaths {
        let path = makepad_fab_toolpath::ExtrusionPath { pts: vec![Pt::from_mm(x, y), Pt::from_mm(x + 10.0, y)], width: 0.4, height: 0.2, area: e / 10.0, speed: 10.0, ..Default::default() };
        let layer = makepad_fab_toolpath::ObjectLayer { islands: vec![Arc::new(makepad_fab_toolpath::Island { walls: Vec::new(), fills: vec![path] })], ..Default::default() };
        let mut paths = LivePaths::default();
        paths.add_with(index as usize, z, &layer, 0, 1.0, makepad_fab_slice::live_paths::Kept::All);
        paths
    }

    /// A queued event's identity, as the test made it: (kind, content key, revision, layer, region,
    /// which plan of the layer).
    type EventId = (u8, u64, u64, u32, u32, u32);

    fn event_of(id: EventId) -> LiveEvent {
        let (kind, key, revision, layer, region, plan) = id;
        match kind {
            0 => LiveEvent::Contours { revision, parts: vec![(key, Pt::default())], objects: Arc::from(["figure".to_string()]), age: 0.0 },
            1 => LiveEvent::PartRegions { part: 0, key, regions: 2, total: 40 },
            _ => {
                // the same geometry in every plan of a layer (no replan), told apart by its extrusion
                let paths = line(layer, 0.2, layer as f64, region as f64, plan as f64 + 1.0);
                LiveEvent::PartLayers { revision, pass: makepad_fab_slice::live::Pass::Window, part: 0, key, region, layers: vec![(layer, paths)], total: 40 }
            }
        }
    }

    fn id_of(e: &LiveEvent) -> EventId {
        match e {
            LiveEvent::Contours { revision, parts, .. } => (0, parts[0].0, *revision, 0, 0, 0),
            LiveEvent::PartRegions { key, .. } => (1, *key, 0, 0, 0, 0),
            LiveEvent::PartLayers { revision, key, region, layers, .. } => (2, *key, *revision, layers[0].0, *region, layers[0].1.records()[0].e.round() as u32 - 1),
            _ => unreachable!("the test queues contours, regions and layers"),
        }
    }

    /// What the live state holds that the events make: the plate, and per slot its content, its
    /// layers' records and who planned them, and the ghosts of superseded contents.
    fn digest(l: &LiveState) -> String {
        let slot = |s: &Slot| {
            let layers: Vec<String> = s.layers.iter().map(|(at, p)| format!("{at:?}={:?}@{:?}", p.records().iter().map(|m| (m.p0, m.e)).collect::<Vec<_>>(), s.by.get(at).map(|b| b.revision))).collect();
            format!("key {} rev {} regions {} total {} layers [{}]", s.key, s.revision, s.regions, s.total, layers.join(" "))
        };
        let slots: Vec<String> = l.slots.iter().map(|s| format!("{} | newer [{}] | good {:?}", slot(s), s.newer.iter().map(slot).collect::<Vec<_>>().join("; "), s.good.as_deref().map(slot))).collect();
        format!("placement {:?} revision {} complete {} slots {}", l.placement, l.revision, l.complete, slots.join(" // "))
    }

    /// An edit's events from many workers go into the live state in the order they were posted,
    /// whichever worker owns it, and leave it as one thread applying them in that order would: a
    /// worker's events in the order it sent them, the regions before the layers they announce, a
    /// new content's contours after the old one's layers, and the layers two workers planned
    /// both in the order they came. Publications follow the inbox (none shows less of it than the
    /// one before) and, paced by a view that takes them, come far fewer than the events.
    #[test]
    fn queued_events_apply_in_their_order_and_leave_the_state_a_single_sink_would() {
        let owner = LiveOwner::new();
        let applied: Mutex<Vec<EventId>> = Mutex::new(Vec::new());
        let apply_noted = |l: &mut LiveState, m: Msg| {
            let Msg::Part(e) = m else { return };
            applied.lock().unwrap().push(id_of(&e));
            apply_queued(l, e);
        };
        // a publication costs a while, and tells the view it is out: how much of the inbox it shows
        let shown: Mutex<Vec<usize>> = Mutex::new(Vec::new());
        let publish = |l: &mut LiveState| {
            l.unpublished = Unpublished::default();
            shown.lock().unwrap().push(applied.lock().unwrap().len());
            owner.taken.store(false, Ordering::Release);
            let t = std::time::Instant::now();
            while t.elapsed() < std::time::Duration::from_micros(200) {
                std::hint::spin_loop();
            }
        };
        let work = Work { handle: &apply_noted, publish: &publish, lost: &|why: String| panic!("the state was lost: {why}") };
        let push = |id: EventId| owner.post_run(Msg::Part(event_of(id)), true, &work);
        let done = std::sync::atomic::AtomicBool::new(false);
        let mut sent: Vec<Vec<EventId>> = Vec::new();
        std::thread::scope(|scope| {
            // the view: it takes each publication a while after it is out, and has the held one published
            scope.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    if owner.took() {
                        owner.post_run(Msg::Took, true, &work);
                    }
                }
            });
            for (key, revision, workers) in [(11u64, 1u64, 8u32), (12, 2, 4)] {
                let lead = vec![(0, key, revision, 0, 0, 0), (1, key, 0, 0, 0, 0)];
                lead.iter().for_each(|&id| push(id));
                sent.push(lead);
                let batches: Vec<Vec<EventId>> = std::thread::scope(|workers_scope| {
                    let handles: Vec<_> = (0..workers).map(|w| workers_scope.spawn(move || {
                        let mut mine = Vec::new();
                        for layer in (0..40).filter(|l| l % workers == w) {
                            for region in 0..2 {
                                mine.push((2, key, revision, layer, region, w));
                            }
                        }
                        // the next worker's first layers planned again, racing its own plan of them
                        for layer in (0..40).filter(|l| l % workers == (w + 1) % workers).take(3) {
                            mine.push((2, key, revision, layer, 0, 100 + w));
                        }
                        for &id in &mine {
                            push(id);
                        }
                        mine
                    })).collect();
                    handles.into_iter().map(|h| h.join().unwrap()).collect()
                });
                sent.extend(batches);
            }
            done.store(true, Ordering::Release);
        });
        // the view takes the last: what is held goes out
        owner.taken.store(true, Ordering::Release);
        owner.post_run(Msg::Took, true, &work);
        let live = owner.inbox.into_inner().unwrap().state.expect("the state is parked");
        let applied = applied.into_inner().unwrap();
        let total: usize = sent.iter().map(Vec::len).sum();
        let mut all: Vec<EventId> = sent.iter().flatten().copied().collect();
        let mut got = applied.clone();
        all.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, all, "every queued event is applied once");
        let at = |id: &EventId| applied.iter().position(|a| a == id).unwrap();
        for s in &sent {
            assert!(s.windows(2).all(|w| at(&w[0]) < at(&w[1])), "a sender's events apply in the order it sent them");
        }
        let last_of_first = sent[..9].iter().flatten().map(at).max().unwrap();
        assert!(last_of_first < at(&(0, 12, 2, 0, 0, 0)), "the new content's contours come after every layer of the old one");
        assert!(applied.iter().filter(|a| a.0 == 2).all(|a| at(a) > at(&(1, a.1, 0, 0, 0, 0))), "a content's regions come before its layers");
        // one thread applying the same events in the same order
        let mut single = LiveState::default();
        for &id in &applied {
            apply(&mut single, event_of(id));
        }
        assert_eq!(digest(&live), digest(&single), "the state is the one a single sink leaves");
        let shown = shown.into_inner().unwrap();
        assert!(shown.windows(2).all(|w| w[0] <= w[1]), "each publication shows the queue up to where the one before did, or further: {shown:?}");
        assert_eq!(shown.last().copied(), Some(total), "the last shows every event");
        assert!(shown.len() * 4 < total, "{} publications for {total} events: the paced ones take what came meanwhile together", shown.len());
    }

    /// The hang of #3620 (the second deadlock of the live slice's locks): with a live view on, a
    /// publication in progress and the estimate preview reading what it shows (the view's
    /// colours, the plate's objects), the person edits the plate. The publication and the preview
    /// are stopped where the hang's stacks had them, and the edit must return while they are
    /// still stopped; then they go on and the publication comes out. In the hang the preview held
    /// the view while it waited for the live state, the publisher held the live state while it
    /// waited for the view, and the edit (storing its view settings) waited for the view. The
    /// Print editor's calls return meanwhile too, with the view closed: a slice superseded by a
    /// newer plate, and one cancelled, are answered once the publication goes on (their answers
    /// let the preview go, and took the live state the publisher held for that). So do the
    /// view's and the rest of the UI's calls. With `FAB_GUARD_ROUNDS=<n>` it is the stress rig:
    /// n rounds, the stopped two let on in either order.
    #[test]
    fn a_plate_edit_returns_while_a_publication_and_an_estimate_preview_are_in_progress() {
        use makepad_fab_slice::live::{Estimate, Streamed};
        let _hub = HUB_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let _cx = crate::cx_shared();
        // Only a failure waits this long: on the pass path nothing waits on the clock.
        const STUCK: std::time::Duration = std::time::Duration::from_secs(5);
        init_for_tests();
        // A live view is on and the engine off: the edits go the real way in, and no slice of
        // theirs fails or publishes in the middle of the test.
        struct EngineOff;
        impl Drop for EngineOff {
            fn drop(&mut self) {
                *OBSERVE.lock().unwrap() = None;
                hub().engine.cancel();
                hub().engine.set_enabled(true);
            }
        }
        let machine = fab_plate::machine::Machine::dual_nozzle_350();
        let job_of = {
            let machine = machine.clone();
            move |gen: u64| PrintJob { gen, settings: fab::print::PrintSettings::default(), colours: vec![[0.8; 4]], machine: machine.clone(), name: "block".into(), objects: Arc::from(["block".to_string()]), check: None, fallback: None, catalog: None, tests: None, melts: Vec::new() }
        };
        let job = Arc::new(job_of(0));
        // The edit's plate (an empty one), and the Print editor's (another).
        let setup = CheckSetup::fab_defaults();
        let plate = Arc::new(fab_plate::Plate::default());
        let input_of = move |plate_id: u64| {
            let make: makepad_fab_slice::live::MakeInput = {
                let (setup, plate, machine) = (setup.clone(), plate.clone(), machine.clone());
                Arc::new(move || input_for(plate_id, &setup, plate.clone(), machine.clone(), None, None))
            };
            SliceInput { plate_id, settings_id: setup.id_for(&plate), parts: 0, make }
        };
        let input = Arc::new(input_of(1));
        let (seen, observed) = std::sync::mpsc::channel();
        *OBSERVE.lock().unwrap() = Some(seen);
        hub().engine.set_enabled(false);
        let _engine_off = EngineOff;
        let view = Arc::new(LiveSlice::start());
        let contours = || LiveEvent::Contours { revision: 1, parts: Vec::new(), objects: Arc::from(["block".to_string()]), age: 0.0 };
        let preview = |mut taker: RecordTaker| {
            let _ = taker(Streamed::Done { estimate: Arc::new(Estimate { timeline: Default::default(), stats: Default::default(), changes: None, tools: Vec::new(), filament_temps: Vec::new(), seconds: 0.0 }) });
        };
        // The Print editor's calls, with the view closed (its preview goes with them): a slice
        // that a newer plate supersedes, and one cancelled.
        struct ViewClosed;
        impl ViewClosed {
            fn new() -> Self {
                hub().on.fetch_sub(1, Ordering::Relaxed);
                ViewClosed
            }
        }
        impl Drop for ViewClosed {
            fn drop(&mut self) {
                hub().on.fetch_add(1, Ordering::Relaxed);
            }
        }
        let print_calls = {
            let input = input.clone();
            move |edit: &PrintJob| {
                settle(job_of(1), input_of(2));
                set_input(&input, edit, std::time::Instant::now());
                settle(job_of(2), input_of(2));
                cancel();
            }
        };
        // The view's calls and the rest of the UI's, on a plate of its own (a small block).
        let ev = {
            use crate::print::slice_bench::plate;
            let script = "use mod.model.*\nuse mod.fab.*\nlet block = mod.model.sdf({bounds: [[-3mm, 0mm, -3mm], [3mm, 0.6mm, 3mm]] cell: 0.1mm field: fn(p) { return sdf_box(p - vec3(0, 0.3mm, 0), vec3(6mm, 0.6mm, 6mm)) }})\nobject(block)\n";
            let project = "use mod.fab.*\nscript(\"ui.cad.splash\")\nplace(\"ui\", object: \"block\", at: [80mm 165mm], name: \"Block\")\n";
            plate(&(project.into(), [("ui.cad.splash".into(), script.into())].into()))
        };
        let materials = crate::print::slice_bench::setup(&ev).materials(&ev.plate, &ev.machine).expect("materials");
        let ui_calls = {
            let (view, input) = (view.clone(), input.clone());
            move || {
                view.set_plate(ev.clone(), 0.2, materials.clone());
                view.set_transform(0, IDENTITY);
                view.release();
                let _ = view.latest();
                let _ = estimate_kept();
                estimate();
                set_focus_z(Some(0.3));
                shape_changed();
                shape_published();
                let _ = (revisions(), latest_sliced(), latest_toolpaths_revision(), input_revision(&input), ai_waiting());
            }
        };
        // Both answered as not sliced, by the live state's owner.
        let answered = |observed: &std::sync::mpsc::Receiver<(std::time::Instant, String, Option<Arc<PrintPreview>>, Option<Arc<LiveResult>>)>, live: bool| {
            let (mut sliced, mut published) = (0, !live);
            while sliced < 2 || !published {
                let o = observed.recv_timeout(STUCK).unwrap_or_else(|_| panic!("{sliced} of the Print editor's 2 slices answered, the publication {}out", if published { "" } else { "not " }));
                sliced += (o.1 == "sliced false") as usize;
                published |= o.1 == "live" && o.3.is_some();
            }
        };
        // Every party once first, so nothing is compiled while they are stopped, and the live
        // state gets its contours: a publication then publishes. Each on a thread that holds no
        // lock, as the UI's and the engine's do (this one holds the hub tests' lock).
        let (warm_print, warm_job) = (print_calls.clone(), job.clone());
        std::thread::spawn(move || {
            let _closed = ViewClosed::new();
            warm_print(&warm_job);
        }).join().expect("the Print editor's calls");
        answered(&observed, false);
        let (warm_ui, warm_input, warm_job) = (ui_calls.clone(), input.clone(), job.clone());
        std::thread::spawn(move || {
            warm_ui();
            set_input(&warm_input, &warm_job, std::time::Instant::now());
            hub().owner.taken.store(true, Ordering::Release);
            post_live(Msg::Part(contours()));
            preview(records_taker(1, None).expect("the view has a machine"));
        }).join().expect("every party once");
        let (finished, done) = std::sync::mpsc::channel::<(&'static str, bool)>();
        let run = |name: &'static str, work: Box<dyn FnOnce() + Send>| {
            let finished = finished.clone();
            std::thread::spawn(move || {
                gates::mark();
                let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_ok();
                let _ = finished.send((name, ok));
            });
        };
        // Once here. The stress rig runs it FAB_GUARD_ROUNDS times, letting the two stopped ones
        // go on in either order.
        let rounds: u64 = std::env::var("FAB_GUARD_ROUNDS").ok().and_then(|n| n.parse().ok()).unwrap_or(1);
        let mut order = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        for round in 0..rounds {
            // The owner job the Print editor's answers started has parked the state: the
            // publication below takes it on its own thread.
            let t = std::time::Instant::now();
            while hub().owner.inbox.lock().unwrap().state.is_none() {
                assert!(t.elapsed() < STUCK, "round {round}: the live state's owner never parks it");
                std::thread::yield_now();
            }
            while observed.try_recv().is_ok() {}
            // The view took the last publication: the next is due.
            hub().owner.taken.store(true, Ordering::Release);
            let taker = records_taker(1, None).expect("the view has a machine");
            // The publisher, stopped owning the live state, before it reads the view.
            let (published, publishing) = gates::arm("publication");
            run("the publication", Box::new(move || post_live(Msg::Part(contours()))));
            published.recv_timeout(STUCK).unwrap_or_else(|_| panic!("round {round}: the publication starts"));
            // The estimate preview, stopped once it has the view's colours, before the plate's objects.
            let (looked, looking) = gates::arm("estimate look");
            run("the estimate preview", Box::new(move || preview(taker)));
            looked.recv_timeout(STUCK).unwrap_or_else(|_| panic!("round {round}: the estimate preview looks at what it shows"));
            // The edit returns while both are stopped, and so do the Print editor's calls.
            let (input, edit, calls) = (input.clone(), job.clone(), job.clone());
            run("the edit", Box::new(move || {
                set_input(&input, &edit, std::time::Instant::now());
            }));
            match done.recv_timeout(STUCK) {
                Ok(("the edit", true)) => {}
                Ok(other) => panic!("round {round}: {other:?} ended first: only the edit can, the others are stopped"),
                Err(_) => panic!("round {round}: the edit waits on a lock the estimate preview or the publisher holds (#3620: the preview held the view and waited for the live state, the publisher held the live state and waited for the view)"),
            }
            {
                let _closed = ViewClosed::new();
                let print_calls = print_calls.clone();
                run("the Print editor's calls", Box::new(move || print_calls(&calls)));
                match done.recv_timeout(STUCK) {
                    Ok(("the Print editor's calls", true)) => {}
                    Ok(other) => panic!("round {round}: {other:?} ended before the Print editor's calls: the others are stopped"),
                    Err(_) => panic!("round {round}: a superseded or cancelled Print editor slice waits for the publisher (its answer let the preview go, which took the live state the publisher held)"),
                }
            }
            run("the UI's calls", Box::new(ui_calls.clone()));
            match done.recv_timeout(STUCK) {
                Ok(("the UI's calls", true)) => {}
                Ok(other) => panic!("round {round}: {other:?} ended before the UI's calls: the others are stopped"),
                Err(_) => panic!("round {round}: a call of the view or the UI waits for the stopped publisher or estimate preview"),
            }
            // What came out while they were stopped is not theirs.
            while observed.try_recv().is_ok() {}
            // Let on in either order (xorshift).
            order ^= order << 13;
            order ^= order >> 7;
            order ^= order << 17;
            let publication_first = order & 1 == 0;
            if publication_first {
                drop(publishing);
                drop(looking);
            } else {
                drop(looking);
                drop(publishing);
            }
            let mut ended = Vec::new();
            for _ in 0..2 {
                ended.push(done.recv_timeout(STUCK).unwrap_or_else(|_| panic!("round {round} (publication let on first: {publication_first}): the publication and the estimate preview wait on each other once let on: only {ended:?} finished")));
            }
            assert!(ended.iter().all(|e| e.1), "round {round}: each finishes without a panic: {ended:?}");
            // The stopped publication came out, and the owner answered both slices after it.
            answered(&observed, true);
        }
    }

    /// A panic in the live state's owner loses the state, not the view: the slice fails with
    /// why, and the next input of the same plate is shown whole again with every layer (the
    /// engine sent every part anew), whether the owner panicked idle or with a slice running.
    #[test]
    fn a_panic_in_the_live_states_owner_fails_the_slice_and_the_next_input_shows_it_whole() {
        use crate::print::slice_bench::{plate, setup, SliceRuntime};
        // A block `mm` tall: its plate, its input, and a job for its edits.
        let block = |mm: u32| {
            let script = format!("use mod.model.*\nuse mod.fab.*\nlet block = mod.model.sdf({{bounds: [[-3mm, 0mm, -3mm], [3mm, {mm}mm, 3mm]] cell: 0.1mm field: fn(p) {{ return sdf_box(p - vec3(0, {}mm, 0), vec3(6mm, {mm}mm, 6mm)) }}}})\nobject(block)\n", mm as f64 / 2.0);
            let project = "use mod.fab.*\nscript(\"lost.cad.splash\")\nplace(\"lost\", object: \"block\", at: [80mm 165mm], name: \"Block\")\n";
            let ev = plate(&(project.into(), [("lost.cad.splash".into(), script)].into()));
            let setup = setup(&ev);
            let make: makepad_fab_slice::live::MakeInput = {
                let (ev, setup) = (ev.clone(), setup.clone());
                Arc::new(move || input_for(plate_id(&ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields)))
            };
            let input = SliceInput { plate_id: plate_id(&ev), settings_id: setup.id_for(&ev.plate), parts: ev.plate.parts.len(), make };
            let job = PrintJob { gen: 0, settings: fab::print::PrintSettings::default(), colours: vec![[0.8; 4]], machine: ev.machine.clone(), name: "lost".into(), objects: Arc::from(["block".to_string()]), check: None, fallback: None, catalog: None, tests: None, melts: Vec::new() };
            (input, job)
        };
        let (input, job) = block(2);
        let (taller, taller_job) = block(8);
        let mut runtime = SliceRuntime::new();
        let (tx, rx) = std::sync::mpsc::channel();
        *OBSERVE.lock().unwrap() = Some(tx);
        let live = LiveSlice::start();
        // The view takes the newest publication on every event, as a frame does, until `until`
        // says what it waited for came.
        let mut follow = |until: &mut dyn FnMut(&str, Option<&Arc<LiveResult>>) -> bool| loop {
            let (_, what, _, result) = runtime.recv(&rx);
            let _ = live.latest();
            if until(&what, result.as_ref()) {
                return;
            }
        };
        let whole = |follow: &mut dyn FnMut(&mut dyn FnMut(&str, Option<&Arc<LiveResult>>) -> bool), revision: u64, what_for: &str| {
            let mut whole = None;
            follow(&mut |what, result| {
                assert!(!what.starts_with("failed"), "{what_for}: the slice fails: {what}");
                whole = result.filter(|r| r.complete && r.revision == revision).cloned();
                whole.is_some()
            });
            let whole = whole.unwrap();
            assert_eq!(whole.layers_done, whole.layers_total, "{what_for}: the complete publication carries every layer");
            whole
        };
        let lost = |follow: &mut dyn FnMut(&mut dyn FnMut(&str, Option<&Arc<LiveResult>>) -> bool), what_for: &str| {
            follow(&mut |what, _| {
                let failed = what.starts_with("failed");
                assert!(!failed || what.contains("state was lost"), "{what_for}: the slice fails because the state was lost, not with {what}");
                failed
            });
        };
        let first = whole(&mut follow, set_input(&input, &job, std::time::Instant::now()), "the first slice");
        assert!(first.layers_total > 0, "the block has layers");
        // Idle: the owner panics with the slice whole.
        hub().owner.post(Msg::Panic);
        lost(&mut follow, "an idle owner's panic");
        let again = whole(&mut follow, set_input(&input, &job, std::time::Instant::now()), "the slice after an idle owner's panic");
        assert_eq!(again.layers_total, first.layers_total, "the same plate, the same layers after an idle owner's panic");
        // With another plate's slice running (the panic comes as its input does).
        set_input(&taller, &taller_job, std::time::Instant::now());
        hub().owner.post(Msg::Panic);
        lost(&mut follow, "a running owner's panic");
        let after = whole(&mut follow, set_input(&input, &job, std::time::Instant::now()), "the slice after a running owner's panic");
        assert_eq!(after.layers_total, first.layers_total, "the same plate, the same layers after a running owner's panic");
        *OBSERVE.lock().unwrap() = None;
        drop(live);
        cancel();
    }

    /// A reslice leaves each layer the beads it was last drawn with until its replacement
    /// arrives: the same beads, in their colours, not built again.
    #[test]
    fn a_layer_keeps_its_last_beads_until_its_replacement_arrives() {
        let machine = fab_plate::machine::Machine::dual_nozzle_350();
        let records = |x: f64| Arc::new(line(1, 0.2, x, 0.0, 1.0));
        let built = |slot: &mut Slot| {
            let mut built = 0;
            let shown = slot.rebuild_tiles(&machine, 0, &|_| 0, 1, &mut built);
            (built, shown.iter().filter(|t| t.ghost).count())
        };
        let mut slot = Slot { key: 1, regions: 1, total: 3, supports_done: true, ..Default::default() };
        (0..3).for_each(|i| { slot.layers.insert((i, 0), records(0.0)); });
        assert_eq!(built(&mut slot), (3, 0));
        // Another content starts: the whole part stays as it was drawn.
        slot.renew(2);
        assert_eq!(built(&mut slot), (0, 3));
        // Its first layer arrives: only that layer is built.
        (slot.regions, slot.total) = (1, 3);
        slot.layers.insert((1, 0), records(5.0));
        assert_eq!(built(&mut slot), (1, 2));
        // The next one starts before it draws anything: the layer drawn last stays, not the
        // complete content's older one.
        slot.renew(3);
        assert!(slot.shown(1).is_some_and(|s| s.key == 2));
        assert!(slot.shown(0).is_some_and(|s| s.key == 1));
        assert_eq!(built(&mut slot), (0, 3));
    }

    /// A drag that raises a part's layer count draws each revision's viewed layer above the
    /// last complete content's top: the next revision's slot shows it until it draws its own.
    #[test]
    fn a_superseded_revisions_layer_above_the_complete_content_stays_shown() {
        let mut slot = Slot { key: 1, regions: 1, total: 3, supports_done: true, ..Default::default() };
        (0..3).for_each(|i| layer(&mut slot, i));
        assert!(slot.complete());
        // Revision 2 (5 layers) draws its viewed layer 4, then revision 3 starts.
        slot.renew(2);
        (slot.regions, slot.total) = (1, 5);
        layer(&mut slot, 4);
        slot.renew(3);
        (slot.regions, slot.total) = (1, 5);
        assert!(slot.shown(4).is_some_and(|s| s.key == 2), "layer 4 is revision 2's ghost");
        assert!(slot.shown(1).is_some_and(|s| s.key == 1), "layer 1 is the complete content's ghost");
        assert!(slot.shown(3).is_none());
        // Revision 4 starts before revision 3 drew anything: revision 2's layer stays.
        slot.renew(4);
        assert_eq!(slot.shown_total(), 5);
        assert!(slot.shown(4).is_some_and(|s| s.key == 2));
        // Complete: no ghost is kept.
        (slot.regions, slot.total) = (1, 5);
        (0..5).for_each(|i| layer(&mut slot, i));
        slot.supports_done = true;
        slot.renew(5);
        assert!(slot.good.as_ref().is_some_and(|g| g.key == 4) && slot.newer.is_empty());
    }

    /// A layer never goes back to an older content's plan while a newer one's is kept. Two
    /// contents superseded in a row, each with its own viewed layer, both stay shown: with only
    /// the newest ghost kept, the second one superseded dropped the first one's layer and the
    /// complete content's older plan showed there again. A ghost keeps only the layers no newer
    /// ghost has.
    #[test]
    fn each_layer_shows_the_newest_superseded_plan_of_it() {
        let mut slot = Slot { key: 1, regions: 1, total: 5, supports_done: true, ..Default::default() };
        (0..5).for_each(|i| layer(&mut slot, i));
        // Revision 2 draws its viewed layer 3, revision 3 (the view moved) layer 1, then 4 starts.
        for (key, viewed) in [(2, 3), (3, 1)] {
            slot.renew(key);
            (slot.regions, slot.total) = (1, 5);
            layer(&mut slot, viewed);
        }
        slot.renew(4);
        assert!(slot.shown(3).is_some_and(|s| s.key == 2), "layer 3 is revision 2's, not the complete content's older plan");
        assert!(slot.shown(1).is_some_and(|s| s.key == 3));
        assert!(slot.shown(0).is_some_and(|s| s.key == 1));
        // Revision 4 draws layer 3 too, then 5 starts: revision 2's ghost holds no layer it shows.
        (slot.regions, slot.total) = (1, 5);
        layer(&mut slot, 3);
        slot.renew(5);
        assert!(slot.shown(3).is_some_and(|s| s.key == 4));
        assert_eq!(slot.newer.iter().map(|g| g.key).collect::<Vec<_>>(), [4, 3], "newest first, revision 2's gone");
        // Revision 3's content again (an undo): its layer is its plan, the others' stay.
        slot.renew(3);
        assert!(slot.shown(1).is_some_and(|s| std::ptr::eq(s, &slot)));
        assert!(slot.shown(3).is_some_and(|s| s.key == 4));
        assert!(slot.shown(2).is_some_and(|s| s.key == 1));
    }

    /// The plate's layer numbers come from each part's whole stack: a taller revision's layers
    /// arriving top-down (the viewed layer first) renumber none of the layers shown above them.
    #[test]
    fn a_layer_arriving_below_shown_ones_renumbers_none() {
        let records = |z: f64| Arc::new(line(1, z, 0.0, 0.0, 1.0));
        let mut slot = Slot { key: 1, regions: 1, total: 3, supports_done: true, ..Default::default() };
        (0..3).for_each(|i| { slot.layers.insert((i, 0), records(0.2 * (i + 1) as f64)); });
        slot.renew(2);
        slot.cut = Some(Arc::new((0..6).map(|i| (0.2 * (i + 1) as f64, 0.2, makepad_fab_slice::live::CutLayer::default())).collect()));
        let stack = |slot: &Slot| (0..slot.stack_len()).filter_map(|i| slot.stack_z(i)).map(|z| (z * 1000.0).round() as i64).collect::<Vec<_>>();
        let before = stack(&slot);
        assert_eq!(before, vec![200, 400, 600, 800, 1000, 1200]);
        // The top layer, then the one below it, arrive.
        (slot.regions, slot.total) = (1, 6);
        slot.layers.insert((5, 0), records(1.2));
        assert_eq!(stack(&slot), before);
        slot.layers.insert((4, 0), records(1.0));
        assert_eq!(stack(&slot), before);
    }

    /// Editing one part builds none of another's tiles: the plate numbers its layers by print z,
    /// so the edited part's new stack (taller here) renumbers none of the other's layers. A
    /// settled slice's beads taken in place of a layer's plan's keep the layer's identity: the
    /// next publication builds none of them and holds the slice's beads.
    #[test]
    fn editing_one_part_builds_none_of_anothers_tiles() {
        let machine = fab_plate::machine::Machine::dual_nozzle_350();
        let layers = |slot: &mut Slot, n: u32, x: f64| {
            (slot.regions, slot.total) = (1, n);
            (0..n).for_each(|i| { slot.layers.insert((i, 0), Arc::new(line(i, 0.2 * (i + 1) as f64, x, 0.0, 1.0))); });
        };
        let part = |key: u64, n: u32, x: f64| {
            let mut slot = Slot { key, supports_done: true, ..Default::default() };
            layers(&mut slot, n, x);
            slot
        };
        // The lattice (part 0) and the vase beside it.
        let mut l = LiveState { placement: vec![(1, Pt::default()), (2, Pt::default())], slots: vec![part(1, 6, 0.0), part(2, 4, 20.0)], ..Default::default() };
        // Each part's tiles as a publication builds them: the layers it built.
        let publish = |l: &mut LiveState| -> Vec<usize> {
            let zs = plate_zs(l);
            let plate_layer = |z: f32| zs.partition_point(|&q| q < (z * 1000.0).round() as i64) as u32;
            let blocks = l.slots.iter().map(|s| s.shown_total().div_ceil(TILE_BLOCK)).max().unwrap_or(0);
            l.slots.iter_mut().enumerate().map(|(part, slot)| {
                let mut built = 0;
                slot.rebuild_tiles(&machine, part, &plate_layer, blocks, &mut built);
                built
            }).collect()
        };
        let lattice = |l: &LiveState| l.slots[0].tiles.values().flat_map(|t| t.1.segments().iter().map(|s| s.id())).collect::<Vec<_>>();
        assert_eq!(publish(&mut l), vec![6, 4], "both parts' layers built");
        let before = lattice(&l);
        // The vase is edited: it shows as it was until its new layers come, taller.
        l.slots[1].renew(3);
        l.placement[1].0 = 3;
        assert_eq!(publish(&mut l), vec![0, 0], "the edit's start builds nothing");
        layers(&mut l.slots[1], 8, 30.0);
        assert_eq!(publish(&mut l), vec![0, 8], "the vase's new layers are built, none of the lattice's");
        assert_eq!(lattice(&l), before, "the lattice keeps its beads (and their GPU buffers)");
        // The settled slice's beads of the lattice's layers (plate layer i at 0.2 (i + 1) mm), in
        // place of their plans'.
        let slice: BTreeMap<(usize, u32), (Vec<InstanceSegment>, Arc<[PreviewBlock]>)> = l.slots[0].layer_tiles.iter()
            .map(|(&i, (_, _, beads, runs))| ((0, i), (vec![InstanceSegment::new(Arc::from(beads.data()))], runs.clone()))).collect();
        let layer_z: BTreeMap<u32, f32> = (0..8).map(|i| (i, 0.2 * (i + 1) as f32)).collect();
        let zs = plate_zs(&l);
        let plate_layer = |z: f32| zs.partition_point(|&q| q < (z * 1000.0).round() as i64) as u32;
        assert_eq!(l.slots[0].adopt(0, &plate_layer, &slice, &layer_z), 6, "every lattice layer takes the slice's beads");
        assert_eq!(publish(&mut l), vec![0, 0], "the slice's beads build nothing");
        let held: Vec<u64> = slice.values().flat_map(|(segments, _)| segments).map(InstanceSegment::id).collect();
        assert_eq!(lattice(&l), held, "the lattice's tiles hold the slice's beads, not a copy");
    }

    /// Every field of two records but their G-code line (a layer's own numbering).
    fn same_records(a: &[MoveRecord], b: &[MoveRecord]) -> bool {
        let key = |r: &MoveRecord| (r.t_start.to_bits(), r.t_end.to_bits(), r.kind, r.role, r.p0.map(f32::to_bits), r.p1.map(f32::to_bits), r.tool, r.nozzle, r.filament,
            (r.width.to_bits(), r.height.to_bits(), r.e.to_bits(), r.v_entry.to_bits(), r.v_cruise.to_bits(), r.v_exit.to_bits(), r.accel.to_bits(), r.f_cmd.to_bits()),
            (r.fan_part, r.fan_aux, r.fan_chamber, r.temps, r.t_bed, r.layer, r.z_layer.to_bits(), r.object, r.flags), (r.center.map(f32::to_bits), r.sweep.to_bits(), r.block, r.mode));
        a.len() == b.len() && a.iter().map(key).eq(b.iter().map(key))
    }

    /// The next of a seeded sequence, below `n`.
    fn xorshift(state: &Mutex<u64>, n: u64) -> u64 {
        let mut s = state.lock().unwrap();
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s % n
    }

    /// The wavy (lattice) cube at `size` and `period` mm, alone on the plate, as the app gives
    /// it to the engine.
    fn wavy_cube(size: f64, period: f64, x: f64) -> LiveInput {
        let project = format!("use mod.fab.*\nscript(\"lattice_cube.cad.splash\", params: {{lattice_size: {size}mm lattice_period: {period}mm}})\nplace(\"lattice_cube\", object: \"lattice\", at: [{x}mm 165mm], name: \"Lattice cube\")\n");
        let scripts = [("lattice_cube.cad.splash".to_string(), include_str!("../resources/lattice_cube.cad.splash").to_string())].into();
        let ev = crate::print::slice_bench::plate(&(project, scripts));
        input_for(plate_id(&ev), &crate::print::slice_bench::setup(&ev), Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields))
    }

    /// A shown plan, the same while its paths keep their identity.
    #[derive(Clone)]
    struct Plan(Arc<LivePaths>);
    impl PartialEq for Plan {
        fn eq(&self, o: &Self) -> bool {
            let id = |p: &LivePaths| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                p.identity(&mut h);
                h.finish()
            };
            id(&self.0) == id(&o.0)
        }
    }

    /// A layer's shown plan: its content key, the paths of each region (by identity) and the
    /// revision and pass that planned them.
    type Shown = (u64, Vec<Plan>, Vec<(u64, &'static str)>);

    /// Per layer, each plan it showed in turn, with the newest revision whose input had that
    /// content when it showed.
    type Seen = Vec<Vec<(Shown, Option<u64>)>>;

    /// The inputs as the guard reads them off the stream: the revision whose driver sends now
    /// (its `Started` comes first), and per content key the newest revision whose driver sent it.
    #[derive(Default)]
    struct Fed {
        driver: u64,
        of: BTreeMap<u64, u64>,
    }

    /// What the live state shows of part 0, layer by layer.
    fn shown_now(l: &LiveState) -> Vec<Option<Shown>> {
        let Some(slot) = l.slots.first() else { return Vec::new() };
        (0..slot.shown_total()).map(|i| slot.shown(i).map(|s| {
            let plans = (0..s.regions).filter_map(|r| s.layers.get(&(i, r))).map(|r| Plan(r.clone())).collect();
            (s.key, plans, (0..s.regions).filter_map(|r| s.by.get(&(i, r))).map(|b| (b.revision, b.pass)).collect())
        })).collect()
    }

    /// Where two plans of a layer first differ.
    fn first_difference(a: &[MoveRecord], b: &[MoveRecord]) -> String {
        match (0..a.len().max(b.len())).find(|&k| a.get(k) != b.get(k)) {
            None => "the same records".into(),
            Some(k) => format!("{} vs {} records, first difference at {k}: {:?} | {:?}", a.len(), b.len(), a.get(k), b.get(k)),
        }
    }

    /// An event on the live state, and on `seen` each layer whose shown plan it changed. A
    /// content is its driver's revision's from the first event that sends it: a part takes a
    /// content at its cut, which comes before the revision's contours (read at the contours, a
    /// content back from an older input looked as old as that input for one event).
    fn apply_seen(l: &mut LiveState, e: LiveEvent, fed: &mut Fed, seen: &mut Seen) {
        match &e {
            LiveEvent::Started { revision, .. } => fed.driver = *revision,
            LiveEvent::PartCut { key, .. } | LiveEvent::PartRegions { key, .. } | LiveEvent::PartLayers { key, .. } => {
                fed.of.insert(*key, fed.driver);
            }
            LiveEvent::Contours { parts, .. } => {
                for (key, _) in parts {
                    fed.of.insert(*key, fed.driver);
                }
            }
            LiveEvent::Failed { error, .. } => panic!("the live slice failed: {error}"),
            _ => {}
        }
        if apply(l, e) {
            for (i, now) in shown_now(l).into_iter().enumerate() {
                if seen.len() <= i { seen.resize(i + 1, Vec::new()); }
                if let Some(now) = now.filter(|n| seen[i].last().map(|s| &s.0) != Some(n)) {
                    let content = fed.of.get(&now.0).copied();
                    seen[i].push((now, content));
                }
            }
        }
    }

    /// Applies events until `revision`'s Toolpaths (the engine's own supervision ends a hang,
    /// not a clock here): the live state, and per layer how its shown plan went after each
    /// event. Returns where each layer's list stood as `revision`'s driver started: every
    /// older input's events applied, none of its own.
    fn run_until(l: &mut LiveState, rx: &std::sync::mpsc::Receiver<LiveEvent>, revision: u64, fed: &mut Fed, seen: &mut Seen) -> Vec<usize> {
        let mut started = None;
        loop {
            let e = rx.recv().expect("the engine sends until it settles");
            if matches!(&e, LiveEvent::Started { revision: r, .. } if *r == revision) {
                started = Some(seen.iter().map(|s| s.len().saturating_sub(1)).collect());
            }
            let done = matches!(&e, LiveEvent::Toolpaths { revision: r, .. } if *r == revision);
            apply_seen(l, e, fed, seen);
            if done { return started.expect("a revision's driver starts before its toolpaths"); }
        }
    }

    /// The end-state parity guard (every change to what the live slice plans or shows passes it
    /// first). Seeded edit runs on the wavy cube: size and period steps (other layer counts,
    /// the same bounds), focus moves, releases, inputs that cancel plans mid-way and land on a
    /// protected viewed layer, 1 to 16 planning threads.
    /// Once the last input settles, every layer shows that content's own plan, record for
    /// record a fresh slice's of the same input; a layer, once it shows that content's plan,
    /// never changes again (each layer appears once, as its real plan); and no content's layer
    /// is planned twice with different geometry.
    /// A field part that only moves keeps its content key, so the engine reuses its cut and
    /// plan: the plate offset taken back off its plate map and box rounded to the position's
    /// float grid, which changes at each power of two (32, 64, 128 mm), and a move across one
    /// re-sliced the whole part.
    #[test]
    fn a_moved_field_part_keeps_its_content_key() {
        let content = |x: f64| {
            let input = wavy_cube(10.3, 5.0, x);
            let frame = &input.frames.as_ref().expect("part frames")[0];
            (frame.field.as_ref().expect("a field part").content, frame.offset)
        };
        let (key, at) = content(80.0);
        for x in [30.3, 50.7, 97.15, 133.7, 200.1] {
            let (k, offset) = content(x);
            assert_ne!(offset, at, "the part moved");
            assert_eq!(k, key, "the move to X {x} mm changed the part's content key");
        }
    }

    #[test]
    fn wavy_cube_edits_settle_to_a_fresh_plan() {
        let variants = [(10.0, 5.0), (11.0, 5.0), (10.0, 4.0), (11.5, 4.5)];
        let inputs: Vec<LiveInput> = variants.iter().map(|&(s, p)| wavy_cube(s, p, 80.0)).collect();
        // The reference: each input sliced once from scratch.
        let fresh: Vec<LiveState> = inputs.iter().map(|input| {
            let (e, rx) = cpu_engine();
            let revision = e.set_input_with(input.plate_id, input.settings_id, 1, { let i = input.clone(); Arc::new(move || i.clone()) });
            let mut l = LiveState::default();
            run_until(&mut l, &rx, revision, &mut Fed::default(), &mut Vec::new());
            l
        }).collect();
        // Never a pass on nothing: each fresh slice is the whole cube, every layer of it spanning
        // about the cube's side (its frame runs along the cube's edges).
        for (l, &(size, _)) in fresh.iter().zip(&variants) {
            let slot = &l.slots[0];
            assert!(slot.total >= 40, "the {size} mm cube sliced into {} layers", slot.total);
            for i in 0..slot.total {
                let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                for r in (0..slot.regions).filter_map(|r| slot.layers.get(&(i, r))).flat_map(|r| r.records()).filter(|r| r.kind == MoveKind::Extrude) {
                    for p in [r.p0, r.p1] {
                        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                    }
                }
                let side = 0.8 * size as f32;
                assert!(hi[0] - lo[0] >= side && hi[1] - lo[1] >= side, "the {size} mm cube's layer {i} extrudes only over {lo:?}..{hi:?}");
            }
        }
        // The seeds' thread counts are the process's: the tests after this one get back the count
        // it found.
        let threads = makepad_fab_toolpath::par::threads();
        for seed in 1..=4u64 {
            makepad_fab_toolpath::par::set_threads([16, 4, 16, 2][seed as usize - 1]);
            let rnd = Arc::new(Mutex::new(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1));
            let (e, rx) = cpu_engine();
            let roll = |n: u64| xorshift(&rnd, n);
            let (mut l, mut fed, mut seen) = (LiveState::default(), Fed::default(), Vec::new());
            let mut current = 0;
            for _ in 0..4 + roll(6) {
                match roll(4) {
                    0 | 1 => {
                        current = roll(inputs.len() as u64) as usize;
                        let i = inputs[current].clone();
                        e.set_input_with(i.plate_id, i.settings_id, 1, Arc::new(move || i.clone()));
                    }
                    2 => e.set_focus_z(Some(roll(100) as f64 * 0.1)),
                    _ => e.release(),
                }
                // Mostly short: the next input lands on the viewed layer or mid-window.
                let wait = std::time::Instant::now() + std::time::Duration::from_millis([0, 5, 30, 120][roll(4) as usize]);
                while let Some(left) = wait.checked_duration_since(std::time::Instant::now()) {
                    let Ok(ev) = rx.recv_timeout(left) else { break };
                    apply_seen(&mut l, ev, &mut fed, &mut seen);
                }
            }
            // What each layer showed as the last input's driver started: the older inputs still
            // on their way when it came (an edited part's viewed layer) are drawn before it.
            let last = inputs[current].clone();
            let revision = e.set_input_with(last.plate_id, last.settings_id, 1, Arc::new(move || last.clone()));
            let mark = run_until(&mut l, &rx, revision, &mut fed, &mut seen);
            let (slot, want) = (&l.slots[0], &fresh[current].slots[0]);
            let ctx = format!("seed {seed}, variant {:?}", variants[current]);
            assert_eq!(slot.key, want.key, "{ctx}: the slot holds the last input's content");
            assert!(slot.complete() && slot.good.is_none() && slot.newer.is_empty(), "{ctx}: complete, no ghost left");
            assert_eq!((slot.total, slot.regions, slot.shown_total()), (want.total, want.regions, want.shown_total()), "{ctx}: the fresh slice's layers");
            for i in 0..slot.shown_total() {
                assert!(slot.shown(i).is_some_and(|s| std::ptr::eq(s, slot)), "{ctx}: layer {i} shows the content's own plan");
                for r in 0..slot.regions {
                    let (a, b) = (slot.layers[&(i, r)].records(), want.layers[&(i, r)].records());
                    assert!(same_records(&a, &b), "{ctx}: layer {i} region {r} differs from a fresh slice: {} | fresh {}", plan_summary(&a), plan_summary(&b));
                }
                assert_eq!(slot.supports.get(&i).map(|s| s.records().len()), want.supports.get(&i).map(|s| s.records().len()), "{ctx}: layer {i}'s supports");
            }
            for (i, shown) in seen.iter().enumerate() {
                // Through every edit, a layer never goes back to an older input's content: each
                // plan it shows is of an input at least as new as the last one's.
                let fed: Vec<Option<u64>> = shown.iter().map(|s| s.1).collect();
                let planned: Vec<&Vec<(u64, &str)>> = shown.iter().map(|s| &s.0 .2).collect();
                assert!(fed.windows(2).all(|w| w[0] <= w[1]), "{ctx}: layer {i} went back to an older input's content: inputs {fed:?}, planned by {planned:?}");
                // Once the last content shows a layer, it shows that plan for good.
                let shown = &shown[mark.get(i).copied().unwrap_or(0)..];
                let from = shown.iter().position(|s| s.0 .0 == slot.key).unwrap_or(shown.len());
                assert!(shown[from..].iter().all(|s| s.0 .0 == slot.key), "{ctx}: layer {i} went back to an older content after the last one showed it: inputs {fed:?}, planned by {planned:?}");
                let changes = shown.len().saturating_sub(from + 1);
                let how: Vec<String> = shown[from..].windows(2).flat_map(|w| w[0].0 .1.iter().zip(&w[1].0 .1).map(|(a, b)| first_difference(&a.0.records(), &b.0.records()))).collect();
                assert!(changes == 0, "{ctx}: layer {i} changed {changes} times after the last content showed it (planned by {:?}, last revision {revision}): {how:?}", shown[from..].iter().map(|s| &s.0 .2).collect::<Vec<_>>());
            }
            drop(e);
        }
        makepad_fab_toolpath::par::set_threads(threads);
        let keys: Vec<u64> = fresh.iter().map(|l| l.slots[0].key).collect();
        let replans: Vec<String> = REPLANS.lock().iter().filter(|r| keys.contains(&r.key))
            .map(|r| format!("layer {} {} r{} -> {} r{}", r.layer, r.before.1, r.before.0, r.after.1, r.after.0)).collect();
        assert!(replans.is_empty(), "a content's layer planned twice with different geometry: {replans:?}");
    }

    /// A tall part painted by its features as a script fills them: a stem with a bead part way
    /// up and a foot, the bead and the foot in a second colour (a tool of its own).
    fn beaded_stem(bead: f64) -> LiveInput {
        beaded_stem_filled(bead, "m.fill(foot, second)\nm.fill(bead, second)\n")
    }

    /// [`beaded_stem`] with fills `fills`, and a ring solid round the stem's upper part that
    /// is no feature of it.
    fn beaded_stem_filled(bead: f64, fills: &str) -> LiveInput {
        let script = format!("{}let foot = m.cylinder({{radius: 8mm height: 3mm at: [0, 1.5mm, 0]}})\nlet stem = m.cylinder({{radius: 3mm height: 60mm at: [0, 30mm, 0]}})\nlet bead = m.sphere({{radius: {bead}mm at: [0, 30mm, 0]}})\nlet ring = m.cylinder({{radius: 4mm height: 4mm at: [0, 50mm, 0]}})\nlet body = m.blend([foot stem bead], 1mm)\nlet second = Material{{albedo: [0.9 0.75 0.05] name: \"Second\"}}\n{fills}object(body)\n", fab_model::program::EMPTY);
        let project = "use mod.fab.*\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"body\", at: [150mm 150mm], name: \"Part\")\n".to_string();
        let ev = crate::print::slice_bench::plate(&(project, [("t.cad.splash".to_string(), script)].into()));
        input_for(plate_id(&ev), &crate::print::slice_bench::setup(&ev), Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields))
    }

    /// Slices `was` then `now` and checks the edit kept over a third of the part's layers and
    /// settled to a fresh slice of `now`, layer for layer and region for region.
    fn keeps_a_third_and_settles_fresh(was: &LiveInput, now: &LiveInput) {
        let slice = |inputs: &[&LiveInput]| {
            let (e, rx) = cpu_engine();
            let mut l = LiveState::default();
            for input in inputs {
                let i = (*input).clone();
                let revision = e.set_input_with(i.plate_id, i.settings_id, 1, Arc::new(move || i.clone()));
                run_until(&mut l, &rx, revision, &mut Fed::default(), &mut Vec::new());
            }
            (l, e.part_kept().swap_remove(0))
        };
        let (fresh, _) = slice(&[now]);
        let (l, (kept, none)) = slice(&[was, now]);
        let (slot, want) = (&l.slots[0], &fresh.slots[0]);
        assert!(want.regions > 1, "the fills print in a tool of their own: {} regions", want.regions);
        assert!(none.is_empty() && kept * 3 > u64::from(want.total), "the edit kept {kept} of {} layers{}", want.total, if none.is_empty() { String::new() } else { format!(" (none: {none})") });
        assert_eq!((slot.key, slot.total, slot.regions), (want.key, want.total, want.regions), "the fresh slice's content and layers");
        let plan = |s: &Slot, i: u32, r: u32| s.layers.get(&(i, r)).map(|p| p.records()).unwrap_or_default();
        for i in 0..slot.total {
            for r in 0..slot.regions {
                let (a, b) = (plan(slot, i, r), plan(want, i, r));
                assert!(same_records(&a, &b), "layer {i} region {r} differs from a fresh slice: {} | fresh {}", plan_summary(&a), plan_summary(&b));
            }
        }
    }

    /// A geometry edit of a part painted by its features plans again only the layers the edit
    /// reaches, in its field and in its paint: a bead grown on a stem keeps the plans of the
    /// layers well below and above it, and the slice settles to the fresh one's.
    #[test]
    fn a_feature_painted_parts_geometry_edit_keeps_the_layers_it_leaves() {
        keeps_a_third_and_settles_fresh(&beaded_stem(4.0), &beaded_stem(4.5));
    }

    /// A fill added between a part's others plans again only the layers its box reaches: a ring
    /// filled round a stem's upper part keeps the plans of the layers below it, and the slice
    /// settles to the fresh one's.
    #[test]
    fn a_fill_added_between_others_keeps_the_layers_it_leaves() {
        let was = beaded_stem_filled(4.0, "m.fill(foot, second)\nm.fill(bead, second)\n");
        let now = beaded_stem_filled(4.0, "m.fill(foot, second)\nm.fill(ring, second)\nm.fill(bead, second)\n");
        keeps_a_third_and_settles_fresh(&was, &now);
    }

    /// A small field part with two print colours in its project.
    fn paintable_part() -> fab_ai::Session {
        let script = format!("{}let body = m.blend([m.cylinder({{radius: 8mm height: 12mm at: [0, 6mm, 0]}}) m.cylinder({{radius: 3mm height: 12mm at: [0, 6mm, 0]}})], 1mm)\nobject(body)\n", fab_model::program::EMPTY);
        let project = "use mod.fab.*\ncolor(\"Blue\", rgb: #x1a4fd8)\ncolor(\"Yellow\", rgb: #xe6c00d)\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"body\", at: [150mm 150mm], name: \"Part\", color: \"Blue\")\n";
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::load("t", project, [("t.cad.splash".to_string(), script)].into()).expect("loads");
        session.path = None;
        session.rebuild().expect("builds");
        assert!(session.last.as_ref().unwrap().fields[0].is_some(), "a field part");
        session
    }

    /// One real stroke of the second colour around the part's +X side, at model height `y`.
    fn stroke_across(session: &mut fab_ai::Session, y: f64) -> Arc<Evaluated> {
        let ev = session.last.clone().unwrap();
        let frame = ev.model_to_plate[0];
        let to = |m: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| (0..3).map(|j| frame[i][j] * m[j]).sum::<f64>() + frame[i][3]) };
        let points = (0..20).map(|k| { let a = -0.5 + k as f64 * 0.05; to([8.0 * a.cos(), y, 8.0 * a.sin()]) }).collect();
        let action = fab::api::PartAction::PaintStroke { object: "t/body".into(), rgb: [0.9, 0.75, 0.05], color: Some("Yellow".into()), erase: false, radius: 1.5, points, target: fab::api::PaintTarget::Color };
        let batch = crate::paint_edit::edits(session.compiled.as_ref().unwrap(), &ev, &action, Some(&frame)).unwrap();
        let reply = session.commit("paint", batch);
        assert!(reply.ok, "{}", reply.text);
        session.last.clone().unwrap()
    }

    /// A slice goes by the plate's id: a stroke of a colour already on the part changes no script
    /// and no placement, yet what prints; so does the tool a colour is mapped to, which lands on
    /// the same sources. Undoing the stroke, or evaluating the same sources again, is the same input.
    #[test]
    fn a_strokes_paint_and_its_tools_are_part_of_the_plates_identity() {
        let mut session = paintable_part();
        let before = plate_id(&stroke_across(&mut session, 4.0));
        let after = plate_id(&stroke_across(&mut session, 8.0));
        assert_ne!(before, after, "a second stroke of the same colour");
        assert_eq!(plate_id(&session.rebuild().unwrap()), after, "the same sources again");
        let mut mapped = (**session.last.as_ref().unwrap()).clone();
        mapped.set_mapped(Arc::new(fab_model::eval::Mapped { inks: Default::default(), tools: [("Blue".to_string(), (2, 0)), ("Yellow".to_string(), (1, 1))].into() }));
        assert_ne!(plate_id(&mapped), after, "the colours mapped to other tools");
        assert!(session.undo_redo(fab_model::HistoryAction::Undo).ok);
        assert_eq!(plate_id(session.last.as_ref().unwrap()), before, "the stroke undone");
    }

    /// Strokes landing on one another, each cutting the last one's slice short anywhere: the view
    /// ends on the newest paint, record for record a fresh slice's, nothing of an older stroke
    /// left in any layer, and each layer's outer beads sit on the paint at the layer's own height.
    #[test]
    fn strokes_landing_mid_slice_leave_nothing_of_the_older_ones() {
        let mut session = paintable_part();
        let evs: Vec<Arc<Evaluated>> = [3.0, 5.0, 7.0, 9.0].map(|y| stroke_across(&mut session, y)).into();
        let setup = crate::print::slice_bench::setup(&evs[0]);
        let inputs: Vec<LiveInput> = evs.iter().map(|ev| input_for(plate_id(ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields))).collect();
        let send = |e: &LiveEngine, i: &LiveInput| { let i = i.clone(); e.set_input_with(i.plate_id, i.settings_id, 1, Arc::new(move || i.clone())) };
        let last = inputs.last().unwrap();
        let fresh = {
            let (e, rx) = cpu_engine();
            let revision = send(&e, last);
            let mut l = LiveState::default();
            run_until(&mut l, &rx, revision, &mut Fed::default(), &mut Vec::new());
            l
        };
        let want = &fresh.slots[0];
        for cut_after in [0, 3, 12] {
            let (e, rx) = cpu_engine();
            let (mut l, mut fed, mut seen) = (LiveState::default(), Fed::default(), Vec::new());
            let revision = send(&e, &inputs[0]);
            run_until(&mut l, &rx, revision, &mut fed, &mut seen);
            for input in &inputs[1..3] {
                send(&e, input);
                let wait = std::time::Instant::now() + std::time::Duration::from_millis(cut_after);
                while let Some(ev) = wait.checked_duration_since(std::time::Instant::now()).and_then(|left| rx.recv_timeout(left).ok()) {
                    apply(&mut l, ev);
                }
            }
            let revision = send(&e, last);
            run_until(&mut l, &rx, revision, &mut fed, &mut seen);
            let slot = &l.slots[0];
            assert_eq!(slot.key, want.key, "the newest paint's content");
            assert!(slot.complete() && slot.good.is_none() && slot.newer.is_empty(), "complete, no ghost left");
            assert_eq!((slot.total, slot.regions), (want.total, want.regions));
            for ((at, a), (_, b)) in slot.layers.iter().zip(&want.layers) {
                let (a, b) = (a.records(), b.records());
                assert!(same_records(&a, &b), "layer {at:?} differs from a fresh slice: {} | fresh {}", plan_summary(&a), plan_summary(&b));
            }
        }
        // The beads against the paint itself, at the part's place on the plate.
        let part = &last.plate.parts[0];
        let sp = part.paint.as_ref().expect("painted");
        let states = sp.states(part.filament);
        let at = fresh.placement[0].1;
        let (dx, dy) = (at.x_mm(), at.y_mm());
        let (mut off, mut painted) = (0.0f64, 0.0f64);
        for (&(layer, _), paths) in want.layers.iter().filter(|((layer, _), _)| *layer > 0) {
            let records = paths.records();
            let printed: std::collections::HashSet<u8> = want.layers.iter().filter(|((l, _), _)| *l == layer).flat_map(|(_, r)| r.records().into_iter().filter(|m| m.kind == MoveKind::Extrude).map(|m| m.filament)).collect();
            let mut stretch = 0.0f64;
            for m in records.iter() {
                if m.kind != MoveKind::Extrude || m.role != makepad_fab_toolpath::Role::OuterWall {
                    off = off.max(std::mem::take(&mut stretch));
                    continue;
                }
                painted += (m.filament as u32 != part.filament) as u8 as f64;
                let r = ((m.p0[0] - m.center[0]).powi(2) + (m.p0[1] - m.center[1]).powi(2)).sqrt() as f64;
                let arc = m.flags & makepad_fab_gcode::record::flags::ARC != 0;
                let a0 = ((m.p0[1] - m.center[1]) as f64).atan2((m.p0[0] - m.center[0]) as f64);
                let len = if arc { m.sweep.abs() as f64 * r } else { ((m.p1[0] - m.p0[0]) as f64).hypot((m.p1[1] - m.p0[1]) as f64) };
                let steps = (len / 0.02).ceil().max(1.0) as usize;
                for i in 0..steps {
                    let t = (i as f64 + 0.5) / steps as f64;
                    let (x, y) = if arc {
                        let a = a0 + m.sweep as f64 * t;
                        (m.center[0] as f64 + r * a.cos(), m.center[1] as f64 + r * a.sin())
                    } else {
                        (m.p0[0] as f64 + (m.p1[0] - m.p0[0]) as f64 * t, m.p0[1] as f64 + (m.p1[1] - m.p0[1]) as f64 * t)
                    };
                    let st = states.get((sp.sample_at)([x + dx, y + dy, (m.z_layer - m.height / 2.0) as f64]).slot as usize).copied().unwrap_or(0);
                    let paint = if st == 0 { part.filament as u8 } else { st as u8 };
                    if paint != m.filament && printed.contains(&paint) {
                        stretch += len / steps as f64;
                    } else {
                        off = off.max(std::mem::take(&mut stretch));
                    }
                }
            }
            off = off.max(stretch);
        }
        assert!(painted > 0.0, "the strokes print in their colour");
        assert!(off <= 0.42, "an outer bead's colour {off:.2} mm off the paint");
    }

    /// A slice settles with no frame painted: its continuations go on without Draw or NextFrame
    /// events, and a second slice of the same input plans the same records.
    #[test]
    fn a_slice_settles_without_paint_events() {
        use crate::print::slice_bench::SliceRuntime;
        let script = "use mod.model.*\nuse mod.fab.*\nlet block = mod.model.sdf({bounds: [[-3mm, 0mm, -3mm], [3mm, 0.6mm, 3mm]] cell: 0.1mm field: fn(p) { return sdf_box(p - vec3(0, 0.3mm, 0), vec3(6mm, 0.6mm, 6mm)) }})\nobject(block)\n";
        let project = "use mod.fab.*\nscript(\"no_paint.cad.splash\")\nplace(\"no_paint\", object: \"block\", at: [80mm 165mm], name: \"Block\")\n";
        let ev = crate::print::slice_bench::plate(&(project.into(), [("no_paint.cad.splash".into(), script.into())].into()));
        let input = input_for(plate_id(&ev), &crate::print::slice_bench::setup(&ev), Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields));
        let mut runtime = SliceRuntime::new();
        runtime.withhold_paint();
        let mut plans = Vec::new();
        for _ in 0..2 {
            let (engine, rx) = runtime.engine();
            let i = input.clone();
            let revision = engine.set_input_with(i.plate_id, i.settings_id, 1, Arc::new(move || i.clone()));
            let mut live = LiveState::default();
            loop {
                let event = runtime.recv(&rx);
                if let LiveEvent::Failed { error, .. } = &event { panic!("no-paint slice failed: {error}"); }
                let done = matches!(&event, LiveEvent::Toolpaths { revision: r, .. } if *r == revision);
                apply(&mut live, event);
                if done { break; }
            }
            plans.push(live);
        }
        assert_eq!(runtime.frame_events(), 0, "no Draw or NextFrame event during either complete slice");
        let (got, fresh) = (&plans[0].slots[0], &plans[1].slots[0]);
        assert!(got.complete() && fresh.complete() && fresh.total > 0);
        assert_eq!((got.total, got.regions), (fresh.total, fresh.regions));
        for (key, b) in &fresh.layers {
            let (a, b) = (got.layers[key].records(), b.records());
            assert!(same_records(&a, &b), "no-paint layer {key:?}: {}", first_difference(&a, &b));
        }
    }

    /// Parts scaled by axis (`scale: [1, 0.5, 2]` in their `place()`, along the printer's X, Y
    /// and Z): a box, a ball, and the box turned 30° on the bed, all field parts. Each one's
    /// proven extent is its stretched box; the slicer's field cuts its middle to the stretched
    /// outline (a 20 x 10 mm rectangle, turned for the third, and a 20 x 10 mm ellipse for the
    /// ball); the slice prints each one 40 mm tall and within that outline. The ball's field on
    /// the plate stays a lower bound on the distance to its ellipsoid.
    #[test]
    fn parts_scaled_by_axis_slice_to_their_stretched_outlines() {
        let script = |body: &str| format!("{}let part = {body}\nobject(part)\n", fab_model::program::EMPTY);
        // field parts (a plain box would be a mesh): a 20 mm cube and a 10 mm ball on the bed
        let cube = "m.sdf({bounds: [[-10mm, 0mm, -10mm], [10mm, 20mm, 10mm]] cell: 0.1mm field: fn(p) { return sdf_box(p - vec3(0, 10mm, 0), vec3(20mm, 20mm, 20mm)) }})";
        let ball = "m.sdf({bounds: [[-10mm, 0mm, -10mm], [10mm, 20mm, 10mm]] cell: 0.1mm field: fn(p) { return sdf_sphere(p - vec3(0, 10mm, 0), 10mm) }})";
        let scripts: BTreeMap<String, String> = [("block", cube), ("ball", ball), ("turned", cube)]
            .into_iter()
            .map(|(name, body)| (format!("{name}.cad.splash"), script(body)))
            .collect();
        let project = "use mod.fab.*\nscript(\"block.cad.splash\")\nscript(\"ball.cad.splash\")\nscript(\"turned.cad.splash\")\nplace(\"block\", object: \"part\", at: [100mm 120mm], scale: [1, 0.5, 2])\nplace(\"ball\", object: \"part\", at: [160mm 120mm], scale: [1, 0.5, 2])\nplace(\"turned\", object: \"part\", at: [220mm 120mm], rotate: 30°, scale: [1, 0.5, 2])\n";
        let ev = crate::print::slice_bench::plate(&(project.into(), scripts));
        assert_eq!(ev.plate.parts.len(), 3);
        let (c30, s30) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
        // per part: its box's size, and how far a point (off its middle) is past its outline
        // there, in mm (negative inside)
        let rect = |u: f64, v: f64| (u.abs() - 10.0).max(v.abs() - 5.0);
        let past: [&dyn Fn(f64, f64) -> f64; 3] = [
            &|x, y| rect(x, y),
            // the ellipse's own measure, near enough its distance for points by it
            &|x, y| ((x / 10.0).powi(2) + (y / 5.0).powi(2)).sqrt() * 5.0 - 5.0,
            &|x, y| rect(x * c30 + y * s30, -x * s30 + y * c30),
        ];
        let size = [[20.0, 10.0, 40.0], [20.0, 10.0, 40.0], [20.0 * c30 + 10.0 * s30, 20.0 * s30 + 10.0 * c30, 40.0]];
        let area = [200.0, std::f64::consts::PI * 50.0, 200.0];
        let setup = crate::print::slice_bench::setup(&ev).job_setup(&ev.plate, &ev.machine).expect("a job");
        let mut frames = Vec::new();
        let mut middles = Vec::new();
        for (i, name) in ["block", "ball", "turned"].into_iter().enumerate() {
            let pf = ev.fields[i].as_ref().unwrap_or_else(|| panic!("{name}: a field part"));
            assert!(pf.stretched.is_some(), "{name}: its placement stretches it");
            let b = pf.bounds.unwrap_or_else(|| panic!("{name}: its extent proven"));
            for k in 0..3 {
                let got = b.max[k] - b.min[k];
                assert!(got >= size[i][k] - 1e-6 && got <= size[i][k] + 2.0 * fab_model::eval::EXTENT_TOL + 1e-6, "{name}: its extent along axis {k} is {got}, its stretched box's {}", size[i][k]);
            }
            let mid: [f64; 3] = std::array::from_fn(|k| (b.min[k] + b.max[k]) * 0.5);
            let field = slicer_field(pf).unwrap_or_else(|| panic!("{name}: the slicer's field"));
            let cut = field.source().expect("a contour source").contours(&[mid[2]]).pop().unwrap();
            assert_eq!(cut.len(), 1, "{name}: one outline at its middle");
            let a = cut[0].area_mm2();
            assert!((a - area[i]).abs() < 0.5, "{name}: its middle's area {a} mm², want {}", area[i]);
            let offs: Vec<(f64, f64, f64)> = cut[0].contour.pts.iter().map(|q| {
                let (x, y) = (makepad_fab_slicer::geom::unscaled(q.x) - mid[0], makepad_fab_slicer::geom::unscaled(q.y) - mid[1]);
                (x, y, past[i](x, y))
            }).collect();
            let (inner, outer) = offs.iter().fold((0.0f64, 0.0f64), |(lo, hi), o| (lo.min(o.2), hi.max(o.2)));
            println!("[nu-scale] {name}: middle area {a:.2} mm², outline {inner:.3} to {outer:.3} mm off the stretched one, extent {:?}", std::array::from_fn::<f64, 3, _>(|k| b.max[k] - b.min[k]));
            for (x, y, d) in offs {
                assert!(d.abs() < 0.03, "{name}: its outline's point ({x:.3}, {y:.3}) off its middle is {d:.3} mm off the stretched outline");
            }
            middles.push(std::array::from_fn::<f64, 3, _>(|k| mid[k] + ev.frames[i].offset[k]));
            frames.push(Frame { local: ev.frames[i].local.clone(), offset: ev.frames[i].offset, field: Some(field) });
        }
        // the ball's field on the plate: zero on its ellipsoid (semi-axes 10, 5 and 20 mm), its
        // smallest semi-axis deep at its middle, and never past the distance to the ellipsoid
        let f = ev.fields[1].as_ref().unwrap().on_plate().expect("a field on the plate");
        let c = middles[1];
        let on = |u: f64, v: f64| [c[0] + 10.0 * v.sin() * u.cos(), c[1] + 5.0 * v.sin() * u.sin(), c[2] + 20.0 * v.cos()];
        let n = 120;
        let surface: Vec<[f64; 3]> = (0..n).flat_map(|i| (0..=n).map(move |j| (i, j))).map(|(i, j)| on(i as f64 * std::f64::consts::TAU / n as f64, j as f64 * std::f64::consts::PI / n as f64)).collect();
        for p in surface.iter().step_by(61) {
            assert!(f(p[0], p[1], p[2]).abs() < 1e-6, "the ball's field at its surface {p:?}: {}", f(p[0], p[1], p[2]));
        }
        assert!((f(c[0], c[1], c[2]) + 5.0).abs() < 1e-6, "the ball's field at its middle: {}", f(c[0], c[1], c[2]));
        for i in 0..9 {
            for j in 0..9 {
                for k in 0..9 {
                    let p = [c[0] - 16.0 + 4.0 * i as f64, c[1] - 12.0 + 3.0 * j as f64, c[2] - 28.0 + 7.0 * k as f64];
                    let v = f(p[0], p[1], p[2]);
                    let near = surface.iter().map(|q| ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2)).sqrt()).fold(f64::MAX, f64::min);
                    assert!(v <= near + 1e-6, "the ball's field at {p:?} reads {v}, past its ellipsoid {near} away");
                }
            }
        }
        // the whole slice: each part's own beads (not the skirt round the plate) reach its top
        // layer, at 40 mm (the ball's tip a layer or two under it), and stay within its outline
        let plate = ev.plate.printable();
        let mut res = makepad_fab_slice::live::slice_frames(&setup, &plate, &frames, &[], None).expect("the plate slices");
        let layers = makepad_fab_gcode::layers::LayerTimeline::from_timeline(&mut res.timeline, None);
        use makepad_fab_gcode::record::Role;
        let own = |r: Role| !matches!(r, Role::Skirt | Role::Brim | Role::Support | Role::SupportInterface | Role::SupportTransition);
        let beads: Vec<MoveRecord> = layers.records().filter(|m| m.kind == MoveKind::Extrude && m.e > 0.0 && own(m.role)).collect();
        for (i, name) in ["block", "ball", "turned"].into_iter().enumerate() {
            let c = middles[i];
            let mine: Vec<&MoveRecord> = beads.iter().filter(|m| (m.p1[0] as f64 - c[0]).abs() < 15.0 && (m.p1[1] as f64 - c[1]).abs() < 15.0).collect();
            let top = mine.iter().map(|m| m.p1[2] as f64).fold(0.0, f64::max);
            let under = if i == 1 { 0.6 } else { 1e-3 };
            assert!(top <= 40.0 + 1e-3 && top >= 40.0 - under, "{name}: its top layer at {top} mm, want 40");
            for m in &mine {
                for p in [m.p0, m.p1] {
                    let (x, y) = (p[0] as f64 - c[0], p[1] as f64 - c[1]);
                    assert!(past[i](x, y) < 0.05, "{name}: a bead at ({x:.2}, {y:.2}) off its middle, z {}, {:.2} mm past its outline", p[2], past[i](x, y));
                }
            }
        }
    }
}
