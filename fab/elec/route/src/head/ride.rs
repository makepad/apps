//! A moved part's own fanout vias ride with it (E6 3.2), decided in one pass.
//!
//! A via one of whose tracks runs to a moved part moves by that part's translation, from the start
//! of the drag and whether a pad meets it or not, when `shove::pushable` lets it move and all its
//! tracks to moved items move by that one translation. Its tracks whose every end moves by its
//! translation (its stubs) are translated whole; its other tracks (its far tracks) are attached
//! tracks with the via end moved, laid before the rest. It rides when its place with its stubs
//! meets nothing in the world with what moves taken out (nor what moves by another translation
//! before it), and every far track of it can be laid. One that does not leaves: its copper goes
//! back as the lays then have it, and the vias that copper meets leave too.
//!
//! The answer is the stretch laid once with the vias that ride in the end, as if that set had been
//! known from the start. It is found in one pass:
//! - Admission: one exact check per via. Those refused leave together with every via their copper
//!   back meets (only the vias near that copper are checked again), and the set is laid out again.
//! - The far tracks, by via, each laid against the moved items, the riding vias and the far tracks
//!   laid before it (none laid after it). One that cannot be laid makes its via leave, with what
//!   its copper back meets. Each lay notes the areas it read (`World::record`): from the first lay
//!   whose areas meet copper that changed, the lays are taken out and done again in order, a lay
//!   whose areas meet none of it kept as it was. So each is what a lay with the final set gives.
//! - The final set laid out once, for what the stretch lays after it.
//!
//! Cost: one `pushable` per candidate via when the base generation or moved ids change. Each sample
//! checks the kept ends' translations, checks a few pieces per via for admission, and lays each
//! far track as the stretch lays a track. Per via that leaves: its tracks, the checks of the
//! vias near its copper, and again the far tracks whose reads its changes meet.

use super::drag::Refusal;
use super::obstacles::HullCache;
use super::shove;
use super::stretch::{changed, lay_clean, pieces_inside, windows, Window};
use crate::verify::{meets, meets_among, reach};
use crate::world::{RBase, RId, RItem, WRef, World};
use fab_elec_board::{Anchor, Board, ItemKind, RuleMatrix, Track};
use fab_elec_core::{BBox, Index, ItemId, LayerId, LayerSet, P};
use std::ops::{ControlFlow, Range};

/// An own fanout via that may ride: moved by its part's translation `by`.
struct Ride {
    via: ItemId,
    by: P,
}

/// The pushable fanout vias on this base, with the moved items their carried tracks end on.
#[derive(Default)]
pub(super) struct Fanout {
    moved: Option<Vec<ItemId>>,
    vias: Vec<(ItemId, Vec<ItemId>)>,
}

