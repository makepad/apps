//! The routing world as the grid's obstacle source (E6 4, gate 3 change 2). The grid holds no
//! rule logic: copper gaps come from `fab_elec_board::required_to` between a probe piece of
//! copper (the plane's class, item kind and no net) and a world item. A via's drill can need more
//! room than its pad: existing copper must keep hole clearance from it, and existing drills
//! must keep hole-to-hole clearance. These are the verifier's `check_to` and `check_drills`
//! rules, converted to the pad frame by adding the drill's rounded-up radius and subtracting
//! the pad's rounded-up radius. An obstacle that needs no gap is not visited.
//!
//! Owners: copper and drills of a net are rippable (`Owner::Net`) when the caller's `rippable`
//! says so, otherwise fixed (`Owner::Fixed`); pads and pad drills are always fixed; edges,
//! keepouts and netless copper or holes are hard.
//!
//! A via plane also keeps the drill wall clear of each pad by its mask expansion plus a minimum
//! mask web, with a floor of 1 nm. It reads the placed pad's expansion through
//! `RuleMatrix::via_drill_to_board_pad` and converts the gap to the via pad's frame. When
//! `RuleMatrix::via_in_pad` allows the probe's class, the via is filled and capped: its drill
//! only keeps off pad copper (1 nm), as DRC judges it, and a netted pad has `Owner::Fixed(net)`,
//! so only its own net may drill in it. Netless pads are hard either way. The ordinary copper and
//! drill clearances still apply to another net's pad.
//!
//! Construction scans the board's pads once for the largest drill-to-pad gap. Each tile build
//! makes one world query for the items in its reach.

use crate::grid::{ObstacleSource, Owner, Probe};
use crate::world::{RItem, RPiece, World};
use fab_elec_board::{required_to, Board, ConnItem, Copper, Obstacle, RuleMatrix};
use fab_elec_core::{clearance, point_clearance, BBox, ItemId, LayerId, NetId, Nm, Shape, P};
use std::ops::ControlFlow;

/// The item id the probe copper carries: never a board item's, so no same-item exemption applies.
const PROBE: ItemId = ItemId(u32::MAX);

/// A world, its rules and the rip policy, seen by the grid.
pub struct WorldSource<'a> {
    pub world: &'a World<'a>,
    pub rules: &'a RuleMatrix,
    pub board: &'a Board,
    drill_pad_max: Nm,
    /// May the router rip this item (`RouteProblem::owned` for a job; solved and unlocked for a
    /// gesture)? Pads are never ripped whatever it says.
    pub rippable: &'a dyn Fn(&RItem) -> bool,
}

impl<'a> WorldSource<'a> {
    /// Bind the world to its board snapshot and bound the search for pad obstacles.
    pub fn new(world: &'a World<'a>, rules: &'a RuleMatrix, board: &'a Board, rippable: &'a dyn Fn(&RItem) -> bool) -> Self {
        Self { world, rules, board, drill_pad_max: rules.max_via_drill_to_pad(board), rippable }
    }

    /// The obstacle's need in the probe's width/pad frame, including a via's drill.
    fn need(&self, probe: &Probe, layer: LayerId, it: &RItem) -> Nm {
        let base = required_to(self.rules, &probe_copper(probe, layer), &it.obstacle(), layer).gap.max(0);
        let Some(d) = probe.drill else { return base };
        let drill_need = match &it.piece {
            RPiece::Copper(c) => {
                let drill_circle = Shape::Circle { c: P::ZERO, r: d / 2 };
                let drill = Obstacle::Drill { item: ConnItem::Via(PROBE), net: None, shape: &drill_circle };
                required_to(self.rules, c, &drill, layer).gap
            }
            RPiece::Drill { .. } if self.rules.hole_to_hole() > 0 => self.rules.hole_to_hole(),
            _ => return base,
        };
        let radius_delta = (i64::from(d) + 1) / 2 - (i64::from(probe.w) + 1) / 2;
        let need = i64::from(base).max(i64::from(drill_need) + radius_delta);
        Nm::try_from(need).unwrap_or(Nm::MAX)
    }

    /// The grid owner of a world item.
    pub fn owner(&self, it: &RItem) -> Owner {
        let Some(net) = it.net() else { return Owner::Hard };
        let pad = matches!(&it.piece, RPiece::Copper(Copper { item: ConnItem::Pad { .. }, .. }) | RPiece::Drill { item: ConnItem::Pad { .. }, .. });
        if !pad && (self.rippable)(it) {
            Owner::Net(net)
        } else {
            Owner::Fixed(net)
        }
    }
}

