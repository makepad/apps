// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (RelaxCache, Snapshot, Relaxation:
// lines 2342-2660, 3912-3918, 4625-4725).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! The relaxation of one step: the persistent per-node state (`RelaxCache`), the change-detection
//! snapshot, and `Relaxation`, which owns the step's Engine and sweeps the nodes.
//!
//! The C++ `Relaxation` holds `Board&` and derives from `RelaxCache`; here the board is a
//! `b: &mut OBoard` argument of the methods that touch it (Engine methods get `&*b`), the cache is
//! the field `c`, the settings a copy in `s`, the Engine the field `e`.
//!
//! Determinism: every `unordered_map` / `unordered_set` of the reference is a `BTreeMap` /
//! `BTreeSet` here (iteration in key order); where the reference sorts before iterating, the sort
//! stays and is a no-op.

pub mod external;
pub mod nodes;
pub mod ops;
pub mod resample;
pub mod state;
pub mod sweep;
pub mod topology;

use super::engine::{finite_vec, Engine};
use super::geom::{Box2, Vec2};
use super::model::{OBoard, TracePoint};
use super::settings::{OrganicSettings, FORCE_TERM_COUNT};
use std::collections::{BTreeMap, BTreeSet};

/// `RelaxCache::BranchLink`: node (t, k) follows host node (h, hk).
#[derive(Copy, Clone, Debug, Default)]
pub struct BranchLink {
    pub t: i32,
    pub k: i32,
    pub h: i32,
    pub hk: i32,
}

/// `RelaxCache::HopKeepRun` (hopKeepDist).
#[derive(Copy, Clone, Debug)]
pub struct HopKeepRun {
    pub va: i32,
    pub vb: i32,
    pub l0: f32,
    pub seen: f32,
}

impl Default for HopKeepRun {
    fn default() -> HopKeepRun {
        HopKeepRun { va: -1, vb: -1, l0: 0.0, seen: -1.0 }
    }
}

/// `RelaxCache::StraightLine`: the line a + s u, 0 <= s <= L.
#[derive(Copy, Clone, Debug, Default)]
pub struct StraightLine {
    pub a: Vec2,
    pub u: Vec2,
    pub l: f32,
}

/// `RelaxCache::StraightMem` (straightPassRelief).
#[derive(Clone, Debug)]
pub struct StraightMem {
    pub lines: Vec<StraightLine>,
    pub seen: f32,
    pub e0: Vec2,
    pub e1: Vec2,
}

impl Default for StraightMem {
    fn default() -> StraightMem {
        StraightMem { lines: Vec::new(), seen: -1.0, e0: Vec2::default(), e1: Vec2::default() }
    }
}

/// `RelaxCache::StraightPull`.
#[derive(Copy, Clone, Debug, Default)]
pub struct StraightPull {
    pub a: Vec2,
    pub n: Vec2,
    pub w: f32,
}

/// `RelaxCache::BundlePull` (bundleKeep).
#[derive(Copy, Clone, Debug, Default)]
pub struct BundlePull {
    pub q: Vec2,
    pub u: Vec2,
    pub w: f32,
    pub pitch: f32,
}

/// `RelaxCache::SpreadCurve` (wireSpread).
#[derive(Clone, Debug, Default)]
pub struct SpreadCurve {
    pub p: Vec<Vec2>,
    pub w: Vec<f32>,
}

