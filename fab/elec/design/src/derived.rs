use crate::ops::DesignOpError;
use crate::Design;
use fab_elec_board::{BoardDelta, BoardOp, Item, Origin};
use fab_elec_core::{BoardId, FeatureRef};
use std::sync::Arc;

impl Design {
    /// The items of unit `unit`'s board derived from `from`, in board order: placed, tracks,
    /// vias, zones, keepouts, holes, cutouts, graphics, texts, tunings; list order within each.
    /// Empty for a missing unit or a unit with no board. Cost: O(items on the board) to collect.
    pub fn derived(&self, unit: BoardId, from: &FeatureRef) -> Vec<Item> {
        let Some(board) = self.unit(unit).and_then(|u| u.board.as_ref()) else { return Vec::new() };
        let mut items = Vec::new();
        macro_rules! collect {
            ($list:ident, $kind:ident) => {
                items.extend(board.$list.iter()
                    .filter(|item| matches!(&item.origin, Origin::Derived { from: source } if source == from))
                    .cloned().map(Item::$kind));
            };
        }
        collect!(placed, Placed);
        collect!(tracks, Track);
        collect!(vias, Via);
        collect!(zones, Zone);
        collect!(keepouts, Keepout);
        collect!(holes, Hole);
        collect!(cutouts, Cutout);
        collect!(graphics, Graphic);
        collect!(texts, Text);
        collect!(tunings, Tuning);
        items
    }

    /// The sources of unit `unit`'s derived items, each once, in board order. Empty for a
    /// missing unit or a unit with no board. Cost: O(items on the board x sources).
    pub fn derived_sources(&self, unit: BoardId) -> Vec<FeatureRef> {
        let Some(board) = self.unit(unit).and_then(|u| u.board.as_ref()) else { return Vec::new() };
        let mut sources: Vec<FeatureRef> = Vec::new();
        macro_rules! scan {
            ($($list:ident),*) => {
                $(for item in &board.$list {
                    if let Origin::Derived { from } = &item.origin {
                        if !sources.contains(from) {
                            sources.push(from.clone());
                        }
                    }
                })*
            };
        }
        scan!(placed, tracks, vias, zones, keepouts, holes, cutouts, graphics, texts, tunings);
        sources
    }

    /// Replace unit `unit`'s items derived from `from` by `items`: a derived layer with no undo
    /// step, never written to text. Each new item takes `Origin::Derived { from }`; items of
    /// each kind pair in order and retain their old ids. Equal sets, ids aside, return
    /// `Ok(None)` without applying anything or bumping a generation. Changes apply as host
    /// ops, with their merged delta returned; an error undoes the ops already applied.
    /// A missing unit or board returns `DesignOpError::NoUnit(unit)`.
    ///
    /// Cost: O(items on the board) to collect, O(k log k) to stably sort the new items, and
    /// O(k) pairing for k derived items, plus content cloning/comparison. Changed sets also
    /// pay for the board's copy on write and one board batch of the changes.
    pub fn set_derived(&mut self, unit: BoardId, from: &FeatureRef, items: Vec<Item>) -> Result<Option<BoardDelta>, DesignOpError> {
        if self.unit(unit).and_then(|u| u.board.as_ref()).is_none() {
            return Err(DesignOpError::NoUnit(unit));
        }
        let old = self.derived(unit, from);
        let mut new: Vec<_> = items.into_iter().map(|item| item.with_origin(Origin::Derived { from: from.clone() })).collect();
        // ItemKind's declaration order is the board's list order.
        new.sort_by_key(|item| item.kind() as u8);
        let mut old = old.into_iter().peekable();
        let mut removes = Vec::new();
        let mut replaces = Vec::new();
        let mut adds = Vec::new();
        for item in new {
            while let Some(before) = old.next_if(|before| (before.kind() as u8) < item.kind() as u8) {
                removes.push(BoardOp::Remove(before.id()));
            }
            if let Some(before) = old.next_if(|before| before.kind() == item.kind()) {
                let item = item.with_id(before.id());
                if item != before {
                    replaces.push(BoardOp::Replace(item));
                }
            } else {
                adds.push(item);
            }
        }
        removes.extend(old.map(|item| BoardOp::Remove(item.id())));
        if removes.is_empty() && replaces.is_empty() && adds.is_empty() {
            return Ok(None);
        }

        let u = self.units.iter_mut().find(|u| u.id == unit).ok_or(DesignOpError::NoUnit(unit))?;
        let board = Arc::make_mut(u.board.as_mut().ok_or(DesignOpError::NoUnit(unit))?);
        let adds = adds.into_iter().map(|item| BoardOp::Add(item.with_id(board.alloc_id())));
        let ops: Vec<BoardOp> = removes.into_iter().chain(replaces).chain(adds).collect();
        let applied = board.apply_batch(&ops, true).map_err(|(_, error)| DesignOpError::Board(error))?;
        self.generation += ops.len() as u64;
        Ok(Some(applied.delta))
    }
}
