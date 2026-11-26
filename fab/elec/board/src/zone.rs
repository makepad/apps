//! Zone fill on an origin-anchored lattice with side `1000 nm << level`. Rules and zone
//! settings choose the smallest level at least 10 whose side covers eight times the largest
//! fill reach; moving copper or changing the outline does not move the lattice.
//!
//! Obstacles come from a piece index through one classifier in both full fills and updates.
//! The index query window is a superset of what the classifier accepts (`fill_select`, plus
//! relief entries for pads with their own thermal), so update equals full by construction.
//! A blind or buried via's drill cuts only the layers it spans. Board and footprint keepouts
//! that forbid zones cut the fill through the index. Higher-priority grown zone bases are read
//! from the board in id order. For `h = min_width / 2`, the
//! dual opening erodes the base once, subtracts obstacles grown by an extra `h`, then dilates
//! by `h`. `grown(shape, g + h)` contains `shape ⊕ (g + h)`: its extra `ARC_ERR` covers the
//! inscribed offset. Thus `E ⊆ erode(S*, h)` for `S* = base − ∪ (shape ⊕ g)`, and inscribed
//! round-join dilation gives `E.offset(h) ⊆ open(S*) ⊆ S*`, preserving at least `gap` from
//! foreign copper on the same rounding terms as `grown`. A final intersection holds the base,
//! keepout, relief inner and spoke edges exactly. With `m = 4 * ARC_ERR`, `F ∩ T` reads `E`
//! within `h` of `T`, and `E` there reads obstacles within `gap + h`: clipping `E` to
//! `T.inflate(h + m)` and selecting with `T.inflate(min_width + m)` suffice. At `h == 0`
//! there are no opening offsets. This is a new canonical tessellation, with vertices moving
//! by up to `ARC_ERR` against the old opening. Tiles, updates and full fills use this one
//! function, so update equals full by construction.
//! Full fills and updates use one plan of independent tiles; updates recompute reach-inflated windows
//! meeting a changed item's side, or all tiles when the zone, lattice, copper layers or global inputs change.
//! Unchanged tiles are shared through `Arc`. Indexed updates reuse the caller's fixed pieces;
//! held or absent fill pieces do not affect selection. A generation mismatch rebuilds the index.
//!
//! Each component of window `E` is a feed; its components clipped to the tile are cores.
//! Where several feeds reach one fill polygon, their dilations partition it without removing
//! copper. A geometry-free pass joins feed nodes across positive-length core cuts and side runs
//! covered by only one feed. Its minimum-width islands that hold own copper supply connectivity
//! labels, each named by its smallest polygon reference; one without own copper keeps its copper
//! island's label. Retention separately joins original polygon groups and polygon cuts, so it
//! still follows the full copper. Labels live outside the immutable tiles; reader keys hash tile
//! content and retention only, never the connectivity labels.
//!
//! Recording splits window `E`, clips and splits each feed into cores, sorts canonically and scans
//! cuts. A feed reaches the polygon holding one of its cores, and beyond that only polygons with a
//! side run within `h` of it: one containment test per core and feed-to-run gaps near the sides,
//! never a feed against a whole polygon. A single candidate keeps the polygon's bytes without
//! another offset. Only neck polygons compute cached
//! feed dilations, partitions and overlap exclusions. At zero radius every polygon is its own
//! feed and core. The geometry-free neck census compares the two partitions only on demand.
//!
//! Opening geometry costs O(v log v) per tile in the window's vertices, after input selection; the
//! islands passes are linear in tiles, polygons, nodes and cut intervals, with amortized union-find
//! operations. Clearance to a fill is the minimum over its pieces. Checks of copper itself
//! (minimum width, slivers, or a starved thermal's spoke count) must judge whole islands:
//! clipping can make a thin tile piece of copper that is not thin in the fill.

use crate::connect::ConnItem;
use crate::index::{Piece, PieceIndex, PieceKind};
use crate::ops::BoardDelta;
use crate::rules::{RuleItem, RuleKey, RuleMatrix};
use crate::{Area, Board, Islands, Zone, ZoneSettings, ARC_ERR};
use fab_elec_core::{
    clearance, Angle, BBox, Canon, ClassId, ContentHash, ItemId, LayerId, NetId, Nm, PolySet, Shape, Writer, P,
};
use fab_elec_parts::{Forbid, PadKind, ThermalRule, ZoneConnect};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::Arc;

/// A lattice tile, ordered by row (`y`) and then column (`x`).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    /// Column relative to the origin, including negative columns.
    pub x: i32,
    /// Row relative to the origin, including negative rows.
    pub y: i32,
}
fab_elec_core::canon_struct!(TileKey { x, y });


impl Ord for TileKey {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.y, self.x).cmp(&(other.y, other.x))
    }
}

impl PartialOrd for TileKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A polygon's canonical reference within a fill layer, ordered by tile and then ordinal.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PolyRef {
    /// The tile containing the polygon.
    pub tile: TileKey,
    /// The polygon's index in `FillTile::polys`.
    pub ord: u32,
}

/// An origin-anchored square lattice chosen from rules and zone settings alone.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct FillLattice {
    /// Tile side exponent: the side is `1000 nm << level`.
    pub level: u8,
}

impl FillLattice {
    /// Choose the smallest level at least 10 covering eight times every zone's fill reach.
    /// Without zones, use the base `ZoneSettings` defaults.
    pub fn of(board: &Board, rules: &RuleMatrix) -> FillLattice {
        let reach = board.zones.iter().map(|z| fill_reach(rules, &z.settings)).max()
            .unwrap_or_else(|| fill_reach(rules, &ZoneSettings::default()));
        let mut level = 10;
        while (1000_i64 << level) < 8 * i64::from(reach) {
            level += 1;
        }
        FillLattice { level }
    }

    /// The tile side in integer nanometres.
    pub fn side(&self) -> Nm {
        Nm::try_from(1000_i64 << self.level).expect("fill tile side exceeds the coordinate range")
    }

    /// The inclusive box `[x*s, (x+1)*s] × [y*s, (y+1)*s]` of a tile.
    pub fn tile_box(&self, k: TileKey) -> BBox {
        let s = i64::from(self.side());
        let coord = |n: i64| (n * s).clamp(i64::from(Nm::MIN), i64::from(Nm::MAX)) as Nm;
        BBox::new(
            P::new(coord(i64::from(k.x)), coord(i64::from(k.y))),
            P::new(coord(i64::from(k.x) + 1), coord(i64::from(k.y) + 1)),
        )
    }

    /// Every tile whose inclusive box meets `b`, in canonical order (empty boxes give none).
    pub fn tiles_in(&self, b: BBox) -> Vec<TileKey> {
        if b.is_empty() {
            return Vec::new();
        }
        let s = self.side();
        let first = |n: Nm| n.div_euclid(s) - i32::from(n.rem_euclid(s) == 0);
        let mut keys = Vec::new();
        for y in first(b.min.y)..=b.max.y.div_euclid(s) {
            for x in first(b.min.x)..=b.max.x.div_euclid(s) {
                keys.push(TileKey { x, y });
            }
        }
        keys
    }
}

/// Immutable once computed and shared between fills (`Arc`): an update copies pointers plus changed tiles.
#[derive(Clone, PartialEq, Debug)]
pub struct FillTile {
    /// The tile's lattice address.
    pub key: TileKey,
    /// Connected polygons, possibly with holes, in canonical bounding-box and content order.
    pub polys: Vec<PolySet>,
    /// Whether each polygon touches fixed copper of the zone's net.
    pub own: Vec<bool>,
    /// Sorted `(lo, hi, ord)` boundary runs on W, E, S, N; coordinates are y, y, x, x.
    pub cuts: [Vec<(Nm, Nm, u32)>; 4],
    /// Feed node of each polygon; nodes follow canonical feed order, then unassigned polygons.
    pub node: Vec<u32>,
    /// Smallest polygon ordinal cut from the same original polygon, for copper retention.
    pub group: Vec<u32>,
    /// Whether each node has no core in this tile, including polygons with no candidate feed.
    pub foreign: Vec<bool>,
    /// Sorted `(lo, hi, node)` core runs on W, E, S, N, in the same coordinates as `cuts`.
    pub core_cuts: [Vec<(Nm, Nm, u32)>; 4],
    /// Sorted `(lo, hi, node)` side runs covered by exactly one feed's fill.
    pub lone: [Vec<(Nm, Nm, u32)>; 4],
    /// Content hash of the key, polygon rings, own flags, cuts, nodes, groups, foreign flags and runs.
    pub hash: ContentHash,
}

impl FillTile {
    /// Build a tile whose supplied polygons are each their own feed and core, preserving their order.
    /// Used at zero opening radius and by callers supplying already partitioned copper.
    pub fn from_polys(key: TileKey, polys: Vec<PolySet>, own: Vec<bool>, b: BBox) -> Self {
        let cuts = boundary_cuts(&polys, b);
        let ordinals: Vec<_> = (0..polys.len()).map(|ord| ord as u32).collect();
        let mut tile = Self {
            key, own, foreign: vec![false; polys.len()], polys,
            node: ordinals.clone(), group: ordinals,
            core_cuts: cuts.clone(), lone: cuts.clone(), cuts, hash: Default::default(),
        };
        tile.rehash();
        tile
    }

    fn rehash(&mut self) {
        let mut w = Writer::hasher();
        self.key.x.canon(&mut w);
        self.key.y.canon(&mut w);
        w.count(self.polys.len());
        for poly in &self.polys {
            write_poly(poly, &mut w);
        }
        self.own.canon(&mut w);
        self.cuts.canon(&mut w);
        self.node.canon(&mut w);
        self.group.canon(&mut w);
        self.foreign.canon(&mut w);
        self.core_cuts.canon(&mut w);
        self.lone.canon(&mut w);
        self.hash = w.finish();
    }
}

/// The islands pass's output for one tile, parallel to `FillLayer::tiles`.
#[derive(Clone, PartialEq, Debug)]
pub struct TileLabels {
    /// Each polygon's minimum-width island's smallest member; used by connectivity only.
    pub island: Vec<PolyRef>,
    /// Whether each polygon's original copper island survives removal.
    pub kept: Vec<bool>,
    /// Reader key over `(FillTile::hash, kept)` only, never island labels.
    pub view_hash: ContentHash,
}

/// One copper layer's tiled fill and global island labels.
#[derive(Clone, PartialEq, Debug)]
pub struct FillLayer {
    /// The copper layer filled.
    pub layer: LayerId,
    /// Computed tiles, including empty tiles, sorted by key.
    pub tiles: Vec<Arc<FillTile>>,
    /// Island labels and retention, parallel to `tiles` and each tile's polygons.
    pub labels: Vec<TileLabels>,
    /// The vias whose ring the zone offers on this layer (`Board::rings`) and a kept polygon touches,
    /// sorted: where an offered ring is copper.
    pub reached: Vec<ItemId>,
}

impl FillLayer {
    /// Union of kept polygons, for export and one-shot readers only: O(V) over the layer.
    /// No per-edit or per-frame path calls this; those readers iterate tiles and key on `view_hash`.
    pub fn set(&self) -> PolySet {
        PolySet::union_all(self.kept().map(|(_, poly)| poly))
    }

    /// Kept polygons with their references in canonical `(tile, ord)` order.
    pub fn kept(&self) -> impl Iterator<Item = (PolyRef, &PolySet)> + '_ {
        self.tiles.iter().zip(&self.labels).flat_map(|(tile, labels)| {
            tile.polys.iter().zip(&labels.kept).enumerate().filter_map(move |(ord, (poly, &kept))| {
                kept.then_some((PolyRef { tile: tile.key, ord: ord as u32 }, poly))
            })
        })
    }
}

/// The tiled fill of one zone, published beside a board and never stored in it.
#[derive(Clone, PartialEq, Debug)]
pub struct ZoneFill {
    /// The source zone's id.
    pub zone: ItemId,
    /// The shared board lattice used by every layer.
    pub lattice: FillLattice,
    /// Filled copper layers in layer order.
    pub layers: Vec<FillLayer>,
}

impl ZoneFill {
    /// The fill on copper layer `l`, if the zone and board both include it.
    pub fn layer(&self, l: LayerId) -> Option<&FillLayer> {
        self.layers.iter().find(|layer| layer.layer == l)
    }

    /// Whether every layer has no kept polygons.
    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|layer| layer.kept().next().is_none())
    }

    /// Equal fills, comparing tiles by content hash and labels in full: O(tiles + polygons), no
    /// geometry. Readers use it to tell which zones an update refilled.
    pub fn same(&self, other: &ZoneFill) -> bool {
        self.zone == other.zone
            && self.lattice == other.lattice
            && self.layers.len() == other.layers.len()
            && self.layers.iter().zip(&other.layers).all(|(a, b)| {
                a.layer == b.layer
                    && a.labels == b.labels
                    && a.reached == b.reached
                    && a.tiles.len() == b.tiles.len()
                    && a.tiles.iter().zip(&b.tiles).all(|(x, y)| {
                        Arc::ptr_eq(x, y) || (x.key == y.key && x.hash == y.hash)
                    })
            })
    }
}

pub(crate) fn rule_item(c: ConnItem) -> RuleItem {
    match c {
        ConnItem::Pad { .. } => RuleItem::Pad,
        ConnItem::Track { .. } => RuleItem::Track,
        ConnItem::Via(_) => RuleItem::Via,
        ConnItem::Hole(_) => RuleItem::Hole,
        ConnItem::Zone { .. } => RuleItem::Zone,
    }
}

pub(crate) fn class_of(board: &Board, net: Option<NetId>) -> ClassId {
    net.and_then(|n| board.nets.get(n.ix())).map(|n| n.class).unwrap_or_default()
}

fn zone_base(board: &Board, rules: &RuleMatrix, z: &Zone) -> PolySet {
    let inner = board.outline_polyset(ARC_ERR).offset(-rules.edge(), ARC_ERR);
    clip_area(z, inner)
}

fn clip_area(z: &Zone, inner: PolySet) -> PolySet {
    match &z.area {
        Area::Board => inner,
        Area::Poly(p) => p.intersection(&inner),
    }
}

/// The gap a zone keeps from copper the pair rules want `rule` from: a zone's own clearance can
/// only widen it, as the reference fills and as the checks measure.
pub fn zone_gap(z: &Zone, rule: Nm) -> Nm {
    z.settings.clearance.map_or(rule, |own| own.max(rule))
}

fn grown(s: &Shape, by: Nm) -> PolySet {
    s.to_polyset(ARC_ERR).offset(by + ARC_ERR, ARC_ERR)
}

/// The farthest any input reaches into a tile's copper: the largest knockout gap (the larger of
/// the zone's own clearance and the rules'), the zone's thermal relief and the opening. A pad's own larger
/// thermal is carried by its item's fill extent instead.
// The near margin (min_width + 4 * ARC_ERR) plus fill_select never exceeds this reach.
fn fill_reach(rules: &RuleMatrix, settings: &ZoneSettings) -> Nm {
    rules.max_clearance().max(rules.hole_clearance()).max(settings.clearance.unwrap_or(0)) + 2 * settings.thermal.gap
        + settings.thermal.spoke + settings.min_width + 8 * ARC_ERR
}

/// The farthest a selected obstacle's box lies from a tile's `near` window, except a pad's own
/// thermal, which the index's relief entries carry.
fn fill_select(rules: &RuleMatrix, settings: &ZoneSettings) -> Nm {
    rules.max_clearance().max(rules.hole_clearance()).max(settings.clearance.unwrap_or(0))
        .max(2 * settings.thermal.gap + settings.thermal.spoke) + ARC_ERR
}

/// Shared indexed fill inputs, base erosion and ordered higher-priority bases for the dual opening.
struct Ctx<'a> {
    board: &'a Board,
    rules: &'a RuleMatrix,
    zone: &'a Zone,
    /// Fixed pieces from the same board as the zone and its settings.
    index: &'a PieceIndex,
    prep: &'a Prep,
}

/// Owned base offsets, selection bounds and higher-priority bases shared by every planned tile.
struct Prep {
    zkey: RuleKey,
    base: PolySet,
    /// Base eroded by half the minimum width, or the unchanged base when that radius is zero.
    base_e: PolySet,
    reach: Nm,
    /// Rule-bounded selection inflation; the index separately carries pads' own reliefs.
    select: Nm,
    lattice: FillLattice,
    /// Higher-priority bases grown by clearance, `ARC_ERR` and half the minimum width, in id/layer order.
    higher: Vec<(ItemId, LayerId, PolySet)>,
}

impl Prep {
    /// Precompute base offsets and selection bounds; fixed obstacles come from the supplied board index.
    fn new(board: &Board, rules: &RuleMatrix, zone: &Zone, lattice: FillLattice) -> Self {
        Self::at_radius(board, rules, zone, lattice, zone.settings.min_width / 2)
    }

    /// Static masks use radius zero; the fill uses half its minimum width for the dual opening.
    fn at_radius(board: &Board, rules: &RuleMatrix, zone: &Zone, lattice: FillLattice, h: Nm) -> Self {
        let reach = fill_reach(rules, &zone.settings);
        let select = fill_select(rules, &zone.settings);
        debug_assert!(zone.settings.min_width + 4 * ARC_ERR + select <= reach);
        let zkey = RuleKey { class: class_of(board, zone.net), item: RuleItem::Zone };
        let base = zone_base(board, rules, zone);
        let base_e = if h > 0 { base.offset(-h, ARC_ERR) } else { base.clone() };
        let mut higher = Vec::new();
        for other in &board.zones {
            if other.id == zone.id || other.priority <= zone.priority
                || (other.net.is_some() && other.net == zone.net)
            {
                continue;
            }
            let base = zone_base(board, rules, other);
            let okey = RuleKey { class: class_of(board, other.net), item: RuleItem::Zone };
            for layer in zone.layers.intersect(other.layers).intersect(board.copper_set()).iter() {
                let grown = base.offset(rules.clearance(zkey, okey, layer) + ARC_ERR + h, ARC_ERR);
                higher.push((other.id, layer, grown));
            }
        }
        higher.sort_by_key(|(id, layer, _)| (*id, *layer));
        Self {
            zkey,
            base,
            base_e,
            reach,
            select,
            lattice,
            higher,
        }
    }
}

/// The role of an accepted obstacle or mask; distances exclude the opening's extra half-width.
#[derive(PartialEq, Debug)]
enum SelectedKind {
    /// Foreign copper with its gap: the larger of the zone's clearance and the pairwise one.
    Foreign(Nm),
    /// A foreign or unplated drill, using the hole clearance.
    Drill,
    /// An own-net pad disconnected from the zone, with its clearance.
    Off(Nm),
    /// An own-net pad's effective thermal rule.
    Thermal(ThermalRule),
    /// A board or footprint keepout forbidding zones, with no clearance margin.
    Keepout,
}

