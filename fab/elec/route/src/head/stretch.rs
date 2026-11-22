//! Part and via drags in Stretch (E6 3.3, landing 4): the moved items go where the person puts
//! them, and each track attached to them is laid again from its base path with its moved end.
//!
//! One answer, one op list, for the preview and the commit alike: the board's `Move` of each moved
//! item first (it moves the pads and drags anchored track ends straight), then a `Replace` of each
//! attached track the stretch laid again. The moved items are never refused: where a part goes is
//! the person's call, and what it then meets is named in `collisions`, which DRC lists after the
//! commit.
//!
//! A track the stretch cannot lay legally is cut back (E6 3.3; the shove answer's S5): a short
//! merges two nets and reads as finished copper, an open shows as a ratsnest line. It keeps its
//! base path from its unmoved end up to the last point legal against everything laid before it
//! (segments that are still legal stay exactly where they are, the first one that is not is cut at
//! the exact nm where it stops being legal), its cut end free, one `Replace`; with nothing kept,
//! or both its ends moved, it is removed. Its net is in `open`, with the collision that cut it
//! (`Reason::NoRoom`). So no attached track commits a short; a moved item still may (on a fixed
//! thing, or on a neighbour's track: that is shove). The segments an attached track keeps are
//! checked against the moved items too, under the fault rule (a fault they had may stay, no
//! worse): a track the moved items come too close to there is cut back, not laid.
//!
//! A moved part's own fanout vias ride with it (E6 3.2, `ride`): a via one of whose tracks runs to
//! a moved part moves by that part's translation, from the start of the drag and whether a pad
//! meets it or not, when `shove::pushable` lets it move and all its tracks to moved items move by
//! that one translation. Its tracks whose other end moves the same way (its stubs) are translated
//! whole, one `Replace` each; its other tracks (its far tracks) are attached tracks with the via
//! end moved, laid before the rest. A via whose place with its stubs meets anything in the world
//! with what moves taken out, or one of whose far tracks cannot be laid, does not ride, and the
//! stretch is as if it never had; nor do the vias its copper back then meets. A via that does not
//! ride is answered as before: it stays, what meets it is named, its stub is stretched or cut, and
//! the wave may push it. The answer has the riding vias' `Move`s after the moved items' own, then
//! the stubs' `Replace`s.
//!
//! A dependent track stays anchored where its host's new path still passes its end. When the
//! host leaves that point, an untouched track that can be laid again follows to the nearest
//! point on the host. Its other end stays in place. A failed lay cuts the dependent back from
//! that other end. A track already laid from a moved pad or via follows from its laid path.
//! If that junction window cannot be laid, the earlier copper stays and its end comes free.
//! A follower's own dependents get the same treatment. A removed host, or a dependent that
//! cannot follow, leaves the junction freed. The answer names each cut and each surviving end
//! that came off its host in `open`. The board may attach a free end to other copper on commit.
//! DRC reports connectivity.
//!
//! From each moved end, the track is laid again (its window) by `make_legal` round everything else,
//! with the base path from that end as the reference: the end's own segment (stretched, at any
//! angle) and the octilinear lines after it, up to the first segment that is not one (an arc, an
//! any-angle stub at the far pad), which keeps its shape with the rest. Legs of the reference that
//! are legal stay exactly where they were, so only what the move puts in the way changes. Each run
//! is laid from the moved end straight to the vertex after it, keeping the rest of the window.
//! On a uniform-width track, the first run tried is the shortest whose rest meets none of the
//! copper that moved or was laid. The rest is judged as the base has it (as laid for a follower),
//! including any spur `unspur` would cut there, so the covering run lays that stretch. A tapered
//! track's widths follow its vertex count, so its rest is not judged ahead. Runs are tried in
//! order of length. The first whose sharpest turn is no sharper than 45 degrees (or than the base
//! path's) is taken. Otherwise the whole window is laid too, and the run that turns least sharply
//! is taken. So a pad moved past its stub's first corner does not leave the track doubling back to
//! that corner. A line that turns straight back along itself is cut short (`unspur`). A moved end
//! in an obstacle's clearance lays nothing: the track keeps its straight stretch. A moved end on
//! an arc keeps its straight stretch until the round embedder exists.
//! Tracks are laid in board order (the riding vias' far tracks first, by via), each against the
//! moved items, the tracks laid before it and the rest of every attached track. Neighbours never
//! move out of the way (that is shove).
//!
//! Cost per sample: the moved items' pieces translated and verified, then per attached track the
//! `make_legal` of its runs (one when the moved end's own segment is enough, at most one per window
//! segment and one more along the window's own vertices; its hulls of base items cached per base generation) and one `verify` of its window.
//! At the start of a growing uniform-width window, each rest segment is checked once from the far
//! end to the first dirty one. A segment keeping its base shape is checked against added copper
//! only. Other rest segments are checked against the whole world.
//! A sharp corner between two new segments costs up to eight `meets` queries.
//! A cut adds one `verify` per changed segment kept and one per halving of the segment it cuts (its
//! length in nm, about 22 for a few mm). The attached tracks, and the tracks anchored on a changed
//! one, come from the world's index of track ends, so nothing in an answer scans the board. Only
//! a junction the new copper would leave behind needs a follower lay: one `lay_one` per followed
//! end. Riding vias: see `ride` (an admission check per via, their far tracks laid once each, and
//! per via that leaves only the checks and lays its copper can change).

use super::obstacles::{HullCache, WorldObstacles};
use super::ride;
use super::shove::{self, by_item, Hit, Kind, Member, Movers, Wave};
use super::{tile_size, GestureState};
use crate::embed::band::{make_legal_path, BandInput};
use crate::gesture::{GestureKind, GestureSample};
use crate::proposal::{BlockCause, Collision, ProposalId, ProposalSource, RouteProposal, Tier};
use crate::request::Base;
use crate::solution::{Reason, RouteMetrics};
use crate::verify::{hits_ruled, meets, meets_added, new_pieces, sorted, track_piece, track_pieces, verify, Near};
use crate::world::{RBase, RId, RItem, RPiece, WRef, World};
use fab_elec_board::ops::on_track;
use fab_elec_board::{Anchor, Board, BoardOp, ConnItem, Item, ItemKind, RuleMatrix, Track};
use fab_elec_circuit::Measure;
use fab_elec_core::{Angle, ClassId, ItemId, NetId, Path, Seg, Sel, P};
use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Instant;

use super::drag::Refusal;