impl WorldSource<'_> {
    /// The rippable nets other than `net` whose copper owns the step from `a` to `b` on `layer`
    /// for `probe` (`a == b`: the centre), by the grid's exact step rule: what a shared step's
    /// owners are. Sorted and deduplicated.
    pub fn owners_on(&self, probe: &Probe, layer: LayerId, a: P, b: P, net: NetId) -> Vec<NetId> {
        let half = (i64::from(probe.w) + 1) / 2;
        let reach = i64::from(self.max_need(probe)) + half;
        let reach = Nm::try_from(reach).unwrap_or(Nm::MAX);
        let mut out = Vec::new();
        self.world.query(layer, BBox::new(a, b).inflate(reach), |_, it| {
            if let Owner::Net(n) = self.owner(it) {
                let need = self.need(probe, layer, it);
                if n != net && need > 0 {
                    let gap = if a == b {
                        point_clearance(a, it.shape())
                    } else {
                        clearance(&Shape::Segment { a, b, w: 0 }, it.shape())
                    };
                    if i64::from(gap) < i64::from(need) + half {
                        out.push(n);
                    }
                }
            }
            ControlFlow::Continue(())
        });
        out.sort();
        out.dedup();
        out
    }

    /// The rippable copper of nets other than `net` that owns the step from `a` to `b` on `layer`
    /// for `probe` (`a == b`: the centre), by the step rule of [`WorldSource::owners_on`]: what a
    /// blockers search crossed there, by item. In world order, each once.
    pub fn in_way_on(&self, probe: &Probe, layer: LayerId, a: P, b: P, net: NetId) -> Vec<ConnItem> {
        let half = (i64::from(probe.w) + 1) / 2;
        let reach = Nm::try_from(i64::from(self.max_need(probe)) + half).unwrap_or(Nm::MAX);
        let mut out: Vec<ConnItem> = Vec::new();
        self.world.query(layer, BBox::new(a, b).inflate(reach), |_, it| {
            let item = match &it.piece {
                RPiece::Copper(c) => c.item,
                RPiece::Drill { item, .. } => *item,
                _ => return ControlFlow::Continue(()),
            };
            if matches!(self.owner(it), Owner::Net(n) if n != net) && !out.contains(&item) {
                let need = self.need(probe, layer, it);
                let gap = if a == b { point_clearance(a, it.shape()) } else { clearance(&Shape::Segment { a, b, w: 0 }, it.shape()) };
                if need > 0 && i64::from(gap) < i64::from(need) + half {
                    out.push(item);
                }
            }
            ControlFlow::Continue(())
        });
        out
    }
}

/// The copper a probe stands for on `layer` (its shape is never read by the rules).
pub fn probe_copper(probe: &Probe, layer: LayerId) -> Copper {
    let item = if probe.drill.is_some() { ConnItem::Via(PROBE) } else { ConnItem::Track { item: PROBE, seg: 0 } };
    Copper { item, net: None, class: probe.class, layer, shape: Shape::Circle { c: P::ZERO, r: probe.w / 2 } }
}

impl ObstacleSource for WorldSource<'_> {
    fn visit(&self, layer: LayerId, area: BBox, probe: &Probe, f: &mut dyn FnMut(&Shape, Owner, Nm)) {
        self.world.query(layer, area, |_, it| {
            let need = self.need(probe, layer, it);
            if need > 0 {
                f(it.shape(), self.owner(it), need);
            }
            if let Some(d) = probe.drill {
                if let RPiece::Copper(Copper { item: ConnItem::Pad { placed, pad }, .. }) = &it.piece {
                    let radius_delta = (i64::from(d) + 1) / 2 - (i64::from(probe.w) + 1) / 2;
                    // A via its class lets sit in pads is filled and capped, so no solder wicks
                    // and its drill only keeps off the copper (as DRC judges it).
                    let (owner, drill_gap) = if self.rules.via_in_pad(probe.class) {
                        (it.net().map_or(Owner::Hard, Owner::Fixed), 1)
                    } else {
                        (Owner::Hard, i64::from(self.rules.via_drill_to_board_pad(self.board, *placed, *pad)))
                    };
                    f(it.shape(), owner, Nm::try_from(drill_gap + radius_delta).unwrap_or(Nm::MAX));
                }
            }
            ControlFlow::Continue(())
        });
    }

    fn max_need(&self, probe: &Probe) -> Nm {
        let base = self.rules.max_clearance().max(self.rules.hole_clearance()).max(self.rules.edge()).max(1);
        let Some(d) = probe.drill else { return base };
        let radius_delta = (i64::from(d) + 1) / 2 - (i64::from(probe.w) + 1) / 2;
        let need = i64::from(base.max(self.rules.hole_to_hole())) + radius_delta.max(0);
        Nm::try_from(need.max(i64::from(self.drill_pad_max) + radius_delta)).unwrap_or(Nm::MAX)
    }
}
