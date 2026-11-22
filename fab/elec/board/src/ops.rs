//! Board edits (C3, E4 A13 and Q7): every change to a board is a `BoardOp` applied by the host.
//! One change's ops apply as one batch (`apply_batch`; `apply` is a batch of one), which returns
//! the batch's exact inverse and a `BoardDelta`, the input every engine's update form takes.
//!
//! Edit rules (`apply`, the path of views, tools, routers and the AI):
//! - an undetached `Block` item accepts only `Detach`;
//! - a `Derived` item accepts nothing (the error names its source: remove the constraint);
//! - `Move` and `SetField` on a `Solved` item adopt it (it becomes `Authored`); `Replace`,
//!   `Remove` and `Add` keep the origin they are given, so engines replace solved copper freely.
//!   Setting a reference label pose is cosmetic and keeps the placement's origin, too.
//!
//! `apply_host` skips the edit rules: the host's own stages (template instancing, derived
//! features) and undo and redo (`undo`) use it.
//!
//! A batch: the ops' own changes apply in order, with no snapping between them, so every item an
//! op adds, inserts or replaces ends the batch as its op wrote it (the last op naming it wins).
//! Then, once, against the final board: the ends anchored to an item absent at the end are freed;
//! every anchor a written track names must exist; one snap puts every anchored end of a track
//! the ops name, and of every track anchored to an item they name, on its anchor's final
//! position (the end segment stretches), then follows junction hosts that moved; every free end
//! the batch may have made attachable (a touched track's, or one on a via, hole or track it
//! touched) is attached as a reader attaches it, so no free end lies where a reload would attach
//! it. Ops that agree with their anchors (a router's answer, an inverse) therefore land exactly
//! as written, but for a free end written where a reader attaches it; a moved track keeps its
//! anchored ends on their anchors. The follow-on changes are part of the batch: in its inverse
//! and its delta.
//!
//! Invariants: a batch then `undo` of its inverse restores the same canonical hash and the same
//! list order (every item the batch touched is named by the inverse, at its pre-batch position);
//! a failed batch changes nothing. `generation` is bumped once per applied op; `next_item` only
//! grows (ids are never reused, an undo included).
//!
//! Cost: a batch of k ops is O(k) structural steps, one snap of O(tracks x snap rounds) (usually
//! one or two rounds), O(tracks + free ends x touched vias, holes and tracks + attached ends x
//! items) to attach free ends, one copy of a
//! kind's ids when the batch inserts into or removes from that kind's list, and one delta over
//! the touched items; indexed id lookups are O(1). The delta's bounding box covers old and new
//! geometry of every changed item.

use crate::{
    Anchor, Area, Board, BoardKeepout, BoardNet, Cutout, Graphic, Hole, ItemKind, NetPair, Origin, Placed, Rules,
    Stackup, Text, Track, Tuning, Via, Zone, ZoneSettings,
};
use fab_elec_circuit::NetClass;
use fab_elec_parts::FpTextKind;
use fab_elec_core::{canon_enum, Angle, BBox, BlockId, Contour, FeatureRef, ItemId, LayerId, LayerSet, NetId, Nm, OnTrack, Path, Seg, Side, P};
use std::collections::{HashMap, HashSet};

/// A board item, owned.
#[derive(Clone, PartialEq, Debug)]
pub enum Item {
    Placed(Placed),
    Track(Track),
    Via(Via),
    Zone(Zone),
    Keepout(BoardKeepout),
    Hole(Hole),
    Cutout(Cutout),
    Graphic(Graphic),
    Text(Text),
    Tuning(Tuning),
}

impl Item {
    /// The item with id `id` (same kind and content).
    pub fn with_id(mut self, id: ItemId) -> Item {
        match &mut self {
            Item::Placed(x) => x.id = id,
            Item::Track(x) => x.id = id,
            Item::Via(x) => x.id = id,
            Item::Zone(x) => x.id = id,
            Item::Keepout(x) => x.id = id,
            Item::Hole(x) => x.id = id,
            Item::Cutout(x) => x.id = id,
            Item::Graphic(x) => x.id = id,
            Item::Text(x) => x.id = id,
            Item::Tuning(x) => x.id = id,
        }
        self
    }
    /// The item with origin `origin`.
    pub fn with_origin(mut self, origin: Origin) -> Item {
        *self.origin_mut() = origin;
        self
    }
    pub fn kind(&self) -> ItemKind {
        match self {
            Item::Placed(_) => ItemKind::Placed,
            Item::Track(_) => ItemKind::Track,
            Item::Via(_) => ItemKind::Via,
            Item::Zone(_) => ItemKind::Zone,
            Item::Keepout(_) => ItemKind::Keepout,
            Item::Hole(_) => ItemKind::Hole,
            Item::Cutout(_) => ItemKind::Cutout,
            Item::Graphic(_) => ItemKind::Graphic,
            Item::Text(_) => ItemKind::Text,
            Item::Tuning(_) => ItemKind::Tuning,
        }
    }
    fn origin_mut(&mut self) -> &mut Origin {
        match self {
            Item::Placed(x) => &mut x.origin,
            Item::Track(x) => &mut x.origin,
            Item::Via(x) => &mut x.origin,
            Item::Zone(x) => &mut x.origin,
            Item::Keepout(x) => &mut x.origin,
            Item::Hole(x) => &mut x.origin,
            Item::Cutout(x) => &mut x.origin,
            Item::Graphic(x) => &mut x.origin,
            Item::Text(x) => &mut x.origin,
            Item::Tuning(x) => &mut x.origin,
        }
    }
}

/// One field of an item.
#[derive(Clone, PartialEq, Debug)]
pub enum Field {
    At(P),
    Rot(Angle),
    Side(Side),
    Locked(bool),
    Width(Nm),
    Widths(Option<Vec<Nm>>),
    Layer(LayerId),
    Layers(LayerSet),
    Net(Option<NetId>),
    Path(Path),
    Ends([crate::Anchor; 2]),
    Drill(Nm),
    Pad(Nm),
    Span(LayerId, LayerId),
    Priority(u16),
    ZoneSettings(ZoneSettings),
    Area(Area),
    /// A text's text; a zone's, hole's or cutout's name; a placement's reference.
    Text(String),
    /// Adopt (`Authored`) or release (`Solved`); other origins only through `apply_host`.
    Origin(Origin),
    /// A placement's own pose for its Reference or Value label (`None`: the footprint's own).
    /// A `User` text has none.
    Label(FpTextKind, Option<crate::LabelPose>),
}