/// The answer to a part or via drag sample, on `base`.
pub(super) fn answer(
    base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState, id: ProposalId, started: Instant,
    queries: &mut usize,
) -> Result<RouteProposal, Refusal> {
    let gesture = &sample.gesture;
    let board = &base.board;
    let rules = &base.rules;
    if let GestureKind::DragVia(via) = gesture.kind {
        if !gesture.moved.iter().any(|(item, _)| *item == via) {
            return Err(Refusal::Invalid);
        }
    }
    let mut moved: Vec<(ItemId, P)> = Vec::with_capacity(gesture.moved.len());
    for &(item, xform) in &gesture.moved {
        if !matches!(board.locate(item), Some((ItemKind::Placed | ItemKind::Via, _))) {
            return Err(Refusal::Invalid);
        }
        if xform.rot != Angle::ZERO {
            return Err(Refusal::Unsupported("turning a part while it is dragged is answered later"));
        }
        if xform.by != P::ZERO {
            moved.push((item, xform.by));
        }
    }
    state.newest = Some((board.generation, sample.seq));
    // The moved parts' own fanout vias ride with them where they can (`ride`): what moves, the
    // attached tracks with the riding vias' far tracks first (laid), and what leaves the world.
    let ride::Settled { moving, stubs, attached, far, out, pieces, by_piece, movers, part } =
        ride::settle(board, rules, world, &moved, &mut state.fanout, &mut state.hulls, queries)?;
    let mut over = World::new(world);
    let mut collisions = Vec::new();
    let mut ops: Vec<BoardOp> = moving.iter().map(|&(item, by)| BoardOp::Move { item, by }).collect();
    ops.extend(stubs.iter().map(|&(track, by)| BoardOp::Replace(Item::Track(Track { path: track.path.translated(by), ..track.clone() }))));
    let mut attached_ids: Vec<ItemId> = attached.iter().map(|(track, ..)| track.id).collect();
    attached_ids.sort_unstable();
    let is_attached = |id: ItemId| attached_ids.binary_search(&id).is_ok();

    // The moved items at their new places, verified against everything they now meet; the
    // neighbours' track segments they meet. A riding via with its stubs meets nothing there.
    over.remove_many(out);
    // The moved items name everything they meet, a fault they had before too (as new copper);
    // but an attached track's kept segments may keep a fault they had, no worse (the fault rule),
    // and one the moved items meet otherwise cannot stay: that track is cut back (`lay_one`).
    let met = hits_ruled(&over, rules, board, &pieces[..part], |it| it.item.is_some_and(is_attached));
    let (mut violated, mut met_vias): (Vec<(ItemId, usize)>, Vec<ItemId>) = (Vec::new(), Vec::new());
    let mut clash: Vec<(ItemId, usize, &Collision)> = Vec::new();
    for (collision, reference) in &met {
        match Hit::of(world, *reference) {
            Some(Hit::Track(item, seg)) if !is_attached(item) => violated.push((item, seg)),
            Some(Hit::Track(item, seg)) => clash.push((item, seg, collision)),
            Some(Hit::Via(via)) => met_vias.push(via),
            _ => {}
        }
    }
    // Each attached track the moved items meet: why it is cut, and the kept segments they meet.
    clash.sort_by_key(|&(item, seg, _)| (item, seg));
    let mut clashes: Vec<(ItemId, Reason, Vec<usize>)> = Vec::new();
    for (item, seg, collision) in clash {
        match clashes.last_mut() {
            Some((last, _, segs)) if *last == item => if segs.last() != Some(&seg) { segs.push(seg) },
            _ => clashes.push((item, no_room(collision), vec![seg])),
        }
    }
    violated.sort_unstable();
    violated.dedup();
    met_vias.sort_unstable();
    met_vias.dedup();
    collisions.extend(sorted(met.into_iter().map(|(collision, _)| collision).collect()));
    for (&rid, &by) in movers.iter().zip(&by_piece) {
        over.add_moved(rid, by);
    }
    // What the wave starts from, before the lays: the moves, and every attached track's windows
    // out (the lays then add the tracks laid clean).
    let before = over.clone();

    // Each attached track: its windows laid again, else its straight stretch, else cut back. The
    // riding vias' far tracks come first, as `ride` laid them (one that cannot be laid makes its via
    // leave instead); a far track whose kept segments the moved items meet is cut back too.
    let mut open = Vec::new();
    // Whether each attached track was cut back.
    let mut is_cut = Vec::with_capacity(attached.len());
    // What the wave starts from: the moves and the tracks laid clean, with their pieces.
    let mut clean_ops = ops.clone();
    let mut clean = Vec::new();
    // A track cut because the moved items meet the segments it keeps stays cut: the wave lays
    // only an attached track's windows, and would keep those segments where they are. Its cut
    // goes into the wave as given up: what it keeps, and why.
    let clashed = |id: ItemId| clashes.binary_search_by_key(&id, |(item, ..)| *item).is_ok();
    let mut cut_for_good: Vec<(&Track, [Option<P>; 2], Option<Track>, Reason)> = Vec::new();
    let mut far = far.into_iter();
    for attached in &attached {
        let (track, ends, _) = attached;
        let met = clashes.binary_search_by_key(&track.id, |(item, ..)| *item).ok().map(|k| (clashes[k].1.clone(), &clashes[k].2[..]));
        let laid = match (far.next(), met) {
            (Some((laid, pieces)), None) => {
                for piece in &pieces {
                    over.add(piece.clone());
                }
                Laid::Track(laid, pieces)
            }
            (_, met) => lay_one(&mut over, board, rules, &mut state.hulls, queries, attached, met)?,
        };
        match laid {
            Laid::Track(moved_track, laid) => {
                if moved_track.path != straight(&track.path, *ends) {
                    ops.push(BoardOp::Replace(Item::Track(moved_track.clone())));
                    clean_ops.push(BoardOp::Replace(Item::Track(moved_track)));
                }
                clean.push(laid);
                is_cut.push(false);
            }
            Laid::Cut { kept, why } => {
                if clashed(track.id) {
                    cut_for_good.push((track, *ends, kept.clone(), why.clone()));
                }
                open.push((track.net, why));
                ops.push(match &kept {
                    Some(kept) => BoardOp::Replace(Item::Track(kept.clone())),
                    None => BoardOp::Remove(track.id),
                });
                is_cut.push(true);
            }
        }
    }
    let (cuts, unjoined) = keep_junctions(base, &mut over, &mut state.hulls, queries, &mut ops);
    open.extend(cuts);
    open.extend(unjoined);
    open.extend(sharp_corners(board, &ops));

    // The shove wave, when there is anything to lay again: the cut tracks (but those cut for
    // good), and the vias (with the tracks they carry) and neighbours the moved items meet. Its
    // Full answer follows this one.
    let mut members: Vec<Member> = attached.iter().zip(&is_cut).filter(|((track, ..), &cut)| cut && !clashed(track.id))
        .map(|((track, ends, windows), _)| Member::new(board, Kind::Attached { track: (*track).clone(), ends: *ends, windows: windows.clone(), back: None }, 0))
        .collect();
    let mut carried: Vec<ItemId> = Vec::new();
    for id in met_vias {
        let Some((ItemKind::Via, i)) = board.locate(id) else { continue };
        let via = &board.vias[i];
        let taken = |t: ItemId| is_attached(t) || carried.contains(&t);
        if let Some(tracks) = shove::pushable(board, world, via, taken) {
            carried.extend(tracks.iter().map(|t| t.id));
            members.extend(shove::via_members(board, via, tracks, contact_at(&pieces, &by_piece, via.at)));
        }
    }
    for (id, segs) in by_item(&violated) {
        let Some((ItemKind::Track, i)) = board.locate(id).filter(|_| !carried.contains(&id)) else { continue };
        let track = &board.tracks[i];
        if let Some(windows) = shovable(world, track, &segs) {
            let contact = contact(&pieces, &by_piece, track, &windows);
            members.push(Member::new(board, Kind::Neighbour { track: track.clone(), windows }, contact));
        }
    }
    let waving = !members.is_empty();
    if waving {
        let mut start = before;
        for piece in clean.into_iter().flatten() {
            start.add(piece);
        }
        for (track, ends, kept, _) in &cut_for_good {
            start.remove_item(track.id);
            for piece in kept.iter().flat_map(|kept| cut_pieces(world, board, track, *ends, kept)) {
                start.add(piece);
            }
        }
        for member in &members {
            shove::take_out_member(&mut start, world, member);
        }
        let mut laid: Vec<(ItemId, [Option<P>; 2])> = attached.iter().zip(&is_cut).filter(|(_, &cut)| !cut)
            .map(|((track, ends, _), _)| (track.id, *ends)).collect();
        laid.sort_unstable_by_key(|(id, _)| *id);
        state.wave = Some(Box::new(Wave::new(
            board.generation, (sample.id, sample.seq), started, clean_ops, collisions.clone(), Movers::Moved(by_piece), start.into_overlay(), members, laid, None,
        ).with_cut(board, cut_for_good.into_iter().map(|(track, _, kept, why)| (track.clone(), kept, why)))));
    }
    Ok(RouteProposal {
        id, source: ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Stretch },
        unit: base.unit, base_generation: board.generation, base_hash: base.hash,
        metrics: RouteMetrics::default(), ops, checked: None, verified: collisions.is_empty(), collisions,
        open, head: None, solve_us: started.elapsed().as_micros().min(u32::MAX as u128) as u32,
        last: !waving, label: "Move".into(),
    })
}

