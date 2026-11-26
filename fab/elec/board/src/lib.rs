//! Fab electronics board model: contract C2, `fab-elec-board` (E1 1.2, 2.4, 2.5, E2.18, E4), its
//! ops (C3, `ops`) and the per-board engines (C4: `connect`, `rules`, `drc`, `zone`).
//!
//! A `Board` stands alone (E4 A5): it carries a copy of each placed footprint and the net of
//! every pad, its own net table mirroring its circuit's, and its rules, so connectivity, DRC,
//! fills and outputs need nothing else; a board that arrives without a circuit is still whole.
//! The host that builds both keeps `nets[i]` equal to `circuit.nets[i]`
//! (`fab_elec_design::Design::check_consistent`).
//!
//! Invariants: every `ItemId` is unique, below `next_item`, never reused; `pad_nets.len() ==
//! footprint.pads.len()`; at most one `Placed` per `CompId` (a component without one is in the
//! tray, not an error); a track's anchored end lies on its anchor (`ops` keeps it there); fills,
//! connectivity, the ratsnest and the rule matrix are never fields; `generation` is not content
//! (A14: change detection per edit uses `generation` and deltas, the hash is for cache keys and
//! tests). Only `Authored`, `Solved` and detached `Block` items have statements (E4 Q7).
//!
//! Geometry: board frame, X right, Y up, nm. A placed footprint's point `p` is at
//! `at + rotate(rot, mirror_x(p) if bottom else p)`; its layers flip on the bottom.

pub mod connect;
pub mod drc;
pub mod geom;
pub mod index;
pub mod length;
pub mod ops;
pub mod placement;
pub mod rings;
pub mod rules;
pub mod text;
pub mod zone;

use fab_elec_core::{
    canon_enum, canon_struct, canon_tags, Angle, BBox, BlockId, ClassId, CompId, Contour, FeatureRef, ItemId, LayerId, LayerSet, NetId, Nm,
    OnTrack, Path, PolySet, SolverId, Side, P,
};
use fab_elec_circuit::{Circuit, Intent, NetClass, Target};
use fab_elec_parts::{Footprint, Forbid, Gfx, ThermalRule, ZoneConnect};
use std::collections::HashMap;
use std::sync::Arc;

pub use connect::{connect, connect_held, connect_indexed, connect_update, ConnItem, Connectivity, Join, RatLine, Short, Witness};
pub use drc::{allowed_need, check_drills, check_item, check_pair, check_to, check_via_in_pad, dangling_ends, drc, merge_violations, required, required_to, DrcResult, ItemFault, ItemFaultKind, Obstacle, PairFault, Required};
pub use ops::{Applied, BoardDelta, BoardOp, Field, Inverse, Item, ItemExtent, OpError};
pub use rules::{apply_capabilities, resolve_rules, resolve_rules_with, Capabilities, CustomCheck, CustomRule, Effective, Factories, RuleCode, RuleItem, RuleKey, RuleMatrix, RuleProps, RuleRef, RuleSet, RuleSource, Rules, Waiver};
pub use geom::{Copper, DrillHole};
pub use rings::{RingIndex, Rings};
pub use index::{IndexChange, PKey, Piece, PieceIndex, PieceKind};
pub use length::LengthMatch;
pub use zone::{
    changed_box, fill, fill_all, fill_update, fill_update_indexed, reaches, FillLattice, FillLayer, FillPlan,
    FillTile, PolyRef, TileKey, TileLabels, ZoneFill,
};

/// Polygon error for arcs turned into polygons by the engines (fills, outline sets): 5 µm.
pub const ARC_ERR: Nm = 5_000;

/// Where each item lives: id -> (kind, index in its list). Derived data: never part of equality or
/// the canonical hash; maintained by the op path, `reindex` rebuilds it.
#[derive(Clone, Default)]
pub struct ItemIndex(HashMap<ItemId, (ItemKind, u32)>);

