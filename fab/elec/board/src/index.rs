//! Persistent geometry pieces shared by connectivity, DRC and routing.
//!
//! A piece is copper on one layer, a fill island, an outline or cutout step, a drill, or a
//! keepout that reaches the copper (a keepout whose height band starts above the surface
//! forbids bodies only). It carries identity and geometry only; rules, fixity and locks live
//! elsewhere.
//! Item ids and per-item ranks order every query, with the outline last; a zone's pieces are
//! the kept polygons of its fill tiles, keyed by (layer, tile) and polygon ordinal. The item
//! trie, layer quadtrees and relief quadtrees are canonical: building fresh and applying edits
//! to the same board and fills produce equal indexes, including their generation and query order.
//! Relief quadtrees let fill queries find pads whose own thermal relief reaches beyond the rule-bounded window.
//!
//! Snapshots share their outline, item arrays, tile piece arrays and tree nodes through `Arc`.
//! Updates copy only touched paths, so readers can retain an index beside its matching
//! `Arc<Board>` generation. A refilled zone keeps the pieces of every tile whose `view_hash`
//! (and the zone's net and class) is unchanged and replaces only the others, so a local refill
//! costs O(tiles) pointer copies plus the changed tiles' pieces. Spatial costs are build
//! O(n log n), update O(d log n) with path copies, query O(log n + k log k), clone O(layers).
//! A pad with its own thermal adds an O(depth) relief-tree edit per copper layer on add or remove.

use crate::geom::pad_relief;
use crate::zone::{FillTile, PolyRef, TileKey, TileLabels};
use crate::{Board, BoardDelta, ConnItem, Copper, ItemKind, Obstacle, Zone, ZoneFill};
use fab_elec_core::{
    clearance, BBox, ClassId, ContentHash, ItemId, LayerId, LayerSet, NetId, Nm, PQuad, PVec, PadIx, Path,
    PolyGrid, Seg, Shape,
};
use fab_elec_parts::Forbid;
use std::collections::HashMap;
use std::fmt;
use std::ops::ControlFlow;
use std::sync::Arc;

/// What a piece is: identity and geometry, without rule logic or router state.
#[derive(Clone, PartialEq, Debug)]
pub enum PieceKind {
    /// A pad, track segment, via ring or plated hole ring on one layer, from `item_copper`.
    Copper(Copper),
    /// A via ring a zone of its net offers on this layer (`Board::rings`): copper only where that
    /// zone's fill reaches it, and then the index holds it as `Copper`. The fill takes it as the via's
    /// copper, so the fill is the same whether it is reached or not; `copper` answers `None`.
    Offer(Copper),
    /// One kept polygon of a zone's fill tile on one layer. Fills yield to routing rather than
    /// obstruct it.
    Fill(Copper),
    /// A step of the outline or a cutout, with width zero.
    Edge(Shape),
    /// A drilled hole, represented by a circle or an oval for a slot.
    Drill {
        /// The pad, via or hole owning the drill.
        item: ConnItem,
        /// The owner's net, if any.
        net: Option<NetId>,
        /// The drill in board coordinates.
        shape: Shape,
        /// Whether the hole is plated; an unplated own-net hole is still knocked out of a zone.
        plated: bool,
    },
    /// A keepout area.
    Keepout {
        /// The area in board coordinates.
        area: Shape,
        /// The kinds of items forbidden in the area.
        forbid: Forbid,
    },
}

/// A history-independent piece identity. Every query reports pieces in this order.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PKey {
    /// The board item id, or `u32::MAX` for the outline.
    pub item: u32,
    /// The fill tile of a zone piece (its layer and lattice key); `None` for every other piece.
    pub tile: Option<(LayerId, TileKey)>,
    /// The piece's ordinal among its item's pieces, or a fill piece's polygon ordinal in its tile.
    pub rank: u32,
}

