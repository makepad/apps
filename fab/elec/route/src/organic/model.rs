// Ported from TiNRS Psychedelic Router 0.1.0: src/core/Board.h (the subset the relaxer reads).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! `OBoard`: the reference's board model, as far as the relaxer, its kernels and its export
//! copper read it. Plain data in the reference's units (mm, `f32` where it says float) and its
//! frame (Y down). Objects refer to each other by index with `-1` for none, as in the
//! reference; Fab's ids appear only in `adapt`.
//!
//! Changes against the reference, each a ledger row:
//! - layer masks are copper stack-index bits (`1 << layer`), not KiCad's named layer bits;
//! - a pad is one copper shape on the layers of `layer_mask`; a Fab pad with different shapes
//!   per layer becomes several pads, linked by `piece_of` (connectivity joins them);
//! - clearances come from `ClearanceTable` (ours, from the rule matrix), indexed by the
//!   objects' `rule`; it replaces `NetClearances` and the pad-clearance policy;
//! - plane membership is data (`LayerInfo::plane_net`) instead of net-name heuristics;
//! - KiCad file round-trip fields (uuids, padstack names, verbatim attributes) are left out.
//! - ADAPTED, pour hook 1 (`engine/build::build_static`, reference relaxer `buildStatic`):
//!   `Keepout::only_net` feeds `KoEdge::only_net`, the reference's avoid-area net filter;
//! - ADAPTED, pour hook 2 (`engine/pair::eval_pair`, reference relaxer `evalPair`, Seg):
//!   distinct same-net traces are inapplicable when either uses a `pour_rules` row; own-trace
//!   exclusions and self-contact thresholds are unchanged;
//! - ADAPTED, pour hook 3 (`relaxer::step`, K4): a pour-row trace keeps its net out of the loop
//!   simplifier without pinning its moving nodes. Empty `pour_rules` preserves the prior policy.
//! - ADAPTED, pour hook 4 (`relax::sweep::relax_node`, reference relaxer `relaxNode`): a pour-row
//!   trace's node has no shape force (spring, bend, snap, keeps), no meander and no crossing
//!   terms; only clearance moves it.
//! - ADAPTED, pour hook 5 (`engine/pair::eval_pair`, Seg): copper that is not on a pour row keeps
//!   `POUR_SHARE` of its margin from an unlocked pour-row segment, in the pair's targets and reach.
//! - ADAPTED, pour hook 6 (`engine/build::gather`, reference relaxer `gather`): a query on behalf of
//!   one net reads the static grid without other nets' `only_net` keepout edges.
//! - ADAPTED, pour hook 7 (`relax::resample::resample`, reference relaxer `resample`): a pour-row
//!   trace splits and merges at `K_POUR_NODE_SPACING_SCALE` times the node spacing.

use super::geom::Vec2;

pub const NONE: i32 = -1;

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum LayerIntent {
    #[default]
    Signal,
    GroundPlane,
    PowerPlane,
    Mixed,
}

#[derive(Clone, Debug, Default)]
pub struct LayerInfo {
    /// 0 = the front copper, in stack order.
    pub index: i32,
    pub intent: LayerIntent,
    /// For a plane layer: the net whose solid zone it is (`Drc::planeNet`); `-1` otherwise.
    pub plane_net: i32,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum PadShape {
    #[default]
    Circle,
    Rect,
    RoundRect,
    Oval,
    /// Exact polygon copper: `Pad::polygon`.
    Polygon,
}

#[derive(Clone, Debug)]
pub struct Pad {
    pub id: i32,
    pub center: Vec2,
    /// Board-frame axis-aligned extent (already rotated).
    pub size: Vec2,
    /// mm, 0 for surface pads.
    pub drill_diameter: f32,
    pub shape: PadShape,
    /// Copper stack-index bits.
    pub layer_mask: u64,
    pub net: i32,
    pub is_smd: bool,
    /// Degrees counter-clockwise as seen on screen (Y down).
    pub rotation: f32,
    /// Unrotated size along the pad's own axes (0, 0 = unknown: use `size`).
    pub local_size: Vec2,
    /// Drill extent along the pad's axes; x != y is a slot, (0, 0) = a round hole of
    /// `drill_diameter`.
    pub drill_size: Vec2,
    /// RoundRect corner radius = ratio * min(width, height).
    pub round_rect_ratio: f32,
    /// Polygon pads: the ring in the pad's local frame (origin `center`, Y down).
    pub polygon: Vec<Vec2>,
    /// The footprint the pad belongs to (index into the adapter's parts), `-1` for none.
    pub component: i32,
    /// The first piece of the same Fab pad (`-1`: this is the first or only piece).
    pub piece_of: i32,
    /// Row of `ClearanceTable`.
    pub rule: u16,
    /// ADAPTED (`Pad::name` within `componentRef`): the Fab pad's number (`no`) as an index into
    /// the adapter's table of numbers, `-1` for none (holes); pads of one footprint with one number
    /// share it, and so do the pieces of one Fab pad (count roots only: `piece_of == -1`).
    pub number: i32,
    /// `RoutingIntents::fanout` of the pad (Route.Fanout); Fab has no such intent: `Default`.
    pub fanout: FanoutStyle,
    /// ADAPTED: mm from a via drill to this pad's copper when the via may not sit in it
    /// (`RuleMatrix::via_drill_to_pad`): the hard level of the same-net via and pad pair.
    pub via_drill_gap: f32,
}

/// `FanoutStyle` (RoutingIntents.h:93): how a designer asked a pad to fan out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FanoutStyle {
    #[default]
    Default,
    Dogbone,
    InPad,
    None,
}