impl PartialEq for ItemIndex {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl ItemIndex {
    /// The entry kept for `id`; after a direct edit of the lists it may be stale or missing
    /// (`Board::locate` checks it against the list before it uses it).
    pub fn get(&self, id: ItemId) -> Option<(ItemKind, usize)> {
        self.0.get(&id).map(|&(k, i)| (k, i as usize))
    }
    /// How many ids have an entry.
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ItemIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ItemIndex({} ids)", self.0.len())
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Board {
    /// The unit this board belongs to.
    pub unit: fab_elec_core::BoardId,
    pub name: String,
    pub stackup: Stackup,
    /// Closed contours, even-odd: the outer edge, cutouts inside it, islands inside those (A4).
    pub outline: Vec<Contour>,
    /// The circuit's nets mirrored (`nets[i]` is `circuit.nets[i]`).
    pub nets: Vec<BoardNet>,
    /// The circuit's net classes mirrored (`classes[i]` is `circuit.classes[i]`).
    pub classes: Vec<NetClass>,
    /// The circuit's differential pairs mirrored (`Intent::Pair`, in intent order).
    pub pairs: Vec<NetPair>,
    /// Routed track length matches, in circuit intent order.
    pub length_matches: Vec<LengthMatch>,
    /// The circuit's parts fixed to an edge mirrored (`edge_parts_of`): their courtyards may
    /// overhang the outline.
    pub edge_parts: Vec<CompId>,
    /// The circuit's parts fixed to a spot mirrored (`spots_of`): DRC finds one standing elsewhere.
    pub spots: Vec<Spot>,
    pub placed: Vec<Placed>,
    pub tracks: Vec<Track>,
    pub vias: Vec<Via>,
    pub zones: Vec<Zone>,
    pub keepouts: Vec<BoardKeepout>,
    pub holes: Vec<Hole>,
    pub cutouts: Vec<Cutout>,
    pub graphics: Vec<Graphic>,
    pub texts: Vec<Text>,
    /// Length and skew tuning patterns (E6 4.2): parameters only; their copper is tracks.
    pub tunings: Vec<Tuning>,
    pub rules: Rules,
    /// Bumped by every applied op; not content.
    pub generation: u64,
    /// The next `ItemId` to issue; only grows (an undo keeps it), so it is not content.
    pub next_item: u32,
    /// Derived; see `ItemIndex`.
    pub index: ItemIndex,
    /// Derived, kept by the op path like `index`; see `RingIndex`.
    pub rings: RingIndex,
}
// Additive: `edge_parts`, `spots` and `length_matches` are written only when there are some, so a board without
// keeps its hash.
impl fab_elec_core::canon::Canon for Board {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let Board {
            unit, name, stackup, outline, nets, classes, pairs, length_matches, edge_parts, spots, placed, tracks, vias, zones, keepouts,
            holes, cutouts, graphics, texts, tunings, rules, generation: _, next_item: _, index: _, rings: _,
        } = self;
        unit.canon(w);
        name.canon(w);
        stackup.canon(w);
        outline.canon(w);
        nets.canon(w);
        classes.canon(w);
        pairs.canon(w);
        placed.canon(w);
        tracks.canon(w);
        vias.canon(w);
        zones.canon(w);
        keepouts.canon(w);
        holes.canon(w);
        cutouts.canon(w);
        graphics.canon(w);
        texts.canon(w);
        tunings.canon(w);
        rules.canon(w);
        if !edge_parts.is_empty() {
            edge_parts.canon(w);
        }
        if !spots.is_empty() {
            w.tag(1);
            spots.canon(w);
        }
        if !length_matches.is_empty() {
            w.tag(2);
            length_matches.canon(w);
        }
    }
}

/// Where a board item came from (E4 Q7, ARCHITECTURE 5.7).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Origin {
    /// Drawn by hand or by the AI's explicit statement.
    #[default]
    Authored,
    /// Made by an engine (a router, "finish this net"); engines may replace it freely; a view's
    /// edit adopts it (it becomes `Authored`).
    Solved { by: SolverId },
    /// Made by a block's board template; rigid with its block until detached.
    Block { block: BlockId, detached: bool },
    /// Projected from another unit's feature; read-only here.
    Derived { from: FeatureRef },
}
canon_enum!(Origin { 0 => Authored, 1 => Solved { by }, 2 => Block { block, detached }, 3 => Derived { from } });

impl Origin {
    /// The item has a statement in the board document.
    pub fn has_statement(&self) -> bool {
        matches!(self, Origin::Authored | Origin::Solved { .. } | Origin::Block { detached: true, .. })
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct BoardNet {
    pub name: String,
    pub class: ClassId,
}
canon_struct!(BoardNet { name, class });

/// A differential pair: the circuit's `Intent::Pair`, mirrored (`z0` stays with the circuit).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct NetPair {
    pub p: NetId,
    pub n: NetId,
    pub gap: Option<Nm>,
}
canon_struct!(NetPair { p, n, gap });

/// The parts a circuit fixes to an edge (`fixed(part, {edge: ..})`), a block's parts with it,
/// sorted.
pub fn edge_parts_of(c: &Circuit) -> Vec<CompId> {
    let mut parts = std::collections::BTreeSet::new();
    for intent in &c.intents {
        let Intent::Fixed { t, edge: Some(_), .. } = intent else { continue };
        match *t {
            Target::Comp(comp) | Target::Pin(comp, _) => { parts.insert(comp); }
            Target::Block(b) => parts.extend(c.block_comps(b)),
        }
    }
    parts.into_iter().collect()
}

/// A part its circuit fixes to a spot (`fixed(part, {at rot})`): its origin stands at `at`, turned
/// `rot` when the intent names one.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Spot {
    pub comp: CompId,
    pub at: P,
    pub rot: Option<Angle>,
}
canon_struct!(Spot { comp, at, rot });

impl Spot {
    /// The pose a part placed at this spot takes: `at`, and `rot` or else the pose's own.
    pub fn pose(&self, rot: Angle) -> (P, Angle) {
        (self.at, self.rot.unwrap_or(rot))
    }

    /// The part stands on its spot.
    pub fn holds(&self, part: &Placed) -> bool {
        part.at == self.at && self.rot.is_none_or(|rot| rot == part.rot)
    }
}

/// A keepout carried by a component, paired with the derived board set it replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedKeepout {
    pub from: FeatureRef,
    pub area: PolySet,
    pub band: (Nm, Nm),
    pub side: Side,
}

/// A port's room and keepout use board axes, with coordinates relative to its component's
/// position. The keepout's side is a board surface and its band is measured outward from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortRoom {
    pub comp: CompId,
    pub room: Option<PolySet>,
    pub keepout: Option<CarriedKeepout>,
}

