// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.h (OrganicRelaxer, StepInfo) and
// src/organic/OrganicRelaxer.cpp (Cache 6025-6054, reset 6073-6076, step 6203-6498, the full rebuild of
// updateSplines 6886-7304).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! The relaxer: one `step` relaxes the awake nodes of a board by guarded Gauss-Seidel sweeps,
//! resamples, rebuilds what topology changes invalidate, and refreshes only the curves read by
//! subsequent steps. It keeps its indices, topology and sleep state between steps and compares
//! the board with what it left behind (`applyExternal`), so a settled board costs one comparison pass.
//!
//! Per step: the budget bounds the sweeps; fixed curve work covers only traces of paired nets
//! when coupling and skew tuning are both on, and target-length nets when matching is on. This
//! includes pour rows of those nets. No curve Engine is built when that set is empty. Call
//! `update_splines` before exporting: it rebuilds every non-pour trace, leaving pour curves alone.
//!
//! ADAPTED, each refused at the door instead of silently ignored:
//! - the work budget is in evaluation units (one trace node = 1, one via = 3), never ms; the
//!   reference's kUnitsPerMs (250) is the caller's conversion. A step needs a budget: the
//!   unbudgeted (tiled, parallel) sweeps are L3;
//! - settings that turn on a term whose code is not ported yet are refused (`unported`);
//! - no clock: StepInfo has no timings (the caller measures);
//! - updateSplines is the full non-pour rebuild on demand; the step uses a mask for its readers
//!   (the spline cache with its hinted updates is L3; neckdowns and teardrops are L4);
//! - force recording and probing (I8) are not ported (diagnostics, L8).

use super::engine::{Engine, EngineCache};
use super::model::OBoard;
use super::relax::ops::{compute_branch_targets, normalize_vias, pin_pair_approaches, remove_duplicate_nodes, sync_branches};
use super::relax::state::{differs_from_snapshot, frozen_mask_of, global_hash, take_snapshot};
use super::relax::{RelaxCache, Relaxation, Snapshot};
use super::settings::{relax_immutable, OrganicSettings};
use super::simplify::{prune_dangling_vias, simplify_all_nets, simplify_net};
use super::spline::{build_curve, clearance_pass};

/// `OrganicRelaxer::StepInfo` (without its wall-clock fields).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StepInfo {
    pub step: i32,
    /// Returned without work (settled, nothing changed).
    pub idle: bool,
    /// The board was changed since the previous step.
    pub external: bool,
    /// Stopped by the work budget (work continues next step).
    pub budget_limited: bool,
    /// Nodes moved externally since the previous step.
    pub changed_nodes: i32,
    /// Trace node evaluations (all substeps).
    pub evaluated_nodes: i32,
    pub evaluated_vias: i32,
    /// Nodes and vias still awake after the step.
    pub awake: i64,
}

/// Why a step was refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StepError {
    /// A setting turns on a term that is not ported yet (its name).
    Unported(&'static str),
    /// The unbudgeted (tiled) step is not ported yet (L3): give a work budget.
    Unbudgeted,
}

/// `OrganicRelaxer::Cache`: the persistent state, copied with the relaxer.
#[derive(Clone, Debug, Default)]
struct Cache {
    valid: bool,
    eng: EngineCache,
    rel: RelaxCache,
    snap: Snapshot,
    prev_snap: Snapshot,
    snap_taken: bool,
    info: StepInfo,
}

/// `OrganicRelaxer`.
#[derive(Clone, Debug, Default)]
pub struct OrganicRelaxer {
    pub settings: OrganicSettings,
    /// Topology freeze for interactive edits: these traces (by id) and every trace of
    /// `frozen_nets` are not resampled or deduplicated; their free nodes still relax.
    pub frozen_traces: Vec<i32>,
    pub frozen_nets: Vec<i32>,
    step_count: i32,
    cache: Option<Box<Cache>>,
}

impl OrganicRelaxer {
    pub fn new(settings: OrganicSettings) -> OrganicRelaxer {
        OrganicRelaxer { settings, ..OrganicRelaxer::default() }
    }

    /// `reset`: forget everything; the next step starts from scratch.
    pub fn reset(&mut self) {
        self.cache = None;
    }

    /// `lastStepInfo`.
    pub fn last_step_info(&self) -> StepInfo {
        self.cache.as_ref().map_or(StepInfo::default(), |c| c.info)
    }