/// `RelaxCache`: what persists from one step to the next.
#[derive(Clone, Debug)]
pub struct RelaxCache {
    // topology (rebuilt when trace / via structure changes)
    pub branch_target: Vec<Vec<u8>>,
    /// Junction host node (trace index << 32 | node) -> branch ends following it (trace index, end node).
    pub host_branches: BTreeMap<u64, Vec<(i32, i32)>>,
    /// Per host trace: branch traces following it.
    pub linked_traces: Vec<Vec<i32>>,
    pub branch_links: Vec<BranchLink>,
    /// Per trace segment: spring weight (junction legs).
    pub seg_w: Vec<Vec<f32>>,
    /// wideTrackSnapDeg: per trace segment the unit normal of its octilinear axis; empty = none.
    pub seg_snap: Vec<Vec<Vec2>>,
    /// wideTrackLinePull: per trace node the line (point, unit normal) of its stretch; (0,0) = none.
    pub node_line: Vec<Vec<(Vec2, Vec2)>>,
    /// hopKeepDist: per trace segment the length floor of this step (0 = none).
    pub seg_floor: Vec<Vec<f32>>,
    pub hop_keep: BTreeMap<i32, HopKeepRun>,
    /// straightPassRelief: per trace id the straight stretches as first seen.
    pub straight_mem: BTreeMap<i32, StraightMem>,
    pub straight_pull: Vec<Vec<StraightPull>>,
    /// bundleKeep: per trace node up to two pulls of this step.
    pub bundle_pull: Vec<Vec<[BundlePull; 2]>>,
    pub seg_pass: Vec<Vec<f32>>,
    /// wireSpread: per trace id the target curve of the last field update.
    pub spread_curve: BTreeMap<i32, SpreadCurve>,
    pub spread_pitch: BTreeMap<i32, f32>,
    pub spread_via_home: Vec<Vec2>,
    pub spread_bus: BTreeSet<u64>,
    pub spread_live: bool,
    pub spread_updates: i32,
    pub spread_age: i32,
    pub via_members: Vec<Vec<(i32, i32)>>,
    pub via_pinned: Vec<u8>,
    /// Escape via without copper link to its pad.
    pub via_orphan: Vec<u8>,
    /// Per trace: no resampling (topology freeze).
    pub frozen: Vec<u8>,
    // sleep state
    /// Per node: evaluations without motion.
    pub quiet: Vec<Vec<u8>>,
    /// Per node: position at its last motion.
    pub anchor: Vec<Vec<Vec2>>,
    /// Per node: global substep of its last motion.
    pub last_move: Vec<Vec<i32>>,
    /// Per trace: last substep any last_move was set.
    pub tr_touch: Vec<i32>,
    /// Per trace: movable nodes with quiet < K_QUIET.
    pub tr_awake: Vec<i32>,
    pub vquiet: Vec<u8>,
    pub vanchor: Vec<Vec2>,
    pub dcs: f32,
    pub dnx: i32,
    pub dny: i32,
    /// Per coarse cell: last substep it was dirtied.
    pub dirty_stamp: Vec<i32>,
    /// Recently dirtied cells (cell, substep).
    pub dirty_list: Vec<(i32, i32)>,
    /// Cells dirtied during the current step.
    pub step_dirty: Vec<i32>,
    /// Per coarse cell: last substep it was in focus.
    pub focus_stamp: Vec<i32>,
    /// Global substep counter.
    pub sub: i32,
    /// >= 0 while a drag is active.
    pub focus_since: i32,
    // resampling backoff + pending work
    pub resample_level: Vec<i32>,
    pub resample_next: Vec<i32>,
    /// Per trace: resample pass pending.
    pub resample_flag: Vec<u8>,
    /// Per trace: passes in a row that only had rejected merges.
    pub resample_tries: Vec<u8>,
    /// Per net id: changed since the last loop simplification.
    pub net_dirty: Vec<u8>,
    /// Motion since the dynamic grid was built.
    pub drift: f32,
    /// Budget: where the next sweep starts.
    pub cursor_t: i32,
    pub cursor_v: i32,
    // intra-pair skew tuning (updatePairSkew)
    pub crinkle_amp: Vec<f32>,
    pub crinkle_bends: Vec<Vec<Vec2>>,
    pub crinkle_best: Vec<f32>,
    pub crinkle_stall: Vec<i32>,
    pub crinkle_last_len: Vec<f32>,
    pub crinkle_wait: Vec<i32>,
    pub crinkle_cpl_ref: Vec<f32>,
    pub crinkle_trace_ref: Vec<Vec<(i32, f32)>>,
    pub crinkle_blocked: Vec<u8>,
    pub tip_mem: Vec<Vec<Vec2>>,
    pub trace_tips: Vec<Vec<f32>>,
}