impl PortRoom {
    /// Add the candidate component position to both areas.
    pub fn placed(&self, at: P) -> Self {
        Self {
            comp: self.comp,
            room: self.room.as_ref().map(|area| area.translated(at)),
            keepout: self.keepout.as_ref().map(|k| CarriedKeepout {
                from: k.from.clone(), area: k.area.translated(at), band: k.band, side: k.side,
            }),
        }
    }

    /// Express published board coordinates as offsets from the evaluated component position.
    pub fn relative_to(&self, at: P) -> Self {
        self.placed(at.neg())
    }
}

/// The parts a circuit fixes to a spot, in intent order, a later intent for a part over an earlier
/// one. A spot names one part: the language refuses one on a block, and a block target here gives
/// none.
pub fn spots_of(c: &Circuit) -> Vec<Spot> {
    let mut spots: Vec<Spot> = Vec::new();
    for intent in &c.intents {
        let Intent::Fixed { t: Target::Comp(comp) | Target::Pin(comp, _), at: Some(at), rot, .. } = *intent else { continue };
        spots.retain(|s| s.comp != comp);
        spots.push(Spot { comp, at, rot });
    }
    spots
}

/// The circuit's differential pairs in intent order.
pub fn pairs_of(c: &Circuit) -> Vec<NetPair> {
    c.intents.iter().filter_map(|intent| match intent {
        Intent::Pair { p, n, gap, .. } => Some(NetPair { p: *p, n: *n, gap: *gap }),
        _ => None,
    }).collect()
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Stackup {
    /// Top to bottom.
    pub layers: Vec<StackLayer>,
    pub finish: Finish,
    pub thickness: Nm,
    /// RGBA.
    pub mask_color: u32,
    pub silk_color: u32,
}
canon_struct!(Stackup { layers, finish, thickness, mask_color, silk_color });

#[derive(Clone, PartialEq, Debug)]
pub struct StackLayer {
    pub kind: StackKind,
    pub thickness: Nm,
    pub material: String,
    /// Relative permittivity.
    pub er: Option<f64>,
    pub loss_tan: Option<f64>,
    /// Copper layers: what the layer is for (the router's reference planes, E6 6.5).
    pub role: LayerRole,
    /// Copper layers: signal velocity in m/s when stated; otherwise derived from the
    /// neighbouring dielectrics' `er` (delay tuning, E6 4.2).
    pub velocity: Option<f64>,
}
canon_struct!(StackLayer { kind, thickness, material, er, loss_tan, role, velocity });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum LayerRole {
    #[default]
    Signal,
    /// A plane (ground or power): a reference for its neighbours.
    Plane,
    Mixed,
}
canon_tags!(LayerRole);

#[derive(Clone, PartialEq, Debug)]
pub enum StackKind {
    Copper(LayerId),
    /// `core: true` a core, else prepreg.
    Dielectric { core: bool },
    Mask(Side),
    Silk(Side),
    Paste(Side),
}
canon_enum!(StackKind { 0 => Copper(l), 1 => Dielectric { core }, 2 => Mask(s), 3 => Silk(s), 4 => Paste(s) });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Finish {
    #[default]
    None,
    Hasl,
    LeadFreeHasl,
    Enig,
    Osp,
    ImmersionSilver,
    ImmersionTin,
}
canon_tags!(Finish);

impl Stackup {
    /// The generic standard 2-layer stack (decision 8): 1.6 mm glass-epoxy, 1 oz copper.
    pub fn standard_2_layer() -> Stackup {
        let cu = |l| StackLayer { kind: StackKind::Copper(l), thickness: 35_000, material: "copper".into(), er: None, loss_tan: None, role: LayerRole::Signal, velocity: None };
        let mask = |s| StackLayer { kind: StackKind::Mask(s), thickness: 10_000, material: "solder mask".into(), er: Some(3.3), loss_tan: None, role: LayerRole::Signal, velocity: None };
        let silk = |s| StackLayer { kind: StackKind::Silk(s), thickness: 0, material: "silk".into(), er: None, loss_tan: None, role: LayerRole::Signal, velocity: None };
        Stackup {
            layers: vec![
                silk(Side::Top),
                mask(Side::Top),
                cu(LayerId::F_CU),
                StackLayer { kind: StackKind::Dielectric { core: true }, thickness: 1_510_000, material: "FR-4".into(), er: Some(4.5), loss_tan: Some(0.02), role: LayerRole::Signal, velocity: None },
                cu(LayerId::B_CU),
                mask(Side::Bottom),
                silk(Side::Bottom),
            ],
            finish: Finish::LeadFreeHasl,
            thickness: 1_600_000,
            mask_color: 0x1a5c2eff,
            silk_color: 0xf0f0f0ff,
        }
    }
    /// The copper layers, top to bottom.
    pub fn copper(&self) -> Vec<LayerId> {
        self.layers.iter().filter_map(|l| if let StackKind::Copper(id) = l.kind { Some(id) } else { None }).collect()
    }
    pub fn copper_set(&self) -> LayerSet {
        LayerSet::of(&self.copper())
    }
}

/// A placement's own pose for one of its footprint's labels (its Reference or its Value text), in the
/// footprint frame like an `FpText`: `at` the text's centre, `rot` added to the placement's turn
/// (`Angle::ZERO` for none). `pinned`: a person put it there; `arrange_references` never moves it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LabelPose {
    pub at: P,
    pub rot: Angle,
    pub pinned: bool,
}
canon_struct!(LabelPose { at, rot, pinned });

