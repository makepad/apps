//! The routing world (E6 1.1): every obstacle and every piece of fixed copper of a board, one
//! spatial index per copper layer, updated by `BoardDelta`, with cheap overlay branches for
//! speculation (a head, a shove wave, an autorouter transaction).
//!
//! - `RBase::build(board)` loads the copper of placed pads, tracks, vias and plated holes (one
//!   piece per pad and layer, per track segment, per via layer), every drilled hole, the outline
//!   and cutout steps, and board and footprint keepouts that reach the surface (a raised height
//!   band forbids bodies, not copper). Zone fills are not obstacles: they are
//!   derived and yield to tracks (E6 1.1).
//! - `RBase::update(board, delta)` removes and re-adds the delta's items: O(d log n) amortised.
//!   The property tests hold it to `build`: the same multiset of items, in the canonical order.
//! - `RBase::tracks_on(item)`: the tracks with an end on a part, via, hole or track, kept with the
//!   pieces, so a lookup by anchor never scans the board's tracks.
//! - A `World` is the base plus an `Overlay` (removed base pieces, added pieces with their own
//!   small index). Cloning an overlay is a branch of a branch. No lock: the route worker owns the
//!   base; pool tasks read `Arc<RBase>` snapshots. An added piece can be hidden and shown again in
//!   place (`hide`, `show`), and a world can note the areas it is asked about (`record`), so a
//!   caller can tell which of its earlier answers a change could alter.
//!
//! Order: an `RId` is a slab slot and depends on the base's history. Everything that decides a
//! route orders items by `RItem::order()` (board item, then rank among its pieces), which does
//! not, so a world updated by deltas answers exactly as a fresh one (E6 9).
//!
//! Legality is the board's: an item is tested with `fab_elec_board::check_to` through
//! `RItem::obstacle()`; there is no rule logic here.
//!
//! Cost: build O(n log n); update O(d log n); a query O(log n + k); memory O(n).

use fab_elec_board::{Board, BoardDelta, ConnItem, Copper, ItemKind, Obstacle, Origin, Track};
use fab_elec_core::{BBox, Index, ItemId, LayerId, LayerSet, NetId, Nm, PadIx, Seg, Shape, P};
use fab_elec_parts::Forbid;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

/// A slot in the base's item slab.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RId(pub u32);

/// What a world item is.
#[derive(Clone, PartialEq, Debug)]
pub enum RPiece {
    /// Fixed copper on one layer: a pad on that layer, a track segment, a via or plated hole ring.
    Copper(Copper),
    /// A step of the outline or of a cutout (width 0).
    Edge(Shape),
    /// A drilled hole (its circle, or its slot as an oval), with the owning item and its net.
    Drill { item: ConnItem, net: Option<NetId>, shape: Shape },
    /// A keepout area.
    Keepout { area: Shape, forbid: Forbid },
}

/// Where an item came from, for the hold rules (E6 1.1, E4 Q7).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Fixity {
    Authored,
    Solved,
    /// From an undetached block template.
    Block,
    Derived,
}

impl Fixity {
    /// Whether copper with this origin and lock stays as it is when its own net is routed: all
    /// but unlocked solver copper (`RouteProblem::owned` for a net in scope).
    pub fn holds(origin: &Origin, locked: bool) -> bool {
        locked || Fixity::of(origin) != Fixity::Solved
    }

    fn of(origin: &Origin) -> Fixity {
        match origin {
            Origin::Authored => Fixity::Authored,
            Origin::Solved { .. } => Fixity::Solved,
            Origin::Block { detached: true, .. } => Fixity::Authored,
            Origin::Block { .. } => Fixity::Block,
            Origin::Derived { .. } => Fixity::Derived,
        }
    }
}

/// One world item.
#[derive(Clone, PartialEq, Debug)]
pub struct RItem {
    pub piece: RPiece,
    /// The board item it belongs to; `None` for the outline.
    pub item: Option<ItemId>,
    /// Its rank among its item's pieces.
    pub rank: u32,
    /// The copper layers it is on.
    pub layers: LayerSet,
    pub bbox: BBox,
    pub fixity: Fixity,
    pub locked: bool,
    /// The base piece of its item whose shape this piece keeps (that piece's rank), and how far it
    /// is moved from it: a base piece itself with zero, a piece carried with a moved item by that
    /// item's translation, a track segment kept where it was (or cut short along itself) with zero.
    /// `None` for new copper. The fault rule reads it (`verify`): copper that keeps its shape may
    /// keep a fault it had.
    pub kept: Option<(u32, P)>,
}