impl Default for RelaxCache {
    fn default() -> RelaxCache {
        RelaxCache {
            branch_target: Vec::new(),
            host_branches: BTreeMap::new(),
            linked_traces: Vec::new(),
            branch_links: Vec::new(),
            seg_w: Vec::new(),
            seg_snap: Vec::new(),
            node_line: Vec::new(),
            seg_floor: Vec::new(),
            hop_keep: BTreeMap::new(),
            straight_mem: BTreeMap::new(),
            straight_pull: Vec::new(),
            bundle_pull: Vec::new(),
            seg_pass: Vec::new(),
            spread_curve: BTreeMap::new(),
            spread_pitch: BTreeMap::new(),
            spread_via_home: Vec::new(),
            spread_bus: BTreeSet::new(),
            spread_live: false,
            spread_updates: 0,
            spread_age: 0,
            via_members: Vec::new(),
            via_pinned: Vec::new(),
            via_orphan: Vec::new(),
            frozen: Vec::new(),
            quiet: Vec::new(),
            anchor: Vec::new(),
            last_move: Vec::new(),
            tr_touch: Vec::new(),
            tr_awake: Vec::new(),
            vquiet: Vec::new(),
            vanchor: Vec::new(),
            dcs: 1.25,
            dnx: 1,
            dny: 1,
            dirty_stamp: Vec::new(),
            dirty_list: Vec::new(),
            step_dirty: Vec::new(),
            focus_stamp: Vec::new(),
            sub: 0,
            focus_since: -1,
            resample_level: Vec::new(),
            resample_next: Vec::new(),
            resample_flag: Vec::new(),
            resample_tries: Vec::new(),
            net_dirty: Vec::new(),
            drift: 0.0,
            cursor_t: 0,
            cursor_v: 0,
            crinkle_amp: Vec::new(),
            crinkle_bends: Vec::new(),
            crinkle_best: Vec::new(),
            crinkle_stall: Vec::new(),
            crinkle_last_len: Vec::new(),
            crinkle_wait: Vec::new(),
            crinkle_cpl_ref: Vec::new(),
            crinkle_trace_ref: Vec::new(),
            crinkle_blocked: Vec::new(),
            tip_mem: Vec::new(),
            trace_tips: Vec::new(),
        }
    }
}

/// `Snapshot::Tr`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapTr {
    pub id: i32,
    pub net: i32,
    pub layer: i32,
    pub width: u32,
    pub routed: i32,
    pub off: i32,
    pub n: i32,
    /// ADAPTED: the trace's clearance-table row (a rule change is an input change).
    pub rule: u16,
}

/// `Snapshot::Pt`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapPt {
    pub x: u32,
    pub y: u32,
    pub term: i32,
    pub via: i32,
    pub br_t: i32,
    pub br_i: i32,
    pub fixed: u8,
    pub straight: u8,
}

impl SnapPt {
    pub fn same_flags(&self, p: &TracePoint) -> bool {
        self.term == p.terminal_pad
            && self.via == p.attached_via
            && self.br_t == p.branch_trace
            && self.br_i == p.branch_point
            && self.fixed == p.is_fixed as u8
            && self.straight == p.straight_before as u8
    }
}

/// `Snapshot::Vi`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapVi {
    pub x: u32,
    pub y: u32,
    pub dia: u32,
    pub drill: u32,
    pub id: i32,
    pub net: i32,
    pub pad: i32,
    pub l0: i32,
    pub l1: i32,
    pub fixed: u8,
    /// ADAPTED: the via's clearance-table row.
    pub rule: u16,
}

/// `Snapshot::Pd`. ADAPTED: `mask` is the port's u64 stack-index mask (the reference's is u32),
/// and `rule` the pad's clearance-table row.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapPd {
    pub cx: u32,
    pub cy: u32,
    pub sx: u32,
    pub sy: u32,
    pub drill: u32,
    pub rot: u32,
    pub lx: u32,
    pub ly: u32,
    pub rr: u32,
    pub dx: u32,
    pub dy: u32,
    pub shape: i32,
    pub net: i32,
    pub mask: u64,
    pub smd: u8,
    pub rule: u16,
}

/// Change-detection snapshot of everything the relaxer reads (taken at the end of a step).
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub tr: Vec<SnapTr>,
    pub pt: Vec<SnapPt>,
    pub vi: Vec<SnapVi>,
    pub pd: Vec<SnapPd>,
    pub global: u64,
}

/// `Relaxation::ExtResult`.
#[derive(Copy, Clone, Debug, Default)]
pub struct ExtResult {
    pub full: bool,
    pub topo: bool,
    pub any: bool,
    pub changed_nodes: i32,
}

/// `OrganicRelaxer::ForceRec` (force recording; written only when a record mask is set).
#[derive(Copy, Clone, Debug)]
pub struct ForceRec {
    pub t: i32,
    pub k: i32,
    pub via: i32,
    pub trace_id: i32,
    pub at: Vec2,
    pub f: [Vec2; FORCE_TERM_COUNT],
}

impl Default for ForceRec {
    fn default() -> ForceRec {
        ForceRec { t: -1, k: -1, via: -1, trace_id: -1, at: Vec2::default(), f: [Vec2::default(); FORCE_TERM_COUNT] }
    }
}