impl Fanout {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What a track is while a set of vias rides.
#[derive(Clone, Copy, PartialEq)]
enum Role {
    /// Nothing it is anchored on moves.
    Still,
    /// Every end moves by riding via `k`'s translation (the first such via): translated whole.
    Stub(usize),
    /// An end moves (`ends`): laid again. `rank`: the first riding via it is on, whose far track it
    /// is, else `NORMAL`.
    Attached { ends: [Option<P>; 2], rank: usize },
}

/// The rank of an attached track on no riding via.
const NORMAL: usize = usize::MAX;

/// The stretch's layout with the vias that ride in the end, and their far tracks laid.
pub(super) struct Settled<'a> {
    /// The moved items, then the riding vias, each with its translation.
    pub(super) moving: Vec<(ItemId, P)>,
    /// The riding vias' stubs, translated whole, in the order their `Replace`s go.
    pub(super) stubs: Vec<(&'a Track, P)>,
    /// The attached tracks with their ends and windows: the far tracks first, by via, then the
    /// rest, each in board order.
    pub(super) attached: Vec<(&'a Track, [Option<P>; 2], Vec<Window>)>,
    /// The far tracks as laid (the first of `attached`): each track and its new pieces.
    pub(super) far: Vec<(Track, Vec<RItem>)>,
    /// The base pieces that leave the world: the moving items', the stubs', and the attached
    /// tracks' windows.
    pub(super) out: Vec<RId>,
    /// What moves, at its new place: the moved items' pieces (the first `part`), then each riding
    /// via's with its stubs'; each with its translation and its base piece.
    pub(super) pieces: Vec<RItem>,
    pub(super) by_piece: Vec<P>,
    pub(super) movers: Vec<RId>,
    pub(super) part: usize,
}

/// The stretch's layout for the moved items `moved` with their own fanout vias riding where they
/// can, and those vias' far tracks laid. Err: a track's net has no class.
pub(super) fn settle<'a>(
    board: &'a Board, rules: &'a RuleMatrix, world: &'a RBase, moved: &'a [(ItemId, P)],
    fanout: &mut Fanout, hulls: &mut HullCache, queries: &mut usize,
) -> Result<Settled<'a>, Refusal> {
    let rides = own_fanout(board, world, moved, fanout);
    let mut tracks: Vec<usize> = moved.iter().map(|&(item, _)| item).chain(rides.iter().map(|ride| ride.via))
        .flat_map(|item| world.tracks_on(item))
        .filter_map(|&id| match board.locate(id) { Some((ItemKind::Track, i)) => Some(i), _ => None }).collect();
    tracks.sort_unstable();
    tracks.dedup();
    let riding = vec![true; rides.len()];
    let mut body = Body { board, world, rules, moved, rides, riding, roles: vec![Role::Still; tracks.len()], outs: vec![None; tracks.len()], tracks };
    if body.rides.is_empty() {
        let lay = body.layout(None);
        return Ok(body.settled(lay, Vec::new()));
    }
    let (still, lay) = body.admit();
    let far = body.lay_far(still, &lay, hulls, queries)?;
    // During the far pass vias can only leave, so an unchanged count means admission's layout is final.
    let lay = if body.riding.iter().filter(|&&on| on).count() == lay.groups.len() {
        lay
    } else {
        body.layout(Some(lay))
    };
    Ok(body.settled(lay, far))
}

/// The moved parts' own fanout vias that may ride, by id. The pushable vias and their moved ends
/// are kept for this base generation and these moved ids. A via rides when it has a moved end
/// and all its moved ends have the same translation in this sample.
fn own_fanout(board: &Board, world: &RBase, moved: &[(ItemId, P)], fanout: &mut Fanout) -> Vec<Ride> {
    let ids = moved.iter().map(|&(item, _)| item);
    if fanout.moved.as_ref().is_none_or(|kept| kept.iter().copied().ne(ids.clone())) {
        let ids: Vec<_> = ids.collect();
        fanout.vias = pushable_fanout(board, world, &ids);
        fanout.moved = Some(ids);
    }
    let by_of = |item: ItemId| moved.iter().find(|(m, _)| *m == item).map(|(_, by)| *by);
    fanout.vias.iter().filter_map(|(via, ends)| {
        let (&first, rest) = ends.split_first()?;
        let by = by_of(first)?;
        rest.iter().all(|&item| by_of(item) == Some(by)).then_some(Ride { via: *via, by })
    }).collect()
}

/// Each candidate via that `shove::pushable` lets move, by id, with its carried tracks' moved ends.
fn pushable_fanout(board: &Board, world: &RBase, moved: &[ItemId]) -> Vec<(ItemId, Vec<ItemId>)> {
    let mut vias = Vec::new();
    for &item in moved {
        if !matches!(board.locate(item), Some((ItemKind::Placed, _))) {
            continue;
        }
        for &id in world.tracks_on(item) {
            let Some((ItemKind::Track, i)) = board.locate(id) else { continue };
            vias.extend(board.tracks[i].ends.iter().filter_map(|end| match *end {
                Anchor::Via(via) if !moved.contains(&via) => Some(via),
                _ => None,
            }));
        }
    }
    vias.sort_unstable();
    vias.dedup();
    vias.into_iter().filter_map(|id| {
        let Some((ItemKind::Via, i)) = board.locate(id) else { return None };
        let tracks = shove::pushable(board, world, &board.vias[i], |_| false)?;
        let ends = tracks.iter().flat_map(|track| track.ends).filter_map(Anchor::item)
            .filter(|item| moved.contains(item)).collect();
        Some((id, ends))
    }).collect()
}