/// A placed component: the board's copy of its footprint, with the net of each pad.
#[derive(Clone, PartialEq, Debug)]
pub struct Placed {
    pub id: ItemId,
    pub comp: CompId,
    pub path: String,
    pub refdes: String,
    pub footprint: Arc<Footprint>,
    /// The net of each footprint pad (`None`: unconnected or mechanical).
    pub pad_nets: Vec<Option<NetId>>,
    pub at: P,
    pub rot: Angle,
    pub side: Side,
    pub locked: bool,
    pub origin: Origin,
    /// The part body's height band (min, max) up from the board surface on `side`, set at
    /// placement from its `Body3d` (`Placed::band_of`). `None` is every height. A keepout with a
    /// band (`BoardKeepout::z`) catches the part only where the two bands overlap.
    pub body_z: Option<(Nm, Nm)>,
    /// The placement's own component value (a board read from a file keeps one per placement).
    /// `None` draws the footprint's Value text.
    pub value: Option<String>,
    /// The placement's own Reference label pose. `None` draws the footprint's Reference text where
    /// the footprint puts it.
    pub ref_label: Option<LabelPose>,
    /// The placement's own Value label pose. `None` draws the footprint's Value text where the
    /// footprint puts it.
    pub value_label: Option<LabelPose>,
    /// The part's legend: what it does (`TEMPO`) as silkscreen text beside it, a footprint-frame
    /// `User` text that moves with the part. The board's reader makes it from the part's `label`
    /// field and places it clear of pads and other parts (`text::place_legends`). `None`: none.
    pub legend: Option<fab_elec_parts::FpText>,
}
// Additive: `body_z` is written only when set, so a placement without a band keeps its hash.
// `value` is written only when set, tagged separately from the band, so `None` keeps its hash.
// `ref_label`, `value_label` and `legend` likewise: each written only when set, with its own tag
// (3, 4, 5).
impl fab_elec_core::canon::Canon for Placed {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let Placed { id, comp, path, refdes, footprint, pad_nets, at, rot, side, locked, origin, body_z, value, ref_label, value_label, legend } = self;
        id.canon(w);
        comp.canon(w);
        path.canon(w);
        refdes.canon(w);
        footprint.canon(w);
        pad_nets.canon(w);
        at.canon(w);
        rot.canon(w);
        side.canon(w);
        locked.canon(w);
        origin.canon(w);
        if body_z.is_some() {
            body_z.canon(w);
        }
        if let Some(value) = value {
            w.tag(2);
            value.canon(w);
        }
        if let Some(label) = ref_label {
            w.tag(3);
            label.canon(w);
        }
        if let Some(label) = value_label {
            w.tag(4);
            label.canon(w);
        }
        if let Some(legend) = legend {
            w.tag(5);
            legend.canon(w);
        }
    }
}

impl Placed {
    /// Every text the placement draws: its footprint's, then its legend.
    pub fn texts(&self) -> impl Iterator<Item = &fab_elec_parts::FpText> {
        self.footprint.texts.iter().chain(&self.legend)
    }

    /// A body's height band above the board surface: from its lowest point (its pose's height,
    /// not below the surface) to its top (`Body3d::height`). `None` without a body.
    pub fn band_of(body: Option<&fab_elec_parts::Body3d>) -> Option<(Nm, Nm)> {
        body.map(|b| {
            let lo = b.offset.t[2].max(0);
            (lo, b.height.max(lo))
        })
    }
}

/// What a track end is attached to (E4 A9). An attached end takes its position from its anchor
/// (a pad's centre, a via's or hole's centre). A `Track` end sits at `at` on its host's centreline
/// and moves with the host; a `seg` past the host's end means the host's end. A `Free` end is a
/// dangling end DRC reports.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Anchor {
    #[default]
    Free,
    Pad { placed: ItemId, pad: fab_elec_core::PadIx },
    Via(ItemId),
    Track { id: ItemId, at: OnTrack },
    Hole(ItemId),
}
canon_enum!(Anchor { 0 => Free, 1 => Pad { placed, pad }, 2 => Via(v), 3 => Track { id, at }, 4 => Hole(h) });

impl Anchor {
    pub fn item(self) -> Option<ItemId> {
        match self {
            Anchor::Free => None,
            Anchor::Pad { placed, .. } => Some(placed),
            Anchor::Via(i) | Anchor::Track { id: i, .. } | Anchor::Hole(i) => Some(i),
        }
    }
}

/// A track: a path of lines and arcs on one copper layer.
#[derive(Clone, PartialEq, Debug)]
pub struct Track {
    pub id: ItemId,
    pub net: NetId,
    pub layer: LayerId,
    /// Width when `widths` is `None`.
    pub w: Nm,
    /// One width per vertex (tapers); `None` is uniform `w`.
    pub widths: Option<Vec<Nm>>,
    pub path: Path,
    /// Start and end anchors.
    pub ends: [Anchor; 2],
    pub locked: bool,
    pub origin: Origin,
}
canon_struct!(Track { id, net, layer, w, widths, path, ends, locked, origin });