/// `Relaxation::SweepLog`: the shared side effects of node moves. The serial sweeps apply them at
/// once (`defer == false`); the tiled sweeps (L3) record them per tile. ADAPTED: the C++ scratch
/// pointer `X` is not a field: a sweep passes its scratch alongside the log.
#[derive(Clone, Debug, Default)]
pub struct SweepLog {
    pub defer: bool,
    /// (trace, +1 / -1).
    pub awake: Vec<(i32, i32)>,
    /// (trace, substep).
    pub touch: Vec<(i32, i32)>,
    /// tr_box expansion.
    pub boxes: Vec<(i32, Vec2)>,
    /// Newly dirtied cells.
    pub dirty: Vec<i32>,
    /// flag_resample.
    pub resample: Vec<i32>,
    /// flag_net.
    pub nets: Vec<i32>,
    pub max_move: f32,
    pub eval_nodes: i32,
    pub eval_vias: i32,
    pub forces: Vec<ForceRec>,
}

impl SweepLog {
    pub fn clear(&mut self) {
        self.forces.clear();
        self.awake.clear();
        self.touch.clear();
        self.boxes.clear();
        self.dirty.clear();
        self.resample.clear();
        self.nets.clear();
        self.max_move = 0.0;
        self.eval_nodes = 0;
        self.eval_vias = 0;
    }
}

/// `Relaxation::TraceSweep`: per-trace sweep parameters (meander forcing, pair coupling, crinkles).
/// ADAPTED: `crinkle` is the trace index whose `trace_tips` row applies (the C++ holds a pointer).
#[derive(Clone, Debug, Default)]
pub struct TraceSweep {
    pub meander: bool,
    pub amp: f32,
    pub couple: bool,
    pub crinkle: Option<usize>,
    pub quiet: Vec<u8>,
    /// ADAPTED: pour hook 4, a pour path follows: no shape term pulls it (see `relax_node`).
    pub follow: bool,
}

/// Evaluations without motion before a node sleeps.
pub const K_QUIET: u8 = 4;
/// Displacements below 0.02 um are not applied.
pub const K_MIN_APPLY2: f32 = 2e-5f32 * 2e-5f32;
/// 1 um: motion threshold (anchor distance).
pub const K_SETTLE: f32 = 1e-3;
pub const K_SETTLE2: f32 = K_SETTLE * K_SETTLE;
/// mm around externally changed copper (drag).
pub const K_FOCUS_RADIUS: f32 = 4.0;
pub const K_DIRTY_LIST_MAX: i32 = 96;
pub const K_PAIR_NODE_SPACING: f32 = 0.5;
/// ADAPTED: pour hook 7. A pour-row trace resamples at this multiple of the node spacing: it
/// follows and never pulls (hook 4), so its nodes carry only clearance.
pub const K_POUR_NODE_SPACING_SCALE: f32 = 2.0;

/// One step's relaxation.
#[derive(Clone, Debug)]
pub struct Relaxation {
    pub c: RelaxCache,
    pub s: OrganicSettings,
    pub e: Engine,
    /// == sub while sweeping.
    pub sub_index: i32,
    /// Largest node displacement of the current substep (grid staleness).
    pub sub_max_move: f32,
    /// Per trace: bbox of its nodes during this step (for skipping).
    pub tr_box: Vec<Box2>,
    /// Remaining evaluation units of this substep (-1: unlimited).
    pub budget: i64,
    pub budget_hit: bool,
    pub eval_nodes: i32,
    pub eval_vias: i32,
    /// Resampling changed point counts: rebuild topology at the end.
    pub topo_dirty: bool,
    /// Set by the step: the board changed from outside since the last step.
    pub skew_reset: bool,
    pub rec_mask: u32,
    pub rec_at_eval: bool,
    pub step_forces: Vec<ForceRec>,
}

impl Relaxation {
    /// `Relaxation(Board&, const OrganicSettings&)`.
    pub fn new(b: &OBoard, s: &OrganicSettings) -> Relaxation {
        Relaxation {
            c: RelaxCache::default(),
            s: s.clone(),
            e: Engine::new(b, s),
            sub_index: 0,
            sub_max_move: 0.0,
            tr_box: Vec::new(),
            budget: -1,
            budget_hit: false,
            eval_nodes: 0,
            eval_vias: 0,
            topo_dirty: false,
            skew_reset: false,
            rec_mask: 0,
            rec_at_eval: false,
            step_forces: Vec::new(),
        }
    }

    /// `hostKey(t, k)`.
    #[inline]
    pub fn host_key(t: i32, k: i32) -> u64 {
        ((t as u32 as u64) << 32) | (k as u32 as u64)
    }

    /// `computeTraceBoxes`.
    pub fn compute_trace_boxes(&mut self, b: &OBoard) {
        self.tr_box = vec![Box2::default(); b.traces.len()];
        for (t, tr) in b.traces.iter().enumerate() {
            for p in &tr.points {
                if finite_vec(p.pos) {
                    self.tr_box[t].expand(p.pos);
                }
            }
        }
    }
}