/// The riding decision for one sample.
struct Body<'a> {
    board: &'a Board,
    world: &'a RBase,
    rules: &'a RuleMatrix,
    moved: &'a [(ItemId, P)],
    /// The own fanout vias that may ride, by id, and whether each still does.
    rides: Vec<Ride>,
    riding: Vec<bool>,
    /// The board indices of the tracks on a moved item or on one of `rides`, sorted, and each one's
    /// role with the vias riding now, with its base pieces out of the world and its windows in that
    /// role (`None` until asked).
    tracks: Vec<usize>,
    roles: Vec<Role>,
    outs: Vec<Option<(Vec<RId>, Vec<Window>)>>,
}

/// The pieces that move, and what leaves the world, for one riding set.
#[derive(Default)]
struct Layout {
    pieces: Vec<RItem>,
    by_piece: Vec<P>,
    movers: Vec<RId>,
    part: usize,
    /// Each riding via (its index in `rides`) with the range of its pieces and its stubs'.
    groups: Vec<(usize, Range<usize>)>,
    /// The stubs (board index) in the order their `Replace`s go, each with its translation.
    stubs: Vec<(usize, P)>,
    out: Vec<RId>,
    /// The number of base pieces from moved items at the start of `out`.
    moved_out: usize,
}

impl<'a> Body<'a> {
    /// Where `anchor` moves by, with the vias `riding` riding.
    fn by_with(&self, riding: &[bool], anchor: Anchor) -> Option<P> {
        let item = anchor.item()?;
        if let Some(&(_, by)) = self.moved.iter().find(|(m, _)| *m == item) {
            return Some(by);
        }
        let k = self.rides.binary_search_by_key(&item, |ride| ride.via).ok()?;
        riding[k].then_some(self.rides[k].by)
    }

    /// The index in `rides` of the riding via `anchor` is on.
    fn ride_at(&self, riding: &[bool], anchor: Anchor) -> Option<usize> {
        let Anchor::Via(via) = anchor else { return None };
        let k = self.rides.binary_search_by_key(&via, |ride| ride.via).ok()?;
        riding[k].then_some(k)
    }

    /// Its moved ends of `track` with the vias `riding` riding.
    fn ends_with(&self, riding: &[bool], track: &Track) -> [Option<P>; 2] {
        [0, 1].map(|e| Some(self.board.anchor_pos(track.ends[e])?.add(self.by_with(riding, track.ends[e])?)))
    }

    /// What `track` is with the vias `riding` riding.
    fn role_with(&self, riding: &[bool], track: &Track) -> Role {
        let [a, b] = track.ends.map(|end| self.ride_at(riding, end));
        let on = match (a, b) {
            (Some(a), Some(b)) => [Some(a.min(b)), Some(a.max(b))],
            (a, b) => [a.or(b), None],
        };
        let bys = track.ends.map(|end| self.by_with(riding, end));
        if let Some(k) = on.into_iter().flatten().find(|&k| bys.iter().all(|&by| by == Some(self.rides[k].by))) {
            return Role::Stub(k);
        }
        let ends = self.ends_with(riding, track);
        if ends == [None, None] || track.path.segs.is_empty() {
            return Role::Still;
        }
        Role::Attached { ends, rank: on[0].unwrap_or(NORMAL) }
    }

    /// The base pieces of track `ix` that are out of the world in role `role`, and its windows.
    fn out_of(&self, ix: usize, role: Role) -> (Vec<RId>, Vec<Window>) {
        let track = &self.board.tracks[ix];
        match role {
            Role::Still => (Vec::new(), Vec::new()),
            Role::Stub(_) => (self.world.pieces(track.id).to_vec(), Vec::new()),
            Role::Attached { ends, .. } => {
                let windows = windows(track, ends);
                let mut out = Vec::new();
                pieces_inside(self.world, track.id, |seg| changed(track, ends, &windows, seg), &mut out);
                (out, windows)
            }
        }
    }

    /// Track `k` (an index into `tracks`) in role `role`, with what it then has out of the world
    /// (worked out again only when its role changes).
    fn set_role(&mut self, k: usize, role: Role) {
        if self.roles[k] != role || self.outs[k].is_none() {
            self.outs[k] = Some(self.out_of(self.tracks[k], role));
        }
        self.roles[k] = role;
    }