/// Accepted obstacles, including keepout masks, and nearby own-net copper, each in candidate order.
#[derive(PartialEq, Debug)]
struct Selected<'p> {
    pieces: Vec<(&'p Piece, SelectedKind)>,
    own: Vec<&'p Piece>,
}

/// Classify fixed pieces with the same closed-box tests for indexed and exhaustive selection.
/// Keepouts forbidding zones use their uninflated boxes because they are margin-less masks.
fn select<'p>(
    ctx: &Ctx<'_>,
    layer: LayerId,
    near: BBox,
    candidates: impl Iterator<Item = &'p Piece>,
) -> Selected<'p> {
    let z = ctx.zone;
    let mut selected = Selected { pieces: Vec::new(), own: Vec::new() };
    for piece in candidates {
        // An offered via ring is the via's copper here whether a fill reaches it or not: the fill is a
        // function of the board, and its tiles do not change when the reach does.
        let c = match &piece.kind {
            PieceKind::Copper(c) | PieceKind::Offer(c) => c,
            PieceKind::Drill { net, plated, .. } => {
                let foreign = z.net.is_none() || *net != z.net;
                let inflate = ctx.rules.hole_clearance() + ARC_ERR;
                debug_assert!(inflate <= ctx.prep.select);
                if (foreign || !*plated) && piece.bbox.inflate(inflate).intersects(&near) {
                    selected.pieces.push((piece, SelectedKind::Drill));
                }
                continue;
            }
            PieceKind::Keepout { forbid, .. } => {
                if forbid.has(Forbid::ZONES) && piece.bbox.intersects(&near) {
                    selected.pieces.push((piece, SelectedKind::Keepout));
                }
                continue;
            }
            PieceKind::Fill(_) | PieceKind::Edge(_) => continue,
        };
        if z.net.is_none() || c.net != z.net {
            let gap = zone_gap(z, ctx.rules.clearance(ctx.prep.zkey, c.key(), layer));
            let inflate = gap + ARC_ERR;
            debug_assert!(inflate <= ctx.prep.select);
            if piece.bbox.inflate(inflate).intersects(&near) {
                selected.pieces.push((piece, SelectedKind::Foreign(gap)));
            }
            continue;
        }
        // Only copper meeting near can touch a polygon in this tile; distant reliefs are separate.
        if piece.bbox.intersects(&near) {
            selected.own.push(piece);
        }
        let ConnItem::Pad { placed, pad } = c.item else { continue };
        let Some(p) = ctx.board.placed_item(placed) else { continue };
        let Some(fp_pad) = p.footprint.pads.get(pad.ix()) else { continue };
        match fp_pad.zone_connect.unwrap_or(z.settings.connect) {
            ZoneConnect::Solid => continue,
            ZoneConnect::ThruThermal if fp_pad.kind != PadKind::Thru => continue,
            ZoneConnect::Off => {
                let gap = zone_gap(z, ctx.rules.clearance(ctx.prep.zkey, ctx.prep.zkey, layer));
                let inflate = gap + ARC_ERR;
                debug_assert!(inflate <= ctx.prep.select);
                if piece.bbox.inflate(inflate).intersects(&near) {
                    selected.pieces.push((piece, SelectedKind::Off(gap)));
                }
                continue;
            }
            ZoneConnect::Thermal | ZoneConnect::ThruThermal => {}
        }
        let t = fp_pad.thermal.as_ref().unwrap_or(&z.settings.thermal);
        let inflate = crate::geom::pad_relief(fp_pad).unwrap_or_else(|| {
            let inflate = 2 * t.gap + t.spoke;
            debug_assert!(inflate <= ctx.prep.select);
            inflate
        });
        if piece.bbox.inflate(inflate).intersects(&near) {
            selected.pieces.push((piece, SelectedKind::Thermal(t.clone())));
        }
    }
    selected
}

/// Ordered knockouts (already grown by `h`) and exact masks (grown by the caller for the opening).
/// Both fill and pour-path masks use this construction. `static_only` leaves out foreign copper
/// and drills, whose clearances the relaxer already enforces. Keepout masks precede thermals.
fn obstacle_masks(
    ctx: &Ctx<'_>,
    layer: LayerId,
    near: BBox,
    selected: &Selected<'_>,
    h: Nm,
    static_only: bool,
) -> (Vec<PolySet>, Vec<PolySet>) {
    let mut knock = Vec::new();
    let mut masks = Vec::new();
    for (piece, kind) in &selected.pieces {
        let gap = match kind {
            SelectedKind::Foreign(_) | SelectedKind::Drill if static_only => continue,
            SelectedKind::Foreign(gap) | SelectedKind::Off(gap) => *gap,
            SelectedKind::Drill => ctx.rules.hole_clearance(),
            SelectedKind::Thermal(_) => continue,
            SelectedKind::Keepout => {
                masks.push(piece.shape().to_polyset(ARC_ERR));
                continue;
            }
        };
        knock.push(grown(piece.shape(), gap + h));
        fab_elec_core::pace::pace();
    }
    for (_, l, base) in &ctx.prep.higher {
        if *l == layer && base.bbox().intersects(&near) {
            knock.push(base.clone());
        }
    }
    for (piece, kind) in &selected.pieces {
        let SelectedKind::Thermal(t) = kind else { continue };
        let PieceKind::Copper(c) = &piece.kind else { continue };
        fab_elec_core::pace::pace();
        let ConnItem::Pad { placed, pad } = c.item else { continue };
        let Some(p) = ctx.board.placed_item(placed) else { continue };
        let b = c.shape.bbox();
        let pad_set = c.shape.to_polyset(ARC_ERR);
        let ring = pad_set.offset(t.gap + ARC_ERR, ARC_ERR).difference(&pad_set);
        let centre = p.pad_at(pad).unwrap_or(b.center());
        let len = ((b.width().max(b.height()) as Nm) / 2 + t.gap * 2).max(t.gap * 2);
        let mut spokes = Vec::new();
        for q in 0..4 {
            let a = t.angle.add(p.rot).add(Angle::from_udeg(q * Angle::QUARTER.udeg() as i64));
            let (s, co) = a.sin_cos();
            let mid = centre.add(P::from_f(co * len as f64 / 2.0, s * len as f64 / 2.0));
            spokes.push(Shape::Rect {
                c: mid, half: P::new(len / 2, t.spoke / 2), rot: a, corner: 0,
            }.to_polyset(ARC_ERR));
        }
        masks.push(ring.difference(&PolySet::union_all(spokes.iter())));
    }
    (knock, masks)
}

/// Select indexed obstacles, apply the dual opening locally and preserve exact margin-less edges.
/// Board and footprint keepout areas become masks in candidate order, before thermal relief masks.
fn tile_fill(ctx: &Ctx<'_>, layer: LayerId, key: TileKey) -> FillTile {
    let tile = ctx.prep.lattice.tile_box(key);
    let z = ctx.zone;
    let h = z.settings.min_width / 2;
    // Keep the selection window: dilation reads E within h, whose obstacles grow by gap + h.
    let near = tile.inflate(z.settings.min_width + 4 * ARC_ERR);
    let mut e = ctx.prep.base_e.intersection(&PolySet::rect(tile.inflate(h + 4 * ARC_ERR)));
    if e.is_empty() {
        return FillTile::from_polys(key, Vec::new(), Vec::new(), tile);
    }
    let mut candidates = Vec::new();
    ctx.index.query_fill(layer, near, ctx.prep.select, |piece| {
        candidates.push(piece.key());
        ControlFlow::Continue(())
    });
    let selected = select(ctx, layer, near, candidates.into_iter().filter_map(|key| ctx.index.get(key)));
    // A large board's tile took up to 1 s: the host's pool gets a turn between its phases.
    fab_elec_core::pace::pace();
    let (mut knock, masks) = obstacle_masks(ctx, layer, near, &selected, h, false);
    fab_elec_core::pace::pace();
    if h > 0 {
        knock.extend(masks.iter().map(|mask| {
            fab_elec_core::pace::pace();
            mask.offset(h, ARC_ERR)
        }));
    } else {
        knock.extend(masks.iter().cloned());
    }
    let knocked = PolySet::union_all(knock.iter());
    fab_elec_core::pace::pace();
    e = e.difference(&knocked);
    fab_elec_core::pace::pace();
    let tile_set = PolySet::rect(tile);
    if h == 0 {
        let mut polys = e.intersection(&tile_set).split();
        polys.sort_by_cached_key(poly_key);
        let own = polygon_own(&polys, &selected.own);
        return FillTile::from_polys(key, polys, own, tile);
    }
    let clip = ctx.prep.base.intersection(&tile_set).difference(&PolySet::union_all(masks.iter()));
    fab_elec_core::pace::pace();
    let set = e.offset(h, ARC_ERR).intersection(&clip);
    fab_elec_core::pace::pace();
    let mut polys = set.split();
    polys.sort_by_cached_key(poly_key);
    partition_tile(key, tile, polys, &e, &clip, h, &selected.own)
}

fn poly_key(poly: &PolySet) -> (Nm, Nm, Nm, Nm, ContentHash) {
    let b = poly.bbox();
    let mut w = Writer::hasher();
    write_poly(poly, &mut w);
    (b.min.y, b.min.x, b.max.y, b.max.x, w.finish())
}

fn polygon_own(polys: &[PolySet], own: &[&Piece]) -> Vec<bool> {
    polys.iter().map(|poly| {
        let b = poly.bbox();
        let shape = Shape::Poly(poly.clone());
        own.iter().any(|piece| piece.bbox.intersects(&b) && clearance(&shape, piece.shape()) == 0)
    }).collect()
}

/// One final polygon and its temporary feed/group references, before canonical renumbering.
struct FillPart {
    poly: PolySet,
    feed: usize,
    group: usize,
    /// A polygon reached by several feeds, less their overlaps, for lone side runs only.
    exclusive: Option<PolySet>,
}

/// Preserve single-feed polygons verbatim; partition only polygons reached by several feeds.
fn partition_tile(
    key: TileKey, tile: BBox, polys: Vec<PolySet>, e: &PolySet, clip: &PolySet, h: Nm, own: &[&Piece],
) -> FillTile {
    let mut feeds = e.split();
    feeds.sort_by_cached_key(poly_key);
    let tile_set = PolySet::rect(tile);
    let mut cores = Vec::new();
    let mut has_core = vec![false; feeds.len()];
    for (feed, poly) in feeds.iter().enumerate() {
        let b = poly.bbox();
        if b.min.x > tile.min.x && b.max.x < tile.max.x && b.min.y > tile.min.y && b.max.y < tile.max.y {
            // Strictly inside the tile: the feed is its own core, with no side runs.
            has_core[feed] = true;
            cores.push((poly.clone(), feed));
            continue;
        }
        for core in poly.intersection(&tile_set).split() {
            has_core[feed] = true;
            cores.push((core, feed));
        }
    }
    cores.sort_by_cached_key(|(core, _)| poly_key(core));
    let boxes: Vec<_> = feeds.iter().map(|feed| feed.bbox().inflate(h)).collect();
    let shapes: Vec<_> = feeds.into_iter().map(Shape::Poly).collect();
    // The feeds whose dilation reaches each polygon. Inside the tile that is the polygon holding
    // one of the feed's cores: a straight path from a point of the feed to a point of the copper
    // within h of it stays in the dilation and in the clip, so it joins that core's polygon. Any
    // other polygon a feed reaches only from its part beyond the tile, across a side, so that
    // polygon has a side run within h of the feed. One containment test per core and a feed
    // against the few side runs near it: never a feed against a whole polygon.
    let poly_boxes: Vec<BBox> = polys.iter().map(PolySet::bbox).collect();
    let mut reach: Vec<Vec<usize>> = vec![Vec::new(); polys.len()];
    for (core, feed) in &cores {
        let Some(&point) = core.polygons().first().and_then(|poly| poly.outer.first()) else {
            continue;
        };
        for (ord, poly) in polys.iter().enumerate() {
            if poly_boxes[ord].contains(point) && poly.contains(point) {
                reach[ord].push(*feed);
            }
        }
    }
    for (side, runs) in boundary_cuts(&polys, tile).iter().enumerate() {
        for &(lo, hi, ord) in runs {
            let (a, b) = match side {
                0 => (P::new(tile.min.x, lo), P::new(tile.min.x, hi)),
                1 => (P::new(tile.max.x, lo), P::new(tile.max.x, hi)),
                2 => (P::new(lo, tile.min.y), P::new(hi, tile.min.y)),
                _ => (P::new(lo, tile.max.y), P::new(hi, tile.max.y)),
            };
            let (run, run_box) = (Shape::Segment { a, b, w: 0 }, BBox::new(a, b));
            for (feed, shape) in shapes.iter().enumerate() {
                if boxes[feed].intersects(&run_box) && !reach[ord as usize].contains(&feed)
                    && clearance(shape, &run) <= h {
                    reach[ord as usize].push(feed);
                }
            }
        }
    }
    let mut grown: Vec<Option<PolySet>> = vec![None; shapes.len()];
    let original_count = polys.len();
    let mut parts = Vec::with_capacity(original_count);
    let mut split = false;
    for (group, (poly, mut candidates)) in polys.into_iter().zip(reach).enumerate() {
        // Overlaps go first to feeds with a core in this tile, so a feed whose core lies in a
        // neighbour never takes a part that is all overlap: such a part would have neither a core
        // nor a lone run here, and would keep an island of its own.
        candidates.sort_by_key(|&i| (!has_core[i], i));
        candidates.dedup();
        if candidates.len() <= 1 {
            let feed = match candidates.first() {
                Some(&feed) => feed,
                None => {
                    // Rounding can leave a polygon without a feed: give it a separate foreign node.
                    has_core.push(false);
                    has_core.len() - 1
                }
            };
            parts.push(FillPart { poly, feed, group, exclusive: None });
            continue;
        }
        // A feed may reach several neck polygons; compute its clipped dilation at most once.
        for &feed in &candidates {
            if grown[feed].is_none() {
                grown[feed] = Some(shapes[feed].to_polyset(ARC_ERR).offset(h, ARC_ERR).intersection(clip));
            }
        }
        let mut covered = PolySet::new();
        let mut overlaps = Vec::new();
        let mut assigned = Vec::new();
        for (i, &feed) in candidates.iter().enumerate() {
            let g = grown[feed].as_ref().expect("candidate dilation was computed");
            if i > 0 {
                overlaps.push(covered.intersection(g));
            }
            // The last candidate takes the rest, so the parts cover the polygon exactly.
            let part = if i + 1 == candidates.len() {
                poly.difference(&covered)
            } else {
                poly.intersection(g).difference(&covered)
            };
            assigned.push((feed, part));
            if i + 1 < candidates.len() {
                covered = covered.union(g);
            }
        }
        // Arc chords of the dilations need not meet the fill's own, so the rest can hold a thin
        // piece that the last candidate's dilation misses: a rounding remainder of a feed nearer to
        // it. Such a piece keeps its geometry and goes to the candidate nearest to it, whatever the
        // tile; only the last candidate's part can hold one.
        let (last, rest) = assigned.pop().expect("a neck polygon has candidates");
        let g_last = grown[last].as_ref().expect("candidate dilation was computed");
        let mut remainders = Vec::new();
        let mut kept = Vec::new();
        for piece in rest.split() {
            if piece.intersection(g_last).is_empty() {
                let piece_shape = Shape::Poly(piece.clone());
                let nearest = candidates.iter().copied()
                    .min_by_key(|&feed| (clearance(&shapes[feed], &piece_shape), feed))
                    .expect("a neck polygon has candidates");
                remainders.push((nearest, piece));
            } else {
                kept.push(piece);
            }
        }
        if remainders.is_empty() {
            assigned.push((last, rest));
        } else {
            assigned.extend(kept.into_iter().map(|piece| (last, piece)));
            assigned.extend(remainders);
        }
        assigned.retain(|(_, part)| !part.is_empty());
        // A side run carries a feed's identity only where that feed's own dilation covers it and no
        // other candidate's does: never inside an overlap, even when one feed takes all of it, and
        // never on a rounding remainder outside its feed's dilation.
        let overlap = poly.intersection(&PolySet::union_all(overlaps.iter()));
        let exclusive = |piece: &PolySet, feed: usize| {
            let g = grown[feed].as_ref().expect("candidate dilation was computed");
            piece.intersection(g).difference(&overlap)
        };
        if assigned.len() == 1 {
            let feed = assigned[0].0;
            let exclusive = Some(exclusive(&poly, feed));
            parts.push(FillPart { poly, feed, group, exclusive });
        } else {
            split = true;
            for (feed, part) in assigned {
                for poly in part.split() {
                    let exclusive = Some(exclusive(&poly, feed));
                    parts.push(FillPart { poly, feed, group, exclusive });
                }
            }
        }
    }
    // With no split the original canonical order and polygon bytes are already intact.
    if split {
        parts.sort_by_cached_key(|part| poly_key(&part.poly));
    }
    let mut used = has_core.clone();
    for part in &parts {
        used[part.feed] = true;
    }
    let mut nodes = vec![u32::MAX; used.len()];
    let mut foreign = Vec::new();
    for (feed, used) in used.into_iter().enumerate() {
        if used {
            nodes[feed] = foreign.len() as u32;
            foreign.push(!has_core[feed]);
        }
    }
    let mut core_cuts = Default::default();
    for (core, feed) in cores {
        append_cuts(&mut core_cuts, &core, nodes[feed], tile);
    }
    merge_cuts(&mut core_cuts);
    let mut first = vec![u32::MAX; original_count];
    let mut group = Vec::with_capacity(parts.len());
    let mut node = Vec::with_capacity(parts.len());
    let mut exclusive = Vec::with_capacity(parts.len());
    let mut polys = Vec::with_capacity(parts.len());
    for (ord, part) in parts.into_iter().enumerate() {
        first[part.group] = first[part.group].min(ord as u32);
        group.push(first[part.group]);
        node.push(nodes[part.feed]);
        exclusive.push(part.exclusive);
        polys.push(part.poly);
    }
    let cuts = boundary_cuts(&polys, tile);
    let mut lone: [Vec<(Nm, Nm, u32)>; 4] = Default::default();
    for (side, runs) in cuts.iter().enumerate() {
        lone[side].extend(runs.iter().filter_map(|&(lo, hi, ord)| {
            exclusive[ord as usize].is_none().then_some((lo, hi, node[ord as usize]))
        }));
    }
    for (ord, exclusive) in exclusive.into_iter().enumerate() {
        if let Some(poly) = exclusive {
            append_cuts(&mut lone, &poly, node[ord], tile);
        }
    }
    merge_cuts(&mut lone);
    let own = polygon_own(&polys, own);
    let mut out = FillTile { key, polys, own, cuts, node, group, foreign, core_cuts, lone, hash: Default::default() };
    out.rehash();
    out
}