/// One board geometry piece, with its identity, copper layers and bounding box.
#[derive(Clone, PartialEq, Debug)]
pub struct Piece {
    /// The geometry and its owning conductor or keepout restrictions.
    pub kind: PieceKind,
    /// The board item; `None` for the outline.
    pub item: Option<ItemId>,
    /// The fill tile of a zone piece; `None` for every other piece.
    pub tile: Option<(LayerId, TileKey)>,
    /// The piece's ordinal among its item's pieces, or a fill piece's polygon ordinal in its tile.
    pub rank: u32,
    /// The copper layers it is on.
    pub layers: LayerSet,
    /// The closed bounding box of its shape; empty boxes are not spatially indexed.
    pub bbox: BBox,
    /// A pad's own thermal relief reach from `pad_relief`; `None` for every other piece.
    pub relief: Option<Nm>,
    /// The kept polygon's distance index for a fill piece; `None` for every other piece.
    pub grid: Option<Arc<PolyGrid>>,
}

impl Piece {
    fn new(kind: PieceKind, item: Option<ItemId>, rank: u32, layers: LayerSet) -> Self {
        let bbox = match &kind {
            PieceKind::Copper(c) | PieceKind::Offer(c) | PieceKind::Fill(c) => c.shape.bbox(),
            PieceKind::Edge(s) | PieceKind::Drill { shape: s, .. } | PieceKind::Keepout { area: s, .. } => s.bbox(),
        };
        Self { kind, item, tile: None, rank, layers, bbox, relief: None, grid: None }
    }

    /// The board item id (or `u32::MAX` for the outline), fill tile and piece rank.
    pub fn key(&self) -> PKey {
        PKey { item: self.item.map_or(u32::MAX, |id| id.0), tile: self.tile, rank: self.rank }
    }

    /// The geometry in board coordinates.
    pub fn shape(&self) -> &Shape {
        match &self.kind {
            PieceKind::Copper(c) | PieceKind::Offer(c) | PieceKind::Fill(c) => &c.shape,
            PieceKind::Edge(s) | PieceKind::Drill { shape: s, .. } | PieceKind::Keepout { area: s, .. } => s,
        }
    }

    /// The copper of a fixed copper piece or a fill island; an offered ring is not copper.
    pub fn copper(&self) -> Option<&Copper> {
        match &self.kind {
            PieceKind::Copper(c) | PieceKind::Fill(c) => Some(c),
            _ => None,
        }
    }

    /// What copper is tested against by `check_to`; fixed copper and fills both give copper, and so
    /// does an offered ring (a foreign net keeps clear of a ring the next refill can flash; DRC, which
    /// checks final copper, skips it).
    pub fn obstacle(&self) -> Obstacle<'_> {
        match &self.kind {
            PieceKind::Copper(c) | PieceKind::Offer(c) | PieceKind::Fill(c) => Obstacle::Copper(c),
            PieceKind::Edge(s) => Obstacle::Edge(s),
            PieceKind::Drill { item, net, shape, .. } => Obstacle::Drill { item: *item, net: *net, shape },
            PieceKind::Keepout { area, forbid } => Obstacle::Keepout { area, forbid: *forbid },
        }
    }

    /// The net of copper, fill islands and drills; edges and keepouts have none.
    pub fn net(&self) -> Option<NetId> {
        match &self.kind {
            PieceKind::Copper(c) | PieceKind::Offer(c) | PieceKind::Fill(c) => c.net,
            PieceKind::Drill { net, .. } => *net,
            PieceKind::Edge(_) | PieceKind::Keepout { .. } => None,
        }
    }
}

/// Equals `clearance(&a.shape, &b.shape)` bit for bit for the pieces' copper.
/// Uses either piece's grid while preserving argument order, including for two polygon sets.
pub fn piece_clearance(pa: &Piece, a: &Copper, pb: &Piece, b: &Copper) -> Nm {
    if let Some(grid) = &pa.grid {
        return grid.clearance(&b.shape);
    }
    if let Some(grid) = &pb.grid {
        return grid.clearance_from(&a.shape);
    }
    clearance(&a.shape, &b.shape)
}

/// The pieces of one board item.
#[derive(Clone, PartialEq, Debug)]
enum Entry {
    /// Fixed copper, drills, keepouts or edges, in rank order.
    Item(Arc<[Piece]>),
    /// A zone's fill, per tile.
    Zone(Arc<ZonePieces>),
}

