//! Interactive heads against concrete pads of the controller fixture.

use fab_elec_board::drc::DRC_EPSILON;
use fab_elec_board::{connect, required_to, resolve_rules, Anchor, BoardOp, ConnItem, Copper, Item, Obstacle, Origin, Placed, Via, ViaKind};
use fab_elec_core::{mm, Canon, ItemId, LayerId, Nm, OnTrack, Path, Seg, Sel, Shape, P};
use fab_elec_route::embed::side;
use fab_elec_route::head::{answer, finish, shove, shove_step, Answer, GestureState};
use fab_elec_route::{
    new_pieces, verify, Base, BlockCause, Blocked, Gesture, GestureId, GestureKind, GestureSample,
    HeadAnswer, Leg, Mode, Progress, ProposalId, ProposalSource, RBase, RouteProposal, SampleCause, Tier, WidthRule, World, Xform,
};
use fab_elec_board::zone::fill_all;
use fab_elec_board::{drc, Board, IndexChange, PieceIndex};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_lang::{evaluate, std_libs};
use fab_elec_route::check::{check, Basis, Outcome, State};
use std::collections::BTreeSet;
use std::sync::Arc;

fn fixture() -> (Base, RBase) {
    let design = fab_elec_fixtures::ledctl();
    let board = design.units[0].board.clone().expect("the controller fixture has a board");
    let rules = Arc::new(resolve_rules(&board, None));
    // Heads ignore zone fills; empty fills avoid doing an unrelated full fill in each test.
    let fills = Arc::new(Vec::new());
    let conn = Arc::new(connect(&board, &fills));
    let world = RBase::build(&board);
    (Base::new(board, rules, conn, fills), world)
}

fn pad(base: &Base, name: &str, number: &str) -> (Anchor, P, Copper) {
    let placed = base.board.placed.iter().find(|p| p.path == name).expect("the named fixture part exists");
    let pad = placed.footprint.pad(number).expect("the named fixture pad exists");
    let anchor = Anchor::Pad { placed: placed.id, pad };
    let at = base.board.anchor_pos(anchor).expect("the fixture pad has a position");
    let copper = base.board.all_copper().into_iter().find(|c| {
        c.item == ConnItem::Pad { placed: placed.id, pad } && c.layer == LayerId::F_CU
    }).expect("the chosen fixture pad has front copper");
    (anchor, at, copper)
}

fn sample(base: &Base, anchor: Anchor, at: P) -> GestureSample {
    GestureSample {
        id: GestureId(7), seq: 1,
        gesture: Gesture::new(GestureKind::Route, base.unit, base.board.generation, anchor, at, LayerId::F_CU),
    }
}

fn solve(base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState) -> RouteProposal {
    answer(base, world, sample, state, ProposalId(99)).proposal()
        .expect("a well-formed current route sample is answered")
}

fn line(points: &[P]) -> Path {
    Path { start: points[0], segs: points.iter().skip(1).copied().map(Seg::Line).collect() }
}

fn points(path: &Path) -> Vec<P> {
    std::iter::once(path.start).chain(path.segs.iter().map(Seg::end)).collect()
}

/// Whether `new` keeps `base`'s end `e`: the same, or an end on another track anchored again at
/// the same point (or freed there) because its host changed.
fn kept_end(new: &fab_elec_board::Track, base: &fab_elec_board::Track, e: usize) -> bool {
    let at = |path: &Path| if e == 0 { path.start } else { path.end() };
    new.ends[e] == base.ends[e] || matches!(base.ends[e], Anchor::Track { .. })
        && matches!(new.ends[e], Anchor::Track { .. } | Anchor::Free) && at(&new.path) == at(&base.path)
}

fn added_track(proposal: &RouteProposal, index: usize) -> &fab_elec_board::Track {
    let BoardOp::Add(Item::Track(track)) = &proposal.ops[index] else { panic!("op {index} must add a track") };
    track
}

fn added_via(proposal: &RouteProposal, index: usize) -> &Via {
    let BoardOp::Add(Item::Via(via)) = &proposal.ops[index] else { panic!("op {index} must add a via") };
    via
}

#[test]
fn target_snap_and_ordered_ops() {
    let (base, world) = fixture();
    // Pads 1 of the two outer resistors share the power rail and open space to their right.
    let (start, s, _) = pad(&base, "lamps[7].r", "1");
    let (target, e, _) = pad(&base, "lamps[6].r", "1");
    let (foreign, _, _) = pad(&base, "lamps[6].r", "2");
    let a = s.add(P::mm(0.5, 0.0));
    let b = s.add(P::mm(1.0, 0.0));
    let c = b.add(P::mm(0.0, -1.0));
    let mut sample = sample(&base, start, s);
    sample.gesture.legs = vec![
        Leg { path: line(&[s, a]), layer: LayerId::F_CU, w: mm(0.5), via_to: None },
        Leg { path: line(&[a, b]), layer: LayerId::F_CU, w: mm(0.5), via_to: Some(LayerId::B_CU) },
        Leg { path: line(&[b, c]), layer: LayerId::B_CU, w: mm(0.5), via_to: Some(LayerId::F_CU) },
    ];
    sample.gesture.target = Some(target);
    sample.gesture.cursor = e.add(P::mm(-1.0, -1.0));
    sample.gesture.trail = vec![c, c.add(P::mm(-1.0, -0.2)), sample.gesture.cursor];
    let mut state = GestureState::default();
    let proposal = solve(&base, &world, &sample, &mut state);
    assert!(proposal.verified, "snapped route must pass pair checks: {:?}", proposal.collisions);
    assert_eq!(proposal.base_hash, base.hash, "gesture proposals must carry the committed content hash");
    assert!(proposal.checked.is_none(), "gesture verification must leave full preview checks to the host");
    assert_eq!(proposal.source, ProposalSource::Gesture { id: sample.id, seq: 1, tier: Tier::Stretch }, "the answer must name its sample and tier");
    assert!(proposal.last, "a stretch answer is the sample's last while no second tier exists");
    let head = proposal.head.as_ref().expect("gesture proposals contain a head answer");
    assert_eq!((head.path.end(), head.end, head.blocked), (e, target, None), "a valid same-net target must override the cursor");
    // Each via comes before the leg that ends on it, so every anchor names an earlier op.
    assert_eq!(proposal.ops.len(), 6, "three legs, two vias and the head must be emitted");
    let id = |offset| ItemId(base.board.next_item + offset);
    let first = added_track(&proposal, 0);
    let second = added_track(&proposal, 2);
    let third = added_track(&proposal, 4);
    let last = added_track(&proposal, 5);
    assert_eq!([first.id, added_via(&proposal, 1).id, second.id, added_via(&proposal, 3).id, third.id, last.id], [id(0), id(1), id(2), id(3), id(4), id(5)], "ids must follow op order from next_item");
    assert_eq!(first.ends, [start, Anchor::Free], "the first leg must attach to the pad and name no later item");
    assert_eq!(second.ends, [Anchor::Track { id: id(0), at: OnTrack { seg: 0, frac: OnTrack::ONE } }, Anchor::Via(id(1))], "plain leg joints and the first via must be anchored");
    assert_eq!(third.ends, [Anchor::Via(id(1)), Anchor::Via(id(3))], "the back leg must join both vias");
    assert_eq!(last.ends, [Anchor::Via(id(3)), target], "the head must attach to the last via and target");
    let mut applied = (*base.board).clone();
    applied.apply_all(&proposal.ops).expect("the board applier must take the answer's ops as they are");
    assert!(proposal.ops.iter().all(|op| match op {
        BoardOp::Add(Item::Track(t)) => t.origin == Origin::Authored && !t.locked && t.widths.is_none() && t.w == mm(0.5),
        BoardOp::Add(Item::Via(v)) => v.origin == Origin::Authored && !v.locked,
        _ => false,
    }), "gesture copper must be authored, unlocked and uniformly sized");
    assert_eq!(proposal.metrics.vias, 2, "metrics must count both vias");
    assert_eq!(proposal.metrics.length, [first, second, third, last].iter().map(|t| t.path.length().round() as i64).sum::<i64>(), "metrics must include the clicked legs and head");

    sample.seq += 1;
    sample.gesture.target = Some(foreign);
    let proposal = solve(&base, &world, &sample, &mut state);
    let head = proposal.head.as_ref().expect("the rejected snap still produces a head");
    assert!(proposal.verified, "rejecting a foreign snap must retain a legal route: {:?}", proposal.collisions);
    assert_eq!((head.path.end(), head.end), (sample.gesture.cursor, Anchor::Free), "a foreign-net target must fall back to the cursor");
}

fn around_pad(base: &Base) -> (GestureSample, P, Vec<P>, Vec<P>) {
    // The outer resistor's pad 2 is a signal; pad 1 is foreign power copper with no outgoing track.
    // The open right-hand area permits paths on both sides without crossing the fixture's tracks.
    let (start, s, _) = pad(base, "lamps[7].r", "2");
    let (_, centre, _) = pad(base, "lamps[7].r", "1");
    let e = centre.add(P::mm(2.0, 0.0));
    let above = vec![s, centre.add(P::mm(1.0, 1.0)), e];
    let below = vec![s, centre.add(P::mm(-1.0, 1.0)), centre.add(P::mm(-1.0, -1.0)), centre.add(P::mm(1.0, -1.0)), e];
    let mut sample = sample(base, start, s);
    sample.gesture.cursor = e;
    sample.gesture.width = WidthRule::Fixed(mm(0.25));
    (sample, centre, above, below)
}

#[test]
fn walk_keeps_the_trails_side_of_a_foreign_pad() {
    let (base, world) = fixture();
    let (mut sample, centre, above, below) = around_pad(&base);
    assert_ne!(side::keep(&above, centre), side::keep(&below, centre), "the fixture trails must choose opposite sides of the foreign pad");
    let mut state = GestureState::default();
    for trail in [above, below] {
        sample.gesture.trail = trail.clone();
        let proposal = solve(&base, &world, &sample, &mut state);
        let head = proposal.head.as_ref().expect("a walk proposal has a head");
        assert!(proposal.verified, "both side passes must be legal: {:?}", proposal.collisions);
        assert!(head.blocked.is_none(), "the open side pass must reach the cursor: {:?}", head.blocked);
        assert_eq!(side::keep(&points(&head.path), centre), side::keep(&trail, centre), "tightening must preserve the chosen side");
        assert_eq!(head.path.end(), sample.gesture.cursor, "a complete side pass must reach the cursor");
        sample.seq += 1;
    }
}

#[test]
fn a_narrow_pad_row_returns_a_verified_prefix() {
    let (base, world) = fixture();
    // The controller's pads 1 and 2 have 0.65 mm pitch and 0.3 mm leads: their 0.35 mm
    // gap is narrower than a 0.25 mm head plus two clearances. Neither is the lamp's signal.
    let (start, s, _) = pad(&base, "lamps[7].r", "2");
    let (_, a, _) = pad(&base, "mcu.u", "1");
    let (_, b, _) = pad(&base, "mcu.u", "2");
    let e = P::new((a.x + b.x) / 2, (a.y + b.y) / 2);
    let mut sample = sample(&base, start, s);
    sample.gesture.cursor = e;
    sample.gesture.width = WidthRule::Fixed(mm(0.25));
    sample.gesture.trail = vec![s, e];
    let proposal = solve(&base, &world, &sample, &mut GestureState::default());
    let head = proposal.head.as_ref().expect("a blocked route still has a head answer");
    let blocked = head.blocked.expect("the narrow pad row must block the head");
    assert!(matches!(blocked.cause, BlockCause::Gap | BlockCause::End), "a real clearance deficit must explain the block: {blocked:?}");
    assert!(blocked.have < blocked.need, "the blocked chip must report a real deficit");
    assert!(proposal.verified, "the retained prefix must remain legal: {:?}", proposal.collisions);
    assert_eq!((head.path.start, head.end), (s, Anchor::Free), "a blocked prefix starts at the pad and has a free end");
    assert_ne!(head.path.end(), e, "the blocked prefix must stop before the forbidden endpoint");
}

#[test]
fn via_slides_to_the_nearest_grid_point_and_trims_the_leg() {
    let (base, world) = fixture();
    // The outer resistor's signal pad has open space on its right; approach its foreign
    // power pad horizontally so the hull boundary is also the exact via clearance boundary.
    let (start, s, signal) = pad(&base, "lamps[7].r", "2");
    let (_, centre, foreign) = pad(&base, "lamps[7].r", "1");
    let net = signal.net.expect("the chosen signal pad is connected");
    let class = signal.class;
    let size = base.rules.class(class).via.unwrap_or_else(|| base.rules.via(class));
    let probe = Copper { item: ConnItem::Via(ItemId(base.board.next_item + 1)), net: Some(net), class, layer: LayerId::F_CU, shape: Shape::Circle { c: centre, r: size.pad / 2 } };
    let need = required_to(&base.rules, &probe, &Obstacle::Copper(&foreign), LayerId::F_CU).gap;
    let expected = P::new(foreign.shape.bbox().max.x + need + size.pad / 2, centre.y);
    let end = expected.add(P::new(-50_000, 0));
    let corner = centre.add(P::mm(2.0, 0.0));
    let mut sample = sample(&base, start, s);
    sample.gesture.width = WidthRule::Fixed(mm(0.25));
    sample.gesture.legs = vec![Leg {
        path: line(&[s, P::new(corner.x, s.y), corner, end]), layer: LayerId::F_CU,
        w: mm(0.25), via_to: Some(LayerId::B_CU),
    }];
    sample.gesture.layer = LayerId::B_CU;
    sample.gesture.cursor = expected.add(P::mm(1.0, -1.0));
    sample.gesture.trail = vec![end, sample.gesture.cursor];
    let mut state = GestureState::default();
    let proposal = solve(&base, &world, &sample, &mut state);
    assert!(proposal.verified, "the slid via and both tracks must verify: {:?}", proposal.collisions);
    assert_eq!(proposal.ops.len(), 3, "a via leg must emit its via, its track, then the head");
    let via = added_via(&proposal, 0);
    assert_eq!(via.at, expected, "the via must stop at the nearest exact clearance boundary");
    // DRC lets a gap fall short by its rounding allowance; one step past it must fail.
    let mut farther = via.clone();
    farther.at.x -= DRC_EPSILON + 1;
    let faults = verify(&World::new(&world), &base.rules, &base.board, &new_pieces(&base.board, &Item::Via(farther)));
    assert!(!faults.is_empty(), "one step past the allowance toward the clicked end must fail exact via verification");
    assert_eq!(added_track(&proposal, 1).path.end(), via.at, "the leg must be trimmed to the via centre");
    assert_eq!(added_track(&proposal, 1).ends[1], Anchor::Via(via.id), "the trimmed leg must be anchored to its via");
    assert_eq!(added_track(&proposal, 2).path.start, via.at, "the head must start at the slid via");
    assert_eq!(added_track(&proposal, 2).ends[0], Anchor::Via(via.id), "the head must attach to the slid via");
    sample.seq += 1;
    let repeated = solve(&base, &world, &sample, &mut state);
    assert_eq!(repeated.ops, proposal.ops, "the same clicked leg must slide identically on the next sample");
}