/// The base pieces of track `id` whose segment is `inside` taken out of `over`. O(pieces).
pub(super) fn take_out(over: &mut World, world: &RBase, id: ItemId, inside: impl Fn(usize) -> bool) {
    let mut out = Vec::new();
    pieces_inside(world, id, inside, &mut out);
    for rid in out {
        over.remove(rid);
    }
}

/// The base pieces of track `id` whose segment is `inside`, added to `out`. O(pieces).
pub(super) fn pieces_inside(world: &RBase, id: ItemId, inside: impl Fn(usize) -> bool, out: &mut Vec<RId>) {
    for &rid in world.pieces(id) {
        let Some(it) = world.get(rid) else { continue };
        if matches!(&it.piece, RPiece::Copper(c) if matches!(c.item, ConnItem::Track { seg, .. } if inside(seg as usize))) {
            out.push(rid);
        }
    }
}

/// Whether this board may lay the track again: it has its own statement, one width and no lock.
fn relayable(track: &Track) -> bool {
    track.origin.has_statement() && !track.locked && track.widths.is_none()
}

/// The windows of a neighbour's track whose segments `segs` a moved item meets, when the wave may
/// lay it again: each the run of straight 45-degree lines round a met segment. `None` when it
/// stays where it is: locked, a block's or another unit's, with widths per segment, met on an arc
/// or a line at another angle, or with another track's end on it (its anchors would not follow).
/// O(segments), and a lookup in the world's index of track ends.
pub(super) fn shovable(world: &RBase, track: &Track, segs: &[usize]) -> Option<Vec<Range<usize>>> {
    if !relayable(track) {
        return None;
    }
    if !world.tracks_on(track.id).is_empty() {
        return None;
    }
    let n = track.path.segs.len();
    let line = |k: usize| octilinear(vertex(&track.path, k), &track.path.segs[k]);
    let mut windows: Vec<Range<usize>> = Vec::new();
    for &seg in segs {
        if seg >= n || !line(seg) {
            return None;
        }
        if windows.last().is_some_and(|w| w.contains(&seg)) {
            continue;
        }
        let (mut lo, mut hi) = (seg, seg + 1);
        while lo > 0 && line(lo - 1) {
            lo -= 1;
        }
        while hi < n && line(hi) {
            hi += 1;
        }
        windows.push(lo..hi);
    }
    Some(windows)
}

/// First contact (the shove answer's S4): the least squared distance along a moved piece's travel,
/// from its base centre, at which that ray meets one of the track's `windows`; `i128::MAX` when it
/// meets none. O(moved pieces * window segments).
pub(super) fn contact(pieces: &[RItem], by_piece: &[P], track: &Track, windows: &[Range<usize>]) -> i128 {
    use fab_elec_core::geom::orient;
    let mut best = i128::MAX;
    for (piece, &by) in pieces.iter().zip(by_piece) {
        let c0 = piece.bbox.center().sub(by);
        let ray = c0.add(by);
        for k in windows.iter().flat_map(|w| w.clone()) {
            let (a, b) = (vertex(&track.path, k), vertex(&track.path, k + 1));
            let (sa, sb) = (orient(c0, ray, a), orient(c0, ray, b));
            if sa > 0 && sb > 0 || sa < 0 && sb < 0 || sa == 0 && sb == 0 {
                continue;
            }
            let (oa, ob) = (orient(a, b, c0), orient(a, b, ray));
            if oa == ob {
                continue;
            }
            let t = oa as f64 / (oa - ob) as f64;
            if t >= 0.0 {
                best = best.min((t * t * (by.x as f64 * by.x as f64 + by.y as f64 * by.y as f64)) as i128);
            }
        }
    }
    best
}

/// First contact with a via at `at`: the squared distance along the travel of the moved piece whose
/// travel passes nearest it to the point of that travel nearest it. O(moved pieces).
pub(super) fn contact_at(pieces: &[RItem], by_piece: &[P], at: P) -> i128 {
    let mut best = (f64::MAX, i128::MAX);
    for (piece, &by) in pieces.iter().zip(by_piece) {
        let c0 = piece.bbox.center().sub(by);
        let (bx, by) = (by.x as f64, by.y as f64);
        let (dx, dy) = (at.x as f64 - c0.x as f64, at.y as f64 - c0.y as f64);
        let len2 = bx * bx + by * by;
        let t = if len2 == 0.0 { 0.0 } else { ((dx * bx + dy * by) / len2).clamp(0.0, 1.0) };
        let off = (dx - t * bx).hypot(dy - t * by);
        if off < best.0 {
            best = (off, (t * t * len2) as i128);
        }
    }
    best.1
}

/// What became of an attached track: laid again (or its straight stretch), legal; or cut back to
/// `kept` (`None`: removed) because of the collision `why`.
enum Laid {
    Track(Track, Vec<RItem>),
    Cut { kept: Option<Track>, why: Reason },
}

/// One attached track laid against `over`, its new pieces verified and added when they are legal;
/// otherwise cut back, and what is kept added. A track whose kept segments the moved items meet
/// (`met`: why, and those segments) is cut back without a lay, up to the first of them that is not
/// legal. O(`relay`), plus `cut_back` when it cuts.
fn lay_one(
    over: &mut World, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, queries: &mut usize,
    (track, ends, windows): &(&Track, [Option<P>; 2], Vec<Window>), met: Option<(Reason, &[usize])>,
) -> Result<Laid, Refusal> {
    let (why, met) = match met {
        Some(met) => met,
        None => {
            let (laid, _, pieces, hit) = lay(over, board, rules, hulls, queries, (track, *ends, windows))?;
            let Some(hit) = hit else {
                for piece in &pieces {
                    over.add(piece.clone());
                }
                return Ok(Laid::Track(laid, pieces));
            };
            (no_room(&hit), &[][..])
        }
    };
    let kept = cut_back(over, board, rules, track, *ends, windows, met);
    over.remove_item(track.id);
    let base = over.base;
    for piece in kept.iter().flat_map(|kept| cut_pieces(base, board, track, *ends, kept)) {
        over.add(piece);
    }
    Ok(Laid::Cut { kept, why })
}

/// `track` laid again against `over` (`relay`): the track, its new segments and their pieces, and
/// the first collision of those pieces, if any. O(`relay`) and one `verify` of its new pieces.
fn lay(
    over: &World, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, queries: &mut usize,
    (track, ends, windows): (&Track, [Option<P>; 2], &[Window]),
) -> Result<(Track, Vec<usize>, Vec<RItem>, Option<Collision>), Refusal> {
    let class = board.nets.get(track.net.ix()).map(|net| net.class).ok_or(Refusal::Invalid)?;
    let (path, fresh) = relay(over, board, rules, hulls, queries, (track, ends, windows), class);
    let laid = Track { path, ..track.clone() };
    let mut pieces = fresh_pieces(board, &laid, &fresh);
    mark_kept(over.base, track.id, &mut pieces);
    let hit = verify(over, rules, board, &pieces).into_iter().next();
    Ok((laid, fresh, pieces, hit))
}

