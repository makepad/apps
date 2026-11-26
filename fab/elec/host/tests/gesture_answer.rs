use fab_elec_board::{BoardOp, Factories};
use fab_elec_core::{BoardId, Canon, ContentHash, ItemId, P};
use fab_elec_design::{By, DesignOp};
use fab_elec_fixtures::ledctl;
use fab_elec_host::{gesture_sample, job_sample, Cmd, Engine, Mailbox};
use fab_elec_route::{GestureId, JobId, ProposalId, ProposalSource, RouteMetrics, RouteProposal, Tier};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

fn answer(source: ProposalSource, base_hash: ContentHash, ops: Vec<BoardOp>, label: &str) -> Arc<RouteProposal> {
    Arc::new(RouteProposal {
        pair: None,
        id: ProposalId(0),
        source,
        unit: BoardId(0),
        base_generation: 0,
        base_hash,
        ops,
        checked: None,
        metrics: RouteMetrics::default(),
        verified: true,
        collisions: Vec::new(),
        open: Vec::new(),
        head: None,
        solve_us: 0,
        last: false,
        label: label.into(),
    })
}

/// Why: the board pane matches the engine's preview, commit and refusal to its gesture by tag
/// (EBOARD item 3); the tag is the gesture id, not the sample seq.
#[test]
fn a_gesture_answer_is_previewed_committed_and_refused_under_its_gesture_id() {
    use fab_elec_board::{Anchor, Item, Origin, Track};
    use fab_elec_circuit::{CheckKind, Violation};
    use fab_elec_core::{mm, LayerId, Path, Sel, Severity};
    use fab_elec_route::Reason;

    let mailbox = Mailbox::default();
    let design = ledctl();
    let unit = BoardId(0);
    let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].id;
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let board = |engine: &Engine| engine.design().unit(unit).expect("unit 0 remains").board.clone().expect("unit 0 keeps its board");
    let at = |engine: &Engine| board(engine).vias.iter().find(|v| v.id == via).expect("the via remains").at;
    let base = board(&engine).content_hash();
    let start = at(&engine);
    let gesture = |id, seq| ProposalSource::Gesture { id: GestureId(id), seq, tier: Tier::Full };
    let by = By::Ai { turn: 3, call: 4 };

    let job = answer(ProposalSource::Job { job: JobId(41), step: 1 }, base, vec![], "job");
    assert!(gesture_sample(job, by, 1).is_none(), "a job's proposal is not a gesture sample");

    let delta = P::mm(0.2, 0.0);
    let net = board(&engine).vias[0].net;
    let id = ItemId(board(&engine).next_item);
    let corner = start.add(delta).add(P::mm(1.0, 0.0));
    let track = Track {
        id, net, layer: LayerId::F_CU, w: mm(0.2), widths: None,
        path: Path::new(start.add(delta)).line(corner).line(corner.add(P::mm(-0.5, 0.5))),
        ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
    };
    let mut moved = answer(gesture(41, 5), base, vec![BoardOp::Move { item: via, by: delta }, BoardOp::Add(Item::Track(track))], "drag via");
    Arc::make_mut(&mut moved).open.push((net, Reason::SharpCorner { at: corner, layer: LayerId::F_CU, track: Sel::Item(unit, id) }));
    let has_corner = |findings: &[Violation]| findings.iter().any(|v| {
        v.kind == CheckKind::SharpCorner && v.severity == Severity::Warning && v.at == Some(corner)
            && v.unit == Some(unit) && v.items == [Sel::Item(unit, id)]
    });
    let sample = gesture_sample(moved.clone(), by, 5).expect("a gesture answer makes a sample");
    assert_eq!(GestureId(41).tag(), 41, "gesture 41: its process-wide tag is its id as u64");
    assert_eq!(sample.tag, GestureId(41).tag(), "gesture 41 answer: the sample uses the gesture's tag");
    assert_eq!((sample.tag, sample.seq, sample.by), (41, 5, by), "the sample's tag is the gesture id and its seq is the supplied host number");
    assert_eq!(sample.expect, Some((unit, base)), "the sample expects the answer's base");
    mailbox.preview(sample);
    let previewed = engine.step(mailbox.take(), &mut |_| {});
    assert!(previewed.rejected.is_empty(), "the answer on the current base is previewed");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.preview_tag, Some(41), "the preview is filed under the gesture id");
    assert!(snapshot.proposal.as_ref().is_some_and(|p| Arc::ptr_eq(p, &moved)), "the preview echoes the answer");
    assert!(has_corner(&snapshot.violations.value), "the preview locates the sharp corner on the track the drag laid");

    mailbox.push(Cmd::CommitPreview { seq: 5, tag: 41 });
    let committed = engine.step(mailbox.take(), &mut |_| {});
    assert!(committed.rejected.is_empty(), "the gesture's answer commits");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.last_commit.as_ref().expect("the commit is retained").tag, Some(41), "the commit is filed under the gesture id");
    assert_eq!(at(&engine), start.add(P::mm(0.2, 0.0)), "the commit applies the answer's ops to its unit");
    assert!(snapshot.preview.is_none() && has_corner(&snapshot.violations.value), "the committed edit keeps the corner as a session note");

    let base = board(&engine).content_hash();
    let broken = answer(gesture(42, 6), base, vec![BoardOp::Remove(ItemId(u32::MAX))], "drag missing");
    mailbox.preview(gesture_sample(broken, by, 6).expect("a gesture answer makes a sample"));
    let refused = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(refused.rejected.len(), 1, "an answer whose op fails is refused once");
    let refusal = engine.snapshot().last_refusal.clone().expect("the refusal is published");
    assert_eq!(refusal.sample, Some(42), "the refusal is filed under the gesture id");

    let stale = answer(gesture(43, 7), moved.base_hash, vec![BoardOp::Move { item: via, by: P::mm(0.1, 0.0) }], "drag stale");
    mailbox.preview(gesture_sample(stale, by, 7).expect("a gesture answer makes a sample"));
    let dropped = engine.step(mailbox.take(), &mut |_| {});
    let name = engine.design().unit(unit).expect("unit 0 remains").name.clone();
    assert_eq!(dropped.rejected, vec![("drag stale".into(), format!("{name}'s board changed after this edit was planned: nothing was applied"))],
        "an answer on a base the commit moved is refused under its tag");
    assert_eq!(engine.snapshot().last_refusal.as_ref().and_then(|r| r.sample), Some(43), "the stale answer's refusal names its gesture id");
    assert!(engine.snapshot().preview.is_none(), "a dropped answer leaves no live preview");
    mailbox.push(Cmd::Undo);
    let undone = engine.step(mailbox.take(), &mut |_| {});
    assert!(undone.rejected.is_empty() && board(&engine).locate(id).is_none(), "undo removes the track with the sharp corner");
    assert!(!has_corner(&engine.snapshot().violations.value), "undo removes the session note with its corner");
}

