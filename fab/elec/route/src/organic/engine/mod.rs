// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (the Engine: lines 85-470).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! The relaxer's Engine: spatial indices of the static and the moving copper, and the guarded
//! evaluation shared by the relaxation steps and the spline rebuild.
//!
//! The C++ `Engine` holds `Board&` and `const OrganicSettings&`; here the board is an argument of
//! every method that reads it (`b: &OBoard`, so a caller can mutate the board between calls) and
//! the settings are a copy in `s`. The C++ base class `EngineCache` (the part that persists across
//! steps) is the field `c`. The single-threaded callers' scratch `X0` is the field `x0`; a method
//! that needs it and `&self` at once takes it out with `std::mem::take` and puts it back.
//!
//! ADAPTED (ours): per net-pair clearances (`NetClearances`, `padRuleClearance`, `padRuleReach`)
//! are the board's `ClearanceTable` by rule row and layer (`pair_shift`, `max_extra`), so `Ob` and
//! `Prim` carry the `rule` of their copper.
//!
//! Files: `build` (bounds, static and dynamic indices, gather), `essential` (contacts the guard
//! keeps), `pair` (evaluation of one moving primitive against one obstacle).

pub mod build;
pub mod essential;
pub mod guard;
pub mod pair;

use super::copper::{PadModel, K_BIG};
use super::geom::{std_clamp, std_max, Box2, Vec2};
use super::grid::UniformGrid;
use super::model::OBoard;
use super::settings::{ForceTerm, OrganicSettings, FORCE_TERM_COUNT};
use std::collections::{BTreeMap, HashSet};

/// `finiteVec`.
#[inline]
pub fn finite_vec(p: Vec2) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

/// `ObKind`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum ObKind {
    #[default]
    Seg = 0,
    Pad = 1,
    Via = 2,
    Edge = 3,
    /// Edge of a keepout / rule area polygon.
    Ko = 4,
}

/// An obstacle found by `gather`, with its geometry cached: segment, via centre or edge ends.
#[derive(Copy, Clone, Debug)]
pub struct Ob {
    pub kind: ObKind,
    pub id: i32,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub a: Vec2,
    pub b: Vec2,
    /// Segment half width or via radius.
    pub h: f32,
    pub net: i32,
    /// Segment layer (-1: all layers).
    pub layer: i32,
    pub trace: i32,
    pub seg: i32,
    /// Via drill.
    pub drill: f32,
    /// ADAPTED: the clearance-table row of its copper (pad, trace or via rule; 0 for edges).
    pub rule: u16,
}

/// `Ob o{}`: the C++ value initialisation with the member defaults.
impl Default for Ob {
    fn default() -> Ob {
        Ob {
            kind: ObKind::Seg,
            id: 0,
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
            a: Vec2::default(),
            b: Vec2::default(),
            h: 0.0,
            net: -1,
            layer: -1,
            trace: -1,
            seg: -1,
            drill: 0.0,
            rule: 0,
        }
    }
}

/// A moving copper primitive: segment (`a` = fixed end, `b` = moving node) or via disc (`a == b`).
#[derive(Copy, Clone, Debug)]
pub struct Prim {
    pub a: Vec2,
    pub b: Vec2,
    /// Half width (segment) or radius (disc).
    pub h: f64,
    /// -1 for discs (through vias).
    pub layer: i32,
    pub net: i32,
    pub trace: i32,
    /// Own-trace segment indices excluded (adjacent).
    pub ex_lo: i32,
    pub ex_hi: i32,
    /// DRC object index inside the move (trace / via).
    pub owner: i32,
    pub via_id: i32,
    /// Disc: via drill.
    pub drill: f64,
    pub disc: bool,
    /// Copper object key of the owner (`copper::trace_key` / `via_key`).
    pub obj: i64,
    /// Segment: straight-stretch weight 0..1 (straightPassRelief; fixed per step).
    pub pass: f32,
    /// ADAPTED: the clearance-table row of its copper.
    pub rule: u16,
}