    fn out(&self, k: usize) -> &[RId] {
        self.outs[k].as_ref().map_or(&[], |(out, _)| out)
    }

    fn position(&self, ix: usize) -> usize {
        self.tracks.binary_search(&ix).expect("a track on what may move is kept")
    }

    /// Every track's role, and the layout, for the vias riding now. A previous layout from this
    /// sample supplies the moved items' translated pieces and their base pieces in `out`.
    fn layout(&mut self, previous: Option<Layout>) -> Layout {
        for k in 0..self.tracks.len() {
            let role = self.role_with(&self.riding, &self.board.tracks[self.tracks[k]]);
            self.set_role(k, role);
        }
        let mut lay = if let Some(mut lay) = previous {
            lay.pieces.truncate(lay.part);
            lay.by_piece.truncate(lay.part);
            lay.movers.truncate(lay.part);
            lay.out.truncate(lay.moved_out);
            lay.groups.clear();
            lay.stubs.clear();
            lay
        } else {
            let mut lay = Layout::default();
            for &(item, by) in self.moved {
                lay.out.extend_from_slice(self.world.pieces(item));
                translated(self.world, item, by, &mut lay.pieces, &mut lay.by_piece, &mut lay.movers);
            }
            lay.part = lay.pieces.len();
            lay.moved_out = lay.out.len();
            lay
        };
        for (k, ride) in self.rides.iter().enumerate().filter(|&(k, _)| self.riding[k]) {
            let first = lay.pieces.len();
            lay.out.extend_from_slice(self.world.pieces(ride.via));
            translated(self.world, ride.via, ride.by, &mut lay.pieces, &mut lay.by_piece, &mut lay.movers);
            for &id in self.world.tracks_on(ride.via) {
                let Some((ItemKind::Track, ix)) = self.board.locate(id) else { continue };
                if self.roles[self.position(ix)] == Role::Stub(k) && !lay.stubs.iter().any(|&(s, _)| s == ix) {
                    lay.stubs.push((ix, ride.by));
                    translated(self.world, id, ride.by, &mut lay.pieces, &mut lay.by_piece, &mut lay.movers);
                }
            }
            lay.groups.push((k, first..lay.pieces.len()));
        }
        for k in 0..self.tracks.len() {
            lay.out.extend_from_slice(self.out(k));
        }
        lay
    }

    /// Whether riding via `k` with `mine` (its pieces and its stubs', moved) meets nothing in
    /// `still`, nor what moves by another translation before it: the moved items' pieces (`part`,
    /// each with its translation), and the riding vias before it (`groups`: each riding via's index
    /// and pieces).
    fn admitted(&self, still: &World, part: (&[RItem], &[P]), groups: &[(usize, &[RItem])], k: usize, mine: &[RItem]) -> bool {
        let by = self.rides[k].by;
        let mut other: Vec<RItem> = part.0.iter().zip(part.1).filter(|&(_, &b)| b != by).map(|(it, _)| it.clone()).collect();
        other.extend(groups.iter().filter(|&&(j, _)| j < k && self.rides[j].by != by).flat_map(|&(_, pieces)| pieces.iter().cloned()));
        if other.is_empty() {
            return !meets(still, self.rules, self.board, mine);
        }
        let mut with = still.clone();
        for it in other {
            with.add(it);
        }
        !meets(&with, self.rules, self.board, mine)
    }

    /// Admission: the riding vias whose place meets anything leave, with those their copper back
    /// meets; the world with what moves taken out, and the layout, for the vias left.
    fn admit(&mut self) -> (World<'a>, Layout) {
        let lay = self.layout(None);
        let mut still = World::new(self.world);
        still.remove_many(lay.out.iter().copied());
        let part = (&lay.pieces[..lay.part], &lay.by_piece[..lay.part]);
        let groups: Vec<(usize, &[RItem])> = lay.groups.iter().map(|(k, range)| (*k, &lay.pieces[range.clone()])).collect();
        let refused: Vec<usize> = groups.iter().filter(|&&(k, mine)| !self.admitted(&still, part, &groups, k, mine)).map(|&(k, _)| k).collect();
        if refused.is_empty() {
            return (still, lay);
        }
        let near = near(&groups, self.rules);
        let refused = self.closure(&still, &groups, &near, refused);
        for k in refused {
            self.riding[k] = false;
        }
        // Laid out again, every via left is admitted: the world is the closure's (the same pieces
        // out), and each via's pieces, and those that move before it, are fewer than the closure
        // admitted it with (a stub on a via that left is a far track now). So no check again.
        let lay = self.layout(Some(lay));
        let mut still = World::new(self.world);
        still.remove_many(lay.out.iter().copied());
        (still, lay)
    }