/// A zone's fill pieces: the net and class they carry, and its tiles sorted by (layer, key).
#[derive(Clone, PartialEq, Debug)]
struct ZonePieces {
    net: Option<NetId>,
    class: ClassId,
    tiles: Vec<TilePieces>,
    /// The offered via rings the fill reaches, (via, layer), sorted.
    reached: Vec<(ItemId, LayerId)>,
}

/// One fill tile's kept polygons as pieces, in ordinal order, with their islands.
#[derive(Clone, PartialEq, Debug)]
struct TilePieces {
    layer: LayerId,
    key: TileKey,
    view_hash: ContentHash,
    pieces: Arc<[Piece]>,
    island: Arc<[PolyRef]>,
}

/// What `PieceIndex::update` re-derived, the unit readers update by. A board item in the delta loses
/// and regains all its pieces (`items`, never a zone). A zone keeps each fill tile whose kept polygons are
/// unchanged and names the others in `tiles` (replaced, added or gone); `relabeled` names the tiles whose
/// polygons stayed while their island labels moved. `rebuilt`: the whole index was derived again (a global
/// delta or a new copper stackup), so every piece changed. Lists are sorted and free of repeats.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct IndexChange {
    pub rebuilt: bool,
    pub items: Vec<ItemId>,
    pub tiles: Vec<(ItemId, LayerId, TileKey)>,
    pub relabeled: Vec<(ItemId, LayerId, TileKey)>,
}

impl IndexChange {
    /// The change that covers every piece, as a rebuild does: what a caller passes for updates it did
    /// not follow.
    pub fn all() -> IndexChange {
        IndexChange { rebuilt: true, ..IndexChange::default() }
    }

    /// Fold `next`, a later update's change, into this one: what the index re-derived over both. A
    /// rebuild covers every piece, so its lists are dropped. O((a + b) log(a + b)) in the list lengths.
    pub fn absorb(&mut self, next: &IndexChange) {
        self.rebuilt |= next.rebuilt;
        if self.rebuilt {
            *self = IndexChange::all();
            return;
        }
        for (into, more) in [(&mut self.tiles, &next.tiles), (&mut self.relabeled, &next.relabeled)] {
            into.extend_from_slice(more);
            into.sort_unstable();
            into.dedup();
        }
        self.items.extend_from_slice(&next.items);
        self.items.sort_unstable();
        self.items.dedup();
    }
}

/// A persistent spatial snapshot of a board and the supplied fills of its zones.
#[derive(Clone)]
pub struct PieceIndex {
    generation: u64,
    layers: Vec<LayerId>,
    outline: Arc<[Piece]>,
    items: PVec<Entry>,
    quads: Vec<PQuad<PKey>>,
    reliefs: Vec<PQuad<PKey>>,
    len: usize,
    /// How many indexed fills reach each offered via ring: where `Offer` pieces are `Copper`.
    reached: Arc<HashMap<(ItemId, LayerId), u32>>,
}

impl PieceIndex {
    /// Derive every piece of `board` and its supplied zone fills.
    ///
    /// Fills whose zones are absent add nothing; zones without supplied fills have no islands.
    pub fn build(board: &Board, fills: &[ZoneFill]) -> PieceIndex {
        let layers = board.stackup.copper();
        let outline: Arc<[Piece]> = outline_pieces(board).into();
        let mut quads = vec![PQuad::new(); layers.len()];
        let mut reliefs = vec![PQuad::new(); layers.len()];
        insert_quads(&layers, &mut quads, &mut reliefs, &outline);
        let mut index = Self {
            generation: board.generation,
            layers,
            len: outline.len(),
            outline,
            items: PVec::new(),
            quads,
            reliefs,
            reached: Arc::default(),
        };
        for id in item_ids(board) {
            index.add_item(board, id);
        }
        let mut change = IndexChange::default();
        for zone in &board.zones {
            index.set_zone(board, zone, fill_of(fills, zone.id), &mut change);
        }
        index
    }

