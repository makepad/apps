//! Segment and corner drags in Walk (E6 3.3, landing 3; the shove answer's slice 4): the grabbed
//! span moves with the cursor and the track is laid again, as one `Replace` of the track; the
//! tracks in its way are pushed. The person shaped it, so the replaced track is `Authored` (a
//! router's track is adopted; a part drag's stretch, in `stretch`, keeps the router's tracks the
//! router's).
//!
//! The reference is the track's base path with the span displaced by the cursor's offset from the
//! press (the straight-line rule: the answer does not depend on how the cursor wandered). Only the
//! window from the fixed vertex before the span to the fixed vertex after it is laid again, by
//! `make_legal`, so the rest of the track keeps its shape and the span stays where the person put
//! it; with no offset the track is its base path (it springs back). The track's ends stay on what
//! they are attached to: grabbing the first or last segment adds a leg from that end to the moved
//! span.
//!
//! The window is laid round the fixed things only: everything but the tracks near it that can be
//! shoved (`stretch::shovable`). Legal as it lies, that is the answer. The shovable tracks its new
//! copper meets are pushed by a shove wave (`shove`), with the drag's new pieces as its movers,
//! each moved by the drag; the wave's Full answer follows this one. A window the fixed things
//! leave no room for has none round everything either: the track stays where it is, saying why.
//! The pushes are all or none: when the window cannot be settled round the fixed things (a shape
//! near it has no hull), meets a track that cannot be shoved, or the wave gives a track up, the
//! drag walks instead. Its window is laid round everything as it is, no track pushed, or the
//! track stays where it is when that is blocked, and the walk's answer names the track it could
//! not push (`HeadAnswer::unpushed`). So a drag commits no short and cuts no copper.
//!
//! Only what is laid again must be lines, and a dragged segment an octilinear one: laying an arc
//! or an any-angle segment through the octilinear embedder would restyle it under the hand, so
//! such a drag is refused as unsupported until the round embedder exists. The rest of the track,
//! arcs and any-angle pad stubs included, is kept as it is.
//!
//! Cost per sample: one query of the tracks near the window, the window's `make_legal` round the
//! fixed things (its legs' hulls, cached per base generation) and one `hits` of its new copper;
//! when it walks, a second `make_legal` round everything and one `verify` of the new track. A
//! wave's steps follow on the worker as a part drag's do.

use super::obstacles::{grow, HullCache, WorldObstacles};
use super::shove::{self, by_item, Hit, Kind, Member, Movers, Walk, Wave};
use super::{stretch, tile_size, GestureState};
use crate::embed::band::{make_legal, BandInput};
use crate::gesture::{GestureKind, GestureSample};
use crate::proposal::{BlockCause, Blocked, Collision, HeadAnswer, ProposalId, ProposalSource, RouteProposal, Tier};
use crate::request::Base;
use crate::solution::{Reason, RouteMetrics};
use crate::verify::{hits, new_pieces, sorted, verify};
use crate::world::{RBase, RPiece, WRef, World};
use fab_elec_board::{Anchor, Board, BoardOp, ConnItem, Item, ItemKind, Origin, RuleMatrix, Track};
use fab_elec_core::{BBox, ClassId, ItemId, NetId, Nm, Path, Seg, P};
use std::ops::Range;
use std::time::Instant;