impl Default for Pad {
    fn default() -> Pad {
        Pad {
            id: NONE,
            center: Vec2::default(),
            size: Vec2::new(1.6, 1.6),
            drill_diameter: 0.8,
            shape: PadShape::Circle,
            layer_mask: u64::MAX,
            net: NONE,
            is_smd: false,
            rotation: 0.0,
            local_size: Vec2::new(0.0, 0.0),
            drill_size: Vec2::new(0.0, 0.0),
            round_rect_ratio: 0.25,
            polygon: Vec::new(),
            component: NONE,
            piece_of: NONE,
            rule: 0,
            number: NONE,
            fanout: FanoutStyle::Default,
            via_drill_gap: 0.0,
        }
    }
}

impl Pad {
    /// Drill extent along the pad axes (round holes: `drill_diameter` square, SMD: 0).
    pub fn drill_extent(&self) -> Vec2 {
        if self.drill_size.x > 0.0 && self.drill_size.y > 0.0 {
            return self.drill_size;
        }
        Vec2::new(self.drill_diameter, self.drill_diameter)
    }
    pub fn has_slot(&self) -> bool {
        self.drill_size.x > 0.0 && self.drill_size.y > 0.0 && (self.drill_size.x - self.drill_size.y).abs() > 1e-4
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ViaType {
    #[default]
    Auto,
    Through,
    Blind,
    Buried,
    Micro,
}

#[derive(Clone, Debug)]
pub struct Via {
    pub id: i32,
    pub net: i32,
    pub pos: Vec2,
    pub velocity: Vec2,
    pub diameter: f32,
    pub drill: f32,
    /// Copper stack indices of the span's ends (either order).
    pub layer_start: i32,
    pub layer_end: i32,
    /// Tethered to this surface pad (dog-bone escape via), `-1` for none.
    pub connected_pad: i32,
    pub is_fixed: bool,
    pub locked: bool,
    pub imported: bool,
    pub pair_frozen: bool,
    pub kind: ViaType,
    pub rule: u16,
    /// ADAPTED (not in Board.h): the caller's tag for this via, carried through the
    /// renumbering the relaxer and the net simplifier do (`id` is the via's index); no ported
    /// code reads it. The adapter stores the via's index in `Adapted::vias`.
    pub src: i32,
}

impl Default for Via {
    fn default() -> Via {
        Via {
            id: NONE,
            net: NONE,
            pos: Vec2::default(),
            velocity: Vec2::default(),
            diameter: 0.8,
            drill: 0.4,
            layer_start: 0,
            layer_end: 1,
            connected_pad: NONE,
            is_fixed: false,
            locked: false,
            imported: false,
            pair_frozen: false,
            kind: ViaType::Auto,
            rule: 0,
            src: NONE,
        }
    }
}

impl Via {
    pub fn span_lo(&self) -> i32 {
        if self.layer_start < self.layer_end {
            self.layer_start
        } else {
            self.layer_end
        }
    }
    pub fn span_hi(&self) -> i32 {
        if self.layer_start < self.layer_end {
            self.layer_end
        } else {
            self.layer_start
        }
    }
}

#[derive(Clone, Debug)]
pub struct TracePoint {
    pub pos: Vec2,
    pub velocity: Vec2,
    /// Local physical width (dynamic neckdown).
    pub width: f32,
    /// E.g. a terminal pad pin.
    pub is_fixed: bool,
    pub terminal_pad: i32,
    /// Attached to a dynamic via.
    pub attached_via: i32,
    /// Branched into another trace's node: that trace's id and node index.
    pub branch_trace: i32,
    pub branch_point: i32,
    /// The segment from the previous node to this one is a fixed straight exit stub.
    pub straight_before: bool,
}

impl Default for TracePoint {
    fn default() -> TracePoint {
        TracePoint {
            pos: Vec2::default(),
            velocity: Vec2::default(),
            width: 0.25,
            is_fixed: false,
            terminal_pad: NONE,
            attached_via: NONE,
            branch_trace: NONE,
            branch_point: NONE,
            straight_before: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Trace {
    pub id: i32,
    pub net: i32,
    /// Copper stack index.
    pub layer: i32,
    pub width: f32,
    pub points: Vec<TracePoint>,
    /// The exported continuous path.
    pub smoothed: Vec<Vec2>,
    /// Width along `smoothed` (neckdowns).
    pub point_widths: Vec<f32>,
    /// `Trace::teardropFront`: filled copper polygon at the first end.
    pub teardrop_front: Vec<Vec2>,
    /// `Trace::teardropBack`: filled copper polygon at the last end.
    pub teardrop_back: Vec<Vec2>,
    pub length: f32,
    pub is_routed: bool,
    pub locked: bool,
    pub imported: bool,
    pub pair_frozen: bool,
    pub rule: u16,
}

impl Default for Trace {
    fn default() -> Trace {
        Trace {
            id: NONE,
            net: NONE,
            layer: 0,
            width: 0.25,
            points: Vec::new(),
            smoothed: Vec::new(),
            point_widths: Vec::new(),
            teardrop_front: Vec::new(),
            teardrop_back: Vec::new(),
            length: 0.0,
            is_routed: false,
            locked: false,
            imported: false,
            pair_frozen: false,
            rule: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Net {
    pub id: i32,
    pub diff_pair_partner: i32,
    /// Adapter-resolved Board::diffPairGap(net), src/core/Board.cpp:236-247:
    /// intent / net-class gap, else rules.diffPairSpacing; 0 until resolved.
    pub pair_gap: f32,
    /// Adapter-resolved Board::viaSizeFor(net), src/core/Board.cpp:314-370:
    /// net-class via diameter in mm, else rules.viaDiameter, then floors; 0 until resolved.
    pub via_diameter: f32,
    /// Adapter-resolved Board::viaSizeFor(net), src/core/Board.cpp:314-370:
    /// net-class via drill in mm, else rules.viaDrill, then floors; 0 until resolved.
    pub via_drill: f32,
    /// Row of `ClearanceTable` for a via of this net's class.
    pub via_rule: u16,
    /// Vias of this net may sit on its own pads: `RuleMatrix::via_in_pad` of its class.
    pub via_in_pad: bool,
    pub target_length: f32,
    pub is_diff_pair: bool,
    /// `NetIntent::current` (Route.Current): the designer sized this net's copper for current, so
    /// no pass thins or merges it; Fab has no such intent: false.
    pub current: bool,
}

impl Default for Net {
    fn default() -> Net {
        Net { id: NONE, diff_pair_partner: NONE, pair_gap: 0.0, via_diameter: 0.0, via_drill: 0.0, via_rule: 0, via_in_pad: false,
            target_length: 0.0, is_diff_pair: false, current: false }
    }
}

/// A rule area: tracks and/or vias kept `clearance` from its polygon.
#[derive(Clone, Debug)]
pub struct Keepout {
    pub id: i32,
    pub poly: Vec<Vec2>,
    pub layer_mask: u64,
    pub clearance: f32,
    pub no_tracks: bool,
    pub no_vias: bool,
    /// Only this net's moving copper feels the keepout (the reference's avoid areas); `NONE`: every net.
    pub only_net: i32,
}

impl Default for Keepout {
    fn default() -> Keepout {
        Keepout { id: NONE, poly: Vec::new(), layer_mask: u64::MAX, clearance: 0.25,
            no_tracks: true, no_vias: true, only_net: NONE }
    }
}

/// The reference's `DesignRules`, the fields the relaxer reads. Filled from the rule matrix by
/// the adapter; `min_clearance` is the smallest copper clearance of the table (`clrRule`).
#[derive(Clone, Debug)]
pub struct DesignRules {
    pub default_trace_width: f32,
    pub min_trace_width: f32,
    pub min_clearance: f32,
    pub via_diameter: f32,
    pub via_drill: f32,
    /// src/core/Board.h:565: DesignRules::diffPairSpacing, 0.20 mm by default.
    pub diff_pair_spacing: f32,
    pub board_edge_clearance: f32,
    pub hole_clearance: f32,
    pub hole_to_hole: f32,
}

impl Default for DesignRules {
    fn default() -> DesignRules {
        DesignRules {
            default_trace_width: 0.25,
            min_trace_width: 0.15,
            min_clearance: 0.20,
            via_diameter: 0.80,
            via_drill: 0.40,
            diff_pair_spacing: 0.20,
            board_edge_clearance: 0.50,
            hole_clearance: 0.25,
            hole_to_hole: 0.25,
        }
    }
}

/// Copper-to-copper clearance by rule row pair and layer, in mm (ours: replaces the reference's
/// `NetClearances` and the pad-clearance policy). Symmetric. Every value already holds the
/// adapter's float margin.
#[derive(Clone, Debug, Default)]
pub struct ClearanceTable {
    pub rows: usize,
    pub layers: usize,
    /// `[(a * rows + b) * layers + layer]`.
    pub mm: Vec<f32>,
    /// The largest value of the table (query reaches).
    pub max: f32,
}

impl ClearanceTable {
    /// The clearance of rule rows `a` and `b` on `layer`; for `layer < 0` (a disc on every layer
    /// it spans) the largest over the layers.
    #[inline]
    pub fn get(&self, a: u16, b: u16, layer: i32) -> f32 {
        let base = (a as usize * self.rows + b as usize) * self.layers;
        if layer < 0 {
            return self.mm[base..base + self.layers].iter().fold(0.0f32, |m, &v| if m < v { v } else { m });
        }
        self.mm[base + (layer as usize).min(self.layers.saturating_sub(1))]
    }
}

#[derive(Clone, Debug, Default)]
pub struct OBoard {
    pub layers: Vec<LayerInfo>,
    /// The board perimeter polygon.
    pub outline: Vec<Vec2>,
    /// Closed edge loops other than `outline` (holes, slots).
    pub cutouts: Vec<Vec<Vec2>>,
    pub pads: Vec<Pad>,
    pub nets: Vec<Net>,
    pub vias: Vec<Via>,
    pub traces: Vec<Trace>,
    pub keepouts: Vec<Keepout>,
    /// Copper zones (pours), the subset the relaxer's passes read.
    pub zones: Vec<Zone>,
    pub rules: DesignRules,
    pub clr: ClearanceTable,
    /// Sorted clearance-table rows used by pour paths; empty until the caller adds pour paths.
    pub pour_rules: Vec<u16>,
    /// `Board::minTrackWidthFor`: resolved (net, layer) minima; missing entries use `rules.min_trace_width`.
    pub min_track_width_for: std::collections::BTreeMap<(i32, i32), f32>,
    /// `Board::trackWidthFixed`: resolved per-net flags (nonzero means an impedance width).
    pub track_width_fixed: Vec<u8>,
}

/// `OrganicRelaxer::NeckdownIssue`: the worst sample of an impossible bottleneck.
#[derive(Clone, Debug)]
pub struct NeckdownIssue {
    /// `NeckdownIssue::traceId`.
    pub trace_id: i32,
    /// `NeckdownIssue::netId`.
    pub net_id: i32,
    /// `NeckdownIssue::layer`.
    pub layer: i32,
    /// `NeckdownIssue::pos`.
    pub pos: Vec2,
    /// `NeckdownIssue::requiredWidth`.
    pub required_width: f32,
    /// `NeckdownIssue::minWidth`.
    pub min_width: f32,
}

impl Default for NeckdownIssue {
    fn default() -> Self {
        Self { trace_id: -1, net_id: -1, layer: 0, pos: Vec2::default(), required_width: 0.0, min_width: 0.0 }
    }
}

/// `Zone` (Board.h:435-485), the fields the net simplifier reads. ADAPTED: one entry per outer
/// ring of a Fab zone (the reference reads a zone's first outline only).
#[derive(Clone, Debug, Default)]
pub struct Zone {
    pub net: i32,
    /// Copper stack-index bits.
    pub layer_mask: u64,
    pub outline: Vec<Vec2>,
    pub is_rule_area: bool,
}

impl Zone {
    /// `isCopper`.
    pub fn is_copper(&self) -> bool {
        !self.is_rule_area && self.net >= 0 && self.layer_mask != 0 && self.outline.len() >= 3
    }
}