impl Field {
    fn name(&self) -> &'static str {
        match self {
            Field::At(_) => "at",
            Field::Rot(_) => "rot",
            Field::Side(_) => "side",
            Field::Locked(_) => "locked",
            Field::Width(_) => "width",
            Field::Widths(_) => "widths",
            Field::Layer(_) => "layer",
            Field::Layers(_) => "layers",
            Field::Net(_) => "net",
            Field::Path(_) => "path",
            Field::Ends(_) => "ends",
            Field::Drill(_) => "drill",
            Field::Pad(_) => "pad",
            Field::Span(..) => "span",
            Field::Priority(_) => "priority",
            Field::ZoneSettings(_) => "settings",
            Field::Area(_) => "area",
            Field::Text(_) => "text",
            Field::Origin(_) => "origin",
            Field::Label(..) => "label",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum BoardOp {
    /// Append an item (its id must be new; issue ids with `Board::alloc_id`).
    Add(Item),
    /// Insert an item at a position in its list (the inverse of `Remove`, exact order).
    Insert { item: Item, pos: u32 },
    Remove(ItemId),
    /// Replace an item by one with the same id and kind.
    Replace(Item),
    Move { item: ItemId, by: P },
    SetField { item: ItemId, field: Field },
    /// Detach a block template's item: it becomes an ordinary statement (`detached: true`).
    Detach(ItemId),
    SetStackup(Stackup),
    SetOutline(Vec<Contour>),
    SetRules(Rules),
    /// The circuit's nets, classes and pairs, mirrored by the host after a circuit change.
    SetNets { nets: Vec<BoardNet>, classes: Vec<NetClass>, pairs: Vec<NetPair> },
}

// Tags never renumber; a new variant takes the next tag.
// Bytes are content only: no board index, generation or id counter, and no Arc identity.
// An item is the last field of its op.
canon_enum!(Item {
    0 => Placed(x),
    1 => Track(x),
    2 => Via(x),
    3 => Zone(x),
    4 => Keepout(x),
    5 => Hole(x),
    6 => Cutout(x),
    7 => Graphic(x),
    8 => Text(x),
    9 => Tuning(x),
});
canon_enum!(Field {
    0 => At(p),
    1 => Rot(a),
    2 => Side(s),
    3 => Locked(b),
    4 => Width(w),
    5 => Widths(ws),
    6 => Layer(l),
    7 => Layers(ls),
    8 => Net(n),
    9 => Path(p),
    10 => Ends(es),
    11 => Drill(d),
    12 => Pad(p),
    13 => Span(a, b),
    14 => Priority(p),
    15 => ZoneSettings(s),
    16 => Area(a),
    17 => Text(s),
    18 => Origin(o),
    19 => Label(k, l),
});
canon_enum!(BoardOp {
    0 => Add(item),
    1 => Insert { pos, item },
    2 => Remove(item),
    3 => Replace(item),
    4 => Move { item, by },
    5 => SetField { item, field },
    6 => Detach(item),
    7 => SetStackup(stackup),
    8 => SetOutline(outline),
    9 => SetRules(rules),
    10 => SetNets { nets, classes, pairs },
});

/// The ops that undo an op, applied in order with `Board::undo`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Inverse(pub Vec<BoardOp>);

/// One side (old or new) of a changed item as the layer-scoped engines see it: the box of what it puts on
/// `layers` (copper, drills, keepout areas, a zone's area) and those copper layers.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct ItemExtent {
    /// The box of this side's fill inputs.
    pub bbox: BBox,
    /// The board copper layers this side can affect.
    pub layers: LayerSet,
}

/// What an op changed (E4 A13): the input of every engine's update form.
#[derive(Clone, PartialEq, Debug)]
pub struct BoardDelta {
    pub added: Vec<ItemId>,
    pub removed: Vec<ItemId>,
    pub changed: Vec<ItemId>,
    /// Covers the old and new extent of every item above, including silk and mask openings.
    pub bbox: BBox,
    /// Every changed item's old and new side, one entry each. The fill reads these instead of
    /// `bbox`; with `global` set they are not read.
    pub extents: Vec<ItemExtent>,
    /// Nets of those items, before and after.
    pub nets: Vec<NetId>,
    /// The stackup, the outline, the rules or the net table changed: engines recompute fully
    /// (and `resolve_rules` re-runs only then).
    pub global: bool,
}

impl Default for BoardDelta {
    fn default() -> Self {
        BoardDelta {
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            bbox: BBox::EMPTY,
            extents: Vec::new(),
            nets: Vec::new(),
            global: false,
        }
    }
}

impl BoardDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty() && !self.global
    }
    /// The delta touches `id`.
    pub fn touches(&self, id: ItemId) -> bool {
        self.added.contains(&id) || self.removed.contains(&id) || self.changed.contains(&id)
    }
    /// Whether a side of a changed item on `layer` meets `window`: what a layer-scoped engine recomputes for.
    /// A global delta meets every window without reading its extents.
    pub fn meets(&self, layer: LayerId, window: BBox) -> bool {
        self.global || self.extents.iter().any(|e| e.layers.has(layer) && e.bbox.intersects(&window))
    }
    /// The delta that takes `old` to `new` (the same unit): every item added, removed or changed by
    /// value, the boxes of both sides, their nets; `global` when the stackup, the outline, the nets,
    /// the classes, pairs or rules differ. Order-only changes in an item list are not content (no engine reads
    /// list order). Cost O(n) in items; a caller skips boards whose `Arc`s are the same.
    pub fn between(old: &Board, new: &Board) -> BoardDelta {
        fn include(delta: &mut BoardDelta, item: Item, board: &Board) {
            delta.bbox = delta.bbox.union(item.extent(board));
            delta.extents.push(item.fill_extent(board));
            delta.nets.extend(item.nets());
        }

        let global = old.stackup != new.stackup || old.outline != new.outline || old.nets != new.nets
            || old.classes != new.classes || old.pairs != new.pairs || old.rules != new.rules;
        let mut delta = BoardDelta {
            global,
            bbox: if global { old.outline_bbox().union(new.outline_bbox()) } else { BBox::EMPTY },
            ..BoardDelta::default()
        };
        macro_rules! compare {
            ($list:ident, $kind:ident) => {{
                let mut old_ids: HashMap<ItemId, usize> = old.$list.iter().enumerate().map(|(i, item)| (item.id, i)).collect();
                for item in &new.$list {
                    if let Some(i) = old_ids.remove(&item.id) {
                        let before = &old.$list[i];
                        if before == item {
                            continue;
                        }
                        delta.changed.push(item.id);
                        include(&mut delta, Item::$kind(before.clone()), old);
                    } else {
                        delta.added.push(item.id);
                    }
                    include(&mut delta, Item::$kind(item.clone()), new);
                }
                // Matched ids were removed above; the remaining old ids are absent from new.
                for (id, i) in old_ids {
                    delta.removed.push(id);
                    include(&mut delta, Item::$kind(old.$list[i].clone()), old);
                }
            }};
        }
        compare!(placed, Placed);
        compare!(tracks, Track);
        compare!(vias, Via);
        compare!(zones, Zone);
        compare!(keepouts, Keepout);
        compare!(holes, Hole);
        compare!(cutouts, Cutout);
        compare!(graphics, Graphic);
        compare!(texts, Text);
        compare!(tunings, Tuning);
        // A via that follows the copper changes with its rings even when it is equal.
        for v in new.vias.iter().filter(|v| v.flash.follows_copper()) {
            if !delta.touches(v.id) && old.rings.peek(v.id) != new.rings.peek(v.id) {
                if let Some(before) = old.item(v.id) {
                    delta.changed.push(v.id);
                    include(&mut delta, before, old);
                    include(&mut delta, Item::Via(v.clone()), new);
                }
            }
        }
        for ids in [&mut delta.added, &mut delta.removed, &mut delta.changed] {
            ids.sort();
            ids.dedup();
        }
        delta.nets.sort();
        delta.nets.dedup();
        delta
    }
    /// Every id the delta names, sorted.
    pub fn ids(&self) -> Vec<ItemId> {
        let mut v: Vec<ItemId> = self.added.iter().chain(&self.removed).chain(&self.changed).copied().collect();
        v.sort();
        v.dedup();
        v
    }
}