impl RItem {
    /// This piece moved by `by`: its shape and box.
    pub fn translated(&self, by: P) -> RItem {
        let mut it = self.clone();
        let shape = match &mut it.piece {
            RPiece::Copper(c) => &mut c.shape,
            RPiece::Edge(s) | RPiece::Drill { shape: s, .. } | RPiece::Keepout { area: s, .. } => s,
        };
        *shape = shape.translated(by);
        it.bbox = shape.bbox();
        it.kept = self.kept.map(|(rank, moved)| (rank, moved.add(by)));
        it
    }
    /// The history-independent order of world items.
    pub fn order(&self) -> (u32, u32) {
        (self.item.map_or(u32::MAX, |i| i.0), self.rank)
    }
    pub fn shape(&self) -> &Shape {
        match &self.piece {
            RPiece::Copper(c) => &c.shape,
            RPiece::Edge(s) | RPiece::Drill { shape: s, .. } | RPiece::Keepout { area: s, .. } => s,
        }
    }
    /// The item as what copper is tested against (`fab_elec_board::check_to`).
    pub fn obstacle(&self) -> Obstacle<'_> {
        match &self.piece {
            RPiece::Copper(c) => Obstacle::Copper(c),
            RPiece::Edge(s) => Obstacle::Edge(s),
            RPiece::Drill { item, net, shape } => Obstacle::Drill { item: *item, net: *net, shape },
            RPiece::Keepout { area, forbid } => Obstacle::Keepout { area, forbid: *forbid },
        }
    }
    /// The net whose copper this is (copper and drills).
    pub fn net(&self) -> Option<NetId> {
        match &self.piece {
            RPiece::Copper(c) => c.net,
            RPiece::Drill { net, .. } => *net,
            RPiece::Edge(_) | RPiece::Keepout { .. } => None,
        }
    }
}

/// Pad copper piece `it`'s box and the mask margin its pad sets itself, if it sets one. O(1).
fn pad_margin(board: &Board, it: &RItem) -> Option<(BBox, Nm)> {
    let RPiece::Copper(Copper { item: ConnItem::Pad { placed, pad }, .. }) = &it.piece else { return None };
    let margin = board.placed_item(*placed)?.footprint.pads.get(pad.ix())?.mask_margin?;
    Some((it.bbox, margin))
}

/// The routing world of one board.
#[derive(Clone)]
pub struct RBase {
    slots: Vec<Option<RItem>>,
    free: Vec<u32>,
    by_item: BTreeMap<ItemId, Vec<RId>>,
    /// The pieces of each part's pad (its copper and its drill), by part and pad.
    by_pad: BTreeMap<(ItemId, PadIx), Vec<RId>>,
    outline: Vec<RId>,
    layers: Vec<LayerId>,
    index: Vec<Index<RId>>,
    /// The tracks with an end on each item (the item an end's anchor names), by id.
    on: BTreeMap<ItemId, Vec<ItemId>>,
    /// The items each track's ends are on, to take the track out of `on` when it changes.
    ends: BTreeMap<ItemId, Vec<ItemId>>,
    /// Per placed item, the copper boxes of its pads that set a mask margin of their own, with
    /// that margin: how far a via drill near one can be from it and still be too close
    /// (`verify::reach_of`).
    margins: BTreeMap<ItemId, Vec<(BBox, Nm)>>,
    /// Per item and the outline, whether its pieces fault no other base piece (`verify`'s fault
    /// rule asks it before looking for a fault the base had).
    clean: CleanBits,
    generation: u64,
}

/// Per base item (by id) and the outline: 0 not yet known, 1 its pieces fault no other base
/// piece, 2 they do. Filled on first ask by whoever asks (the value is the same whoever fills it),
/// so a shared base needs no lock; any update of the base clears it all.
#[derive(Default)]
struct CleanBits {
    items: Vec<AtomicU8>,
    outline: AtomicU8,
}

impl CleanBits {
    /// All unknown, for the items of a board whose next id is `next_item`.
    fn new(next_item: u32) -> CleanBits {
        CleanBits { items: (0..next_item).map(|_| AtomicU8::new(0)).collect(), outline: AtomicU8::new(0) }
    }
}

impl Clone for CleanBits {
    fn clone(&self) -> CleanBits {
        CleanBits {
            items: self.items.iter().map(|b| AtomicU8::new(b.load(Relaxed))).collect(),
            outline: AtomicU8::new(self.outline.load(Relaxed)),
        }
    }
}

