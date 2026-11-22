//! Per-layer via rings: on which layers of its span a via carries copper.
//!
//! The drill and the barrel are on the whole span (`Via::span_layers`) in every mode; the ring is
//! what `Via::flash` chooses. `All` and `Ends` follow from the via alone. `Connected` and
//! `ConnectedEnds` follow the copper around the via, so the op path keeps them in a table on the
//! board (`RingIndex`), beside `ItemIndex` and with the same contract: derived, never content,
//! rebuilt by `reindex`, kept by `apply` / `apply_host` / `apply_all` / `undo`.
//!
//! A ring is dropped only on a layer where no same-net copper touches it, so connectivity is the
//! same in every mode. On layer `L` of the span, a following via has
//! - a fixed ring when (`ConnectedEnds` and `L` is a span end) or same-net fixed copper on `L`
//!   touches its disc: a track segment (anchored or not), a pad, a netted plated hole's ring, or
//!   another via of the net whose span has `L` (its span, not its rings: no fixpoint);
//! - an offered ring when it has no fixed one and a zone of its net on `L` has an area that meets
//!   the disc. An offered ring is real only where a kept fill of that zone reaches it (the fill's
//!   business, not the board's).
//!
//! Cost: an edit marks the following vias of its nets near its old and new extent (a pass over the
//! table, nothing for a board without following vias); the flush after the op (once for a sequence)
//! resolves the marked ones in one pass over the board's items, net first.

use crate::ops::BoardDelta;
use crate::{Area, Board, ConnItem, Copper, Flash, ItemKind, Via, ZoneFill};
use fab_elec_core::{clearance, BBox, ItemId, LayerId, LayerSet, NetId, PadIx, Shape};
use std::collections::HashMap;

/// A via's ring layers within its span: `fixed` always carry copper, `offer` only where a fill of
/// the offering zone reaches the ring. Disjoint.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Rings {
    pub fixed: LayerSet,
    pub offer: LayerSet,
}

#[derive(Clone, Debug)]
struct Entry {
    rings: Rings,
    net: NetId,
    dbox: BBox,
    span: LayerSet,
    /// The (zone, layer) pairs that offer this via a ring, sorted.
    zones: Vec<(ItemId, LayerId)>,
}

/// The rings of the board's vias that follow the copper (`Flash::follows_copper`). Derived data:
/// never part of equality or the canonical hash.
#[derive(Clone, Default)]
pub struct RingIndex {
    vias: HashMap<ItemId, Entry>,
    /// What each zone offers: (via, layer), sorted.
    by_zone: HashMap<ItemId, Vec<(ItemId, LayerId)>>,
    /// Vias to resolve; empty outside an op sequence.
    dirty: Vec<ItemId>,
}

impl PartialEq for RingIndex {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for RingIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RingIndex({} vias, {} dirty)", self.vias.len(), self.dirty.len())
    }
}

impl RingIndex {
    /// How many vias have an entry.
    pub fn len(&self) -> usize {
        self.vias.len()
    }
    pub fn is_empty(&self) -> bool {
        self.vias.is_empty()
    }
    /// The (via, layer) rings `zone` offers, sorted.
    pub fn offers(&self, zone: ItemId) -> &[(ItemId, LayerId)] {
        self.by_zone.get(&zone).map_or(&[], |v| v.as_slice())
    }
    /// The entry's rings, without the staleness check (comparing two boards' tables).
    pub fn peek(&self, via: ItemId) -> Option<Rings> {
        self.vias.get(&via).map(|e| e.rings)
    }
    fn drop_entry(&mut self, via: ItemId) {
        let Some(old) = self.vias.remove(&via) else { return };
        self.unlink(via, &old.zones);
    }
    fn unlink(&mut self, via: ItemId, zones: &[(ItemId, LayerId)]) {
        for (zone, _) in zones {
            if let Some(list) = self.by_zone.get_mut(zone) {
                list.retain(|(v, _)| *v != via);
                if list.is_empty() {
                    self.by_zone.remove(zone);
                }
            }
        }
    }
}

/// The span's two end layers.
fn span_ends(v: &Via, span: LayerSet) -> LayerSet {
    LayerSet::EMPTY.with(v.span.0).with(v.span.1).intersect(span)
}

/// One via being resolved.
struct Work {
    id: ItemId,
    net: NetId,
    disc: Shape,
    dbox: BBox,
    span: LayerSet,
    fixed: LayerSet,
    offer: LayerSet,
    zones: Vec<(ItemId, LayerId)>,
}

impl Work {
    /// Same-net copper `shape` on `layer` touches the disc: a fixed ring there.
    fn touch(&mut self, layer: LayerId, shape: &Shape) {
        if self.span.has(layer) && !self.fixed.has(layer) && shape.bbox().intersects(&self.dbox) && clearance(shape, &self.disc) == 0 {
            self.fixed = self.fixed.with(layer);
        }
    }
}