    /// The vias that leave when the riding vias `refused` do: those, then every via whose place
    /// meets their copper back as the next layout has it (with the vias left moving), until none
    /// more. Only the vias near that copper (`near`) can meet more than they did, so only those are
    /// checked again. `still`: the world with what moves taken out; `groups`: each riding via with
    /// its pieces and its stubs'.
    fn closure(&self, still: &World<'a>, groups: &[(usize, &[RItem])], near: &Index<usize>, mut refused: Vec<usize>) -> Vec<usize> {
        let mut back = still.clone();
        let mut left = self.riding.clone();
        let mut done = 0;
        while done < refused.len() {
            for &k in &refused[done..] {
                left[k] = false;
            }
            // Each refused via's pieces and its tracks' back, then those tracks' windows (with the
            // vias left moving) out again: at once for the batch, each track's windows being within
            // its own pieces.
            let (mut restored, mut out) = (Vec::new(), Vec::new());
            for &k in &refused[done..] {
                let via = self.rides[k].via;
                restored.extend_from_slice(self.world.pieces(via));
                for &id in self.world.tracks_on(via) {
                    let Some((ItemKind::Track, ix)) = self.board.locate(id) else { continue };
                    let track = &self.board.tracks[ix];
                    restored.extend_from_slice(self.world.pieces(id));
                    let ends = self.ends_with(&left, track);
                    if ends != [None, None] && !track.path.segs.is_empty() {
                        let windows = windows(track, ends);
                        pieces_inside(self.world, id, |seg| changed(track, ends, &windows, seg), &mut out);
                    }
                }
            }
            // The copper that comes back: out of `back` before, and not out again.
            out.sort_unstable();
            let came: Vec<RId> = restored.iter().copied().filter(|&rid| back.get(WRef::Base(rid)).is_none() && out.binary_search(&rid).is_err()).collect();
            back.restore_many(restored.iter().copied());
            back.remove_many(out);
            done = refused.len();
            // A via left met nothing in `back` before, nor in what moves before it (which only
            // shrinks): only the copper that came back can refuse it now.
            for (k, its) in near_back(near, self.world, &came) {
                if left[k] && meets_among(self.world, self.rules, self.board, mine_of(groups, k), &its) {
                    refused.push(k);
                }
            }
        }
        refused
    }