impl RBase {
    /// The world of `board`.
    pub fn build(board: &Board) -> RBase {
        let layers = board.stackup.copper();
        let mut base = RBase {
            slots: Vec::new(), free: Vec::new(), by_item: BTreeMap::new(), by_pad: BTreeMap::new(), outline: Vec::new(), layers, index: Vec::new(),
            on: BTreeMap::new(), ends: BTreeMap::new(), margins: BTreeMap::new(), clean: CleanBits::new(board.next_item),
            generation: board.generation,
        };
        for track in &board.tracks {
            base.anchor(track);
        }
        let mut entries: Vec<Vec<(BBox, RId)>> = vec![Vec::new(); base.layers.len()];
        let mut all: Vec<RItem> = outline_pieces(board);
        let n_outline = all.len();
        for id in item_ids(board) {
            pieces_of(board, id, &mut all);
        }
        for (k, it) in all.into_iter().enumerate() {
            let rid = RId(base.slots.len() as u32);
            for (li, l) in base.layers.iter().enumerate() {
                if it.layers.has(*l) {
                    entries[li].push((it.bbox, rid));
                }
            }
            if let (Some(item), Some(margin)) = (it.item, pad_margin(board, &it)) {
                base.margins.entry(item).or_default().push(margin);
            }
            match it.item {
                Some(item) => base.by_item.entry(item).or_default().push(rid),
                None if k < n_outline => base.outline.push(rid),
                None => {}
            }
            if let Some(pad) = pad_of(&it) {
                base.by_pad.entry(pad).or_default().push(rid);
            }
            base.slots.push(Some(it));
        }
        base.index = entries.into_iter().map(Index::build).collect();
        base
    }

    /// Bring the world to `board`, which `delta` led to from the board this world shows.
    pub fn update(&mut self, board: &Board, delta: &BoardDelta) {
        if delta.global || board.stackup.copper() != self.layers {
            *self = RBase::build(board);
            return;
        }
        let mut fresh = Vec::new();
        for id in delta.ids() {
            for item in self.ends.remove(&id).unwrap_or_default() {
                if let Some(tracks) = self.on.get_mut(&item) {
                    tracks.retain(|t| *t != id);
                    if tracks.is_empty() {
                        self.on.remove(&item);
                    }
                }
            }
            if let Some((ItemKind::Track, i)) = board.locate(id) {
                self.anchor(&board.tracks[i]);
            }
            self.margins.remove(&id);
            for rid in self.by_item.remove(&id).unwrap_or_default() {
                self.remove_slot(rid);
            }
            let pads: Vec<(ItemId, PadIx)> = self.by_pad.range((id, PadIx(0))..=(id, PadIx(u16::MAX))).map(|(&k, _)| k).collect();
            for pad in pads {
                self.by_pad.remove(&pad);
            }
            fresh.clear();
            pieces_of(board, id, &mut fresh);
            for it in fresh.drain(..) {
                if let Some(margin) = pad_margin(board, &it) {
                    self.margins.entry(id).or_default().push(margin);
                }
                let pad = pad_of(&it);
                let rid = self.insert_slot(it);
                self.by_item.entry(id).or_default().push(rid);
                if let Some(pad) = pad {
                    self.by_pad.entry(pad).or_default().push(rid);
                }
            }
        }
        self.clean = CleanBits::new(board.next_item);
        self.generation = board.generation;
    }

    /// `track` in `on`, under each item its ends are on. O(log n).
    fn anchor(&mut self, track: &Track) {
        let mut items: Vec<ItemId> = track.ends.iter().filter_map(|end| end.item()).collect();
        items.sort_unstable();
        items.dedup();
        for &item in &items {
            let tracks = self.on.entry(item).or_default();
            if let Err(k) = tracks.binary_search(&track.id) {
                tracks.insert(k, track.id);
            }
        }
        if !items.is_empty() {
            self.ends.insert(track.id, items);
        }
    }

    /// The tracks with an end on `item` (a part's pad, a via, a hole or a track), by id. O(log n).
    pub fn tracks_on(&self, item: ItemId) -> &[ItemId] {
        self.on.get(&item).map_or(&[], Vec::as_slice)
    }

    fn insert_slot(&mut self, it: RItem) -> RId {
        let rid = match self.free.pop() {
            Some(s) => RId(s),
            None => {
                self.slots.push(None);
                RId(self.slots.len() as u32 - 1)
            }
        };
        for (li, l) in self.layers.iter().enumerate() {
            if it.layers.has(*l) {
                self.index[li].insert(it.bbox, rid);
            }
        }
        self.slots[rid.0 as usize] = Some(it);
        rid
    }

    fn remove_slot(&mut self, rid: RId) {
        let Some(it) = self.slots[rid.0 as usize].take() else { return };
        for (li, l) in self.layers.iter().enumerate() {
            if it.layers.has(*l) {
                self.index[li].remove(it.bbox, rid);
            }
        }
        self.free.push(rid.0);
    }

