//! An interactive tune uses exactly the catalog planner's replaced tracks and measured result.
use super::*;
use crate::meander;
use fab_elec_circuit::Circuit;

pub(super) fn answer(
    base: &Base, world: &RBase, sample: &GestureSample, id: ProposalId, track: ItemId,
    from: usize, to: usize, circuit: &Circuit, started: Instant,
) -> RouteProposal {
    let board = &base.board;
    let track = board.locate(track).and_then(|(kind, i)| match kind {
        fab_elec_board::ItemKind::Track => board.tracks.get(i),
        _ => None,
    });
    let result = track.filter(|t| Some(t.net) == sample.gesture.net && t.layer == sample.gesture.layer)
        .ok_or_else(|| "the track you grabbed has changed".to_owned())
        .and_then(|t| meander::Tune::dragged(t, from, to, sample.gesture.cursor))
        .and_then(|tune| meander::plan(board, &base.rules, &World::new(world),
            meander::Request { tune: &tune, circuit, connectivity: &base.conn }));
    let label = track.and_then(|t| board.nets.get(t.net.ix())).map_or_else(|| "Tune net".into(), |n| format!("Tune {}", n.name));
    let verified = result.is_ok();
    let (ops, report) = match result {
        Ok(plan) => (plan.ops, Ok(plan.report)),
        Err(reason) => (Vec::new(), Err(reason)),
    };
    RouteProposal {
        id, source: ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Full },
        unit: base.unit, base_generation: board.generation, base_hash: base.hash, ops, checked: None,
        metrics: RouteMetrics { tune: Some(report), ..Default::default() }, verified, collisions: Vec::new(),
        open: Vec::new(), pair: None, head: None, solve_us: started.elapsed().as_micros().min(u32::MAX as u128) as u32,
        last: true, label,
    }
}