impl Default for Prim {
    fn default() -> Prim {
        Prim {
            a: Vec2::default(),
            b: Vec2::default(),
            h: 0.0,
            layer: 0,
            net: -1,
            trace: -1,
            ex_lo: 0,
            ex_hi: -1,
            owner: 0,
            via_id: -1,
            drill: 0.0,
            disc: false,
            obj: -1,
            pass: 0.0,
            rule: 0,
        }
    }
}

/// `PairClass`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum PairClass {
    /// Not applicable (same net pad, other layer, ...).
    Na = 0,
    /// Applicable, provably farther than any relevant threshold.
    Far = 1,
    Eval = 2,
}

#[derive(Copy, Clone, Debug)]
pub struct PairEval {
    pub gap: f64,
    pub thr: f64,
    /// Soft margin target (repulsion zone end).
    pub target: f64,
    pub target_wide: f64,
    pub t: f64,
    pub vx: f64,
    pub vy: f64,
    pub repel: bool,
    pub partner: bool,
    /// ADAPTED: the pair's hard level less `gap`, both against `thr`. 0 for every pair but a
    /// same-net via and pad it may not sit in: there `gap` keeps the ring a full threshold off the
    /// pad (the wall ordinary steps keep) and the hard level is the drill wall at the pad's
    /// `via_drill_gap` (the rule DRC judges), which only a restoration step may go down to.
    pub hard_extra: f64,
}

impl Default for PairEval {
    fn default() -> PairEval {
        PairEval { gap: K_BIG, thr: 0.0, target: 0.0, target_wide: 0.0, t: 0.0, vx: 0.0, vy: 0.0, repel: false, partner: false,
            hard_extra: 0.0 }
    }
}

impl PairEval {
    /// ADAPTED: the gap against `thr` at the pair's hard level (`gap` but on a same-net via and pad).
    #[inline]
    pub fn gap_hard(&self) -> f64 {
        self.gap + self.hard_extra
    }
}

/// `PenPair {prim, ob, T, Tw, k}`.
#[derive(Copy, Clone, Debug, Default)]
pub struct PenPair {
    pub prim: i32,
    pub ob: i32,
    pub t: f64,
    pub tw: f64,
    pub k: f64,
}

/// ADAPTED: a pair under its threshold by more than `guard::K_RESTORE_DELTA`, as the restoration
/// step weighs it: its push direction (unit, as the force uses it) and the moved end's lever.
#[derive(Copy, Clone, Debug)]
pub struct Deficit {
    pub prim: i32,
    pub ob: i32,
    /// `thr - gap` (mm).
    pub deficit: f64,
    /// 1 for a disc; the segment parameter of the closest point, from the fixed end.
    pub lever: f64,
    pub dir: Vec2,
}

/// Per-thread scratch of the Engine's queries and guarded moves: only buffers and per-call
/// parameters; results never depend on what a scratch held before.
#[derive(Clone, Debug)]
pub struct EngineScratch {
    /// Result of `gather`.
    pub obs: Vec<Ob>,
    pub stamp_s: Vec<u32>,
    pub stamp_d: Vec<u32>,
    pub stamp: u32,
    pub g_old: Vec<f64>,
    /// ADAPTED: `g_old` at the pairs' hard level (`PairEval::gap_hard`), for a restoration step.
    pub g_old_h: Vec<f64>,
    pub g_new: Vec<f64>,
    pub g_thr: Vec<f64>,
    pub c_old: Vec<f64>,
    pub c_new: Vec<f64>,
    pub pen_pairs: Vec<PenPair>,
    /// ADAPTED: the under-threshold pairs of the current guarded move (restoration step).
    pub deficits: Vec<Deficit>,
    pub done_new: Vec<u8>,
    pub partner_obs: Vec<i32>,
    /// (prim, ob): other-layer candidates, re-tested at the trial position.
    pub perp_cand: Vec<(i32, i32)>,
    // per call parameters of try_move (set by the caller, reset after the call)
    /// Dynamic segment ids ignored as obstacles by try_move.
    pub co_moving: Vec<i32>,
    /// Local coupling relaxation (intra-pair skew).
    pub coupling_scale: f64,
    /// Forcing (away from the partner) that grows a crinkle.
    pub crinkle_f: f64,
    /// Crinkle amplitude bound (mm beyond the pair spacing).
    pub crinkle_max: f64,
    /// Coupling only pushes apart to the pair spacing.
    pub coupling_repel_only: bool,
    /// Plain interior trace node: layer-crossing perpendicularity / crossing disentanglement apply.
    pub crossing_forces: bool,
    // relaxation sweep buffers
    pub prims: Vec<Prim>,
    pub contacts: Vec<i32>,
    /// Force recording: requested terms and the per-term force of the current evaluation.
    pub rec_mask: u32,
    pub rec_x: [f64; FORCE_TERM_COUNT],
    pub rec_y: [f64; FORCE_TERM_COUNT],
}