/// A batch's result: its exact inverse and what it changed.
#[derive(Clone, PartialEq, Debug)]
pub struct Applied {
    pub inverse: Inverse,
    pub delta: BoardDelta,
}

#[derive(Clone, PartialEq, Debug)]
pub enum OpError {
    NoItem(ItemId),
    DuplicateId(ItemId),
    /// `Replace` with an item of another kind.
    KindMismatch(ItemId),
    /// The item is part of an undetached block template; only `Detach` applies.
    BlockItem { item: ItemId, block: BlockId },
    /// The item is derived from another unit's feature; change the constraint instead.
    Derived { item: ItemId, from: FeatureRef },
    /// `Detach` on an item that is not an undetached block item.
    NotDetachable(ItemId),
    /// The field does not exist on this kind of item, or the value is not allowed.
    BadField { item: ItemId, field: &'static str },
    /// A track end names an anchor that does not exist.
    BadAnchor(ItemId),
    BadPosition { item: ItemId, pos: u32 },
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Pre-states of every item a batch touches, in touch order.
#[derive(Default)]
struct Touch {
    old: Vec<(ItemId, Option<(Item, usize)>)>,
    seen: HashSet<ItemId>,
}

/// What a batch's ops did, read once they have all applied.
#[derive(Default)]
struct Batch {
    t: Touch,
    /// Items an op adds, inserts, replaces, moves or sets, in first-op order: the snap starts here.
    named: Vec<ItemId>,
    /// Items an Add or Insert put on the board.
    added: Vec<ItemId>,
    /// Items a Remove took off the board (an item added back later is a replacement).
    removed: HashSet<ItemId>,
    /// The last op that wrote each track: the index an anchor error names.
    writer: HashMap<ItemId, usize>,
    /// Each kind's ids before the batch's first insert or remove there: the pre-batch positions.
    lists: Vec<(ItemKind, Vec<ItemId>)>,
    /// Each global op's inverse and the box it covers, in op order.
    global: Vec<(BoardOp, BBox)>,
}

impl Batch {
    fn name(&mut self, id: ItemId) {
        if !self.named.contains(&id) {
            self.named.push(id);
        }
    }
}

impl Board {
    /// Apply one op under the edit rules: a batch of one.
    pub fn apply(&mut self, op: &BoardOp) -> Result<Applied, OpError> {
        self.apply_batch(std::slice::from_ref(op), false).map_err(|(_, e)| e)
    }
    /// Apply one op without the edit rules (host stages, undo, redo): a batch of one.
    pub fn apply_host(&mut self, op: &BoardOp) -> Result<Applied, OpError> {
        self.apply_batch(std::slice::from_ref(op), true).map_err(|(_, e)| e)
    }
    /// Apply one change's ops as one batch under the edit rules.
    pub fn apply_all(&mut self, ops: &[BoardOp]) -> Result<Applied, OpError> {
        self.apply_batch(ops, false).map_err(|(_, e)| e)
    }
    /// Apply an inverse as one batch (undo; the result's inverse is the redo).
    pub fn undo(&mut self, inv: &Inverse) -> Result<Applied, OpError> {
        self.apply_batch(&inv.0, true).map_err(|(_, e)| e)
    }
    /// Apply one change's ops as one batch (module doc), all or nothing: one inverse and one
    /// delta. `host` skips the edit rules. An error names the index of the op it comes from.
    pub fn apply_batch(&mut self, ops: &[BoardOp], host: bool) -> Result<Applied, (usize, OpError)> {
        self.apply_batch_seen(ops, host, |_, _| {})
    }
    /// `apply_batch` over borrowed ops, calling `seen` with the board and the op's index before
    /// each op's own change (list positions and fields as the ops so far left them; nothing is
    /// snapped yet).
    pub fn apply_batch_seen<'a>(&mut self, ops: impl IntoIterator<Item = &'a BoardOp>, host: bool, mut seen: impl FnMut(&Board, usize)) -> Result<Applied, (usize, OpError)> {
        let generation = self.generation;
        let mut b = Batch::default();
        let mut count = 0;
        for (i, op) in ops.into_iter().enumerate() {
            count += 1;
            seen(self, i);
            if let Err(e) = self.step(&mut b, i, op, host) {
                self.restore(&b, generation);
                return Err((i, e));
            }
        }
        let gone: Vec<ItemId> = b.t.old.iter().map(|(id, _)| *id).filter(|id| b.removed.contains(id) && self.locate(*id).is_none()).collect();
        for id in gone {
            self.free_anchors(&mut b.t, id);
        }
        for &id in &b.named {
            let Some(&op) = b.writer.get(&id) else { continue };
            let Some((ItemKind::Track, i)) = self.locate(id) else { continue };
            if let Err(e) = self.check_anchors(&self.tracks[i]) {
                self.restore(&b, generation);
                return Err((op, e));
            }
        }
        self.snap(&mut b.t, &b.named);
        let attached = self.attach_free_ends(&mut b.t);
        if !attached.is_empty() {
            self.snap(&mut b.t, &attached);
        }
        self.generation = generation + count;
        let mut delta = self.delta_of(&b);
        self.settle_rings(&mut delta);
        Ok(Applied { inverse: Inverse(self.undo_ops(&b)), delta })
    }

    /// Put the board back as it was before the batch: its inverse, step by step (every item it
    /// names is exact, so nothing is snapped).
    fn restore(&mut self, b: &Batch, generation: u64) {
        let mut scratch = Batch::default();
        for op in self.undo_ops(b) {
            self.step(&mut scratch, 0, &op, true).expect("a batch's inverse restores it");
        }
        self.generation = generation;
    }

    /// The ops that restore the board as it was before the batch, applied as a batch: the
    /// global tables in reverse op order, then a Remove of every added (or removed and added
    /// again) item, then an Insert of every removed one at its pre-batch position (ascending, so
    /// each lands where it was), then a Replace of every other touched item.
    fn undo_ops(&self, b: &Batch) -> Vec<BoardOp> {
        let mut out: Vec<BoardOp> = b.global.iter().rev().map(|(inv, _)| inv.clone()).collect();
        let mut inserts = Vec::new();
        let mut replaces = Vec::new();
        let mut at: Option<HashMap<ItemId, usize>> = None;
        for (id, pre) in &b.t.old {
            let present = self.locate(*id).is_some();
            let Some((old, pos)) = pre else {
                if present {
                    out.push(BoardOp::Remove(*id));
                }
                continue;
            };
            if present && !b.removed.contains(id) {
                replaces.push(BoardOp::Replace(old.clone()));
                continue;
            }
            if present {
                out.push(BoardOp::Remove(*id));
            }
            let at = at.get_or_insert_with(|| {
                b.lists.iter().flat_map(|(_, ids)| ids.iter().enumerate().map(|(i, id)| (*id, i))).collect()
            });
            let pos = at.get(id).copied().unwrap_or(*pos);
            inserts.push((pos, BoardOp::Insert { item: old.clone(), pos: pos as u32 }));
        }
        inserts.sort_by_key(|(pos, _)| *pos);
        out.extend(inserts.into_iter().map(|(_, op)| op));
        out.extend(replaces);
        out
    }

    /// What the batch changed: the pre-batch and final sides of every touched item.
    fn delta_of(&self, b: &Batch) -> BoardDelta {
        let mut delta = BoardDelta::default();
        for (_, bbox) in &b.global {
            delta.global = true;
            delta.bbox = delta.bbox.union(*bbox);
        }
        for (id, pre) in &b.t.old {
            let post = self.item(*id);
            if let Some((old, _)) = pre {
                delta.bbox = delta.bbox.union(old.extent(self));
                delta.extents.push(old.fill_extent(self));
                delta.nets.extend(old.nets());
            }
            if let Some(new) = &post {
                delta.bbox = delta.bbox.union(new.extent(self));
                delta.extents.push(new.fill_extent(self));
                delta.nets.extend(new.nets());
            }
            match (pre.is_some(), post.is_some()) {
                (false, true) => delta.added.push(*id),
                (true, false) => delta.removed.push(*id),
                (true, true) => delta.changed.push(*id),
                (false, false) => {}
            }
        }
        delta.nets.sort();
        delta.nets.dedup();
        delta
    }

    /// Keep `kind`'s ids as they were before the batch's first insert or remove there.
    fn keep_list(&self, b: &mut Batch, kind: ItemKind) {
        if b.lists.iter().any(|(k, _)| *k == kind) {
            return;
        }
        macro_rules! ids {
            ($list:ident) => { self.$list.iter().map(|x| x.id).collect() };
        }
        let ids = match kind {
            ItemKind::Placed => ids!(placed),
            ItemKind::Track => ids!(tracks),
            ItemKind::Via => ids!(vias),
            ItemKind::Zone => ids!(zones),
            ItemKind::Keepout => ids!(keepouts),
            ItemKind::Hole => ids!(holes),
            ItemKind::Cutout => ids!(cutouts),
            ItemKind::Graphic => ids!(graphics),
            ItemKind::Text => ids!(texts),
            ItemKind::Tuning => ids!(tunings),
        };
        b.lists.push((kind, ids));
    }

    fn check_rules(&self, id: ItemId, op: &BoardOp) -> Result<(), OpError> {
        let item = self.item(id).ok_or(OpError::NoItem(id))?;
        match item.origin() {
            Origin::Block { block, detached: false } if !matches!(op, BoardOp::Detach(_)) => Err(OpError::BlockItem { item: id, block: *block }),
            Origin::Derived { from } => Err(OpError::Derived { item: id, from: from.clone() }),
            _ => Ok(()),
        }
    }

    fn touch(&self, t: &mut Touch, id: ItemId) {
        if !t.seen.insert(id) {
            return;
        }
        let pre = self.locate(id).map(|(_, pos)| (self.item(id).unwrap(), pos));
        t.old.push((id, pre));
    }

    fn put(&mut self, item: Item) {
        let id = item.id();
        let (k, i) = self.locate(id).expect("put of a located item");
        match item {
            Item::Placed(x) => self.placed[i] = x,
            Item::Track(x) => self.tracks[i] = x,
            Item::Via(x) => self.vias[i] = x,
            Item::Zone(x) => self.zones[i] = x,
            Item::Keepout(x) => self.keepouts[i] = x,
            Item::Hole(x) => self.holes[i] = x,
            Item::Cutout(x) => self.cutouts[i] = x,
            Item::Graphic(x) => self.graphics[i] = x,
            Item::Text(x) => self.texts[i] = x,
            Item::Tuning(x) => self.tunings[i] = x,
        }
        self.index.0.insert(id, (k, i as u32));
    }

    fn insert_at(&mut self, item: Item, pos: Option<usize>) -> Result<(), OpError> {
        let kind = item.kind();
        macro_rules! ins {
            ($list:ident, $x:expr) => {{
                let p = pos.unwrap_or(self.$list.len());
                if p > self.$list.len() {
                    return Err(OpError::BadPosition { item: $x.id, pos: p as u32 });
                }
                self.$list.insert(p, $x);
                self.index_from(kind, p);
            }};
        }
        match item {
            Item::Placed(x) => ins!(placed, x),
            Item::Track(x) => ins!(tracks, x),
            Item::Via(x) => ins!(vias, x),
            Item::Zone(x) => ins!(zones, x),
            Item::Keepout(x) => ins!(keepouts, x),
            Item::Hole(x) => ins!(holes, x),
            Item::Cutout(x) => ins!(cutouts, x),
            Item::Graphic(x) => ins!(graphics, x),
            Item::Text(x) => ins!(texts, x),
            Item::Tuning(x) => ins!(tunings, x),
        }
        Ok(())
    }

    fn remove_item(&mut self, id: ItemId) {
        let (k, i) = self.locate(id).expect("remove of a located item");
        match k {
            ItemKind::Placed => drop(self.placed.remove(i)),
            ItemKind::Track => drop(self.tracks.remove(i)),
            ItemKind::Via => drop(self.vias.remove(i)),
            ItemKind::Zone => drop(self.zones.remove(i)),
            ItemKind::Keepout => drop(self.keepouts.remove(i)),
            ItemKind::Hole => drop(self.holes.remove(i)),
            ItemKind::Cutout => drop(self.cutouts.remove(i)),
            ItemKind::Graphic => drop(self.graphics.remove(i)),
            ItemKind::Text => drop(self.texts.remove(i)),
            ItemKind::Tuning => drop(self.tunings.remove(i)),
        }
        self.index.0.remove(&id);
        self.index_from(k, i);
    }

    /// Every anchor a track names must exist (`Track` anchors name another track).
    /// A segment past the host's end is valid: `point_on` clamps it to the host's end.
    fn check_anchors(&self, t: &Track) -> Result<(), OpError> {
        for a in t.ends {
            let ok = match a {
                crate::Anchor::Free => true,
                crate::Anchor::Pad { placed, pad } => self.placed_item(placed).is_some_and(|p| pad.ix() < p.footprint.pads.len()),
                crate::Anchor::Via(v) => matches!(self.locate(v), Some((ItemKind::Via, _))),
                crate::Anchor::Hole(h) => matches!(self.locate(h), Some((ItemKind::Hole, _))),
                crate::Anchor::Track { id, .. } => id != t.id && matches!(self.locate(id), Some((ItemKind::Track, _))),
            };
            if !ok {
                return Err(OpError::BadAnchor(t.id));
            }
        }
        Ok(())
    }

    /// The anchor a free end of `t` at `p` takes: the reader's rule. O(vias + holes + tracks).
    pub fn infer_anchor(&self, t: &Track, p: P) -> Anchor {
        if let Some(v) = self.vias.iter().find(|v| v.net == t.net && v.at == p) { Anchor::Via(v.id) }
        else if let Some(h) = self.holes.iter().find(|h| h.at == p) { Anchor::Hole(h.id) }
        else if let Some(anchor) = self.tracks.iter().filter(|other| other.id != t.id && other.net == t.net && other.layer == t.layer)
            .find_map(|other| on_track(p, &other.path).map(|at| Anchor::Track { id: other.id, at })) { anchor }
        else { Anchor::Free }
    }

    /// Put every anchored track end on its anchor (for boards built directly, not through ops;
    /// ops keep this invariant themselves). Not an op: no inverse, no delta.
    pub fn snap_anchors(&mut self) {
        let ids: Vec<ItemId> = self.tracks.iter().map(|t| t.id).collect();
        self.snap(&mut Touch::default(), &ids);
    }

    /// Put the anchored ends of the tracks in `which` (or of every track anchored to an item in
    /// `which`) on their anchors, then propagate through junction hosts whose paths changed.
    /// Each (track, end) moves at most once per call to end cycles. O(tracks) per round, usually
    /// one or two rounds.
    fn snap(&mut self, t: &mut Touch, which: &[ItemId]) {
        let mut which: HashSet<ItemId> = which.iter().copied().collect();
        let mut snapped = vec![[false; 2]; self.tracks.len()];
        let mut first = true;
        while !which.is_empty() {
            let mut moved = HashSet::new();
            for ti in 0..self.tracks.len() {
                let tr = &self.tracks[ti];
                let mine = first && which.contains(&tr.id);
                let mut new_path = None;
                for (e, a) in tr.ends.iter().enumerate() {
                    let follows = match *a {
                        crate::Anchor::Track { id, .. } => which.contains(&id),
                        _ => first && a.item().is_some_and(|i| which.contains(&i)),
                    };
                    if snapped[ti][e] || !(mine || follows) {
                        continue;
                    }
                    let Some(pos) = self.anchor_pos(*a) else { continue };
                    let cur = if e == 0 { tr.path.start } else { tr.path.end() };
                    if cur != pos {
                        let p = new_path.get_or_insert_with(|| tr.path.clone());
                        set_end(p, e, pos);
                        snapped[ti][e] = true;
                    }
                }
                if let Some(p) = new_path {
                    let id = self.tracks[ti].id;
                    self.touch(t, id);
                    self.tracks[ti].path = p;
                    moved.insert(id);
                }
            }
            which = moved;
            first = false;
        }
    }

    /// Free the track ends anchored to `gone` (a removed item); `attach_free_ends` then attaches
    /// those a reader would attach to other copper there.
    fn free_anchors(&mut self, t: &mut Touch, gone: ItemId) {
        for ti in 0..self.tracks.len() {
            if self.tracks[ti].ends.iter().any(|a| a.item() == Some(gone)) {
                let id = self.tracks[ti].id;
                self.touch(t, id);
                for a in &mut self.tracks[ti].ends {
                    if a.item() == Some(gone) {
                        *a = crate::Anchor::Free;
                    }
                }
            }
        }
    }

    /// Attach, as a reader attaches them (`infer_anchor`), the free track ends a batch may have
    /// made attachable: every free end of a track it touched (added, written free, or freed by a
    /// removal) and every free end on a via, hole or track it touched (added, moved or re-laid).
    /// So no free end lies where a reload would attach it, whatever the ops wrote, and undoing a
    /// Remove restores exactly. Returns the tracks changed, in board order. O(tracks + free ends x
    /// touched vias, holes and tracks + attached ends x items).
    fn attach_free_ends(&mut self, t: &mut Touch) -> Vec<ItemId> {
        let hosts: Vec<(ItemKind, usize)> = t.old.iter().filter_map(|(id, _)| match self.locate(*id) {
            Some(found @ (ItemKind::Via | ItemKind::Hole | ItemKind::Track, _)) => Some(found),
            _ => None,
        }).collect();
        let on_host = |track: &Track, p: P| hosts.iter().any(|&(kind, i)| match kind {
            ItemKind::Via => self.vias[i].net == track.net && self.vias[i].at == p,
            ItemKind::Hole => self.holes[i].at == p,
            _ => {
                let host = &self.tracks[i];
                host.id != track.id && host.net == track.net && host.layer == track.layer && on_track(p, &host.path).is_some()
            }
        });
        let mut changes = Vec::new();
        for (ti, track) in self.tracks.iter().enumerate() {
            let points = [track.path.start, track.path.end()];
            let mut ends = track.ends;
            for e in 0..2 {
                if ends[e] == Anchor::Free && (t.seen.contains(&track.id) || on_host(track, points[e])) {
                    ends[e] = self.infer_anchor(track, points[e]);
                }
            }
            if ends != track.ends {
                changes.push((ti, track.id, ends));
            }
        }
        let mut changed = Vec::with_capacity(changes.len());
        for (ti, id, ends) in changes {
            self.touch(t, id);
            self.tracks[ti].ends = ends;
            changed.push(id);
        }
        changed
    }

    /// One op's own change, in op order: no snapping, no anchor check (the batch does those once
    /// its ops have all applied).
    fn step(&mut self, b: &mut Batch, i: usize, op: &BoardOp, host: bool) -> Result<(), OpError> {
        match op {
            BoardOp::Add(item) | BoardOp::Insert { item, .. } => {
                let id = item.id();
                if self.locate(id).is_some() {
                    return Err(OpError::DuplicateId(id));
                }
                let pos = if let BoardOp::Insert { pos, .. } = op { Some(*pos as usize) } else { None };
                if pos.is_some() {
                    self.keep_list(b, item.kind());
                }
                self.touch(&mut b.t, id);
                self.insert_at(item.clone(), pos)?;
                self.next_item = self.next_item.max(id.0 + 1);
                b.name(id);
                b.added.push(id);
                if item.kind() == ItemKind::Track {
                    b.writer.insert(id, i);
                }
            }
            BoardOp::Remove(id) => {
                if !host {
                    self.check_rules(*id, op)?;
                }
                let (kind, _) = self.locate(*id).ok_or(OpError::NoItem(*id))?;
                self.keep_list(b, kind);
                self.touch(&mut b.t, *id);
                self.remove_item(*id);
                b.removed.insert(*id);
            }
            BoardOp::Replace(item) => {
                let id = item.id();
                let (k, _) = self.locate(id).ok_or(OpError::NoItem(id))?;
                if k != item.kind() {
                    return Err(OpError::KindMismatch(id));
                }
                if !host {
                    self.check_rules(id, op)?;
                }
                self.touch(&mut b.t, id);
                self.put(item.clone());
                b.name(id);
                if k == ItemKind::Track {
                    b.writer.insert(id, i);
                }
            }
            BoardOp::Move { item: id, by } => {
                if !host {
                    self.check_rules(*id, op)?;
                }
                let mut it = self.item(*id).ok_or(OpError::NoItem(*id))?;
                move_item(&mut it, *by).map_err(|f| OpError::BadField { item: *id, field: f })?;
                if !host {
                    adopt(&mut it);
                }
                self.touch(&mut b.t, *id);
                self.put(it);
                b.name(*id);
            }
            BoardOp::SetField { item: id, field } => {
                if !host {
                    self.check_rules(*id, op)?;
                    if let Field::Origin(o) = field {
                        if !matches!(o, Origin::Authored | Origin::Solved { .. }) {
                            return Err(OpError::BadField { item: *id, field: "origin" });
                        }
                    }
                }
                let mut it = self.item(*id).ok_or(OpError::NoItem(*id))?;
                set_field(&mut it, field).map_err(|f| OpError::BadField { item: *id, field: f })?;
                if !host && !matches!(field, Field::Origin(_) | Field::Label(..)) {
                    adopt(&mut it);
                }
                let track = matches!(it, Item::Track(_));
                self.touch(&mut b.t, *id);
                self.put(it);
                b.name(*id);
                if track {
                    b.writer.insert(*id, i);
                }
            }
            BoardOp::Detach(id) => {
                let mut it = self.item(*id).ok_or(OpError::NoItem(*id))?;
                match it.origin_mut() {
                    Origin::Block { detached, .. } if !*detached => *detached = true,
                    _ => return Err(OpError::NotDetachable(*id)),
                }
                self.touch(&mut b.t, *id);
                self.put(it);
            }
            BoardOp::SetStackup(s) => {
                let old = std::mem::replace(&mut self.stackup, s.clone());
                b.global.push((BoardOp::SetStackup(old), self.outline_bbox()));
            }
            BoardOp::SetOutline(o) => {
                let before = self.outline_bbox();
                let old = std::mem::replace(&mut self.outline, o.clone());
                b.global.push((BoardOp::SetOutline(old), before.union(self.outline_bbox())));
            }
            BoardOp::SetRules(r) => {
                let old = std::mem::replace(&mut self.rules, r.clone());
                b.global.push((BoardOp::SetRules(old), self.outline_bbox()));
            }
            BoardOp::SetNets { nets, classes, pairs } => {
                let old_nets = std::mem::replace(&mut self.nets, nets.clone());
                let old_classes = std::mem::replace(&mut self.classes, classes.clone());
                let old_pairs = std::mem::replace(&mut self.pairs, pairs.clone());
                b.global.push((BoardOp::SetNets { nets: old_nets, classes: old_classes, pairs: old_pairs }, self.outline_bbox()));
            }
        }
        Ok(())
    }
}

fn set_end(p: &mut Path, which: usize, to: P) {
    if which == 0 {
        p.start = to;
        return;
    }
    match p.segs.last_mut() {
        Some(Seg::Line(e)) => *e = to,
        Some(Seg::Arc { end, .. }) => *end = to,
        None => p.start = to,
    }
}

/// Where `p` lies exactly on `path`: its start, a segment's end, or a point of a straight segment.
/// The test a reader and an added item use to attach a free track end.
pub fn on_track(p: P, path: &Path) -> Option<OnTrack> {
    if p == path.start { return Some(OnTrack::default()); }
    path.steps().enumerate().find_map(|(seg, (a, s))| {
        let seg = seg as u32;
        let b = s.end();
        if p == b { return Some(OnTrack { seg, frac: OnTrack::ONE }); }
        if !matches!(s, Seg::Line(_)) { return None; }
        let cross = (b.x as i128 - a.x as i128) * (p.y as i128 - a.y as i128)
            - (b.y as i128 - a.y as i128) * (p.x as i128 - a.x as i128);
        (cross == 0 && p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x)
            && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)).then(|| OnTrack::along(seg, a, None, b, p))
    })
}

