//! Test helper: a tiled fill built from one polygon set.

use fab_elec_board::zone::{FillLattice, FillLayer, FillTile, PolyRef, TileKey, TileLabels};
use fab_elec_board::ZoneFill;
use fab_elec_core::{ItemId, LayerId, PolySet};
use std::sync::Arc;

/// A fill of `zone` with one kept polygon (`poly`) on `layer`, in one tile of the level-14 lattice.
pub fn one_poly(zone: ItemId, layer: LayerId, poly: PolySet) -> ZoneFill {
    let key = TileKey { x: 0, y: 0 };
    let tile = FillTile {
        key, polys: vec![poly], own: vec![true],
        cuts: Default::default(), node: vec![0], group: vec![0], foreign: vec![false],
        core_cuts: Default::default(), lone: Default::default(), hash: Default::default(),
    };
    let island = vec![PolyRef { tile: key, ord: 0 }];
    let labels = TileLabels { island, kept: vec![true], view_hash: Default::default() };
    ZoneFill { zone, lattice: FillLattice { level: 14 }, layers: vec![FillLayer { layer, tiles: vec![Arc::new(tile)], labels: vec![labels], reached: Vec::new() }] }
}