fn write_poly(poly: &PolySet, w: &mut Writer) {
    w.count(poly.rings().count());
    for ring in poly.rings() {
        ring.canon(w);
    }
}

fn append_cuts(cuts: &mut [Vec<(Nm, Nm, u32)>; 4], poly: &PolySet, node: u32, b: BBox) {
    for ring in poly.rings() {
        for (&a, &c) in ring.iter().zip(ring.iter().cycle().skip(1)) {
            let edges = [
                (a.x == b.min.x && c.x == b.min.x, a.y, c.y),
                (a.x == b.max.x && c.x == b.max.x, a.y, c.y),
                (a.y == b.min.y && c.y == b.min.y, a.x, c.x),
                (a.y == b.max.y && c.y == b.max.y, a.x, c.x),
            ];
            for (side, (on, lo, hi)) in edges.into_iter().enumerate() {
                if on && lo != hi {
                    cuts[side].push((lo.min(hi), lo.max(hi), node));
                }
            }
        }
    }
}

/// Merge runs of the same node, then sort by position for the two-cursor seam pass.
fn merge_cuts(cuts: &mut [Vec<(Nm, Nm, u32)>; 4]) {
    for side in cuts {
        side.sort_unstable_by_key(|&(lo, hi, node)| (node, lo, hi));
        let mut n = 0;
        for i in 0..side.len() {
            let (lo, hi, node) = side[i];
            if n > 0 && side[n - 1].2 == node && lo <= side[n - 1].1 {
                side[n - 1].1 = side[n - 1].1.max(hi);
            } else {
                side[n] = (lo, hi, node);
                n += 1;
            }
        }
        side.truncate(n);
        side.sort_unstable();
    }
}

fn boundary_cuts(polys: &[PolySet], b: BBox) -> [Vec<(Nm, Nm, u32)>; 4] {
    let mut cuts = Default::default();
    for (ord, poly) in polys.iter().enumerate() {
        append_cuts(&mut cuts, poly, u32::try_from(ord).expect("tile polygon ordinal exceeds u32"), b);
    }
    merge_cuts(&mut cuts);
    cuts
}

struct IslandsPass {
    parent: Vec<usize>,
    size: Vec<usize>,
    first: Vec<usize>,
    own: Vec<bool>,
}

impl IslandsPass {
    fn root(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn join(&mut self, a: usize, b: usize) {
        let (mut a, mut b) = (self.root(a), self.root(b));
        if a == b {
            return;
        }
        if self.size[a] < self.size[b] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parent[b] = a;
        self.size[a] = self.size[a] + self.size[b];
        self.first[a] = self.first[a].min(self.first[b]);
        self.own[a] = self.own[a] || self.own[b];
    }

    fn cuts(&mut self, a: &[(Nm, Nm, u32)], b: &[(Nm, Nm, u32)], ai: usize, bi: usize) {
        // Polygons on one side have disjoint interiors, so a two-cursor merge visits every overlap.
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            let (al, ah, ao) = a[i];
            let (bl, bh, bo) = b[j];
            if al.max(bl) < ah.min(bh) {
                self.join(ai + ao as usize, bi + bo as usize);
            }
            if ah <= bh {
                i += 1;
            }
            if bh <= ah {
                j += 1;
            }
        }
    }
}

/// Original copper components, shared by retention and the on-demand census.
fn polygon_islands(layer: &FillLayer) -> (IslandsPass, Vec<usize>, Vec<PolyRef>) {
    let mut offsets = Vec::with_capacity(layer.tiles.len());
    let mut members = Vec::new();
    let mut own = Vec::new();
    let mut by_key = HashMap::with_capacity(layer.tiles.len());
    for (i, tile) in layer.tiles.iter().enumerate() {
        offsets.push(members.len());
        by_key.insert(tile.key, i);
        members.extend((0..tile.polys.len()).map(|ord| PolyRef { tile: tile.key, ord: ord as u32 }));
        own.extend_from_slice(&tile.own);
    }
    let mut pass = IslandsPass {
        parent: (0..members.len()).collect(),
        size: vec![1; members.len()],
        first: (0..members.len()).collect(),
        own,
    };
    for (i, tile) in layer.tiles.iter().enumerate() {
        for (ord, &group) in tile.group.iter().enumerate() {
            pass.join(offsets[i] + ord, offsets[i] + group as usize);
        }
        let neighbors = [
            (TileKey { x: tile.key.x + 1, y: tile.key.y }, 1, 0),
            (TileKey { x: tile.key.x, y: tile.key.y + 1 }, 3, 2),
        ];
        for (key, side, opposite) in neighbors {
            if let Some(&j) = by_key.get(&key) {
                pass.cuts(&tile.cuts[side], &layer.tiles[j].cuts[opposite], offsets[i], offsets[j]);
            }
        }
    }
    (pass, offsets, members)
}

/// Minimum-width components over all tiles' feed nodes, joined only through positive-length core
/// cuts and lone runs, never polygon cuts. Each root's `first` is its smallest polygon ordinal in
/// `poly_offsets` numbering and its `own` whether any of its polygons touches own copper.
/// Returns the pass and each tile's node offset.
fn width_islands(layer: &FillLayer, poly_offsets: &[usize]) -> (IslandsPass, Vec<usize>) {
    let mut offsets = Vec::with_capacity(layer.tiles.len());
    let mut by_key = HashMap::with_capacity(layer.tiles.len());
    let mut count = 0;
    for (i, tile) in layer.tiles.iter().enumerate() {
        offsets.push(count);
        by_key.insert(tile.key, i);
        count += tile.foreign.len();
    }
    let mut width = IslandsPass {
        parent: (0..count).collect(), size: vec![1; count],
        first: vec![usize::MAX; count], own: vec![false; count],
    };
    for (i, tile) in layer.tiles.iter().enumerate() {
        for (ord, &node) in tile.node.iter().enumerate() {
            let n = offsets[i] + node as usize;
            width.first[n] = width.first[n].min(poly_offsets[i] + ord);
            width.own[n] |= tile.own[ord];
        }
    }
    for (i, tile) in layer.tiles.iter().enumerate() {
        let neighbors = [
            (TileKey { x: tile.key.x + 1, y: tile.key.y }, 1, 0),
            (TileKey { x: tile.key.x, y: tile.key.y + 1 }, 3, 2),
        ];
        for (key, side, opposite) in neighbors {
            if let Some(&j) = by_key.get(&key) {
                let other = &layer.tiles[j];
                width.cuts(&tile.core_cuts[side], &other.core_cuts[opposite], offsets[i], offsets[j]);
                width.cuts(&tile.lone[side], &other.lone[opposite], offsets[i], offsets[j]);
            }
        }
    }
    (width, offsets)
}

/// Retention follows the copper islands; connectivity labels follow the minimum-width islands
/// that hold own copper. A minimum-width island without own copper connects nothing, so it
/// stays with its copper island: it takes the label of that island's own-copper part with the
/// smallest representative, or the copper island's own label when no part holds own copper.
/// A copper island with at most one own-copper part therefore keeps exactly today's label, and
/// only a neck between two own-copper parts splits a conductor.
fn label_islands(layer: &mut FillLayer, remove: bool) {
    let (mut copper, poly_offsets, members) = polygon_islands(layer);
    let (mut width, node_offsets) = width_islands(layer, &poly_offsets);
    // Per copper root: the own-copper width root with the smallest representative.
    let mut target: HashMap<usize, usize> = HashMap::new();
    for (i, tile) in layer.tiles.iter().enumerate() {
        for (ord, &node) in tile.node.iter().enumerate() {
            let w = width.root(node_offsets[i] + node as usize);
            if width.own[w] {
                let c = copper.root(poly_offsets[i] + ord);
                let best = target.entry(c).or_insert(w);
                if width.first[w] < width.first[*best] {
                    *best = w;
                }
            }
        }
    }
    // The label of each target: the smallest polygon among it and the parts that join it.
    let mut joined: HashMap<usize, usize> = HashMap::new();
    for (i, tile) in layer.tiles.iter().enumerate() {
        for (ord, &node) in tile.node.iter().enumerate() {
            let w = width.root(node_offsets[i] + node as usize);
            let c = copper.root(poly_offsets[i] + ord);
            if let Some(&t) = target.get(&c) {
                if w == t || !width.own[w] {
                    let first = joined.entry(t).or_insert(width.first[t]);
                    *first = (*first).min(poly_offsets[i] + ord);
                }
            }
        }
    }
    layer.labels = layer.tiles.iter().enumerate().map(|(i, tile)| {
        let mut island = Vec::with_capacity(tile.polys.len());
        let mut kept = Vec::with_capacity(tile.polys.len());
        for (ord, &node) in tile.node.iter().enumerate() {
            let w = width.root(node_offsets[i] + node as usize);
            let c = copper.root(poly_offsets[i] + ord);
            let first = match target.get(&c) {
                Some(&t) if w == t || !width.own[w] => joined[&t],
                Some(_) => width.first[w],
                None => copper.first[c],
            };
            island.push(members[first]);
            kept.push(!remove || copper.own[c]);
        }
        let mut w = Writer::hasher();
        tile.hash.canon(&mut w);
        kept.canon(&mut w);
        TileLabels { island, kept, view_hash: w.finish() }
    }).collect();
}

/// One original fill island containing more than one minimum-width island.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NeckSite {
    /// The original copper island's smallest polygon reference.
    pub island: PolyRef,
    /// Number of distinct minimum-width labels in this copper island.
    pub width_islands: usize,
    /// Number of those labels with own-net copper on at least one polygon.
    pub own: usize,
    /// Union of all tiles containing this copper island's polygons, in integer nm.
    pub bbox: BBox,
}

/// On-demand comparison of copper islands with minimum-width islands, including removed fill.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct NeckCensus {
    /// Components of polygon groups and cuts, regardless of retention.
    pub f_islands: usize,
    /// Distinct minimum-width island labels over all polygons.
    pub width_islands: usize,
    /// Copper islands containing at least two minimum-width labels.
    pub neck_islands: usize,
    /// Neck islands with own copper on polygons in at least two of their minimum-width islands.
    pub splitting: usize,
    /// Feed nodes with no core in their tile.
    pub foreign: usize,
    /// Foreign nodes without a lone side run, which conservatively keep their own island.
    pub loneless: usize,
    /// Neck sites in canonical order of their original copper island representative.
    pub sites: Vec<NeckSite>,
}

/// Count necks without geometry or changing labels. Never called by the fill path.
/// Expected O(T + P + N + K) with amortized union-find operations, plus O(S log S) to order
/// sites, for tiles, polygons, nodes, cut intervals and neck sites respectively.
/// Auxiliary storage is O(T + P + N). All polygons participate, regardless of retention.
pub fn neck_census(lattice: FillLattice, layer: &FillLayer) -> NeckCensus {
    let (mut copper, offsets, members) = polygon_islands(layer);
    let (mut width, node_offsets) = width_islands(layer, &offsets);
    let mut out = NeckCensus::default();
    let mut widths = HashSet::new();
    let mut groups: HashMap<usize, (HashSet<usize>, BBox)> = HashMap::new();
    for (i, tile) in layer.tiles.iter().enumerate() {
        let mut has_lone = vec![false; tile.foreign.len()];
        for &(_, _, node) in tile.lone.iter().flatten() {
            has_lone[node as usize] = true;
        }
        for (&foreign, &lone) in tile.foreign.iter().zip(&has_lone) {
            out.foreign += usize::from(foreign);
            out.loneless += usize::from(foreign && !lone);
        }
        for (ord, &node) in tile.node.iter().enumerate() {
            let w = width.root(node_offsets[i] + node as usize);
            widths.insert(w);
            let root = copper.root(offsets[i] + ord);
            let (islands, bbox) = groups.entry(root).or_default();
            islands.insert(w);
            *bbox = bbox.union(lattice.tile_box(tile.key));
        }
    }
    out.f_islands = groups.len();
    out.width_islands = widths.len();
    for (root, (islands, bbox)) in groups {
        if islands.len() < 2 {
            continue;
        }
        let own = islands.iter().filter(|&&w| width.own[w]).count();
        out.splitting += usize::from(own >= 2);
        out.sites.push(NeckSite {
            island: members[copper.first[root]], width_islands: islands.len(), own, bbox,
        });
    }
    out.sites.sort_unstable_by_key(|site| site.island);
    out.neck_islands = out.sites.len();
    out
}

enum PlannedTile {
    Shared(Arc<FillTile>),
    Compute(Option<Arc<FillTile>>),
}

/// One zone's fill or update split into independent tile computations.
pub struct FillPlan {
    prep: Option<Prep>,
    /// A replacement only when the caller's index has a different generation.
    index: Option<PieceIndex>,
    /// The same layer/key order as the `Compute` slots in `layers`.
    work: Vec<(LayerId, TileKey)>,
    layers: Vec<(LayerId, Vec<PlannedTile>)>,
    /// The previous fill for an unchanged plan; otherwise just the output zone and lattice.
    out: ZoneFill,
    remove: bool,
    regional: bool,
    /// The via rings the zone offers: layer, via, ring disc.
    offers: Vec<(LayerId, ItemId, Shape)>,
}

impl FillPlan {
    /// The plan of a full fill of `zone` (what [`fill`] computes), over `index`.
    /// If `index.generation() != board.generation`, builds and retains a fresh index once.
    /// In that case, [`Self::tile`] ignores its `index` argument.
    pub fn full(board: &Board, rules: &RuleMatrix, zone: ItemId, index: &PieceIndex) -> FillPlan {
        Self::new(board, rules, zone, index, None)
    }

    /// The plan of [`fill_update_indexed`] for `board`, `rules`, `prev`, `delta` and `index`.
    /// If `index.generation() != board.generation`, builds and retains a fresh index once.
    /// In that case, [`Self::tile`] ignores its `index` argument.
    pub fn update(
        board: &Board,
        rules: &RuleMatrix,
        prev: &ZoneFill,
        delta: &BoardDelta,
        index: &PieceIndex,
    ) -> FillPlan {
        Self::new(board, rules, prev.zone, index, Some((prev, delta)))
    }

    fn new(
        board: &Board,
        rules: &RuleMatrix,
        zone: ItemId,
        index: &PieceIndex,
        update: Option<(&ZoneFill, &BoardDelta)>,
    ) -> FillPlan {
        Self::at_lattice(board, rules, zone, index, update, None)
    }

    fn at_lattice(
        board: &Board,
        rules: &RuleMatrix,
        zone: ItemId,
        index: &PieceIndex,
        update: Option<(&ZoneFill, &BoardDelta)>,
        lattice: Option<FillLattice>,
    ) -> FillPlan {
        let z = board.zones.iter().find(|z| z.id == zone);
        if let Some((prev, delta)) = update {
            if z.is_some() && !reaches(board, rules, zone, delta) {
                return Self {
                    prep: None, index: None, work: Vec::new(), layers: Vec::new(), out: prev.clone(),
                    remove: false, regional: false, offers: Vec::new(),
                };
            }
        }
        let lattice = lattice.unwrap_or_else(|| FillLattice::of(board, rules));
        let mut plan = Self {
            prep: None,
            index: None,
            work: Vec::new(),
            layers: Vec::new(),
            out: ZoneFill { zone, lattice, layers: Vec::new() },
            remove: false,
            regional: false,
            offers: Vec::new(),
        };
        let Some(z) = z else { return plan };
        plan.offers = board
            .rings
            .offers(zone)
            .iter()
            .filter_map(|&(via, layer)| match board.locate(via) {
                Some((crate::ItemKind::Via, i)) => Some((layer, via, board.vias[i].shape())),
                _ => None,
            })
            .collect();
        // Only a plan with tiles reads pieces, so only it replaces a stale index.
        plan.index = (index.generation() != board.generation).then(|| PieceIndex::build(board, &[]));
        let regional = update.and_then(|(prev, delta)| {
            (!delta.global && !delta.touches(zone) && lattice == prev.lattice
                && z.layers.intersect(board.copper_set()).iter().eq(prev.layers.iter().map(|layer| layer.layer)))
                .then_some(delta)
        });
        let prep = Prep::new(board, rules, z, lattice);
        let keys = lattice.tiles_in(prep.base.bbox());
        plan.remove = z.settings.islands == Islands::Remove && z.net.is_some();
        plan.regional = regional.is_some();
        for layer in z.layers.intersect(board.copper_set()).iter() {
            let mut previous = update.and_then(|(prev, _)| prev.layer(layer))
                .into_iter().flat_map(|old| &old.tiles).peekable();
            let mut tiles = Vec::with_capacity(keys.len());
            for &key in &keys {
                while previous.peek().is_some_and(|old| old.key < key) {
                    previous.next();
                }
                let old = previous.next_if(|old| old.key == key);
                if let Some(old) = old {
                    if regional.is_some_and(|delta| !delta.meets(layer, lattice.tile_box(key).inflate(prep.reach))) {
                        tiles.push(PlannedTile::Shared(Arc::clone(old)));
                        continue;
                    }
                }
                plan.work.push((layer, key));
                tiles.push(PlannedTile::Compute(old.cloned()));
            }
            plan.layers.push((layer, tiles));
        }
        plan.prep = Some(prep);
        plan
    }

    /// The number of tiles to compute; `finish` takes them in this order (index 0..len).
    pub fn len(&self) -> usize {
        self.work.len()
    }

    /// Whether the plan needs no tile computations.
    pub fn is_empty(&self) -> bool {
        self.work.is_empty()
    }

    /// Compute tile `i` over the board, rules and index the plan was made from. Pure: any order, any thread.
    /// If the original index had a generation mismatch, uses the retained fresh index and ignores `index`.
    pub fn tile(&self, board: &Board, rules: &RuleMatrix, index: &PieceIndex, i: usize) -> Arc<FillTile> {
        let (layer, key) = self.work[i];
        let zone = board.zones.iter().find(|z| z.id == self.out.zone)
            .expect("fill plan's zone is absent from the supplied board");
        let prep = self.prep.as_ref().expect("fill plan's tile has no precomputed inputs");
        let index = self.index.as_ref().unwrap_or(index);
        let ctx = Ctx { board, rules, zone, index, prep };
        Arc::new(tile_fill(&ctx, layer, key))
    }