    /// The board generation the world shows.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// The copper box of every pad that sets a mask margin of its own, with that margin.
    pub fn pad_margins(&self) -> impl Iterator<Item = (BBox, Nm)> + '_ {
        self.margins.values().flatten().copied()
    }
    /// The board's copper layers, in stack order.
    pub fn layers(&self) -> &[LayerId] {
        &self.layers
    }
    pub fn layer_index(&self, l: LayerId) -> Option<usize> {
        self.layers.iter().position(|x| *x == l)
    }
    pub fn get(&self, rid: RId) -> Option<&RItem> {
        self.slots.get(rid.0 as usize).and_then(|s| s.as_ref())
    }
    pub fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The pieces of board item `id`.
    pub fn pieces(&self, id: ItemId) -> &[RId] {
        self.by_item.get(&id).map_or(&[], |v| v.as_slice())
    }
    /// The outline's pieces.
    pub fn outline(&self) -> &[RId] {
        &self.outline
    }
    /// The bit that says whether the pieces of `item` (`None`: the outline) fault no other base
    /// piece (`CleanBits`); `None` for an item the base was not built or updated with.
    pub(crate) fn clean_bit(&self, item: Option<ItemId>) -> Option<&AtomicU8> {
        match item {
            Some(id) => self.clean.items.get(id.0 as usize),
            None => Some(&self.clean.outline),
        }
    }
    /// The pieces of pad `pad` of part `placed`: its copper and its drill.
    pub fn pad_pieces(&self, placed: ItemId, pad: PadIx) -> &[RId] {
        self.by_pad.get(&(placed, pad)).map_or(&[], |v| v.as_slice())
    }
    /// Every item on `layer` whose box meets `area` (index order: sort by `RItem::order` when the
    /// order decides anything).
    pub fn query<'s>(&'s self, layer: LayerId, area: BBox, mut f: impl FnMut(RId, &'s RItem) -> ControlFlow<()>) {
        let Some(li) = self.layer_index(layer) else { return };
        self.index[li].query(area, |_, rid| match self.get(rid) {
            Some(it) => f(rid, it),
            None => ControlFlow::Continue(()),
        });
    }
    /// Every item in canonical order (tests and full passes).
    pub fn sorted(&self) -> Vec<&RItem> {
        let mut v: Vec<&RItem> = self.slots.iter().flatten().collect();
        v.sort_by(|a, b| a.order().cmp(&b.order()).then_with(|| format!("{a:?}").cmp(&format!("{b:?}"))));
        v
    }
}

/// Copper-bearing and obstacle-bearing board items, in list order.
fn item_ids(board: &Board) -> Vec<ItemId> {
    board
        .placed
        .iter()
        .map(|x| x.id)
        .chain(board.tracks.iter().map(|x| x.id))
        .chain(board.vias.iter().map(|x| x.id))
        .chain(board.holes.iter().map(|x| x.id))
        .chain(board.keepouts.iter().map(|x| x.id))
        .chain(board.cutouts.iter().map(|x| x.id))
        .collect()
}

fn edge_steps(path: &fab_elec_core::Path, out: &mut Vec<Shape>) {
    for (a, seg) in path.steps() {
        out.push(match seg {
            Seg::Line(b) => Shape::Segment { a, b, w: 0 },
            Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w: 0 },
        });
    }
}

/// The part and pad a piece is part of, for a pad's copper or drill.
fn pad_of(it: &RItem) -> Option<(ItemId, PadIx)> {
    match &it.piece {
        RPiece::Copper(Copper { item: ConnItem::Pad { placed, pad }, .. }) | RPiece::Drill { item: ConnItem::Pad { placed, pad }, .. } => Some((*placed, *pad)),
        _ => None,
    }
}

fn outline_pieces(board: &Board) -> Vec<RItem> {
    let copper = board.copper_set();
    let mut steps = Vec::new();
    for c in &board.outline {
        edge_steps(&c.path, &mut steps);
    }
    steps
        .into_iter()
        .enumerate()
        .map(|(k, s)| RItem { bbox: s.bbox(), piece: RPiece::Edge(s), item: None, rank: k as u32, layers: copper, fixity: Fixity::Derived, locked: true, kept: Some((k as u32, P::ZERO)) })
        .collect()
}