impl Default for EngineScratch {
    fn default() -> EngineScratch {
        EngineScratch {
            obs: Vec::new(),
            stamp_s: Vec::new(),
            stamp_d: Vec::new(),
            stamp: 0,
            g_old: Vec::new(),
            g_old_h: Vec::new(),
            g_new: Vec::new(),
            g_thr: Vec::new(),
            c_old: Vec::new(),
            c_new: Vec::new(),
            pen_pairs: Vec::new(),
            deficits: Vec::new(),
            done_new: Vec::new(),
            partner_obs: Vec::new(),
            perp_cand: Vec::new(),
            co_moving: Vec::new(),
            coupling_scale: 1.0,
            crinkle_f: 0.0,
            crinkle_max: 0.0,
            coupling_repel_only: false,
            crossing_forces: false,
            prims: Vec::new(),
            contacts: Vec::new(),
            rec_mask: 0,
            rec_x: [0.0; FORCE_TERM_COUNT],
            rec_y: [0.0; FORCE_TERM_COUNT],
        }
    }
}

impl EngineScratch {
    pub fn rec_clear(&mut self) {
        for i in 0..FORCE_TERM_COUNT {
            self.rec_x[i] = 0.0;
            self.rec_y[i] = 0.0;
        }
    }
    pub fn rec_add(&mut self, t: ForceTerm, fx: f64, fy: f64) {
        self.rec_x[t as usize] += fx;
        self.rec_y[t as usize] += fy;
    }
    /// The terms owned by try_move (redone when a move is re-evaluated without the crossing terms).
    pub fn rec_clear_move(&mut self) {
        for t in [
            ForceTerm::PadRepulsion,
            ForceTerm::TraceRepulsion,
            ForceTerm::ViaRepulsion,
            ForceTerm::Perpendicularity,
            ForceTerm::Disentanglement,
            ForceTerm::Coupling,
            ForceTerm::Crinkle,
            ForceTerm::Tether,
            ForceTerm::PadExit,
        ] {
            self.rec_x[t as usize] = 0.0;
            self.rec_y[t as usize] = 0.0;
        }
    }
}

/// `EngineCache::KoEdge`: an edge of a keepout polygon (or, later, of a copper shape, an avoid
/// area or a courtyard): tracks on `track_layers` / vias spanning `via_layers` keep `thr` from it.
#[derive(Copy, Clone, Debug)]
pub struct KoEdge {
    pub a: Vec2,
    pub b: Vec2,
    pub track_layers: u64,
    pub via_layers: u64,
    pub thr: f64,
    /// Edge of a copper shape of net `net` (-1 = no net, foreign for every net).
    pub copper: bool,
    pub net: i32,
    /// Avoid area: guards copper of this net only (-1 = every net).
    pub only_net: i32,
    /// Index into the avoid areas when the area has exempt zones, else -1.
    pub area: i32,
    /// Edge of a footprint courtyard (guards vias only): index into `cy_nets`, -1 = none.
    pub courtyard: i32,
}

impl Default for KoEdge {
    fn default() -> KoEdge {
        KoEdge {
            a: Vec2::default(),
            b: Vec2::default(),
            track_layers: 0,
            via_layers: 0,
            thr: 0.0,
            copper: false,
            net: -1,
            only_net: -1,
            area: -1,
            courtyard: -1,
        }
    }
}