    /// Assemble the fill from `tiles[i] == self.tile(.., i)`, the reused tiles and the islands pass.
    /// Takes `&self` so the task that stores the last tile can assemble while others still hold the plan.
    /// Panics if `tiles.len() != self.len()`.
    pub fn finish(&self, tiles: Vec<Arc<FillTile>>) -> ZoneFill {
        assert_eq!(tiles.len(), self.len(), "fill plan requires exactly one result per planned tile");
        let mut out = self.out.clone();
        let mut computed = tiles.into_iter();
        for (layer, planned) in &self.layers {
            let tiles = planned.iter().map(|tile| match tile {
                PlannedTile::Shared(old) => Arc::clone(old),
                PlannedTile::Compute(old) => {
                    let tile = computed.next().expect("fill plan is missing a computed tile");
                    match old {
                        Some(old) if self.regional && old.hash == tile.hash => Arc::clone(old),
                        _ => tile,
                    }
                }
            }).collect();
            let mut layer = FillLayer { layer: *layer, tiles, labels: Vec::new(), reached: Vec::new() };
            label_islands(&mut layer, self.remove);
            // Full fills label the computed geometry before sharing equal tiles from the previous fill.
            if !self.regional {
                for (tile, previous) in layer.tiles.iter_mut().zip(planned) {
                    if let PlannedTile::Compute(Some(old)) = previous {
                        if old.hash == tile.hash {
                            *tile = Arc::clone(old);
                        }
                    }
                }
            }
            let offers = self.offers.iter().filter(|(l, ..)| *l == layer.layer).map(|(_, via, disc)| (*via, disc));
            layer.reached = reached_vias(&layer, self.out.lattice, offers);
            out.layers.push(layer);
        }
        out
    }
}

