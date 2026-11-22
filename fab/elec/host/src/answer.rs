//! A route gesture's or job's answer as the engine sample the board pane previews and commits.

use crate::inbox::Sample;
use fab_elec_design::{By, DesignOp};
use fab_elec_route::{ProposalSource, Reason, RouteProposal};
use std::sync::Arc;

/// The engine sample for a gesture `answer`; `None` for a job's proposal.
///
/// - `tag` is the gesture's id as u64. The board pane issues gesture ids and preview tags from
///   one counter, so the engine files the preview, the commit and any refusal under the gesture.
/// - `seq` is the host's number from its shared preview counter (the Driver's), so ends and
///   refusals order it with every other preview.
/// - `expect` is the answer's base, so an answer on a board that has moved since is dropped.
/// - `circuits` is empty: board ops name items by stable id, which no circuit commit renumbers.
/// - `proposal` is the answer itself; the snapshot echoes it while the sample is the live preview.
///
/// Cost: one copy of the answer's ops.
pub fn gesture_sample(answer: Arc<RouteProposal>, by: By, seq: u64) -> Option<Sample> {
    let ProposalSource::Gesture { id, .. } = answer.source else {
        return None;
    };
    let unit = answer.unit;
    let corners = answer.open.partition_point(|(_, why)| !matches!(why, Reason::SharpCorner { .. }));
    Some(Sample {
        seq,
        tag: id.tag(),
        ops: answer.ops.iter().map(|op| DesignOp::Board { unit, op: op.clone() }).collect(),
        place: vec![],
        label: answer.label.clone(),
        by,
        circuits: Vec::new(),
        expect: Some((unit, answer.base_hash)),
        notes: answer.open[corners..].to_vec(),
        proposal: Some(answer),
    })
}

/// The engine sample for a job `answer`; `None` for a gesture's proposal.
/// A job's newer proposal is complete against the same base and replaces the live preview.
///
/// - `tag` is the job's host tag, separate from gesture tags.
/// - `seq` is the host's number from its shared preview counter (the Driver's), so ends and
///   refusals order it with every other preview.
/// - `expect` is the answer's base, so an answer on a board that has moved since is dropped.
/// - `circuits` is empty: board ops name items by stable id, which no circuit commit renumbers.
/// - `proposal` is the answer itself; the snapshot echoes it while the sample is the live preview.
///
/// Cost: one copy of the answer's ops.
pub fn job_sample(answer: Arc<RouteProposal>, by: By, seq: u64) -> Option<Sample> {
    let ProposalSource::Job { job, .. } = answer.source else {
        return None;
    };
    let unit = answer.unit;
    Some(Sample {
        seq,
        tag: job.tag(),
        ops: answer.ops.iter().map(|op| DesignOp::Board { unit, op: op.clone() }).collect(),
        place: vec![],
        label: answer.label.clone(),
        by,
        circuits: Vec::new(),
        expect: Some((unit, answer.base_hash)),
        notes: vec![],
        proposal: Some(answer),
    })
}