#[test]
fn a_job_answer_is_previewed_under_its_job_tag() {
    let mailbox = Mailbox::default();
    let design = ledctl();
    let unit = BoardId(0);
    let board = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board");
    let via = board.vias[0].id;
    let base = board.content_hash();
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let job = JobId(7);
    let by = By::Ai { turn: 3, call: 4 };
    let op = BoardOp::Move { item: via, by: P::mm(0.2, 0.0) };
    let moved = answer(ProposalSource::Job { job, step: 3 }, base, vec![op.clone()], "route job");
    let sample = job_sample(moved.clone(), by, 3).expect("a job answer makes a sample");

    assert_eq!(sample.seq, 3, "job sample: the sequence is the supplied host number");
    assert_eq!(job.tag(), (1u64 << 63) | 7, "job tag: bit 63 separates jobs from gesture tags");
    assert_eq!(sample.tag, job.tag(), "job sample: the sample uses the job's host tag");
    assert_eq!(sample.expect, Some((unit, base)), "job sample: the sample expects the answer's base");
    assert_eq!(sample.ops, vec![DesignOp::Board { unit, op }], "job sample: each board op is mapped to the answer's unit");
    assert!(sample.place.is_empty(), "job sample: a route answer adds no placement requests");
    assert_eq!((&sample.label, sample.by), (&moved.label, by), "job sample: label and author are preserved");
    assert!(sample.proposal.as_ref().is_some_and(|p| Arc::ptr_eq(p, &moved)), "job sample: the proposal keeps the same Arc");
    assert!(gesture_sample(moved.clone(), by, 3).is_none(), "job sample: a job answer is not a gesture sample");
    let gesture = answer(ProposalSource::Gesture { id: GestureId(7), seq: 3, tier: Tier::Full }, base, vec![], "gesture");
    assert!(job_sample(gesture, by, 3).is_none(), "job sample: a gesture answer is not a job sample");

    mailbox.preview(sample);
    let previewed = engine.step(mailbox.take(), &mut |_| {});
    assert!(previewed.rejected.is_empty(), "job preview: an answer on the current base is accepted");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.preview_tag, Some(job.tag()), "job preview: the engine files the preview under the job's tag");
    assert!(snapshot.proposal.as_ref().is_some_and(|p| Arc::ptr_eq(p, &moved)), "job preview: the snapshot echoes the same proposal Arc");
}