#[test]
fn changing_width_rebuilds_the_clearance_hulls() {
    let (base, world) = fixture();
    let (mut sample, _, _, below) = around_pad(&base);
    sample.gesture.trail = below;
    let mut state = GestureState::default();
    sample.gesture.width = WidthRule::Fixed(mm(0.2));
    let narrow = solve(&base, &world, &sample, &mut state);
    sample.seq += 1;
    sample.gesture.width = WidthRule::Fixed(mm(0.5));
    let wide = solve(&base, &world, &sample, &mut state);
    assert!(narrow.verified && wide.verified, "both widths must pass exact checks: narrow={:?}, wide={:?}", narrow.collisions, wide.collisions);
    let narrow = narrow.head.expect("the narrow route has a head");
    let wide = wide.head.expect("the wide route has a head");
    assert!(narrow.blocked.is_none() && wide.blocked.is_none(), "both widths must reach the open cursor");
    assert_eq!((narrow.w, wide.w), (mm(0.2), mm(0.5)), "each head must use its sample's width");
    assert_ne!(narrow.path, wide.path, "the wider head must move outward where the narrow head hugged the pad");
}

#[test]
fn an_unplaceable_via_stops_the_answer_at_its_leg() {
    let (base, world) = fixture();
    // The controller's power pad 4 lies between foreign pads at 0.65 mm pitch. Its class's
    // 0.8 mm via pad cannot fit there, although a narrow track can leave along the pad.
    let (start, s, _) = pad(&base, "mcu.u", "4");
    let end = s.add(P::new(10_000, 0));
    let after = end.add(P::mm(1.0, 0.0));
    let mut sample = sample(&base, start, s);
    sample.gesture.width = WidthRule::Fixed(mm(0.15));
    sample.gesture.layer = LayerId::B_CU;
    sample.gesture.legs = vec![
        Leg { path: line(&[s, end]), layer: LayerId::F_CU, w: mm(0.15), via_to: Some(LayerId::B_CU) },
        Leg { path: line(&[end, after]), layer: LayerId::B_CU, w: mm(0.15), via_to: None },
    ];
    sample.gesture.cursor = after.add(P::mm(1.0, 0.0));
    let proposal = solve(&base, &world, &sample, &mut GestureState::default());
    let head = proposal.head.as_ref().expect("a failed via still produces head metadata");
    let blocked = head.blocked.expect("the power via must not fit on the short leg");
    assert_eq!((blocked.cause, blocked.at), (BlockCause::Via, end), "the via failure must name its clicked leg end");
    assert_eq!(proposal.ops.len(), 1, "the leg up to the failed via remains; later legs and the head would hang off nothing");
    assert_eq!(added_track(&proposal, 0).ends[1], Anchor::Free, "the missing via must not leave a dangling item reference");
    assert_eq!((head.path.start, head.path.end(), head.length), (end, end, 0), "the head must stay suppressed after any via failure");
    assert!(proposal.verified, "the retained narrow legs must still pass pair checks: {:?}", proposal.collisions);
}

#[test]
fn zero_length_head_adds_no_track() {
    let (base, world) = fixture();
    // The outer resistor's signal pad has legal clearance from its companion at the start.
    let (start, at, _) = pad(&base, "lamps[7].r", "2");
    let mut sample = sample(&base, start, at);
    sample.gesture.trail = vec![at, at.add(P::mm(1.0, 0.0)), at];
    let proposal = solve(&base, &world, &sample, &mut GestureState::default());
    assert!(proposal.ops.is_empty(), "a zero-length head must not add a track");
    assert!(proposal.verified, "a zero-length route must verify without copper to test");
    let head = proposal.head.expect("a zero-length route still has a head answer");
    assert_eq!((head.path.start, head.path.end(), head.length, head.blocked), (at, at, 0, None), "zero-length head metadata must describe the start without a block");
}

#[test]
fn refusals_and_the_generation_and_sequence_order() {
    let (base, world) = fixture();
    let (mut sample, _, _, below) = around_pad(&base);
    sample.gesture.trail = below;
    let mut state = GestureState::default();
    let first = solve(&base, &world, &sample, &mut state);
    assert_eq!(first.base_generation, base.board.generation, "the answer names the base it was made on");
    assert!(matches!(answer(&base, &world, &sample, &mut state, ProposalId(100)), Answer::Covered),
        "a repeated sequence on the same base is covered by its answer");

    // A base that changes under the gesture: the same sample is answered again on the new base,
    // whatever generation the pane drew it on.
    let mut moved = (*base.board).clone();
    let far = moved.placed.iter().find(|p| p.path == "lamps[0].r").expect("a far part exists").id;
    moved.apply_all(&[BoardOp::Move { item: far, by: P::mm(0.5, 0.0) }]).expect("the far part moves");
    let moved = Arc::new(moved);
    let newer = Base::new(moved.clone(), base.rules.clone(), Arc::new(connect(&moved, &[])), Arc::new(Vec::new()));
    let newer_world = RBase::build(&newer.board);
    let again = answer(&newer, &newer_world, &sample, &mut state, ProposalId(101)).proposal()
        .expect("a sample answered on an older base is answered again on a newer one, not covered");
    assert_eq!((again.base_generation, again.base_hash), (newer.board.generation, newer.hash),
        "the re-answer carries the newer base's generation and hash");
    assert!(again.verified, "the re-answer is verified on the newer base: {:?}", again.collisions);
    assert!(matches!(answer(&newer, &newer_world, &sample, &mut state, ProposalId(102)), Answer::Covered),
        "the re-answer covers the same sample on the same base");

    // The worker holds only its newest base: the rest is answered there.
    sample.seq += 1;
    sample.gesture.mode = Mode::Mark;
    assert!(matches!(answer(&newer, &newer_world, &sample, &mut state, ProposalId(103)), Answer::Refused(SampleCause::Unsupported, _)),
        "mark mode is refused as unsupported, not walked");
    sample.gesture.mode = Mode::Walk;
    sample.gesture.kind = GestureKind::Fanout(ItemId(0));
    assert!(matches!(answer(&newer, &newer_world, &sample, &mut state, ProposalId(105)), Answer::Refused(SampleCause::Unsupported, _)),
        "gestures of later tiers (a fanout) are refused as unsupported, not answered as something else");
    sample.gesture.kind = GestureKind::Route;
    sample.gesture.width = WidthRule::Fixed(-1);
    assert!(matches!(answer(&newer, &newer_world, &sample, &mut state, ProposalId(106)), Answer::Refused(SampleCause::Invalid, _)),
        "invalid widths must be rejected before hull construction: expected an Invalid refusal");
    sample.gesture.width = WidthRule::Fixed(mm(0.25));
    sample.gesture.start_at = sample.gesture.start_at.add(P::new(1, 0));
    assert!(matches!(answer(&newer, &newer_world, &sample, &mut state, ProposalId(107)),
        Answer::Refused(SampleCause::Invalid, "what you grabbed has moved")),
        "a start anchor that is no longer where the gesture grabbed it is refused");
}

#[test]
fn a_fixed_leg_keeps_its_width_when_the_head_is_widened() {
    let (base, world) = fixture();
    let (start, s, _) = pad(&base, "lamps[7].r", "2");
    let a = s.add(P::mm(2.0, 0.0));
    let mut sample = sample(&base, start, s);
    sample.gesture.legs = vec![Leg { path: line(&[s, a]), layer: LayerId::F_CU, w: mm(0.25), via_to: None }];
    sample.gesture.width = WidthRule::Fixed(mm(0.4));
    sample.gesture.cursor = a.add(P::mm(0.0, 1.5));
    sample.gesture.trail = vec![a, sample.gesture.cursor];
    let proposal = solve(&base, &world, &sample, &mut GestureState::default());
    assert!(proposal.verified, "the widened head and the narrow leg verify: {:?}", proposal.collisions);
    assert_eq!(added_track(&proposal, 0).w, mm(0.25), "a W press after a click leaves the fixed leg at the width it was fixed with");
    assert_eq!(added_track(&proposal, 1).w, mm(0.4), "the head takes the sample's current width");
}

#[test]
fn a_second_via_keeps_hole_to_hole_from_the_first() {
    let (base, world) = fixture();
    let (start, s, signal) = pad(&base, "lamps[7].r", "2");
    let class = signal.class;
    let size = base.rules.class(class).via.unwrap_or_else(|| base.rules.via(class));
    // The first leg ends 3 mm right of the pad with a via; the back leg runs on, turns and comes
    // back to end a drill from that via, where the second via cannot sit.
    let a = s.add(P::mm(3.0, 0.0));
    let turn = a.add(P::mm(3.0, 0.0));
    let down = turn.add(P::mm(0.0, 1.0));
    let close = P::new(a.x + size.drill, down.y);
    let mut sample = sample(&base, start, s);
    sample.gesture.width = WidthRule::Fixed(mm(0.25));
    sample.gesture.legs = vec![
        Leg { path: line(&[s, a]), layer: LayerId::F_CU, w: mm(0.25), via_to: Some(LayerId::B_CU) },
        Leg { path: line(&[a, turn, down, close]), layer: LayerId::B_CU, w: mm(0.25), via_to: Some(LayerId::F_CU) },
    ];
    sample.gesture.cursor = close.add(P::mm(0.0, 1.0));
    sample.gesture.trail = vec![close, sample.gesture.cursor];
    let proposal = solve(&base, &world, &sample, &mut GestureState::default());
    let first = added_via(&proposal, 0);
    let second = added_via(&proposal, 2);
    let apart = (first.at.dist2(second.at) as f64).sqrt() - f64::from(first.drill + second.drill) / 2.0;
    assert!(apart >= f64::from(base.rules.hole_to_hole()), "the second via slides back until its drill keeps hole to hole from the first: {apart}");
    assert!(proposal.verified, "both vias and every leg verify: {:?}", proposal.collisions);
    assert_eq!((first.kind, first.span), (ViaKind::Through, (LayerId::F_CU, LayerId::B_CU)), "a via spans the stack until the stackup declares other spans");
}

#[test]
fn blocked_summaries_round_deficits_outward() {
    let blocked = |cause, have: Nm, need: Nm| Blocked { at: P::ZERO, cause, have, need };
    assert_eq!(blocked(BlockCause::Gap, 359_999, 360_001).summary(), "gap 0.35 mm, needs 0.37 mm", "gap text must not round available room upward or need downward");
    assert_eq!(blocked(BlockCause::End, 349_999, 350_000).summary(), "end in clearance: 0.34 of 0.35 mm", "endpoint text must retain the real deficit");
    assert_eq!(blocked(BlockCause::Via, 0, 0).summary(), "no room for the via on this leg", "via text must name the leg failure");
    assert_eq!(blocked(BlockCause::Thread, 400_000, 350_000).summary(), "no 45-degree pass through this gap", "thread text must not claim a clearance deficit");
    assert_eq!(blocked(BlockCause::Unsettled, 0, 0).summary(), "the router could not settle this path", "unsettled text must describe the solver limit");
}

/// drum16's board, with the board's fills, connectivity and findings: what `check` measures a
/// gesture answer against.
struct Drum {
    base: Base,
    world: RBase,
    index: PieceIndex,
    violations: Vec<Violation>,
}

impl Drum {
    /// drum16's board as its project builds it.
    fn board() -> Board {
        let sources = fab_elec_fixtures::docs("drum16").expect("fixture setup: drum16 is a stock project");
        let evaluated = evaluate(&sources, &std_libs()).expect("fixture setup: drum16 evaluates");
        let unit = evaluated.design.units.iter().find(|u| u.name == "drum16").expect("fixture setup: the drum16 unit");
        (**unit.board.as_ref().expect("fixture setup: drum16 has a board")).clone()
    }

    /// `board` with `net`'s track from `from` removed, so a person draws it again; and that track.
    fn without(board: &Board, net: &str, from: (f64, f64)) -> (Drum, fab_elec_board::Track) {
        let (drum, mut tracks) = Drum::without_all(board, &[(net, from)]);
        (drum, tracks.remove(0))
    }

    /// `board` with each (net, start) track removed; and those tracks, in order.
    fn without_all(board: &Board, which: &[(&str, (f64, f64))]) -> (Drum, Vec<fab_elec_board::Track>) {
        let mut board = board.clone();
        let tracks: Vec<fab_elec_board::Track> = which.iter().map(|&(net, from)| {
            let id = board.net(net).expect("fixture setup: the net exists");
            board.tracks.iter().find(|t| t.net == id && t.path.start == P::mm(from.0, from.1))
                .expect("fixture setup: the net's track from that pad").clone()
        }).collect();
        let ops: Vec<BoardOp> = tracks.iter().map(|t| BoardOp::Remove(t.id)).collect();
        board.apply_all(&ops).expect("fixture setup: the tracks come off");
        (Drum::of(board), tracks)
    }