impl Track {
    /// The width of segment `i` (the wider of its two vertices for a taper).
    pub fn seg_width(&self, i: usize) -> Nm {
        match &self.widths {
            Some(ws) => ws.get(i).copied().unwrap_or(self.w).max(ws.get(i + 1).copied().unwrap_or(self.w)),
            None => self.w,
        }
    }
    pub fn max_width(&self) -> Nm {
        self.widths.as_ref().and_then(|w| w.iter().max().copied()).unwrap_or(self.w).max(self.w)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Via {
    pub id: ItemId,
    pub net: NetId,
    pub at: P,
    pub drill: Nm,
    pub pad: Nm,
    /// First and last copper layer, in stack order.
    pub span: (LayerId, LayerId),
    pub kind: ViaKind,
    pub tent: Tent,
    /// Back-drilled from this side down to (not through) this layer, removing the stub (E6 6.4).
    /// Derived by the engine, written for output.
    pub backdrill: Option<(Side, LayerId)>,
    pub locked: bool,
    pub origin: Origin,
    /// Which span layers carry a ring; the drill and barrel are on the whole span in every mode.
    pub flash: Flash,
}
// Additive: `flash` is written only when it is not `All`, so a via that rings every layer keeps its hash.
impl fab_elec_core::canon::Canon for Via {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let Via { id, net, at, drill, pad, span, kind, tent, backdrill, locked, origin, flash } = self;
        id.canon(w);
        net.canon(w);
        at.canon(w);
        drill.canon(w);
        pad.canon(w);
        span.canon(w);
        kind.canon(w);
        tent.canon(w);
        backdrill.canon(w);
        locked.canon(w);
        origin.canon(w);
        if *flash != Flash::All {
            flash.canon(w);
        }
    }
}

/// Where a via carries a ring (its annular pad) within its span. `Board::rings` resolves the layers.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Flash {
    /// On every span layer.
    #[default]
    All,
    /// Where same-net copper touches the ring, and where a zone of the net reaches it.
    Connected,
    /// `Connected`, and always on the span's two end layers.
    ConnectedEnds,
    /// On the span's two end layers only.
    Ends,
}
canon_tags!(Flash);

impl Flash {
    /// The rings follow the copper around the via, so `Board::rings` keeps them in its table.
    pub fn follows_copper(self) -> bool {
        matches!(self, Flash::Connected | Flash::ConnectedEnds)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum ViaKind {
    #[default]
    Through,
    Blind,
    Buried,
    Micro,
}
canon_tags!(ViaKind);

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Tent {
    #[default]
    Both,
    Front,
    Back,
    None,
}
canon_tags!(Tent);

/// A copper zone (pour). Its fill is computed (`zone::fill`), never stored.
#[derive(Clone, PartialEq, Debug)]
pub struct Zone {
    pub id: ItemId,
    pub net: Option<NetId>,
    pub name: String,
    pub layers: LayerSet,
    pub area: Area,
    /// Higher fills first and is subtracted from lower ones.
    pub priority: u16,
    pub settings: ZoneSettings,
    pub locked: bool,
    pub origin: Origin,
}
canon_struct!(Zone { id, net, name, layers, area, priority, settings, locked, origin });

/// A zone's area: the whole board (following the outline through `SetOutline`) or a polygon.
#[derive(Clone, PartialEq, Debug)]
pub enum Area {
    Board,
    Poly(PolySet),
}
canon_enum!(Area { 0 => Board, 1 => Poly(p) });

#[derive(Clone, PartialEq, Debug)]
pub struct ZoneSettings {
    /// Clearance to foreign copper: it can widen the rule matrix's gap, never narrow it; `None`
    /// keeps the matrix's.
    pub clearance: Option<Nm>,
    pub min_width: Nm,
    pub connect: ZoneConnect,
    pub thermal: ThermalRule,
    pub islands: Islands,
}
canon_struct!(ZoneSettings { clearance, min_width, connect, thermal, islands });

impl Default for ZoneSettings {
    fn default() -> Self {
        ZoneSettings {
            clearance: None,
            min_width: 200_000,
            connect: ZoneConnect::Thermal,
            thermal: ThermalRule { gap: 300_000, spoke: 300_000, angle: Angle::ZERO },
            islands: Islands::Remove,
        }
    }
}

/// What happens to fill islands that touch no copper of the zone's net.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Islands {
    #[default]
    Remove,
    Keep,
}
canon_tags!(Islands);

#[derive(Clone, PartialEq, Debug)]
pub struct BoardKeepout {
    pub id: ItemId,
    pub area: PolySet,
    pub layers: LayerSet,
    pub forbid: Forbid,
    pub reason: String,
    pub origin: Origin,
    /// The height band it forbids (min, max), measured up from the board surface on each side its
    /// layers name: a body is caught only where its band there (`Placed::body_z`) overlaps this
    /// one, so a light pipe's keepout spares the short LED under it. `None` is every height.
    pub z: Option<(Nm, Nm)>,
}
// Additive: `z` is written only when set, so a keepout without a band keeps the hash it had.
impl fab_elec_core::canon::Canon for BoardKeepout {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let BoardKeepout { id, area, layers, forbid, reason, origin, z } = self;
        id.canon(w);
        area.canon(w);
        layers.canon(w);
        forbid.canon(w);
        reason.canon(w);
        origin.canon(w);
        if z.is_some() {
            z.canon(w);
        }
    }
}