/// The drill of pad `pi` of `p` as a board shape: a circle, or an oval for a slot.
/// The world pieces of board item `id`, appended to `out` in rank order.
fn pieces_of(board: &Board, id: ItemId, out: &mut Vec<RItem>) {
    let Some((kind, i)) = board.locate(id) else { return };
    let copper_layers = board.copper_set();
    let n = board.copper_count();
    let (fixity, locked) = match kind {
        ItemKind::Placed => (Fixity::of(&board.placed[i].origin), board.placed[i].locked),
        ItemKind::Track => (Fixity::of(&board.tracks[i].origin), board.tracks[i].locked),
        ItemKind::Via => (Fixity::of(&board.vias[i].origin), board.vias[i].locked),
        ItemKind::Hole => (Fixity::of(&board.holes[i].origin), true),
        ItemKind::Keepout => (Fixity::of(&board.keepouts[i].origin), true),
        ItemKind::Cutout => (Fixity::of(&board.cutouts[i].origin), true),
        _ => return,
    };
    let start = out.len();
    let push = |piece: RPiece, layers: LayerSet, out: &mut Vec<RItem>| {
        let bbox = match &piece {
            RPiece::Copper(c) => c.shape.bbox(),
            RPiece::Edge(s) | RPiece::Drill { shape: s, .. } | RPiece::Keepout { area: s, .. } => s.bbox(),
        };
        let rank = (out.len() - start) as u32;
        out.push(RItem { piece, item: Some(id), rank, layers, bbox, fixity, locked, kept: Some((rank, P::ZERO)) });
    };
    let mut copper = Vec::new();
    board.item_copper(id, &mut copper);
    for c in copper {
        let l = LayerSet::EMPTY.with(c.layer);
        push(RPiece::Copper(c), l, out);
    }
    match kind {
        ItemKind::Placed => {
            let p = &board.placed[i];
            for pi in 0..p.footprint.pads.len() {
                if let Some(h) = p.drill_hole(PadIx(pi as u16)) {
                    push(RPiece::Drill { item: h.item, net: h.net, shape: h.shape() }, copper_layers, out);
                }
            }
            for k in &p.footprint.keepouts {
                let layers = copper_layers.iter().filter(|l| k.layers.has(p.fp_layer(*l, n))).fold(LayerSet::EMPTY, |s, l| s.with(l));
                if !layers.is_empty() {
                    push(RPiece::Keepout { area: p.shape_to_board(&Shape::Poly(k.area.clone())), forbid: k.forbid }, layers, out);
                }
            }
        }
        ItemKind::Via => {
            let v = &board.vias[i];
            // An offered ring obstructs like copper: a foreign net keeps clear of a ring the next
            // refill can flash (`Board::rings`).
            let mut offers = Vec::new();
            board.offer_copper(v, &mut offers);
            for c in offers {
                let l = LayerSet::EMPTY.with(c.layer);
                push(RPiece::Copper(c), l, out);
            }
            let h = v.drill_hole();
            push(RPiece::Drill { item: h.item, net: h.net, shape: h.shape() }, v.span_layers(copper_layers), out);
        }
        ItemKind::Hole => {
            let h = &board.holes[i];
            let d = h.drill_hole();
            push(RPiece::Drill { item: d.item, net: d.net, shape: d.shape() }, copper_layers, out);
        }
        ItemKind::Keepout => {
            let k = &board.keepouts[i];
            let layers = k.layers.intersect(copper_layers);
            // A keepout whose height band starts above the surface forbids bodies, not copper.
            if !layers.is_empty() && fab_elec_board::drc::reaches_copper(k.z) {
                push(RPiece::Keepout { area: Shape::Poly(k.area.clone()), forbid: k.forbid }, layers, out);
            }
        }
        ItemKind::Cutout => {
            let mut steps = Vec::new();
            edge_steps(&board.cutouts[i].contour.path, &mut steps);
            for s in steps {
                push(RPiece::Edge(s), copper_layers, out);
            }
        }
        _ => {}
    }
}

/// A reference to a world item: in the base, or added by the overlay.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum WRef {
    Base(RId),
    Over(u32),
}

/// Speculative changes over a base: base pieces taken away, pieces added. A branch.
#[derive(Clone, Default)]
pub struct Overlay {
    removed: Vec<RId>,
    added: Vec<RItem>,
    index: Vec<Index<u32>>,
    /// The added pieces that are base pieces moved (`World::add_moved`): (added index, base piece,
    /// by), in the order added.
    moved: Vec<(u32, RId, P)>,
    /// Whether each added piece is hidden (`World::hide`): out of the index, as if not added. Empty
    /// while none ever was.
    hidden: Vec<bool>,
}

impl std::fmt::Debug for RBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RBase").field("items", &self.len()).field("generation", &self.generation).finish()
    }
}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overlay").field("removed", &self.removed.len()).field("added", &self.added.len()).finish()
    }
}

/// The base seen through an overlay.
#[derive(Clone, Debug)]
pub struct World<'a> {
    pub base: &'a RBase,
    pub over: Overlay,
    /// The areas queried, by layer, while recording (`record`).
    reads: Option<RefCell<Vec<(LayerId, BBox)>>>,
}