    /// `board` with its fills, connectivity, findings and routing world.
    fn of(board: Board) -> Drum {
        let board = Arc::new(board);
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        let violations = drc(&board, &conn, &rules, &fills, None).violations;
        let index = PieceIndex::build(&board, &fills);
        let world = RBase::build(&board);
        Drum { base: Base::new(board, rules, conn, fills), world, index, violations }
    }

    fn pad(&self, part: &str, number: &str) -> (Anchor, P) {
        let placed = self.base.board.placed.iter().find(|p| p.path == part).expect("fixture setup: the part exists");
        let pad = placed.footprint.pad(number).expect("fixture setup: the pad exists");
        (Anchor::Pad { placed: placed.id, pad }, placed.pad_at(pad).expect("fixture setup: the pad has a position"))
    }

    /// The findings `ops` add to the board, by kind.
    fn new_findings(&self, ops: &[BoardOp]) -> Vec<CheckKind> {
        self.new_violations(ops).into_iter().map(|v| v.kind).collect()
    }

    /// The findings `ops` add to the board or make worse.
    fn new_violations(&self, ops: &[BoardOp]) -> Vec<Violation> {
        let out = self.checked(ops);
        out.new.iter().map(|&i| out.violations[i].clone()).collect()
    }

    /// `ops` applied to the board and checked.
    fn checked(&self, ops: &[BoardOp]) -> Outcome {
        let since = IndexChange::all();
        let state = State {
            rules: &self.base.rules, fills: &self.base.fills, index: &self.index, conn: &self.base.conn,
            violations: &self.violations, since: &since,
        };
        let mut board = (*self.base.board).clone();
        check(&mut board, ops, &state).expect("the answer's ops apply to its base")
    }

    /// A drag along `path`: one sample per point after the first, the last snapped to `target`.
    /// Every answer is verified, and DRC agrees: a verified answer adds no copper finding.
    fn drag(&self, mut sample: GestureSample, path: &[P], target: Anchor, state: &mut GestureState) -> RouteProposal {
        let mut last = None;
        for (i, &p) in path.iter().enumerate().skip(1) {
            sample.seq += 1;
            sample.gesture.trail.push(p);
            sample.gesture.cursor = p;
            sample.gesture.target = (i + 1 == path.len()).then_some(target);
            let proposal = match answer(&self.base, &self.world, &sample, state, ProposalId(sample.seq as u32)) {
                Answer::Proposal(proposal) => proposal,
                Answer::Refused(cause, text) => panic!("sample {}: refused ({cause:?}): {text}", sample.seq),
                Answer::Covered => panic!("sample {}: covered", sample.seq),
            };
            assert!(proposal.verified, "sample {}: the answer verifies: {:?}", sample.seq, proposal.collisions);
            let copper: Vec<_> = self.new_findings(&proposal.ops).into_iter()
                .filter(|kind| !matches!(kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected))
                .collect();
            assert!(copper.is_empty(), "sample {}: one definition of legal: DRC finds nothing new in a verified answer: {copper:?}", sample.seq);
            last = Some(proposal);
        }
        last.expect("the drag sends samples")
    }
}

/// The vias `ops` push (a `Move` of a via), with how far. Each track on one is laid after the
/// move, its end on the via where the via goes, its other end and its anchors kept.
fn pushed_vias(board: &Board, ops: &[BoardOp]) -> Vec<(ItemId, P)> {
    let mut vias = Vec::new();
    for (k, op) in ops.iter().enumerate() {
        let BoardOp::Move { item, by } = *op else { continue };
        let Some(via) = board.vias.iter().find(|v| v.id == item) else { continue };
        for track in board.tracks.iter().filter(|t| t.ends.contains(&Anchor::Via(item))) {
            let laid = ops[k + 1..].iter().find_map(|op| match op {
                BoardOp::Replace(Item::Track(t)) if t.id == track.id => Some(t),
                _ => None,
            }).expect("a pushed via's tracks are laid after its move");
            for (e, old, new) in [(0, track.path.start, laid.path.start), (1, track.path.end(), laid.path.end())] {
                let want = if track.ends[e] == Anchor::Via(item) { via.at.add(by) } else { old };
                assert_eq!(new, want, "track {:?} on pushed via {item:?}: its end on the via goes with it, its other end stays", track.id);
            }
            assert_eq!(laid.ends, track.ends, "track {:?} on pushed via {item:?} keeps its anchors", track.id);
        }
        vias.push((item, by));
    }
    vias
}

/// The head's y where it crosses x (its first segment spanning x).
fn y_at(path: &Path, x: Nm) -> Option<Nm> {
    points(path).windows(2).find_map(|w| {
        let (a, b) = (w[0], w[1]);
        let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
        (lo <= x && x <= hi && a.x != b.x).then(|| (a.y as i64 + (b.y as i64 - a.y as i64) * (x as i64 - a.x as i64) / (b.x as i64 - a.x as i64)) as Nm)
    })
}

/// The guard: on drum16, copper drawn by hand from a pad round an obstacle to a same-net pad
/// lands with no new finding, ends on the target's anchor and leaves no dangling end, on one
/// layer and with a via on the way.
#[test]
fn drawn_copper_on_drum16_lands_legal_on_its_target() {
    // A voice's output drawn round a transistor whose pads lie between the trail and the
    // straight line to the target: the head keeps them on the trail's side, wrapping them,
    // never on the straight line's shorter side.
    let drum16 = Drum::board();
    let (drum, removed) = Drum::without(&drum16, "hats.yin", (162.2, 98.59));
    let [start, target] = removed.ends;
    let path = points(&removed.path);
    let t = *path.last().expect("a track has an end");
    let (_, q2) = drum.pad("hats.cymg.q2", "2");
    let mut state = GestureState::default();
    let mut drawn = sample(&drum.base, start, path[0]);
    drawn.gesture.trail = vec![path[0]];
    let last = drum.drag(drawn, &path, target, &mut state);
    let head = last.head.as_ref().expect("a route answer has a head");
    assert_eq!((head.end, head.path.end(), head.blocked), (target, t, None), "the head ends on the target's anchor");
    assert!(y_at(&head.path, q2.x).is_some_and(|y| y < q2.y), "the head passes the transistor's pads on the side the trail did: {:?}", head.path);
    assert_eq!(drum.new_findings(&last.ops), Vec::<CheckKind>::new(), "the committed track adds no finding: no dangling end, no open line");

    // The programming header's pin from its row resistor, through a via to the back.
    let (drum, _) = Drum::without(&drum16, "mcu.miso", (22.5, 86.52));
    let (start, s) = drum.pad("mcu.r_row4", "1");
    let (target, t) = drum.pad("mcu.isp_j", "1");
    let (_, power) = drum.pad("mcu.isp_j", "2");
    // Legs are octilinear: a committed head is.
    let leg = [s, P::mm(22.08, 86.1), P::mm(18.5, 86.1)];
    let mut state = GestureState::default();
    let mut drawn = sample(&drum.base, start, s);
    drawn.gesture.width = WidthRule::Fixed(mm(0.15));
    drawn.gesture.legs = vec![Leg { path: line(&leg), layer: LayerId::F_CU, w: mm(0.15), via_to: Some(LayerId::B_CU) }];
    drawn.gesture.layer = LayerId::B_CU;
    drawn.gesture.trail = vec![leg[2]];
    let path = [leg[2], P::mm(16.0, 84.0), P::mm(14.0, 82.4), t];
    let last = drum.drag(drawn, &path, target, &mut state);
    let head = last.head.as_ref().expect("a route answer has a head");
    assert_eq!((head.end, head.path.end(), head.blocked, head.layer), (target, t, None, LayerId::B_CU), "the back head ends on the through pad's anchor");
    let via = added_via(&last, 0);
    assert_eq!((via.kind, via.span), (ViaKind::Through, (LayerId::F_CU, LayerId::B_CU)), "the via spans both layers");
    assert!(y_at(&head.path, power.x).is_some_and(|y| y > power.y), "the head passes the header's power pin on the side the trail did: {:?}", head.path);
    assert_eq!(drum.new_findings(&last.ops), Vec::<CheckKind>::new(), "the via and both tracks add no finding: legal on both layers, nothing dangling");
}

/// Landing 3's guard: a dragged segment of a routed drum16 track walks round the pads it is
/// dragged onto, ends legal, keeps its id and both ends, and springs back to its base path when
/// the cursor returns to the press point.
#[test]
fn a_dragged_segment_on_drum16_walks_round_and_springs_back() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    let net = board.net("hats.yin").expect("fixture setup: the net exists");
    let track = board.tracks.iter().find(|t| t.net == net && t.path.start == P::mm(162.2, 98.59))
        .expect("fixture setup: the voice output's track").clone();
    // Its long straight run, above a transistor's pads.
    let run = points(&track.path);
    assert_eq!((run[2], run[3]), (P::mm(158.2, 94.5), P::mm(132.4, 94.5)), "fixture setup: the dragged segment");
    assert!(matches!(track.origin, Origin::Solved { .. }), "fixture setup: the router laid the dragged track");
    let (_, q2) = drum.pad("hats.cymg.q2", "2");
    let press = P::mm(145.3, 94.5);
    let grab = |seg: u32, at: P| {
        let start = Anchor::Track { id: track.id, at: OnTrack::along(seg, run[seg as usize], None, run[seg as usize + 1], at) };
        let at = board.anchor_pos(start).expect("the press is on the segment");
        let mut drag = GestureSample {
            id: GestureId(9), seq: 1,
            gesture: Gesture::new(GestureKind::DragSeg { track: track.id, seg }, board.unit, board.generation, start, at, track.layer),
        };
        drag.gesture.net = Some(net);
        drag
    };
    let mut drag = grab(2, press);
    let at = drag.gesture.start_at;
    let mut state = GestureState::default();
    let answered = |drag: &GestureSample, state: &mut GestureState| match answer(&drum.base, &drum.world, drag, state, ProposalId(1)) {
        Answer::Proposal(proposal) => proposal,
        other => panic!("seq {}: a drag of a 45-degree segment is answered: {other:?}", drag.seq),
    };

    // Down onto the transistor's pads, above their centres: the run walks round them there and
    // stays where it was put elsewhere.
    let by = P::new(0, q2.y - mm(0.2) - run[2].y);
    drag.gesture.cursor = at.add(by);
    let moved = answered(&drag, &mut state);
    let [BoardOp::Replace(Item::Track(laid))] = &moved.ops[..] else { panic!("a drag is one Replace of the track: {:?}", moved) };
    assert!(moved.verified, "the dragged track verifies: {:?} {:?}", moved.collisions, laid.path);
    assert_eq!((laid.id, laid.ends, laid.net, laid.layer, laid.w), (track.id, track.ends, track.net, track.layer, track.w),
        "the dragged track keeps its id, both ends, net, layer and width");
    assert_eq!(laid.origin, Origin::Authored, "a person's drag adopts the router's track: it is theirs to edit and delete");
    assert_eq!((laid.path.start, laid.path.end()), (track.path.start, track.path.end()), "its ends stay on their pads");
    // Fills are derived: a track moved into a pour's neck cuts it (the pour-cut chip says so) and
    // DRC lists the cut pads as unconnected. Nothing else is new, and nothing on the dragged net.
    let found = drum.new_violations(&moved.ops);
    let kinds: Vec<CheckKind> = found.iter().map(|v| v.kind.clone()).filter(|kind| *kind != CheckKind::Unconnected).collect();
    assert_eq!(kinds, Vec::<CheckKind>::new(), "the dragged track adds no copper finding in DRC");
    assert!(found.iter().all(|v| !v.items.contains(&Sel::Item(board.unit, track.id))), "the dragged track stays connected: {found:?}");
    assert!(y_at(&laid.path, press.x).is_some_and(|y| run[2].y < y && y <= run[2].y + by.y), "the run moved toward the cursor: {:?}", laid.path);
    assert!(y_at(&laid.path, q2.x).is_some_and(|y| y < q2.y - mm(0.2)), "the run walks round the pads on the side it was dragged from: {:?}", laid.path);

    // Back to the press: the track as it was.
    drag.seq += 1;
    drag.gesture.cursor = at;
    let back = answered(&drag, &mut state);
    assert!(back.ops.is_empty(), "back at the press, nothing changes: {:?}", back.ops);
    assert_eq!(back.head.as_ref().map(|head| &head.path), Some(&track.path), "it springs back to its base path");

    // A sample whose press is not on what it names: what was grabbed is not where it was.
    drag.seq += 1;
    drag.gesture.kind = GestureKind::DragSeg { track: track.id, seg: 1 };
    assert!(matches!(answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)), Answer::Refused(SampleCause::Invalid, _)),
        "a sample naming another segment than its press is refused as invalid");
    // What this tier does not lay again is refused, never restyled: the any-angle pad stub, and
    // a corner on the track's end.
    let mut stub = grab(0, P::new((run[0].x + run[1].x) / 2, (run[0].y + run[1].y) / 2));
    stub.gesture.cursor = stub.gesture.start_at.add(P::mm(0.3, 0.0));
    let mut state = GestureState::default();
    assert!(matches!(answer(&drum.base, &drum.world, &stub, &mut state, ProposalId(1)), Answer::Refused(SampleCause::Unsupported, _)),
        "an any-angle segment is not dragged through the octilinear embedder");
    stub.gesture.kind = GestureKind::DragCorner { track: track.id, vertex: 0 };
    stub.seq += 1;
    assert!(matches!(answer(&drum.base, &drum.world, &stub, &mut state, ProposalId(1)), Answer::Refused(SampleCause::Unsupported, _)),
        "a track's end does not move off what it is attached to");
}