    /// Apply `delta` and the zones in `refilled`, using current fills (including held old fills), and
    /// say what changed.
    ///
    /// A global delta or changed copper stackup rebuilds. Otherwise sorted, deduplicated ids
    /// lose their old pieces and gain their current pieces, copying only touched tree paths.
    pub fn update(
        &mut self,
        board: &Board,
        fills: &[ZoneFill],
        delta: &BoardDelta,
        refilled: &[ItemId],
    ) -> IndexChange {
        let mut change = IndexChange::default();
        if delta.global || board.stackup.copper() != self.layers {
            *self = Self::build(board, fills);
            change.rebuilt = true;
            return change;
        }
        let mut ids = delta.ids();
        ids.extend_from_slice(refilled);
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            match board.locate(id) {
                Some((ItemKind::Zone, i)) => {
                    let zone = &board.zones[i];
                    self.set_zone(board, zone, fill_of(fills, id), &mut change);
                }
                _ => {
                    match self.items.remove(id.0) {
                        Some(Entry::Item(previous)) => self.remove_pieces(&previous),
                        Some(Entry::Zone(previous)) => {
                            for tile in &previous.tiles {
                                self.remove_pieces(&tile.pieces);
                                change.tiles.push((id, tile.layer, tile.key));
                            }
                            self.move_reach(board, &previous.reached, &[], &mut change);
                        }
                        None => {}
                    }
                    self.add_item(board, id);
                    change.items.push(id);
                }
            }
        }
        change.items.sort_unstable();
        change.items.dedup();
        change.tiles.sort_unstable();
        change.tiles.dedup();
        change.relabeled.sort_unstable();
        change.relabeled.dedup();
        self.generation = board.generation;
        change
    }

    /// The kept polygons of one fill tile as pieces, in ordinal order; empty for a tile not indexed.
    pub fn tile_pieces(&self, zone: ItemId, layer: LayerId, key: TileKey) -> &[Piece] {
        self.tile(zone, layer, key).map_or(&[][..], |tile| &tile.pieces[..])
    }

    /// The board generation represented by this snapshot.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The board's copper layers, in stack order.
    pub fn layers(&self) -> &[LayerId] {
        &self.layers
    }

    /// The position of a copper layer in stack order, if present.
    pub fn layer_index(&self, layer: LayerId) -> Option<usize> {
        self.layers.iter().position(|candidate| *candidate == layer)
    }

    /// The pieces of board item `id`, in key order; a zone gives its kept fill polygons.
    pub fn pieces(&self, id: ItemId) -> impl Iterator<Item = &Piece> + '_ {
        let (fixed, tiles): (&[Piece], &[TilePieces]) = match self.items.get(id.0) {
            Some(Entry::Item(pieces)) => (&pieces[..], &[][..]),
            Some(Entry::Zone(zone)) => (&[][..], &zone.tiles[..]),
            None => (&[][..], &[][..]),
        };
        fixed.iter().chain(tiles.iter().flat_map(|tile| tile.pieces.iter()))
    }

    /// The pieces of one connectivity item: a fill polygon directly, other items by a filter
    /// over their (few) pieces.
    pub fn copper_of(&self, c: ConnItem) -> impl Iterator<Item = &Piece> + '_ {
        let direct = match c {
            ConnItem::Zone { item, layer, tile, ord } => {
                self.get(PKey { item: item.0, tile: Some((layer, tile)), rank: ord })
            }
            _ => None,
        };
        let scan = (!matches!(c, ConnItem::Zone { .. })).then(|| self.pieces(c.item()));
        direct
            .into_iter()
            .chain(scan.into_iter().flatten().filter(move |piece| piece.copper().is_some_and(|copper| copper.item == c)))
    }

    /// The island (its smallest member polygon) of a zone's fill polygon.
    pub fn island(&self, c: ConnItem) -> Option<PolyRef> {
        let ConnItem::Zone { item, layer, tile, ord } = c else { return None };
        let tile = self.tile(item, layer, tile)?;
        let i = tile.pieces.binary_search_by_key(&ord, |piece| piece.rank).ok()?;
        tile.island.get(i).copied()
    }

    fn tile(&self, item: ItemId, layer: LayerId, key: TileKey) -> Option<&TilePieces> {
        let Some(Entry::Zone(zone)) = self.items.get(item.0) else { return None };
        let i = zone.tiles.binary_search_by_key(&(layer, key), |tile| (tile.layer, tile.key)).ok()?;
        Some(&zone.tiles[i])
    }

    /// The outline steps in rank order, each with no board item.
    pub fn outline(&self) -> &[Piece] {
        &self.outline
    }

    /// Find a piece by its history-independent identity.
    pub fn get(&self, key: PKey) -> Option<&Piece> {
        if key.item == u32::MAX {
            return self.outline.get(key.rank as usize);
        }
        match (self.items.get(key.item)?, key.tile) {
            (Entry::Item(pieces), None) => pieces.get(key.rank as usize),
            (Entry::Zone(_), Some((layer, tile))) => {
                let tile = self.tile(ItemId(key.item), layer, tile)?;
                let i = tile.pieces.binary_search_by_key(&key.rank, |piece| piece.rank).ok()?;
                Some(&tile.pieces[i])
            }
            _ => None,
        }
    }

    /// The number of pieces, including outline steps and pieces with empty boxes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there are no pieces, including outline steps.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Visit pieces on `layer` whose closed boxes meet `area`, in `PKey` order, until a break.
    pub fn query(&self, layer: LayerId, area: BBox, mut f: impl FnMut(&Piece) -> ControlFlow<()>) {
        let Some(index) = self.layer_index(layer) else { return };
        let mut keys = Vec::new();
        self.quads[index].query(area, |_, key| {
            keys.push(*key);
            ControlFlow::Continue(())
        });
        keys.sort_unstable();
        for key in keys {
            if let Some(piece) = self.get(key) {
                if f(piece).is_break() {
                    break;
                }
            }
        }
    }

    /// Visit the non-fill, non-edge pieces on `layer` that the zone fill may select for the window `near`:
    /// those whose closed boxes meet `near.inflate(r)`, and pads whose own thermal relief box meets `near`;
    /// in `PKey` order, each once, until a break.
    pub fn query_fill(
        &self,
        layer: LayerId,
        near: BBox,
        r: Nm,
        mut f: impl FnMut(&Piece) -> ControlFlow<()>,
    ) {
        let Some(index) = self.layer_index(layer) else { return };
        let mut keys = Vec::new();
        self.quads[index].query(near.inflate(r), |_, key| {
            keys.push(*key);
            ControlFlow::Continue(())
        });
        self.reliefs[index].query(near, |_, key| {
            keys.push(*key);
            ControlFlow::Continue(())
        });
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            if let Some(piece) = self.get(key) {
                if matches!(&piece.kind, PieceKind::Fill(_) | PieceKind::Edge(_)) {
                    continue;
                }
                if f(piece).is_break() {
                    break;
                }
            }
        }
    }

    /// Every piece: items in id order, each in key order, followed by the outline.
    pub fn iter(&self) -> impl Iterator<Item = &Piece> + '_ {
        self.items.iter().flat_map(|(id, _)| self.pieces(ItemId(id))).chain(self.outline.iter())
    }

    fn add_item(&mut self, board: &Board, id: ItemId) {
        let mut pieces = Vec::new();
        match board.locate(id) {
            Some((ItemKind::Zone, _)) | None => return,
            Some((kind, i)) => item_pieces(board, id, kind, i, &mut pieces),
        }
        // An offered ring an indexed fill reaches is copper.
        for piece in &mut pieces {
            let reached = match &piece.kind {
                PieceKind::Offer(ring) if self.reached.contains_key(&(id, ring.layer)) => Some(ring.clone()),
                _ => None,
            };
            if let Some(ring) = reached {
                piece.kind = PieceKind::Copper(ring);
            }
        }
        if !pieces.is_empty() {
            insert_quads(&self.layers, &mut self.quads, &mut self.reliefs, &pieces);
            self.len += pieces.len();
            self.items.set(id.0, Entry::Item(pieces.into()));
        }
    }

    fn remove_pieces(&mut self, pieces: &[Piece]) {
        self.len -= pieces.len();
        for piece in pieces.iter().filter(|piece| !piece.bbox.is_empty()) {
            for ((layer, quad), relief_quad) in self.layers.iter().zip(&mut self.quads).zip(&mut self.reliefs) {
                if piece.layers.has(*layer) {
                    quad.remove(piece.bbox, &piece.key());
                    if let Some(relief) = piece.relief {
                        relief_quad.remove(piece.bbox.inflate(relief), &piece.key());
                    }
                }
            }
        }
    }

    // Tiles whose view hash (and the zone's net and class) match the previous entry keep their
    // pieces and quad entries; every other old tile leaves the quads and every other new one
    // enters them. Island labels are always taken from the new fill (they change without it).
    fn set_zone(&mut self, board: &Board, zone: &Zone, fill: Option<&ZoneFill>, change: &mut IndexChange) {
        let net = zone.net;
        let class = crate::zone::class_of(board, net);
        let old = match self.items.remove(zone.id.0) {
            Some(Entry::Zone(old)) => Some(old),
            Some(Entry::Item(pieces)) => {
                self.remove_pieces(&pieces);
                change.items.push(zone.id);
                None
            }
            None => None,
        };
        let reusable = old.as_ref().filter(|old| old.net == net && old.class == class);
        let mut tiles = Vec::new();
        for layer in fill.map_or(&[][..], |fill| fill.layers.as_slice()) {
            for (tile, labels) in layer.tiles.iter().zip(&layer.labels) {
                let previous = reusable.and_then(|old| {
                    let i = old.tiles.binary_search_by_key(&(layer.layer, tile.key), |t| (t.layer, t.key)).ok()?;
                    Some(&old.tiles[i]).filter(|t| t.view_hash == labels.view_hash)
                });
                let island: Arc<[PolyRef]> =
                    labels.island.iter().zip(&labels.kept).filter(|(_, kept)| **kept).map(|(r, _)| *r).collect();
                let pieces = match previous {
                    Some(previous) => previous.pieces.clone(),
                    None => tile_pieces(zone, class, layer.layer, tile, labels).into(),
                };
                tiles.push(TilePieces { layer: layer.layer, key: tile.key, view_hash: labels.view_hash, pieces, island });
            }
        }
        tiles.sort_by_key(|tile| (tile.layer, tile.key));
        if let Some(old) = &old {
            for tile in &old.tiles {
                let kept = tiles
                    .binary_search_by_key(&(tile.layer, tile.key), |t| (t.layer, t.key))
                    .is_ok_and(|i| Arc::ptr_eq(&tiles[i].pieces, &tile.pieces));
                if !kept {
                    self.remove_pieces(&tile.pieces);
                    change.tiles.push((zone.id, tile.layer, tile.key));
                }
            }
        }
        for tile in &tiles {
            let previous = old.as_ref().and_then(|old| {
                let i = old.tiles.binary_search_by_key(&(tile.layer, tile.key), |t| (t.layer, t.key)).ok()?;
                Some(&old.tiles[i]).filter(|old| Arc::ptr_eq(&old.pieces, &tile.pieces))
            });
            match previous {
                Some(previous) => {
                    if previous.island != tile.island {
                        change.relabeled.push((zone.id, tile.layer, tile.key));
                    }
                }
                None => {
                    insert_quads(&self.layers, &mut self.quads, &mut self.reliefs, &tile.pieces);
                    self.len += tile.pieces.len();
                    change.tiles.push((zone.id, tile.layer, tile.key));
                }
            }
        }
        // A fill with no tiles has no copper, so it reaches nothing.
        let reached = match fill {
            Some(fill) if !tiles.is_empty() => board.reached_by(fill),
            _ => Vec::new(),
        };
        self.move_reach(board, old.as_ref().map_or(&[][..], |old| &old.reached[..]), &reached, change);
        if !tiles.is_empty() {
            self.items.set(zone.id.0, Entry::Zone(Arc::new(ZonePieces { net, class, tiles, reached })));
        }
    }

    /// Move one zone's reach from `old` to `new` (sorted (via, layer) lists) and re-piece each via
    /// whose ring on a layer became, or stopped being, reached by any indexed fill.
    fn move_reach(&mut self, board: &Board, old: &[(ItemId, LayerId)], new: &[(ItemId, LayerId)], change: &mut IndexChange) {
        if old == new {
            return;
        }
        let mut flipped = Vec::new();
        {
            let reached = Arc::make_mut(&mut self.reached);
            for pair in old.iter().filter(|pair| new.binary_search(pair).is_err()) {
                if let Some(count) = reached.get_mut(pair) {
                    *count -= 1;
                    if *count == 0 {
                        reached.remove(pair);
                        flipped.push(pair.0);
                    }
                }
            }
            for pair in new.iter().filter(|pair| old.binary_search(pair).is_err()) {
                let count = reached.entry(*pair).or_insert(0);
                *count += 1;
                if *count == 1 {
                    flipped.push(pair.0);
                }
            }
        }
        flipped.sort_unstable();
        flipped.dedup();
        for via in flipped {
            if let Some(Entry::Item(previous)) = self.items.remove(via.0) {
                self.remove_pieces(&previous);
            }
            self.add_item(board, via);
            change.items.push(via);
        }
    }
}