/// `track` laid again against `over` as `lay_one` lays it, when that is legal: the track, its new
/// segments and their pieces; `None` where `lay_one` would cut it back. `over` is not changed.
pub(super) fn lay_clean(
    over: &World, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, queries: &mut usize,
    attached: (&Track, [Option<P>; 2], &[Window]),
) -> Result<Option<(Track, Vec<usize>, Vec<RItem>)>, Refusal> {
    let (laid, fresh, pieces, hit) = lay(over, board, rules, hulls, queries, attached)?;
    Ok(hit.is_none().then_some((laid, fresh, pieces)))
}

/// The open reason a collision gives a cut track.
pub(super) fn no_room(hit: &Collision) -> Reason {
    Reason::NoRoom {
        at: hit.at, layer: hit.layer, need: Measure::Len(hit.need), have: Measure::Len(hit.gap.max(0)), between: [hit.a.clone(), hit.b.clone()], by: Vec::new(),
    }
}

/// `track` cut back from its one unmoved end: its base path from that end, the segments outside
/// its windows as they are (but those in `met`, which something moved meets: those are checked)
/// and each changed one while it is legal against `over`, then the first
/// that is not up to the last nm where it is (a line; an arc is not kept), the cut end free. `None`
/// when nothing is kept: both ends moved, or not even the first changed segment's start is legal.
/// O(changed segments + log of the cut segment's length) `verify` calls of one piece each.
pub(super) fn cut_back(over: &World, board: &Board, rules: &RuleMatrix, track: &Track, ends: [Option<P>; 2], windows: &[Window], met: &[usize]) -> Option<Track> {
    let forward = match ends {
        [None, Some(_)] => true,
        [Some(_), None] => false,
        _ => return None,
    };
    let segs = &track.path.segs;
    let n = segs.len();
    let at = |k: usize| vertex(&track.path, k);
    let mut pieces = new_pieces(board, &Item::Track(track.clone()));
    mark_kept(over.base, track.id, &mut pieces);
    let legal = |piece: &RItem| verify(over, rules, board, std::slice::from_ref(piece)).is_empty();
    // A line of the track from `a` to `p` along segment `k`, as one piece: the piece a one-line
    // track of it would be (its first segment's width does not depend on its path), keeping
    // segment `k`'s base piece (it lies along it).
    let probe = |k: usize, a: P, p: P| RItem { kept: base_segment(over.base, track.id, k).map(|rank| (rank, P::ZERO)), ..track_piece(board, track, 0, a, &Seg::Line(p)) };
    // Whole segments kept from the unmoved end, and the cut segment's last legal point.
    let mut whole = 0;
    let mut tip = None;
    for k in (0..n).map(|i| if forward { i } else { n - 1 - i }) {
        if !changed(track, ends, windows, k) && !met.contains(&k) || legal(&pieces[k]) {
            whole += 1;
            continue;
        }
        if matches!(segs[k], Seg::Line(_)) {
            let (a, b) = if forward { (at(k), at(k + 1)) } else { (at(k + 1), at(k)) };
            // Every probe lies within the whole line's box: one query for all of them.
            let near = Near::gather(over, rules, track.layer, probe(k, a, b).bbox);
            tip = slide(a, b, |p| {
                let piece = probe(k, a, p);
                near.clear(rules, board, &piece).unwrap_or_else(|| legal(&piece))
            });
        }
        break;
    }
    let path = if forward {
        Path { start: track.path.start, segs: segs[..whole].iter().copied().chain(tip.map(Seg::Line)).collect() }
    } else {
        let from = n - whole;
        match tip {
            Some(tip) => Path { start: tip, segs: std::iter::once(Seg::Line(at(from))).chain(segs[from..].iter().copied()).collect() },
            None => Path { start: at(from), segs: segs[from..].to_vec() },
        }
    };
    if path.segs.is_empty() {
        return None;
    }
    let ends = if forward { [track.ends[0], Anchor::Free] } else { [Anchor::Free, track.ends[1]] };
    Some(Track { path, ends, ..track.clone() })
}

/// The farthest point from `a` toward `b` (`b` itself not legal) up to which a line from `a` is
/// `legal`, on the nm steps of the line; `None` when no step is. Exact, because a shorter line's
/// copper lies inside a longer one's: legality only fails once along the way. O(log length) calls.
fn slide(a: P, b: P, legal: impl Fn(P) -> bool) -> Option<P> {
    let (dx, dy) = (b.x as i64 - a.x as i64, b.y as i64 - a.y as i64);
    let steps = dx.abs().max(dy.abs());
    let point = |m: i64| {
        let along = |d: i64| (d as i128 * m as i128 / steps as i128) as i64;
        P::new((a.x as i64 + along(dx)) as _, (a.y as i64 + along(dy)) as _)
    };
    let (mut lo, mut hi) = (0, steps);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if legal(point(mid)) { lo = mid } else { hi = mid }
    }
    (lo > 0).then(|| point(lo))
}

/// Keep the junctions on changed tracks. A host's path comes from its replacement or its
/// straight stretch. Visit the first pending host in board order, including followers whose
/// copper has just been laid or cut. Return the cuts and the surviving ends that lost a host.
pub(super) fn keep_junctions(
    base: &Base, over: &mut World, hulls: &mut HullCache, queries: &mut usize, ops: &mut Vec<BoardOp>,
) -> (Vec<(NetId, Reason)>, Vec<(NetId, Reason)>) {
    let board = &base.board;
    let world = over.base;
    let moves: Vec<(ItemId, P)> = ops.iter().filter_map(|op| match op {
        BoardOp::Move { item, by } => Some((*item, *by)),
        _ => None,
    }).collect();
    let moved_end = |end: Anchor| {
        let item = end.item()?;
        let (_, by) = moves.iter().find(|(m, _)| *m == item)?;
        Some(board.anchor_pos(end)?.add(*by))
    };
    let changed = ops.iter().filter_map(|op| match op {
        BoardOp::Replace(Item::Track(t)) => Some(t.id),
        BoardOp::Remove(id) => Some(*id),
        _ => None,
    }).chain(moves.iter().flat_map(|&(item, _)| world.tracks_on(item).iter().copied()));
    // A follower may already have been visited as a straight-stretched host. Its dependents'
    // replacements then hold their current junction points on that earlier path.
    let mut hosts: BTreeMap<usize, bool> = changed.filter(|&id| !world.tracks_on(id).is_empty())
        .filter_map(|id| match board.locate(id) { Some((ItemKind::Track, i)) => Some((i, false)), _ => None }).collect();
    let mut cuts = Vec::new();
    let mut unjoined = Vec::new();
    while let Some((i, followed)) = hosts.pop_first() {
        let host = &board.tracks[i];
        let path = if ops.contains(&BoardOp::Remove(host.id)) {
            None
        } else {
            let replaced = ops.iter().find_map(|op| match op {
                BoardOp::Replace(Item::Track(t)) if t.id == host.id => Some(t.path.clone()),
                _ => None,
            });
            let path = replaced.or_else(|| {
                let ends = host.ends.map(&moved_end);
                (ends != [None, None]).then(|| straight(&host.path, ends))
            });
            let Some(path) = path.filter(|path| followed || *path != host.path) else { continue };
            Some(path)
        };
        reanchor(base, over, hulls, queries, (host, path.as_ref(), followed), &moved_end, ops, &mut hosts, &mut cuts, &mut unjoined);
    }
    (cuts, unjoined)
}