/// Shove slice 4's guard: a segment dragged across a neighbour's track pushes it, all or none.
/// Tier 1 lays the span round the fixed things only, where the cursor puts it when none is in the
/// way, and says a wave follows. Its Full answer pushes the neighbour (ends kept, the dragged track as tier 1 laid it);
/// or, when the neighbour cannot be pushed, walks: the dragged track laid round everything as it
/// is, or left where it is when that is blocked, naming the track it could not push. Either way no
/// two tracks collide, DRC finds nothing new on copper, the same sample gives the same ops again,
/// and the answer undoes exactly.
#[test]
fn a_dragged_segment_on_drum16_pushes_the_track_in_its_way_or_walks() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    let net_of = |name: &str| board.net(name).expect("fixture setup: the net exists");
    let is_track = |sel: &Sel| matches!(sel, Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == *id));
    // (the dragged track's net, its start, the neighbour's net, whether the span is where the
    // cursor puts it, what the Full answer does: whether it pushes, whether a via in the
    // neighbour's way is pushed with it, whether its walk is laid when it does not)
    for (name, start, neighbour, at_cursor, pushes, via, walk_laid) in [
        ("hats.chp", P::mm(173.24, 108.59), "hats.ochop", true, true, false, true),
        ("snare.hiss.st3.g", P::mm(63.94, 118.94), "NOISE", true, true, true, true),
        ("chain.steps[15].d.A", P::mm(195.8, 31.0), "COL3", true, false, false, false),
    ] {
        let what = format!("{name}'s second segment moved 0.6 mm toward -y");
        let track = board.tracks.iter().find(|t| t.net == net_of(name) && t.path.start == start).expect("fixture setup: the dragged track");
        let other = net_of(neighbour);
        let run = points(&track.path);
        let mid = P::new((run[1].x + run[2].x) / 2, run[1].y);
        assert_eq!(run[1].y, run[2].y, "fixture setup: {what}: a level segment");
        let press = Anchor::Track { id: track.id, at: OnTrack::along(1, run[1], None, run[2], mid) };
        let at = board.anchor_pos(press).expect("the press is on the segment");
        let mut drag = GestureSample {
            id: GestureId(15), seq: 1,
            gesture: Gesture::new(GestureKind::DragSeg { track: track.id, seg: 1 }, board.unit, board.generation, press, at, track.layer),
        };
        drag.gesture.net = Some(track.net);
        drag.gesture.cursor = at.add(P::mm(0.0, -0.6));
        let release = |state: &mut GestureState| {
            let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, state, ProposalId(1)) else { panic!("{what}: the drag is answered") };
            let (full, _) = shove(&drum.base, &drum.world, state, ProposalId(2)).expect("a wave follows");
            (stretched, full)
        };
        let (stretched, full) = release(&mut GestureState::default());
        assert!(!stretched.last && stretched.collisions.iter().any(|c| matches!(c.b, Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == id && t.net == other))),
            "{what}: tier 1 lays the span where it is put, across the {neighbour} track, and a wave follows: {:?}", stretched.collisions);
        let [BoardOp::Replace(Item::Track(laid))] = &stretched.ops[..] else { panic!("{what}: tier 1 is one Replace of the dragged track") };
        assert_eq!(y_at(&laid.path, mid.x) == Some(mid.y - mm(0.6)), at_cursor, "{what}: whether the span is where the cursor put it: {:?}", laid.path);
        let head = full.head.as_ref().expect("a drag's answer has a head");
        let others: Vec<&fab_elec_board::Track> = full.ops.iter().filter_map(|op| match op {
            BoardOp::Replace(Item::Track(t)) if t.id != track.id => Some(t),
            _ => None,
        }).collect();
        let vias = pushed_vias(board, &full.ops);
        assert_eq!(!vias.is_empty(), via, "{what}: whether a via in the {neighbour} track's way is pushed with its tracks: {:?}", full.ops);
        let others: Vec<&fab_elec_board::Track> = others.into_iter()
            .filter(|t| !t.ends.iter().any(|e| matches!(*e, Anchor::Via(v) if vias.iter().any(|(id, _)| *id == v)))).collect();
        if pushes {
            assert!(head.unpushed.is_none() && full.ops.first() == stretched.ops.first(), "{what}: the dragged track stays as tier 1 laid it: {:?}", full.ops);
            let [pushed] = others[..] else { panic!("{what}: the {neighbour} track is pushed: {:?}", full.ops) };
            let base_track = board.tracks.iter().find(|t| t.id == pushed.id).expect("the pushed track is on the board");
            assert!(pushed.net == other && pushed.ends == base_track.ends && (pushed.path.start, pushed.path.end()) == (base_track.path.start, base_track.path.end()),
                "{what}: the {neighbour} track is pushed with its ends kept");
        } else {
            assert!(others.is_empty(), "{what}: a track it cannot push moves no track: {:?}", full.ops);
            assert_eq!(head.unpushed.as_ref().map(|(net, _)| *net), Some(other), "{what}: the answer names the {neighbour} track it could not push");
            assert_eq!((full.ops.len(), head.blocked.is_none()), (usize::from(walk_laid), walk_laid),
                "{what}: it walks round, or stays where it is when that is blocked: {:?} {:?}", full.ops, head.blocked);
        }
        assert!(full.verified && !full.collisions.iter().any(|c| is_track(&c.a) && is_track(&c.b)), "{what}: no two tracks collide: {:?}", full.collisions);
        let copper: Vec<_> = drum.new_findings(&full.ops).into_iter()
            .filter(|kind| !matches!(kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected)).collect();
        assert!(copper.is_empty(), "{what}: DRC finds nothing new on copper: {copper:?}");
        assert_eq!(release(&mut GestureState::default()).1.ops, full.ops, "{what}: the same sample answered again gives the same ops");
        let mut after = (**board).clone();
        let applied = after.apply_all(&full.ops).expect("the answer applies as one transaction");
        after.undo(&applied.inverse).expect("the undo applies");
        assert_eq!(after.content_hash(), board.content_hash(), "{what}: the undo restores the board exactly");
    }
}

/// The shove answer's slice 5 guard: copper drawn on drum16 in Shove mode that meets nothing is
/// the Walk-mode answer; drawn over a neighbour's track it pushes that track, by a finger round the
/// head where the head crosses it or aside where it only meets it, the track's ends kept and the
/// route as tier 1 drew it; when the track cannot be pushed the route walks, as in Walk mode, and
/// names it. Every Full answer: no two tracks collide, DRC finds nothing new on copper, the same
/// sample gives the same ops again, and the answer undoes exactly.
#[test]
fn copper_drawn_in_shove_mode_on_drum16_pushes_the_track_in_its_way_or_walks() {
    // (the redrawn track's net and start, the segment whose middle the head turns off at, the
    // track it meets, whether that is pushed, by how many fingers)
    let cases = [
        ("hats.chp", (173.24, 108.59), 1, "hats.ochop", true, 1),
        ("snare.gate.tb", (76.17, 118.94), 1, "snare.hiss.s2", true, 0),
        ("snare.hiss.st1.g", (78.01, 123.94), 1, "VREF", false, 0),
    ];
    let (drum, removed) = Drum::without_all(&Drum::board(), &cases.map(|(name, from, ..)| (name, from)));
    let board = &drum.base.board;
    let is_track = |sel: &Sel| matches!(sel, Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == *id));
    for ((name, _, k, neighbour, pushes, fingers), removed) in cases.into_iter().zip(removed) {
        let run = points(&removed.path);
        let mut drawn = sample(&drum.base, removed.ends[0], run[0]);
        drawn.gesture.net = Some(removed.net);
        drawn.gesture.width = WidthRule::Fixed(removed.w);
        drawn.gesture.layer = removed.layer;
        drawn.gesture.mode = Mode::Shove;

        // Along its own path to its pad it meets no track: the Walk answer.
        let mut along = drawn.clone();
        along.gesture.trail = run.clone();
        along.gesture.cursor = removed.path.end();
        along.gesture.target = Some(removed.ends[1]);
        let shoved = solve(&drum.base, &drum.world, &along, &mut GestureState::default());
        along.gesture.mode = Mode::Walk;
        let walked = solve(&drum.base, &drum.world, &along, &mut GestureState::default());
        assert!(shoved.last && shoved.ops == walked.ops && shoved.head == walked.head,
            "{name} drawn along its own path: meeting no track, Shove answers as Walk does");

        // Turned off its segment k at the middle, 0.8 mm to the right, over the neighbour.
        let (a, b) = (run[k], run[k + 1]);
        let mid = P::new(a.x + (b.x - a.x) / 2, a.y + (b.y - a.y) / 2);
        let (ux, uy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        let l = ux.hypot(uy);
        let tip = P::new(mid.x + (uy / l * 8e5) as Nm, mid.y - (ux / l * 8e5) as Nm);
        let what = format!("{name} drawn to 0.8 mm off its segment {k}");
        let other = board.net(neighbour).expect("fixture setup: the net exists");
        drawn.gesture.trail = run[..=k].iter().copied().chain([mid, tip]).collect();
        drawn.gesture.cursor = tip;
        let release = |state: &mut GestureState| {
            let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drawn, state, ProposalId(1)) else { panic!("{what}: the sample is answered") };
            let (full, stats) = shove(&drum.base, &drum.world, state, ProposalId(2)).expect("a wave follows");
            (stretched, full, stats)
        };
        let (stretched, full, stats) = release(&mut GestureState::default());
        assert!(!stretched.last && stretched.collisions.iter().any(|c| matches!(c.b, Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == id && t.net == other))),
            "{what}: tier 1 draws the head over the {neighbour} track, and a wave follows: {:?}", stretched.collisions);
        let head = full.head.as_ref().expect("a route answer has a head");
        let added = |p: &RouteProposal| p.ops.iter().filter(|op| matches!(op, BoardOp::Add(_))).cloned().collect::<Vec<_>>();
        let pushed: Vec<&fab_elec_board::Track> = full.ops.iter().filter_map(|op| match op {
            BoardOp::Replace(Item::Track(t)) => Some(t),
            _ => None,
        }).collect();
        if pushes {
            assert!(head.unpushed.is_none() && head.blocked.is_none() && head.path.end() == tip && added(&full) == added(&stretched),
                "{what}: the route stays as tier 1 drew it, to the cursor: {:?}", full.ops);
            let [pushed] = pushed[..] else { panic!("{what}: the {neighbour} track is pushed: {:?}", full.ops) };
            let base_track = board.tracks.iter().find(|t| t.id == pushed.id).expect("the pushed track is on the board");
            assert!(pushed.net == other && pushed.ends == base_track.ends && (pushed.path.start, pushed.path.end()) == (base_track.path.start, base_track.path.end()),
                "{what}: the {neighbour} track is pushed with its ends kept");
            assert_eq!(stats.spikes, fingers, "{what}: the head crosses the {neighbour} track {fingers} times, pushing a finger of it ahead of its tip each time");
        } else {
            assert!(pushed.is_empty(), "{what}: a track it cannot push moves no track: {:?}", full.ops);
            assert_eq!(head.unpushed.as_ref().map(|(net, _)| *net), Some(other), "{what}: the answer names the {neighbour} track it could not push");
            let mut walk = drawn.clone();
            walk.gesture.mode = Mode::Walk;
            let walked = solve(&drum.base, &drum.world, &walk, &mut GestureState::default());
            assert!(full.ops == walked.ops && HeadAnswer { unpushed: None, ..head.clone() } == *walked.head.as_ref().expect("a route answer has a head"),
                "{what}: it walks as Walk mode does");
        }
        assert!(full.verified && !full.collisions.iter().any(|c| is_track(&c.a) && is_track(&c.b)), "{what}: no two tracks collide: {:?}", full.collisions);
        let copper: Vec<_> = drum.new_findings(&full.ops).into_iter()
            .filter(|kind| !matches!(kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected)).collect();
        assert!(copper.is_empty(), "{what}: DRC finds nothing new on copper: {copper:?}");
        assert_eq!(release(&mut GestureState::default()).1.ops, full.ops, "{what}: the same sample answered again gives the same ops");
        let mut after = (**board).clone();
        let applied = after.apply_all(&full.ops).expect("the answer applies as one transaction");
        after.undo(&applied.inverse).expect("the undo applies");
        assert_eq!(after.content_hash(), board.content_hash(), "{what}: the undo restores the board exactly");
    }
}