    /// The first setting that turns on a term whose code is not ported yet.
    pub fn unported(s: &OrganicSettings) -> Option<&'static str> {
        let on = [
            (s.wire_spread > 0.0, "wire_spread"),
            (s.enable_biarcs, "enable_biarcs"),
            (s.enable_dynamic_neckdown, "enable_dynamic_neckdown"),
            (s.enable_teardrops, "enable_teardrops"),
        ];
        on.iter().find(|(on, _)| *on).map(|(_, name)| *name)
    }

    fn cache(&mut self) -> &mut Cache {
        self.cache.get_or_insert_with(Box::default)
    }

    /// `step`: one relaxation step under a work budget of `budget_units` evaluation units.
    /// Control points and topology are current on success. Curves (`smoothed`, `length`,
    /// `point_widths`) and nominal node widths are refreshed at the end only for traces whose
    /// nets have a partner with coupling and skew tuning on, or a positive target length with
    /// target-length matching on, including pour rows. Other exports may be stale; call
    /// `update_splines` before reading non-pour curves or node widths. An idle step changes nothing.
    ///
    /// The loop simplifier copies a node width into each split point (simplify.rs), but no step
    /// decision depends on it: the full rebuild before each lift rewrites all non-pour node
    /// widths, and no result reads a pour row's node widths. Deferring that reset cannot reach
    /// the lifted result.
    pub fn step(&mut self, board: &mut OBoard, budget_units: i64) -> Result<(), StepError> {
        if let Some(name) = Self::unported(&self.settings) {
            return Err(StepError::Unported(name));
        }
        if budget_units <= 0 {
            return Err(StepError::Unbudgeted);
        }
        self.step_count += 1;
        let step_count = self.step_count;
        let s = self.settings.clone();
        let mut info = StepInfo { step: step_count, ..StepInfo::default() };

        // topology freeze (interactive drag): frozen nets = listed nets + nets of listed traces
        let mut frozen_net_ids = self.frozen_nets.clone();
        if !self.frozen_traces.is_empty() {
            for tr in &board.traces {
                if self.frozen_traces.contains(&tr.id) {
                    frozen_net_ids.push(tr.net);
                }
            }
        }
        frozen_net_ids.sort_unstable();
        frozen_net_ids.dedup();
        let is_frozen_net = |net: i32| frozen_net_ids.binary_search(&net).is_ok();

        let gh = global_hash(board, &s);
        let simplify_tick = s.enable_loop_simplification && step_count % 20 == 0;

        // ---- idle: settled and nothing changed since the previous step -> nothing to do ----
        {
            let c = self.cache();
            if c.valid && (c.rel.focus_since < 0) == frozen_net_ids.is_empty() {
                let mut pending = c.info.awake > 0;
                if !pending && simplify_tick {
                    for n in 0..c.rel.net_dirty.len() {
                        if c.rel.net_dirty[n] != 0 && !is_frozen_net(n as i32) {
                            pending = true;
                            break;
                        }
                    }
                }
                // (the P1d wire-spread field updates never pend: wire_spread is refused above)
                if !pending {
                    // ADAPTED (OrganicRelaxer.cpp:6235-6237): an immutable trace (locked, kept imported:
                    // `frozen`) is skipped too. resample never clears its flag (5776 skips it before
                    // 5789), so in the reference a board with locked copper never reports idle; the
                    // steps it runs instead change nothing.
                    let n = std::cmp::min(c.rel.resample_flag.len(), board.traces.len());
                    for t in 0..n {
                        if c.rel.resample_flag[t] != 0
                            && step_count >= c.rel.resample_next[t]
                            && !is_frozen_net(board.traces[t].net)
                            && !(t < c.rel.frozen.len() && c.rel.frozen[t] != 0)
                        {
                            pending = true;
                            break;
                        }
                    }
                }
                if !pending && !differs_from_snapshot(&c.snap, board, gh) {
                    info.idle = true;
                    c.info = info;
                    return Ok(());
                }
            }
        }

        // ---- input normalisation (may change the board; detected below like any edit) ----
        normalize_vias(board);
        // H6b: a fresh relaxer keeps the router's paired pad approaches.
        if !self.cache.as_ref().is_some_and(|c| c.valid) {
            pin_pair_approaches(board, &s);
        }
        // K4: immutable traces (locked / kept imported routing): every node pinned, never
        // resampled (frozen mask) and their nets never rebuilt by the loop simplifier.
        let mut keep_nets = frozen_net_ids.clone();
        for tr in &mut board.traces {
            // ADAPTED: pour hook 3, preserve the net's traces without making the pour path immutable.
            if board.pour_rules.binary_search(&tr.rule).is_ok() {
                keep_nets.push(tr.net);
            }
            if tr.imported && !tr.locked && s.edit_imported {
                tr.imported = false;
            }
            if !relax_immutable(tr, &s) {
                continue;
            }
            for p in &mut tr.points {
                if !p.is_fixed {
                    p.is_fixed = true;
                }
            }
            keep_nets.push(tr.net);
        }
        for v in &mut board.vias {
            // existing vias never move (the reader marks them is_fixed too)
            if (v.locked || v.imported) && !v.is_fixed {
                v.is_fixed = true;
            }
        }
        keep_nets.sort_unstable();
        keep_nets.dedup();
        let mut frozen_mask = frozen_mask_of(board, &frozen_net_ids, &s);
        let cache_valid = self.cache.as_ref().is_some_and(|c| c.valid);
        {
            let mut dup = false;
            for t in 0..board.traces.len() {
                if dup {
                    break;
                }
                let pts = &board.traces[t].points;
                if pts.len() < 3 || frozen_mask[t] != 0 {
                    continue;
                }
                for k in 0..pts.len() - 1 {
                    if pts[k].pos.distance_squared_to(pts[k + 1].pos) < 1e-12f32 {
                        dup = true;
                        break;
                    }
                }
            }
            if dup {
                let bt = compute_branch_targets(board);
                remove_duplicate_nodes(board, &bt, &frozen_mask);
            }
        }
        if s.enable_loop_simplification && (!cache_valid || simplify_tick) {
            // only nets whose copper changed since their last pass can have new loops / crossings
            let mut simplified = 0;
            if !cache_valid {
                simplified = simplify_all_nets(board, if keep_nets.is_empty() { None } else { Some(keep_nets.as_slice()) });
            } else {
                let c = self.cache();
                for n in 0..std::cmp::min(board.nets.len(), c.rel.net_dirty.len()) {
                    if c.rel.net_dirty[n] == 0 || keep_nets.binary_search(&(n as i32)).is_ok() {
                        continue;
                    }
                    c.rel.net_dirty[n] = 0;
                    if simplify_net(n as i32, board) {
                        simplified += 1;
                    }
                }
                if simplified > 0 {
                    prune_dangling_vias(board);
                }
            }
            if simplified > 0 {
                normalize_vias(board);
                frozen_mask = frozen_mask_of(board, &frozen_net_ids, &s);
            }
        }
        sync_branches(board);

        let mut r = Relaxation::new(board, &s);
        let c = self.cache.get_or_insert_with(Box::default);
        r.e.c = std::mem::take(&mut c.eng);
        r.c = std::mem::take(&mut c.rel);
        // drag focus: while a net is frozen (being dragged) only the region around its changes relaxes
        if frozen_net_ids.is_empty() {
            r.c.focus_since = -1;
        } else if r.c.focus_since < 0 {
            r.c.focus_since = r.c.sub; // cells marked from now on (stamp >= sub) are in focus
        }

        let ext = r.apply_external(board, &c.snap, c.valid, gh);
        r.c.frozen = frozen_mask;
        r.spring_weights(board);
        r.compute_trace_boxes(board);
        info.external = ext.any;
        info.changed_nodes = ext.changed_nodes;
        // (P1d wire spreading: refused above, L5)

        r.skew_reset = ext.any && c.valid;
        r.update_pair_skew(board);
        r.rec_mask = 0;
        r.rec_at_eval = false;
        let subs = std::cmp::max(1, s.substeps_per_frame);
        // Deterministic work budget: evaluation units (one trace node = 1, one via = 3). Work left
        // over stays awake for the next step (the sweeps resume where they stopped).
        let mut budget_total: i64 = std::cmp::max(1, budget_units);
        for si in 0..subs {
            r.c.sub += 1;
            r.sub_index = r.c.sub;
            r.prune_dirty_list(r.sub_index - 1);
            if r.c.drift >= r.e.max_step as f32 {
                r.e.build_dynamic(board, None, 0.0, None);
                r.c.drift = 0.0;
            }
            if budget_total >= 0 {
                r.budget = std::cmp::max(1, budget_total / (subs - si) as i64);
            }
            let b0 = r.budget;
            r.sub_max_move = 0.0;
            // budgeted (interactive) steps: serial sweeps in trace / via order, resumable
            r.sweep_traces(board);
            r.sweep_vias(board);
            r.sync_branch_links(board);
            r.c.drift += r.sub_max_move;
            if budget_total >= 0 {
                budget_total -= std::cmp::max(0, b0 - std::cmp::max(0, r.budget));
            }
        }
        info.budget_limited = r.budget_hit;
        if r.c.drift >= r.e.max_step as f32 {
            r.e.build_dynamic(board, None, 0.0, None);
            r.c.drift = 0.0;
        }
        r.resample(board, step_count);
        if r.topo_dirty {
            r.build_topology(board);
            r.e.build_dynamic(board, None, 0.0, None);
            r.c.drift = 0.0;
            r.e.compute_essential(board, None, 0.0);
        }
        r.hop_keep_seen(board); // O8b: run lengths as this step leaves them
        r.straight_seen(board); // FA25: the tracks as this step leaves them
        info.evaluated_nodes = r.eval_nodes;
        info.evaluated_vias = r.eval_vias;
        info.awake = r.awake_count(board);

        let c = self.cache.get_or_insert_with(Box::default);
        c.eng = std::mem::take(&mut r.e.c);
        c.rel = std::mem::take(&mut r.c);
        c.valid = true;
        std::mem::swap(&mut c.snap, &mut c.prev_snap);
        take_snapshot(&mut c.snap, board, gh);
        c.snap_taken = true;
        c.info = info;
        // Keep the readers' curves at the same end-of-step point as the former full rebuild:
        // pair skew and target-length matching consume these on the next step.
        let pairs = s.enable_diff_pair_coupling && s.enable_diff_pair_skew_tuning;
        if pairs || s.enable_target_length_matching {
            let mask: Vec<u8> = board.traces.iter().map(|tr| {
                u8::from(board.nets.get(tr.net as usize).is_some_and(|net| {
                    // Match Engine::partner_of's membership checks without building an Engine.
                    (pairs && net.is_diff_pair && net.diff_pair_partner >= 0
                        && (net.diff_pair_partner as usize) < board.nets.len()
                        && net.diff_pair_partner != tr.net)
                        || (s.enable_target_length_matching && net.target_length > 0.0)
                }))
            }).collect();
            self.update_splines_masked(board, &mask);
        }
        Ok(())
    }

    /// Full on-demand rebuild of all non-pour curves (`smoothed`, `length`, `point_widths`) and
    /// nominal node widths. Call before reading or exporting them after `step` or a board edit.
    /// Pour rows remain obstacles but are not rebuilt: the step maintains those needed by its
    /// readers, and the lift never exports them. This does not change the relaxer's cached state.
    pub fn update_splines(&mut self, board: &mut OBoard) {
        let mask: Vec<u8> = board.traces.iter()
            .map(|tr| u8::from(board.pour_rules.binary_search(&tr.rule).is_err()))
            .collect();
        self.update_splines_masked(board, &mask);
    }

    /// Rebuild selected outputs against every trace's control geometry, including pour paths.
    fn update_splines_masked(&self, board: &mut OBoard, mask: &[u8]) {
        if !mask.iter().any(|&selected| selected != 0) {
            return;
        }
        let s = &self.settings;
        let sub = std::cmp::max(1, s.curve_subdivisions);
        let need_slack = sub > 1 && s.guard_splines;
        let mut all_slack: Vec<Vec<f64>> = Vec::new();
        if need_slack || s.enable_dynamic_neckdown {
            let mut e = Engine::new(board, s);
            e.build_static(board, true);
            // (the reach argument limits a partial index only; the full rebuild indexes everything)
            e.build_dynamic(board, None, 0.0, None);
            // (the chord gaps for the neckdown pass: L4)
            clearance_pass(&mut e, board, s, if need_slack { Some(&mut all_slack) } else { None }, Some(mask), None);
        } else {
            for (ti, tr) in board.traces.iter_mut().enumerate() {
                if mask[ti] == 0 {
                    continue;
                }
                let w = tr.width;
                for pt in &mut tr.points {
                    pt.width = w;
                }
            }
        }
        for ti in 0..board.traces.len() {
            if mask[ti] == 0 {
                continue;
            }
            let slack = if need_slack { Some(all_slack[ti].as_slice()) } else { None };
            build_curve(&mut board.traces[ti], s, sub, slack);
        }
        // (neckdowns and teardrops on the exported path: L4)
    }
}