/// The Engine's indices that persist across steps (rebuilt only when their inputs change).
#[derive(Clone, Debug)]
pub struct EngineCache {
    // static obstacles (pads, board outline) and the fixed index rectangle
    pub pad_m: Vec<PadModel>,
    pub edges: Vec<(Vec2, Vec2)>,
    pub ko_edges: Vec<KoEdge>,
    /// Sorted pad nets per courtyard footprint.
    pub cy_nets: Vec<Vec<i32>>,
    /// Largest keepout threshold (query reach).
    pub ko_max_g: f64,
    pub sgrid: UniformGrid,
    /// ADAPTED: pour hook 6. `sgrid` without other nets' masks, for a query on behalf of one net:
    /// entry 0 holds no net-filtered keepout edge, entry `net_sgrid[n]` (when not 0) also net `n`'s.
    /// Empty when the board has no net-filtered keepout edge.
    pub net_sgrids: Vec<UniformGrid>,
    /// Per net: its entry in `net_sgrids`; a net without masks, or beyond the end, reads entry 0.
    pub net_sgrid: Vec<usize>,
    pub bounds: Box2,
    pub board_center: Vec2,
    // dynamic copper: trace segments per layer + vias, positions read live
    pub vgrid: UniformGrid,
    pub lgrid: Vec<UniformGrid>,
    pub seg_trace: Vec<i32>,
    pub seg_idx: Vec<i32>,
    pub seg_base: Vec<i32>,
    pub n_segs: i32,
    pub max_half_w: f64,
    pub max_via_r: f64,
    /// Same-net contacts the connectivity of a net depends on, per net id (iterated in id order).
    pub ess_by_net: BTreeMap<i32, Vec<(i64, i64)>>,
    /// Looked up only, never iterated: hash order cannot reach a result.
    pub essential: HashSet<u64>,
    pub trace_ess: Vec<u8>,
    pub via_ess: Vec<u8>,
    /// Per trace: junction group (traces linked by branches), -1 = none.
    pub jgroup: Vec<i32>,
    /// `build_dynamic(only)` scratch.
    pub region_bits: Vec<Vec<u8>>,
}

impl Default for EngineCache {
    fn default() -> EngineCache {
        EngineCache {
            pad_m: Vec::new(),
            edges: Vec::new(),
            ko_edges: Vec::new(),
            cy_nets: Vec::new(),
            ko_max_g: 0.0,
            sgrid: UniformGrid::default(),
            net_sgrids: Vec::new(),
            net_sgrid: Vec::new(),
            bounds: Box2::default(),
            board_center: Vec2::default(),
            vgrid: UniformGrid::default(),
            lgrid: Vec::new(),
            seg_trace: Vec::new(),
            seg_idx: Vec::new(),
            seg_base: Vec::new(),
            n_segs: 0,
            max_half_w: 0.125,
            max_via_r: 0.4,
            ess_by_net: BTreeMap::new(),
            essential: HashSet::new(),
            trace_ess: Vec::new(),
            via_ess: Vec::new(),
            jgroup: Vec::new(),
            region_bits: Vec::new(),
        }
    }
}

