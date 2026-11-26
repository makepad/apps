//! The placement problem uses only the board's footprint data and pad-to-net assignments.

use fab_elec_board::placement::placed_courtyard;
use fab_elec_board::{Board, Placed};
use fab_elec_core::{Angle, BBox, CompId, ItemId, NetId, Nm, PadIx, PolySet, Side, P};
use fab_elec_parts::{FpMount, PadKind};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pose {
    pub at: P,
    pub rot: Angle,
    pub side: Side,
}

impl From<&Placed> for Pose {
    fn from(p: &Placed) -> Self {
        Self { at: p.at, rot: p.rot, side: p.side }
    }
}

/// Geometry in footprint coordinates. A missing courtyard uses the same local box
/// that `Placed::bbox` transforms. The fallback guides placement without creating a DRC rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyShape {
    Courtyard(PolySet),
    Bounds(BBox),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Movable {
    pub comp: CompId,
    pub item: ItemId,
    pub shape: BodyShape,
    pub pads: Vec<P>,
    pub pad_nets: Vec<Option<NetId>>,
    pub through_hole: bool,
    pub body_z: Option<(Nm, Nm)>,
    pub start: Pose,
}

impl Movable {
    fn new(p: &Placed) -> Self {
        let fp = &p.footprint;
        let shape = if fp.courtyard.is_empty() {
            let b = fp.bbox();
            BodyShape::Bounds(if b.is_empty() { BBox::point(P::ZERO) } else { b })
        } else {
            BodyShape::Courtyard(fp.courtyard.clone())
        };
        Self {
            comp: p.comp, item: p.id, shape, pads: fp.pads.iter().map(|pad| pad.at).collect(),
            pad_nets: p.pad_nets.clone(),
            through_hole: fp.attrs.mount == FpMount::Thru || fp.pads.iter().any(|p| p.kind == PadKind::Thru),
            body_z: p.body_z, start: p.into(),
        }
    }

    /// Mirror in footprint coordinates before rotating and translating, as in `Placed::pad_at`.
    pub fn pad_at(&self, pad: PadIx, pose: Pose) -> P {
        let p = self.pads[pad.ix()];
        let p = if pose.side == Side::Bottom { p.mirrored_x(0) } else { p };
        p.rotated(pose.rot).add(pose.at)
    }
}

/// A fixed footprint in board coordinates. Its connected pads appear as fixed pins in the nets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Obstacle {
    pub item: ItemId,
    pub side: Side,
    pub courtyard: PolySet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinRef {
    Mov(usize, PadIx),
    Fixed(P),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PNet {
    pub net: NetId,
    /// Ordered by (item id, pad index), the ratsnest's anchor order for unconnected pads.
    pub pins: Vec<PinRef>,
    /// Q32. A zone-served net has zero wirelength weight. Above twelve pins the weight is 12/n.
    pub weight: i64,
    // Kept per pin: two fixed pads can share a position on opposite sides.
    pub(super) start_sides: Vec<Side>,
}

impl PNet {
    pub const UNIT_WEIGHT: i64 = 1 << 32;

    pub(super) fn wirelength(&self, bbox: BBox) -> i64 {
        let hpwl = bbox.width() + bbox.height();
        (i128::from(hpwl) * i128::from(self.weight) / i128::from(Self::UNIT_WEIGHT)) as i64
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceProblem {
    pub movables: Vec<Movable>,
    pub obstacles: Vec<Obstacle>,
    pub nets: Vec<PNet>,
}

impl PlaceProblem {
    /// Empty scope selects all editable, unlocked placements. Locks, attached block
    /// members and derived items are fixed even when explicitly named.
    pub fn new(board: &Board, scope: &[CompId]) -> Self {
        let scope: BTreeSet<_> = scope.iter().copied().collect();
        let mut parts: Vec<_> = board.placed.iter().collect();
        parts.sort_unstable_by_key(|p| p.id);
        let mut movables = Vec::new();
        let mut obstacles = Vec::new();
        let mut nets = BTreeMap::new();
        for part in parts {
            let movable = if !part.locked && part.origin.has_statement() && (scope.is_empty() || scope.contains(&part.comp)) {
                let i = movables.len();
                movables.push(Movable::new(part));
                Some(i)
            } else {
                obstacles.push(Obstacle { item: part.id, side: part.side, courtyard: placed_courtyard(part) });
                None
            };
            for (i, (&net, pad)) in part.pad_nets.iter().zip(&part.footprint.pads).enumerate() {
                let Some(net) = net else { continue };
                let n = nets.entry(net).or_insert_with(|| PNet {
                    net, pins: Vec::new(), weight: PNet::UNIT_WEIGHT, start_sides: Vec::new(),
                });
                let pin = match movable {
                    Some(ix) => PinRef::Mov(ix, PadIx(i as u16)),
                    None => PinRef::Fixed(part.to_board(pad.at)),
                };
                n.pins.push(pin);
                n.start_sides.push(part.side);
            }
        }
        let served: BTreeSet<_> = board.zones.iter().filter_map(|z| z.net).collect();
        for n in nets.values_mut() {
            if served.contains(&n.net) {
                n.weight = 0;
            } else if n.pins.len() > 12 {
                n.weight = 12 * PNet::UNIT_WEIGHT / n.pins.len() as i64;
            }
        }
        Self { movables, obstacles, nets: nets.into_values().collect() }
    }
}