fn adopt(it: &mut Item) {
    let o = it.origin_mut();
    if matches!(o, Origin::Solved { .. }) {
        *o = Origin::Authored;
    }
}

fn move_item(it: &mut Item, by: P) -> Result<(), &'static str> {
    match it {
        Item::Placed(x) => x.at = x.at.add(by),
        Item::Track(x) => x.path = x.path.translated(by),
        Item::Via(x) => x.at = x.at.add(by),
        Item::Zone(x) => match &mut x.area {
            Area::Poly(p) => *p = p.translated(by),
            Area::Board => return Err("area"),
        },
        Item::Keepout(x) => x.area = x.area.translated(by),
        Item::Hole(x) => x.at = x.at.add(by),
        Item::Cutout(x) => x.contour = Contour::new(x.contour.path.translated(by)),
        Item::Graphic(x) => x.gfx = x.gfx.mapped(|s| s.translated(by), |p| p.translated(by)),
        Item::Text(x) => x.at = x.at.add(by),
        Item::Tuning(_) => return Err("at"),
    }
    Ok(())
}

fn set_field(it: &mut Item, f: &Field) -> Result<(), &'static str> {
    let bad = Err(f.name());
    match (it, f) {
        (Item::Placed(x), Field::At(v)) => x.at = *v,
        (Item::Via(x), Field::At(v)) => x.at = *v,
        (Item::Hole(x), Field::At(v)) => x.at = *v,
        (Item::Text(x), Field::At(v)) => x.at = *v,
        (Item::Placed(x), Field::Rot(v)) => x.rot = *v,
        (Item::Text(x), Field::Rot(v)) => x.rot = *v,
        (Item::Placed(x), Field::Side(v)) => x.side = *v,
        (Item::Placed(x), Field::Locked(v)) => x.locked = *v,
        (Item::Track(x), Field::Locked(v)) => x.locked = *v,
        (Item::Via(x), Field::Locked(v)) => x.locked = *v,
        (Item::Zone(x), Field::Locked(v)) => x.locked = *v,
        (Item::Graphic(x), Field::Locked(v)) => x.locked = *v,
        (Item::Text(x), Field::Locked(v)) => x.locked = *v,
        (Item::Track(x), Field::Width(v)) => x.w = *v,
        (Item::Track(x), Field::Widths(v)) => x.widths = v.clone(),
        (Item::Track(x), Field::Layer(v)) if v.is_copper() => x.layer = *v,
        (Item::Graphic(x), Field::Layer(v)) => x.layer = *v,
        (Item::Text(x), Field::Layer(v)) => x.layer = *v,
        (Item::Zone(x), Field::Layers(v)) => x.layers = *v,
        (Item::Keepout(x), Field::Layers(v)) => x.layers = *v,
        (Item::Track(x), Field::Net(Some(v))) => x.net = *v,
        (Item::Via(x), Field::Net(Some(v))) => x.net = *v,
        (Item::Zone(x), Field::Net(v)) => x.net = *v,
        (Item::Hole(x), Field::Net(v)) => x.net = *v,
        (Item::Track(x), Field::Path(v)) => x.path = v.clone(),
        (Item::Track(x), Field::Ends(v)) => x.ends = *v,
        (Item::Via(x), Field::Drill(v)) => x.drill = *v,
        (Item::Hole(x), Field::Drill(v)) => x.drill = *v,
        (Item::Via(x), Field::Pad(v)) => x.pad = *v,
        (Item::Hole(x), Field::Pad(v)) => x.pad = Some(*v),
        (Item::Via(x), Field::Span(a, b)) => x.span = (*a, *b),
        (Item::Zone(x), Field::Priority(v)) => x.priority = *v,
        (Item::Zone(x), Field::ZoneSettings(v)) => x.settings = v.clone(),
        (Item::Zone(x), Field::Area(v)) => x.area = v.clone(),
        (Item::Keepout(x), Field::Area(Area::Poly(v))) => x.area = v.clone(),
        (Item::Text(x), Field::Text(v)) => x.text = v.clone(),
        (Item::Zone(x), Field::Text(v)) => x.name = v.clone(),
        (Item::Hole(x), Field::Text(v)) => x.name = v.clone(),
        (Item::Cutout(x), Field::Text(v)) => x.name = v.clone(),
        (Item::Placed(x), Field::Text(v)) => x.refdes = v.clone(),
        (Item::Placed(x), Field::Label(FpTextKind::Reference, v)) => x.ref_label = *v,
        (Item::Placed(x), Field::Label(FpTextKind::Value, v)) => x.value_label = *v,
        (it, Field::Origin(o)) => *it.origin_mut() = o.clone(),
        _ => return bad,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect, dangling_ends, drc, resolve_rules, ConnItem, Flash, Tent, ViaKind, Waiver};
    use fab_elec_circuit::CheckKind;
    use fab_elec_core::{mm, BoardId, ClassId, Sel};

    #[test]
    fn track_junction_follows_host_edits() {
        let mut board = Board::new(BoardId(0), "track junction");
        board.nets.push(BoardNet { name: "signal".into(), class: ClassId(0) });
        let host = board.alloc_id();
        board.apply(&BoardOp::Add(Item::Track(Track {
            id: host, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None,
            path: Path::new(P::mm(2.0, 2.0)).line(P::mm(12.0, 2.0)).line(P::mm(12.0, 10.0)),
            ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
        }))).expect("add the junction host through ops");
        let at = OnTrack { seg: 0, frac: OnTrack::ONE / 2 };
        let start = board.tracks[0].path.point_on(at);
        let branch = board.alloc_id();
        board.apply(&BoardOp::Add(Item::Track(Track {
            id: branch, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None,
            path: Path::new(start).line(P::mm(7.0, 8.0)),
            ends: [Anchor::Track { id: host, at }, Anchor::Free], locked: false, origin: Origin::Authored,
        }))).expect("add the anchored branch through ops");

        let moved = board.apply(&BoardOp::Move { item: host, by: P::mm(3.0, -1.0) }).expect("move the host");
        assert_eq!(board.tracks[1].path.start, board.tracks[0].path.point_on(at), "moving the host carries the branch start to its stored position");
        let conn = connect(&board, &[]);
        assert!(conn.clusters.iter().any(|c| [
            ConnItem::Track { item: host, seg: 0 }, ConnItem::Track { item: host, seg: 1 }, ConnItem::Track { item: branch, seg: 0 },
        ].iter().all(|item| c.items.contains(item))), "moving the host preserves copper connectivity to the branch");

        board.undo(&moved.inverse).expect("undo the host move");
        assert_eq!(board.tracks[1].path.start, start, "undoing the host move restores the branch start");

        board.apply(&BoardOp::SetField { item: host, field: Field::Path(Path::new(P::mm(2.0, 2.0)).line(P::mm(6.0, 4.0))) }).expect("shorten the host path");
        let start = board.tracks[1].path.start;
        assert_eq!(start, board.tracks[0].path.point_on(at), "shortening the host preserves the stored junction position");
        assert_eq!(board.tracks[0].path.locate(start).1, start, "shortening the host keeps the branch start on its centreline");

        let removed = board.apply(&BoardOp::Remove(host)).expect("remove the junction host");
        assert_eq!(board.tracks[0].ends[0], Anchor::Free, "removing the host frees the branch start");
        let findings = drc(&board, &connect(&board, &[]), &resolve_rules(&board, None), &[], None);
        assert!(findings.violations.iter().any(|v| v.kind == CheckKind::Dangling && v.items.contains(&Sel::Item(board.unit, branch)) && v.at == Some(start)), "removing the host reports the branch start as dangling");

        board.undo(&removed.inverse).expect("undo the host removal");
        assert_eq!(board.tracks[1].ends[0], Anchor::Track { id: host, at }, "undoing removal restores the host anchor and its stored position");

        // Two tracks anchored on each other's ends, the host re-laid with more segments and the
        // branch re-anchored on its new last one, in one change: both land as written. Applied
        // op by op, the host's op carried the branch to the host's stale segment and dragged the
        // host's end after it.
        let track = |id, path: Path, ends| Track {
            id, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None, path, ends, locked: false, origin: Origin::Authored,
        };
        let (branch, host) = (board.alloc_id(), board.alloc_id());
        let junction = P::mm(24.0, 4.0);
        board.apply(&BoardOp::Add(Item::Track(track(branch, Path::new(junction).line(P::mm(28.0, 4.0)), [Anchor::Free; 2]))))
            .expect("add a branch");
        board.apply(&BoardOp::Add(Item::Track(track(host, Path::new(P::mm(20.0, 0.0)).line(P::mm(24.0, 0.0)).line(junction), [Anchor::Free; 2]))))
            .expect("add a second host ending on the branch's start");
        let on_end = |seg| Anchor::Track { id: host, at: OnTrack { seg, frac: OnTrack::ONE } };
        let (b, h) = (board.tracks.len() - 2, board.tracks.len() - 1);
        assert_eq!((board.tracks[b].ends[0], board.tracks[h].ends[1]), (on_end(1), Anchor::Track { id: branch, at: OnTrack { seg: 0, frac: 0 } }),
            "the two free ends at the junction attach to each other");
        let before = board.clone();
        let relaid = track(host, Path::new(P::mm(20.0, 0.0)).line(P::mm(21.0, 1.0)).line(P::mm(22.0, 0.0)).line(P::mm(23.0, 1.0)).line(junction), board.tracks[h].ends);
        let moved = track(branch, Path::new(junction).line(P::mm(28.0, 4.0)), [on_end(3), Anchor::Free]);
        let change = [BoardOp::Replace(Item::Track(relaid.clone())), BoardOp::Replace(Item::Track(moved.clone()))];
        let applied = board.apply_all(&change).expect("the change applies");
        assert_eq!((&board.tracks[h], &board.tracks[b]), (&relaid, &moved), "both tracks land exactly as the ops wrote them");
        let after = board.clone();
        let undone = board.undo(&applied.inverse).expect("undo the change");
        assert_eq!(board.tracks, before.tracks, "undo restores both tracks exactly");
        board.undo(&undone.inverse).expect("redo the change");
        assert_eq!(board.tracks, after.tracks, "redo lands the change exactly again");
        // A change that fails part way names its op and leaves the board as it was.
        let (lost, generation) = (board.alloc_id(), board.generation);
        let bad = [BoardOp::Remove(branch), BoardOp::Replace(Item::Track(track(host, relaid.path.clone(), [Anchor::Via(lost), Anchor::Free])))];
        assert_eq!(board.apply_batch(&bad, false).map(|_| ()), Err((1, OpError::BadAnchor(host))), "an anchor to a missing via fails at the op that wrote it");
        assert_eq!((&board.tracks, board.generation), (&after.tracks, generation), "a failed change leaves the tracks and the generation");
    }

    #[test]
    fn free_ends_attach_on_add_and_free_on_remove() {
        let mut board = Board::new(BoardId(0), "free track ends");
        board.nets.push(BoardNet { name: "signal".into(), class: ClassId(0) });
        let track = |id, net, layer, start, end| BoardOp::Add(Item::Track(Track {
            id, net, layer, w: mm(0.25), widths: None, path: Path::new(start).line(end),
            ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
        }));
        let run_drc = |board: &Board| drc(board, &connect(board, &[]), &resolve_rules(board, None), &[], None);
        let a = board.alloc_id();
        let (start, end) = (P::mm(2.0, 2.0), P::mm(6.0, 2.0));
        board.apply(&track(a, NetId(0), LayerId::F_CU, start, end)).expect("add track A");
        assert_eq!(dangling_ends(&board, &board.tracks[0]), [true, true], "both ends of isolated A are dangling");

        let via = board.alloc_id();
        let added = board.apply(&BoardOp::Add(Item::Via(Via {
            id: via, net: NetId(0), at: end, drill: mm(0.3), pad: mm(0.6), span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through, tent: Tent::Both, backdrill: None, locked: false, origin: Origin::Authored, flash: Flash::All,
        }))).expect("add the via at A's end");
        assert_eq!(board.tracks[0].ends[1], Anchor::Via(via), "adding the via anchors A's end to it");
        assert!(!run_drc(&board).violations.iter().any(|v| v.kind == CheckKind::Dangling && v.at == Some(end)),
            "DRC clears the dangling finding at the via as soon as it is added");

        let removed = board.apply(&BoardOp::Remove(via)).expect("remove the via");
        assert_eq!(board.tracks[0].ends[1], Anchor::Free, "removing the via frees A's end");
        board.undo(&removed.inverse).expect("undo the via removal");
        assert_eq!(board.tracks[0].ends[1], Anchor::Via(via), "undoing removal restores A's via anchor");
        board.undo(&added.inverse).expect("undo the via addition");
        assert_eq!(board.tracks[0].ends[1], Anchor::Free, "undoing the addition restores A's free end");
        assert!(board.locate(via).is_none(), "undoing the addition removes the via from the board");

        // An added track's own free end at a via attaches too, so removing the via and undoing
        // that restores the board exactly.
        let pin = board.alloc_id();
        let pin_at = P::mm(10.0, 2.0);
        board.apply(&BoardOp::Add(Item::Via(Via {
            id: pin, net: NetId(0), at: pin_at, drill: mm(0.3), pad: mm(0.6), span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through, tent: Tent::Both, backdrill: None, locked: false, origin: Origin::Authored, flash: Flash::All,
        }))).expect("add a lone via");
        let lead = board.alloc_id();
        board.apply(&track(lead, NetId(0), LayerId::F_CU, P::mm(10.0, 6.0), pin_at)).expect("add a track ending at the via");
        let lead_ix = board.tracks.iter().position(|t| t.id == lead).expect("the lead is on the board");
        assert_eq!(board.tracks[lead_ix].ends[1], Anchor::Via(pin), "an added track's own free end at a via attaches to it");
        let before = board.clone();
        let removed = board.apply(&BoardOp::Remove(pin)).expect("remove the lone via");
        board.undo(&removed.inverse).expect("undo the lone via's removal");
        assert_eq!(board.tracks, before.tracks, "removing a via and undoing it restores every track exactly");

        let b = board.alloc_id();
        let b_start = P::mm(2.0, 0.0);
        board.apply(&track(b, NetId(0), LayerId::F_CU, b_start, P::mm(2.0, 4.0))).expect("add B through A's start");
        assert!(matches!(board.tracks[0].ends[0], Anchor::Track { id, .. } if id == b),
            "adding B anchors A's start to B's straight segment");
        assert_eq!(board.tracks[0].path.start, start, "snapping to B preserves A's junction at (2, 2)");

        board.nets.push(BoardNet { name: "other".into(), class: ClassId(0) });
        let (from, to) = (P::mm(6.0, 0.0), P::mm(6.0, 4.0));
        let other = board.alloc_id();
        board.apply(&track(other, NetId(1), LayerId::F_CU, from, to)).expect("add another net through A's end");
        assert_eq!(board.tracks[0].ends[1], Anchor::Free, "a track on another net leaves A's end free");
        let back = board.alloc_id();
        board.apply(&track(back, NetId(0), LayerId::B_CU, from, to)).expect("add back copper through A's end");
        assert_eq!(board.tracks[0].ends[1], Anchor::Free, "a same-net track on another layer leaves A's end free");

        let findings = run_drc(&board);
        let finding = findings.violations.iter().find(|v| v.kind == CheckKind::Dangling
            && v.at == Some(b_start) && v.items.contains(&Sel::Item(board.unit, b))).expect("DRC reports B's free start");
        board.rules.waivers.push(Waiver { violation: finding.id, reason: "test stub".into() });
        let b_track = board.tracks.iter().find(|t| t.id == b).expect("B is on the board");
        assert_eq!(dangling_ends(&board, b_track), [false, true], "waiving B's start leaves only its end dangling");
        assert!(!run_drc(&board).violations.iter().any(|v| v.kind == CheckKind::Dangling && v.at == Some(b_start)),
            "DRC omits the waived dangling finding at B's start");

        // A free end the ops leave where a reader attaches it is attached as the reader does,
        // whether an op writes it free or a removal frees it onto other copper there.
        let (h, s, k) = (board.alloc_id(), board.alloc_id(), board.alloc_id());
        let at = P::mm(8.0, 10.0);
        for op in [track(h, NetId(0), LayerId::F_CU, P::mm(2.0, 10.0), at), track(s, NetId(0), LayerId::F_CU, at, P::mm(8.0, 14.0))] {
            board.apply(&op).expect("add a host and a spur from its end");
        }
        let spur = |b: &Board| b.tracks.iter().find(|t| t.id == s).expect("the spur is on the board").clone();
        let attached = spur(&board).ends[0];
        assert!(matches!(attached, Anchor::Track { id, .. } if id == h), "the spur's start attaches to the host's end");
        let mut freed = spur(&board);
        freed.ends[0] = Anchor::Free;
        board.apply(&BoardOp::Replace(Item::Track(freed.clone()))).expect("a change writes the spur's start free");
        assert_eq!(spur(&board).ends[0], board.infer_anchor(&freed, at), "a start written free on the host attaches as a reader attaches it");
        board.apply(&track(k, NetId(0), LayerId::F_CU, at, P::mm(12.0, 10.0))).expect("add a second track from the junction");
        let before = board.clone();
        let removed = board.apply(&BoardOp::Remove(h)).expect("remove the host");
        assert_eq!(spur(&board).ends[0], board.infer_anchor(&spur(&board), at), "removing the host frees the spur's start onto the other track there");
        assert!(matches!(spur(&board).ends[0], Anchor::Track { id, .. } if id == k), "the spur's start attaches to the track that stays");
        board.undo(&removed.inverse).expect("undo the host's removal");
        assert_eq!(board.tracks, before.tracks, "undoing the removal restores every track exactly");
    }
}