fn fill_of(fills: &[ZoneFill], zone: ItemId) -> Option<&ZoneFill> {
    // Fills come sorted by zone id (`fill_all`); fall back to a scan for any other order.
    match fills.binary_search_by_key(&zone, |fill| fill.zone) {
        Ok(i) => Some(&fills[i]),
        Err(_) => fills.iter().find(|fill| fill.zone == zone),
    }
}

impl PartialEq for PieceIndex {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.layers == other.layers
            && self.outline == other.outline
            && self.items == other.items
            && self.quads == other.quads
            && self.reliefs == other.reliefs
            && self.len == other.len
    }
}

impl fmt::Debug for PieceIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PieceIndex").field("pieces", &self.len).field("generation", &self.generation).finish()
    }
}

fn insert_quads(layers: &[LayerId], quads: &mut [PQuad<PKey>], reliefs: &mut [PQuad<PKey>], pieces: &[Piece]) {
    for piece in pieces.iter().filter(|piece| !piece.bbox.is_empty()) {
        for ((layer, quad), relief_quad) in layers.iter().zip(quads.iter_mut()).zip(reliefs.iter_mut()) {
            if piece.layers.has(*layer) {
                quad.insert(piece.bbox, piece.key());
                if let Some(relief) = piece.relief {
                    relief_quad.insert(piece.bbox.inflate(relief), piece.key());
                }
            }
        }
    }
}