/// Why a drag is not answered: the board lacks what it names (`None` from `answer`), or it asks
/// for what this tier does not do (the canvas text).
pub(super) enum Refusal {
    Invalid,
    Unsupported(&'static str),
}

/// The answer to a segment or corner drag sample, on `base`.
pub(super) fn answer(
    base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState, id: ProposalId, started: Instant,
    queries: &mut usize,
) -> Result<RouteProposal, Refusal> {
    let gesture = &sample.gesture;
    let board = &base.board;
    let (track_id, lo, hi) = match gesture.kind {
        GestureKind::DragSeg { track, seg } => (track, seg as usize, seg as usize + 1),
        GestureKind::DragCorner { track, vertex } => (track, vertex as usize, vertex as usize),
        _ => return Err(Refusal::Invalid),
    };
    // What was grabbed is still where it was: the press is on that segment (or vertex) of the track.
    let on = match gesture.start {
        Anchor::Track { id, at } if id == track_id => match gesture.kind {
            GestureKind::DragSeg { seg, .. } => at.seg == seg,
            _ => true,
        },
        _ => false,
    };
    if !on || board.anchor_pos(gesture.start) != Some(gesture.start_at) {
        return Err(Refusal::Invalid);
    }
    let Some((ItemKind::Track, i)) = board.locate(track_id) else { return Err(Refusal::Invalid) };
    let track = &board.tracks[i];
    if track.locked {
        return Err(Refusal::Unsupported("this track is locked"));
    }
    let segs = &track.path.segs;
    let n = segs.len();
    if hi > n {
        return Err(Refusal::Invalid);
    }
    if matches!(gesture.kind, GestureKind::DragCorner { .. }) && (lo == 0 || hi == n) {
        return Err(Refusal::Unsupported("a track's end stays on what it is attached to"));
    }
    let vertex = |k: usize| if k == 0 { track.path.start } else { segs[k - 1].end() };
    // The window: the fixed vertex before the span to the fixed vertex after it. A span on an end
    // keeps the end and adds a leg from it.
    let (from, to) = (lo.saturating_sub(1), (hi + 1).min(n));
    if segs[from..to].iter().any(|seg| !matches!(seg, Seg::Line(_))) {
        return Err(Refusal::Unsupported("an arc is dragged once the round embedder exists"));
    }
    if let GestureKind::DragSeg { seg, .. } = gesture.kind {
        let (a, b) = (vertex(seg as usize), vertex(seg as usize + 1));
        let (dx, dy) = ((b.x - a.x).abs(), (b.y - a.y).abs());
        if dx != 0 && dy != 0 && dx != dy {
            return Err(Refusal::Unsupported("only a 45-degree segment can be dragged yet"));
        }
    }
    let by = gesture.cursor.sub(gesture.start_at);
    state.newest = Some((board.generation, sample.seq));
    let source = ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Stretch };
    // Back at the press: the track as it is.
    if by == P::new(0, 0) {
        return Ok(proposal(base, source, id, started, (Vec::new(), shown(track, None, None), Vec::new()), true));
    }

    let mut window = vec![vertex(from)];
    window.extend((lo..=hi).map(|k| vertex(k).add(by)));
    window.push(vertex(to));
    let class = board.nets.get(track.net.ix()).map(|net| net.class).ok_or(Refusal::Invalid)?;
    let drag = Dragged { track: track.clone(), class, window, from, to };
    let blocked = |blocked| proposal(base, source, id, started, (Vec::new(), shown(track, Some(blocked), None), Vec::new()), true);
    match push(base, world, sample, state, id, started, queries, &drag, by) {
        Ok(Some(pushed)) => return Ok(pushed),
        Err(why) => return Ok(blocked(why)),
        Ok(None) => {}
    }
    let mut over = World::new(world);
    over.remove_item(track.id);
    Ok(match walk(&over, board, &base.rules, &mut state.hulls, queries, &drag) {
        Ok((moved, collisions)) => {
            let head = shown(&moved, None, None);
            proposal(base, source, id, started, (vec![BoardOp::Replace(Item::Track(moved))], head, collisions), true)
        }
        // A drag that cannot be laid leaves the track where it is, saying why.
        Err(why) => blocked(why),
    })
}

/// A dragged track: as it was, its net's class, and its window (from the fixed vertex `from` to
/// the fixed vertex `to`) as the reference to lay again, the span moved by the cursor's offset.
#[derive(Clone)]
pub(super) struct Dragged {
    pub(super) track: Track,
    class: ClassId,
    window: Vec<P>,
    from: usize,
    to: usize,
}

impl Dragged {
    /// The track with the window's legs `points` in place of its window segments, the segments
    /// before and after kept, adopted (a person shaped it); and which of its segments are new.
    fn moved(&self, points: &[P]) -> (Track, Range<usize>) {
        let track = &self.track;
        let segs = &track.path.segs;
        let mut laid = Path { start: track.path.start, segs: segs[..self.from].to_vec() };
        let mut last = self.window[0];
        for &p in points.iter().skip(1) {
            if p != last {
                laid.segs.push(Seg::Line(p));
                last = p;
            }
        }
        let fresh = self.from..laid.segs.len();
        laid.segs.extend_from_slice(&segs[self.to..]);
        (Track { path: laid, origin: Origin::Authored, ..track.clone() }, fresh)
    }
}