/// Reanchor the ends on `track` in board order. A dependent may follow once. The
/// follower's copper goes into `over` before the next lay, and its dependents go on the worklist.
fn reanchor(
    base: &Base, over: &mut World, hulls: &mut HullCache, queries: &mut usize,
    (track, kept, followed): (&Track, Option<&Path>, bool), moved_end: &impl Fn(Anchor) -> Option<P>,
    ops: &mut Vec<BoardOp>, hosts: &mut BTreeMap<usize, bool>, cuts: &mut Vec<(NetId, Reason)>, unjoined: &mut Vec<(NetId, Reason)>,
) {
    let board = &base.board;
    let world = over.base;
    let mut on: Vec<usize> = world.tracks_on(track.id).iter()
        .filter_map(|&id| match board.locate(id) { Some((ItemKind::Track, i)) => Some(i), _ => None }).collect();
    on.sort_unstable();
    for i in on {
        let other = &board.tracks[i];
        if ops.iter().any(|op| *op == BoardOp::Remove(other.id)) {
            continue;
        }
        let mut replaced = ops.iter_mut().find_map(|op| match op {
            BoardOp::Replace(Item::Track(t)) if t.id == other.id => Some(t),
            _ => None,
        });
        let ends = replaced.as_deref().map_or(other.ends, |t| t.ends);
        let mut reanchored = ends;
        let mut lost = [None; 2];
        let mut follow = None;
        for (e, end) in ends.into_iter().enumerate() {
            let Anchor::Track { id, at } = end else { continue };
            if id != track.id { continue; }
            let p = match (followed, replaced.as_deref()) {
                (true, Some(t)) => if e == 0 { t.path.start } else { t.path.end() },
                _ => track.path.point_on(at),
            };
            if let Some(path) = kept {
                let (on, q) = path.locate(p);
                let anchor = Anchor::Track { id, at: on };
                if q == p {
                    reanchored[e] = anchor;
                    continue;
                }
                let dep = replaced.as_deref().unwrap_or(other);
                if follow.is_none() && relayable(dep) && !dep.path.segs.is_empty() {
                    let can_follow = replaced.as_deref().is_none_or(|t| {
                        let at_base = if e == 0 { t.path.start == other.path.start } else { t.path.end() == other.path.end() };
                        at_base && moved_end(t.ends[1 - e]).is_some() && t.path != other.path
                    });
                    // The anchor a reader gives the new end (`on_track`), so the answer reads back
                    // from its text as it is; no follow where the point is not exactly on the host.
                    let point = path.point_on(on);
                    let at = on_track(point, path).filter(|&at| path.point_on(at) == point);
                    if let (true, Some(at)) = (can_follow, at) { follow = Some((e, point, Anchor::Track { id, at })); }
                }
            }
            lost[e] = Some(if followed { board.anchor_pos(other.ends[e]).unwrap_or(p) } else { p });
            reanchored[e] = Anchor::Free;
        }
        let mut follower_op = None;
        if let (Some((e, point, anchor)), Some(dep)) = (follow, replaced.as_deref_mut()) {
            if let Some(path) = follow_laid(base, over, hulls, queries, other, dep, e, point) {
                dep.path = path;
                reanchored[e] = anchor;
                lost[e] = None;
                if !world.tracks_on(other.id).is_empty() { hosts.insert(i, true); }
            }
        } else if let Some((e, point, anchor)) = follow {
            // A Move can already have stretched the other end with no Replace. Start from the
            // path in the answer so the lay and the board's snap use the same far end.
            let stretched;
            let dep = if let Some(far) = moved_end(other.ends[1 - e]) {
                let mut prior = [None; 2];
                prior[1 - e] = Some(far);
                stretched = Track { path: straight(&other.path, prior), ..other.clone() };
                &stretched
            } else {
                other
            };
            let mut moved = [None; 2];
            moved[e] = Some(point);
            let windows = windows(dep, moved);
            let mut out = Vec::new();
            let changes = |seg| changed(dep, moved, &windows, seg);
            pieces_inside(world, dep.id, changes, &mut out);
            // A wave can keep a neighbour in place with its base windows already in the overlay.
            // Hide any earlier lay of these windows too. A cut replaces the whole track.
            let mut old_windows = Vec::new();
            let mut kept_added = Vec::new();
            for piece in over.remove_added(other.id) {
                if matches!(&piece.piece, RPiece::Copper(c) if matches!(c.item, ConnItem::Track { seg, .. } if changes(seg as usize))) {
                    old_windows.push(piece);
                } else {
                    kept_added.push(over.add(piece));
                }
            }
            out.retain(|&rid| over.get(WRef::Base(rid)).is_some());
            over.remove_many(out.iter().copied());
            match lay_one(over, board, &base.rules, hulls, queries, &(dep, moved, windows), None) {
                Ok(Laid::Track(mut laid, _)) => {
                    lost[e] = None;
                    reanchored[e] = anchor;
                    laid.ends = reanchored;
                    follower_op = Some(BoardOp::Replace(Item::Track(laid)));
                }
                Ok(Laid::Cut { kept, why }) => {
                    for r in kept_added { over.hide(r); }
                    cuts.push((other.net, why));
                    follower_op = Some(match kept {
                        Some(mut kept) => {
                            kept.ends = reanchored;
                            BoardOp::Replace(Item::Track(kept))
                        }
                        None => {
                            // A removed host visited earlier may already have named this track.
                            lost = [None; 2];
                            unjoined.retain(|(_, why)| !matches!(why, Reason::Unjoined { track: Sel::Item(_, id), .. } if *id == other.id));
                            BoardOp::Remove(other.id)
                        }
                    });
                }
                // An invalid net has no class to lay with. Restore the pieces that were present
                // and leave the end free, as for a dependent that could not follow.
                Err(_) => {
                    over.restore_many(out);
                    for piece in old_windows { over.add(piece); }
                }
            }
        }
        if let Some(op) = follower_op {
            ops.push(op);
            if !world.tracks_on(other.id).is_empty() { hosts.insert(i, true); }
        } else if kept.is_some() && reanchored != ends {
            match replaced {
                Some(t) => t.ends = reanchored,
                None => ops.push(BoardOp::Replace(Item::Track(Track { ends: reanchored, ..other.clone() }))),
            }
        }
        unjoined.extend(lost.into_iter().flatten().map(|at| (other.net, Reason::Unjoined {
            at, track: Sel::Item(board.unit, other.id), host: Sel::Item(board.unit, track.id),
        })));
    }
}

/// Try the junction end of a track the answer already laid. Base copper still names the base
/// path's segments, while added copper names the laid path's. Restore the window on a failed lay.
fn follow_laid(
    base: &Base, over: &mut World, hulls: &mut HullCache, queries: &mut usize, original: &Track, dep: &Track, end: usize, point: P,
) -> Option<Path> {
    let mut moved = [None; 2];
    moved[end] = Some(point);
    let windows = windows(dep, moved);
    let changes = |seg| changed(dep, moved, &windows, seg);
    let geometry: std::collections::HashSet<(P, Seg)> = dep.path.steps().enumerate()
        .filter_map(|(k, step)| changes(k).then_some(step)).collect();
    let mut removed = Vec::new();
    let in_window = |seg| geometry.contains(&(vertex(&original.path, seg), original.path.segs[seg]));
    pieces_inside(over.base, dep.id, in_window, &mut removed);
    removed.retain(|&rid| over.get(WRef::Base(rid)).is_some());
    over.remove_many(removed.iter().copied());
    let mut old_window = Vec::new();
    for piece in over.remove_added(dep.id) {
        if matches!(&piece.piece, RPiece::Copper(c) if matches!(c.item, ConnItem::Track { seg, .. } if changes(seg as usize))) {
            old_window.push(piece);
        } else {
            over.add(piece);
        }
    }
    if let Ok(Some((laid, _, pieces))) = lay_clean(over, &base.board, &base.rules, hulls, queries, (dep, moved, &windows)) {
        for piece in pieces { over.add(piece); }
        Some(laid.path)
    } else {
        over.restore_many(removed);
        for piece in old_window { over.add(piece); }
        None
    }
}