/// Landing 4's guard, with the cut-back and the rank-1 shove of the shove answer's slices 1 and 2:
/// a part moved with its attached tracks is one transaction of the release answer's ops (the
/// wave's Full answer, when the stretch answer is not the last); its own fanout vias that ride
/// with it move by its own translation, and their tracks are attached too; every attached track is still
/// anchored at both ends, or cut back to copper on its base path from its unmoved end (its net in
/// `open`); a neighbour's track is pushed with its ends kept, or stays as it was; a track anchored
/// on a changed one keeps its copper (anchored at the same point, or freed); applied, the board's
/// copper is exactly the ops' (no anchored end is dragged anywhere unchecked); no committed
/// collision is between two tracks; each attached or pushed track is clean in DRC or named in
/// `collisions`; the same sample answered again gives the same ops; and undoing it restores the
/// board exactly.
#[test]
fn a_moved_part_on_drum16_stretches_its_tracks_and_undoes_exactly() {
    use fab_elec_board::Track;
    use fab_elec_route::Reason;

    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    let find = |name: &str| board.placed.iter().find(|p| p.path == name || p.refdes == name).expect("fixture setup: the part").clone();
    let (part, led, resistor) = (find("hats.cymg.q2"), find("D17"), find("R22"));
    let (diode, key_diode) = (find("D29"), find("D24"));
    let attached_count = |part: &Placed| board.tracks.iter().filter(|t| t.ends.iter().any(|a| a.item() == Some(part.id))).count();
    let busiest = board.placed.iter().max_by_key(|p| (attached_count(p), std::cmp::Reverse(p.id))).expect("drum16 has parts");
    let up_left = P::new(-1_079_250, -996_855);
    assert!(attached_count(&part) >= 3, "fixture setup: the transistor has a track on each pad");
    // The sharpest turn of a path, as the cosine of its angle (1 straight on, -1 doubling back).
    let sharpest = |path: &Path| {
        let mut points = vec![path.start];
        points.extend(path.segs.iter().map(|seg| seg.end()));
        points.windows(3).map(|w| {
            let (a, b) = (w[1].sub(w[0]), w[2].sub(w[1]));
            let (ax, ay, bx, by) = (a.x as f64, a.y as f64, b.x as f64, b.y as f64);
            let n = (ax * ax + ay * ay).sqrt() * (bx * bx + by * by).sqrt();
            if n == 0.0 { 1.0 } else { (ax * bx + ay * by) / n }
        }).fold(1.0f64, f64::min)
    };
    let mut state = GestureState::default();
    let mut drag = GestureSample {
        id: GestureId(11), seq: 0,
        gesture: Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU),
    };
    let base_of = |id: ItemId| board.tracks.iter().find(|b| b.id == id).expect("the track is on the board");
    let is_track = |sel: &Sel| matches!(sel, Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == *id));
    // Every vertex of `kept` lies on `base`.
    let on_base = |kept: &Path, base: &Path| points(kept).iter().all(|&p| base.locate(p).1 == p);
    // Each move; whether it has the room to stretch every attached track clean; and how many of
    // them it cuts back, where that is known.
    for (part, by, what, room, cuts) in [
        (&part, P::mm(0.6, 0.0), "right", false, None), (&part, P::mm(0.0, -0.8), "up, toward the run above it", false, None),
        (&part, P::mm(-0.5, 0.5), "down and left", false, None), (&led, P::mm(4.0, -2.5), "the LED past its stub's first corner", true, Some(0)),
        (&resistor, P::mm(1.0, -0.5), "the resistor below its other track's corner", true, Some(0)),
        (&resistor, P::mm(1.5, -1.0), "the resistor 1.5 mm, where the wave lays one track again round the other", true, Some(0)),
        (&diode, P::mm(0.0, -0.5), "a diode whose track laid again has a junction on it, at another segment now", true, Some(0)),
        (&key_diode, P::mm(0.0, -1.0), "a diode whose track laid again leaves a junction that was on it, and the joined track follows", true, Some(0)),
        (&key_diode, P::mm(-1.0, 0.0), "a diode moved left, whose track's diagonal and stub are laid again as one run", true, Some(0)),
    ] {
        drag.seq += 1;
        drag.gesture.moved = vec![(part.id, Xform::translate(by))];
        let release = |state: &mut GestureState| {
            let stretched = match answer(&drum.base, &drum.world, &drag, state, ProposalId(1)) {
                Answer::Proposal(proposal) => proposal,
                other => panic!("{what}: a part drag is answered, never refused: {other:?}"),
            };
            if stretched.last {
                return stretched;
            }
            let (full, _) = shove(&drum.base, &drum.world, state, ProposalId(2)).expect("a wave follows a stretch answer that is not the last");
            assert!(full.last && full.source == ProposalSource::Gesture { id: drag.id, seq: drag.seq, tier: Tier::Full },
                "{what}: the wave's Full answer is the last, for the same sample: {:?}", full.source);
            full
        };
        let moved = release(&mut state);
        assert_eq!(release(&mut GestureState::default()).ops, moved.ops, "{what}: the same sample answered again gives the same ops");
        assert_eq!(moved.ops.first(), Some(&BoardOp::Move { item: part.id, by }), "{what}: the part's move comes first");
        // The part's own fanout vias that ride with it, by its own translation; their tracks are
        // attached to what moves too.
        let own = |via: ItemId| board.tracks.iter().any(|t| t.ends.contains(&Anchor::Via(via)) && t.ends.iter().any(|a| a.item() == Some(part.id)));
        let riding: Vec<ItemId> = moved.ops[1..].iter().filter_map(|op| match *op {
            BoardOp::Move { item, by: rode } => {
                assert!(own(item) && rode == by, "{what}: only the part's own fanout vias ride with it, by its translation: {op:?}");
                Some(item)
            }
            _ => None,
        }).collect();
        let on_moved = |end: Anchor| end.item().is_some_and(|item| item == part.id || riding.contains(&item));
        let attached: Vec<ItemId> = board.tracks.iter().filter(|t| t.ends.iter().any(|&end| on_moved(end))).map(|t| t.id).collect();
        // A failed follower can keep its whole base path, with only the end freed. Its cut
        // reason names the track whose lay met the obstacle.
        let cut_entries = moved.open.iter().take_while(|(_, why)| !matches!(why, Reason::Unjoined { .. } | Reason::SharpCorner { .. })).count();
        let cut_lay = |id: ItemId| moved.open[..cut_entries].iter().any(|(net, why)| {
            *net == base_of(id).net && matches!(why, Reason::NoRoom { between, .. } if between[0] == Sel::Item(board.unit, id))
        });
        let endpoint = |path: &Path, e: usize| if e == 0 { path.start } else { path.end() };
        let following_end = |t: &Track, base: &Track| (0..2).find(|&e| matches!((base.ends[e], t.ends[e]),
            (Anchor::Track { id: old, .. }, Anchor::Track { id, .. }) if old == id)
            && endpoint(&base.path, e) != endpoint(&t.path, e));
        let mut cut = Vec::new();
        let mut replaced = Vec::new();
        let mut pushed = Vec::new();
        let mut followed = Vec::new();
        for op in &moved.ops[1..] {
            match op {
                BoardOp::Move { item, .. } if riding.contains(item) => {}
                BoardOp::Replace(Item::Track(t)) if attached.contains(&t.id) => {
                    let base = base_of(t.id);
                    assert_eq!(t.origin, base.origin, "{what}: a part drag's stretch keeps track {:?}'s origin (the router's stays the router's)", t.id);
                    // A cut frees its end on the part. The junction end may stay at its point,
                    // follow its host, or come free.
                    let cut_end = (0..2).any(|e| base.ends[e].item() == Some(part.id) && t.ends[e] == Anchor::Free);
                    if !cut_end {
                        let follows = following_end(t, base);
                        assert!((0..2).all(|e| follows == Some(e) || on_moved(base.ends[e]) && t.ends[e] == base.ends[e] || kept_end(t, base, e)),
                            "{what}: track {:?} laid again keeps its ends or follows its junction host: {:?} from {:?}", t.id, t.ends, base.ends);
                        if follows.is_some() { followed.push(t.id); }
                        assert!(sharpest(&t.path) >= sharpest(&base.path).min(0.0) - 1e-9,
                            "{what}: track {:?} laid again turns no more sharply than a right angle or its base path: {:?} from {:?}", t.id, t.path, base.path);
                        replaced.push(t.id);
                    } else {
                        let unmoved = (0..2).find(|&e| base.ends[e].item() != Some(part.id)).expect("a cut track has an unmoved end");
                        assert!(kept_end(t, base, unmoved) && t.ends[1 - unmoved] == Anchor::Free,
                            "{what}: cut track {:?} keeps its unmoved end and frees the other: {:?}", t.id, t.ends);
                        assert!(on_base(&t.path, &base.path), "{what}: cut track {:?} keeps copper on its base path: {:?} of {:?}", t.id, t.path, base.path);
                        cut.push(t.id);
                    }
                }
                BoardOp::Remove(id) if attached.contains(id) => cut.push(*id),
                BoardOp::Remove(id) if cut_lay(*id) && base_of(*id).ends.iter().any(|a| matches!(a, Anchor::Track { .. })) => cut.push(*id),
                BoardOp::Replace(Item::Track(t)) if cut_lay(t.id) => {
                    let base = base_of(t.id);
                    assert!((0..2).any(|e| matches!(base.ends[e], Anchor::Track { .. }) && t.ends[e] == Anchor::Free
                        && kept_end(t, base, 1 - e)), "{what}: a cut follower frees its end on the host and keeps its other end");
                    assert!(on_base(&t.path, &base.path), "{what}: a cut follower keeps copper on its base path");
                    cut.push(t.id);
                }
                BoardOp::Replace(Item::Track(t)) if t.path != base_of(t.id).path => {
                    let base = base_of(t.id);
                    if let Some(e) = following_end(t, base) {
                        assert!(kept_end(t, base, 1 - e) && endpoint(&t.path, 1 - e) == endpoint(&base.path, 1 - e),
                            "{what}: track {:?} follows its host with its other end kept", t.id);
                        followed.push(t.id);
                    } else {
                        assert!((0..2).all(|e| kept_end(t, base, e)), "{what}: a pushed track {:?} keeps its ends: {:?}", t.id, t.ends);
                        pushed.push(t.id);
                    }
                }
                BoardOp::Replace(Item::Track(t)) => {
                    assert!(t.path == base_of(t.id).path && t.ends.iter().zip(&base_of(t.id).ends).any(|(new, old)| new != old
                        && matches!(old, Anchor::Track { .. })),
                        "{what}: on another track only an end anchored on a changed track changes, its copper kept: {op:?}");
                }
                other => panic!("{what}: then only the attached tracks are laid again or cut: {other:?}"),
            }
        }
        assert!(!replaced.is_empty() || !cut.is_empty(), "{what}: the stretch lays an attached track again or cuts one: {:?}", moved.collisions);
        assert_eq!(cut_entries, cut.len(), "{what}: each cut track's net is open, with why: {:?}", moved.open);
        if let Some(cuts) = cuts {
            assert_eq!(cut.len(), cuts, "{what}: the tracks cut back: {cut:?} {:?}", moved.open);
        }
        assert!(!moved.collisions.iter().any(|c| is_track(&c.a) && is_track(&c.b)),
            "{what}: no committed collision is between two tracks: {:?}", moved.collisions);

        let mut after = (**board).clone();
        let applied = after.apply_all(&moved.ops).expect("the answer applies as one transaction");
        let mut lost = BTreeSet::new();
        let mut lost_ends = 0;
        for t in &after.tracks {
            if followed.contains(&t.id) {
                for (e, anchor) in t.ends.iter().enumerate() {
                    assert_eq!(after.anchor_pos(*anchor), Some(endpoint(&t.path, e)),
                        "{what}: end {e} of follower {:?} lies on its applied anchor", t.id);
                }
            }
            let original = base_of(t.id);
            for (e, &anchor) in original.ends.iter().enumerate() {
                let Anchor::Track { id: host, .. } = anchor else { continue };
                if !matches!(t.ends[e], Anchor::Track { id, .. } if id == host) {
                    let at = board.anchor_pos(anchor).expect("the base junction has a point on its host");
                    lost.insert((t.id, at, host));
                    lost_ends += 1;
                }
            }
            let said = moved.ops.iter().find_map(|op| match op {
                BoardOp::Replace(Item::Track(r)) if r.id == t.id => Some(&r.path),
                _ => None,
            });
            match said {
                Some(path) => assert_eq!(&t.path, path, "{what}: applied, track {:?} has the copper its op says", t.id),
                None if !attached.contains(&t.id) => assert_eq!(t.path, original.path, "{what}: applied, track {:?} that no op names keeps its copper", t.id),
                None => {}
            }
        }
        // After the cuts come the opened junctions, then the sharp corners the answer kept.
        let corner_entries = moved.open.iter().rev().take_while(|(_, why)| matches!(why, Reason::SharpCorner { .. })).count();
        let junction_entries = &moved.open[cut_entries..moved.open.len() - corner_entries];
        for (_, why) in &moved.open[moved.open.len() - corner_entries..] {
            let Reason::SharpCorner { at, track: Sel::Item(_, id), .. } = why else { unreachable!() };
            let t = after.tracks.iter().find(|t| t.id == *id).expect("a kept sharp corner names a track on the board");
            let mut pts = vec![t.path.start];
            pts.extend(t.path.segs.iter().map(|seg| seg.end()));
            assert!(pts.windows(3).any(|w| w[1] == *at && sharpest(&Path { start: w[0], segs: vec![Seg::Line(w[1]), Seg::Line(w[2])] }) < 0.0),
                "{what}: a kept sharp corner of track {id:?} is one on its applied path at {at:?}");
        }
        let mut reported = BTreeSet::new();
        for (net, why) in junction_entries {
            let Reason::Unjoined { at, track: Sel::Item(unit, track), host: Sel::Item(host_unit, host) } = why else {
                panic!("{what}: after the cuts, each entry names a track and its former host: {why:?}");
            };
            assert_eq!((*unit, *host_unit), (board.unit, board.unit), "{what}: both tracks are on the dragged board");
            assert_eq!(*net, base_of(*track).net, "{what}: the entry names the unjoined track's net");
            reported.insert((*track, *at, *host));
        }
        assert_eq!(reported, lost, "{what}: the answer names exactly the junctions whose ends came off their hosts");
        assert_eq!(junction_entries.len(), lost_ends, "{what}: one entry per end that came off its host");
        if part.id == key_diode.id {
            assert!(!followed.is_empty(), "{what}: the junction follows the diode's track");
            assert!(reported.iter().all(|(id, _, _)| !followed.contains(id)), "{what}: the follower keeps its junction");
        }
        for id in attached.iter().filter(|id| !cut.contains(id)) {
            let t = after.tracks.iter().find(|t| t.id == *id).expect("the attached track is still there");
            for (e, at) in [(0, t.path.start), (1, t.path.end())] {
                let freed = t.ends[e] == Anchor::Free && matches!(base_of(*id).ends[e], Anchor::Track { .. });
                assert!(freed || after.anchor_pos(t.ends[e]) == Some(at), "{what}: track {id:?} is still anchored at both ends (or a junction its host left is freed)");
            }
        }
        // A cut track's free end opens its net: unconnected and dangling are what a cut is.
        let named = |id: &ItemId| moved.collisions.iter().any(|c| [&c.a, &c.b].contains(&&Sel::Item(board.unit, *id)));
        for v in drum.new_violations(&moved.ops) {
            assert!(followed.iter().all(|&id| !v.items.contains(&Sel::Item(board.unit, id))),
                "{what}: the followed tracks {followed:?} are clear of every new DRC finding: {v:?}");
            if matches!(v.kind, CheckKind::Unconnected) { continue; }
            for id in attached.iter().chain(&pushed).filter(|id| v.items.contains(&Sel::Item(board.unit, **id))) {
                if cut.contains(id) && matches!(v.kind, CheckKind::Dangling) { continue; }
                assert!(named(id), "{what}: a DRC finding on attached or pushed track {id:?} is named in collisions: {v:?} {:?}", moved.collisions);
            }
        }
        assert_eq!(moved.verified, moved.collisions.is_empty(), "{what}: verified means no collision");
        assert!(!room || moved.verified, "{what}: with room, each attached track is laid clean of the others: {:?}", moved.collisions);
        after.undo(&applied.inverse).expect("the undo applies");
        assert_eq!(after.content_hash(), board.content_hash(), "{what}: the undo restores the board exactly");
    }

    // Back where it was: nothing changes.
    drag.seq += 1;
    drag.gesture.moved = vec![(part.id, Xform::translate(P::ZERO))];
    let Answer::Proposal(back) = answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)) else { panic!("the drag back is answered") };
    assert!(back.ops.is_empty() && back.verified, "back at its place, the part's answer changes nothing: {:?}", back.ops);

    // The common case: the busiest part moved up and left (a sample of the bench's 1 mm circle).
    // One of its attached tracks is T-joined to another attached track that the stretch lays
    // again. The wave's Full answer lays the joined end again onto its host's new copper (it
    // pushes vias too, which the loop above does not model, so only the follow is checked).
    let what = "the busiest part moved up and left";
    let on_busiest = |t: &Track| t.ends.iter().any(|a| a.item() == Some(busiest.id));
    let joined: Vec<(ItemId, usize, ItemId)> = board.tracks.iter().filter(|t| on_busiest(t)).flat_map(|t| (0..2).filter_map(move |e| match t.ends[e] {
        Anchor::Track { id: host, .. } if board.tracks.iter().any(|h| h.id == host && on_busiest(h)) => Some((t.id, e, host)),
        _ => None,
    })).collect();
    assert!(!joined.is_empty(), "fixture setup: an attached track of the busiest part is joined to another of its attached tracks");
    let mut state = GestureState::default();
    drag.seq += 1;
    drag.gesture = Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, busiest.at, LayerId::F_CU);
    drag.gesture.moved = vec![(busiest.id, Xform::translate(up_left))];
    let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)) else { panic!("{what}: answered") };
    let full = if stretched.last { stretched } else { shove(&drum.base, &drum.world, &mut state, ProposalId(2)).expect("the busiest part: a wave follows its stretch answer").0 };
    let mut after = (**board).clone();
    after.apply_all(&full.ops).expect("the answer applies as one transaction");
    let follows: Vec<ItemId> = joined.iter().filter_map(|&(id, e, host)| {
        let t = after.tracks.iter().find(|t| t.id == id)?;
        let at = if e == 0 { t.path.start } else { t.path.end() };
        let base_at = { let b = base_of(id); if e == 0 { b.path.start } else { b.path.end() } };
        (matches!(t.ends[e], Anchor::Track { id, .. } if id == host) && at != base_at).then_some(id)
    }).collect();
    assert!(!follows.is_empty(), "{what}: an attached track's joined end follows its host's new copper: {joined:?}");
    for id in &follows {
        let t = after.tracks.iter().find(|t| t.id == *id).expect("the follower is on the board");
        for (e, at) in [(0, t.path.start), (1, t.path.end())] {
            assert_eq!(after.anchor_pos(t.ends[e]), Some(at), "{what}: end {e} of follower {id:?} lies on its applied anchor");
            if let Anchor::Track { id: host, at: on } = t.ends[e] {
                let host = after.tracks.iter().find(|h| h.id == host).expect("the follower's host is on the board");
                assert_eq!(fab_elec_board::ops::on_track(at, &host.path), Some(on),
                    "{what}: end {e} of follower {id:?} has the anchor a reader gives its point, so the answer reads back as it is");
            }
        }
        assert!(!full.open.iter().any(|(_, why)| matches!(why, Reason::Unjoined { track: Sel::Item(_, t), .. } if t == id)),
            "{what}: no opened junction names the follower {id:?}: {:?}", full.open);
    }
    for v in drum.new_violations(&full.ops) {
        assert!(follows.iter().all(|&id| !v.items.contains(&Sel::Item(board.unit, id))), "{what}: the follower is clear of every new DRC finding: {v:?}");
    }

    // The busiest part moved left (the circle's sample 43), where two of its tracks laid again
    // would turn back on themselves: their corners are chamfered, so no attached track laid
    // again turns more sharply than a right angle or its base path, and no corner is kept.
    let what = "the busiest part moved left";
    let mut state = GestureState::default();
    drag.seq += 1;
    drag.gesture.moved = vec![(busiest.id, Xform::translate(P::new(-1_916_108, 400_931)))];
    let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)) else { panic!("{what}: answered") };
    let full = if stretched.last { stretched } else { shove(&drum.base, &drum.world, &mut state, ProposalId(2)).expect("the busiest part: a wave follows its stretch answer").0 };
    let laid: Vec<&Track> = full.ops.iter().filter_map(|op| match op {
        BoardOp::Replace(Item::Track(t)) if on_busiest(base_of(t.id)) => Some(t),
        _ => None,
    }).collect();
    assert!(laid.len() >= 2, "{what}: the answer lays attached tracks again: {:?}", full.ops);
    for t in laid {
        assert!(sharpest(&t.path) >= sharpest(&base_of(t.id).path).min(0.0) - 1e-9,
            "{what}: track {:?} laid again turns no more sharply than a right angle or its base path: {:?} from {:?}", t.id, t.path, base_of(t.id).path);
    }
    assert!(!full.open.iter().any(|(_, why)| matches!(why, Reason::SharpCorner { .. })), "{what}: no sharp corner is kept: {:?}", full.open);

    // A nudge of the busiest part (the bench circle's first sample, 0.06 mm). Where a short run's
    // rest would meet the moved pads, the stretch starts at a run that covers that segment.
    // So every attached track is laid again and none is cut.
    let what = "the busiest part nudged";
    let mut state = GestureState::default();
    drag.seq += 1;
    drag.gesture.moved = vec![(busiest.id, Xform::translate(P::new(-2_013, 63_424)))];
    let Answer::Proposal(nudged) = answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)) else { panic!("{what}: answered") };
    assert!(nudged.verified, "{what}: the stretch answer is legal: {:?}", nudged.collisions);
    assert!(nudged.open.is_empty(), "{what}: no attached track is cut: {:?}", nudged.open);
}