/// The Engine (see the module doc for how the C++ references map).
#[derive(Clone, Debug)]
pub struct Engine {
    pub c: EngineCache,
    pub s: OrganicSettings,
    pub n_l: i32,
    pub clr_g: f64,
    pub edge_g: f64,
    pub self_thr: f64,
    pub margin: f64,
    pub margin_wide: f64,
    pub max_step: f64,
    pub k_rep_pad: f64,
    pub k_rep_trace: f64,
    /// Via-to-via soft margin stiffness (viaViaRepulsion).
    pub k_rep_via: f64,
    /// mm added to the via-to-via soft margin.
    pub via_extra: f64,
    /// straightPassRelief: per trace segment the straight-stretch weight of this step (empty = none).
    pub seg_pass: Vec<Vec<f32>>,
    pub pass_relief: f64,
    /// bundleKeep: per net its bundles and each bundle's layer; empty = off.
    pub bundle_net: Vec<Vec<i32>>,
    pub bundle_layer: Vec<i32>,
    /// Hole rules: copper keeps `hole_clr` from a drill wall, holes keep `hole_h2h` from each
    /// other; `hole_extra` is how far a hole rule reaches beyond the copper clearance.
    pub clr_rule: f64,
    pub hole_clr: f64,
    pub hole_h2h: f64,
    pub hole_extra: f64,
    pub partner_net: Vec<i32>,
    /// Per net: the diff-pair partner (NetClearances::partner; -1 = none).
    pub pair_partner: Vec<i32>,
    /// Per net: the pair gap in mm (NetClearances::pairGap; 0 = none).
    pub pair_gap: Vec<f32>,
    /// Per net: vias may sit on own-net pads.
    pub vip_net: Vec<u8>,
    /// How far the largest pair clearance reaches beyond `clr_rule` (query reaches).
    pub max_extra: f64,
    /// Scratch of the single-threaded callers (`X0`).
    pub x0: EngineScratch,
}

impl Engine {
    /// `Engine(Board&, const OrganicSettings&, const NetClearances*)`. ADAPTED: the per-pair
    /// clearances are the board's table (no `NetClearances` to build or pass); each net's via-in-pad
    /// permission is resolved from its class and the factory capability by the adapter;
    /// bundles (bundleKeep, L5) are absent from the port's board.
    pub fn new(b: &OBoard, s: &OrganicSettings) -> Engine {
        let n_l = std::cmp::max(1, b.layers.len() as i32);
        let slack = std_max(0.0f32, s.guard_clearance_slack) as f64;
        let clr_g = b.rules.min_clearance as f64 + slack;
        let edge_g = b.rules.board_edge_clearance as f64 + slack;
        let margin = std_max(0.0f32, s.clearance_margin) as f64;
        let margin_wide = margin
            * (1.0 + if s.enable_medial_axis_centering { std_max(0.0f32, s.medial_axis_centering_strength) as f64 } else { 0.0 });
        let max_step = std_clamp(s.max_node_step as f64, 0.005, 0.5);
        let k_rep_pad = std_clamp(3.5 * s.pad_repulsion as f64, 0.1, 200.0);
        let k_rep_trace = std_clamp(6.0 * s.trace_repulsion as f64, 0.1, 200.0);
        let vv = std_clamp(s.via_via_repulsion as f64, 0.0, 5.0);
        let k_rep_via = std_clamp(k_rep_pad * (1.0 + vv), 0.1, 400.0);
        let via_extra = 0.1 * vv;
        let clr_rule = b.rules.min_clearance as f64;
        let max_extra = std_max(0.0, b.clr.max as f64 - clr_rule);
        let hole_clr = std_max(0.0f32, b.rules.hole_clearance) as f64;
        let hole_h2h = b.rules.hole_to_hole as f64;
        let hole_extra = std_max(0.0, std_max(hole_clr, hole_h2h) - clr_rule);
        // NetClearances::build (Board.cpp:463-486), independent of coupling settings.
        // ADAPTED: the adapter resolves Board::diffPairGap into Net::pair_gap.
        let mut pair_partner = vec![-1; b.nets.len()];
        let mut pair_gap = vec![0.0f32; b.nets.len()];
        for (i, n) in b.nets.iter().enumerate() {
            if n.is_diff_pair && n.diff_pair_partner >= 0 && (n.diff_pair_partner as usize) < b.nets.len() && n.diff_pair_partner != i as i32 {
                pair_partner[i] = n.diff_pair_partner;
                pair_gap[i] = n.pair_gap;
            }
        }
        let mut partner_net = vec![-2; b.nets.len()];
        if s.enable_diff_pair_coupling {
            for (i, n) in b.nets.iter().enumerate() {
                if n.is_diff_pair && n.diff_pair_partner >= 0 && (n.diff_pair_partner as usize) < b.nets.len() && n.diff_pair_partner != i as i32 {
                    partner_net[i] = n.diff_pair_partner;
                }
            }
        }
        Engine {
            c: EngineCache::default(),
            s: s.clone(),
            n_l,
            clr_g,
            edge_g,
            self_thr: 0.005,
            margin,
            margin_wide,
            max_step,
            k_rep_pad,
            k_rep_trace,
            k_rep_via,
            via_extra,
            seg_pass: Vec::new(),
            pass_relief: 0.0,
            bundle_net: Vec::new(),
            bundle_layer: Vec::new(),
            clr_rule,
            hole_clr,
            hole_h2h,
            hole_extra,
            partner_net,
            pair_partner,
            pair_gap,
            vip_net: b.nets.iter().map(|net| u8::from(net.via_in_pad)).collect(),
            max_extra,
            x0: EngineScratch::default(),
        }
    }

