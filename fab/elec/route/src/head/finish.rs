//! Finish (E6 3.1, landing 5): F while copper is drawn asks the grid finder to lay the rest of the
//! net from where the person's copper ends. The hand part (the fixed legs, their vias and the head,
//! as the sample's stretch answer laid them) goes onto a copy of the base, and the grid router
//! routes that one net there. A free end of the hand part is a route somebody started, so the
//! router continues it from its tip (`Terminal::ends`), and it keeps clear of the hand part like
//! any fixed copper. A head the walk stopped short is finished the same way: the finder is what a
//! blocked head offers.
//!
//! The answer is the sample's second tier (`Tier::Full`): the hand ops, then the router's
//! (`Origin::Solved`), so its commit is one undo step. Only the drawn net is in the router's
//! problem, so no other net's copper moves (that is shove). A net the router cannot finish keeps
//! its reason in `open`, and the answer is the hand part alone. The hand part's ids come first and
//! the router's follow from the copy's `next_item`, so the ops apply in order to the base.
//!
//! Runs on `Spawn`, never on the worker's thread. Cost: the base's job basis (built once per base,
//! by the first job that needs it), the hand part settled on a board copy (`check::settle`: the
//! apply, the refill near it, the index and connectivity updates), one world copy and its update
//! (O(n) + O(delta log n)), the copy's content hash (O(n)), and the grid route of one net.

use super::anchor_net;
use crate::auto::GridRouter;
use crate::check::{settle, Basis, State};
use crate::gesture::GestureSample;
use crate::problem::{problem, Autorouter};
use crate::progress::Progress;
use crate::proposal::{ProposalId, ProposalSource, RouteProposal, Tier};
use crate::request::{Base, Effort};
use crate::world::RBase;
use fab_elec_board::IndexChange;
use fab_elec_core::Canon;
use std::sync::Arc;
use std::time::Instant;

/// The Finish of `sample` on `base` (its routing `world` and job `basis`): `hand`, the sample's
/// stretch answer, then the grid router's route of the drawn net on the base with `hand` applied.
/// `progress` cancels the route at its next transaction boundary.
pub fn finish(
    base: &Base, world: &RBase, basis: &Basis, sample: &GestureSample, hand: &RouteProposal, id: ProposalId,
    progress: &Progress,
) -> RouteProposal {
    let started = Instant::now();
    let board = &base.board;
    let mut answer = RouteProposal {
        id, source: ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Full },
        unit: base.unit, base_generation: board.generation, base_hash: base.hash,
        ops: hand.ops.clone(), checked: None, metrics: hand.metrics.clone(), verified: hand.verified,
        collisions: hand.collisions.clone(), open: Vec::new(), head: hand.head.clone(), solve_us: 0, last: true,
        label: "Finish".into(),
    };
    let elapsed = |answer: &mut RouteProposal| answer.solve_us = started.elapsed().as_micros().min(u32::MAX as u128) as u32;
    let Some(net) = sample.gesture.net.or_else(|| anchor_net(board, sample.gesture.start)) else {
        answer.label = "Finish: the drawn copper has no net".into();
        elapsed(&mut answer);
        return answer;
    };
    let mut drawn = (**board).clone();
    let since = IndexChange::all();
    let state = State {
        rules: &base.rules, fills: &base.fills, index: &basis.index, conn: &base.conn, violations: &basis.violations,
        since: &since,
    };
    let settled = match settle(&mut drawn, &hand.ops, &state) {
        Ok(settled) => settled,
        Err(error) => {
            answer.label = format!("Finish: the drawn copper does not apply: {error}");
            elapsed(&mut answer);
            return answer;
        }
    };
    let mut routed = world.clone();
    routed.update(&drawn, &settled.applied.delta);
    let hash = drawn.content_hash();
    let drawn = Base {
        unit: base.unit, board: Arc::new(drawn), rules: base.rules.clone(), conn: Arc::new(settled.conn),
        fills: Arc::new(settled.fills), delta: None, hash, exact: base.exact,
    };
    let problem = problem(&drawn, Arc::new(routed), &[net], sample.gesture.style, Effort::default());
    let solution = GridRouter::default().route(&problem, progress);
    answer.ops.extend(solution.ops());
    let (metrics, routed) = (&mut answer.metrics, &solution.metrics);
    metrics.length = metrics.length.saturating_add(routed.length);
    metrics.vias += routed.vias;
    metrics.bends += routed.bends;
    metrics.nets_routed = routed.nets_routed;
    metrics.nets_open = routed.nets_open;
    metrics.work = routed.work.clone();
    // Finished: nothing stops the drawn copper short any more.
    if !solution.open.iter().any(|(open, _)| *open == net) {
        if let Some(head) = answer.head.as_mut() {
            head.blocked = None;
        }
    }
    answer.open = solution.open;
    elapsed(&mut answer);
    answer
}