impl Board {
    /// The via's ring layers. `All` and `Ends` follow from the via; a via that follows the copper
    /// reads the table, which the op path and `reindex` keep (a missing entry is a board whose lists
    /// were edited without either, and panics rather than resolve the board here).
    pub fn rings(&self, v: &Via) -> Rings {
        let span = v.span_layers(self.copper_set());
        match v.flash {
            Flash::All => Rings { fixed: span, offer: LayerSet::EMPTY },
            Flash::Ends => Rings { fixed: span_ends(v, span), offer: LayerSet::EMPTY },
            Flash::Connected | Flash::ConnectedEnds => {
                assert!(self.rings.dirty.is_empty(), "ring table stale: reindex");
                self.rings.vias.get(&v.id).expect("ring table stale: reindex").rings
            }
        }
    }

    /// The via's offered rings as copper, one per offered layer: what the fill takes as the via's
    /// copper and the router keeps clear of (the next refill can flash them). Final copper is
    /// `item_copper` plus the reached ones (`reached_copper`).
    pub fn offer_copper(&self, v: &Via, out: &mut Vec<Copper>) {
        let offer = self.rings(v).offer;
        if offer.is_empty() {
            return;
        }
        let class = crate::zone::class_of(self, Some(v.net));
        out.extend(offer.iter().map(|layer| Copper { item: ConnItem::Via(v.id), net: Some(v.net), class, layer, shape: v.shape() }));
    }

    /// The offered rings `fill` reaches that its zone offers on this board, (via, layer), sorted: a
    /// held fill from an older generation names only what the board still offers.
    pub fn reached_by(&self, fill: &ZoneFill) -> Vec<(ItemId, LayerId)> {
        let offers = self.rings.offers(fill.zone);
        if offers.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(ItemId, LayerId)> = fill
            .layers
            .iter()
            .flat_map(|layer| layer.reached.iter().map(move |via| (*via, layer.layer)))
            .filter(|pair| offers.binary_search(pair).is_ok())
            .collect();
        out.sort_unstable();
        out
    }