    /// The far tracks laid, by via: one that cannot be laid makes its via leave (with what its
    /// copper back then meets), and the lays its changes may alter are laid again.
    fn lay_far(&mut self, still: World<'a>, lay: &Layout, hulls: &mut HullCache, queries: &mut usize) -> Result<Vec<Far>, Refusal> {
        let mut far: Vec<Far> = (0..self.tracks.len()).filter_map(|k| match self.roles[k] {
            Role::Attached { ends, rank } if rank != NORMAL => Some(Far { ix: self.tracks[k], ends, rank, refs: Vec::new(), shown: false, laid: None }),
            _ => None,
        }).collect();
        if far.is_empty() {
            return Ok(far);
        }
        far.sort_by_key(|far| (far.rank, far.ix));
        let mut pass = Pass { over: still.clone(), still, movers: vec![Vec::new(); self.rides.len()], groups: vec![Vec::new(); self.rides.len()], changes: Vec::new(), far };
        for i in 0..lay.part {
            pass.over.add_moved(lay.movers[i], lay.by_piece[i]);
        }
        for (k, range) in &lay.groups {
            pass.groups[*k] = lay.pieces[range.clone()].to_vec();
            for i in range.clone() {
                let r = pass.over.add_moved(lay.movers[i], lay.by_piece[i]).expect("a mover is a base piece");
                pass.movers[*k].push(r);
            }
        }
        let groups: Vec<(usize, &[RItem])> = lay.groups.iter().map(|(k, range)| (*k, &lay.pieces[range.clone()])).collect();
        let near = near(&groups, self.rules);
        let mut at = 0;
        while at < pass.far.len() {
            let far = &mut pass.far[at];
            if far.laid.as_ref().is_some_and(|laid| laid.valid) {
                for &r in &far.refs {
                    pass.over.show(r);
                }
                far.shown = true;
                at += 1;
                continue;
            }
            let (ix, ends, rank) = (far.ix, far.ends, far.rank);
            let track = &self.board.tracks[ix];
            let windows = windows(track, ends);
            pass.over.record();
            let tried = lay_clean(&pass.over, self.board, self.rules, hulls, queries, (track, ends, &windows));
            let reads = pass.over.take_reads();
            let Some((laid, fresh, pieces)) = tried? else {
                // Its via leaves; from the first lay that is not as it was, they go again in order.
                pass.changes.clear();
                self.leave(rank, &mut pass, &near);
                at = pass.roll_back();
                continue;
            };
            let far = &mut pass.far[at];
            if far.laid.as_ref().is_some_and(|old| old.track.path == laid.path && old.fresh == fresh) {
                for &r in &far.refs {
                    pass.over.show(r);
                }
            } else {
                pass.changes = far.laid.iter().flat_map(|old| old.pieces.iter()).chain(&pieces).map(|it| (it.layers, it.bbox)).collect();
                far.refs = pieces.iter().map(|it| pass.over.add(it.clone())).collect();
                pass.invalidate(at + 1);
            }
            let area = reads.iter().fold(BBox::EMPTY, |a, &(_, b)| a.union(b));
            let far = &mut pass.far[at];
            far.shown = true;
            far.laid = Some(Laid { track: laid, fresh, pieces, reads, area, valid: true });
            at += 1;
        }
        Ok(pass.far)
    }

    /// Riding via `v` leaves, as the next stretch would have it: its copper back, then the vias
    /// whose place that copper meets leave with what their copper back meets (`closure`). Only the
    /// vias near the copper back can meet more than they did. The lays whose reads the changes
    /// meet are marked to be laid again.
    fn leave(&mut self, v: usize, pass: &mut Pass<'a>, near: &Index<usize>) {
        self.riding[v] = false;
        let came = self.apply_leave(pass, &[v]);
        let refused = {
            let groups: Vec<(usize, &[RItem])> = (0..self.rides.len()).filter(|&j| self.riding[j]).map(|j| (j, pass.groups[j].as_slice())).collect();
            // Each via riding met nothing before, nor what moves before it (which only shrinks):
            // only the copper that came back can refuse it now.
            let refused: Vec<usize> = near_back(near, self.world, &came).into_iter()
                .filter(|(k, its)| self.riding[*k] && meets_among(self.world, self.rules, self.board, &pass.groups[*k], its)).map(|(k, _)| k).collect();
            if refused.is_empty() { refused } else { self.closure(&pass.still, &groups, near, refused) }
        };
        if !refused.is_empty() {
            for &k in &refused {
                self.riding[k] = false;
            }
            self.apply_leave(pass, &refused);
        }
        pass.invalidate(0);
    }

