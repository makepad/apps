//! The route head's Shove mode (E6 3.1 and 3.2; the shove answer's slice 5). The legs and vias
//! are as the person fixed them; the head is laid round the fixed things only: the world of the
//! legs without the tracks near the head's trail that can be shoved (`stretch::shovable`). Legal
//! as it lies, that is the answer. The tracks the route's copper meets (legs, vias and head: legs
//! fixed in Shove mode lie over the tracks they pushed, which only the drop commits) are pushed by
//! a shove wave (`shove`) whose Full answer follows this one, nearest the route's start first.
//! The route's copper pushes as drawn: a track the route crosses once on a layer is pushed ahead
//! of that layer's run of it like a rubber band (`shove::fingered`), one it meets without
//! crossing is pushed aside.
//!
//! The pushes are all or none. When the head cannot be settled round the fixed things, the
//! route's copper meets something that is not a track or a track that cannot be shoved, or the
//! wave gives a track up, the sample walks: the head is laid round everything as in Walk mode, no
//! track pushed, and the wave's answer names the track it could not push
//! (`HeadAnswer::unpushed`). A head the fixed things block is the prefix before the block, its
//! pushes with it. A finishing sample walks (its second answer is Finish's).
//!
//! Cost per sample: the legs as in Walk mode; one query of the tracks near the trail; the head's
//! `make_legal` and `tighten` round the fixed things; one `hits` of the route's copper. When it
//! walks, the Walk-mode route again, as the wave's last step.

use super::obstacles::grow;
use super::obstacles::WorldObstacles;
use super::shove::{self, by_item, Hit, Kind, Member, Movers, Walk, Wave};
use super::{allocate, head_input, path, proposal, stretch, tile_size, track, GestureState, Legs};
use crate::embed::band::{make_legal, tighten, BandInput};
use crate::gesture::GestureSample;
use crate::proposal::{BlockCause, HeadAnswer, ProposalId, ProposalSource, RouteProposal, Tier};
use crate::request::Base;
use crate::verify::{hits, new_pieces, sorted};
use crate::world::{RBase, RItem, RPiece, WRef, World};
use fab_elec_board::{Anchor, BoardOp, ConnItem, Item};
use fab_elec_core::{BBox, ItemId, LayerId, Nm, P};
use std::collections::BTreeMap;
use std::time::Instant;