impl<'a> World<'a> {
    pub fn new(base: &'a RBase) -> World<'a> {
        let over = Overlay { removed: Vec::new(), added: Vec::new(), index: vec![Index::new(); base.layers.len()], moved: Vec::new(), hidden: Vec::new() };
        World { base, over, reads: None }
    }
    /// A world over `base` continuing `over` (a branch of a branch).
    pub fn with(base: &'a RBase, over: Overlay) -> World<'a> {
        World { base, over, reads: None }
    }

    /// Note every area queried from now on, with its layer, until `take_reads`.
    pub fn record(&mut self) {
        self.reads = Some(RefCell::new(Vec::new()));
    }
    /// The areas queried since `record`, and stop noting them. What a caller computed from this
    /// world reads nothing outside them: a change no piece of which meets them leaves it the same.
    pub fn take_reads(&mut self) -> Vec<(LayerId, BBox)> {
        self.reads.take().map(RefCell::into_inner).unwrap_or_default()
    }

    /// Hide added piece `r`: out of the index, as if never added, until `show`. Its reference
    /// stays its own. O(log pieces added).
    pub fn hide(&mut self, r: WRef) {
        let WRef::Over(k) = r else { return };
        if self.over.hidden.get(k as usize).copied().unwrap_or(false) {
            return;
        }
        let Some(it) = self.over.added.get(k as usize) else { return };
        for (li, l) in self.base.layers.iter().enumerate() {
            if it.layers.has(*l) {
                self.over.index[li].remove(it.bbox, k);
            }
        }
        if self.over.hidden.len() < self.over.added.len() {
            self.over.hidden.resize(self.over.added.len(), false);
        }
        self.over.hidden[k as usize] = true;
    }
    /// Show hidden piece `r` again, as it was. O(log pieces added).
    pub fn show(&mut self, r: WRef) {
        let WRef::Over(k) = r else { return };
        if !self.over.hidden.get(k as usize).copied().unwrap_or(false) {
            return;
        }
        self.over.hidden[k as usize] = false;
        let it = &self.over.added[k as usize];
        for (li, l) in self.base.layers.iter().enumerate() {
            if it.layers.has(*l) {
                self.over.index[li].insert(it.bbox, k);
            }
        }
    }
    fn is_hidden(&self, k: usize) -> bool {
        self.over.hidden.get(k).copied().unwrap_or(false)
    }

    /// Take every base piece of board item `id` out of this world.
    pub fn remove_item(&mut self, id: ItemId) {
        for rid in self.base.pieces(id) {
            if let Err(k) = self.over.removed.binary_search(rid) {
                self.over.removed.insert(k, *rid);
            }
        }
    }
    /// Take one base piece out of this world.
    pub fn remove(&mut self, rid: RId) {
        if let Err(k) = self.over.removed.binary_search(&rid) {
            self.over.removed.insert(k, rid);
        }
    }
    /// Take base pieces `rids` out of this world at once: one merge into the removed list, where
    /// `remove` moves the list once per piece. O(removed + rids log rids).
    pub fn remove_many(&mut self, rids: impl IntoIterator<Item = RId>) {
        let mut add: Vec<RId> = rids.into_iter().collect();
        if add.is_empty() {
            return;
        }
        add.sort_unstable();
        add.dedup();
        let old = std::mem::take(&mut self.over.removed);
        let mut merged = Vec::with_capacity(old.len() + add.len());
        let (mut i, mut j) = (0, 0);
        while i < old.len() && j < add.len() {
            match old[i].cmp(&add[j]) {
                std::cmp::Ordering::Less => {
                    merged.push(old[i]);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    merged.push(add[j]);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    merged.push(old[i]);
                    i += 1;
                    j += 1;
                }
            }
        }
        merged.extend_from_slice(&old[i..]);
        merged.extend_from_slice(&add[j..]);
        self.over.removed = merged;
    }
    /// Put a base piece taken out back in this world (itself, not a copy).
    pub fn restore(&mut self, rid: RId) {
        if let Ok(k) = self.over.removed.binary_search(&rid) {
            self.over.removed.remove(k);
        }
    }
    /// Put base pieces `rids` back at once: one pass over the removed list, where `restore` moves
    /// the list once per piece. O(removed × log rids + rids log rids).
    pub fn restore_many(&mut self, rids: impl IntoIterator<Item = RId>) {
        let mut rids: Vec<RId> = rids.into_iter().collect();
        if rids.is_empty() {
            return;
        }
        rids.sort_unstable();
        self.over.removed.retain(|rid| rids.binary_search(rid).is_err());
    }
    /// Take the pieces added for board item `id` out again, and return them; the base pieces it
    /// took out stay out. The overlay is rebuilt without them, the rest in the order added (their
    /// references change; moved pieces keep where they came from). O(pieces added).
    pub fn remove_added(&mut self, id: ItemId) -> Vec<RItem> {
        self.remove_added_by(|it| it == id)
    }
    /// `remove_added` for each of board items `ids` at once, with one rebuild: the same world as
    /// taking them out in turn, and their pieces in the order added. O(pieces added × log ids).
    pub fn remove_added_many(&mut self, ids: &[ItemId]) -> Vec<RItem> {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        self.remove_added_by(|it| ids.binary_search(&it).is_ok())
    }
    fn remove_added_by(&mut self, take: impl Fn(ItemId) -> bool) -> Vec<RItem> {
        let takes = |it: &RItem| it.item.is_some_and(&take);
        if !self.over.added.iter().enumerate().any(|(k, it)| !self.is_hidden(k) && takes(it)) {
            return Vec::new();
        }
        let old = std::mem::replace(&mut self.over, World::new(self.base).over);
        self.over.removed = old.removed;
        let mut moved = old.moved.into_iter().peekable();
        let mut taken = Vec::new();
        for (k, it) in old.added.into_iter().enumerate() {
            let from = moved.next_if(|&(m, ..)| m as usize == k).map(|(_, rid, by)| (rid, by));
            // A hidden piece is not in this world: dropped, not taken.
            if old.hidden.get(k).copied().unwrap_or(false) {
                continue;
            }
            if takes(&it) {
                taken.push(it);
                continue;
            }
            if let (WRef::Over(k), Some((rid, by))) = (self.add(it), from) {
                self.over.moved.push((k, rid, by));
            }
        }
        taken
    }
    /// Add a piece; returns its reference.
    pub fn add(&mut self, it: RItem) -> WRef {
        let k = self.over.added.len() as u32;
        for (li, l) in self.base.layers.iter().enumerate() {
            if it.layers.has(*l) {
                self.over.index[li].insert(it.bbox, k);
            }
        }
        self.over.added.push(it);
        WRef::Over(k)
    }
    /// Add base piece `rid` moved by `by` (a dragged item's piece at its new place), keeping
    /// where it came from so its hull can be the base piece's, moved; `None` when the base has no
    /// such piece. The base piece itself stays unless it is removed.
    pub fn add_moved(&mut self, rid: RId, by: P) -> Option<WRef> {
        let it = self.base.get(rid)?.translated(by);
        let r = self.add(it);
        if let WRef::Over(k) = r {
            self.over.moved.push((k, rid, by));
        }
        Some(r)
    }
    /// The base piece an added piece is, moved, and by how much (`add_moved`).
    pub fn moved_from(&self, r: WRef) -> Option<(RId, P)> {
        let WRef::Over(k) = r else { return None };
        let i = self.over.moved.binary_search_by_key(&k, |&(k, ..)| k).ok()?;
        let (_, rid, by) = self.over.moved[i];
        Some((rid, by))
    }
    pub fn get(&self, r: WRef) -> Option<&RItem> {
        match r {
            WRef::Base(rid) if self.over.removed.binary_search(&rid).is_err() => self.base.get(rid),
            WRef::Base(_) => None,
            WRef::Over(k) if self.is_hidden(k as usize) => None,
            WRef::Over(k) => self.over.added.get(k as usize),
        }
    }
    /// Every item of this world on `layer` whose box meets `area`: the base's minus the removed,
    /// then the added.
    pub fn query<'s>(&'s self, layer: LayerId, area: BBox, mut f: impl FnMut(WRef, &'s RItem) -> ControlFlow<()>) {
        self.note_read(layer, area);
        let mut stop = false;
        self.base.query(layer, area, |rid, it| {
            if self.over.removed.binary_search(&rid).is_ok() {
                return ControlFlow::Continue(());
            }
            let r = f(WRef::Base(rid), it);
            stop = r.is_break();
            r
        });
        if !stop {
            self.visit_added(layer, area, f);
        }
    }
    /// Each added piece on `layer` whose box meets `area`, stopping when `f` breaks. While
    /// recording, this notes the same read as `query`, even when the overlay has no piece there.
    pub fn query_added<'s>(&'s self, layer: LayerId, area: BBox, f: impl FnMut(WRef, &'s RItem) -> ControlFlow<()>) {
        self.note_read(layer, area);
        self.visit_added(layer, area, f);
    }
    fn note_read(&self, layer: LayerId, area: BBox) {
        if let Some(reads) = &self.reads {
            reads.borrow_mut().push((layer, area));
        }
    }
    fn visit_added<'s>(&'s self, layer: LayerId, area: BBox, mut f: impl FnMut(WRef, &'s RItem) -> ControlFlow<()>) {
        let Some(li) = self.base.layer_index(layer) else { return };
        self.over.index[li].query(area, |_, k| f(WRef::Over(k), &self.over.added[k as usize]));
    }
    /// `query` collected and put in the canonical order.
    pub fn query_sorted<'s>(&'s self, layer: LayerId, area: BBox) -> Vec<(WRef, &'s RItem)> {
        let mut v = Vec::new();
        self.query(layer, area, |r, it| {
            v.push((r, it));
            ControlFlow::Continue(())
        });
        v.sort_by_key(|&(r, it)| query_key(r, it));
        v
    }
    /// The overlay, to keep as a branch.
    pub fn into_overlay(self) -> Overlay {
        self.over
    }
}

/// Total query key so sorted lists of world pieces merge into the order of their union.
pub(crate) fn query_key(reference: WRef, item: &RItem) -> ((u32, u32), bool, WRef) {
    (item.order(), matches!(reference, WRef::Over(_)), reference)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::{Anchor, BoardOp, Field, Track};
    use fab_elec_core::{Path, P};

    fn fixture() -> Board {
        let design = fab_elec_fixtures::ledctl();
        design.units[0].board.as_deref().expect("the fixture has a board").clone()
    }

    #[test]
    fn update_equals_build_and_overlays_branch() {
        let mut board = fixture();
        let mut base = RBase::build(&board);
        assert_eq!(base.generation(), board.generation);
        let fresh = RBase::build(&board);
        assert_eq!(base.sorted(), fresh.sorted(), "build is deterministic");
        let n0 = base.len();
        assert!(n0 > 100, "the fixture's world has its pads, tracks, vias, holes and outline");
        // a scripted edit sequence: move a part (attached tracks follow), add a track, change a width, remove a via
        let part = board.placed[3].id;
        let net = board.tracks[0].net;
        let new_id = board.alloc_id();
        let a = P::new(5_000_000, 5_000_000);
        let ops = vec![
            BoardOp::Move { item: part, by: P::new(700_000, -300_000) },
            BoardOp::Add(fab_elec_board::Item::Track(Track {
                id: new_id,
                net,
                layer: LayerId::B_CU,
                w: 250_000,
                widths: None,
                path: Path::new(a).line(P::new(9_000_000, 5_000_000)),
                ends: [Anchor::Free, Anchor::Free],
                locked: false,
                origin: Origin::Authored,
            })),
            BoardOp::SetField { item: board.tracks[0].id, field: Field::Width(300_000) },
        ];
        let mut ops = ops;
        if let Some(v) = board.vias.first() {
            ops.push(BoardOp::Remove(v.id));
        }
        for op in &ops {
            let applied = board.apply(op).expect("the scripted op applies");
            base.update(&board, &applied.delta);
            let built = RBase::build(&board);
            assert_eq!(base.sorted(), built.sorted(), "update equals build after {op:?}");
            assert_eq!((&base.on, &base.ends), (&built.on, &built.ends), "the tracks by anchor follow the board after {op:?}");
        }
        assert_eq!(base.generation(), board.generation, "the world follows the board's generation");
        // the moved part's pads are at their new place in the index
        let p = board.placed_item(part).expect("still placed");
        let pad_at = p.pad_at(PadIx(0)).expect("pad 0");
        let mut found = false;
        base.query(LayerId::F_CU, BBox::point(pad_at), |_, it| {
            found |= matches!(&it.piece, RPiece::Copper(c) if c.item == ConnItem::Pad { placed: part, pad: PadIx(0) });
            ControlFlow::Continue(())
        });
        assert!(found || !p.pad_layers(PadIx(0), board.copper_set(), board.copper_count()).has(LayerId::F_CU), "the moved pad is found at its new place");
        // overlays: a branch sees removals and additions, the base and the parent branch do not change
        let mut w = World::new(&base);
        w.remove_item(new_id);
        let probe = BBox::new(a, a);
        let count = |w: &World| {
            let mut k = 0;
            w.query(LayerId::B_CU, probe, |_, it| {
                k += (it.item == Some(new_id)) as u32;
                ControlFlow::Continue(())
            });
            k
        };
        assert_eq!(count(&w), 0, "a removed item is gone from its branch");
        let mut branch = World::with(&base, w.over.clone());
        let added = branch.base.get(base.pieces(new_id)[0]).cloned().expect("the new track's piece");
        let r = branch.add(added);
        assert_eq!(count(&branch), 1, "an added piece is found in the branch");
        assert_eq!(count(&w), 0, "the parent branch is unchanged");
        assert_eq!(count(&World::new(&base)), 1, "the base is unchanged");
        assert!(branch.get(r).is_some() && branch.get(WRef::Base(base.pieces(new_id)[0])).is_none(), "get follows the overlay");
        let sorted = branch.query_sorted(LayerId::B_CU, board.outline_bbox());
        assert!(sorted.windows(2).all(|p| p[0].1.order() <= p[1].1.order()), "query_sorted is in canonical order");

        // an added item taken out again: its pieces come back, everything else stays, and a piece
        // added moved after it keeps where it came from
        let pad = base.pieces(board.placed[0].id)[0];
        let by = P::new(120_000, -40_000);
        branch.add_moved(pad, by).expect("the part has a piece");
        let taken = branch.remove_added(new_id);
        assert_eq!(taken, vec![base.get(base.pieces(new_id)[0]).cloned().expect("the new track's piece")], "the added piece comes back");
        assert_eq!(count(&branch), 0, "the piece taken out is gone, and its base piece stays out");
        assert_eq!(branch.moved_from(WRef::Over(0)), Some((pad, by)), "the moved piece keeps where it came from");
        assert_eq!(branch.get(WRef::Over(0)), base.get(pad).map(|it| it.translated(by)).as_ref(), "the moved piece is found by its new reference");
        assert!(branch.remove_added(new_id).is_empty(), "nothing more to take out");
    }
}