/// The offered via rings (via, disc) a kept polygon of `layer` touches, sorted. Cost: per offer, the
/// tiles under its disc and their kept polygons whose box meets it.
fn reached_vias<'a>(layer: &FillLayer, lattice: FillLattice, offers: impl Iterator<Item = (ItemId, &'a Shape)>) -> Vec<ItemId> {
    let mut out = Vec::new();
    for (via, disc) in offers {
        let b = disc.bbox();
        let touched = lattice.tiles_in(b).into_iter().any(|key| {
            let Ok(i) = layer.tiles.binary_search_by_key(&key, |t| t.key) else { return false };
            let (tile, labels) = (&layer.tiles[i], &layer.labels[i]);
            tile.polys.iter().zip(&labels.kept).any(|(poly, &kept)| {
                kept && poly.bbox().intersects(&b) && clearance(&Shape::Poly(poly.clone()), disc) == 0
            })
        });
        if touched {
            out.push(via);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Fill every tile of each copper layer shared by the zone and board, retaining empty tiles too.
/// A zone absent from the board has no layers. Updates use the same tile keys and islands pass.
/// Builds one piece index for all tiles and layers, then computes a [`FillPlan`] in order.
pub fn fill(board: &Board, rules: &RuleMatrix, zone: ItemId) -> ZoneFill {
    let index = PieceIndex::build(board, &[]);
    let plan = FillPlan::full(board, rules, zone, &index);
    let tiles = (0..plan.len()).map(|i| plan.tile(board, rules, &index, i)).collect();
    plan.finish(tiles)
}

/// Static exclusions for the organic relax's pour paths, per zone copper layer in layer order.
/// Returns thermal rings less spokes, disconnected own-net pads grown by their gap, zone keepouts,
/// grown higher-priority bases, and the outside of a polygon zone within the outline box grown by
/// the zone's gap. Foreign copper, drills and board edges are already guarded by the relaxer.
/// An absent zone returns no layers. Uses the fill's selection and mask construction at radius zero.
/// Cost: O(N log N + L * (N + V log V)), for N indexed pieces and zones, L zone layers and V vertices
/// processed per layer (including outline and higher-priority bases); intended once per relax job.
pub fn static_masks(board: &Board, rules: &RuleMatrix, zone: ItemId) -> Vec<(LayerId, PolySet)> {
    let Some(z) = board.zones.iter().find(|z| z.id == zone) else { return Vec::new() };
    let index = PieceIndex::build(board, &[]);
    let prep = Prep::at_radius(board, rules, z, FillLattice::of(board, rules), 0);
    let ctx = Ctx { board, rules, zone: z, index: &index, prep: &prep };
    let mut out = Vec::new();
    for layer in z.layers.intersect(board.copper_set()).iter() {
        let gap = zone_gap(z, rules.clearance(prep.zkey, prep.zkey, layer));
        let near = board.outline_bbox().inflate(gap);
        let selected = select(&ctx, layer, near, index.iter().filter(|piece| piece.layers.has(layer)));
        let (mut knock, masks) = obstacle_masks(&ctx, layer, near, &selected, 0, true);
        knock.extend(masks);
        if matches!(&z.area, Area::Poly(_)) {
            let frame = PolySet::rect(near);
            knock.push(frame.difference(&clip_area(z, frame.clone())));
        }
        out.push((layer, PolySet::union_all(knock.iter())));
    }
    out
}

/// The box outside which no change can alter zone `z`'s fill.
fn influence(board: &Board, rules: &RuleMatrix, z: &Zone) -> BBox {
    let area = match &z.area {
        Area::Board => board.outline_bbox(),
        Area::Poly(p) => p.bbox(),
    };
    let t = &z.settings.thermal;
    let extra = rules.max_clearance().max(z.settings.clearance.unwrap_or(0)) + t.gap * 2
        + t.spoke + z.settings.min_width + ARC_ERR * 8;
    area.inflate(extra)
}

/// Whether `delta` can change zone `zone`'s fill; false for a zone absent from the board.
/// A changed item's side must share a zone layer and meet its influence box, unless the delta
/// is global or touches the zone itself.
pub fn reaches(board: &Board, rules: &RuleMatrix, zone: ItemId, delta: &BoardDelta) -> bool {
    let Some(z) = board.zones.iter().find(|z| z.id == zone) else { return false };
    let window = influence(board, rules, z);
    delta.global || delta.touches(zone) || delta.extents.iter().any(|e| {
        !e.layers.intersect(z.layers).is_empty() && e.bbox.intersects(&window)
    })
}

/// Update `prev` after `delta`, equal to a full fill and sharing unchanged tiles by content hash.
/// An unchanged zone outside the delta's reach retains the previous fill directly.
/// Otherwise only missing tiles and tiles whose reach-inflated windows meet a changed item's side
/// on their layer are recomputed, unless the zone, lattice, copper layers or global board inputs changed.
/// Island labels are rebuilt across each layer, including tiles whose geometry is shared.
/// `index` must be a `PieceIndex` of `board`; zone pieces may be stale or absent because the fill
/// reads only fixed pieces. If `index.generation() != board.generation`, it is not that board's
/// index and a fresh one is built instead: slower, but preserving correctness.
/// Computes the [`FillPlan::update`] tiles in order, then assembles the result.
pub fn fill_update_indexed(
    board: &Board,
    rules: &RuleMatrix,
    prev: &ZoneFill,
    delta: &BoardDelta,
    index: &PieceIndex,
) -> ZoneFill {
    let plan = FillPlan::update(board, rules, prev, delta, index);
    let tiles = (0..plan.len()).map(|i| plan.tile(board, rules, index, i)).collect();
    plan.finish(tiles)
}

/// Update `prev` after `delta` for callers without a piece index.
/// Builds a fresh index and delegates to [`fill_update_indexed`].
pub fn fill_update(board: &Board, rules: &RuleMatrix, prev: &ZoneFill, delta: &BoardDelta) -> ZoneFill {
    fill_update_indexed(board, rules, prev, delta, &PieceIndex::build(board, &[]))
}

/// The box of every tile whose drawn copper differs between two fills of one zone: its `view_hash`
/// differs, or it has kept polygons and is listed on one side only. This is what a regional reader
/// (DRC, a redraw) revisits. Fills on different lattices differ everywhere. O(tiles), no geometry.
pub fn changed_box(before: &ZoneFill, after: &ZoneFill) -> BBox {
    let boxes = |f: &ZoneFill| {
        f.layers.iter().flat_map(|l| &l.tiles).fold(BBox::EMPTY, |b, t| b.union(f.lattice.tile_box(t.key)))
    };
    if before.lattice != after.lattice {
        return boxes(before).union(boxes(after));
    }
    let lattice = after.lattice;
    let mut ids: Vec<LayerId> = before.layers.iter().chain(&after.layers).map(|l| l.layer).collect();
    ids.sort();
    ids.dedup();
    let mut out = BBox::EMPTY;
    for id in ids {
        // (key, view_hash, has kept copper) per tile, in key order.
        let view = |f: &ZoneFill| -> Vec<(TileKey, ContentHash, bool)> {
            f.layer(id).map_or_else(Vec::new, |l| {
                l.tiles.iter().zip(&l.labels)
                    .map(|(t, lab)| (t.key, lab.view_hash, lab.kept.contains(&true))).collect()
            })
        };
        let (a, b) = (view(before), view(after));
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            let order = match (a.get(i), b.get(j)) {
                (Some(x), Some(y)) => x.0.cmp(&y.0),
                (Some(_), None) => Ordering::Less,
                _ => Ordering::Greater,
            };
            let changed = match order {
                Ordering::Equal => {
                    let (x, y) = (a[i], b[j]);
                    i += 1;
                    j += 1;
                    (x.1 != y.1).then_some(x.0)
                }
                Ordering::Less => {
                    i += 1;
                    a[i - 1].2.then_some(a[i - 1].0)
                }
                Ordering::Greater => {
                    j += 1;
                    b[j - 1].2.then_some(b[j - 1].0)
                }
            };
            if let Some(key) = changed {
                out = out.union(lattice.tile_box(key));
            }
        }
    }
    out
}

/// Every zone's fill, in id order. Builds one piece index for all zones.
/// Computes each zone's [`FillPlan::full`] tiles in order, then assembles its result. Gives the
/// host's pool a turn before each tile ([`fab_elec_core::pace`]): a large board's zone took 1.9 s
/// at a 2-core share, one tile up to 1 s.
pub fn fill_all(board: &Board, rules: &RuleMatrix) -> Vec<ZoneFill> {
    let index = PieceIndex::build(board, &[]);
    let mut zones: Vec<_> = board.zones.iter().collect();
    zones.sort_by_key(|zone| zone.id);
    zones.into_iter().map(|zone| {
        let plan = FillPlan::full(board, rules, zone.id, &index);
        let tiles = (0..plan.len()).map(|i| {
            fab_elec_core::pace::pace();
            plan.tile(board, rules, &index, i)
        }).collect();
        plan.finish(tiles)
    }).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{resolve_rules, Anchor, BoardKeepout, BoardNet, BoardOp, Flash, Hole, Item, Origin, Placed, Track, Via};
    use fab_elec_core::{BoardId, CompId, Contour, LayerSet, Path, Side};
    use fab_elec_parts::{Footprint, Keepout, Pad, PadProp, PadShape, PadStack, PasteRule};

    fn board() -> (Board, ItemId) {
        let mut board = Board::new(BoardId(0), "tile fill");
        board.nets = vec![
            BoardNet { name: "own".into(), class: ClassId(0) },
            BoardNet { name: "foreign".into(), class: ClassId(0) },
        ];
        let id = board.alloc_id();
        board.zones.push(Zone {
            id,
            net: Some(NetId(0)),
            name: "pour".into(),
            layers: LayerSet::one(LayerId::F_CU),
            area: Area::Board,
            priority: 0,
            settings: ZoneSettings::default(),
            locked: false,
            origin: Origin::Authored,
        });
        (board, id)
    }

    fn track(board: &mut Board, net: NetId, path: Path) {
        let id = board.alloc_id();
        board.tracks.push(Track {
            id,
            net,
            layer: LayerId::F_CU,
            w: 400_000,
            widths: None,
            path,
            ends: [Anchor::Free; 2],
            locked: false,
            origin: Origin::Authored,
        });
    }

    fn pad(board: &mut Board, at: P, net: NetId, connect: Option<ZoneConnect>) {
        let id = board.alloc_id();
        let comp = CompId(board.placed.len() as u32);
        board.placed.push(Placed {
            id,
            comp,
            path: String::new(),
            refdes: String::new(),
            footprint: Arc::new(Footprint {
                pads: vec![Pad {
                    no: "1".into(),
                    kind: PadKind::Smd,
                    stack: PadStack::smd(PadShape::Rect { size: P::new(800_000, 800_000) }),
                    at: P::ZERO,
                    rot: Angle::ZERO,
                    drill: None,
                    mask_margin: None,
                    paste: PasteRule::default(),
                    thermal: None,
                    zone_connect: connect,
                    die_len: 0,
                    prop: PadProp::None,
                }],
                ..Footprint::default()
            }),
            pad_nets: vec![Some(net)],
            at,
            rot: Angle::ZERO,
            side: Side::Top,
            locked: false,
            origin: Origin::Authored,
            body_z: None,
            ref_label: None, value_label: None, legend: None,
            value: None,
        });
    }

    fn neck_board(passage: Nm) -> (Board, ItemId, P) {
        walled_board(&[(0, passage)], &[(-500_000, ZoneConnect::Solid), (800_000, ZoneConnect::Thermal)])
    }

    /// A pour cut by vertical walls, each with one passage on the centre row, and pads of the
    /// pour's net on that row: walls and pads at x offsets from the centre.
    pub(crate) fn walled_board(walls: &[(Nm, Nm)], pads: &[(Nm, ZoneConnect)]) -> (Board, ItemId, P) {
        let (mut board, zone) = board();
        let s = FillLattice { level: 10 }.side();
        // The necks cross a row boundary, with both feeds present on both sides.
        let centre = P::new(3 * s / 2, 2 * s);
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(15 * s / 4, 15 * s / 4), 0));
        for &(dx, passage) in walls {
            for (lo, hi) in [(0, centre.y - passage / 2), (centre.y + passage / 2, 4 * s)] {
                let id = board.alloc_id();
                board.keepouts.push(BoardKeepout {
                    id,
                    area: PolySet::rect(BBox::new(P::new(centre.x + dx - 10_000, lo), P::new(centre.x + dx + 10_000, hi))),
                    layers: LayerSet::one(LayerId::F_CU), forbid: Forbid::ZONES,
                    reason: String::new(), origin: Origin::Authored, z: None,
                });
            }
        }
        for &(dx, connect) in pads {
            pad(&mut board, centre.add(P::new(dx, 0)), NetId(0), Some(connect));
        }
        board.reindex();
        (board, zone, centre)
    }

    fn labels_at(layer: &FillLayer, point: P) -> HashSet<PolyRef> {
        layer.tiles.iter().zip(&layer.labels).flat_map(|(tile, labels)| {
            tile.polys.iter().zip(&labels.island).filter_map(move |(poly, &island)| {
                (poly.bbox().contains(point) && poly.contains(point)).then_some(island)
            })
        }).collect()
    }

    /// Padless copper behind a neck joins an own part's island, so it is kept and anchors no
    /// open; between two own parts behind their own necks it bridges nothing, and the open
    /// between them is still reported.
    #[test]
    fn padless_width_islands_join_an_own_part_without_hiding_opens() {
        let finger = walled_board(&[(0, 180_000)], &[(800_000, ZoneConnect::Solid)]);
        let between = walled_board(&[(0, 180_000), (1_100_000, 180_000)],
            &[(-500_000, ZoneConnect::Solid), (1_700_000, ZoneConnect::Solid)]);
        for (name, (mut board, _, centre)) in [("finger", finger), ("between", between)] {
            board.zones[0].settings.min_width = 200_000;
            let rules = resolve_rules(&board, None);
            let pads: Vec<P> = board.placed.iter().map(|placed| placed.at).collect();
            let padless = centre.add(P::new(if name == "finger" { -700_000 } else { 550_000 }, 0));
            for level in [13, 10] {
                let case = format!("{name}, level={level}");
                let filled = fill_at(&board, &rules, FillLattice { level }, None);
                let layer = &filled.layers[0];
                assert!(layer.labels.iter().all(|labels| labels.kept.iter().all(|&kept| kept)), "{case}: the padless copper is kept");
                assert!(layer.set().contains(padless), "{case}: the padless part is filled");
                let census = neck_census(filled.lattice, layer);
                let own = pads.len();
                assert_eq!((census.f_islands, census.width_islands, census.neck_islands), (1, own + 1, 1),
                    "{case}: one copper island, one width island per side of each neck");
                assert_eq!(census.splitting, usize::from(own == 2), "{case}: only own copper on two sides splits");
                assert_eq!(census.sites[0].own, own, "{case}: the site counts the width islands with own copper");
                let padless_label = labels_at(layer, padless);
                let pad_labels: Vec<_> = pads.iter().map(|&at| labels_at(layer, at)).collect();
                assert_eq!(padless_label.len(), 1, "{case}: the padless part has one island label");
                assert!(pad_labels.iter().all(|labels| labels.len() == 1), "{case}: each pad's copper has one island label");
                assert!(pad_labels.contains(&padless_label), "{case}: the padless part takes an own part's label");
                let conn = crate::connect(&board, std::slice::from_ref(&filled));
                if own == 1 {
                    assert!(conn.ratsnest.is_empty() && conn.unrouted.is_empty(), "{case}: a padless finger anchors no open");
                } else {
                    assert_ne!(pad_labels[0], pad_labels[1], "{case}: the two own parts keep separate labels");
                    let cluster = |placed| conn.clusters.iter().position(|cluster| {
                        cluster.items.contains(&ConnItem::Pad { placed, pad: fab_elec_core::PadIx(0) })
                    }).expect("the fixture pad belongs to a cluster");
                    assert_ne!(cluster(board.placed[0].id), cluster(board.placed[1].id), "{case}: the two pads stay apart");
                    assert_eq!(conn.ratsnest.len(), 1, "{case}: the open between the own parts is still reported");
                    assert_eq!(conn.unrouted, vec![NetId(0)], "{case}: the pour's net is unrouted");
                }
            }
        }
    }

    /// Exercise the same full/update plan at a supplied lattice without changing board rules.
    fn fill_at(board: &Board, rules: &RuleMatrix, lattice: FillLattice, update: Option<(&ZoneFill, &BoardDelta)>) -> ZoneFill {
        let index = PieceIndex::build(board, &[]);
        let plan = FillPlan::at_lattice(board, rules, board.zones[0].id, &index, update, Some(lattice));
        let tiles = (0..plan.len()).map(|i| plan.tile(board, rules, &index, i)).collect();
        plan.finish(tiles)
    }

    /// Unpartitioned copper and its eroded set, computed by the original opening calculation.
    fn copper_at(ctx: &Ctx<'_>, key: TileKey) -> (PolySet, PolySet, FillTile) {
        let tile = ctx.prep.lattice.tile_box(key);
        let tile_set = PolySet::rect(tile);
        let h = ctx.zone.settings.min_width / 2;
        let near = tile.inflate(ctx.zone.settings.min_width + 4 * ARC_ERR);
        let selected = query_selection(ctx, LayerId::F_CU, near);
        let (mut knock, masks) = obstacle_masks(ctx, LayerId::F_CU, near, &selected, h, false);
        knock.extend(masks.iter().map(|mask| if h > 0 { mask.offset(h, ARC_ERR) } else { mask.clone() }));
        let e = ctx.prep.base_e.intersection(&PolySet::rect(tile.inflate(h + 4 * ARC_ERR)))
            .difference(&PolySet::union_all(knock.iter()));
        let clip = ctx.prep.base.intersection(&tile_set).difference(&PolySet::union_all(masks.iter()));
        let set = if h > 0 { e.offset(h, ARC_ERR).intersection(&clip) } else { e.intersection(&tile_set) };
        let mut polys = set.split();
        polys.sort_by_cached_key(poly_key);
        let own = polygon_own(&polys, &selected.own);
        (e, clip, FillTile::from_polys(key, polys, own, tile))
    }

    #[test]
    fn neck_labels_split_connectivity_and_preserve_copper() {
        for (passage, min_width) in [(180_000, 200_000), (240_000, 200_000), (180_000, 0)] {
            let (mut board, zone, centre) = neck_board(passage);
            board.zones[0].settings.min_width = min_width;
            let rules = resolve_rules(&board, None);
            let index = PieceIndex::build(&board, &[]);
            let mut counts = Vec::new();
            for level in [13, 10] {
                let lattice = FillLattice { level };
                let filled = fill_at(&board, &rules, lattice, None);
                let layer = &filled.layers[0];
                let prep = Prep::new(&board, &rules, &board.zones[0], lattice);
                let ctx = Ctx { board: &board, rules: &rules, zone: &board.zones[0], index: &index, prep: &prep };
                let mut original = FillLayer {
                    layer: LayerId::F_CU,
                    tiles: layer.tiles.iter().map(|tile| Arc::new(copper_at(&ctx, tile.key).2)).collect(),
                    labels: Vec::new(), reached: Vec::new(),
                };
                label_islands(&mut original, true);
                let case = format!("passage={passage}, min_width={min_width}, level={level}");
                assert_eq!(layer.tiles.len(), if level == 13 { 1 } else { 16 }, "{case}: inside one tile versus across a side");
                assert_eq!(layer.set(), original.set(), "{case}: partitioning and retention preserve the copper exactly");
                assert!(layer.set().contains(centre), "{case}: copper still conducts through the passage");
                for (tile, old) in layer.tiles.iter().zip(&original.tiles) {
                    for (ord, poly) in tile.polys.iter().enumerate() {
                        if tile.group.iter().filter(|&&group| group == tile.group[ord]).count() == 1 {
                            assert!(old.polys.contains(poly), "{case}: every unsplit polygon keeps its bytes");
                        }
                    }
                    if min_width == 0 {
                        let ordinals: Vec<_> = (0..tile.polys.len() as u32).collect();
                        assert_eq!(tile.node, ordinals, "{case}: zero-radius nodes are polygon ordinals");
                        assert_eq!(tile.group, ordinals, "{case}: zero-radius groups are polygon ordinals");
                        assert!(tile.foreign.iter().all(|&v| !v), "{case}: zero-radius nodes have cores");
                        assert_eq!(tile.core_cuts, tile.cuts, "{case}: zero-radius core cuts equal polygon cuts");
                        assert_eq!(tile.lone, tile.cuts, "{case}: zero-radius lone runs equal polygon cuts");
                    }
                }
                let census = neck_census(lattice, layer);
                let narrow = min_width > passage;
                assert_eq!(census.f_islands, 1, "{case}: retention sees one copper island");
                assert_eq!(census.width_islands, if narrow { 2 } else { 1 }, "{case}: only the narrow passage splits width islands");
                assert_eq!(census.neck_islands, usize::from(narrow), "{case}: the narrow passage counts as a neck");
                assert_eq!(census.splitting, usize::from(narrow), "{case}: both solid and thermal contacts count toward splitting");
                assert_eq!(census.loneless, 0, "{case}: every foreign node can reach a lone side run");
                let pocket = labels_at(layer, board.placed[0].at);
                let main = labels_at(layer, board.placed[1].at);
                assert_eq!(pocket.len(), 1, "{case}: the pocket pad's copper has one island label");
                assert_eq!(main.len(), 1, "{case}: the main pour pad's copper has one island label");
                assert_eq!(pocket != main, narrow, "{case}: pad polygon labels separate only across the neck");
                let conn = crate::connect(&board, std::slice::from_ref(&filled));
                let cluster = |placed| conn.clusters.iter().position(|cluster| {
                    cluster.items.contains(&ConnItem::Pad { placed, pad: fab_elec_core::PadIx(0) })
                }).expect("the fixture pad belongs to a cluster");
                assert_eq!(cluster(board.placed[0].id) != cluster(board.placed[1].id), narrow, "{case}: the pocket pad is open only across the neck");
                assert_eq!(conn.ratsnest.len(), usize::from(narrow), "{case}: the ratsnest exposes exactly the missing pad connection");
                if narrow {
                    let site = &census.sites[0];
                    assert_eq!(site.width_islands, 2, "{case}: the site counts stitched width islands");
                    assert_eq!(site.own, 2, "{case}: repeated contacts count once per width island");
                    let bbox = layer.tiles.iter().fold(BBox::EMPTY, |b, tile| b.union(lattice.tile_box(tile.key)));
                    assert_eq!(site.bbox, bbox, "{case}: site bounds cover whole involved tiles");
                    assert_eq!(site.island, PolyRef { tile: layer.tiles[0].key, ord: 0 }, "{case}: the site names the smallest copper polygon");
                    if level == 13 {
                        let mut single_contact = layer.clone();
                        let tile = Arc::make_mut(&mut single_contact.tiles[0]);
                        tile.own.fill(false);
                        tile.own[0] = true;
                        tile.rehash();
                        label_islands(&mut single_contact, true);
                        assert!(single_contact.labels[0].kept.iter().all(|&kept| kept),
                            "{case}: the original polygon group retains parts with no own contact");
                        let labels: HashSet<_> = single_contact.labels[0].island.iter().copied().collect();
                        assert_eq!(labels, HashSet::from([PolyRef { tile: layer.tiles[0].key, ord: 0 }]),
                            "{case}: a width island without own copper stays with its copper island's own part");
                        assert_eq!(neck_census(lattice, &single_contact).splitting, 0,
                            "{case}: own copper on one side only does not split");
                    }
                } else if min_width > 0 {
                    assert!(passage > min_width + 2 * ARC_ERR, "{case}: the wide control clears the arc-error margin");
                }
                let old_fill = ZoneFill { zone, lattice, layers: vec![original] };
                assert!(crate::connect(&board, &[old_fill]).ratsnest.is_empty(), "{case}: the original copper labels hide the open");
                counts.push((census.f_islands, census.width_islands, census.neck_islands, census.splitting));
            }
            assert_eq!(counts[0], counts[1], "passage={passage}: island counts are independent of tile size");
        }
    }

    #[test]
    fn node_seams_require_positive_core_or_lone_runs() {
        let lattice = FillLattice { level: 10 };
        let s = lattice.side();
        let keys = [TileKey { x: 0, y: 0 }, TileKey { x: 1, y: 0 }, TileKey { x: 3, y: 0 }];
        let mut tiles = Vec::new();
        for (i, &key) in keys[..2].iter().enumerate() {
            let b = lattice.tile_box(key);
            let mut tile = FillTile::from_polys(key, vec![PolySet::rect(b)], vec![i == 0], b);
            tile.core_cuts = Default::default();
            tile.lone = Default::default();
            tile.core_cuts[1 - i].push((s / 4 + i as Nm * s / 4, s / 2 + i as Nm * s / 4, 0));
            tile.rehash();
            tiles.push(Arc::new(tile));
        }
        let b = lattice.tile_box(keys[2]);
        let poly = PolySet::rect(b.inflate(-s / 4));
        let foreign = partition_tile(keys[2], b, vec![poly.clone()], &PolySet::new(), &poly, 100_000, &[]);
        assert_eq!(foreign.polys, vec![poly], "no-candidate rounding preserves the polygon");
        assert_eq!(foreign.foreign, vec![true], "no-candidate rounding gets its own foreign node");
        tiles.push(Arc::new(foreign));
        let mut layer = FillLayer { layer: LayerId::F_CU, tiles, labels: Vec::new(), reached: Vec::new() };
        label_islands(&mut layer, true);
        let census = neck_census(lattice, &layer);
        assert_eq!(census.f_islands, 2, "the copper census includes removed foreign polygons");
        assert_eq!(census.width_islands, 3, "point-only core cuts do not join nodes");
        assert_eq!(census.neck_islands, 1, "polygon cuts never join width islands");
        assert_eq!(census.splitting, 0, "own copper on only one width island is not splitting");
        assert_eq!(census.foreign, 1, "the census counts foreign nodes");
        assert_eq!(census.loneless, 1, "a foreign node without lone runs remains isolated");
        assert_eq!(layer.labels[1].kept, vec![true], "retention still joins across polygon cuts");
        assert_eq!(layer.labels[2].kept, vec![false], "retention still removes the unconnected copper");
        assert_eq!(census.sites[0].bbox, lattice.tile_box(keys[0]).union(lattice.tile_box(keys[1])), "site bounds exclude unrelated copper islands");
        let right = Arc::make_mut(&mut layer.tiles[1]);
        right.core_cuts[0][0].0 -= 1;
        right.rehash();
        label_islands(&mut layer, true);
        assert_eq!(neck_census(lattice, &layer).neck_islands, 0, "one nm of core overlap stitches nodes");
        let right = Arc::make_mut(&mut layer.tiles[1]);
        right.foreign[0] = true;
        right.lone = right.core_cuts.clone();
        right.core_cuts = Default::default();
        right.rehash();
        let left = Arc::make_mut(&mut layer.tiles[0]);
        left.lone = left.core_cuts.clone();
        left.rehash();
        label_islands(&mut layer, true);
        let census = neck_census(lattice, &layer);
        assert_eq!(census.width_islands, 2, "lone runs attach a foreign node without a core");
        assert_eq!(census.foreign, 2, "both foreign nodes are counted");
        assert_eq!(census.loneless, 1, "only the foreign node without a lone run is loneless");
    }

    #[test]
    fn overlaps_go_to_feeds_with_a_core_across_a_seam() {
        let lattice = FillLattice { level: 10 };
        let s = lattice.side();
        let h = 100_000;
        let e = PolySet::rect(BBox::new(P::new(s / 4, s / 4), P::new(s - 60_000, 3 * s / 4)))
            .union(&PolySet::rect(BBox::new(P::new(s + 60_000, s / 4), P::new(7 * s / 4, 3 * s / 4))));
        let fill = |lattice: FillLattice, keys: &[TileKey]| {
            let tiles = keys.iter().map(|&key| {
                let b = lattice.tile_box(key);
                let e = e.intersection(&PolySet::rect(b.inflate(h + 4 * ARC_ERR)));
                let clip = PolySet::rect(b);
                let mut polys = e.offset(h, ARC_ERR).intersection(&clip).split();
                polys.sort_by_cached_key(poly_key);
                Arc::new(partition_tile(key, b, polys, &e, &clip, h, &[]))
            }).collect();
            let mut layer = FillLayer { layer: LayerId::F_CU, tiles, labels: Vec::new(), reached: Vec::new() };
            label_islands(&mut layer, false);
            layer
        };
        let whole = fill(FillLattice { level: 12 }, &[TileKey { x: 0, y: 0 }]);
        let tiled = fill(lattice, &[TileKey { x: 0, y: 0 }, TileKey { x: 1, y: 0 }]);
        assert_eq!(whole.set(), tiled.set(), "a seam through an overlap preserves all copper");
        let census = neck_census(lattice, &tiled);
        assert_eq!(census.f_islands, 1, "polygon groups and cuts retain the connected copper");
        assert_eq!(census.foreign, 0, "each overlap goes to the feed with a core in its tile");
        assert_eq!(census.loneless, 0, "no part is left without a core or a lone run");
        assert_eq!(census.neck_islands, 1, "the seam through the overlap still separates the two feeds");
        let whole_census = neck_census(FillLattice { level: 12 }, &whole);
        assert_eq!((census.width_islands, whole_census.width_islands), (2, 2),
            "the seam through an overlap changes no island count");
    }

    /// A strict interior nm point, chosen between vertex rows and edge crossings.
    fn interior_point(poly: &PolySet) -> P {
        let mut ys: Vec<_> = poly.rings().flatten().map(|p| p.y).collect();
        ys.sort_unstable();
        ys.dedup();
        for pair in ys.windows(2) {
            let y = ((i64::from(pair[0]) + i64::from(pair[1])) / 2) as Nm;
            if y == pair[0] || y == pair[1] { continue; }
            let mut xs = Vec::new();
            for ring in poly.rings() {
                for (&a, &b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                    if (a.y > y) != (b.y > y) {
                        xs.push(a.x as f64 + (y - a.y) as f64 * (b.x - a.x) as f64 / (b.y - a.y) as f64);
                    }
                }
            }
            xs.sort_by(f64::total_cmp);
            for pair in xs.windows(2) {
                let p = P::new(((pair[0] + pair[1]) / 2.0).round() as Nm, y);
                let on_edge = poly.rings().any(|ring| ring.iter().zip(ring.iter().cycle().skip(1)).any(|(&a, &b)| {
                    fab_elec_core::geom::orient(a, b, p) == 0 && BBox::new(a, b).contains(p)
                }));
                if poly.contains(p) && !on_edge { return p; }
            }
        }
        panic!("the fixture polygon must contain a strict interior nm point");
    }

    fn reference_overlap(board: &Board, rules: &RuleMatrix, reference: &ZoneFill) -> PolySet {
        let lattice = reference.lattice;
        let index = PieceIndex::build(board, &[]);
        let prep = Prep::new(board, rules, &board.zones[0], lattice);
        let ctx = Ctx { board, rules, zone: &board.zones[0], index: &index, prep: &prep };
        let layer = &reference.layers[0];
        assert_eq!(layer.tiles.len(), 1, "the reference fits in one tile");
        let (e, clip, _) = copper_at(&ctx, layer.tiles[0].key);
        let h = board.zones[0].settings.min_width / 2;
        let grown: Vec<_> = e.split().iter().map(|feed| feed.offset(h, ARC_ERR).intersection(&clip)).collect();
        let mut pairs = Vec::new();
        for (i, a) in grown.iter().enumerate() {
            for b in &grown[i + 1..] { pairs.push(a.intersection(b)); }
        }
        PolySet::union_all(pairs.iter())
    }

    /// Each polygon's raw minimum-width island (its smallest member), before parts without own
    /// copper join their copper island's own part: the partition that must not depend on tiles.
    fn width_labels(layer: &FillLayer) -> Vec<Vec<PolyRef>> {
        let (_, offsets, members) = polygon_islands(layer);
        let (mut width, node_offsets) = width_islands(layer, &offsets);
        layer.tiles.iter().enumerate().map(|(i, tile)| {
            tile.node.iter().map(|&node| {
                let root = width.root(node_offsets[i] + node as usize);
                members[width.first[root]]
            }).collect()
        }).collect()
    }

    fn compare_reference(tiled: &ZoneFill, reference: &ZoneFill, overlap: &PolySet, no_neck: bool, at: &str) {
        let layer = &reference.layers[0];
        let mut forward = HashMap::new();
        let mut backward = HashMap::new();
        let tiled_layer = &tiled.layers[0];
        let (mut copper, offsets, members) = polygon_islands(tiled_layer);
        let (tiled_widths, reference_widths) = (width_labels(tiled_layer), width_labels(layer));
        for (i, (tile, labels)) in tiled_layer.tiles.iter().zip(&tiled_layer.labels).enumerate() {
            for (ord, poly) in tile.polys.iter().enumerate() {
                let point = interior_point(&poly.difference(overlap));
                assert!(poly.contains(point) && !overlap.contains(point), "the reference probe lies outside every feed overlap");
                let matches: Vec<_> = layer.tiles[0].polys.iter().enumerate()
                    .filter(|(_, p)| p.bbox().contains(point) && p.contains(point)).map(|(ord, _)| ord).collect();
                assert_eq!(matches.len(), 1, "each tiled probe has exactly one reference polygon");
                let a = tiled_widths[i][ord];
                let b = reference_widths[0][matches[0]];
                assert_eq!(*forward.entry(a).or_insert(b), b, "{at}: tiled polygon {:?} #{ord} at {point:?}: one tiled island maps to exactly one reference island", tile.key);
                assert_eq!(*backward.entry(b).or_insert(a), a, "{at}: tiled polygon {:?} #{ord} at {point:?}: one reference island maps to exactly one tiled island", tile.key);
                if no_neck {
                    let root = copper.root(offsets[i] + ord);
                    assert_eq!(a, members[copper.first[root]], "without a neck the width islands are the copper islands");
                    assert_eq!(labels.island[ord], a, "without a neck the labels equal the original copper labels");
                }
            }
        }
        assert_eq!(forward.len(), neck_census(reference.lattice, layer).width_islands, "the probes cover every reference island");
        if no_neck {
            assert_eq!(neck_census(tiled.lattice, tiled_layer).neck_islands, 0, "the narrow-radius control has no neck");
        }
    }

    #[test]
    fn tiled_labels_match_reference_before_and_after_track_updates() {
        let mut rng = 7_u64;
        let mut jitter = || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((rng >> 32) % 100_001) as Nm - 50_000
        };
        for case in 0..3 {
            let (mut base, zone, centre) = neck_board(180_000);
            pad(&mut base, P::new(2_900_000 + jitter(), 2_900_000 + jitter()), NetId(0), Some(ZoneConnect::Solid));
            track(&mut base, NetId(1), Path::new(P::new(2_700_000 + jitter(), 1_050_000 + jitter()))
                .line(P::new(2_850_000 + jitter(), 1_100_000 + jitter())));
            track(&mut base, NetId(1), Path::new(centre.add(P::new(0, 600_000)))
                .line(centre.add(P::new(0, 700_000))));
            let moving = base.tracks[1].id;
            base.tracks[1].w = 40_000;
            base.reindex();
            for min_width in [200_000, 50_000] {
                let mut board = base.clone();
                board.zones[0].settings.min_width = min_width;
                let rules = resolve_rules(&board, None);
                let levels = [10, 11];
                let before: Vec<_> = levels.iter().map(|&level| fill_at(&board, &rules, FillLattice { level }, None)).collect();
                let reference = fill_at(&board, &rules, FillLattice { level: 13 }, None);
                let overlap = reference_overlap(&board, &rules, &reference);
                for tiled in &before {
                    compare_reference(tiled, &reference, &overlap, min_width == 50_000,
                        &format!("case {case}, min_width {min_width}, level {}, before", tiled.lattice.level));
                }
                let original = fill(&board, &rules, zone);
                let delta = board.apply(&BoardOp::Move { item: moving, by: P::new(0, -650_000) })
                    .expect("the foreign track moves across the neck").delta;
                let updated = fill_update(&board, &rules, &original, &delta);
                assert_eq!(updated, fill(&board, &rules, zone), "case {case}: public update equals a full fill");
                let reference = fill_at(&board, &rules, FillLattice { level: 13 }, None);
                let overlap = reference_overlap(&board, &rules, &reference);
                compare_reference(&updated, &reference, &overlap, min_width == 50_000,
                    &format!("case {case}, min_width {min_width}, default lattice, after"));
                for old in &before {
                    let next = fill_at(&board, &rules, old.lattice, Some((old, &delta)));
                    assert_eq!(next, fill_at(&board, &rules, old.lattice, None), "case {case}: small-lattice update equals full fill");
                    compare_reference(&next, &reference, &overlap, min_width == 50_000,
                        &format!("case {case}, min_width {min_width}, level {}, after", old.lattice.level));
                }
            }
        }
    }

    /// Place a padless footprint with separate front-layer zone and track keepouts at local centres.
    fn keepout_part(board: &mut Board, at: P, zones: P, tracks: P) -> ItemId {
        let id = board.alloc_id();
        let comp = CompId(board.placed.len() as u32);
        board.placed.push(Placed {
            id,
            comp,
            path: String::new(),
            refdes: String::new(),
            footprint: Arc::new(Footprint {
                keepouts: [(zones, Forbid::ZONES), (tracks, Forbid::TRACKS)].into_iter()
                    .map(|(centre, forbid)| Keepout {
                        area: PolySet::rect(BBox::point(centre).inflate(250_000)),
                        layers: LayerSet::one(LayerId::F_CU),
                        forbid,
                        reason: String::new(),
                    }).collect(),
                ..Footprint::default()
            }),
            pad_nets: Vec::new(),
            at,
            rot: Angle::ZERO,
            side: Side::Top,
            locked: false,
            origin: Origin::Authored,
            body_z: None,
            ref_label: None, value_label: None, legend: None,
            value: None,
        });
        id
    }

    /// Classify a fill query without producing tile geometry; callback references stay in the index.
    fn query_selection<'p>(ctx: &Ctx<'p>, layer: LayerId, near: BBox) -> Selected<'p> {
        let mut keys = Vec::new();
        ctx.index.query_fill(layer, near, ctx.prep.select, |piece| {
            keys.push(piece.key());
            ControlFlow::Continue(())
        });
        select(ctx, layer, near, keys.into_iter().map(|key| ctx.index.get(key).expect("queried piece")))
    }

    /// The indexed window includes every classified obstacle, including keepouts, contacts and distant reliefs.
    #[test]
    fn fill_selection_query_is_complete() {
        let (mut board, _) = board();
        board.zones[0].layers = board.copper_set();
        let rules = resolve_rules(&board, None);
        // A zone clearance above every other term makes `fill_select` exactly the foreign copper test's reach.
        let t = &board.zones[0].settings.thermal;
        let gap = rules.max_clearance().max(rules.hole_clearance()).max(2 * t.gap + t.spoke) + 100_000;
        board.zones[0].settings.clearance = Some(gap);
        assert_eq!(fill_select(&rules, &board.zones[0].settings), gap + ARC_ERR, "the zone clearance binds the bound");
        let lattice = FillLattice::of(&board, &rules);
        let s = lattice.side();
        let margin = board.zones[0].settings.min_width + 4 * ARC_ERR;
        let distant = TileKey { x: 3, y: 0 };
        let distant_near = lattice.tile_box(distant).inflate(margin);
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(17 * s / 4, 7 * s / 4), 0));
        pad(&mut board, P::new(s / 2, s / 2), NetId(0), Some(ZoneConnect::Thermal));
        let big = board.placed[0].id;
        let pad_box = board.placed[0].pad_shape(fab_elec_core::PadIx(0), LayerId::F_CU, board.copper_count())
            .expect("large thermal pad copper").bbox();
        let relief = distant_near.min.x - pad_box.max.x;
        let mut thermal = board.zones[0].settings.thermal.clone();
        thermal.gap = (relief - thermal.spoke) / 2;
        Arc::make_mut(&mut board.placed[0].footprint).pads[0].thermal = Some(thermal.clone());
        assert!(relief > s, "the pad's own relief exceeds a tile side");
        assert!(
            relief > fill_select(&rules, &board.zones[0].settings),
            "the pad's own relief exceeds the ordinary query inflation",
        );
        assert_eq!(
            crate::geom::pad_relief(&board.placed[0].footprint.pads[0]), Some(relief),
            "the own thermal reaches the distant near window exactly",
        );
        pad(&mut board, P::new(s / 2, 3 * s / 4), NetId(1), Some(ZoneConnect::Thermal));
        let foreign_pad = board.placed[1].id;
        Arc::make_mut(&mut board.placed[1].footprint).pads[0].thermal = Some(thermal);
        pad(&mut board, P::new(3 * s / 2, s / 2), NetId(0), Some(ZoneConnect::Off));
        let off = board.placed[2].id;
        let hole = board.alloc_id();
        board.holes.push(Hole {
            id: hole, name: String::new(), at: P::new(s / 2, s / 3), drill: 500_000,
            pad: None, net: Some(NetId(0)), plated: false, origin: Origin::Authored,
        });
        let contact_tile = TileKey { x: 1, y: 0 };
        let contact_near = lattice.tile_box(contact_tile).inflate(margin);
        let x = contact_near.min.x - gap - ARC_ERR - 200_000;
        track(&mut board, NetId(1), Path::new(P::new(x, s / 4)).line(P::new(x, 3 * s / 4)));
        let contact_track = board.tracks[0].id;
        track(&mut board, NetId(1), Path::new(P::new(x - 1, s / 4)).line(P::new(x - 1, 3 * s / 4)));
        let past_track = board.tracks[1].id;
        let keepout = keepout_part(
            &mut board, P::new(contact_near.min.x - 250_000, s / 2), P::ZERO, P::new(0, s / 4),
        );
        board.reindex();

        for short in [false, true] {
            if short {
                board.apply(&BoardOp::Move { item: big, by: P::new(-1, 0) }).expect("move relief 1 nm short");
            }
            let index = PieceIndex::build(&board, &[]);
            let prep = Prep::new(&board, &rules, &board.zones[0], lattice);
            let ctx = Ctx { board: &board, rules: &rules, zone: &board.zones[0], index: &index, prep: &prep };
            let mut distant_live = false;
            let mut hole_live = false;
            let mut off_live = false;
            let mut foreign_live = false;
            let mut keepout_live = false;
            for layer in ctx.zone.layers.iter() {
                for key in lattice.tiles_in(board.outline_bbox()) {
                    let near = lattice.tile_box(key).inflate(margin);
                    let actual = query_selection(&ctx, layer, near);
                    let expected = select(&ctx, layer, near, index.iter().filter(|piece| piece.layers.has(layer)));
                    assert_eq!(
                        actual.pieces.iter().map(|(piece, _)| piece.key()).collect::<Vec<_>>(),
                        expected.pieces.iter().map(|(piece, _)| piece.key()).collect::<Vec<_>>(),
                        "short={short}: obstacle keys match exhaustive selection at {key:?} on {layer:?}",
                    );
                    assert_eq!(
                        actual.own.iter().map(|piece| piece.key()).collect::<Vec<_>>(),
                        expected.own.iter().map(|piece| piece.key()).collect::<Vec<_>>(),
                        "short={short}: own copper keys match exhaustive selection at {key:?} on {layer:?}",
                    );
                    for (piece, kind) in &actual.pieces {
                        if piece.item == Some(big) && key == distant && layer == LayerId::F_CU {
                            distant_live = matches!(kind, SelectedKind::Thermal(_));
                        }
                        hole_live |= piece.item == Some(hole) && matches!(kind, SelectedKind::Drill);
                        off_live |= piece.item == Some(off) && matches!(kind, SelectedKind::Off(_));
                        if piece.item == Some(keepout) {
                            assert!(matches!(kind, SelectedKind::Keepout), "footprint keepouts select as masks");
                            assert!(
                                matches!(&piece.kind, PieceKind::Keepout { forbid, .. } if forbid.has(Forbid::ZONES)),
                                "track-only footprint keepouts never enter the selected obstacles",
                            );
                            assert_eq!(layer, LayerId::F_CU, "front footprint keepouts select only on front copper");
                            keepout_live = true;
                        }
                        if piece.item == Some(foreign_pad) {
                            assert!(
                                matches!(kind, SelectedKind::Foreign(_)),
                                "a foreign pad with its own thermal is only a copper knockout",
                            );
                            foreign_live = true;
                        }
                    }
                }
            }
            assert_eq!(distant_live, !short, "closed relief contact three tiles away accepts exactly at contact");
            assert!(hole_live, "an own-net unplated hole is a live drill knockout");
            assert!(off_live, "an own-net Off pad is a live copper knockout");
            assert!(foreign_live, "a foreign pad with its own thermal is selected near its copper");
            assert!(keepout_live, "the exhaustive oracle covers a live footprint keepout mask");
            let piece = index.pieces(big).find(|piece| matches!(&piece.kind, PieceKind::Copper(_)))
                .expect("large relief piece");
            assert_eq!(
                piece.bbox.inflate(relief).max.x, distant_near.min.x - Nm::from(short),
                "the relief box is exactly at contact or 1 nm short",
            );
            assert_eq!(
                distant.x - piece.bbox.center().x.div_euclid(s), 3,
                "the contact tile is three columns from the pad's own tile",
            );
            let touching = index.pieces(contact_track).next().expect("touching track piece");
            let past = index.pieces(past_track).next().expect("past-contact track piece");
            assert_eq!(touching.bbox.max.x + gap + ARC_ERR, contact_near.min.x, "track box has exact contact");
            assert_eq!(past.bbox.max.x + gap + ARC_ERR, contact_near.min.x - 1, "other track is 1 nm past");
            let selected = query_selection(&ctx, LayerId::F_CU, contact_near);
            assert!(
                selected.pieces.iter().any(|(piece, _)| piece.item == Some(keepout)),
                "a footprint keepout is accepted at uninflated closed contact",
            );
            assert!(
                selected.pieces.iter().any(|(piece, _)| piece.item == Some(contact_track)),
                "foreign copper at gap plus ARC_ERR is accepted at closed contact",
            );
            assert!(
                selected.pieces.iter().all(|(piece, _)| piece.item != Some(past_track)),
                "foreign copper 1 nm beyond gap plus ARC_ERR is rejected",
            );
            let past_near = BBox::new(contact_near.min.add(P::new(1, 0)), contact_near.max);
            let selected = query_selection(&ctx, LayerId::F_CU, past_near);
            assert!(
                selected.pieces.iter().all(|(piece, _)| piece.item != Some(keepout)),
                "a footprint keepout 1 nm outside the near window has no selection margin",
            );
            let selected = query_selection(&ctx, LayerId::F_CU, distant_near);
            assert!(
                selected.pieces.iter().all(|(piece, _)| piece.item != Some(foreign_pad)),
                "a foreign pad's relief entry does not extend its copper knockout",
            );
        }
    }

    /// Unaffected windows retain the complete selected sequence, and each edited fill equals a fresh fill.
    #[test]
    fn fill_selection_is_stable_outside_the_delta() {
        let (mut board, zone) = board();
        board.zones[0].layers = board.copper_set();
        board.zones[0].settings.islands = Islands::Keep;
        let rules = resolve_rules(&board, None);
        let lattice = FillLattice::of(&board, &rules);
        let s = lattice.side();
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(29 * s / 4, 3 * s / 4), 0));
        pad(&mut board, P::new(s / 2, s / 2), NetId(0), Some(ZoneConnect::Thermal));
        Arc::make_mut(&mut board.placed[0].footprint).pads[0].thermal = Some(ThermalRule {
            gap: 3 * s / 2, ..board.zones[0].settings.thermal.clone()
        });
        let big = board.placed[0].id;
        pad(&mut board, P::new(7 * s / 4, s / 2), NetId(0), Some(ZoneConnect::Off));
        let off = board.placed[1].id;
        pad(&mut board, P::new(13 * s / 2, s / 2), NetId(0), None);
        track(&mut board, NetId(1), Path::new(P::new(5 * s / 4, s / 3)).line(P::new(5 * s / 4, 2 * s / 3)));
        let moving_track = board.tracks[0].id;
        track(&mut board, NetId(1), Path::new(P::new(25 * s / 4, 2 * s / 3))
            .line(P::new(27 * s / 4, 2 * s / 3)));
        board.reindex();
        let mut moved = board.placed[0].clone();
        moved.at = moved.at.add(P::new(s / 4, 0));
        let mut smaller = moved.clone();
        Arc::make_mut(&mut smaller.footprint).pads[0].thermal = Some(board.zones[0].settings.thermal.clone());
        let mut foreign = moved.clone();
        foreign.pad_nets[0] = Some(NetId(1));
        let edits = [
            ("move large relief", vec![BoardOp::Move { item: big, by: P::new(s / 4, 0) }]),
            ("remove large relief", vec![BoardOp::Remove(big)]),
            ("add large relief back", vec![BoardOp::Add(Item::Placed(moved.clone()))]),
            ("replace by smaller relief", vec![BoardOp::Replace(Item::Placed(smaller))]),
            ("restore original footprint", vec![BoardOp::Replace(Item::Placed(moved.clone()))]),
            ("own pad becomes foreign", vec![BoardOp::Replace(Item::Placed(foreign))]),
            ("foreign pad becomes own", vec![BoardOp::Replace(Item::Placed(moved))]),
            ("overlapping edit windows", vec![
                BoardOp::Move { item: moving_track, by: P::new(s / 16, 0) },
                BoardOp::Move { item: off, by: P::new(s / 16, 0) },
            ]),
            ("folded move there and back", vec![
                BoardOp::Move { item: big, by: P::new(s / 2, 0) },
                BoardOp::Move { item: big, by: P::new(-s / 2, 0) },
            ]),
        ];
        let mut prev = fill(&board, &rules, zone);
        for (edit, ops) in edits {
            let before = board.clone();
            let before_index = PieceIndex::build(&before, &[]);
            let old_prep = Prep::new(&before, &rules, &before.zones[0], lattice);
            let old = Ctx {
                board: &before, rules: &rules, zone: &before.zones[0], index: &before_index, prep: &old_prep,
            };
            let delta = board.apply_all(&ops).unwrap_or_else(|err| panic!("{edit}: {err}")).delta;
            let index = PieceIndex::build(&board, &[]);
            let prep = Prep::new(&board, &rules, &board.zones[0], lattice);
            let ctx = Ctx { board: &board, rules: &rules, zone: &board.zones[0], index: &index, prep: &prep };
            assert!(!delta.global && !delta.touches(zone), "{edit}: the update takes the regional path");
            assert_eq!(FillLattice::of(&board, &rules), lattice, "{edit}: edits preserve the lattice");
            let mut unchanged_front = 0;
            let mut unchanged_nonempty = 0;
            for layer in ctx.zone.layers.iter() {
                for key in lattice.tiles_in(board.outline_bbox()) {
                    let tile = lattice.tile_box(key);
                    if delta.meets(layer, tile.inflate(ctx.prep.reach)) {
                        continue;
                    }
                    let near = tile.inflate(ctx.zone.settings.min_width + 4 * ARC_ERR);
                    let a = query_selection(&old, layer, near);
                    let b = query_selection(&ctx, layer, near);
                    assert_eq!(a, b, "{edit}: full pieces, roles and own copper stay ordered at {key:?} on {layer:?}");
                    unchanged_front += usize::from(layer == LayerId::F_CU);
                    unchanged_nonempty += usize::from(!a.pieces.is_empty() && !a.own.is_empty());
                }
            }
            assert!(unchanged_front > 0, "{edit}: the stability check covers unaffected front tiles");
            assert!(unchanged_nonempty > 0, "{edit}: the stability check covers both obstacles and own copper");
            if edit == "overlapping edit windows" {
                assert_eq!(delta.extents.len(), 4, "both edits retain their old and new extents");
                let shared = lattice.tiles_in(board.outline_bbox()).into_iter().filter(|&key| {
                    let window = lattice.tile_box(key).inflate(ctx.prep.reach);
                    delta.extents[..2].iter().any(|e| e.layers.has(LayerId::F_CU) && e.bbox.intersects(&window))
                        && delta.extents[2..].iter()
                            .any(|e| e.layers.has(LayerId::F_CU) && e.bbox.intersects(&window))
                }).count();
                assert_eq!(shared, 1, "the two edit windows overlap in exactly one tile");
            }
            if edit == "folded move there and back" {
                assert_eq!(board.item(big), before.item(big), "the folded move restores the original placement");
                assert_eq!(delta.extents.len(), 2, "one change's delta has the part's sides before and after it, not the state between its ops");
            }
            let next = fill_update(&board, &rules, &prev, &delta);
            assert_eq!(next, fill(&board, &rules, zone), "{edit}: update equals full fill bit for bit");
            prev = next;
        }
        let mut clearance = board.zones[0].clone();
        clearance.settings.clearance = Some(rules.max_clearance() + 100_000);
        let mut thermal = clearance.clone();
        thermal.settings.thermal.gap += 200_000;
        for (edit, replacement) in [("zone clearance override", clearance), ("zone thermal change", thermal)] {
            let delta = board.apply(&BoardOp::Replace(Item::Zone(replacement)))
                .unwrap_or_else(|err| panic!("{edit}: {err}")).delta;
            assert!(delta.touches(zone), "{edit}: touching the zone forces the full path");
            let next = fill_update(&board, &rules, &prev, &delta);
            assert_eq!(next, fill(&board, &rules, zone), "{edit}: update equals full fill bit for bit");
            assert_ne!(next, prev, "{edit}: the zone override changes filled copper");
            prev = next;
        }
        let mut tight = board.clone();
        tight.zones[0].settings.clearance = Some(1);
        let mut unset = board.clone();
        unset.zones[0].settings.clearance = None;
        assert_eq!(fill(&tight, &rules, zone), fill(&unset, &rules, zone), "a zone clearance below the rules keeps the rules' gap");
    }

    /// Footprint zone keepouts cut the fill and follow moves beyond a tile; track-only keepouts do not cut it.
    #[test]
    fn footprint_keepouts_cut_the_fill() {
        let (mut board, zone) = board();
        board.zones[0].settings.islands = Islands::Keep;
        let rules = resolve_rules(&board, None);
        let s = FillLattice::of(&board, &rules).side();
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(35 * s / 4, 7 * s / 8), 0));
        let zones = P::new(2 * s, 0);
        let tracks = P::new(2 * s, s / 4);
        let part = keepout_part(&mut board, P::new(s / 2, s / 2), zones, tracks);
        board.reindex();
        let mut centre = board.placed[0].to_board(zones);
        let mut prev = fill(&board, &rules, zone);
        let copper = prev.layer(LayerId::F_CU).expect("front fill").set();
        assert!(!copper.contains(centre), "a footprint zone keepout removes copper at its centre");
        assert!(
            copper.contains(board.placed[0].to_board(tracks)),
            "a footprint track-only keepout retains copper at its centre",
        );
        let by = P::new(2 * s, 0);
        assert!(by.x > s, "each footprint move exceeds a tile side");
        for step in 1..=3 {
            let old = centre;
            let delta = board.apply(&BoardOp::Move { item: part, by })
                .unwrap_or_else(|err| panic!("keepout move {step}: {err}")).delta;
            centre = board.placed[0].to_board(zones);
            assert!(!delta.global && !delta.touches(zone), "move {step}: the keepout takes the regional update path");
            let next = fill_update(&board, &rules, &prev, &delta);
            assert_eq!(next, fill(&board, &rules, zone), "move {step}: keepout update equals full fill");
            let copper = next.layer(LayerId::F_CU).expect("front fill after move").set();
            assert!(!copper.contains(centre), "move {step}: copper is absent at the new zone keepout centre");
            assert!(copper.contains(old), "move {step}: copper returns at the old zone keepout centre");
            assert!(
                copper.contains(board.placed[0].to_board(tracks)),
                "move {step}: copper remains at the track-only keepout centre",
            );
            prev = next;
        }
    }

    /// A blind via removes copper only on the layers traversed by its ring and drill.
    #[test]
    fn blind_via_cuts_only_its_span() {
        let (mut board, zone) = board();
        let mut inner = board.stackup.layers.iter()
            .find(|layer| matches!(layer.kind, crate::StackKind::Copper(_))).expect("front copper").clone();
        let back = board.stackup.layers.iter()
            .position(|layer| layer.kind == crate::StackKind::Copper(LayerId::B_CU)).expect("back copper");
        inner.kind = crate::StackKind::Copper(LayerId::inner(1));
        board.stackup.layers.insert(back, inner.clone());
        inner.kind = crate::StackKind::Copper(LayerId::inner(2));
        board.stackup.layers.insert(back + 1, inner);
        board.zones[0].layers = board.copper_set();
        board.zones[0].settings.islands = Islands::Keep;
        board.outline.push(Contour::rect(P::ZERO, P::new(8_000_000, 8_000_000), 0));
        let centre = P::new(4_000_000, 4_000_000);
        let id = board.alloc_id();
        board.vias.push(Via {
            id,
            net: NetId(1),
            at: centre,
            drill: 300_000,
            pad: 450_000,
            span: (LayerId::F_CU, LayerId::inner(1)),
            kind: crate::ViaKind::Blind,
            tent: Default::default(),
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        });
        board.reindex();
        let rules = resolve_rules(&board, None);
        let filled = fill(&board, &rules, zone);
        assert!(
            !filled.layer(LayerId::F_CU).expect("front fill").set().contains(centre),
            "the blind via knocks out front copper at its drill centre",
        );
        assert!(
            !filled.layer(LayerId::inner(1)).expect("first inner fill").set().contains(centre),
            "the blind via knocks out the first inner layer at its drill centre",
        );
        assert!(
            filled.layer(LayerId::B_CU).expect("back fill").set().contains(centre),
            "back copper covers the drill centre beyond the blind via's span",
        );
        assert!(
            filled.layer(LayerId::inner(2)).expect("second inner fill").set().contains(centre),
            "the second inner layer covers the drill centre beyond the blind via's span",
        );
        // The only hook installed in this test binary: a host's pool, counted.
        static TURNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn turn() { TURNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
        fab_elec_core::pace::install_pace(turn);
        let turns = TURNS.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(fill_all(&board, &rules), vec![filled], "the shared-index fill agrees on every copper layer");
        assert!(TURNS.load(std::sync::atomic::Ordering::Relaxed) > turns, "the full fill gives the host's pool a turn between its tiles");
    }

    fn check_seams(fill: &ZoneFill) {
        let mut joined = [false; 2];
        for layer in &fill.layers {
            for (i, tile) in layer.tiles.iter().enumerate() {
                let b = fill.lattice.tile_box(tile.key);
                for poly in &tile.polys {
                    assert!(poly.rings().flatten().all(|&p| b.contains(p)), "clip stays inside its tile");
                }
                for (side, cuts) in tile.cuts.iter().enumerate() {
                    let line = [b.min.x, b.max.x, b.min.y, b.max.y][side];
                    assert_eq!(line.rem_euclid(fill.lattice.side()), 0, "cut line is exact integer nm");
                    for &(lo, hi, ord) in cuts {
                        assert!(lo < hi, "cuts have positive length");
                        for v in [lo, hi] {
                            let point = if side < 2 { P::new(line, v) } else { P::new(v, line) };
                            assert!(
                                tile.polys[ord as usize].rings().flatten().any(|&p| p == point),
                                "every cut endpoint is a polygon vertex exactly on the lattice line",
                            );
                        }
                    }
                }
                let neighbors = [
                    (TileKey { x: tile.key.x + 1, y: tile.key.y }, 1, 0),
                    (TileKey { x: tile.key.x, y: tile.key.y + 1 }, 3, 2),
                ];
                for (axis, (key, side, opposite)) in neighbors.into_iter().enumerate() {
                    let Ok(j) = layer.tiles.binary_search_by_key(&key, |t| t.key) else { continue };
                    let a = &tile.cuts[side];
                    let b = &layer.tiles[j].cuts[opposite];
                    let intervals = |cuts: &[(Nm, Nm, u32)]| {
                        let mut out: Vec<(Nm, Nm)> = Vec::new();
                        for &(lo, hi, _) in cuts {
                            if let Some(last) = out.last_mut() {
                                if lo <= last.1 { last.1 = last.1.max(hi); continue; }
                            }
                            out.push((lo, hi));
                        }
                        out
                    };
                    assert_eq!(intervals(a), intervals(b), "copper coverage meets exactly on both sides of a seam");
                    for &(al, ah, ao) in a {
                        for &(bl, bh, bo) in b {
                            if al.max(bl) < ah.min(bh) {
                                assert_eq!(
                                    layer.labels[i].kept[ao as usize], layer.labels[j].kept[bo as usize],
                                    "retention joins across polygon cuts",
                                );
                            }
                        }
                    }
                    for (a, b) in [
                        (&tile.core_cuts[side], &layer.tiles[j].core_cuts[opposite]),
                        (&tile.lone[side], &layer.tiles[j].lone[opposite]),
                    ] {
                        for &(al, ah, an) in a {
                            for &(bl, bh, bn) in b {
                                if al.max(bl) < ah.min(bh) {
                                    joined[axis] = true;
                                    if let (Some(ao), Some(bo)) = (
                                        tile.node.iter().position(|&node| node == an),
                                        layer.tiles[j].node.iter().position(|&node| node == bn),
                                    ) {
                                        assert_eq!(layer.labels[i].island[ao], layer.labels[j].island[bo],
                                            "connectivity joins across core or lone runs");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(joined.into_iter().all(|v| v), "fixture joins across both axes");
    }

    /// Tiled dual fills, including footprint keepouts, match a direct whole-zone form and keep exact seams on updates.
    #[test]
    fn tiles_cover_whole_fill_and_updates() {
        let (mut board, zone) = board();
        let rules = resolve_rules(&board, None);
        let lattice = FillLattice::of(&board, &rules);
        let s = lattice.side();
        assert!(s >= 8 * fill_reach(&rules, &board.zones[0].settings), "side covers eight reaches");
        assert!(lattice.level == 10 || s / 2 < 8 * fill_reach(&rules, &board.zones[0].settings));
        assert_eq!(
            lattice.tiles_in(BBox::point(P::ZERO)),
            vec![TileKey { x: -1, y: -1 }, TileKey { x: 0, y: -1 },
                TileKey { x: -1, y: 0 }, TileKey { x: 0, y: 0 }],
            "inclusive tile lookup and row-first ordering at the origin",
        );
        assert!(lattice.tiles_in(BBox::EMPTY).is_empty(), "empty box has no tiles");
        board.outline.push(Contour::rect(P::new(-s / 2, -s / 2), P::new(5 * s / 2, 5 * s / 2), 0));
        pad(&mut board, P::new(s, s / 2), NetId(1), None);
        pad(&mut board, P::new(3 * s / 2, s), NetId(1), None);
        pad(&mut board, P::new(s, s), NetId(0), None);
        pad(&mut board, P::new(s / 2, s / 2), NetId(0), Some(ZoneConnect::Off));
        pad(&mut board, P::new(3 * s / 4, 3 * s / 4), NetId(0), Some(ZoneConnect::Thermal));
        Arc::make_mut(&mut board.placed[4].footprint).pads[0].thermal = Some(ThermalRule {
            gap: s / 2, ..board.zones[0].settings.thermal.clone()
        });
        track(&mut board, NetId(1), Path::new(P::new(-s / 4, s / 3)).line(P::new(2 * s, s / 3)));
        track(&mut board, NetId(1), Path::new(P::new(5 * s / 3, -s / 4)).line(P::new(5 * s / 3, 2 * s)));
        let id = board.alloc_id();
        board.holes.push(Hole {
            id, name: String::new(), at: P::new(s / 2, 3 * s / 2), drill: 500_000,
            pad: None, net: None, plated: false, origin: Origin::Authored,
        });
        let id = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id,
            area: PolySet::rect(BBox::new(P::new(15 * s / 8, s / 2), P::new(9 * s / 4, 3 * s / 4))),
            layers: LayerSet::one(LayerId::F_CU),
            forbid: Forbid::ZONES,
            reason: String::new(),
            origin: Origin::Authored,
            z: None,
        });
        keepout_part(&mut board, P::new(s, 3 * s / 2), P::ZERO, P::new(s / 4, 0));
        let mut higher = board.zones[0].clone();
        higher.id = board.alloc_id();
        higher.net = Some(NetId(1));
        higher.priority = 1;
        higher.area = Area::Poly(PolySet::rect(BBox::new(
            P::new(7 * s / 4, 7 * s / 4), P::new(9 * s / 4, 9 * s / 4),
        )));
        board.zones.push(higher);
        assert_eq!(FillLattice::of(&board, &rules), lattice, "geometry does not move the lattice");
        let tiled = fill(&board, &rules, zone);

        // The whole-zone dual form, independent of tile_fill and the cuts/islands pass.
        let z = &board.zones[0];
        let layer = LayerId::F_CU;
        let h = z.settings.min_width / 2;
        let base = zone_base(&board, &rules, z);
        let reach = base.bbox().inflate(rules.max_clearance() + 4 * ARC_ERR);
        let zkey = RuleKey { class: class_of(&board, z.net), item: RuleItem::Zone };
        let copper = board.all_copper();
        let mut own = Vec::new();
        let mut knock = Vec::new();
        for c in copper.iter().filter(|c| c.layer == layer && c.shape.bbox().intersects(&reach)) {
            if z.net.is_some() && c.net == z.net {
                own.push(c);
                continue;
            }
            let gap = zone_gap(z, rules.clearance(zkey, c.key(), layer));
            knock.push(grown(&c.shape, gap + h));
        }
        for hole in board.drills() {
            let shape = hole.shape();
            if (z.net.is_none() || hole.net != z.net || !hole.plated) && shape.bbox().intersects(&reach) {
                knock.push(grown(&shape, rules.hole_clearance() + h));
            }
        }
        for other in &board.zones {
            if other.id == z.id || other.priority <= z.priority || !other.layers.has(layer)
                || (other.net.is_some() && other.net == z.net)
            {
                continue;
            }
            let okey = RuleKey { class: class_of(&board, other.net), item: RuleItem::Zone };
            knock.push(zone_base(&board, &rules, other).offset(
                rules.clearance(zkey, okey, layer) + ARC_ERR + h, ARC_ERR,
            ));
        }
        let mut masks = Vec::new();
        for k in &board.keepouts {
            if k.layers.has(layer) && k.forbid.has(Forbid::ZONES) && crate::drc::reaches_copper(k.z) {
                masks.push(k.area.clone());
            }
        }
        for placed in &board.placed {
            for keepout in &placed.footprint.keepouts {
                if keepout.forbid.has(Forbid::ZONES)
                    && keepout.layers.has(placed.fp_layer(layer, board.copper_count()))
                {
                    masks.push(placed.shape_to_board(&Shape::Poly(keepout.area.clone())).to_polyset(ARC_ERR));
                }
            }
        }
        for c in &own {
            let ConnItem::Pad { placed, pad } = c.item else { continue };
            let p = board.placed_item(placed).expect("own pad placement");
            let fp_pad = &p.footprint.pads[pad.ix()];
            let thermal = match fp_pad.zone_connect.unwrap_or(z.settings.connect) {
                ZoneConnect::Solid => false,
                ZoneConnect::Thermal => true,
                ZoneConnect::ThruThermal => fp_pad.kind == PadKind::Thru,
                ZoneConnect::Off => {
                    let gap = zone_gap(z, rules.clearance(zkey, zkey, layer));
                    knock.push(grown(&c.shape, gap + h));
                    continue;
                }
            };
            if !thermal {
                continue;
            }
            let t = fp_pad.thermal.as_ref().unwrap_or(&z.settings.thermal);
            let pad_set = c.shape.to_polyset(ARC_ERR);
            let ring = pad_set.offset(t.gap + ARC_ERR, ARC_ERR).difference(&pad_set);
            let b = c.shape.bbox();
            let centre = p.pad_at(pad).unwrap_or(b.center());
            let len = ((b.width().max(b.height()) as Nm) / 2 + t.gap * 2).max(t.gap * 2);
            let mut spokes = Vec::new();
            for q in 0..4 {
                let a = t.angle.add(p.rot).add(Angle::from_udeg(q * Angle::QUARTER.udeg() as i64));
                let (s, co) = a.sin_cos();
                let mid = centre.add(P::from_f(co * len as f64 / 2.0, s * len as f64 / 2.0));
                spokes.push(Shape::Rect {
                    c: mid, half: P::new(len / 2, t.spoke / 2), rot: a, corner: 0,
                }.to_polyset(ARC_ERR));
            }
            masks.push(ring.difference(&PolySet::union_all(spokes.iter())));
        }
        let base_e = if h > 0 { base.offset(-h, ARC_ERR) } else { base.clone() };
        let masks_e: Vec<_> = masks.iter().map(|mask| {
            if h > 0 { mask.offset(h, ARC_ERR) } else { mask.clone() }
        }).collect();
        let whole_e = base_e.difference(&PolySet::union_all(knock.iter()))
            .difference(&PolySet::union_all(masks_e.iter()));
        let mut whole = if h > 0 {
            whole_e.offset(h, ARC_ERR).intersection(&base.difference(&PolySet::union_all(masks.iter())))
        } else {
            whole_e
        };
        if z.settings.islands == Islands::Remove && z.net.is_some() {
            let kept: Vec<_> = whole.split().into_iter().filter(|poly| {
                let b = poly.bbox();
                let shape = Shape::Poly(poly.clone());
                own.iter().any(|c| c.shape.bbox().intersects(&b) && clearance(&shape, &c.shape) == 0)
            }).collect();
            whole = PolySet::union_all(kept.iter());
        }
        let assembled = tiled.layer(layer).expect("front fill").set();
        assert!(whole.area() > 0.0 && !tiled.is_empty(), "fixture contains kept copper");
        let difference = assembled.difference(&whole).area() + whole.difference(&assembled).area();
        assert!(difference < whole.area() * 1e-6, "tiles cover what the whole fill covers: {difference}");
        check_seams(&tiled);

        let before = board.clone();
        track(&mut board, NetId(1), Path::new(P::new(4 * s, 4 * s)).line(P::new(5 * s, 4 * s)));
        let delta = BoardDelta::between(&before, &board);
        assert!(!reaches(&board, &rules, zone, &delta), "far edit cannot reach the pour");
        let far = fill_update(&board, &rules, &tiled, &delta);
        assert_eq!(far, tiled, "far update retains the previous fill");
        assert!(far.layers[0].tiles.iter().zip(&tiled.layers[0].tiles).all(|(a, b)| Arc::ptr_eq(a, b)));
        let before = board.clone();
        track(&mut board, NetId(1), Path::new(P::new(s / 4, 9 * s / 4)).line(P::new(s, 9 * s / 4)));
        let delta = BoardDelta::between(&before, &board);
        assert!(reaches(&board, &rules, zone, &delta), "near edit reaches the pour");
        let near = fill_update(&board, &rules, &far, &delta);
        assert_eq!(near, fill(&board, &rules, zone), "update equals full");
        assert_ne!(near, far, "near edit changes copper");
        assert!(near.layers[0].tiles.iter().zip(&far.layers[0].tiles).any(|(a, b)| Arc::ptr_eq(a, b)));
        board.zones.retain(|z| z.id != zone);
        let gone = fill_update(&board, &rules, &near, &BoardDelta::default());
        assert!(gone.layers.is_empty() && gone.is_empty(), "absent zones have no layers");
    }

    #[test]
    fn a_drag_recomputes_only_its_ends() {
        let (mut board, zone) = board();
        board.zones[0].layers = board.copper_set();
        board.zones[0].settings.islands = Islands::Keep;
        let rules = resolve_rules(&board, None);
        let lattice = FillLattice::of(&board, &rules);
        let s = lattice.side();
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(27 * s / 4, 7 * s / 4), 0));
        pad(&mut board, P::new(3 * s / 4, 3 * s / 4), NetId(1), None);
        board.reindex();
        let part = board.placed[0].id;
        let old_box = board.placed[0].bbox();
        let old_extent = board.item(part).expect("the part is indexed").extent(&board);
        let before = fill(&board, &rules, zone);

        // A preview starts from the committed, already filled board, even after a long drag.
        let mut preview = board.clone();
        let delta = preview.apply(&BoardOp::Move { item: part, by: P::new(5 * s, 0) })
            .expect("the preview move applies").delta;
        let new_box = preview.placed[0].bbox();
        let reach = fill_reach(&rules, &preview.zones[0].settings);
        let front = before.layer(LayerId::F_CU).expect("the zone has a front layer");
        let mut between = 0;
        let mut ends = [0; 2];
        for tile in &front.tiles {
            let window = lattice.tile_box(tile.key).inflate(reach);
            let old_hit = window.intersects(&old_box);
            let new_hit = window.intersects(&new_box);
            assert_eq!(
                delta.meets(LayerId::F_CU, window), old_hit || new_hit,
                "front tile {:?} recomputes exactly when its window meets an end", tile.key,
            );
            ends[0] += usize::from(old_hit);
            ends[1] += usize::from(new_hit);
            if window.min.x > old_box.max.x && window.max.x < new_box.min.x
                && window.min.y <= old_box.max.y && window.max.y >= old_box.min.y
            {
                between += 1;
                assert!(
                    !delta.meets(LayerId::F_CU, window),
                    "front tile {:?} strictly between the ends is not recomputed", tile.key,
                );
            }
        }
        assert!(ends.iter().all(|&count| count > 0), "both drag ends meet front tile windows");
        assert!(between > 0, "the drag leaves tile windows strictly between its ends");
        let back = before.layer(LayerId::B_CU).expect("the zone has a back layer");
        assert!(!back.tiles.is_empty(), "the back layer has tiles to check");
        for tile in &back.tiles {
            assert!(
                !delta.meets(LayerId::B_CU, lattice.tile_box(tile.key).inflate(reach)),
                "a top SMD part reaches no back tile, including {:?}", tile.key,
            );
        }
        let new_extent = preview.item(part).expect("the moved part is indexed").extent(&preview);
        assert_eq!(delta.bbox, old_extent.union(new_extent), "DRC's region still spans both complete extents");
        let updated = fill_update(&preview, &rules, &before, &delta);
        assert_eq!(updated, fill(&preview, &rules, zone), "the preview update equals a full fill");
        assert_ne!(updated, before, "the moved foreign pad changes the filled copper");
    }

    /// A zone clearance above the rules' largest, and a pad thermal above the zone's, reach past the
    /// rules' reach: an update must still recompute the tiles they reach into.
    #[test]
    fn updates_reach_zone_and_pad_overrides() {
        for case in ["zone clearance", "pad thermal"] {
            let (mut board, zone) = board();
            let rules = resolve_rules(&board, None);
            let wide = 6_000_000;
            if case == "zone clearance" {
                board.zones[0].settings.clearance = Some(rules.max_clearance() + wide);
            }
            let lattice = FillLattice::of(&board, &rules);
            let s = lattice.side();
            board.outline.push(Contour::rect(P::ZERO, P::new(2 * s, s), 0));
            // The edited copper stays 4 mm inside the first tile; its knockout or relief reaches the second.
            let x = s - 4_000_000;
            let item = match case {
                "zone clearance" => {
                    track(&mut board, NetId(1), Path::new(P::new(x, s / 4)).line(P::new(x, 3 * s / 4)));
                    board.tracks[0].id
                }
                _ => {
                    pad(&mut board, P::new(x, s / 2), NetId(0), Some(ZoneConnect::Thermal));
                    let fp = Arc::make_mut(&mut board.placed[0].footprint);
                    fp.pads[0].thermal = Some(ThermalRule { gap: wide / 2, ..board.zones[0].settings.thermal });
                    board.placed[0].id
                }
            };
            board.reindex();
            let mut prev = fill(&board, &rules, zone);
            for step in 0..3 {
                let delta = board.apply(&BoardOp::Move { item, by: P::new(300_000, 0) })
                    .unwrap_or_else(|e| panic!("{case}: move {step}: {e}")).delta;
                let next = fill_update(&board, &rules, &prev, &delta);
                assert_eq!(next, fill(&board, &rules, zone), "{case}: update equals full after move {step}");
                prev = next;
            }
            let second = prev.layer(LayerId::F_CU).expect("front fill").tiles.iter()
                .any(|t| t.key.x == 1);
            assert!(second, "{case}: the second tile column is filled");
        }
    }

    #[test]
    fn indexed_update_matches_fresh_index() {
        let (mut board, zone) = board();
        board.zones[0].settings.islands = Islands::Keep;
        let rules = resolve_rules(&board, None);
        let s = FillLattice::of(&board, &rules).side();
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(11 * s / 4, 3 * s / 4), 0));
        pad(&mut board, P::new(3 * s / 2, s / 2), NetId(1), None);
        let foreign_pad = board.placed[0].id;
        track(&mut board, NetId(1), Path::new(P::new(s / 2, s / 3)).line(P::new(s / 2, 2 * s / 3)));
        let foreign_track = board.tracks[0].id;
        board.reindex();
        let mut wider = board.zones[0].clone();
        wider.settings.clearance = Some(rules.max_clearance() + 100_000);
        let edits = [
            ("move foreign track", vec![BoardOp::Move { item: foreign_track, by: P::new(s / 8, 0) }]),
            ("change clearance and move track", vec![
                BoardOp::Replace(Item::Zone(wider)),
                BoardOp::Move { item: foreign_track, by: P::new(s / 8, 0) },
            ]),
            ("move foreign pad", vec![BoardOp::Move { item: foreign_pad, by: P::new(s / 8, 0) }]),
        ];
        let mut prev = fill(&board, &rules, zone);
        let mut index = PieceIndex::build(&board, std::slice::from_ref(&prev));
        for (edit, ops) in edits {
            let old_index = index.clone();
            let delta = board.apply_all(&ops).unwrap_or_else(|err| panic!("{edit}: {err}")).delta;
            // Keep the previous fill held while updating fixed pieces for the new board.
            index.update(&board, std::slice::from_ref(&prev), &delta, &[]);
            assert_eq!(index.generation(), board.generation, "{edit}: the maintained index is current");
            assert_ne!(old_index.generation(), board.generation, "{edit}: the old index needs the guard");
            let next = fill_update_indexed(&board, &rules, &prev, &delta, &index);
            let fresh = fill_update(&board, &rules, &prev, &delta);
            let full = fill(&board, &rules, zone);
            assert_ne!(full, prev, "{edit}: the edit changes filled copper");
            assert_eq!(next, fresh, "{edit}: a maintained index with held fills matches a fresh index");
            assert_eq!(next, full, "{edit}: the indexed update matches a full fill");
            assert_eq!(
                fill_update_indexed(&board, &rules, &prev, &delta, &old_index), full,
                "{edit}: the generation guard replaces a stale index and matches a full fill",
            );
            prev = next;
        }
    }

    #[test]
    fn fill_plan_is_send_sync() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<FillPlan>();
    }

    #[test]
    fn plan_tiles_in_any_order_match_update() {
        let (mut board, zone) = board();
        board.zones[0].layers = board.copper_set();
        board.zones[0].settings.islands = Islands::Keep;
        let rules = resolve_rules(&board, None);
        let s = FillLattice::of(&board, &rules).side();
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(11 * s / 4, 3 * s / 4), 0));
        pad(&mut board, P::new(3 * s / 2, s / 2), NetId(1), None);
        track(&mut board, NetId(1), Path::new(P::new(s / 2, s / 3)).line(P::new(s / 2, 2 * s / 3)));
        let foreign_track = board.tracks[0].id;
        board.reindex();
        let mut wider = board.zones[0].clone();
        wider.settings.clearance = Some(rules.max_clearance() + 100_000);
        let mut far = board.tracks[0].clone();
        far.id = board.alloc_id();
        far.path = Path::new(P::new(8 * s, 8 * s)).line(P::new(9 * s, 8 * s));
        let edits = [
            ("move foreign track", vec![BoardOp::Move { item: foreign_track, by: P::new(s / 8, 0) }]),
            ("change zone clearance", vec![
                BoardOp::Replace(Item::Zone(wider)),
                BoardOp::Move { item: foreign_track, by: P::new(s / 8, 0) },
            ]),
            ("add far track", vec![BoardOp::Add(Item::Track(far))]),
        ];
        let mut prev = fill(&board, &rules, zone);
        let mut index = PieceIndex::build(&board, std::slice::from_ref(&prev));
        for (edit, ops) in edits {
            let old_index = index.clone();
            let delta = board.apply_all(&ops).unwrap_or_else(|err| panic!("{edit}: {err}")).delta;
            index.update(&board, std::slice::from_ref(&prev), &delta, &[]);
            assert_eq!(index.generation(), board.generation, "{edit}: the maintained index is current");
            assert_ne!(old_index.generation(), board.generation, "{edit}: the previous index is stale");
            let full = fill(&board, &rules, zone);
            let tile_count: usize = full.layers.iter().map(|layer| layer.tiles.len()).sum();
            for (source, index) in [("current", &index), ("stale", &old_index)] {
                let plan = FillPlan::update(&board, &rules, &prev, &delta, index);
                match edit {
                    "move foreign track" => {
                        assert!(!delta.global && !delta.touches(zone), "{edit}: the edit is regional");
                        assert!(plan.len() > 0, "{edit}: {source} index plans some tile computations");
                        assert!(plan.len() < tile_count, "{edit}: {source} index plans fewer than all tiles");
                    }
                    "change zone clearance" => {
                        assert!(delta.touches(zone), "{edit}: the zone edit requires a full plan");
                        assert_eq!(plan.len(), tile_count, "{edit}: {source} index plans every tile");
                    }
                    _ => {
                        assert!(!reaches(&board, &rules, zone, &delta), "{edit}: the edit cannot reach the zone");
                        assert_eq!(plan.len(), 0, "{edit}: {source} index plans no computations");
                        assert!(plan.is_empty(), "{edit}: {source} index reports an empty plan");
                    }
                }
                let mut tiles: Vec<_> = (0..plan.len()).rev()
                    .map(|i| plan.tile(&board, &rules, index, i)).collect();
                tiles.reverse();
                let next = plan.finish(tiles);
                assert_eq!(
                    next, fill_update_indexed(&board, &rules, &prev, &delta, index),
                    "{edit}: reverse-order tiles with the {source} index match the serial update",
                );
                assert_eq!(next, full, "{edit}: reverse-order tiles with the {source} index match a full fill");
                if edit == "add far track" {
                    assert_eq!(next, prev, "{edit}: {source} index retains the unchanged fill");
                } else {
                    assert_ne!(next, prev, "{edit}: {source} index reflects the changed copper");
                }
                for layer in &next.layers {
                    let Some(old_layer) = prev.layer(layer.layer) else { continue };
                    for tile in &layer.tiles {
                        let old = old_layer.tiles.iter().find(|old| old.key == tile.key && old.hash == tile.hash);
                        if let Some(old) = old {
                            assert!(
                                Arc::ptr_eq(tile, old), "{edit}: {source} index shares equal tiles by key and hash",
                            );
                        }
                    }
                }
            }
            prev = full;
        }
    }

    #[test]
    fn regional_updates_match_full_across_edits() {
        let (mut board, zone) = board();
        board.zones[0].layers = board.copper_set();
        let rules = resolve_rules(&board, None);
        let s = FillLattice::of(&board, &rules).side();
        board.outline.push(Contour::rect(P::new(s / 4, s / 4), P::new(11 * s / 4, 7 * s / 4), 0));
        pad(&mut board, P::new(7 * s / 8, 3 * s / 4), NetId(1), None);
        let foreign_pad = board.placed[0].id;
        pad(&mut board, P::new(s / 2, 5 * s / 4), NetId(0), Some(ZoneConnect::Thermal));
        let own_pad = board.placed[1].id;
        track(&mut board, NetId(0), Path::new(P::new(s / 3, 3 * s / 2)).line(P::new(5 * s / 2, 3 * s / 2)));
        track(&mut board, NetId(1), Path::new(P::new(3 * s / 2, s / 2)).line(P::new(5 * s / 2, s / 2)));
        let via = board.alloc_id();
        board.vias.push(Via {
            id: via,
            net: NetId(0),
            at: P::new(2 * s, 5 * s / 4),
            drill: 300_000,
            pad: 600_000,
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: Default::default(),
            tent: Default::default(),
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        });
        board.reindex();
        let mut corner = board.tracks[1].clone();
        corner.id = board.alloc_id();
        corner.path = Path::new(P::new(3 * s / 4, 3 * s / 4)).line(P::new(5 * s / 4, 5 * s / 4));
        let corner_id = corner.id;
        let mut far = corner.clone();
        far.id = board.alloc_id();
        far.path = Path::new(P::new(4 * s, 4 * s)).line(P::new(5 * s, 4 * s));
        let far_id = far.id;
        let edits = [
            ("move foreign pad across seam", BoardOp::Move { item: foreign_pad, by: P::new(s / 4, 0) }),
            ("move foreign pad within tile", BoardOp::Move { item: foreign_pad, by: P::new(s / 8, 0) }),
            ("add foreign track across corner", BoardOp::Add(Item::Track(corner))),
            ("remove foreign track", BoardOp::Remove(corner_id)),
            ("move thermal pad next to seam", BoardOp::Move { item: own_pad, by: P::new(s / 2 - 100_000, 0) }),
            ("remove via", BoardOp::Remove(via)),
            ("add far track", BoardOp::Add(Item::Track(far))),
            ("change outline", BoardOp::SetOutline(vec![
                Contour::rect(P::new(s / 4, s / 4), P::new(9 * s / 4, 7 * s / 4), 0),
            ])),
        ];
        let mut prev = fill(&board, &rules, zone);
        for (edit, op) in edits {
            let delta = board.apply(&op).unwrap_or_else(|err| panic!("{edit}: {err}")).delta;
            assert_eq!(delta.global, matches!(&op, BoardOp::SetOutline(_)), "{edit}: global flag");
            let next = fill_update(&board, &rules, &prev, &delta);
            let full = fill(&board, &rules, zone);
            assert_eq!(next.lattice, full.lattice, "{edit}: lattice");
            assert_eq!(next.layers.len(), full.layers.len(), "{edit}: layer count");
            for (actual, expected) in next.layers.iter().zip(&full.layers) {
                assert_eq!(actual.layer, expected.layer, "{edit}: layer order");
                assert_eq!(actual.tiles.len(), expected.tiles.len(), "{edit}: tile count on {:?}", actual.layer);
                for (a, b) in actual.tiles.iter().zip(&expected.tiles) {
                    assert_eq!(a.key, b.key, "{edit}: tile key on {:?}", actual.layer);
                    assert_eq!(a.hash, b.hash, "{edit}: tile {:?} hash on {:?}", a.key, actual.layer);
                }
                assert_eq!(actual.labels, expected.labels, "{edit}: tile labels on {:?}", actual.layer);
            }
            if delta.added.contains(&far_id) {
                let reach = fill_reach(&rules, &board.zones[0].settings);
                assert!(prev.layers.iter().flat_map(|layer| &layer.tiles).all(|tile| {
                    !prev.lattice.tile_box(tile.key).inflate(reach).intersects(&delta.bbox)
                }), "{edit}: outside every tile window");
                assert_eq!(next, prev, "{edit}: retains the previous fill");
                for (new, old) in next.layers.iter().zip(&prev.layers) {
                    for (a, b) in new.tiles.iter().zip(&old.tiles) {
                        assert!(Arc::ptr_eq(a, b), "{edit}: shares tile {:?} on {:?}", a.key, new.layer);
                    }
                }
            }
            prev = next;
        }
    }

    #[test]
    fn global_islands_change_view_keys_without_copying_tiles() {
        let (mut board, zone) = board();
        let rules = resolve_rules(&board, None);
        let s = FillLattice::of(&board, &rules).side();
        board.outline.push(Contour::rect(P::ZERO, P::new(4 * s, 4 * s), 0));
        pad(&mut board, P::new(s / 4, s / 4), NetId(0), Some(ZoneConnect::Solid));
        let lo = s / 2;
        let hi = 7 * s / 2;
        track(&mut board, NetId(1), Path::new(P::new(hi, 3 * s / 2))
            .line(P::new(hi, lo)).line(P::new(lo, lo)).line(P::new(lo, hi))
            .line(P::new(hi, hi)).line(P::new(hi, 5 * s / 2)));
        let open = fill(&board, &rules, zone);
        let key = TileKey { x: 1, y: 1 };
        let old = open.layer(LayerId::F_CU).expect("front fill");
        let i = old.tiles.binary_search_by_key(&key, |tile| tile.key).expect("interior tile");
        assert_eq!(old.tiles[i].polys.len(), 1, "one interior polygon");
        assert_eq!(old.tiles[i].own, vec![false], "interior has no own copper");
        assert_eq!(old.labels[i].kept, vec![true], "open ring reaches own copper through distant tiles");

        let before = board.clone();
        track(&mut board, NetId(1), Path::new(P::new(hi, 3 * s / 2)).line(P::new(hi, 5 * s / 2)));
        let delta = BoardDelta::between(&before, &board);
        assert!(
            !delta.bbox.intersects(&open.lattice.tile_box(key).inflate(fill_reach(&rules, &board.zones[0].settings))),
            "closing the ring is outside the interior tile's window",
        );
        let closed = fill_update(&board, &rules, &open, &delta);
        assert_eq!(closed, fill(&board, &rules, zone), "ring-closing update equals full");
        let removed = closed.layer(LayerId::F_CU).expect("front fill");
        assert_eq!(removed.labels[i].kept, vec![false], "islands are removed globally");
        assert_eq!(old.tiles[i].hash, removed.tiles[i].hash, "far edit preserves tile geometry hash");
        assert!(Arc::ptr_eq(&old.tiles[i], &removed.tiles[i]), "far edit shares immutable tile");
        assert_ne!(old.labels[i].view_hash, removed.labels[i].view_hash, "the draw key follows removal");
        let centre = P::new(3 * s / 2, 3 * s / 2);
        assert!(!removed.set().contains(centre), "export excludes the removed island");
        assert!(removed.set().contains(P::new(s / 4, s / 4)), "own-connected exterior survives");
        check_seams(&closed);

        let before = board.clone();
        board.zones[0].settings.islands = Islands::Keep;
        let kept = fill_update(&board, &rules, &closed, &BoardDelta::between(&before, &board));
        let kept_layer = kept.layer(LayerId::F_CU).expect("front fill");
        assert!(kept_layer.labels.iter().all(|l| l.kept.iter().all(|&v| v)), "Keep retains every island");
        assert!(kept_layer.set().contains(centre), "Keep exports the interior island");
        assert!(Arc::ptr_eq(&removed.tiles[i], &kept_layer.tiles[i]), "island policy does not copy tiles");
    }
}