    /// The vias `vias`, no longer riding, out of the pass: their moved pieces out, their base
    /// copper back, each of their tracks in the role it has now (a stub on one that rides on is
    /// that one's far track; a far track on one that rides on is that one's), and the changes
    /// noted. The base pieces put back.
    fn apply_leave(&mut self, pass: &mut Pass<'a>, vias: &[usize]) -> Vec<RId> {
        let (mut restored, mut removed) = (Vec::new(), Vec::new());
        for &k in vias {
            for r in std::mem::take(&mut pass.movers[k]) {
                pass.hide(r);
            }
            pass.groups[k].clear();
            restored.extend_from_slice(self.world.pieces(self.rides[k].via));
        }
        for &k in vias {
            for &id in self.world.tracks_on(self.rides[k].via) {
                let Some((ItemKind::Track, ix)) = self.board.locate(id) else { continue };
                let at = self.position(ix);
                let (old, new) = (self.roles[at], self.role_with(&self.riding, &self.board.tracks[ix]));
                if old == new {
                    continue;
                }
                let was = self.out(at).to_vec();
                self.set_role(at, new);
                let is = self.out(at);
                restored.extend(was.iter().filter(|rid| !is.contains(rid)));
                removed.extend(is.iter().filter(|rid| !was.contains(rid)));
                match old {
                    // Its moved pieces leave with the via whose stub it was.
                    Role::Stub(j) => {
                        let mut kept = Vec::new();
                        for r in std::mem::take(&mut pass.movers[j]) {
                            match pass.over.get(r) {
                                Some(it) if it.item == Some(id) => pass.hide(r),
                                _ => kept.push(r),
                            }
                        }
                        pass.movers[j] = kept;
                        pass.groups[j].retain(|it| it.item != Some(id));
                    }
                    Role::Attached { rank, .. } if rank != NORMAL => pass.drop_far(ix),
                    _ => {}
                }
                if let Role::Attached { ends, rank } = new {
                    if rank != NORMAL {
                        let at = pass.far.partition_point(|far| (far.rank, far.ix) < (rank, ix));
                        pass.far.insert(at, Far { ix, ends, rank, refs: Vec::new(), shown: false, laid: None });
                    }
                }
            }
        }
        pass.still.restore_many(restored.iter().copied());
        pass.over.restore_many(restored.iter().copied());
        pass.still.remove_many(removed.iter().copied());
        pass.over.remove_many(removed.iter().copied());
        pass.changes.extend(restored.iter().chain(&removed).filter_map(|&rid| self.world.get(rid)).map(|it| (it.layers, it.bbox)));
        restored
    }

    /// Takes the final layout and combines it with the riding vias' far tracks as laid.
    fn settled(&self, lay: Layout, far: Vec<Far>) -> Settled<'a> {
        let board = self.board;
        let mut moving: Vec<(ItemId, P)> = self.moved.to_vec();
        moving.extend(self.rides.iter().zip(&self.riding).filter(|(_, &on)| on).map(|(ride, _)| (ride.via, ride.by)));
        let mut attached: Vec<(usize, usize, [Option<P>; 2], usize)> = (0..self.tracks.len()).filter_map(|k| match self.roles[k] {
            Role::Attached { ends, rank } => Some((rank, self.tracks[k], ends, k)),
            _ => None,
        }).collect();
        attached.sort_by_key(|&(rank, ix, ..)| (rank, ix));
        Settled {
            moving,
            stubs: lay.stubs.iter().map(|&(ix, by)| (&board.tracks[ix], by)).collect(),
            attached: attached.into_iter().map(|(_, ix, ends, k)| (&board.tracks[ix], ends, self.outs[k].as_ref().map_or(Vec::new(), |(_, windows)| windows.clone()))).collect(),
            far: far.into_iter().map(|far| far.laid.map(|laid| (laid.track, laid.pieces)).expect("every far track of a via that rides is laid")).collect(),
            out: lay.out,
            pieces: lay.pieces,
            by_piece: lay.by_piece,
            movers: lay.movers,
            part: lay.part,
        }
    }
}

/// One far track: an attached track on a riding via, with what its lay gave.
struct Far {
    /// Its board index.
    ix: usize,
    ends: [Option<P>; 2],
    rank: usize,
    /// Its pieces in the world, shown while it is laid there.
    refs: Vec<WRef>,
    shown: bool,
    laid: Option<Laid>,
}

/// A far track's lay, the areas it read, and whether nothing that changed since meets them.
struct Laid {
    track: Track,
    fresh: Vec<usize>,
    pieces: Vec<RItem>,
    reads: Vec<(LayerId, BBox)>,
    area: BBox,
    valid: bool,
}

/// The far tracks' pass: the world with what moves taken out (`still`), the world they are laid
/// in (`over`: `still` with what moves at its new place and the far tracks laid so far), each
/// riding via's pieces moved there and its pieces with its stubs', the copper the last change
/// changed, and the far tracks.
struct Pass<'a> {
    still: World<'a>,
    over: World<'a>,
    movers: Vec<Vec<WRef>>,
    groups: Vec<Vec<RItem>>,
    changes: Vec<(LayerSet, BBox)>,
    far: Vec<Far>,
}