/// `track` laid again against `over` with its ends at `ends`: its path (its straight stretch when
/// its windows cannot be laid), and the segments that are new (the rest is in the world already).
/// See the module docs for the runs tried; `Relay` takes them one at a time.
pub(super) fn relay(
    over: &World, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, queries: &mut usize,
    (track, ends, windows): (&Track, [Option<P>; 2], &[Window]), class: ClassId,
) -> (Path, Vec<usize>) {
    let mut relay = Relay::new(windows);
    loop {
        if let Some(laid) = relay.step(over, board, rules, hulls, queries, (track, ends, windows), class) {
            return laid;
        }
    }
}

/// A relay (`relay`) in progress, one band run (`make_legal`) per `step`, so a caller can do
/// other work between them: the window it is on, the next run of it to try, the best run so far,
/// and what each window before it laid (`None` once one cannot be laid).
#[derive(Clone)]
pub(super) struct Relay {
    window: usize,
    /// How many segments to cover from the moved end on the next run of a growing window.
    /// Starts at one, advanced to cover the farthest dirty segment on a uniform-width track.
    /// At the window's length the whole window is laid.
    k: usize,
    best: Option<(f64, Vec<Seg>)>,
    laid: Option<Vec<Vec<Seg>>>,
    /// Runs laid so far.
    runs: usize,
}

impl Relay {
    pub(super) fn new(windows: &[Window]) -> Relay {
        Relay { window: 0, k: 1, best: None, laid: (!windows.is_empty()).then(Vec::new), runs: 0 }
    }

    /// Whether no run has been laid yet.
    pub(super) fn untried(&self) -> bool {
        self.runs == 0
    }

    /// Try the next run: `None` while runs are left, else the path and its new segments. The
    /// same `over`, track, ends and windows each time.
    pub(super) fn step(
        &mut self, over: &World, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, queries: &mut usize,
        (track, ends, windows): (&Track, [Option<P>; 2], &[Window]), class: ClassId,
    ) -> Option<(Path, Vec<usize>)> {
        self.runs += 1;
        let straight = &straight(&track.path, ends);
        // A reference laid round everything else: its legs, or where it is blocked when it is.
        let mut lay = |reference: &[P], hulls: &mut HullCache| -> Result<Vec<Seg>, Option<P>> {
            let input = BandInput { s: reference[0], e: reference[reference.len() - 1], reference, tile: tile_size(track.w, rules) };
            let mut obstacles = WorldObstacles::new(over, rules, track.layer, track.net, class, track.w, track.id, hulls);
            obstacles.gather(reference);
            let band = make_legal_path(&input, &mut obstacles);
            *queries += band.stats.queries;
            match (&band.blocked, obstacles.failed()) {
                (None, false) => Ok(band.points.into_iter().skip(1).map(Seg::Line).collect()),
                (Some(blocked), false) if blocked.cause == BlockCause::End => Err(Some(blocked.at)),
                _ => Err(None),
            }
        };
        // Take the first run whose turns are good enough, including where it meets kept copper.
        // A moved end in an obstacle's clearance rules out every run from it.
        if let (Some(_), Some(window)) = (&self.laid, windows.get(self.window)) {
            let len = window.segs.len();
            if self.k == 1 && self.best.is_none() && window.grows && len > 1 && track.widths.is_none() {
                // Run k covers the kth segment from the moved end. Start with the farthest dirty one.
                for k in (2..=len).rev() {
                    let s = if window.at_start { window.segs.start + k - 1 } else { window.segs.end - k };
                    let mut piece = [track_piece(board, track, s, vertex(straight, s), &straight.segs[s])];
                    mark_kept(over.base, track.id, &mut piece);
                    let dirty = if piece[0].kept.is_some() {
                        meets_added(over, rules, board, &piece)
                    } else {
                        meets(over, rules, board, &piece)
                    };
                    if dirty {
                        self.k = k;
                        break;
                    }
                }
            }
            let moved_end = if window.at_start { window.reference[0] } else { window.reference[len] };
            let start = if window.at_start { window.reference[0] } else { vertex(straight, window.segs.start) };
            // A segment outside every window comes from the straight stretch in the assembled path.
            let outside = |k: usize| !windows.iter().any(|w| w.segs.contains(&k));
            let before = !window.at_start && window.segs.start > 0 && outside(window.segs.start - 1);
            let after = window.at_start && window.segs.end < straight.segs.len() && outside(window.segs.end);
            let window_score = |path: &Path, start: P, segs: &[Seg]| {
                let mut pts = Vec::with_capacity(segs.len() + 2);
                if before { pts.push(vertex(path, window.segs.start - 1)); }
                pts.push(start);
                pts.extend(segs.iter().map(Seg::end));
                if after { pts.push(path.segs[window.segs.end].end()); }
                sharpest(&pts)
            };
            let base = window_score(&track.path, vertex(&track.path, window.segs.start), &track.path.segs[window.segs.clone()]);
            // Taken at once: a run whose sharpest turn is no sharper than 45 degrees or the base's.
            let good = base.min(std::f64::consts::FRAC_1_SQRT_2) - 1e-9;
            let mut stuck = false;
            let decided = if window.grows && len > 1 && self.k <= len {
                // Lay the k segments at the moved end with a straight reference and keep the rest.
                // At k = len the reference joins the window's ends and the rest is empty.
                let k = self.k;
                let (reference, rest) = if window.at_start {
                    (vec![window.reference[0], window.reference[k]], window.segs.start + k..window.segs.end)
                } else {
                    (vec![window.reference[len - k], window.reference[len]], window.segs.start..window.segs.end - k)
                };
                self.k += 1;
                match lay(&reference, hulls) {
                    Ok(segs) => {
                        let rest = rest.map(|k| straight.segs[k]);
                        let segs = unspur(start, if window.at_start { segs.into_iter().chain(rest).collect() } else { rest.chain(segs).collect() });
                        let score = window_score(straight, start, &segs);
                        if self.best.as_ref().is_none_or(|(b, _)| score > *b) {
                            self.best = Some((score, segs));
                        }
                        score >= good
                    }
                    Err(at) => {
                        stuck = at == Some(moved_end);
                        stuck
                    }
                }
            } else {
                // Try the original window vertices when no straight run was good enough.
                // Keep the candidate that turns least sharply.
                if self.best.as_ref().is_none_or(|(b, _)| *b < good) {
                    if let Ok(segs) = lay(&window.reference, hulls) {
                        let segs = unspur(start, segs);
                        let score = window_score(straight, start, &segs);
                        if self.best.as_ref().is_none_or(|(b, _)| score > *b) {
                            self.best = Some((score, segs));
                        }
                    }
                }
                true
            };
            if decided {
                match self.best.take().filter(|_| !stuck) {
                    Some((_, segs)) => self.laid.as_mut().expect("a window is laid only while all before it were").push(segs),
                    None => self.laid = None,
                }
                self.window += 1;
                self.k = 1;
            }
            if self.laid.is_some() && self.window < windows.len() {
                return None;
            }
        }
        // The path, and which of its segments are new (the rest is in the world already).
        let (mut path, mut fresh): (Path, Vec<usize>) = match self.laid.take() {
            Some(laid) => {
                let mut path = Path { start: straight.start, segs: Vec::with_capacity(track.path.segs.len() + 4) };
                let mut fresh = Vec::new();
                let mut laid = windows.iter().zip(laid).peekable();
                let mut k = 0;
                while k < track.path.segs.len() {
                    match laid.next_if(|(w, _)| w.segs.start == k) {
                        Some((w, segs)) => {
                            fresh.extend(path.segs.len()..path.segs.len() + segs.len());
                            path.segs.extend(segs);
                            k = w.segs.end;
                        }
                        None => {
                            // A moved end outside every window (an arc) is the straight stretch's.
                            if changed(track, ends, &[], k) {
                                fresh.push(path.segs.len());
                            }
                            path.segs.push(straight.segs[k]);
                            k += 1;
                        }
                    }
                }
                (path, fresh)
            }
            None => (straight.clone(), (0..straight.segs.len()).filter(|&k| changed(track, ends, windows, k)).collect()),
        };
        chamfer(over, board, rules, track, &mut path, &mut fresh);
        Some((path, fresh))
    }
}