/// The Shove-mode answer to a route sample whose legs are `legs`: the route with the tracks it
/// meets left to a wave (its Full answer follows), or legal as it lies; `None` when it walks.
pub(super) fn answer(
    base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState, id: ProposalId, started: Instant,
    queries: &mut usize, legs: &Legs,
) -> Option<RouteProposal> {
    let gesture = &sample.gesture;
    let board = &base.board;
    let rules = &base.rules;
    let (anchor, end, trail) = head_input(board, gesture, legs.net, legs.start);
    // The fixed things: the legs' world without the shovable tracks near the trail.
    let mut fixed = legs.over.clone();
    let area = grow(BBox::of_points(trail.iter().copied()), legs.w as i64 + rules.max_clearance() as i64);
    let mut near: Vec<(ItemId, usize)> = fixed.query_sorted(gesture.layer, area).into_iter().filter_map(|(reference, it)| {
        let (WRef::Base(_), RPiece::Copper(c)) = (reference, &it.piece) else { return None };
        let ConnItem::Track { item, seg } = c.item else { return None };
        Some((item, seg as usize))
    }).collect();
    near.sort_unstable();
    near.dedup();
    // The area is the whole trail's: a track's arcs in it stay, its straight runs leave.
    for (other, mut segs) in by_item(&near) {
        let Some(other) = board.tracks.iter().find(|t| t.id == other) else { continue };
        segs.retain(|&k| other.path.segs.get(k).is_some_and(|seg| stretch::octilinear(stretch::vertex(&other.path, k), seg)));
        if let Some(windows) = stretch::shovable(world, other, &segs) {
            stretch::take_out(&mut fixed, world, other.id, |seg| windows.iter().any(|w| w.contains(&seg)));
        }
    }
    shove::free_vias(&mut fixed, world, board, gesture.layer, area, |_| false);
    let mut next = legs.next;
    let input = BandInput { s: legs.start, e: end, reference: &trail, tile: tile_size(legs.w, rules) };
    let mut obstacles = WorldObstacles::new(&fixed, rules, gesture.layer, legs.net, legs.class, legs.w, ItemId(next), &mut state.hulls);
    let band = make_legal(&input, &mut obstacles);
    let band = tighten(band, &input, &mut obstacles);
    *queries = band.stats.queries;
    if obstacles.failed() || band.blocked.is_some_and(|blocked| blocked.cause == BlockCause::Unsettled) {
        return None;
    }
    let blocked = band.blocked;
    let mut head_points = band.points;
    head_points.dedup();
    let end_anchor = if blocked.is_none() { anchor } else { Anchor::Free };
    let head_path = path(&head_points, legs.start);
    let length = head_path.length().round() as Nm;
    let mut items = legs.items.clone();
    if head_points.len() >= 2 {
        items.push(track(allocate(&mut next)?, legs.net, gesture.layer, legs.w, head_path.clone(), legs.start_anchor, end_anchor));
    }
    let head = HeadAnswer { path: head_path, layer: gesture.layer, w: legs.w, end: end_anchor, length, blocked, unpushed: None };

    // What the route's copper meets in the base: tracks' and vias' copper only, each pushed, the
    // nearest the route's start first.
    let runs = runs(&items);
    let pieces: Vec<RItem> = items.iter().flat_map(|item| new_pieces(board, item)).collect();
    let met = hits(&World::new(world), rules, board, &pieces);
    let (mut violated, mut met_vias) = (Vec::with_capacity(met.len()), Vec::new());
    let mut contact: BTreeMap<ItemId, i128> = BTreeMap::new();
    for (collision, reference) in &met {
        let item = match Hit::of(world, *reference)? {
            Hit::Track(item, seg) => {
                violated.push((item, seg));
                item
            }
            Hit::Via(via) => {
                met_vias.push(via);
                via
            }
        };
        let along = along(&runs, collision.at);
        contact.entry(item).and_modify(|c| *c = (*c).min(along)).or_insert(along);
    }
    violated.sort_unstable();
    violated.dedup();
    met_vias.sort_unstable();
    met_vias.dedup();
    let collisions = sorted(met.into_iter().map(|(collision, _)| collision).collect());
    let source = ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Stretch };
    let ops: Vec<BoardOp> = items.into_iter().map(BoardOp::Add).collect();
    if violated.is_empty() && met_vias.is_empty() {
        return Some(proposal(base, source, id, started, (ops, collisions, head), true));
    }
    let contact = |id: ItemId| contact.get(&id).copied().unwrap_or(i128::MAX);
    let mut members = Vec::new();
    let mut carried: Vec<ItemId> = Vec::new();
    for id in met_vias {
        let via = board.vias.iter().find(|v| v.id == id)?;
        let tracks = shove::pushable(board, world, via, |t| carried.contains(&t))?;
        carried.extend(tracks.iter().map(|t| t.id));
        members.extend(shove::via_members(board, via, tracks, contact(id)));
    }
    for (other, segs) in by_item(&violated) {
        if carried.contains(&other) {
            continue;
        }
        let other = board.tracks.iter().find(|t| t.id == other)?;
        let windows = stretch::shovable(world, other, &segs)?;
        members.push(Member::new(board, Kind::Neighbour { track: other.clone(), windows }, contact(other.id)));
    }
    let mut start = World::new(world);
    for piece in &pieces {
        start.add(piece.clone());
    }
    for member in &members {
        shove::take_out_member(&mut start, world, member);
    }
    state.wave = Some(Box::new(Wave::new(
        board.generation, (sample.id, sample.seq), started, ops.clone(), collisions.clone(),
        Movers::Drawn { count: pieces.len(), runs }, start.into_overlay(), members, Vec::new(),
        Some(Walk::head(gesture.clone(), head.clone())),
    )));
    Some(proposal(base, source, id, started, (ops, collisions, head), false))
}

/// The route's runs: its tracks' points, a run per stretch on one layer (a via starts the next).
fn runs(items: &[Item]) -> Vec<(LayerId, Vec<P>)> {
    let mut runs: Vec<(LayerId, Vec<P>)> = Vec::new();
    for item in items {
        let Item::Track(t) = item else { continue };
        let points = stretch::points(t.path.start, &t.path.segs);
        match runs.last_mut() {
            Some((layer, run)) if *layer == t.layer && run.last() == Some(&t.path.start) => run.extend(points.into_iter().skip(1)),
            _ => runs.push((t.layer, points)),
        }
    }
    runs
}

/// How far along the route (its runs in order, in nm) the point nearest `at` is.
fn along(runs: &[(LayerId, Vec<P>)], at: P) -> i128 {
    let (mut walked, mut best) = (0.0f64, (f64::MAX, 0.0f64));
    for (_, run) in runs {
        for pair in run.windows(2) {
            let (a, b) = ((pair[0].x as f64, pair[0].y as f64), (pair[1].x as f64, pair[1].y as f64));
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((at.x as f64 - a.0) * dx + (at.y as f64 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let (px, py) = (a.0 + t * dx - at.x as f64, a.1 + t * dy - at.y as f64);
            let d2 = px * px + py * py;
            if d2 < best.0 {
                best = (d2, walked + t * l2.sqrt());
            }
            walked += l2.sqrt();
        }
    }
    best.1.round() as i128
}