impl Pass<'_> {
    /// Hide moved piece `r`: a change.
    fn hide(&mut self, r: WRef) {
        if let Some(it) = self.over.get(r) {
            self.changes.push((it.layers, it.bbox));
        }
        self.over.hide(r);
    }

    /// The far track of board index `ix` is one no more: its pieces out, a change.
    fn drop_far(&mut self, ix: usize) {
        let Some(at) = self.far.iter().position(|far| far.ix == ix) else { return };
        let far = self.far.remove(at);
        // The lays after it saw it as last laid, shown or not.
        if let Some(laid) = &far.laid {
            self.changes.extend(laid.pieces.iter().map(|it| (it.layers, it.bbox)));
        }
        if far.shown {
            for r in far.refs {
                self.over.hide(r);
            }
        }
    }

    /// Mark the lays from `from` on whose reads meet the changes.
    fn invalidate(&mut self, from: usize) {
        let changes = &self.changes;
        for far in self.far.iter_mut().skip(from) {
            let Some(laid) = far.laid.as_mut().filter(|laid| laid.valid) else { continue };
            if changes.iter().any(|&(layers, b)| b.intersects(&laid.area) && laid.reads.iter().any(|&(layer, r)| layers.has(layer) && b.intersects(&r))) {
                laid.valid = false;
            }
        }
    }

    /// The far tracks from the first that is not laid as it was taken out of the world; where the
    /// lays go on from.
    fn roll_back(&mut self) -> usize {
        let first = self.far.iter().position(|far| !far.shown || !far.laid.as_ref().is_some_and(|laid| laid.valid)).unwrap_or(self.far.len());
        for far in &mut self.far[first..] {
            if far.shown {
                for &r in &far.refs {
                    self.over.hide(r);
                }
                far.shown = false;
            }
        }
        first
    }
}

/// An index of the riding vias `groups` by their pieces' box grown by the rules' reach. A via
/// drill's wider reach to pads does not matter here: the copper that comes back has no pads.
fn near(groups: &[(usize, &[RItem])], rules: &RuleMatrix) -> Index<usize> {
    Index::build(groups.iter().map(|&(k, mine)| (grown(mine, reach(rules)), k)).collect())
}

/// The riding vias (indices into `rides`) whose pieces may meet one of the base pieces `came`,
/// in order, each with those of `came` near it: the via's pieces' box, grown by the rules' reach,
/// meets the piece's box.
fn near_back<'w>(near: &Index<usize>, world: &'w RBase, came: &[RId]) -> Vec<(usize, Vec<&'w RItem>)> {
    let mut pairs: Vec<(usize, &RItem)> = Vec::new();
    for it in came.iter().filter_map(|&rid| world.get(rid)) {
        near.query(it.bbox, |_, k| {
            pairs.push((k, it));
            ControlFlow::Continue(())
        });
    }
    pairs.sort_by_key(|&(k, _)| k);
    let mut out: Vec<(usize, Vec<&RItem>)> = Vec::new();
    for (k, it) in pairs {
        match out.last_mut() {
            Some((last, its)) if *last == k => its.push(it),
            _ => out.push((k, vec![it])),
        }
    }
    out
}

/// The pieces of riding via `k` in `groups` (each riding via with its pieces, in order).
fn mine_of<'g>(groups: &[(usize, &'g [RItem])], k: usize) -> &'g [RItem] {
    groups.binary_search_by_key(&k, |&(j, _)| j).map_or(&[], |i| groups[i].1)
}

/// The box of `pieces`, grown by `by`.
fn grown(pieces: &[RItem], by: fab_elec_core::Nm) -> BBox {
    pieces.iter().fold(BBox::EMPTY, |a, it| a.union(it.bbox)).inflate(by)
}

/// The base pieces of `item` translated by `by`, each with its translation and its base piece.
fn translated(world: &RBase, item: ItemId, by: P, pieces: &mut Vec<RItem>, by_piece: &mut Vec<P>, movers: &mut Vec<RId>) {
    for &rid in world.pieces(item) {
        let Some(it) = world.get(rid) else { continue };
        pieces.push(it.translated(by));
        by_piece.push(by);
        movers.push(rid);
    }
}