/// Sharp line-to-line corners a track op lays at a point where its base track had none.
/// Visits the changed tracks' segments in linear time and reports in op and path order.
pub(super) fn sharp_corners(board: &Board, ops: &[BoardOp]) -> Vec<(NetId, Reason)> {
    fn corners(path: &Path) -> impl Iterator<Item = P> + '_ {
        path.segs.windows(2).enumerate().filter_map(move |(i, pair)| {
            let [Seg::Line(at), Seg::Line(to)] = pair else { return None };
            let from = vertex(path, i);
            // The dot has the sign of `turn`. Widen before subtracting across the board.
            let (ax, ay) = (at.x as f64 - from.x as f64, at.y as f64 - from.y as f64);
            let (bx, by) = (to.x as f64 - at.x as f64, to.y as f64 - at.y as f64);
            (ax * bx + ay * by < 0.0).then_some(*at)
        })
    }

    let mut found = Vec::new();
    for op in ops {
        let (BoardOp::Replace(Item::Track(t)) | BoardOp::Add(Item::Track(t))) = op else { continue };
        let mut laid = corners(&t.path).peekable();
        if laid.peek().is_none() { continue; }
        // An added track normally has no base item. Avoid locate's full-board scan on a miss.
        let base = match op {
            BoardOp::Add(_) if board.index.get(t.id).is_none() => None,
            _ => board.locate(t.id),
        };
        let old: std::collections::HashSet<P> = match base {
            Some((ItemKind::Track, i)) => corners(&board.tracks[i].path).collect(),
            _ => Default::default(),
        };
        for at in laid.filter(|p| !old.contains(p)) {
            found.push((t.net, Reason::SharpCorner { at, layer: t.layer, track: Sel::Item(board.unit, t.id) }));
        }
    }
    found
}

/// Chamfer sharp corners between two fresh lines only, since a kept leg is still in the world
/// at its full length. Check the shortened legs too because they can lose their exact base shape.
fn chamfer(over: &World, board: &Board, rules: &RuleMatrix, track: &Track, path: &mut Path, fresh: &mut Vec<usize>) {
    if track.widths.is_some() { return; }
    let mut points = Vec::new();
    let mut pieces = Vec::new();
    let mut i = 0;
    while i + 1 < fresh.len() {
        let left = fresh[i];
        i += 1;
        let k = fresh[i];
        if k != left + 1 { continue; }
        let (Seg::Line(b), Seg::Line(c)) = (path.segs[left], path.segs[k]) else { continue };
        let a = vertex(path, left);
        let (px, py) = (b.x as f64 - a.x as f64, b.y as f64 - a.y as f64);
        let (nx, ny) = (c.x as f64 - b.x as f64, c.y as f64 - b.y as f64);
        let delta = |from: P, to: P| Some(P::new(to.x.checked_sub(from.x)?, to.y.checked_sub(from.y)?));
        let acute = match (delta(a, b), delta(b, c)) {
            (Some(prev), Some(next)) => turn(prev, next) < 0.0,
            // Endpoints fit a P even when their difference does not. The dot gives the turn's sign.
            _ => px * nx + py * ny < 0.0,
        };
        if !acute { continue; }
        let (lp, ln) = (px.hypot(py), nx.hypot(ny));
        let prev_fraction = if left == 0 { 0.9 } else { 0.45 };
        let next_fraction = if k + 1 == path.segs.len() { 0.9 } else { 0.45 };
        let tmax = 600_000.0_f64.min(lp * prev_fraction).min(ln * next_fraction);
        if tmax < 10_000.0 { continue; }
        let d0 = (px / lp, py / lp);
        let d1 = (nx / ln, ny / ln);
        let theta = (d0.0 * d1.1 - d0.1 * d1.0).atan2(d0.0 * d1.0 + d0.1 * d1.1);
        let first = (((theta.abs().to_degrees() - 1e-3) / 45.0).ceil() as usize).max(2);
        'counts: for turns in std::iter::once(first).chain((first != 2).then_some(2)) {
            let offsets = fillet_offsets(d0, d1, theta, turns);
            for scale in [1.0, 0.6, 0.35, 0.2] {
                let tl = tmax * scale;
                if tl < 10_000.0 { break; }
                points.clear();
                points.push(a);
                // Round offsets from the corner so an axis or diagonal leg keeps its direction.
                points.extend(offsets.iter().map(|&(x, y)| {
                    b.add(P::new((tl * x).round() as i32, (tl * y).round() as i32))
                }));
                points.push(c);
                if points.windows(2).any(|w| w[0] == w[1]) { continue; }
                pieces.clear();
                pieces.extend(points.windows(2).enumerate().map(|(j, w)| {
                    track_piece(board, track, left + j, w[0], &Seg::Line(w[1]))
                }));
                if meets(over, rules, board, &pieces) { continue; }

                let fillet = &points[1..points.len() - 1];
                let inserted = fillet.len() - 1;
                path.segs.splice(left..k, fillet.iter().copied().map(Seg::Line));
                for seg in &mut fresh[i..] { *seg += inserted; }
                fresh.splice(i..i, k..k + inserted);
                i += inserted; // Continue from the shortened outgoing leg.
                break 'counts;
            }
        }
    }
}

/// The organic fillet's offsets from the corner for a unit trim along each leg.
fn fillet_offsets(d0: (f64, f64), d1: (f64, f64), theta: f64, turns: usize) -> Vec<(f64, f64)> {
    let mut p = (-d0.0, -d0.1);
    let mut offsets = Vec::with_capacity(turns);
    offsets.push(p);
    if turns > 2 {
        let phi = theta / turns as f64;
        let dirs: Vec<_> = (1..turns).map(|m| {
            let (sin, cos) = (phi * m as f64).sin_cos();
            (d0.0 * cos - d0.1 * sin, d0.0 * sin + d0.1 * cos)
        }).collect();
        let sum = dirs.iter().fold((0.0, 0.0), |(x, y), &(dx, dy)| (x + dx, y + dy));
        let length = sum.0.hypot(sum.1);
        if length >= 1e-6 {
            let step = (d1.0 - p.0).hypot(d1.1 - p.1) / length;
            for &(dx, dy) in &dirs[..dirs.len() - 1] {
                p = (p.0 + dx * step, p.1 + dy * step);
                offsets.push(p);
            }
        }
    }
    offsets.push(d1);
    offsets
}

/// `pieces`, of track `id` laid again, each marked as keeping the base piece of the track whose
/// shape it has (`RItem::kept`, moved by zero): a segment kept where it was, or a leg laid back
/// where it was. O(pieces × the track's base pieces).
pub(super) fn mark_kept(base: &RBase, id: ItemId, pieces: &mut [RItem]) {
    let rids = base.pieces(id);
    for piece in pieces.iter_mut() {
        let RPiece::Copper(c) = &piece.piece else { continue };
        let same = |it: &&RItem| matches!(&it.piece, RPiece::Copper(b) if b.layer == c.layer && b.shape == c.shape);
        piece.kept = rids.iter().filter_map(|&rid| base.get(rid)).find(same).map(|it| (it.rank, P::ZERO));
    }
}