/// Shove slice 3's guard (track pushes track): a part moved so that what it pushes (a neighbour's
/// track, or its own track laid again) has no room until another track moves too: a neighbour's,
/// pushed though the part never met it, or the part's other track as the stretch laid it (R6's:
/// laid again out of the way, it must not go back where the stretch laid it once the wave is
/// done). That track joins the wave: the wave's Full answer opens
/// nothing (without the join the stretch answer's cut stays), verifies, keeps every pushed track's
/// ends, moves a track only where the part or another changed track meets its base copper, finds
/// nothing new in DRC, gives the same ops again and undoes exactly.
#[test]
fn a_track_pushed_on_drum16_pushes_the_next_one() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    for (name, by, joins_for_own, pushes_unmet) in
        [("SW14", P::mm(0.0, -1.0), false, true), ("R60", P::mm(0.0, 0.6), true, true), ("R6", P::mm(-1.0, -1.0), true, false)]
    {
        let what = format!("{name} by {by:?}");
        let part = board.placed.iter().find(|p| p.refdes == name).expect("fixture setup: the part");
        let attached: Vec<ItemId> = board.tracks.iter().filter(|t| t.ends.iter().any(|a| a.item() == Some(part.id))).map(|t| t.id).collect();
        let mut drag = GestureSample {
            id: GestureId(13), seq: 1,
            gesture: Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU),
        };
        drag.gesture.moved = vec![(part.id, Xform::translate(by))];
        let release = |state: &mut GestureState| -> (RouteProposal, RouteProposal, fab_elec_route::head::WaveStats) {
            let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, state, ProposalId(1)) else { panic!("{what}: the drag is answered") };
            let (full, stats) = shove(&drum.base, &drum.world, state, ProposalId(2)).expect("a wave follows");
            (stretched, full, stats)
        };
        let (stretched, full, stats) = release(&mut GestureState::default());
        assert_eq!(release(&mut GestureState::default()).1.ops, full.ops, "{what}: the same sample answered again gives the same ops");
        assert_eq!(!stretched.open.is_empty(), joins_for_own, "fixture setup: whether the stretch cuts the part's own track");
        assert!(full.verified && full.open.is_empty() && stats.cut == 0 && stats.joined >= 1,
            "{what}: a track in the way joins the wave and everything is laid: {stats:?} {:?} {:?}", full.open, full.collisions);
        let reshaped: Vec<&fab_elec_board::Track> = full.ops.iter().filter_map(|op| match op {
            BoardOp::Replace(Item::Track(t)) if board.tracks.iter().any(|b| b.id == t.id && b.path != t.path) => Some(t),
            _ => None,
        }).collect();
        // The vias pushed, with the tracks they carry; the answer's new copper.
        let vias = pushed_vias(board, &full.ops);
        let mut laid = World::new(&drum.world);
        for t in &reshaped {
            laid.remove_item(t.id);
            for piece in new_pieces(board, &Item::Track((*t).clone())) { laid.add(piece); }
        }
        for &(id, by) in &vias {
            laid.remove_item(id);
            for &rid in drum.world.pieces(id) { laid.add_moved(rid, by); }
        }
        let changed = |id: ItemId| reshaped.iter().any(|r| r.id == id) || vias.iter().any(|(v, _)| *v == id);
        let carried = |t: &fab_elec_board::Track| t.ends.iter().any(|e| matches!(*e, Anchor::Via(v) if vias.iter().any(|(id, _)| *id == v)));
        // A track or via the part never met moves only where another changed one meets its base copper.
        let met_part = |id: ItemId| stretched.collisions.iter().any(|c| [&c.a, &c.b].contains(&&Sel::Item(board.unit, id)));
        let pushed_by = |id: ItemId, pieces: &[fab_elec_route::RItem]| verify(&laid, &drum.base.rules, board, pieces).iter()
            .any(|c| matches!(c.b, Sel::Item(_, other) if other != id && changed(other)));
        let mut rank2 = 0;
        for t in reshaped.iter().filter(|t| !attached.contains(&t.id) && !carried(t)) {
            let base = board.tracks.iter().find(|b| b.id == t.id).expect("the pushed track is on the board");
            assert!((0..2).all(|e| kept_end(t, base, e)) && (t.path.start, t.path.end()) == (base.path.start, base.path.end()),
                "{what}: pushed track {:?} keeps its ends: {:?} from {:?}", t.id, t.ends, base.ends);
            if met_part(t.id) { continue; }
            assert!(pushed_by(t.id, &new_pieces(board, &Item::Track(base.clone()))),
                "{what}: track {:?}, which the part never met, moves only where another changed track or via meets its base copper", t.id);
            rank2 += 1;
        }
        // The part's own fanout vias ride with it by its translation; nothing pushes them.
        let rides = |id: ItemId, moved: P| moved == by && board.tracks.iter().any(|t| t.ends.contains(&Anchor::Via(id)) && attached.contains(&t.id));
        for &(id, _) in vias.iter().filter(|&&(id, moved)| !met_part(id) && !rides(id, moved)) {
            let base: Vec<_> = drum.world.pieces(id).iter().filter_map(|&rid| drum.world.get(rid)).cloned().collect();
            assert!(pushed_by(id, &base), "{what}: via {id:?}, which the part never met, moves only where a changed track or via meets it");
            rank2 += 1;
        }
        assert_eq!(rank2 >= 1, pushes_unmet, "{what}: whether a track the part never met is pushed: {:?}", full.ops);
        let copper: Vec<_> = drum.new_findings(&full.ops).into_iter()
            .filter(|kind| !matches!(kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected)).collect();
        assert!(copper.is_empty(), "{what}: DRC finds nothing new: {copper:?}");
        let mut after = (**board).clone();
        let applied = after.apply_all(&full.ops).expect("the answer applies as one transaction");
        after.undo(&applied.inverse).expect("the undo applies");
        assert_eq!(after.content_hash(), board.content_hash(), "{what}: the undo restores the board exactly");
    }
}

/// The wave's end: U4 moved so that its tracks block each other round in a ring, each pair one
/// way (a before b, b before c, then c blocked by a). That last pair closes a cycle
/// through the others, so one of the two is given up, and the wave ends: each repair records a
/// new pair the order keeps, at most one per two tracks that entered. The answer gives the same
/// ops again, commits no collision between two tracks, and undoes exactly.
#[test]
fn a_wave_whose_order_repairs_close_a_ring_on_drum16_ends() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    let part = board.placed.iter().find(|p| p.refdes == "U4").expect("fixture setup: the part");
    let mut drag = GestureSample {
        id: GestureId(14), seq: 1,
        gesture: Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU),
    };
    drag.gesture.moved = vec![(part.id, Xform::translate(P::new(mm(-1.866), mm(-0.5))))];
    let release = |state: &mut GestureState| {
        let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, state, ProposalId(1)) else { panic!("the drag is answered") };
        assert!(!stretched.last, "fixture setup: a wave follows the stretch answer");
        // Stepped as the worker steps it; a wave that repairs its order without end stops here.
        loop {
            if let Some(full) = shove_step(&drum.base, &drum.world, state, || ProposalId(2)) {
                return full;
            }
            let stats = state.wave_stats().expect("the wave goes on until its Full answer");
            assert!(stats.steps < 20_000, "the wave ends: {stats:?}");
        }
    };
    let (full, stats) = release(&mut GestureState::default());
    assert!(stats.repairs >= 2 && stats.repairs <= stats.members * (stats.members - 1) / 2,
        "each order repair records a new pair that closes no cycle: {stats:?}");
    assert_eq!(release(&mut GestureState::default()).0.ops, full.ops, "the same sample answered again gives the same ops");
    let is_track = |sel: &Sel| matches!(sel, Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == *id));
    assert!(!full.collisions.iter().any(|c| is_track(&c.a) && is_track(&c.b)), "no committed collision is between two tracks: {:?}", full.collisions);
    let mut after = (**board).clone();
    let applied = after.apply_all(&full.ops).expect("the answer applies as one transaction");
    after.undo(&applied.inverse).expect("the undo applies");
    assert_eq!(after.content_hash(), board.content_hash(), "the undo restores the board exactly");
}

