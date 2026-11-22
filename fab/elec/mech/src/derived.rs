//! Place generated board features and publish them as a part's derived set.

use fab_elec_board::{BoardDelta, BoardKeepout, Item, Placed};
use fab_elec_core::{mm, BoardId, FeatureRef, LayerId, LayerSet, LayerSide, PolySet, Side, P};
use fab_elec_design::{ops::DesignOpError, Design};
use fab_elec_parts::Forbid;

/// Place holes, cutouts and keepouts from a footprint frame into the board frame.
/// Other item kinds pass through unchanged. Bottom placements mirror x before rotation
/// and translation, and swap outer and technical layer pairs; inner layers stay unchanged.
pub fn place(mut items: Vec<Item>, at: &Placed) -> Vec<Item> {
    for item in &mut items {
        match item {
            Item::Hole(hole) => hole.at = at.to_board(hole.at),
            Item::Cutout(cutout) => {
                cutout.contour.path = cutout.contour.path.mapped(|p| at.to_board(p));
            }
            Item::Keepout(keepout) => {
                if at.side == Side::Bottom {
                    keepout.area = keepout.area.mirrored_x(0);
                    keepout.layers = keepout.layers.iter().fold(LayerSet::EMPTY, |layers, layer| {
                        layers.with(match layer.side() {
                            LayerSide::Front => layer.on(Side::Bottom),
                            LayerSide::Back => layer.on(Side::Top),
                            LayerSide::Neither => layer,
                        })
                    });
                }
                keepout.area = keepout.area.rotated_about(P::ZERO, at.rot).translated(at.at);
            }
            _ => {}
        }
    }
    items
}

/// The board space a case part keeps clear (an `m.keepouts` record), as a board keepout: `area`
/// its outline in the board's frame and `z` its band above the board's top surface, in mm, and
/// `reason` what keeps it clear. It sits on the top copper side and forbids footprints: a part
/// whose body reaches the band there is a DRC error, one under it (an LED under its light pipe) is
/// not. Its band starts above the surface, so it never catches copper. O(points).
pub fn case_keepout(reason: &str, area: &[[f64; 2]], z: [f64; 2]) -> Item {
    Item::Keepout(BoardKeepout {
        id: Default::default(),
        area: PolySet::from_ring_nonzero(area.iter().map(|p| P::new(mm(p[0]), mm(p[1]))).collect()),
        layers: LayerSet::of(&[LayerId::F_CU]),
        forbid: Forbid::FOOTPRINTS,
        reason: reason.into(),
        origin: Default::default(),
        z: Some((mm(z[0]), mm(z[1]))),
    })
}

/// Replace the board items owned by the generated part `part`.
pub fn publish(design: &mut Design, unit: BoardId, part: &str, items: Vec<Item>) -> Result<Option<BoardDelta>, DesignOpError> {
    design.set_derived(unit, &FeatureRef::Generated(part.into()), items)
}