fn item_ids(board: &Board) -> impl Iterator<Item = ItemId> + '_ {
    board
        .placed
        .iter()
        .map(|item| item.id)
        .chain(board.tracks.iter().map(|item| item.id))
        .chain(board.vias.iter().map(|item| item.id))
        .chain(board.holes.iter().map(|item| item.id))
        .chain(board.keepouts.iter().map(|item| item.id))
        .chain(board.cutouts.iter().map(|item| item.id))
}

fn edge_steps(path: &Path, out: &mut Vec<Shape>) {
    for (a, seg) in path.steps() {
        out.push(match seg {
            Seg::Line(b) => Shape::Segment { a, b, w: 0 },
            Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w: 0 },
        });
    }
}

fn outline_pieces(board: &Board) -> Vec<Piece> {
    let layers = board.copper_set();
    let mut steps = Vec::new();
    for contour in &board.outline {
        edge_steps(&contour.path, &mut steps);
    }
    steps
        .into_iter()
        .enumerate()
        .map(|(rank, shape)| Piece::new(PieceKind::Edge(shape), None, rank as u32, layers))
        .collect()
}

// Fixed pieces follow the router's derivation and rank order; zone pieces come from fills below.
fn item_pieces(board: &Board, id: ItemId, kind: ItemKind, i: usize, out: &mut Vec<Piece>) {
    let copper_layers = board.copper_set();
    let n = board.copper_count();
    let start = out.len();
    let push = |kind: PieceKind, layers: LayerSet, out: &mut Vec<Piece>| {
        let rank = (out.len() - start) as u32;
        out.push(Piece::new(kind, Some(id), rank, layers));
    };
    let mut copper = Vec::new();
    board.item_copper(id, &mut copper);
    for c in copper {
        let layers = LayerSet::EMPTY.with(c.layer);
        let relief = match (kind, c.item) {
            (ItemKind::Placed, ConnItem::Pad { pad, .. }) => pad_relief(&board.placed[i].footprint.pads[pad.ix()]),
            _ => None,
        };
        let mut piece = Piece::new(PieceKind::Copper(c), Some(id), (out.len() - start) as u32, layers);
        piece.relief = relief;
        out.push(piece);
    }
    match kind {
        ItemKind::Placed => {
            let placed = &board.placed[i];
            for pi in 0..placed.footprint.pads.len() {
                if let Some(hole) = placed.drill_hole(PadIx(pi as u16)) {
                    push(
                        PieceKind::Drill {
                            item: hole.item, net: hole.net, shape: hole.shape(), plated: hole.plated,
                        },
                        copper_layers,
                        out,
                    );
                }
            }
            for keepout in &placed.footprint.keepouts {
                let layers = copper_layers
                    .iter()
                    .filter(|layer| keepout.layers.has(placed.fp_layer(*layer, n)))
                    .fold(LayerSet::EMPTY, |layers, layer| layers.with(layer));
                if !layers.is_empty() {
                    let area = placed.shape_to_board(&Shape::Poly(keepout.area.clone()));
                    push(PieceKind::Keepout { area, forbid: keepout.forbid }, layers, out);
                }
            }
        }
        ItemKind::Via => {
            let via = &board.vias[i];
            let mut offers = Vec::new();
            board.offer_copper(via, &mut offers);
            for ring in offers {
                let layers = LayerSet::EMPTY.with(ring.layer);
                push(PieceKind::Offer(ring), layers, out);
            }
            let hole = via.drill_hole();
            push(
                PieceKind::Drill { item: hole.item, net: hole.net, shape: hole.shape(), plated: hole.plated },
                via.span_layers(copper_layers),
                out,
            );
        }
        ItemKind::Hole => {
            let hole = board.holes[i].drill_hole();
            push(
                PieceKind::Drill { item: hole.item, net: hole.net, shape: hole.shape(), plated: hole.plated },
                copper_layers,
                out,
            );
        }
        ItemKind::Keepout => {
            let keepout = &board.keepouts[i];
            let layers = keepout.layers.intersect(copper_layers);
            if !layers.is_empty() && crate::drc::reaches_copper(keepout.z) {
                push(
                    PieceKind::Keepout { area: Shape::Poly(keepout.area.clone()), forbid: keepout.forbid },
                    layers,
                    out,
                );
            }
        }
        ItemKind::Cutout => {
            let mut steps = Vec::new();
            edge_steps(&board.cutouts[i].contour.path, &mut steps);
            for shape in steps {
                push(PieceKind::Edge(shape), copper_layers, out);
            }
        }
        _ => {}
    }
}

fn tile_pieces(zone: &Zone, class: ClassId, layer: LayerId, tile: &FillTile, labels: &TileLabels) -> Vec<Piece> {
    let mut out = Vec::new();
    for (ord, poly) in tile.polys.iter().enumerate() {
        if !labels.kept[ord] {
            continue;
        }
        let ord = ord as u32;
        let copper = Copper {
            item: ConnItem::Zone { item: zone.id, layer, tile: tile.key, ord },
            net: zone.net,
            class,
            layer,
            shape: Shape::Poly(poly.clone()),
        };
        let mut piece = Piece::new(PieceKind::Fill(copper), Some(zone.id), ord, LayerSet::EMPTY.with(layer));
        piece.tile = Some((layer, tile.key));
        piece.grid = Some(Arc::new(PolyGrid::new(poly.clone())));
        out.push(piece);
    }
    out
}