/// A mounting or tooling hole (not a footprint pad).
#[derive(Clone, PartialEq, Debug)]
pub struct Hole {
    pub id: ItemId,
    pub name: String,
    pub at: P,
    pub drill: Nm,
    /// Copper ring diameter (plated holes).
    pub pad: Option<Nm>,
    pub net: Option<NetId>,
    pub plated: bool,
    pub origin: Origin,
}
canon_struct!(Hole { id, name, at, drill, pad, net, plated, origin });

/// A cutout through the board (derived ones follow another unit's feature, E2.17).
#[derive(Clone, PartialEq, Debug)]
pub struct Cutout {
    pub id: ItemId,
    pub name: String,
    pub contour: Contour,
    pub origin: Origin,
}
canon_struct!(Cutout { id, name, contour, origin });

#[derive(Clone, PartialEq, Debug)]
pub struct Graphic {
    pub id: ItemId,
    pub layer: LayerId,
    pub gfx: Gfx,
    pub locked: bool,
    pub origin: Origin,
}
canon_struct!(Graphic { id, layer, gfx, locked, origin });

#[derive(Clone, PartialEq, Debug)]
pub struct Text {
    pub id: ItemId,
    pub layer: LayerId,
    pub text: String,
    pub at: P,
    pub rot: Angle,
    pub size: Nm,
    pub thick: Nm,
    pub mirrored: bool,
    pub locked: bool,
    pub origin: Origin,
}
canon_struct!(Text { id, layer, text, at, rot, size, thick, mirrored, locked, origin });

/// A tuning pattern (E6 4.2, 13.1 7): a board object holding its parameters, normally
/// `Origin::Solved { by: tuner }`. The tuner regenerates its meander copper (ordinary tracks
/// listed in `copper`, of the same origin) whenever its host span or target changes, so it is
/// elastic like any solved copper. Not copper itself: connectivity, DRC and fills ignore it.
#[derive(Clone, PartialEq, Debug)]
pub struct Tuning {
    pub id: ItemId,
    pub net: NetId,
    /// The other member when a pair is tuned as one band.
    pub pair: Option<NetId>,
    /// The host track span, in order along the net.
    pub host: Vec<ItemId>,
    pub target: TuneTarget,
    pub side: TuneSide,
    /// Amplitude range and bump pitch.
    pub amp: (Nm, Nm),
    pub pitch: Nm,
    pub style: TuneStyle,
    /// The tracks it generated.
    pub copper: Vec<ItemId>,
    pub origin: Origin,
}
canon_struct!(Tuning { id, net, pair, host, target, side, amp, pitch, style, copper, origin });

#[derive(Clone, PartialEq, Debug)]
pub enum TuneTarget {
    Len(Nm),
    /// Seconds.
    Delay(f64),
    /// The longest member of this length-match or lane group.
    Group(String),
    /// Skew to another net within a tolerance.
    Skew { to: NetId, tol: Nm },
}
canon_enum!(TuneTarget { 0 => Len(l), 1 => Delay(t), 2 => Group(g), 3 => Skew { to, tol } });

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum TuneSide {
    Left,
    Right,
    #[default]
    Both,
}
canon_tags!(TuneSide);

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum TuneStyle {
    /// Alternating tangent arcs.
    #[default]
    Organic,
    /// Rounded trapezoids.
    AnyAngle,
    /// Chamfered trapezoids.
    FortyFive,
}
canon_tags!(TuneStyle);

/// Which list an item is in.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ItemKind {
    Placed,
    Track,
    Via,
    Zone,
    Keepout,
    Hole,
    Cutout,
    Graphic,
    Text,
    Tuning,
}

