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
/// its outline in the board's frame, `z` its band in mm out from the board surface on `side`, and
/// `reason` what keeps it clear. It sits on that side's outer copper layer and forbids footprints:
/// a part on that side whose body reaches the band is a DRC error, one under it (an LED under its
/// light pipe) is not. It never catches copper. O(points).
pub fn case_keepout(reason: &str, area: &[[f64; 2]], side: Side, z: [f64; 2]) -> Item {
    Item::Keepout(BoardKeepout {
        id: Default::default(),
        area: PolySet::from_ring_nonzero(area.iter().map(|p| P::new(mm(p[0]), mm(p[1]))).collect()),
        layers: LayerSet::of(&[LayerId::F_CU.on(side)]),
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

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::placement::{Conflict, PlacementIndex};

    #[test]
    fn a_bottom_keepout_only_catches_a_body_in_its_band_on_the_bottom() {
        let design = fab_elec_fixtures::ledctl();
        let mut board = design.units[0].board.as_deref().unwrap().clone();
        let area = [[-2.0, -2.0], [2.0, -2.0], [2.0, 2.0], [-2.0, 2.0]];
        let Item::Keepout(mut pin) = case_keepout("Rest pin", &area, Side::Bottom, [0.0, 3.0]) else { unreachable!() };
        assert_eq!(pin.layers, LayerSet::of(&[LayerId::B_CU]), "a bottom record sits on the bottom copper layer");
        assert_eq!(pin.z, Some((0, mm(3.0))), "its band keeps the record's millimetres");
        pin.id = board.alloc_id();
        let owner = pin.id;
        board.keepouts.push(pin);
        let part = board.placed[0].id;
        let catches = |board: &fab_elec_board::Board, side| {
            let index = PlacementIndex::new(board, None);
            index.pose_conflicts(part, P::ZERO, board.placed[0].rot, side)
                .any(|c| matches!(c, Conflict::Keepout { owner: id, .. } if id == owner))
        };
        board.placed[0].body_z = Some((mm(1.0), mm(2.0)));
        assert!(catches(&board, Side::Bottom), "a bottom-side body in the band is caught");
        assert!(!catches(&board, Side::Top), "the same body on the top side is not");
        board.placed[0].body_z = Some((mm(3.0), mm(4.0)));
        assert!(!catches(&board, Side::Bottom), "a bottom-side body past the band is not");
    }
}