/// The drag's window laid round everything in `over` by `make_legal`: its points, or where and
/// why it is blocked.
fn lay(over: &World, rules: &RuleMatrix, hulls: &mut HullCache, queries: &mut usize, drag: &Dragged) -> Result<Vec<P>, Blocked> {
    let track = &drag.track;
    let window = &drag.window;
    let input = BandInput { s: window[0], e: *window.last().expect("a window has ends"), reference: window, tile: tile_size(track.w, rules) };
    let mut obstacles = WorldObstacles::new(over, rules, track.layer, track.net, drag.class, track.w, track.id, hulls);
    let band = make_legal(&input, &mut obstacles);
    *queries += band.stats.queries;
    if obstacles.failed() {
        return Err(Blocked { at: input.s, cause: BlockCause::Unsettled, have: 0, need: 0 });
    }
    match band.blocked {
        Some(blocked) => Err(blocked),
        None => Ok(band.points),
    }
}

/// The walk: the dragged track with its window laid again round everything in `over` (the world
/// without the track), and what the track then meets (nothing when it is legal); or where and why
/// it is blocked. O(the window's `make_legal`, plus one `verify` of the track).
pub(super) fn walk(
    over: &World, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, queries: &mut usize, drag: &Dragged,
) -> Result<(Track, Vec<Collision>), Blocked> {
    let points = lay(over, rules, hulls, queries, drag)?;
    let (moved, _) = drag.moved(&points);
    let collisions = verify(over, rules, board, &new_pieces(board, &Item::Track(moved.clone())));
    Ok((moved, collisions))
}