/// Shove slice 2's guard (rank 1, the shove answer's S3 and S4): a part moved onto a neighbour's
/// track pushes it. The stretch answer is not the last; the wave's Full answer for the same sample
/// replaces the neighbour's track with its ends kept and its copper changed only in a bump round the
/// moved pads, which it passes on the side it passed them before: also where a pad's travel crossed
/// the track (transport by a spike in its reference), and where the part's own track is laid only
/// after the neighbour's (an order repair; without it the part's track is cut). The answer
/// verifies, DRC agrees, and it undoes exactly.
#[test]
fn a_part_moved_onto_a_neighbours_track_on_drum16_pushes_it() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    for (name, by, crossed, repaired) in [
        ("C7", P::mm(0.0, -1.0), false, false), ("D6", P::mm(0.0, 1.0), true, false), ("D31", P::mm(-0.7, -0.7), true, true),
    ] {
        let what = format!("{name} by {by:?}");
        let part = board.placed.iter().find(|p| p.refdes == name).expect("fixture setup: the part");
        let attached: Vec<ItemId> = board.tracks.iter().filter(|t| t.ends.iter().any(|a| a.item() == Some(part.id))).map(|t| t.id).collect();
        let mut drag = GestureSample {
            id: GestureId(12), seq: 1,
            gesture: Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU),
        };
        drag.gesture.moved = vec![(part.id, Xform::translate(by))];
        let mut state = GestureState::default();
        let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)) else { panic!("{what}: the drag is answered") };
        assert!(!stretched.last && state.waving(), "{what}: a moved pad on a neighbour's track: the stretch answer is not the last");
        // The same sample again is covered and keeps the wave; a newer one's answer drops it.
        assert!(matches!(answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)), Answer::Covered) && state.waving(), "{what}: a covered sample keeps the wave");
        let mut newer = drag.clone();
        newer.seq += 1;
        newer.gesture.moved = vec![(part.id, Xform::translate(P::ZERO))];
        let mut dropped = GestureState::default();
        let _ = answer(&drum.base, &drum.world, &drag, &mut dropped, ProposalId(1));
        assert!(matches!(answer(&drum.base, &drum.world, &newer, &mut dropped, ProposalId(1)), Answer::Proposal(p) if p.last), "{what}: back at its place, no wave");
        assert!(!dropped.waving() && shove(&drum.base, &drum.world, &mut dropped, ProposalId(2)).is_none(), "{what}: a newer sample's answer drops the wave unanswered");
        let (full, stats) = shove(&drum.base, &drum.world, &mut state, ProposalId(2)).expect("a wave follows");
        assert!(full.last && full.source == ProposalSource::Gesture { id: drag.id, seq: drag.seq, tier: Tier::Full }, "{what}: the Full answer is the last, for the sample");
        assert!(full.verified, "{what}: there is room to push: {:?}", full.collisions);
        // The pads whose copper met a neighbour's track, by track: their base and new centres.
        let mut met: Vec<(ItemId, P, P)> = Vec::new();
        for c in &stretched.collisions {
            let (Sel::Feature(fab_elec_core::FeatureRef::Pad(comp, pad)), Sel::Item(_, id)) = (&c.a, &c.b) else { continue };
            if comp.comp != part.comp || attached.contains(id) { continue; }
            let c0 = part.pad_at(*pad).expect("the pad has a place");
            met.push((*id, c0, c0.add(by)));
        }
        assert!(!met.is_empty(), "fixture setup: a moved pad meets a neighbour's track: {:?}", stretched.collisions);
        let mut pushed = 0;
        for op in &full.ops {
            let BoardOp::Replace(Item::Track(t)) = op else { continue };
            if attached.contains(&t.id) { continue; }
            let base = board.tracks.iter().find(|b| b.id == t.id).expect("the pushed track is on the board");
            if t.path == base.path {
                assert!((0..2).all(|e| kept_end(t, base, e)), "{what}: a track whose copper stays changes only an end on a changed track: {:?}", t.ends);
                continue;
            }
            pushed += 1;
            assert!((0..2).all(|e| kept_end(t, base, e)) && (t.path.start, t.path.end()) == (base.path.start, base.path.end()),
                "{what}: pushed track {:?} keeps its ends: {:?} from {:?}", t.id, t.ends, base.ends);
            let (old, new) = (points(&base.path), points(&t.path));
            for &p in new.iter().filter(|&&p| base.path.locate(p).1 != p) {
                let near = part.footprint.pads.iter().enumerate().filter_map(|(i, _)| part.pad_at(fab_elec_core::PadIx(i as u16)))
                    .any(|c0| c0.add(by).dist2(p) <= (mm(3.0) as i128).pow(2));
                assert!(near, "{what}: pushed track {:?} changes only in a bump round the moved pads: {p:?} in {new:?}", t.id);
            }
            for &(_, c0, c1) in met.iter().filter(|(id, ..)| *id == t.id) {
                assert_eq!(side::keep(&new, c1), side::keep(&old, c0), "{what}: pushed track {:?} passes the pad from {c0:?} on its old side", t.id);
            }
        }
        assert!(pushed >= 1 && stats.pushed == pushed, "{what}: the neighbour's track is pushed: {:?} {stats:?}", full.ops);
        assert!(full.open.is_empty() && stats.cut == 0 && (stats.repairs > 0) == repaired,
            "{what}: every track is laid, after an order repair where only one order works: {stats:?} {:?}", full.open);
        assert_eq!(crossed, met.iter().any(|&(id, c0, c1)| {
            let base = board.tracks.iter().find(|b| b.id == id).expect("the track");
            points(&base.path).windows(2).any(|w| {
                let o = |a: P, b: P, c: P| fab_elec_core::geom::orient(a, b, c).signum();
                o(w[0], w[1], c0) * o(w[0], w[1], c1) < 0 && o(c0, c1, w[0]) * o(c0, c1, w[1]) < 0
            })
        }), "fixture setup: whether a moved pad's travel crossed the track it meets");
        let copper: Vec<_> = drum.new_findings(&full.ops).into_iter()
            .filter(|kind| !matches!(kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected)).collect();
        assert!(copper.is_empty(), "{what}: DRC finds nothing new in the pushed answer: {copper:?}");
        let mut after = (**board).clone();
        let applied = after.apply_all(&full.ops).expect("the answer applies as one transaction");
        after.undo(&applied.inverse).expect("the undo applies");
        assert_eq!(after.content_hash(), board.content_hash(), "{what}: the undo restores the board exactly");
    }
}

/// Shove slice 6's guard (pushed vias): a part moved onto a fanout via pushes it, and the via's
/// tracks follow: a `Move` of the via, then each of its tracks laid with its end on the via where
/// the via goes and its other end and anchors kept; the answer verifies and cuts nothing. Moved
/// further, where the via has no room, the via goes back with all its tracks: no via moves, its
/// tracks stay as they were, and the part's collisions with them are still named. Every answer: no
/// two tracks collide, a track the part does not own keeps its ends, DRC finds nothing new on copper
/// but what is named against the kept via or its tracks (the part's own silk and courtyard go where
/// it is put), the same sample gives the same ops again, and the answer undoes exactly.
#[test]
fn a_part_moved_onto_a_via_on_drum16_pushes_it_and_its_tracks_follow() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    let is_track = |sel: &Sel| matches!(sel, Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == *id));
    // (the part, how far it moves, whether the via it meets is pushed)
    for (name, by, pushed) in [("C3", P::mm(-0.5, 0.0), true), ("R99", P::mm(0.0, 0.5), true), ("R99", P::mm(0.0, 0.8), false)] {
        let what = format!("{name} by {by:?}");
        let part = board.placed.iter().find(|p| p.refdes == name).expect("fixture setup: the part");
        let attached: Vec<ItemId> = board.tracks.iter().filter(|t| t.ends.iter().any(|a| a.item() == Some(part.id))).map(|t| t.id).collect();
        let mut drag = GestureSample {
            id: GestureId(12), seq: 1,
            gesture: Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU),
        };
        drag.gesture.moved = vec![(part.id, Xform::translate(by))];
        let release = |state: &mut GestureState| {
            let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, state, ProposalId(1)) else { panic!("{what}: the drag is answered") };
            let (full, stats) = shove(&drum.base, &drum.world, state, ProposalId(2)).expect("a wave follows");
            (stretched, full, stats)
        };
        let (stretched, full, stats) = release(&mut GestureState::default());
        let met: BTreeSet<ItemId> = stretched.collisions.iter().flat_map(|c| [&c.a, &c.b]).filter_map(|sel| match sel {
            Sel::Item(_, id) if board.vias.iter().any(|v| v.id == *id) => Some(*id),
            _ => None,
        }).collect();
        let [via] = met.into_iter().collect::<Vec<_>>()[..] else { panic!("fixture setup: {what}: the part meets one via: {:?}", stretched.collisions) };
        let fanned: Vec<&fab_elec_board::Track> = board.tracks.iter().filter(|t| t.ends.contains(&Anchor::Via(via))).collect();
        assert!(!stretched.last && !fanned.is_empty() && fanned.iter().all(|t| !attached.contains(&t.id)),
            "fixture setup: {what}: the via fans out tracks the part does not own, and a wave follows the stretch");
        let named = full.collisions.iter().any(|c| [&c.a, &c.b].contains(&&Sel::Item(board.unit, via)));
        let vias = pushed_vias(board, &full.ops);
        if pushed {
            assert_eq!(vias.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [via], "{what}: the via the part met is pushed, and only it: {:?}", full.ops);
            assert!(full.verified && full.open.is_empty() && (stats.vias, stats.vias_kept, stats.cut) == (1, 0, 0),
                "{what}: the via's tracks follow it and nothing is cut: {stats:?} {:?} {:?}", full.open, full.collisions);
        } else {
            assert!(vias.is_empty() && (stats.vias, stats.vias_kept) == (0, 1), "{what}: with no room the via goes back: {stats:?} {:?}", full.ops);
            for t in &fanned {
                assert!(!full.ops.iter().any(|op| matches!(op, BoardOp::Replace(Item::Track(n)) if n.id == t.id && n.path != t.path) || *op == BoardOp::Remove(t.id)),
                    "{what}: the kept via's track {:?} stays as it was: {:?}", t.id, full.ops);
            }
            assert!(named && !full.verified, "{what}: the kept via's collision is still named: {:?}", full.collisions);
        }
        for op in &full.ops {
            match op {
                BoardOp::Replace(Item::Track(t)) if !attached.contains(&t.id) => {
                    let base = board.tracks.iter().find(|b| b.id == t.id).expect("the changed track is on the board");
                    assert!((0..2).all(|e| kept_end(t, base, e)), "{what}: track {:?}, not the part's, keeps its ends: {:?} from {:?}", t.id, t.ends, base.ends);
                }
                BoardOp::Remove(id) => assert!(attached.contains(id), "{what}: only the part's own track is cut: {id:?}"),
                _ => {}
            }
        }
        assert!(!full.collisions.iter().any(|c| is_track(&c.a) && is_track(&c.b)), "{what}: no two tracks collide: {:?}", full.collisions);
        for v in drum.new_violations(&full.ops) {
            if matches!(v.kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected) { continue; }
            if matches!(v.kind, CheckKind::CourtyardOverlap | CheckKind::SilkOverPad | CheckKind::SilkClipped) {
                assert!(v.items.contains(&Sel::Item(board.unit, part.id)), "{what}: a new silk or courtyard finding is the moved part's own: {v:?}");
                continue;
            }
            let kept = std::iter::once(via).chain(fanned.iter().map(|t| t.id)).map(|id| Sel::Item(board.unit, id));
            let on_kept = kept.filter(|sel| v.items.contains(sel)).any(|sel| full.collisions.iter().any(|c| c.a == sel || c.b == sel));
            assert!(!pushed && on_kept, "{what}: DRC finds nothing new on copper but what is named against the kept via or its tracks: {v:?} {:?}", full.collisions);
        }
        assert_eq!(release(&mut GestureState::default()).1.ops, full.ops, "{what}: the same sample answered again gives the same ops");
        let mut after = (**board).clone();
        let applied = after.apply_all(&full.ops).expect("the answer applies as one transaction");
        after.undo(&applied.inverse).expect("the undo applies");
        assert_eq!(after.content_hash(), board.content_hash(), "{what}: the undo restores the board exactly");
    }
}

