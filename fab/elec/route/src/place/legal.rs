//! The legaliser: every movable part on a legal spot, each as near as it can be to where it
//! was. It only translates; rotation and side stay as given.

use super::free::Free;
use super::{BodyShape, PlaceProblem, Pose};
use fab_elec_board::{Board, PortRoom};
use fab_elec_core::{CompId, Nm};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegalOpts {
    /// The lattice the search falls back to for round and odd shapes; flush spots are exact.
    pub grid: Nm,
    /// Parts that keep their spot if it is legal against what does not move, ahead of the rest:
    /// a part the person just dropped. The others move round them.
    pub pin: Vec<CompId>,
    /// Parts fixed to an edge beyond the board's own (`Board::edge_parts`): their bodies may
    /// overhang the outline.
    pub edge: Vec<CompId>,
    /// Port geometry relative to its owner's position in board axes.
    pub rooms: Vec<PortRoom>,
}

impl Default for LegalOpts {
    fn default() -> Self {
        Self { grid: 50_000, pin: Vec::new(), edge: Vec::new(), rooms: Vec::new() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegalReport {
    /// The movables that moved, by index, with how far.
    pub moved: Vec<(usize, Nm)>,
    /// The movables with no legal spot on the board: they keep the pose they came with.
    pub unplaced: Vec<usize>,
}

impl LegalReport {
    pub fn max_move(&self) -> Nm {
        self.moved.iter().map(|m| m.1).max().unwrap_or(0)
    }
}

/// Make `poses` (one per movable) legal. Pinned parts go first: each keeps its spot when that
/// is legal against what does not move, or takes the nearest legal one. Then every other part
/// that is legal where it is stays, larger parts first (then by `CompId`), so a legal board
/// answers no move at all. Each part left takes the nearest legal spot in the same order and
/// becomes an obstacle for the next. A part with no room is listed, not an error.
/// Cost: O(n log n) plus one `Free::nearest` per part that moves.
pub fn legalise(board: &Board, problem: &PlaceProblem, poses: &mut [Pose], opts: &LegalOpts) -> LegalReport {
    assert_eq!(poses.len(), problem.movables.len(), "one pose per movable");
    let moving: BTreeSet<_> = problem.movables.iter().map(|m| m.item).collect();
    let edge = problem.movables.iter().filter(|m| opts.edge.contains(&m.comp) || board.edge_parts.contains(&m.comp))
        .map(|m| m.item).collect();
    let mut free = Free::with_rooms(board, &moving, edge, &opts.rooms);
    let area = |i: usize| match &problem.movables[i].shape {
        BodyShape::Courtyard(c) => c.area(),
        BodyShape::Bounds(b) => b.width() as f64 * b.height() as f64,
    };
    let pinned = |i: usize| opts.pin.iter().position(|&c| c == problem.movables[i].comp);
    let mut order: Vec<usize> = (0..poses.len()).collect();
    order.sort_by(|&a, &b| {
        let rank = |i| pinned(i).unwrap_or(usize::MAX);
        rank(a).cmp(&rank(b)).then(area(b).total_cmp(&area(a))).then(problem.movables[a].comp.cmp(&problem.movables[b].comp))
    });
    let mut report = LegalReport::default();
    let settle = |free: &mut Free<'_>, i: usize, poses: &mut [Pose], report: &mut LegalReport| {
        let item = problem.movables[i].item;
        match free.nearest(item, poses[i], opts.grid) {
            Some(at) => {
                if at != poses[i].at {
                    let d = (f64::from(at.x) - f64::from(poses[i].at.x)).hypot(f64::from(at.y) - f64::from(poses[i].at.y));
                    report.moved.push((i, d.round() as Nm));
                    poses[i].at = at;
                }
                free.put(item, poses[i]);
            }
            None => report.unplaced.push(i),
        }
    };
    let mut wait = Vec::new();
    for &i in &order {
        if pinned(i).is_some() {
            settle(&mut free, i, poses, &mut report);
        } else if free.fits(problem.movables[i].item, poses[i]) {
            free.put(problem.movables[i].item, poses[i]);
        } else {
            wait.push(i);
        }
    }
    for i in wait {
        settle(&mut free, i, poses, &mut report);
    }
    report
}
