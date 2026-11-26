//! Adapt one canvas trail to the same spine request used by the catalog.
use super::*;
use crate::pair::{mate_anchor, plan, terminal, Endpoint, Pair, PairPlan, Request, Route};

pub(super) fn answer(
    base: &Base, world: &RBase, sample: &GestureSample, id: ProposalId,
    mate: (NetId, Anchor, P), started: Instant,
) -> RouteProposal {
    let g = &sample.gesture;
    let board = &base.board;
    let solve = || -> Result<PairPlan, String> {
        let net = g.net.ok_or("the pressed pad has no net")?;
        let pair = Pair::resolve(board, &base.rules, net)?;
        let (mate_net, mate_start, mate_at) = mate;
        if terminal(board, g.start).map(|(n, p, _)| (n, p)) != Some((net, g.start_at))
            || terminal(board, mate_start).map(|(n, p, _)| (n, p)) != Some((mate_net, mate_at))
            || mate_anchor(board, pair, g.start)? != mate_start
        {
            return Err("the pair pads you grabbed have changed".into());
        }
        // The planner supplies the centre station at either anchored end. The trail
        // includes the press and the raw cursor sample, including when it snaps to a pad.
        let waypoints = g.trail.iter().skip(1).take(g.trail.len().saturating_sub(2)).copied().collect();
        let route = Route {
            net, from: Endpoint::Anchor(g.start), to: g.target.map_or(Endpoint::Point(g.cursor), Endpoint::Anchor),
            waypoints, layer: Some(g.layer),
        };
        plan(board, &base.rules, &World::new(world), Request { route: &route, connectivity: &base.conn })
    };
    let (ops, pair, pair_refusal) = match solve() {
        Ok(plan) => (plan.ops(), Some(plan.report), None),
        Err(why) => (Vec::new(), None, Some(why)),
    };
    let name = g.net.and_then(|n| board.nets.get(n.ix())).map_or("pair", |n| n.name.as_str());
    RouteProposal {
        id, source: ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Full },
        unit: base.unit, base_generation: board.generation, base_hash: base.hash, ops, checked: None,
        metrics: RouteMetrics { pair_refusal, ..Default::default() }, verified: pair.is_some(),
        collisions: Vec::new(), open: Vec::new(), pair, head: None,
        solve_us: started.elapsed().as_micros().min(u32::MAX as u128) as u32, last: true, label: format!("Route pair {name}"),
    }
}