/// The rank of the base piece of segment `k` of track `id`.
fn base_segment(base: &RBase, id: ItemId, k: usize) -> Option<u32> {
    base.pieces(id).iter().filter_map(|&rid| base.get(rid))
        .find(|it| matches!(&it.piece, RPiece::Copper(c) if c.item == ConnItem::Track { item: id, seg: k as u32 })).map(|it| it.rank)
}

/// The pieces of `kept`, what `cut_back` kept of `track` with its ends at `ends`: each whole
/// segment keeps its base piece, and so does the cut tip, along the base segment it was cut from
/// (the last segment from the start, or the first from the end). O(segments × base pieces).
pub(super) fn cut_pieces(base: &RBase, board: &Board, track: &Track, ends: [Option<P>; 2], kept: &Track) -> Vec<RItem> {
    let mut pieces = new_pieces(board, &Item::Track(kept.clone()));
    mark_kept(base, track.id, &mut pieces);
    let (n, m) = (track.path.segs.len(), kept.path.segs.len());
    let tip = match ends {
        [None, Some(_)] => pieces.last_mut().map(|piece| (piece, m - 1)),
        [Some(_), None] => pieces.first_mut().map(|piece| (piece, n - m)),
        _ => None,
    };
    if let Some((piece, k)) = tip.filter(|(piece, _)| piece.kept.is_none()) {
        piece.kept = base_segment(base, track.id, k).map(|rank| (rank, P::ZERO));
    }
    pieces
}

/// The pieces of `track` that its segments `fresh` make. O(segments x fresh).
pub(super) fn fresh_pieces(board: &Board, track: &Track, fresh: &[usize]) -> Vec<RItem> {
    track_pieces(board, track, |seg| fresh.contains(&seg))
}

/// A run of a track's segments laid again, and its reference: the run's points with the moved end
/// in place, at the window's start (`at_start`) or its end. When it `grows`, shorter runs from the
/// moved end are tried first (not when the window reaches the track's other moved end).
#[derive(Clone)]
pub(super) struct Window {
    pub(super) segs: Range<usize>,
    pub(super) reference: Vec<P>,
    pub(super) at_start: bool,
    grows: bool,
}

/// Vertex `k` of `path`: its start, then each segment's end. O(1).
pub(super) fn vertex(path: &Path, k: usize) -> P {
    if k == 0 { path.start } else { path.segs[k - 1].end() }
}

/// `segs` from `start` without the spurs of a run laid through a vertex the moved end passed: a
/// line that turns straight back along itself is cut short where its neighbour ends. The cut line
/// lies on copper that was laid there, so it stays legal. O(segments).
fn unspur(start: P, segs: Vec<Seg>) -> Vec<Seg> {
    let mut points = vec![start];
    for seg in segs {
        let p = seg.end();
        if points.last() == Some(&p) {
            continue;
        }
        while points.len() >= 2 {
            let (a, b) = (points[points.len() - 2], points[points.len() - 1]);
            let (u, v) = (b.sub(a), p.sub(b));
            let cross = u.x as i128 * v.y as i128 - u.y as i128 * v.x as i128;
            let dot = u.x as i128 * v.x as i128 + u.y as i128 * v.y as i128;
            if cross != 0 || dot >= 0 {
                break;
            }
            points.pop();
        }
        if points.last() != Some(&p) {
            points.push(p);
        }
    }
    points.into_iter().skip(1).map(Seg::Line).collect()
}

/// `start` and the ends of `segs`. O(segments).
pub(super) fn points(start: P, segs: &[Seg]) -> Vec<P> {
    std::iter::once(start).chain(segs.iter().map(|seg| seg.end())).collect()
}

/// The sharpest turn along `points`, as `turn` measures it (1 with no turn). O(points).
pub(super) fn sharpest(points: &[P]) -> f64 {
    points.windows(3).map(|w| turn(w[1].sub(w[0]), w[2].sub(w[1]))).fold(1.0, f64::min)
}

/// How straight on the turn from direction `a` to direction `b` goes: the cosine of the angle
/// between them, 1 straight on, -1 doubling back; 1 when either has no length. O(1).
fn turn(a: P, b: P) -> f64 {
    let (ax, ay, bx, by) = (a.x as f64, a.y as f64, b.x as f64, b.y as f64);
    let n = (ax * ax + ay * ay).sqrt() * (bx * bx + by * by).sqrt();
    if n == 0.0 { 1.0 } else { (ax * bx + ay * by) / n }
}

/// Whether segment `seg` of `track` changes with its ends at `ends`: it is in a window, or it is a
/// moved end's own.
pub(super) fn changed(track: &Track, ends: [Option<P>; 2], windows: &[Window], seg: usize) -> bool {
    ends[0].is_some() && seg == 0 || ends[1].is_some() && seg + 1 == track.path.segs.len() || windows.iter().any(|w| w.segs.contains(&seg))
}

/// Whether `seg`, starting at `a`, is a horizontal, vertical or diagonal line.
pub(super) fn octilinear(a: P, seg: &Seg) -> bool {
    let Seg::Line(b) = *seg else { return false };
    let (dx, dy) = ((b.x as i64 - a.x as i64).abs(), (b.y as i64 - a.y as i64).abs());
    dx == 0 || dy == 0 || dx == dy
}

/// The windows of `track` with its ends moved to `ends`: from each moved end, its segment (a line
/// at any angle: it is the one stretched) and every octilinear line after it, up to the other
/// window or a segment that is not. A moved end on an arc has no window (its straight stretch
/// stays). Both windows are one when they meet. O(segments).
pub(super) fn windows(track: &Track, ends: [Option<P>; 2]) -> Vec<Window> {
    let segs = &track.path.segs;
    let n = segs.len();
    let vertex = |k: usize| if k == 0 { track.path.start } else { segs[k - 1].end() };
    let mut out = Vec::new();
    let mut taken = 0;
    if let Some(p) = ends[0].filter(|_| matches!(segs[0], Seg::Line(_))) {
        let mut hi = 1;
        while hi < n && (octilinear(vertex(hi), &segs[hi]) || hi + 1 == n && ends[1].is_some() && matches!(segs[hi], Seg::Line(_))) {
            hi += 1;
        }
        let mut reference: Vec<P> = std::iter::once(p).chain((1..=hi).map(vertex)).collect();
        if hi == n {
            if let Some(q) = ends[1] {
                *reference.last_mut().expect("a window has an end") = q;
            }
        }
        out.push(Window { segs: 0..hi, reference, at_start: true, grows: !(hi == n && ends[1].is_some()) });
        taken = hi;
    }
    if let Some(q) = ends[1].filter(|_| taken < n && matches!(segs[n - 1], Seg::Line(_))) {
        let mut lo = n - 1;
        while lo > taken && octilinear(vertex(lo - 1), &segs[lo - 1]) {
            lo -= 1;
        }
        let reference = (lo..n).map(vertex).chain(std::iter::once(q)).collect();
        out.push(Window { segs: lo..n, reference, at_start: false, grows: true });
    }
    out
}

/// What the board's `Move` makes of `path` with its ends at `ends`: each moved end dragged straight.
pub(super) fn straight(path: &Path, ends: [Option<P>; 2]) -> Path {
    let mut path = path.clone();
    if let Some(p) = ends[0] {
        path.start = p;
    }
    if let Some(p) = ends[1] {
        match path.segs.last_mut() {
            Some(Seg::Line(e)) | Some(Seg::Arc { end: e, .. }) => *e = p,
            None => path.start = p,
        }
    }
    path
}