/// The drag as a pusher (the shove answer's slice 4): its window laid round the fixed things only
/// (everything but the shovable tracks near it), so the span stays where the person puts it. The
/// shovable tracks its new copper then meets are pushed by a wave, whose Full answer follows this
/// one; legal as it lies, it is the answer. Where and why the window is blocked when the fixed
/// things leave it no room (round everything they are in its way too); `None` when it cannot be
/// settled round them or meets a track that cannot be pushed: the drag walks round everything
/// instead.
fn push(
    base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState, id: ProposalId, started: Instant,
    queries: &mut usize, drag: &Dragged, by: P,
) -> Result<Option<RouteProposal>, Blocked> {
    let board = &base.board;
    let rules = &base.rules;
    let track = &drag.track;
    let mut fixed = World::new(world);
    fixed.remove_item(track.id);
    // The shovable tracks near the window leave the world it is laid in: the runs of straight
    // 45-degree lines round their segments there.
    let area = grow(BBox::of_points(drag.window.iter().copied()), track.w as i64 + rules.max_clearance() as i64);
    let mut near: Vec<(ItemId, usize)> = fixed.query_sorted(track.layer, area).into_iter().filter_map(|(reference, it)| {
        let (WRef::Base(_), RPiece::Copper(c)) = (reference, &it.piece) else { return None };
        let ConnItem::Track { item, seg } = c.item else { return None };
        Some((item, seg as usize))
    }).collect();
    near.sort_unstable();
    near.dedup();
    for (other, segs) in by_item(&near) {
        let Some(other) = board.tracks.iter().find(|t| t.id == other) else { continue };
        if let Some(windows) = stretch::shovable(world, other, &segs) {
            stretch::take_out(&mut fixed, world, other.id, |seg| windows.iter().any(|w| w.contains(&seg)));
        }
    }
    shove::free_vias(&mut fixed, world, board, track.layer, area, |t| t == track.id);
    let points = match lay(&fixed, rules, &mut state.hulls, queries, drag) {
        Ok(points) => points,
        Err(Blocked { cause: BlockCause::Unsettled, .. }) => return Ok(None),
        Err(blocked) => return Err(blocked),
    };
    let (moved, fresh) = drag.moved(&points);
    let pieces = stretch::fresh_pieces(board, &moved, &fresh.collect::<Vec<usize>>());

    // What its new copper meets: tracks' and vias' base copper only, as it is legal against the
    // rest.
    let mut over = World::new(world);
    over.remove_item(track.id);
    let met = hits(&over, rules, board, &pieces);
    let (mut violated, mut met_vias) = (Vec::with_capacity(met.len()), Vec::new());
    for (_, reference) in &met {
        match Hit::of(world, *reference) {
            Some(Hit::Track(item, seg)) => violated.push((item, seg)),
            Some(Hit::Via(via)) => met_vias.push(via),
            None => return Ok(None),
        }
    }
    violated.sort_unstable();
    violated.dedup();
    met_vias.sort_unstable();
    met_vias.dedup();
    let collisions = sorted(met.into_iter().map(|(collision, _)| collision).collect());
    let source = ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Stretch };
    let ops = vec![BoardOp::Replace(Item::Track(moved.clone()))];
    if violated.is_empty() && met_vias.is_empty() {
        return Ok(Some(proposal(base, source, id, started, (ops, shown(&moved, None, None), collisions), true)));
    }

    // The wave: each via met with the tracks it carries, and each track met, its windows round
    // the segments met, in order of first contact along the drag; the dragged track's new pieces
    // are the movers, each moved by the drag.
    let travel = vec![by; pieces.len()];
    let mut members = Vec::new();
    let mut carried: Vec<ItemId> = Vec::new();
    for id in met_vias {
        let Some(via) = board.vias.iter().find(|v| v.id == id) else { return Ok(None) };
        let Some(tracks) = shove::pushable(board, world, via, |t| t == track.id || carried.contains(&t)) else { return Ok(None) };
        carried.extend(tracks.iter().map(|t| t.id));
        members.extend(shove::via_members(board, via, tracks, stretch::contact_at(&pieces, &travel, via.at)));
    }
    for (other, segs) in by_item(&violated) {
        if carried.contains(&other) {
            continue;
        }
        let Some(other) = board.tracks.iter().find(|t| t.id == other) else { return Ok(None) };
        let Some(windows) = stretch::shovable(world, other, &segs) else { return Ok(None) };
        let contact = stretch::contact(&pieces, &travel, other, &windows);
        members.push(Member::new(board, Kind::Neighbour { track: other.clone(), windows }, contact));
    }
    let mut start = World::new(world);
    stretch::take_out(&mut start, world, track.id, |seg| (drag.from..drag.to).contains(&seg));
    for piece in &pieces {
        start.add(piece.clone());
    }
    for member in &members {
        shove::take_out_member(&mut start, world, member);
    }
    let head = shown(&moved, None, None);
    state.wave = Some(Box::new(Wave::new(
        board.generation, (sample.id, sample.seq), started, ops.clone(), collisions.clone(), Movers::Moved(travel), start.into_overlay(), members,
        Vec::new(), Some(Walk::drag(drag.clone(), moved)),
    )));
    Ok(Some(proposal(base, source, id, started, (ops, head, collisions), false)))
}

/// The head of a drag's answer: the track shown, why it stays where it is (`blocked`), and the
/// track it could not push.
pub(super) fn shown(track: &Track, blocked: Option<Blocked>, unpushed: Option<(NetId, Reason)>) -> HeadAnswer {
    HeadAnswer {
        path: track.path.clone(), layer: track.layer, w: track.w, end: track.ends[1],
        length: track.path.length().round() as Nm, blocked, unpushed,
    }
}

/// A drag's proposal from `source`: its ops, head and collisions; `last` when no wave follows.
pub(super) fn proposal(
    base: &Base, source: ProposalSource, id: ProposalId, started: Instant, (ops, head, collisions): (Vec<BoardOp>, HeadAnswer, Vec<Collision>),
    last: bool,
) -> RouteProposal {
    RouteProposal {
        pair: None,
        id, source, unit: base.unit, base_generation: base.board.generation, base_hash: base.hash,
        metrics: RouteMetrics { length: head.length as i64, ..RouteMetrics::default() },
        ops, checked: None, verified: collisions.is_empty(), collisions, open: Vec::new(), head: Some(head),
        solve_us: started.elapsed().as_micros().min(u32::MAX as u128) as u32,
        last, label: String::new(),
    }
}