/// Own fanout (E6 3.2, shove cut 1): a moved part's own fanout vias ride with it. U1 moved 63 um
/// meets four of its fanout vias with its other pads, and moved 0.1 mm diagonally two (one with its
/// drill inside a pad's mask web, `ViaInPad`). In both, the
/// fanout vias it meets ride, and the ones it does not meet too, from the start of the drag; each
/// that rides is moved by U1's own
/// translation, in the stretch answer and so in the Full one; its tracks to U1 translated whole,
/// and its other tracks laid again from where it goes; the stretch answer names no collision with
/// a via that rides, the four U1 meets among them. Nothing is cut, DRC finds nothing new on copper,
/// the same sample gives the same ops, and the answer undoes exactly. With another net's via put
/// where one that U1 does not meet would go, that one stays and the others still ride.
#[test]
fn a_part_drag_on_drum16_carries_its_own_fanout_vias() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    let part = board.placed.iter().find(|p| p.refdes == "U1").expect("fixture setup: the part");
    let to_part = |via: ItemId| board.tracks.iter().filter(move |t| t.ends.contains(&Anchor::Via(via)) && t.ends.iter().any(|a| a.item() == Some(part.id)));
    let own: BTreeSet<ItemId> = board.vias.iter().map(|v| v.id).filter(|&v| to_part(v).next().is_some()).collect();
    let release = |drum: &Drum, by: P| {
        let mut drag = GestureSample {
            id: GestureId(15), seq: 1,
            gesture: Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU),
        };
        drag.gesture.moved = vec![(part.id, Xform::translate(by))];
        let mut state = GestureState::default();
        let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)) else { panic!("the drag is answered") };
        let full = if stretched.last { stretched.clone() } else { shove(&drum.base, &drum.world, &mut state, ProposalId(2)).expect("a wave follows").0 };
        let rode: BTreeSet<ItemId> = stretched.ops.iter().filter_map(|op| match *op {
            BoardOp::Move { item, by: rode } if item != part.id => {
                assert!(own.contains(&item) && rode == by, "only U1's own fanout vias ride, by its translation: {op:?}");
                Some(item)
            }
            _ => None,
        }).collect();
        (stretched, full, rode)
    };
    let mut last = (BTreeSet::new(), BTreeSet::new());
    for (what, by, meets) in [("U1 63 um onto four of its vias", P::new(-1_983, 62_948), 4), ("U1 0.1 mm diagonally", P::new(70_711, 70_711), 2)] {
        let pads: Vec<_> = drum.world.pieces(part.id).iter().filter_map(|&rid| drum.world.get(rid)).map(|it| it.translated(by)).collect();
        let mut without = World::new(&drum.world);
        without.remove_item(part.id);
        let met: BTreeSet<ItemId> = verify(&without, &drum.base.rules, board, &pads).iter().filter_map(|c| match c.b {
            Sel::Item(_, id) if own.contains(&id) => Some(id),
            _ => None,
        }).collect();
        assert_eq!(met.len(), meets, "fixture setup: {what}: U1's pads meet {meets} of its fanout vias: {met:?}");
        let (stretched, full, rode) = release(&drum, by);
        assert!(met.is_subset(&rode), "{what}: the fanout vias U1 meets ride: {met:?}, {rode:?} ride");
        assert!(!rode.is_subset(&met), "{what}: fanout vias U1 does not meet ride too: {rode:?} of {own:?}");
        let names = |id: &ItemId| stretched.collisions.iter().any(|c| [&c.a, &c.b].contains(&&Sel::Item(board.unit, *id)));
        assert!(!rode.iter().any(names), "{what}: the stretch answer names no collision with a riding via: {:?}", stretched.collisions);
        let mut carried = (**board).clone();
        carried.apply_all(&stretched.ops).expect("the stretch answer applies");
        for &via in &rode {
            let at = board.vias.iter().find(|v| v.id == via).expect("a riding via is on the board").at;
            for track in board.tracks.iter().filter(|t| t.ends.contains(&Anchor::Via(via))) {
                let new = carried.tracks.iter().find(|t| t.id == track.id).expect("a riding via's tracks stay on the board");
                if to_part(via).any(|t| t.id == track.id) {
                    assert!(new.path == track.path.translated(by) && new.ends == track.ends, "{what}: track {:?} from U1 to riding via {via:?} is translated whole", track.id);
                } else {
                    let end = if track.ends[0] == Anchor::Via(via) { new.path.start } else { new.path.end() };
                    assert!(new.ends == track.ends && end == at.add(by), "{what}: track {:?} from riding via {via:?} to elsewhere starts where the via goes: {:?}", track.id, new.path);
                }
            }
            assert!(full.ops.contains(&BoardOp::Move { item: via, by }), "{what}: via {via:?} rides in the Full answer too");
        }
        assert!(full.open.is_empty() && !full.ops.iter().any(|op| matches!(op, BoardOp::Remove(_))), "{what}: nothing is cut: {:?}", full.open);
        for v in drum.new_violations(&full.ops) {
            if matches!(v.kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected) { continue; }
            assert!(matches!(v.kind, CheckKind::CourtyardOverlap | CheckKind::SilkOverPad | CheckKind::SilkClipped) && v.items.contains(&Sel::Item(board.unit, part.id)),
                "{what}: DRC finds nothing new on copper, only the moved part's own silk or courtyard: {v:?}");
        }
        assert_eq!(release(&drum, by).1.ops, full.ops, "{what}: the same sample answered again gives the same ops");
        let mut after = (**board).clone();
        let applied = after.apply_all(&full.ops).expect("the answer applies as one transaction");
        after.undo(&applied.inverse).expect("the undo applies");
        assert_eq!(after.content_hash(), board.content_hash(), "{what}: the undo restores the board exactly");
        last = (rode, met);
    }
    // Another net's via where the first riding via of the 0.1 mm drag that U1 does not meet would
    // go, clear of where it is.
    let by = P::new(70_711, 70_711);
    let taken = *last.0.difference(&last.1).next().expect("fixture setup: an unmet via rides");
    let via = board.vias.iter().find(|v| v.id == taken).expect("fixture setup: the via");
    let mut blocked = (**board).clone();
    let other = board.vias.iter().find(|o| o.net != via.net && o.pad == via.pad && o.span == via.span).expect("fixture setup: another net's via");
    let id = blocked.alloc_id();
    let blocker = |at: P| Via { id, at, ..other.clone() };
    let mut carried = World::new(&drum.world);
    carried.remove_item(via.id);
    for &rid in drum.world.pieces(via.id) {
        carried.add_moved(rid, by);
    }
    let meets = |world: &World, at: P| verify(world, &drum.base.rules, board, &new_pieces(board, &Item::Via(blocker(at)))).iter()
        .any(|c| c.b == Sel::Item(board.unit, via.id));
    let at = (1..80).map(|k| via.at.add(by).add(P::new(by.x * k / 4, by.y * k / 4))).find(|&at| !meets(&World::new(&drum.world), at))
        .expect("fixture setup: a place clear of the via that its carried place meets");
    assert!(meets(&carried, at), "fixture setup: the blocking via meets the via's carried place");
    blocked.apply_all(&[BoardOp::Add(Item::Via(blocker(at)))]).expect("fixture setup: the via goes on");
    let (_, full, rode) = release(&Drum::of(blocked), by);
    assert!(!rode.contains(&taken) && !rode.is_empty(), "a via whose carried place is taken stays, and the others still ride: {rode:?}");
    assert!(!full.ops.iter().any(|op| matches!(*op, BoardOp::Move { item, .. } if item == taken)), "the via that stays is not moved in the Full answer either");
}

/// What nothing pushes stays where it is. U1 dragged so that a via the wave took in is laid where
/// it is: its track to another part was laid again anyway and came back tightened. D31 dragged so
/// that a neighbour the wave took in is in nothing's way by the time it is laid: it came back
/// reshaped. U1 dragged so that tracks of its own that the stretch laid make room for another
/// track in the wave, which then goes elsewhere: they came back laid again. In each Full answer, a
/// reshaped track that is neither the part's own nor on a pushed via has base copper that the part
/// meets where it goes (a collision of the stretch answer) or that meets the answer's new copper;
/// and a track of the part's that the stretch laid is as the stretch laid it, unless that copper
/// is not legal on the Full answer's board.
#[test]
fn a_part_drag_on_drum16_moves_nothing_that_nothing_pushes() {
    let drum = Drum::of(Drum::board());
    let board = &drum.base.board;
    for (name, by) in [("U1", P::new(mm(-0.5), mm(0.5))), ("D31", P::new(mm(-1.0), mm(-1.0))), ("U1", P::new(-1_176_232, 984_349))] {
        let what = format!("{name} by {by:?}");
        let part = board.placed.iter().find(|p| p.refdes == name).expect("fixture setup: the part");
        let mut drag = GestureSample {
            id: GestureId(16), seq: 1,
            gesture: Gesture::new(GestureKind::DragParts, board.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU),
        };
        drag.gesture.moved = vec![(part.id, Xform::translate(by))];
        let mut state = GestureState::default();
        let Answer::Proposal(stretched) = answer(&drum.base, &drum.world, &drag, &mut state, ProposalId(1)) else { panic!("{what}: the drag is answered") };
        let (full, _) = shove(&drum.base, &drum.world, &mut state, ProposalId(2)).expect("fixture setup: a wave follows");
        let pushed: Vec<(ItemId, P)> = full.ops.iter().filter_map(|op| match *op {
            BoardOp::Move { item, by } if item != part.id => Some((item, by)),
            _ => None,
        }).collect();
        let reshaped: Vec<&fab_elec_board::Track> = full.ops.iter().filter_map(|op| match op {
            BoardOp::Replace(Item::Track(t)) if board.tracks.iter().any(|b| b.id == t.id && b.path != t.path) => Some(t),
            _ => None,
        }).collect();
        let mut laid = World::new(&drum.world);
        for t in &reshaped {
            laid.remove_item(t.id);
            for piece in new_pieces(board, &Item::Track((*t).clone())) {
                laid.add(piece);
            }
        }
        for &(id, by) in &pushed {
            laid.remove_item(id);
            for &rid in drum.world.pieces(id) {
                laid.add_moved(rid, by);
            }
        }
        let changed = |id: ItemId| reshaped.iter().any(|r| r.id == id) || pushed.iter().any(|(v, _)| *v == id);
        for t in &reshaped {
            let old = board.tracks.iter().find(|b| b.id == t.id).expect("a reshaped track is on the board");
            if old.ends.iter().any(|e| e.item() == Some(part.id) || matches!(*e, Anchor::Via(v) if pushed.iter().any(|(id, _)| *id == v))) {
                continue;
            }
            let met_part = stretched.collisions.iter().any(|c| [&c.a, &c.b].contains(&&Sel::Item(board.unit, t.id)));
            let met_new = verify(&laid, &drum.base.rules, board, &new_pieces(board, &Item::Track(old.clone()))).iter()
                .any(|c| matches!(c.b, Sel::Item(_, id) if id != t.id && changed(id)));
            assert!(met_part || met_new, "{what}: track {:?} is reshaped though nothing is in its way: {:?} from {:?}", t.id, t.path, old.path);
        }
        let applied = |ops: &[BoardOp]| {
            let mut after = (**board).clone();
            after.apply_all(ops).expect("the answer applies");
            after
        };
        let (laid_first, last) = (applied(&stretched.ops), applied(&full.ops));
        let last_world = RBase::build(&last);
        for t in board.tracks.iter().filter(|t| t.ends.iter().any(|e| e.item() == Some(part.id))) {
            // Laid by the stretch, not cut back.
            let Some(first) = laid_first.tracks.iter().find(|k| k.id == t.id && !k.ends.contains(&Anchor::Free)) else { continue };
            let now = last.tracks.iter().find(|k| k.id == t.id).expect("a track the stretch laid stays on the board");
            if now.path == first.path {
                continue;
            }
            let mut others = World::new(&last_world);
            others.remove_item(t.id);
            assert!(!verify(&others, &drum.base.rules, &last, &new_pieces(&last, &Item::Track(first.clone()))).is_empty(),
                "{what}: track {:?} of the part's is laid again though its copper as the stretch laid it is legal on the Full answer's board", t.id);
        }
    }
}

/// Landing 5's guard: F on a half-drawn drum16 track lets the grid finder lay the rest. The answer
/// is the finishing sample's second tier: the hand part's ops first and the router's after them
/// (one proposal, one undo step). The router continues from the hand part's tip, the net is
/// joined, and the check finds nothing new (nothing dangles).
#[test]
fn finish_lays_the_rest_of_a_half_drawn_drum16_track() {
    let drum16 = Drum::board();
    let (drum, removed) = Drum::without(&drum16, "hats.yin", (162.2, 98.59));
    let [start, _] = removed.ends;
    let path = points(&removed.path);
    // The hand part: the removed track's way to the middle of its middle segment.
    let j = (path.len() - 2) / 2;
    let tip = P::new((path[j].x + path[j + 1].x) / 2, (path[j].y + path[j + 1].y) / 2);
    let mut drawn_path = path[..=j].to_vec();
    drawn_path.push(tip);
    let mut state = GestureState::default();
    let mut drawn = sample(&drum.base, start, path[0]);
    drawn.gesture.trail = vec![path[0]];
    drum.drag(drawn.clone(), &drawn_path, Anchor::Free, &mut state);
    // F: the drag's last gesture, finishing, as the next sample.
    let mut finishing = drawn;
    finishing.seq = drawn_path.len() as u64 + 1;
    finishing.gesture.trail = drawn_path.clone();
    finishing.gesture.cursor = tip;
    finishing.gesture.target = Some(Anchor::Free);
    finishing.gesture.finish = true;
    let hand = solve(&drum.base, &drum.world, &finishing, &mut state);
    let head = hand.head.as_ref().expect("a route answer has a head");
    assert_eq!((head.path.end(), head.end, head.blocked), (tip, Anchor::Free, None), "fixture setup: the hand part ends free at its tip");
    assert!(!hand.last, "a finishing sample's stretch answer says its Finish follows");

    let basis = Basis { index: drum.index.clone(), violations: Arc::new(drum.violations.clone()) };
    let finished = finish(&drum.base, &drum.world, &basis, &finishing, &hand, ProposalId(100), &Progress::new());
    assert!(matches!(finished.source, ProposalSource::Gesture { id, seq, tier: Tier::Full } if id == finishing.id && seq == finishing.seq),
        "the Finish is the sample's second tier: {:?}", finished.source);
    assert_eq!(&finished.ops[..hand.ops.len()], &hand.ops[..], "the hand part's ops come first, in the one proposal");
    let routed: Vec<&Item> = finished.ops[hand.ops.len()..].iter().filter_map(|op| match op {
        BoardOp::Add(item) => Some(item),
        _ => None,
    }).collect();
    assert!(!routed.is_empty() && routed.iter().all(|item| matches!(item.origin(), Origin::Solved { .. })),
        "the router adds the rest as solved copper: {routed:?}");
    assert!(routed.iter().any(|item| matches!(item, Item::Track(t) if t.path.start == tip || t.path.end() == tip)),
        "the router continues from the hand part's tip, not beside it: {routed:?}");
    assert!(finished.open.is_empty(), "the router finishes the net: {:?}", finished.open);
    let net = drum.base.board.net("hats.yin").expect("fixture setup: the net exists");
    assert!(drum.base.conn.unrouted.contains(&net), "fixture setup: the net is open before");
    let outcome = drum.checked(&finished.ops);
    assert!(!outcome.conn.unrouted.contains(&net), "the Finish joins the net");
    let new: Vec<_> = outcome.new.iter().map(|&i| outcome.violations[i].kind.clone()).collect();
    assert_eq!(new, Vec::<CheckKind>::new(), "the hand part and the router's copper add no finding: legal, nothing dangling");
    assert_eq!(finished.head.as_ref().and_then(|head| head.blocked), None, "nothing stops the finished copper short");
}