    /// `sameBundle`: nets `a` and `b` share a bundle on `layer` (bundleKeep; empty = off).
    pub fn same_bundle(&self, a: i32, b: i32, layer: i32) -> bool {
        if a < 0 || b < 0 || a as usize >= self.bundle_net.len() || b as usize >= self.bundle_net.len() {
            return false;
        }
        for &x in &self.bundle_net[a as usize] {
            if self.bundle_layer[x as usize] == layer && self.bundle_net[b as usize].contains(&x) {
                return true;
            }
        }
        false
    }

    /// `passScale`: soft-margin share of a moving primitive vs obstacle `o`: 1 - R w.
    pub fn pass_scale(&self, p: &Prim, o: Option<&Ob>) -> f64 {
        let mut w = p.pass;
        if let Some(o) = o {
            if !self.seg_pass.is_empty() && o.trace >= 0 && (o.trace as usize) < self.seg_pass.len() {
                let sp = &self.seg_pass[o.trace as usize];
                if o.seg >= 0 && (o.seg as usize) < sp.len() {
                    w = sp[o.seg as usize];
                }
            }
        }
        1.0 - self.pass_relief * w as f64
    }

    /// ADAPTED `pairShift(a, b, tracks)`: how far the clearance of rule rows `ra` and `rb` on
    /// `layer` (< 0: a disc, the largest over the layers) lies above `clr_rule`. Gaps against
    /// foreign copper are evaluated as gap - shift, so every threshold keeps its floor form.
    /// The reference's diff-pair branch (the partner's tracks at the pair gap, `tracks`) is applied
    /// by the one caller that has tracks on both sides, `eval_pair` (see there).
    #[inline]
    pub fn pair_shift(&self, b: &OBoard, ra: u16, rb: u16, layer: i32) -> f64 {
        b.clr.get(ra, rb, layer) as f64 - self.clr_rule
    }

    /// `net`'s pair partner is `other` and the pair has a gap (NetClearances::get's `tracks` case).
    #[inline]
    pub fn pair_partner_track(&self, net: i32, other: i32) -> bool {
        net >= 0 && (net as usize) < self.pair_partner.len() && self.pair_partner[net as usize] == other && self.pair_gap[net as usize] > 0.0
    }

    /// `partnerOf`.
    #[inline]
    pub fn partner_of(&self, net: i32) -> i32 {
        if net >= 0 && (net as usize) < self.partner_net.len() {
            self.partner_net[net as usize]
        } else {
            -2
        }
    }

    /// `partnerMarginScale`: soft-margin share against the pair partner's pads / vias; 1 for
    /// every other net pair.
    pub fn partner_margin_scale(&self, net: i32, other: i32) -> f64 {
        if other < 0 || self.partner_of(net) != other {
            return 1.0;
        }
        std_clamp(self.s.diff_pair_partner_margin as f64, 0.0, 1.0)
    }

    /// `viaInPadOk`: vias of `net` may sit on own-net pads.
    #[inline]
    pub fn via_in_pad_ok(&self, net: i32) -> bool {
        net >= 0 && (net as usize) < self.vip_net.len() && self.vip_net[net as usize] != 0
    }
}