impl Board {
    /// An empty board of `unit` on the standard 2-layer stack with the standard rules.
    pub fn new(unit: fab_elec_core::BoardId, name: &str) -> Board {
        Board { unit, name: name.into(), stackup: Stackup::standard_2_layer(), rules: Rules::standard(), ..Board::default() }
    }
    /// Mirror the circuit's nets, classes, differential pairs, length matches, edge parts and spots.
    pub fn mirror(&mut self, c: &Circuit) {
        self.nets = c.nets.iter().map(|n| BoardNet { name: n.name.clone(), class: n.class }).collect();
        self.classes = c.classes.clone();
        self.pairs = pairs_of(c);
        self.length_matches = c.intents.iter().filter_map(|intent| match intent {
            Intent::LengthMatch { nets, tol } => Some(LengthMatch { nets: nets.clone(), tol: *tol }),
            _ => None,
        }).collect();
        self.edge_parts = edge_parts_of(c);
        self.spots = spots_of(c);
    }
    pub fn copper_count(&self) -> u8 {
        self.stackup.copper().len() as u8
    }
    pub fn copper_set(&self) -> LayerSet {
        self.stackup.copper_set()
    }
    /// Rebuild the derived item index from all lists in O(items).
    pub fn reindex(&mut self) {
        self.index.0.clear();
        for kind in [
            ItemKind::Placed, ItemKind::Track, ItemKind::Via, ItemKind::Zone, ItemKind::Keepout,
            ItemKind::Hole, ItemKind::Cutout, ItemKind::Graphic, ItemKind::Text, ItemKind::Tuning,
        ] {
            self.index_from(kind, 0);
        }
        self.reindex_rings();
    }
    /// Refresh only the suffix whose indices an insertion or removal changed.
    fn index_from(&mut self, kind: ItemKind, start: usize) {
        macro_rules! index {
            ($list:ident) => {
                for (i, item) in self.$list.iter().enumerate().skip(start) {
                    self.index.0.insert(item.id, (kind, i as u32));
                }
            };
        }
        match kind {
            ItemKind::Placed => index!(placed),
            ItemKind::Track => index!(tracks),
            ItemKind::Via => index!(vias),
            ItemKind::Zone => index!(zones),
            ItemKind::Keepout => index!(keepouts),
            ItemKind::Hole => index!(holes),
            ItemKind::Cutout => index!(cutouts),
            ItemKind::Graphic => index!(graphics),
            ItemKind::Text => index!(texts),
            ItemKind::Tuning => index!(tunings),
        }
    }
    /// Which list holds `id`, and where. Indexed hits are O(1); stale or missing entries fall
    /// back to a scan so direct edits to the public lists remain supported.
    pub fn locate(&self, id: ItemId) -> Option<(ItemKind, usize)> {
        if let Some(&(k, i)) = self.index.0.get(&id) {
            let found = match k {
                ItemKind::Placed => self.placed.get(i as usize).map(|x| x.id),
                ItemKind::Track => self.tracks.get(i as usize).map(|x| x.id),
                ItemKind::Via => self.vias.get(i as usize).map(|x| x.id),
                ItemKind::Zone => self.zones.get(i as usize).map(|x| x.id),
                ItemKind::Keepout => self.keepouts.get(i as usize).map(|x| x.id),
                ItemKind::Hole => self.holes.get(i as usize).map(|x| x.id),
                ItemKind::Cutout => self.cutouts.get(i as usize).map(|x| x.id),
                ItemKind::Graphic => self.graphics.get(i as usize).map(|x| x.id),
                ItemKind::Text => self.texts.get(i as usize).map(|x| x.id),
                ItemKind::Tuning => self.tunings.get(i as usize).map(|x| x.id),
            };
            if found == Some(id) {
                return Some((k, i as usize));
            }
        }
        macro_rules! find {
            ($list:ident, $k:ident) => {
                if let Some(i) = self.$list.iter().position(|x| x.id == id) {
                    return Some((ItemKind::$k, i));
                }
            };
        }
        find!(placed, Placed);
        find!(tracks, Track);
        find!(vias, Via);
        find!(zones, Zone);
        find!(keepouts, Keepout);
        find!(holes, Hole);
        find!(cutouts, Cutout);
        find!(graphics, Graphic);
        find!(texts, Text);
        find!(tunings, Tuning);
        None
    }
    /// The item `id`, cloned.
    pub fn item(&self, id: ItemId) -> Option<Item> {
        let (k, i) = self.locate(id)?;
        Some(match k {
            ItemKind::Placed => Item::Placed(self.placed[i].clone()),
            ItemKind::Track => Item::Track(self.tracks[i].clone()),
            ItemKind::Via => Item::Via(self.vias[i].clone()),
            ItemKind::Zone => Item::Zone(self.zones[i].clone()),
            ItemKind::Keepout => Item::Keepout(self.keepouts[i].clone()),
            ItemKind::Hole => Item::Hole(self.holes[i].clone()),
            ItemKind::Cutout => Item::Cutout(self.cutouts[i].clone()),
            ItemKind::Graphic => Item::Graphic(self.graphics[i].clone()),
            ItemKind::Text => Item::Text(self.texts[i].clone()),
            ItemKind::Tuning => Item::Tuning(self.tunings[i].clone()),
        })
    }
    pub fn placed_of(&self, comp: CompId) -> Option<&Placed> {
        self.placed.iter().find(|p| p.comp == comp)
    }
    pub fn placed_item(&self, id: ItemId) -> Option<&Placed> {
        match self.locate(id)? {
            (ItemKind::Placed, i) => self.placed.get(i),
            _ => None,
        }
    }
    pub fn net(&self, name: &str) -> Option<NetId> {
        self.nets.iter().position(|n| n.name == name).map(|i| NetId(i as u32))
    }
    /// The outline as a polygon set (even-odd), arcs within `max_err`, minus the cutouts.
    pub fn outline_polyset(&self, max_err: Nm) -> PolySet {
        let rings: Vec<Vec<P>> = self.outline.iter().map(|c| c.to_ring(max_err)).collect();
        let mut set = PolySet::from_rings_even_odd(&rings);
        for c in &self.cutouts {
            set = set.difference(&PolySet::from_ring_nonzero(c.contour.to_ring(max_err)));
        }
        set
    }
    pub fn outline_bbox(&self) -> BBox {
        self.outline.iter().fold(BBox::EMPTY, |b, c| b.union(c.bbox()))
    }
    /// Issue a fresh item id.
    pub fn alloc_id(&mut self) -> ItemId {
        let id = ItemId(self.next_item);
        self.next_item += 1;
        id
    }
}