    /// The offered rings any of `fills` reaches, (via, layer), sorted and deduplicated.
    pub fn reached_rings(&self, fills: &[ZoneFill]) -> Vec<(ItemId, LayerId)> {
        let mut out: Vec<(ItemId, LayerId)> = fills.iter().flat_map(|fill| self.reached_by(fill)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The copper of the reached rings, for readers that take `all_copper` and the fills without an
    /// index (an index holds them as `Copper` pieces already).
    pub fn reached_copper(&self, fills: &[ZoneFill]) -> Vec<Copper> {
        self.reached_rings(fills)
            .into_iter()
            .filter_map(|(via, layer)| {
                let Some((ItemKind::Via, i)) = self.locate(via) else { return None };
                let v = &self.vias[i];
                let class = crate::zone::class_of(self, Some(v.net));
                Some(Copper { item: ConnItem::Via(v.id), net: Some(v.net), class, layer, shape: v.shape() })
            })
            .collect()
    }

    /// Rebuild the ring table from the board (after `ItemIndex`: it locates through it).
    pub(crate) fn reindex_rings(&mut self) {
        self.rings = RingIndex::default();
        self.rings.dirty = self.vias.iter().filter(|v| v.flash.follows_copper()).map(|v| v.id).collect();
        self.flush_rings();
    }

    /// After an op or a sequence: resolve the vias the delta can have changed, and name every via
    /// whose rings changed in the delta (with its extent), so the engines that follow the delta
    /// re-piece it.
    pub(crate) fn settle_rings(&mut self, delta: &mut BoardDelta) {
        self.mark_rings(delta);
        let mut grew = false;
        for id in self.flush_rings() {
            if delta.touches(id) {
                continue;
            }
            let Some(item) = self.item(id) else { continue };
            delta.bbox = delta.bbox.union(item.extent(self));
            delta.extents.push(item.fill_extent(self));
            delta.nets.extend(item.nets());
            delta.changed.push(id);
            grew = true;
        }
        if grew {
            delta.nets.sort();
            delta.nets.dedup();
        }
    }

    fn follows_copper(&self, id: ItemId) -> bool {
        matches!(self.locate(id), Some((ItemKind::Via, i)) if self.vias[i].flash.follows_copper())
    }

    /// Mark the following vias the delta can have changed: those it names, and those of its nets
    /// whose disc box meets one of its extents on a span layer. A global delta marks them all.
    fn mark_rings(&mut self, delta: &BoardDelta) {
        let mut dirty = std::mem::take(&mut self.rings.dirty);
        for &id in delta.added.iter().chain(&delta.changed) {
            if self.follows_copper(id) {
                dirty.push(id);
            }
        }
        for &id in delta.removed.iter().chain(&delta.changed) {
            if self.rings.vias.contains_key(&id) {
                dirty.push(id);
            }
        }
        if delta.global {
            dirty.extend(self.rings.vias.keys().copied());
            dirty.extend(self.vias.iter().filter(|v| v.flash.follows_copper()).map(|v| v.id));
        } else {
            for (id, e) in &self.rings.vias {
                if delta.nets.contains(&e.net)
                    && delta.extents.iter().any(|x| !x.layers.intersect(e.span).is_empty() && x.bbox.intersects(&e.dbox))
                {
                    dirty.push(*id);
                }
            }
        }
        self.rings.dirty = dirty;
    }

    /// Resolve the marked vias; the ids whose rings changed (a new entry is not a change: its via is
    /// in the delta already).
    fn flush_rings(&mut self) -> Vec<ItemId> {
        if self.rings.dirty.is_empty() {
            return Vec::new();
        }
        let mut dirty = std::mem::take(&mut self.rings.dirty);
        dirty.sort();
        dirty.dedup();
        let copper = self.copper_set();
        let n = self.copper_count();
        let mut work: Vec<Work> = Vec::new();
        for id in dirty {
            match self.locate(id) {
                Some((ItemKind::Via, i)) if self.vias[i].flash.follows_copper() => {
                    let v = &self.vias[i];
                    let span = v.span_layers(copper);
                    let disc = v.shape();
                    let fixed = if v.flash == Flash::ConnectedEnds { span_ends(v, span) } else { LayerSet::EMPTY };
                    work.push(Work {
                        id, net: v.net, dbox: disc.bbox(), disc, span, fixed, offer: LayerSet::EMPTY, zones: Vec::new(),
                    });
                }
                _ => self.rings.drop_entry(id),
            }
        }
        if work.is_empty() {
            return Vec::new();
        }
        let mut by_net: HashMap<NetId, Vec<usize>> = HashMap::new();
        for (k, w) in work.iter().enumerate() {
            by_net.entry(w.net).or_default().push(k);
        }

        for t in &self.tracks {
            let Some(group) = by_net.get(&t.net) else { continue };
            let tb = t.bbox();
            for &k in group {
                let w = &mut work[k];
                if !w.span.has(t.layer) || w.fixed.has(t.layer) || !tb.intersects(&w.dbox) {
                    continue;
                }
                for (si, (a, seg)) in t.path.steps().enumerate() {
                    w.touch(t.layer, &t.seg_shape(si, a, &seg));
                    if w.fixed.has(t.layer) {
                        break;
                    }
                }
            }
        }
        for p in &self.placed {
            for (pi, net) in p.pad_nets.iter().enumerate() {
                let Some(group) = net.and_then(|net| by_net.get(&net)) else { continue };
                let pix = PadIx(pi as u16);
                for l in p.pad_layers(pix, copper, n).iter() {
                    let Some(shape) = p.pad_shape(pix, l, n) else { continue };
                    for &k in group {
                        work[k].touch(l, &shape);
                    }
                }
            }
        }
        for h in &self.holes {
            let (true, Some(pad), Some(net)) = (h.plated, h.pad, h.net) else { continue };
            let Some(group) = by_net.get(&net) else { continue };
            let ring = Shape::Circle { c: h.at, r: pad / 2 };
            for &k in group {
                for l in copper.iter() {
                    work[k].touch(l, &ring);
                }
            }
        }
        for v in &self.vias {
            let Some(group) = by_net.get(&v.net) else { continue };
            let span = v.span_layers(copper);
            let disc = v.shape();
            for &k in group {
                let w = &mut work[k];
                let layers = span.intersect(w.span).minus(w.fixed);
                if v.id != w.id && !layers.is_empty() && disc.bbox().intersects(&w.dbox) && clearance(&disc, &w.disc) == 0 {
                    w.fixed = w.fixed.union(layers);
                }
            }
        }
        // Offers last: a layer with a fixed ring is offered nothing.
        for z in &self.zones {
            let Some(group) = z.net.and_then(|net| by_net.get(&net)) else { continue };
            let mut area: Option<Shape> = None;
            for &k in group {
                let w = &mut work[k];
                let layers = z.layers.intersect(w.span).minus(w.fixed);
                if layers.is_empty() {
                    continue;
                }
                let meets = match &z.area {
                    Area::Board => true,
                    Area::Poly(p) => {
                        p.bbox().intersects(&w.dbox) && {
                            let shape = area.get_or_insert_with(|| Shape::Poly(p.clone()));
                            clearance(shape, &w.disc) == 0
                        }
                    }
                };
                if meets {
                    w.offer = w.offer.union(layers);
                    w.zones.extend(layers.iter().map(|l| (z.id, l)));
                }
            }
        }

        let mut changed = Vec::new();
        for mut w in work {
            w.zones.sort();
            let rings = Rings { fixed: w.fixed, offer: w.offer };
            if let Some(old) = self.rings.vias.get(&w.id) {
                if old.rings != rings {
                    changed.push(w.id);
                }
            }
            self.rings.drop_entry(w.id);
            for &(zone, l) in &w.zones {
                let list = self.rings.by_zone.entry(zone).or_default();
                let at = list.partition_point(|x| *x < (w.id, l));
                list.insert(at, (w.id, l));
            }
            self.rings.vias.insert(w.id, Entry { rings, net: w.net, dbox: w.dbox, span: w.span, zones: w.zones });
        }
        changed
    }
}