#[test]
fn a_port_room_moves_with_its_position_and_keeps_its_board_axes_and_surface() {
    use fab_elec_core::{mm, BoardId};
    use placement::{Conflict, PlacementIndex};

    let area = PolySet::from_ring_nonzero(vec![P::mm(1.0, 2.0), P::mm(4.0, 2.0), P::mm(1.0, 6.0)]);
    let band = (mm(0.5), mm(2.0));
    let from = FeatureRef::Panel("port A/Raised cover".into());
    let local = PortRoom { comp: CompId(2), room: Some(area.clone()), keepout: Some(CarriedKeepout {
        from: from.clone(), area, band, side: Side::Bottom,
    }) };
    let at = P::mm(10.0, 20.0);
    let placed = local.placed(at);
    let expected = PolySet::from_ring_nonzero(vec![P::mm(11.0, 22.0), P::mm(14.0, 22.0), P::mm(11.0, 26.0)]);
    assert_eq!(placed.room, Some(expected.clone()));
    assert_eq!(placed.keepout, Some(CarriedKeepout { from: from.clone(), area: expected.clone(), band, side: Side::Bottom }));
    assert_eq!(placed.relative_to(at), local);

    let mut board = Board::new(BoardId(0), "placement");
    let footprint = Arc::new(Footprint {
        courtyard: PolySet::rect(BBox::new(P::mm(-0.1, -0.1), P::mm(0.1, 0.1))), ..Default::default()
    });
    let old = P::mm(12.0, 23.0); // Inside the published triangle, clear of the owner's body.
    let by = P::mm(20.0, 0.0);
    for (comp, pos, rot, height) in [
        (local.comp, at, Angle::QUARTER, band.0),
        (CompId(3), old.add(by), Angle::ZERO, band.1),
        (CompId(4), P::ZERO, Angle::ZERO, band.0),
    ] {
        let id = board.alloc_id();
        board.placed.push(Placed {
            id, comp, at: pos, rot, side: Side::Bottom, footprint: footprint.clone(), body_z: Some((0, height)),
            path: format!("part{}", comp.0), refdes: String::new(), pad_nets: vec![], locked: false, origin: Origin::Authored,
            value: None, ref_label: None, value_label: None, legend: None,
        });
    }
    let (owner, tall, short) = (board.placed[0].id, board.placed[1].id, board.placed[2].id);
    let fixed_offset = P::mm(0.0, 10.0);
    let other_offset = P::mm(0.0, 20.0);
    let other = FeatureRef::Panel("port B/Raised cover".into());
    // Both ports and the fixed record have the same reason. Publish B and the fixed one
    // before A, but list A's port first. Include a second old copy from A's source.
    for (source, offset) in [
        (other.clone(), other_offset), (FeatureRef::Panel("Raised cover".into()), fixed_offset),
        (from.clone(), P::ZERO), (from, P::mm(0.0, -10.0)),
    ] {
        let id = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id, area: expected.translated(offset), z: Some(band), layers: LayerSet::of(&[LayerId::B_CU]), forbid: Forbid::FOOTPRINTS,
            reason: "Raised cover".into(), origin: Origin::Derived { from: source },
        });
    }
    board.reindex();
    let stationary = PortRoom { comp: CompId(4), room: None, keepout: Some(CarriedKeepout {
        from: other, area: expected.translated(other_offset), band, side: Side::Bottom,
    }) };
    let ports = [local, stationary];
    let moving = [owner].into_iter().collect();
    let hits = |index: &PlacementIndex, id, pos, rot, side| {
        index.pose_conflicts(id, pos, rot, side).filter_map(|c| match c {
            Conflict::Keepout { owner, .. } => Some(owner),
            _ => None,
        }).collect::<Vec<_>>()
    };
    let bottom = Side::Bottom;
    assert_eq!(hits(&PlacementIndex::without(&board, &moving), tall, old, Angle::ZERO, bottom), [board.keepouts[2].id]);
    let mut index = PlacementIndex::with_carried_keepouts(&board, &moving, &ports);
    assert!(hits(&index, tall, old, Angle::ZERO, bottom).is_empty(), "the moving part's published keepout drops out");
    assert!(hits(&index, tall, old.add(P::mm(0.0, -10.0)), Angle::ZERO, bottom).is_empty());
    assert_eq!(hits(&index, tall, old.add(other_offset), Angle::ZERO, bottom), [board.keepouts[0].id], "the other port stays exactly once");
    assert_eq!(hits(&index, tall, old.add(fixed_offset), Angle::ZERO, bottom), [board.keepouts[1].id], "the fixed record stays exactly once");
    assert_eq!(hits(&index, owner, at.add(by), Angle::ZERO, Side::Top), [tall], "the candidate keepout reaches the standing body");
    index.place(owner, at.add(by), Angle::ZERO, Side::Top);
    assert!(hits(&index, tall, old, Angle::ZERO, bottom).is_empty(), "the old position stays clear");
    assert_eq!(hits(&index, tall, old.add(by), Angle::ZERO, bottom), [owner], "the carried band blocks the tall body at its new pose");
    assert!(hits(&index, short, old.add(by), Angle::ZERO, bottom).is_empty(), "the shorter body ends at the band's lower face");
    assert!(hits(&index, tall, old.add(by), Angle::ZERO, Side::Top).is_empty(), "the opposite surface stays clear");

    board.placed[0].at = at.add(by);
    board.placed[0].rot = Angle::ZERO;
    board.placed[0].side = Side::Top;
    let (projected, removed) = placement::projected_carried_keepouts(&board, &moving, &ports);
    assert_eq!(removed, [board.keepouts[3].id].into_iter().collect());
    let mut moved = board.keepouts[2].clone();
    moved.area = moved.area.translated(by);
    assert_eq!(projected, [(owner, moved)], "the check replaces only the moving owner's derived member");
}
