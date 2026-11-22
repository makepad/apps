//! Shove at rank 1 (E6 3.2; the shove answer's S2 to S5). After a part or via drag's stretch
//! answer, the tracks it cut back and the neighbours' tracks the moved items now meet are laid
//! again, one at a time, on the worker's thread between its other work. The wave's Full answer
//! for the same sample replaces the stretch answer, and a release commits only that one. A segment
//! drag's wave (`drag`) and a route drawn in Shove mode (`push`) have neighbours only, and their
//! pushes are all or none (`Walk`). A drawn route's pieces did not move from anywhere: where a run
//! of it crosses a neighbour's base path once, the path is pushed ahead of the run's end like a
//! rubber band (a finger, `fingered`), as a moved part's travel spikes it.
//!
//! The wave's world is the base with the moved items at their new places and the attached tracks
//! the stretch laid clean; every member's windows are out of it. Each member is laid once, in
//! first-contact order (attached tracks first, then neighbours by how far along a moved item's
//! travel it meets them, then board order), against everything laid before it:
//! - A neighbour stays where it is when its copper there is legal against what is laid (what pushed
//!   it went back or moved off). Else its windows (its runs of straight 45-degree lines round the
//!   segments a moved item meets) are laid from their base path, with a spike past each moved part
//!   whose travel crossed it (transport: the track keeps passing that part on the side it passed
//!   it before), then tightened in the runs that changed, their ends fixed.
//! - An attached track's windows (a carried track's too: it stays where it is, as a neighbour
//!   does, when none of its ends moves) are laid from its moved end along its first segment moved
//!   with the part, back to that segment's far end and on along its base path, spiked the same way and
//!   tightened, when that turns no more sharply than a right angle or the base path; else as the
//!   stretch lays it.
//!
//! A member that cannot be laid, but can be against the world without the members laid before
//! it, is blocked by the earliest of those its path meets: the pair is recorded, and the wave rolls
//! back to just before that member with this one in front of it, and any member that a recorded
//! pair puts before this one in front of it too. A member blocked by one that the recorded pairs
//! already put before it (that pair, or a chain of them) closes a cycle, and the later of the two
//! in board order is given up.
//!
//! An attached track whose moved end sits in the clearance of something the wave never moves (a
//! pad, the outline, a hole or keepout, a via that is not a member and cannot be pushed) cannot be
//! laid in any world the wave tries it in: every way ends in giving it up for why it is blocked
//! against its start alone. It is laid there at once, by the stretch's way, and given up as soon
//! as it is blocked (`Wave::stuck`), with the same answer and without the ways that cannot help.
//!
//! A member blocked by what stays (it cannot be laid against the start alone) is laid against the
//! start without the tracks near it that have not entered the wave and can move: neighbours' tracks
//! that can be shoved, and tracks attached to a moved item that the stretch laid. The tracks its
//! copper then meets join the wave after its members (track pushes track): a neighbour's with its
//! windows round the segments met, an attached one with its windows as the stretch laid them, out
//! of its world; and it is laid again. There the stretch's way of laying it (after a transport not
//! taken) runs only when the copper it was blocked with met one of those tracks: a member blocked
//! by other copper alone is given up after the transport. Each track enters the wave once. A member
//! that cannot be laid even so, or meets a track that cannot be shoved, is given up: an attached
//! track is cut back as the stretch cuts it (clear of every neighbour's base windows too, and of
//! the copper the stretch laid for each attached track that joined, since those go back there when
//! given up), or, when it joined, goes back where the stretch laid it; a neighbour stays where it
//! is, marked by its collision with the moved item (a track that joined has none). Copper the
//! person did not touch is never
//! cut, and tracks anchored on a track the answer changes keep their copper as in the stretch
//! answer. Transport past a joined track's pusher (a spike from the pusher's base window) is not
//! done yet: a joined track takes the side its base path gives it, and a wrong side is blocked.
//!
//! Each step takes one way of laying one member, or one run of it, so the worker reads its inbox
//! between them (and a newer sample there stops a step between its parts, `Wanted`): a neighbour's windows, an attached track's transport, or one run the stretch's
//! way of laying it tries (after a transport not taken); in the wave's world, then, blocked there,
//! in its start alone, then, blocked by what stays, in the start without the tracks near it. Cost
//! per step: that way's spikes (the parts near its windows against their segments), its
//! `make_legal` and `tighten` (one `make_legal` for a run), and one `verify` of what it lays when
//! it is decided, plus a `verify` against what is laid when it is laid alone or without the tracks
//! near it. A roll back or a give-up rebuilds the wave's world from its start, O(pieces laid).
//! Every repair adds a pair that closes no cycle (the members' order keeps every pair recorded, so
//! none is recorded twice), every join adds a track that never entered the wave, and every give-up
//! removes a member, so a wave ends; there is no cap.

use super::drag::{self, Dragged};
use super::obstacles::{HullCache, WorldObstacles};
use super::stretch::{self, Window};
use super::{tile_size, via};
use crate::embed::band::{make_legal, tighten, Band, BandInput, BandStats, Obstacles, Part};
use crate::gesture::{Gesture, GestureId};
use super::Routed;
use crate::proposal::{BlockCause, Blocked, Collision, HeadAnswer, ProposalId, ProposalSource, RouteProposal, Tier};
use crate::request::Base;
use crate::solution::{Reason, RouteMetrics};
use crate::verify::{hits, new_pieces, verify};
use crate::world::{Overlay, RBase, RId, RItem, RPiece, WRef, World};
use fab_elec_board::{check_to, Anchor, Board, BoardOp, ConnItem, Copper, Item, ItemKind, Origin, RuleMatrix, Track, Via};
use fab_elec_core::dist::clearance;
use fab_elec_core::geom::orient;
use fab_elec_core::{BBox, ClassId, ItemId, LayerId, NetId, Nm, Path, Seg, Sel, Shape, P};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::ops::Range;
use std::time::Instant;

/// A track or via the wave lays again.
pub(super) struct Member {
    pub(super) kind: Kind,
    /// First contact: the squared distance along a moved item's travel at which it meets the track
    /// or via (0 for an attached track, `i128::MAX` when none does).
    contact: i128,
    /// Its place in the board's tracks, then vias.
    order: usize,
}

/// What a member is.
pub(super) enum Kind {
    /// A track attached to a moved item: its moved ends and its windows. Cut back by the stretch,
    /// or laid by it and joined the wave (`back`).
    Attached { track: Track, ends: [Option<P>; 2], windows: Vec<Window>, back: Option<Back> },
    /// A track a pushed via carries, laid as an attached track is: its ends and windows are from
    /// where that via is laid. Never cut: when it cannot be laid, the via goes back with it.
    Carried { track: Track, ends: [Option<P>; 2], windows: Vec<Window>, via: ItemId },
    /// A neighbour a moved item meets: the runs of its segments laid again.
    Neighbour { track: Track, windows: Vec<Range<usize>> },
    /// A via a moved item meets, pushed to the nearest place clear of everything, and the tracks
    /// it carries (members after it).
    Via { via: Via, carried: Vec<ItemId> },
}

impl Member {
    /// A member; a via's carried tracks take its place in the order, right after it.
    pub(super) fn new(board: &Board, kind: Kind, contact: i128) -> Member {
        let via_order = |via: ItemId| board.tracks.len() + board_index(board, ItemKind::Via, via).unwrap_or(board.vias.len());
        let order = match &kind {
            Kind::Via { via, .. } => via_order(via.id),
            Kind::Carried { via, .. } => via_order(*via),
            Kind::Attached { track, .. } | Kind::Neighbour { track, .. } => board_index(board, ItemKind::Track, track.id).unwrap_or(usize::MAX),
        };
        Member { kind, contact, order }
    }

    fn id_of(kind: &Kind) -> ItemId {
        match kind {
            Kind::Attached { track, .. } | Kind::Carried { track, .. } | Kind::Neighbour { track, .. } => track.id,
            Kind::Via { via, .. } => via.id,
        }
    }

    pub(super) fn id(&self) -> ItemId {
        Member::id_of(&self.kind)
    }

    fn net(&self) -> NetId {
        match &self.kind {
            Kind::Attached { track, .. } | Kind::Carried { track, .. } | Kind::Neighbour { track, .. } => track.net,
            Kind::Via { via, .. } => via.net,
        }
    }

    fn neighbour(&self) -> bool {
        matches!(self.kind, Kind::Neighbour { .. })
    }

    /// The via this member is or is carried by.
    fn group(&self) -> Option<ItemId> {
        match &self.kind {
            Kind::Via { via, .. } => Some(via.id),
            Kind::Carried { via, .. } => Some(*via),
            Kind::Attached { .. } | Kind::Neighbour { .. } => None,
        }
    }
}

/// Where the stretch laid an attached track that joined the wave: the pieces of its windows, and
/// its op in the stretch answer (none when it is its straight stretch). It goes back there when it
/// is given up.
pub(super) struct Back {
    pieces: Vec<RItem>,
    op: Option<BoardOp>,
}

/// A member laid: its track or via, and the pieces of its new segments or of the via where it
/// is now.
struct Done {
    id: ItemId,
    what: What,
    pieces: Vec<RItem>,
}

/// What a member laid is.
enum What {
    /// A track laid again: a neighbour pushed, an attached track, or one a via carries.
    Track { track: Track, neighbour: bool, carried: bool },
    /// A via pushed by `by`.
    Via { by: P },
}

/// A member given up: cut back (`kept`; `None` removed) because of `why`, or left where it is.
enum Given {
    Cut { track: Track, kept: Option<Track>, why: Reason },
    Kept,
}

/// What a wave did, for the benchmark and the hand-in.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub struct WaveStats {
    /// Tracks that entered the wave.
    pub members: usize,
    /// Steps taken, each one way of laying a member, or one run of the stretch's way.
    pub steps: usize,
    /// Embeds (`make_legal` calls) of its own; the stretch's way of laying an attached track
    /// counts one.
    pub embeds: usize,
    /// Order repairs, each a roll back.
    pub repairs: usize,
    /// Neighbours laid again (pushed), not those that stay where they are.
    pub pushed: usize,
    /// Neighbours given up, left where they are, and attached tracks that joined given up, back
    /// where the stretch laid them.
    pub kept: usize,
    /// Attached tracks laid again.
    pub relaid: usize,
    /// Attached tracks given up, cut back.
    pub cut: usize,
    /// Spikes put in references (moved parts whose travel crossed a track).
    pub spikes: usize,
    /// Tracks and vias that joined the wave, in the way of a member (rank 2 and up).
    pub joined: usize,
    /// Vias pushed, and vias given up (back where they were with their tracks).
    pub vias: usize,
    pub vias_kept: usize,
    /// Where the steps' time went.
    pub spent: Spent,
}

/// Where a wave's steps spend their time (nanoseconds; the parts overlap as noted).
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub struct Spent {
    /// Laying members (their embeds and the `verify` of what they lay are part of it).
    pub lay_ns: u64,
    /// Embeds: obstacles, spikes, `make_legal`, `tighten` and the local bump, or the stretch's
    /// way of laying an attached track.
    pub embed_ns: u64,
    /// Every `verify` of the wave.
    pub verify_ns: u64,
    /// Cutting given-up attached tracks back.
    pub cut_ns: u64,
    /// Rebuilding the wave's world: roll backs and the start a blocked member is laid in alone.
    pub rebuild_ns: u64,
    /// Hulls built (not found in the cache), and the time that took (part of the embeds').
    pub hulls: usize,
    pub hull_ns: u64,
    /// Of those, the hulls of pieces the wave's world added (laid tracks).
    pub hulls_added: usize,
}

/// Nanoseconds since `since`.
fn ns(since: Instant) -> u64 {
    since.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

/// Whether a wave is still wanted, asked between the parts of its steps: once a newer sample
/// waits, whose answer drops the wave anyway, the step stops at its next part and the wave is
/// dropped unanswered (`head::shove_step_until`), so the sample waits only for the part in hand.
pub(super) struct Wanted<'a> {
    stale: &'a dyn Fn() -> bool,
    gone: Cell<bool>,
}

impl<'a> Wanted<'a> {
    pub(super) fn new(stale: &'a dyn Fn() -> bool) -> Wanted<'a> {
        Wanted { stale, gone: Cell::new(false) }
    }

    /// Whether the wave is no longer wanted; once it is not, it never is again.
    pub(super) fn gone(&self) -> bool {
        if !self.gone.get() && (self.stale)() {
            self.gone.set(true);
        }
        self.gone.get()
    }
}

/// A part or via drag's shove wave, parked in its gesture's state between steps. It borrows
/// nothing: each step sees the worker's world through its overlay.
pub struct Wave {
    generation: u64,
    sample: (GestureId, u64),
    started: Instant,
    /// The moves and the attached tracks the stretch laid clean.
    ops: Vec<BoardOp>,
    /// The moved items' collisions, as the stretch found them.
    collisions: Vec<Collision>,
    /// The pushers: their pieces are the first the start adds.
    travel: Movers,
    /// The world before any member is laid, with what was given up in it.
    start: Overlay,
    /// The start with the members laid so far.
    over: Overlay,
    members: Vec<Member>,
    done: Vec<Done>,
    /// Recorded orders: (a, b) when a must be laid before b.
    pairs: BTreeSet<(ItemId, ItemId)>,
    given: Vec<(usize, Given)>,
    /// How far the next member's lay is.
    attempt: Attempt,
    /// Every track that entered the wave: each enters once.
    entered: BTreeSet<ItemId>,
    /// The attached tracks the stretch laid, by id, with their moved ends: each may join (its
    /// windows are its moved ends').
    laid: Vec<(ItemId, [Option<P>; 2])>,
    /// A segment drag's or a drawn route's: its pushes are all or none.
    walk: Option<Walk>,
    stats: WaveStats,
}

/// A wave whose pushes are all or none: its pusher, and, once a track it pushes is given up
/// (that track's net and why), the walk laid instead (no track pushed).
pub(super) struct Walk {
    pusher: Pusher,
    given: Option<(NetId, Reason)>,
}

/// What pushes, and its walk once laid.
enum Pusher {
    /// A segment drag (`drag`): the dragged track as tier 1 laid it; its walk is the window laid
    /// round everything as it is, or where and why that is blocked.
    Drag { drag: Dragged, pushed: Track, laid: Option<Result<(Track, Vec<Collision>), Blocked>> },
    /// A route drawn in Shove mode (`push`): the sample and the head as tier 1 laid it; its walk is
    /// the sample's Walk-mode route (`None` when the gesture names what the board does not have).
    Head { gesture: Gesture, head: HeadAnswer, laid: Option<Option<Routed>> },
}

impl Walk {
    pub(super) fn drag(drag: Dragged, pushed: Track) -> Walk {
        Walk { pusher: Pusher::Drag { drag, pushed, laid: None }, given: None }
    }

    pub(super) fn head(gesture: Gesture, head: HeadAnswer) -> Walk {
        Walk { pusher: Pusher::Head { gesture, head, laid: None }, given: None }
    }

    fn laid(&self) -> bool {
        match &self.pusher {
            Pusher::Drag { laid, .. } => laid.is_some(),
            Pusher::Head { laid, .. } => laid.is_some(),
        }
    }
}

/// The pushers, whose pieces are the first a wave's start adds: moved items, each piece moved
/// straight by its travel (part, via and segment drags); or a drawn route, its pieces drawn along
/// its runs, each run the route's points on one layer from where it came onto that layer (the
/// route head's Shove mode).
pub(super) enum Movers {
    Moved(Vec<P>),
    Drawn { count: usize, runs: Vec<(LayerId, Vec<P>)> },
}

impl Movers {
    /// How many pieces push.
    fn count(&self) -> usize {
        match self {
            Movers::Moved(by_piece) => by_piece.len(),
            Movers::Drawn { count, .. } => *count,
        }
    }

    /// How far a moved piece came, in its larger coordinate (a drawn route's pieces are where
    /// they were drawn: 0).
    fn reach(&self) -> i64 {
        match self {
            Movers::Moved(by_piece) => by_piece.iter().map(|by| (by.x as i64).abs().max((by.y as i64).abs())).max().unwrap_or(0),
            Movers::Drawn { .. } => 0,
        }
    }
}

/// How far the next member's lay is: each step takes one way.
#[derive(Clone)]
enum Attempt {
    /// In the wave's world.
    Here(Way),
    /// In its start alone, after it was blocked in the wave's world for `Reason`; given up as soon
    /// as it is blocked there when it is `stuck` (`Wave::stuck`).
    Alone(Way, Reason, bool),
    /// Blocked by what stays (`why`): in its start without the shovable tracks near it that have
    /// not entered the wave (`over`), those being `room`; by the stretch's way too when `relay`
    /// (the copper it was blocked with met one of them).
    Free { way: Way, why: Reason, over: Overlay, room: Vec<ItemId>, relay: bool },
}

/// A way of laying a member: a neighbour's windows, or an attached track's transport, then the
/// stretch's way, one of its runs per step.
#[derive(Clone)]
enum Way {
    Transport,
    Relay(stretch::Relay),
}

/// What one step of laying a member found.
enum Laid {
    Done(Done),
    /// Not laid, for `Reason`: and the items the copper it was blocked with met.
    Blocked(Reason, Vec<ItemId>),
    /// Not decided yet: the way to go on with.
    Next(Way),
}

impl Wave {
    pub(super) fn new(
        generation: u64, sample: (GestureId, u64), started: Instant, ops: Vec<BoardOp>, collisions: Vec<Collision>,
        travel: Movers, start: Overlay, mut members: Vec<Member>, laid: Vec<(ItemId, [Option<P>; 2])>,
        walk: Option<Walk>,
    ) -> Wave {
        members.sort_by_key(|member| (member.contact, member.order));
        let stats = WaveStats { members: members.len(), ..WaveStats::default() };
        let entered = members.iter().map(Member::id).collect();
        // A via is laid before the tracks it carries.
        let pairs = members.iter().filter_map(|m| match &m.kind {
            Kind::Carried { track, via, .. } => Some((*via, track.id)),
            _ => None,
        }).collect();
        let mut wave = Wave {
            generation, sample, started, ops, collisions, travel, over: start.clone(), start, members,
            done: Vec::new(), pairs, given: Vec::new(), attempt: Attempt::Here(Way::Transport), entered, laid, walk, stats,
        };
        wave.keep_pairs(0);
        wave
    }

    /// The wave with `cut` given up before it starts: attached tracks the stretch cut back for
    /// good (`kept`; `None` removed), its Full answer cutting them as the stretch did, for `why`.
    /// Its start has them as kept; they count as entered, so none joins or is carried. O(cut).
    pub(super) fn with_cut(mut self, board: &Board, cut: impl Iterator<Item = (Track, Option<Track>, Reason)>) -> Wave {
        for (track, kept, why) in cut {
            let order = board_index(board, ItemKind::Track, track.id).unwrap_or(usize::MAX);
            self.entered.insert(track.id);
            self.given.push((order, Given::Cut { track, kept, why }));
        }
        self
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn stats(&self) -> WaveStats {
        self.stats
    }

    /// Take one way of laying the next member; lay it, repair the order or give one up when that
    /// decides it. True when every member is laid or given up. It stops between its parts once the
    /// wave is not `wanted`, and the wave is then dropped.
    pub(super) fn step(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, wanted: &Wanted) -> bool {
        let (built, built_ns, added) = (hulls.built(), hulls.built_ns(), hulls.built_added());
        let done = self.step_in(world, board, rules, hulls, wanted);
        if done && !wanted.gone() && self.walk.as_ref().is_none_or(|walk| walk.given.is_none()) {
            self.settle(world, board, rules, wanted);
        }
        self.stats.spent.hulls += hulls.built() - built;
        self.stats.spent.hull_ns += hulls.built_ns() - built_ns;
        self.stats.spent.hulls_added += hulls.built_added() - added;
        done
    }

    /// The wave decided: each track the stretch laid that joined it, laid again, goes back to where
    /// the stretch laid it when that copper is legal on the wave's board (what it made room for
    /// went elsewhere, or was given up). Laid again while the wave went on, it left room for the
    /// members laid after it; here nothing is laid after it. One verify per such track, and the
    /// overlay rebuilt for each that goes back: O(joined × pieces added). Then each pushed via goes
    /// back to the base, with the tracks it carries, the same way.
    fn settle(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, wanted: &Wanted) {
        let mut current = World::with(world, std::mem::take(&mut self.over));
        for (member, done) in self.members.iter().zip(self.done.iter_mut()) {
            if wanted.gone() {
                break;
            }
            let Kind::Attached { track, ends, back: Some(back), .. } = &member.kind else { continue };
            if done.pieces == back.pieces {
                continue;
            }
            let t = Instant::now();
            let laid = current.remove_added(track.id);
            let legal = verify(&current, rules, board, &back.pieces).is_empty();
            self.stats.spent.verify_ns += ns(t);
            if !legal {
                for piece in laid {
                    current.add(piece);
                }
                continue;
            }
            for piece in &back.pieces {
                current.add(piece.clone());
            }
            let stretched = match &back.op {
                Some(BoardOp::Replace(Item::Track(laid))) => laid.clone(),
                _ => Track { path: stretch::straight(&track.path, *ends), ..track.clone() },
            };
            *done = Done { id: track.id, what: What::Track { track: stretched, neighbour: false, carried: false }, pieces: back.pieces.clone() };
        }
        // Then each via the wave pushed goes back to the base with the tracks it carries when that
        // copper is legal on the wave's board: what pushed it was laid again elsewhere after it
        // joined, so the push is no longer needed. One verify per pushed via, and the overlay
        // rebuilt for each: O(pushed vias × the group's pieces).
        for k in 0..self.members.len() {
            if wanted.gone() {
                break;
            }
            let Kind::Via { via, carried } = &self.members[k].kind else { continue };
            if matches!(self.done[k].what, What::Via { by } if by == P::ZERO) {
                continue;
            }
            let ids: Vec<ItemId> = std::iter::once(via.id).chain(carried.iter().copied()).collect();
            let back: Vec<RId> = ids.iter().flat_map(|&id| world.pieces(id)).copied().collect();
            let pieces: Vec<RItem> = back.iter().filter_map(|&rid| world.get(rid)).cloned().collect();
            let t = Instant::now();
            let laid = current.remove_added_many(&ids);
            let legal = verify(&current, rules, board, &pieces).is_empty();
            self.stats.spent.verify_ns += ns(t);
            if !legal {
                for piece in laid {
                    current.add(piece);
                }
                continue;
            }
            current.restore_many(back);
            for (member, done) in self.members.iter().zip(self.done.iter_mut()).skip(k) {
                let id = member.id();
                if !ids.contains(&id) {
                    continue;
                }
                let own = world.pieces(id).iter().filter_map(|&rid| world.get(rid)).cloned().collect();
                *done = match &member.kind {
                    Kind::Via { .. } => Done { id, what: What::Via { by: P::ZERO }, pieces: own },
                    Kind::Carried { track, .. } => Done { id, what: What::Track { track: track.clone(), neighbour: false, carried: true }, pieces: own },
                    _ => continue,
                };
            }
        }
        self.over = current.into_overlay();
    }

    fn step_in(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, wanted: &Wanted) -> bool {
        if wanted.gone() {
            return false;
        }
        // A segment drag or a drawn route that could not push a track walks: its last step.
        if let Some(walk) = self.walk.as_mut().filter(|walk| walk.given.is_some()) {
            self.stats.steps += 1;
            self.stats.embeds += 1;
            let t = Instant::now();
            match &mut walk.pusher {
                Pusher::Drag { drag, laid, .. } => {
                    let mut over = World::new(world);
                    over.remove_item(drag.track.id);
                    *laid = Some(drag::walk(&over, board, rules, hulls, &mut 0, drag));
                }
                Pusher::Head { gesture, laid, .. } => *laid = Some(super::walked(world, board, rules, hulls, gesture)),
            }
            self.stats.spent.lay_ns += ns(t);
            return true;
        }
        let i = self.done.len();
        if i == self.members.len() {
            return true;
        }
        self.stats.steps += 1;
        self.carry(board, i);
        if matches!(self.attempt, Attempt::Here(Way::Transport)) {
            if let Some(stuck) = self.stuck(world, board, rules, i) {
                self.attempt = stuck;
            }
        }
        match std::mem::replace(&mut self.attempt, Attempt::Here(Way::Transport)) {
            Attempt::Here(way) => {
                let current = World::with(world, std::mem::take(&mut self.over));
                let t = Instant::now();
                let tried = lay(&current, board, rules, hulls, &self.members[i], way, &self.travel, &mut self.stats, wanted);
                self.stats.spent.lay_ns += ns(t);
                self.over = current.into_overlay();
                if wanted.gone() {
                    return false;
                }
                match tried {
                    Laid::Done(done) => self.push(world, done),
                    Laid::Next(way) => self.attempt = Attempt::Here(way),
                    // Against the start alone next: blocked by members laid before it, or by what
                    // stays. With none laid yet, the start is the world it just failed in.
                    Laid::Blocked(why, met) if self.done.is_empty() => self.blocked(world, board, rules, i, why, &met, wanted),
                    Laid::Blocked(why, _) => self.attempt = Attempt::Alone(Way::Transport, why, false),
                }
            }
            Attempt::Alone(way, why, stuck) => {
                let t = Instant::now();
                let alone = World::with(world, self.start.clone());
                self.stats.spent.rebuild_ns += ns(t);
                let t = Instant::now();
                let tried = lay(&alone, board, rules, hulls, &self.members[i], way, &self.travel, &mut self.stats, wanted);
                self.stats.spent.lay_ns += ns(t);
                if wanted.gone() {
                    return false;
                }
                match tried {
                    Laid::Done(free) => self.place(world, board, rules, i, free, why),
                    Laid::Next(way) => self.attempt = Attempt::Alone(way, why, stuck),
                    Laid::Blocked(why, _) if stuck => self.give_up(world, board, rules, i, why),
                    Laid::Blocked(why, met) => self.blocked(world, board, rules, i, why, &met, wanted),
                }
            }
            Attempt::Free { way, why, over, room, relay } => {
                let free = World::with(world, over);
                let t = Instant::now();
                let tried = lay(&free, board, rules, hulls, &self.members[i], way, &self.travel, &mut self.stats, wanted);
                self.stats.spent.lay_ns += ns(t);
                let over = free.into_overlay();
                if wanted.gone() {
                    return false;
                }
                match tried {
                    Laid::Done(laid) => self.join(world, board, rules, i, laid, why, &room),
                    Laid::Next(Way::Relay(_)) if !relay => self.give_up(world, board, rules, i, why),
                    Laid::Next(way) => self.attempt = Attempt::Free { way, why, over, room, relay },
                    Laid::Blocked(..) => self.give_up(world, board, rules, i, why),
                }
            }
        }
        self.decided()
    }

    /// Member `i` when it is an attached track that did not join, with a moved end in the
    /// clearance of something the wave never moves: a pad, the outline, a hole or a keepout, or a
    /// via that is not a member and that `pushable` refuses (the tracks taken only grow as the
    /// wave goes on, so it never takes it later). No world the wave tries it in can lay it, and
    /// every way ends in giving it up for why it is blocked against its start alone (in the
    /// wave's world when none is laid, which is the start then): its attempt, in its start alone
    /// by the stretch's way (its transport keeps that end, so it is never taken), given up as soon
    /// as it is blocked there. O(the pieces near its moved ends).
    fn stuck(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, i: usize) -> Option<Attempt> {
        let Kind::Attached { track, ends, windows, back: None } = &self.members[i].kind else { return None };
        let class = board.nets.get(track.net.ix()).map(|net| net.class).unwrap_or_default();
        let (entered, laid) = (&self.entered, &self.laid);
        let taken = |t: ItemId| entered.contains(&t) || laid.binary_search_by_key(&t, |(id, _)| *id).is_ok();
        let fixed = |it: &RItem| match &it.piece {
            RPiece::Copper(Copper { item: ConnItem::Via(via), .. }) | RPiece::Drill { item: ConnItem::Via(via), .. } => {
                !entered.contains(via) && via_of(board, *via).is_some_and(|v| pushable(board, world, v, taken).is_none())
            }
            RPiece::Edge(_) | RPiece::Keepout { .. } | RPiece::Drill { .. } => true,
            RPiece::Copper(Copper { item: ConnItem::Pad { .. } | ConnItem::Hole(_), .. }) => true,
            RPiece::Copper(_) => false,
        };
        // A piece of its copper at the end: inside the end segment's own, so what it meets that
        // one meets, wherever the rest of the track goes.
        let reach = rules.max_clearance().max(rules.hole_to_hole()).max(rules.edge()).max(1);
        let last = track.path.segs.len().saturating_sub(1);
        let start = World::with(world, std::mem::take(&mut self.start));
        let at_end = ends.iter().zip([0, last]).filter_map(|(end, seg)| end.map(|at| (at, seg))).find(|&(at, seg)| {
            let copper = Copper {
                item: ConnItem::Track { item: track.id, seg: seg as u32 }, net: Some(track.net), class, layer: track.layer,
                shape: Shape::Segment { a: at, b: at, w: track.seg_width(seg) },
            };
            start.query_sorted(track.layer, copper.shape.bbox().inflate(reach)).into_iter().any(|(_, it)| {
                it.item != Some(track.id) && fixed(it) && check_to(rules, &copper, &it.obstacle(), track.layer).is_some()
            })
        });
        let attempt = at_end.map(|(at, _)| Attempt::Alone(Way::Relay(stretch::Relay::new(windows)), unsettled(board, track, at), true));
        self.start = start.into_overlay();
        attempt
    }

    /// Member `i`, when a via carries it: its ends and windows from where that via is laid (the
    /// via is laid before it). O(its segments).
    fn carry(&mut self, board: &Board, i: usize) {
        let Kind::Carried { track, ends, windows, via } = &mut self.members[i].kind else { return };
        let Some(by) = self.done.iter().find_map(|d| match d.what {
            What::Via { by } if d.id == *via => Some(by),
            _ => None,
        }) else { return };
        let at = via_of(board, *via).map(|v| v.at.add(by));
        *ends = [0, 1].map(|e| (track.ends[e] == Anchor::Via(*via)).then_some(at).flatten());
        *windows = stretch::windows(track, *ends);
    }

    /// Every member laid or given up; for a segment drag that gave one up, its walk laid.
    fn decided(&self) -> bool {
        match &self.walk {
            Some(walk) if walk.given.is_some() => walk.laid(),
            _ => self.done.len() == self.members.len(),
        }
    }

    /// Member `i`, blocked in the wave's world (`why`), laid as `free` against its start alone:
    /// legal here too when it meets nothing laid since; else blocked by the earliest member laid
    /// that it meets, whose pair is recorded and laid after it (or, when the pairs already put
    /// that one first, a cycle: the later in board order given up).
    fn place(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, i: usize, free: Done, why: Reason) {
        let current = World::with(world, std::mem::take(&mut self.over));
        let t = Instant::now();
        let met = verify(&current, rules, board, &free.pieces);
        self.stats.spent.verify_ns += ns(t);
        self.over = current.into_overlay();
        let blocker = self.done.iter().position(|d| met.iter().any(|c| [&c.a, &c.b].contains(&&Sel::Item(board.unit, d.id))));
        let Some(j) = blocker else {
            self.push(world, free);
            return;
        };
        let (a, b) = (self.members[i].id(), self.members[j].id());
        if self.precedes(b, a) {
            if self.members[i].order > self.members[j].order {
                self.give_up(world, board, rules, i, why);
            } else {
                let why = met.first().map(stretch::no_room).unwrap_or(why);
                self.roll_back(world, j);
                self.give_up(world, board, rules, j, why);
            }
        } else {
            self.pairs.insert((a, b));
            let member = self.members.remove(i);
            self.members.insert(j, member);
            self.keep_pairs(j);
            self.roll_back(world, j);
            self.stats.repairs += 1;
        }
    }

    /// Whether the pairs recorded among the members chain `x` to `y`: `y` must be laid after `x`.
    /// O(members² × pairs).
    fn precedes(&self, x: ItemId, y: ItemId) -> bool {
        let mut reached = vec![x];
        let mut k = 0;
        while k < reached.len() {
            let from = reached[k];
            k += 1;
            for &(p, q) in &self.pairs {
                if p == from && !reached.contains(&q) && self.members.iter().any(|m| m.id() == q) {
                    if q == y {
                        return true;
                    }
                    reached.push(q);
                }
            }
        }
        false
    }

    /// The members from `j` on in the order nearest theirs that keeps every recorded pair: each
    /// next is the first whose recorded predecessors are all before it. The members before `j`
    /// keep them already. O(members³ × log pairs).
    fn keep_pairs(&mut self, j: usize) {
        let mut rest: Vec<Member> = self.members.drain(j..).collect();
        while !rest.is_empty() {
            let k = rest.iter().position(|m| !rest.iter().any(|o| self.pairs.contains(&(o.id(), m.id()))))
                .expect("the recorded pairs close no cycle");
            self.members.push(rest.remove(k));
        }
    }

    /// Member `i`, blocked by what stays (`why`, its copper meeting `met`): laid next against the
    /// start without the tracks and vias near it that have not entered the wave and can move (a
    /// shovable neighbour's windows round the segments there, an attached track's copper the
    /// stretch laid, a via that can be pushed), by the stretch's way too when `met` holds one of
    /// them; given up when there are none. O(the pieces near it, plus the pieces the start adds
    /// for each attached track, plus each via's tracks).
    fn blocked(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, i: usize, why: Reason, met: &[ItemId], wanted: &Wanted) {
        let reach = self.travel.reach() + rules.max_clearance() as i64;
        let (layers, area): (Vec<LayerId>, BBox) = match &self.members[i].kind {
            Kind::Attached { track, .. } | Kind::Carried { track, .. } | Kind::Neighbour { track, .. } => {
                (vec![track.layer], grow(BBox::of_points(stretch::points(track.path.start, &track.path.segs)), reach + track.w as i64))
            }
            Kind::Via { via, .. } => (via_layers(world, via.id), grow(BBox::of_points([via.at]), reach + via.pad as i64)),
        };
        let mut free = World::with(world, self.start.clone());
        let laid = |item: ItemId| self.laid.binary_search_by_key(&item, |(t, _)| *t).is_ok();
        let (mut near, mut attached, mut vias) = (Vec::new(), Vec::new(), Vec::new());
        for &layer in &layers {
            for (reference, it) in free.query_sorted(layer, area) {
                let item = match (&it.piece, reference) {
                    (RPiece::Copper(Copper { item: ConnItem::Via(via), .. }) | RPiece::Drill { item: ConnItem::Via(via), .. }, WRef::Base(_)) => {
                        if !self.entered.contains(via) { vias.push(*via); }
                        continue;
                    }
                    (RPiece::Copper(c), _) => c.item,
                    _ => continue,
                };
                let ConnItem::Track { item, seg } = item else { continue };
                match reference {
                    _ if self.entered.contains(&item) => {}
                    WRef::Base(_) if !laid(item) => near.push((item, seg as usize)),
                    WRef::Over(_) if laid(item) => attached.push(item),
                    _ => {}
                }
            }
        }
        near.sort_unstable();
        near.dedup();
        attached.sort_unstable();
        attached.dedup();
        vias.sort_unstable();
        vias.dedup();
        let mut room = Vec::new();
        for id in vias {
            if wanted.gone() {
                return;
            }
            let Some(via) = via_of(board, id) else { continue };
            if pushable(board, world, via, |t| self.entered.contains(&t) || laid(t)).is_some() {
                free.remove_item(id);
                room.push(id);
            }
        }
        for (id, segs) in by_item(&near) {
            if wanted.gone() {
                return;
            }
            let Some(other) = track_of(board, id) else { continue };
            if let Some(windows) = stretch::shovable(world, other, &segs) {
                stretch::take_out(&mut free, world, id, |seg| windows.iter().any(|w| w.contains(&seg)));
                room.push(id);
            }
        }
        if wanted.gone() {
            return;
        }
        free.remove_added_many(&attached);
        room.extend(attached);
        if room.is_empty() {
            self.give_up(world, board, rules, i, why);
        } else {
            let relay = room.iter().any(|id| met.contains(id));
            self.attempt = Attempt::Free { way: Way::Transport, why, over: free.into_overlay(), room, relay };
        }
    }

    /// Member `i`, blocked by what stays (`why`), laid as `laid` without the tracks and vias
    /// `room`: the ones its copper meets join the wave after its members (a neighbour's windows
    /// round the segments met, an attached track's copper the stretch laid, a via with the tracks
    /// it carries, out of its world), and it is laid again; placed as it is when it meets none;
    /// given up when a neighbour's cannot be shoved there.
    fn join(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, i: usize, laid: Done, why: Reason, room: &[ItemId]) {
        let start = World::with(world, self.start.clone());
        let t = Instant::now();
        let (mut met, mut met_vias): (Vec<(ItemId, usize)>, Vec<ItemId>) = (Vec::new(), Vec::new());
        for (_, reference) in hits(&start, rules, board, &laid.pieces) {
            match start.get(reference).map(|it| &it.piece) {
                Some(RPiece::Copper(Copper { item: ConnItem::Track { item, seg }, .. })) if room.contains(item) => met.push((*item, *seg as usize)),
                Some(RPiece::Copper(Copper { item: ConnItem::Via(via), .. }) | RPiece::Drill { item: ConnItem::Via(via), .. }) if room.contains(via) => met_vias.push(*via),
                _ => {}
            }
        }
        self.stats.spent.verify_ns += ns(t);
        met.sort_unstable();
        met.dedup();
        met_vias.sort_unstable();
        met_vias.dedup();
        if met.is_empty() && met_vias.is_empty() {
            self.place(world, board, rules, i, laid, why);
            return;
        }
        // The vias first: the tracks they carry follow them.
        let (mut groups, mut carried): (Vec<Vec<Member>>, Vec<ItemId>) = (Vec::new(), Vec::new());
        for id in met_vias {
            let via = via_of(board, id).expect("a via met is on the board");
            let taken = |t: ItemId| self.entered.contains(&t) || self.laid.binary_search_by_key(&t, |(l, _)| *l).is_ok() || carried.contains(&t);
            let Some(tracks) = pushable(board, world, via, taken) else {
                self.give_up(world, board, rules, i, why);
                return;
            };
            carried.extend(tracks.iter().map(|t| t.id));
            groups.push(via_members(board, via, tracks, i128::MAX));
        }
        let (mut joins, mut attached) = (Vec::new(), Vec::new());
        for (id, segs) in by_item(&met) {
            if carried.contains(&id) {
                continue;
            }
            if let Ok(k) = self.laid.binary_search_by_key(&id, |(t, _)| *t) {
                attached.push(k);
                continue;
            }
            let other = track_of(board, id).expect("a track met is on the board");
            let Some(windows) = stretch::shovable(world, other, &segs) else {
                self.give_up(world, board, rules, i, why);
                return;
            };
            joins.push((other.clone(), windows));
        }
        let mut start = World::with(world, std::mem::take(&mut self.start));
        let mut current = World::with(world, std::mem::take(&mut self.over));
        for (other, windows) in joins {
            let inside = |seg: usize| windows.iter().any(|w| w.contains(&seg));
            stretch::take_out(&mut start, world, other.id, inside);
            stretch::take_out(&mut current, world, other.id, inside);
            self.entered.insert(other.id);
            self.members.push(Member::new(board, Kind::Neighbour { track: other, windows }, i128::MAX));
            self.stats.members += 1;
            self.stats.joined += 1;
        }
        // Their copper out of both worlds at once (one rebuild each).
        let ids: Vec<ItemId> = attached.iter().map(|&k| self.laid[k].0).collect();
        let mut added = start.remove_added_many(&ids);
        current.remove_added_many(&ids);
        for k in attached {
            let (id, ends) = self.laid[k];
            let Some((ItemKind::Track, at)) = board.locate(id) else { unreachable!("a laid track is on the board") };
            let track = board.tracks[at].clone();
            let windows = stretch::windows(&track, ends);
            let (pieces, rest) = added.into_iter().partition(|it| it.item == Some(id));
            added = rest;
            let op = self.ops.iter().position(|op| matches!(op, BoardOp::Replace(Item::Track(t)) if t.id == track.id)).map(|k| self.ops.remove(k));
            self.entered.insert(track.id);
            self.members.push(Member::new(board, Kind::Attached { track, ends, windows, back: Some(Back { pieces, op }) }, 0));
            self.stats.members += 1;
            self.stats.joined += 1;
        }
        for group in groups {
            for member in group {
                take_out_member(&mut start, world, &member);
                take_out_member(&mut current, world, &member);
                self.entered.insert(member.id());
                if let Some(via) = member.group().filter(|&via| via != member.id()) {
                    self.pairs.insert((via, member.id()));
                }
                self.stats.members += 1;
                self.members.push(member);
            }
            self.stats.joined += 1;
        }
        self.start = start.into_overlay();
        self.over = current.into_overlay();
    }

    fn push(&mut self, world: &RBase, done: Done) {
        let mut current = World::with(world, std::mem::take(&mut self.over));
        for piece in &done.pieces {
            current.add(piece.clone());
        }
        self.over = current.into_overlay();
        self.done.push(done);
        self.attempt = Attempt::Here(Way::Transport);
    }

    /// Back to just before member `j`: the wave's world rebuilt from its start. O(pieces laid).
    fn roll_back(&mut self, world: &RBase, j: usize) {
        let t = Instant::now();
        self.attempt = Attempt::Here(Way::Transport);
        self.done.truncate(j);
        let mut current = World::with(world, self.start.clone());
        for done in &self.done {
            for piece in &done.pieces {
                current.add(piece.clone());
            }
        }
        self.over = current.into_overlay();
        self.stats.spent.rebuild_ns += ns(t);
    }

    /// Member `i`, the next to lay, given up for good: an attached track cut back against what is
    /// laid; a neighbour's base copper, or the copper the stretch laid for an attached track that
    /// joined, back in the world, and the wave rolled back to before the first member laid where
    /// it is; a via, or a track it carries, back with the via's whole group (`give_up_via`).
    fn give_up(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, i: usize, why: Reason) {
        debug_assert_eq!(self.done.len(), i, "the member given up is the next one to lay");
        let member = self.members.remove(i);
        if let Some(walk) = self.walk.as_mut() {
            walk.given.get_or_insert_with(|| (member.net(), why.clone()));
        }
        match member.kind {
            Kind::Via { via, carried } => self.give_up_via(world, board, rules, via.id, (i, member.order), &carried),
            Kind::Carried { via, .. } => {
                let (first, order, carried) = self.members.iter().enumerate().find_map(|(k, m)| match &m.kind {
                    Kind::Via { via: v, carried } if v.id == via => Some((k, m.order, carried.clone())),
                    _ => None,
                }).expect("a carried track's via is laid before it");
                self.give_up_via(world, board, rules, via, (first, order), &carried);
            }
            Kind::Attached { back: Some(back), .. } => {
                let current = World::with(world, std::mem::take(&mut self.over));
                let t = Instant::now();
                let met = verify(&current, rules, board, &back.pieces);
                self.stats.spent.verify_ns += ns(t);
                self.over = current.into_overlay();
                let mut start = World::with(world, std::mem::take(&mut self.start));
                for piece in &back.pieces {
                    start.add(piece.clone());
                }
                self.ops.extend(back.op);
                self.start = start.into_overlay();
                self.stats.kept += 1;
                self.given.push((member.order, Given::Kept));
                let j = self.first_meeting(board, &met);
                self.roll_back(world, j);
            }
            Kind::Attached { track, ends, windows, back: None } => {
                // Cut against what is laid, and against what goes back when given up later (also
                // after a roll back): every neighbour's base windows, the copper the stretch laid
                // for every attached track that joined, and every via with its tracks.
                let mut current = World::with(world, self.over.clone());
                for m in &self.members {
                    match &m.kind {
                        Kind::Neighbour { track, windows } => {
                            for rid in base_windows(world, track.id, windows) {
                                current.restore(rid);
                            }
                        }
                        Kind::Attached { back: Some(back), .. } => {
                            for piece in &back.pieces {
                                current.add(piece.clone());
                            }
                        }
                        Kind::Via { via, carried } => {
                            for &rid in std::iter::once(&via.id).chain(carried).flat_map(|&id| world.pieces(id)) {
                                current.restore(rid);
                            }
                        }
                        Kind::Attached { back: None, .. } | Kind::Carried { .. } => {}
                    }
                }
                let t = Instant::now();
                let kept = stretch::cut_back(&current, board, rules, &track, ends, &windows, &[]);
                self.stats.spent.cut_ns += ns(t);
                let mut start = World::with(world, std::mem::take(&mut self.start));
                start.remove_item(track.id);
                for piece in kept.iter().flat_map(|kept| stretch::cut_pieces(world, board, &track, ends, kept)) {
                    start.add(piece);
                }
                self.stats.cut += 1;
                self.given.push((member.order, Given::Cut { track, kept, why }));
                self.start = start.into_overlay();
                let j = self.done.len();
                self.roll_back(world, j);
            }
            Kind::Neighbour { track, windows } => {
                let back = base_windows(world, track.id, &windows);
                let pieces: Vec<RItem> = back.iter().filter_map(|&rid| world.get(rid)).cloned().collect();
                let current = World::with(world, std::mem::take(&mut self.over));
                let t = Instant::now();
                let met = verify(&current, rules, board, &pieces);
                self.stats.spent.verify_ns += ns(t);
                self.over = current.into_overlay();
                let mut start = World::with(world, std::mem::take(&mut self.start));
                for rid in back {
                    start.restore(rid);
                }
                self.start = start.into_overlay();
                self.stats.kept += 1;
                self.given.push((member.order, Given::Kept));
                let j = self.first_meeting(board, &met);
                self.roll_back(world, j);
            }
        }
    }

    /// Via `via` (member `first`, `order`) given up with the tracks it carries (`carried`), itself
    /// or one of them not laid: all go back where they were, in the wave's start, and the wave
    /// rolls back to before the via and before the first member laid that they meet, so whatever
    /// pushed the via meets it again. Its tracks are never cut. O(the group's base pieces and
    /// their `verify`, and the roll back).
    fn give_up_via(&mut self, world: &RBase, board: &Board, rules: &RuleMatrix, via: ItemId, (first, order): (usize, usize), carried: &[ItemId]) {
        let back: Vec<RId> = std::iter::once(&via).chain(carried).flat_map(|&id| world.pieces(id)).copied().collect();
        let pieces: Vec<RItem> = back.iter().filter_map(|&rid| world.get(rid)).cloned().collect();
        let current = World::with(world, std::mem::take(&mut self.over));
        let t = Instant::now();
        let met = verify(&current, rules, board, &pieces);
        self.stats.spent.verify_ns += ns(t);
        self.over = current.into_overlay();
        let mut start = World::with(world, std::mem::take(&mut self.start));
        for &rid in &back {
            start.restore(rid);
        }
        self.start = start.into_overlay();
        let j = self.first_meeting(board, &met).min(first);
        self.roll_back(world, j);
        self.members.retain(|m| m.group() != Some(via));
        self.stats.vias_kept += 1;
        self.given.push((order, Given::Kept));
    }

    /// The first member laid that `met` names, else how many are laid.
    fn first_meeting(&self, board: &Board, met: &[Collision]) -> usize {
        self.done.iter().position(|d| met.iter().any(|c| [&c.a, &c.b].contains(&&Sel::Item(board.unit, d.id)))).unwrap_or(self.done.len())
    }

    /// The Full answer of a finished wave, for the same sample as its stretch answer.
    pub(super) fn answer(self, base: &Base, world: &RBase, hulls: &mut HullCache, id: ProposalId) -> (RouteProposal, WaveStats) {
        let board = &base.board;
        let Wave { generation, sample, started, mut ops, collisions, done, mut given, mut stats, walk, over, .. } = self;
        let source = ProposalSource::Gesture { id: sample.0, seq: sample.1, tier: Tier::Full };
        // A segment drag or a drawn route that could not push a track walks round them all as
        // they are.
        let walk = match walk {
            Some(Walk { pusher: Pusher::Drag { drag, laid: Some(laid), .. }, given: Some(why) }) => {
                let answer = match laid {
                    Ok((moved, collisions)) => {
                        let head = drag::shown(&moved, None, Some(why));
                        (vec![BoardOp::Replace(Item::Track(moved))], head, collisions)
                    }
                    Err(blocked) => (Vec::new(), drag::shown(&drag.track, Some(blocked), Some(why)), Vec::new()),
                };
                return (drag::proposal(base, source, id, started, answer, true), stats);
            }
            Some(Walk { pusher: Pusher::Head { head, laid: Some(routed), .. }, given: Some(why) }) => {
                let answer = match routed {
                    Some(routed) => (
                        routed.items.into_iter().map(BoardOp::Add).collect(), routed.collisions,
                        HeadAnswer { unpushed: Some(why), ..routed.head },
                    ),
                    None => {
                        let blocked = Blocked { at: head.path.start, cause: BlockCause::Unsettled, have: 0, need: 0 };
                        (Vec::new(), Vec::new(), HeadAnswer { blocked: Some(blocked), unpushed: Some(why), ..head })
                    }
                };
                return (super::proposal(base, source, id, started, answer, true), stats);
            }
            walk => walk,
        };
        // The vias pushed first: the board's `Move` drags their tracks' ends straight, and the
        // tracks' `Replace`s then lay them.
        let mut pushed = BTreeSet::new();
        for d in &done {
            if let What::Via { by } = d.what {
                pushed.insert(d.id);
                stats.vias += 1;
                if by != P::ZERO {
                    ops.push(BoardOp::Move { item: d.id, by });
                }
            }
        }
        let mut laid: Vec<(&Track, bool, bool)> = done.iter().filter_map(|d| match &d.what {
            What::Track { track, neighbour, carried } => Some((track, *neighbour, *carried)),
            What::Via { .. } => None,
        }).collect();
        laid.sort_by_key(|(track, ..)| board_index(board, ItemKind::Track, track.id));
        for (track, neighbour, carried) in laid {
            let Some(base_track) = track_of(board, track.id) else { continue };
            if neighbour || carried {
                pushed.insert(track.id);
            }
            if !neighbour {
                stats.relaid += 1;
            } else if track.path != base_track.path {
                stats.pushed += 1;
            }
            if track.path != base_track.path {
                ops.push(BoardOp::Replace(Item::Track(track.clone())));
            }
        }
        given.sort_by_key(|(order, _)| *order);
        let mut open = Vec::new();
        for (_, g) in &given {
            if let Given::Cut { track, kept, why } = g {
                open.push((track.net, why.clone()));
                ops.push(match kept {
                    Some(kept) => BoardOp::Replace(Item::Track(kept.clone())),
                    None => BoardOp::Remove(track.id),
                });
            }
        }
        let collisions: Vec<Collision> = collisions.into_iter()
            .filter(|c| !pushed.iter().any(|id| [&c.a, &c.b].contains(&&Sel::Item(board.unit, *id))))
            .collect();
        // A segment drag's or a drawn route's pushes: the tracks they push have no other track's
        // end on them, and the pusher keeps its own as its tier 1 answer does.
        if let Some(walk) = walk {
            let proposal = match walk.pusher {
                Pusher::Drag { pushed, .. } => drag::proposal(base, source, id, started, (ops, drag::shown(&pushed, None, None), collisions), true),
                Pusher::Head { head, .. } => super::proposal(base, source, id, started, (ops, collisions, head), true),
            };
            return (proposal, stats);
        }
        let mut over = World::with(world, over);
        let mut queries = 0;
        let (cuts, unjoined) = stretch::keep_junctions(base, &mut over, hulls, &mut queries, &mut ops);
        open.extend(cuts);
        open.extend(unjoined);
        open.extend(stretch::sharp_corners(board, &ops));
        let proposal = RouteProposal {
            id, source,
            unit: base.unit, base_generation: generation, base_hash: base.hash,
            metrics: RouteMetrics::default(), ops, checked: None, verified: collisions.is_empty(), collisions,
            open, head: None, solve_us: started.elapsed().as_micros().min(u32::MAX as u128) as u32,
            last: true, label: "Move".into(),
        };
        (proposal, stats)
    }
}

/// The tracks via `via` carries when the wave may push it: those with an end on it. `None` when it
/// stays where it is: locked or a block's; one of its tracks cannot follow it (`taken` names it,
/// as the wave or the answer has it already, or it is locked, a block's, empty, with widths per
/// segment, with both ends or another track's end on it); or other copper of its net touches it
/// (it sits in a pad, a track ends on it unanchored, another via meets it), which a push would
/// leave behind. A pour refills round it. O(its tracks and the pieces round it): the tracks on it
/// and on each of its tracks come from the world (`RBase::tracks_on`), not from a board scan.
pub(super) fn pushable(board: &Board, world: &RBase, via: &Via, taken: impl Fn(ItemId) -> bool) -> Option<Vec<Track>> {
    let movable = |origin: &Origin| matches!(origin, Origin::Authored | Origin::Solved { .. } | Origin::Block { detached: true, .. });
    if via.locked || !movable(&via.origin) {
        return None;
    }
    let on = |end: &Anchor| *end == Anchor::Via(via.id);
    // In board order, as the wave enters them.
    let mut at: Vec<usize> = world.tracks_on(via.id).iter()
        .filter_map(|&id| match board.locate(id) { Some((ItemKind::Track, i)) => Some(i), _ => None }).collect();
    at.sort_unstable();
    let carried: Vec<Track> = at.into_iter().map(|i| board.tracks[i].clone()).collect();
    for track in &carried {
        if taken(track.id) || track.locked || !movable(&track.origin) || track.widths.is_some() || track.path.segs.is_empty() || track.ends.iter().all(on) {
            return None;
        }
        // A track's end on it (a junction) holds it.
        if !world.tracks_on(track.id).is_empty() {
            return None;
        }
    }
    let mut touched = false;
    for &rid in world.pieces(via.id) {
        let Some(RItem { piece: RPiece::Copper(ring), .. }) = world.get(rid) else { continue };
        world.query(ring.layer, ring.shape.bbox(), |_, it| {
            let RPiece::Copper(other) = &it.piece else { return ControlFlow::Continue(()) };
            let held = other.net == Some(via.net) && match other.item {
                ConnItem::Pad { .. } | ConnItem::Hole(_) => true,
                ConnItem::Via(id) => id != via.id,
                ConnItem::Track { item, .. } => !carried.iter().any(|t| t.id == item),
                ConnItem::Zone { .. } => false,
            };
            touched = held && clearance(&ring.shape, &other.shape) == 0;
            if touched { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        if touched {
            return None;
        }
    }
    Some(carried)
}

/// What a pusher's copper meets in the base: a track's segment, or a via (its ring or drill).
pub(super) enum Hit {
    Track(ItemId, usize),
    Via(ItemId),
}

impl Hit {
    /// What base piece `reference` is, when it is a track's copper or a via's; `None` for anything
    /// else, and for what the world added.
    pub(super) fn of(world: &RBase, reference: WRef) -> Option<Hit> {
        let WRef::Base(rid) = reference else { return None };
        match &world.get(rid)?.piece {
            RPiece::Copper(Copper { item: ConnItem::Track { item, seg }, .. }) => Some(Hit::Track(*item, *seg as usize)),
            RPiece::Copper(Copper { item: ConnItem::Via(via), .. }) | RPiece::Drill { item: ConnItem::Via(via), .. } => Some(Hit::Via(*via)),
            _ => None,
        }
    }
}

/// The vias on `layer` in `area` that a wave may push, taken out of `fixed`: what a pusher is laid
/// round leaves them out, as it leaves out the tracks that can be shoved. `taken` names the tracks
/// they may not carry. O(the pieces there, and each via's tracks).
pub(super) fn free_vias(fixed: &mut World, world: &RBase, board: &Board, layer: LayerId, area: BBox, taken: impl Fn(ItemId) -> bool) {
    let mut vias: Vec<ItemId> = fixed.query_sorted(layer, area).into_iter().filter_map(|(reference, _)| match Hit::of(world, reference) {
        Some(Hit::Via(via)) => Some(via),
        _ => None,
    }).collect();
    vias.dedup();
    for id in vias {
        if via_of(board, id).is_some_and(|via| pushable(board, world, via, &taken).is_some()) {
            fixed.remove_item(id);
        }
    }
}

/// Via `via` and the tracks it carries as wave members at first contact `contact`, the via first:
/// each track's ends and windows as from where the via is (they are laid from where it is laid).
pub(super) fn via_members(board: &Board, via: &Via, carried: Vec<Track>, contact: i128) -> Vec<Member> {
    let ids = carried.iter().map(|t| t.id).collect();
    let mut members = vec![Member::new(board, Kind::Via { via: via.clone(), carried: ids }, contact)];
    for track in carried {
        let ends = [0, 1].map(|e| (track.ends[e] == Anchor::Via(via.id)).then_some(via.at));
        let windows = stretch::windows(&track, ends);
        members.push(Member::new(board, Kind::Carried { track, ends, windows, via: via.id }, contact));
    }
    members
}

/// Where board item `id` of `kind` is in its list, by the board's index. O(log n).
fn board_index(board: &Board, kind: ItemKind, id: ItemId) -> Option<usize> {
    board.locate(id).filter(|(k, _)| *k == kind).map(|(_, i)| i)
}

/// Track `id` of `board`. O(log n).
fn track_of(board: &Board, id: ItemId) -> Option<&Track> {
    board_index(board, ItemKind::Track, id).map(|i| &board.tracks[i])
}

/// Via `id` of `board`. O(log n).
fn via_of(board: &Board, id: ItemId) -> Option<&Via> {
    board_index(board, ItemKind::Via, id).map(|i| &board.vias[i])
}

/// Member `member`'s base copper that the wave lays again taken out of `over`: a neighbour's
/// windows, a via's pieces, the segments of a track it carries that change with it. An attached
/// track's are taken out where the stretch lays it. O(its pieces).
pub(super) fn take_out_member(over: &mut World, world: &RBase, member: &Member) {
    match &member.kind {
        Kind::Neighbour { track, windows } => stretch::take_out(over, world, track.id, |seg| windows.iter().any(|w| w.contains(&seg))),
        Kind::Via { via, .. } => over.remove_item(via.id),
        Kind::Carried { track, ends, windows, .. } => stretch::take_out(over, world, track.id, |seg| stretch::changed(track, *ends, windows, seg)),
        Kind::Attached { .. } => {}
    }
}

/// The copper layers via `id`'s pieces are on. O(its pieces × layers).
fn via_layers(world: &RBase, id: ItemId) -> Vec<LayerId> {
    let pieces: Vec<&RItem> = world.pieces(id).iter().filter_map(|&rid| world.get(rid)).collect();
    world.layers().iter().copied().filter(|&layer| pieces.iter().any(|it| it.layers.has(layer))).collect()
}

/// Via `via` laid against `world`: where it is when its copper is legal there; else moved to the
/// nearest place clear of everything there (`via::nearest` over its fences, from an area round it
/// as far as that place may lie, so no fence outside the area matters); blocked, by what its
/// copper meets where it is, when there is no such place. O(a `verify` of its
/// pieces, then its fences and `nearest` once, or twice when its candidates reach farther, and a
/// `verify` where it goes).
fn lay_via(world: &World, board: &Board, rules: &RuleMatrix, via: &Via, stats: &mut WaveStats, wanted: &Wanted) -> Laid {
    stats.embeds += 1;
    let t = Instant::now();
    let base: Vec<RItem> = world.base.pieces(via.id).iter().filter_map(|&rid| world.base.get(rid)).cloned().collect();
    let here = verify(world, rules, board, &base);
    let Some(hit) = here.first() else {
        stats.spent.embed_ns += ns(t);
        return Laid::Done(Done { id: via.id, what: What::Via { by: P::ZERO }, pieces: base });
    };
    let blocked = Laid::Blocked(stretch::no_room(hit), met_items(&here));
    let rings: Vec<(Copper, Nm)> = base.iter().filter_map(|it| match &it.piece {
        RPiece::Copper(c) => Some((c.clone(), via.pad)),
        _ => None,
    }).collect();
    let span: Vec<LayerId> = world.base.layers().iter().copied()
        .filter(|&layer| base.iter().any(|it| matches!(it.piece, RPiece::Drill { .. }) && it.layers.has(layer))).collect();
    let class = board.nets.get(via.net.ix()).map(|net| net.class).unwrap_or_default();
    let probe = via::Probe { rings: &rings, drill: (ConnItem::Via(via.id), Some(via.net), via.drill), span: &span, in_pad: rules.via_in_pad(class) };
    let reach = (super::obstacles::reach(rules) as i64).max(rules.max_via_drill_to_pad(board) as i64).max(rules.hole_to_hole() as i64);
    // The fences round it, and again twice as far while no free point is within reach of them.
    let mut r = reach + via.pad as i64;
    let gone = || wanted.gone();
    let by = loop {
        let Some(fences) = via::exclusion(world, rules, board, &probe, grow(BBox::of_points([via.at]), r), &gone) else {
            stats.spent.embed_ns += ns(t);
            return blocked;
        };
        match via::nearest(via.at, &fences, r, &gone) {
            via::Found::At(p) => break p.sub(via.at),
            via::Found::Nowhere => {
                stats.spent.embed_ns += ns(t);
                return blocked;
            }
            via::Found::Beyond(far) => r = far,
        }
    };
    stats.spent.embed_ns += ns(t);
    let pieces: Vec<RItem> = base.iter().map(|it| it.translated(by)).collect();
    let t = Instant::now();
    let met = verify(world, rules, board, &pieces);
    stats.spent.verify_ns += ns(t);
    match met.first() {
        None => Laid::Done(Done { id: via.id, what: What::Via { by }, pieces }),
        Some(hit) => Laid::Blocked(stretch::no_room(hit), met_items(&met)),
    }
}

/// Sorted (track, segment) pairs grouped by track: each track and its segments.
pub(super) fn by_item(pairs: &[(ItemId, usize)]) -> Vec<(ItemId, Vec<usize>)> {
    let mut out: Vec<(ItemId, Vec<usize>)> = Vec::new();
    for &(id, seg) in pairs {
        match out.last_mut() {
            Some((last, segs)) if *last == id => segs.push(seg),
            _ => out.push((id, vec![seg])),
        }
    }
    out
}

/// The base pieces of track `id` in `windows`. O(its pieces).
fn base_windows(world: &RBase, id: ItemId, windows: &[Range<usize>]) -> Vec<RId> {
    world.pieces(id).iter().copied()
        .filter(|&rid| matches!(world.get(rid).map(|it| &it.piece), Some(RPiece::Copper(c))
            if matches!(c.item, ConnItem::Track { seg, .. } if windows.iter().any(|w| w.contains(&(seg as usize))))))
        .collect()
}

/// Member `member` laid against `world` one step of `way`: its track and new pieces when they are
/// legal, else why not; the way to go on with when an attached track's transport is not taken (the
/// stretch's way is next) or the stretch's way has runs left to try.
fn lay(
    world: &World, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, member: &Member, way: Way, travel: &Movers,
    stats: &mut WaveStats, wanted: &Wanted,
) -> Laid {
    let class_of = |track: &Track| board.nets.get(track.net.ix()).map(|net| net.class).unwrap_or_default();
    // The track with its new path, when its new segments are legal; else why not, and what they meet.
    let legal = |track: &Track, path: Path, fresh: &[usize], stats: &mut WaveStats| -> Result<Done, (Reason, Vec<ItemId>)> {
        let laid = Track { path, ..track.clone() };
        let mut pieces = stretch::fresh_pieces(board, &laid, fresh);
        stretch::mark_kept(world.base, track.id, &mut pieces);
        let t = Instant::now();
        let met = verify(world, rules, board, &pieces);
        stats.spent.verify_ns += ns(t);
        match met.first() {
            None => Ok(Done { id: track.id, what: What::Track { track: laid, neighbour: member.neighbour(), carried: member.group().is_some() }, pieces }),
            Some(hit) => Err((stretch::no_room(hit), met_items(&met))),
        }
    };
    match &member.kind {
        Kind::Via { via, .. } => lay_via(world, board, rules, via, stats, wanted),
        Kind::Neighbour { track, windows } => {
            // Where it is, when its copper there is legal: nothing laid is in its way (what pushed
            // it went back, or moved off).
            let here: Vec<RItem> = base_windows(world.base, track.id, windows).iter().filter_map(|&rid| world.base.get(rid)).cloned().collect();
            let t = Instant::now();
            let stays = verify(world, rules, board, &here).is_empty();
            stats.spent.verify_ns += ns(t);
            if stays {
                return Laid::Done(Done { id: track.id, what: What::Track { track: track.clone(), neighbour: true, carried: false }, pieces: here });
            }
            let class = class_of(track);
            let base = &track.path;
            let mut path = Path { start: base.start, segs: Vec::with_capacity(base.segs.len() + 8) };
            let mut fresh = Vec::new();
            let mut k = 0;
            for w in windows {
                path.segs.extend_from_slice(&base.segs[k..w.start]);
                let reference: Vec<P> = (w.start..=w.end).map(|v| stretch::vertex(base, v)).collect();
                let crossable = vec![true; reference.len() - 1];
                stats.embeds += 1;
                let Some(laid) = embed(world, rules, hulls, track, class, &reference, &crossable, false, travel, stats, wanted) else {
                    if wanted.gone() {
                        return Laid::Next(Way::Transport);
                    }
                    // Why: what its base copper meets.
                    let mut base_pieces: Vec<RItem> = new_pieces(board, &Item::Track(track.clone())).into_iter()
                        .filter(|it| matches!(&it.piece, RPiece::Copper(c) if matches!(c.item, ConnItem::Track { seg, .. } if w.contains(&(seg as usize)))))
                        .collect();
                    stretch::mark_kept(world.base, track.id, &mut base_pieces);
                    let met = verify(world, rules, board, &base_pieces);
                    return Laid::Blocked(met.first().map(stretch::no_room).unwrap_or_else(|| unsettled(board, track, reference[0])), met_items(&met));
                };
                fresh.extend(path.segs.len()..path.segs.len() + laid.len() - 1);
                path.segs.extend(laid.into_iter().skip(1).map(Seg::Line));
                k = w.end;
            }
            path.segs.extend_from_slice(&base.segs[k..]);
            match legal(track, path, &fresh, stats) {
                Ok(done) => Laid::Done(done),
                Err((why, met)) => Laid::Blocked(why, met),
            }
        }
        Kind::Attached { track, ends, windows, .. } | Kind::Carried { track, ends, windows, .. } => {
            // None of its ends moves (its via laid where it is): it stays where it is when its
            // copper there is legal.
            let still = [track.path.start, track.path.end()].iter().zip(ends).all(|(&at, end)| end.is_none_or(|end| end == at));
            if still && matches!(way, Way::Transport) {
                let here: Vec<RItem> = world.base.pieces(track.id).iter().filter_map(|&rid| world.base.get(rid))
                    .filter(|it| matches!(&it.piece, RPiece::Copper(c) if matches!(c.item, ConnItem::Track { seg, .. } if stretch::changed(track, *ends, windows, seg as usize))))
                    .cloned().collect();
                let t = Instant::now();
                let stays = verify(world, rules, board, &here).is_empty();
                stats.spent.verify_ns += ns(t);
                if stays {
                    return Laid::Done(Done { id: track.id, what: What::Track { track: track.clone(), neighbour: false, carried: member.group().is_some() }, pieces: here });
                }
            }
            let class = class_of(track);
            // Transport is taken as the stretch takes a run: turning no more sharply than a right
            // angle or the base path does.
            let fair = |path: &Path| stretch::sharpest(&stretch::points(path.start, &path.segs))
                >= stretch::sharpest(&stretch::points(track.path.start, &track.path.segs)).min(0.0) - 1e-9;
            let mut relay = match way {
                Way::Transport => {
                    let taken = transported(world, rules, hulls, track, class, *ends, windows, travel, stats, wanted).filter(|(path, _)| fair(path))
                        .and_then(|(path, fresh)| legal(track, path, &fresh, stats).ok());
                    if let Some(done) = taken { return Laid::Done(done); }
                    return Laid::Next(Way::Relay(stretch::Relay::new(windows)));
                }
                Way::Relay(relay) => relay,
            };
            if relay.untried() {
                stats.embeds += 1;
            }
            let mut queries = 0;
            let t = Instant::now();
            let laid = relay.step(world, board, rules, hulls, &mut queries, (track, *ends, windows), class);
            stats.spent.embed_ns += ns(t);
            let Some((path, fresh)) = laid else { return Laid::Next(Way::Relay(relay)) };
            match legal(track, path, &fresh, stats) {
                Ok(done) => Laid::Done(done),
                Err((why, met)) => Laid::Blocked(why, met),
            }
        }
    }
}

/// The board items `met` names on its other side.
fn met_items(met: &[Collision]) -> Vec<ItemId> {
    met.iter().filter_map(|hit| match &hit.b {
        Sel::Item(_, id) => Some(*id),
        _ => None,
    }).collect()
}

/// The reason given when a member cannot be laid and its base copper meets nothing: the band
/// could not settle.
fn unsettled(board: &Board, track: &Track, at: P) -> Reason {
    let sel = Sel::Item(board.unit, track.id);
    Reason::NoRoom { at, layer: track.layer, need: fab_elec_circuit::Measure::Len(0), have: fab_elec_circuit::Measure::Len(0), between: [sel.clone(), sel], by: Vec::new() }
}

/// An attached track's windows laid by transport: each from its moved end along its first segment
/// moved with the part, back to that segment's far end, then on along the base path (to the other
/// moved end, moved the same way), spiked where a moved part's travel crossed the base path, and
/// tightened. The path, and its new segments; `None` when a window cannot be laid.
fn transported(
    world: &World, rules: &RuleMatrix, hulls: &mut HullCache, track: &Track, class: ClassId, ends: [Option<P>; 2],
    windows: &[Window], travel: &Movers, stats: &mut WaveStats, wanted: &Wanted,
) -> Option<(Path, Vec<usize>)> {
    let base = &track.path;
    let n = base.segs.len();
    let straight = stretch::straight(base, ends);
    let v = |k: usize| stretch::vertex(base, k);
    let mut path = Path { start: straight.start, segs: Vec::with_capacity(n + 8) };
    let mut fresh = Vec::new();
    let mut windows = windows.iter().peekable();
    let mut k = 0;
    while k < n {
        let Some(w) = windows.next_if(|w| w.segs.start == k) else {
            if stretch::changed(track, ends, &[], k) {
                fresh.push(path.segs.len());
            }
            path.segs.push(straight.segs[k]);
            k += 1;
            continue;
        };
        let (lo, hi) = (w.segs.start, w.segs.end);
        let (first, last) = (w.at_start.then_some(ends[0]).flatten(), (hi == n).then_some(ends[1]).flatten());
        // The reference's points, each with whether it is on the base path.
        let mut points: Vec<(P, bool)> = Vec::new();
        let inner = if first.is_some() { lo + 1 } else { lo }..=if last.is_some() { hi - 1 } else { hi };
        if let Some(p) = first {
            points.push((p, false));
            if !inner.is_empty() {
                points.push((v(lo + 1).add(p.sub(v(lo))), false));
            }
        }
        points.extend(inner.clone().map(|k| (v(k), true)));
        if let Some(q) = last {
            if !inner.is_empty() {
                points.push((v(hi - 1).add(q.sub(v(hi))), false));
            }
            points.push((q, false));
        }
        points.dedup_by_key(|(p, _)| *p);
        let reference: Vec<P> = points.iter().map(|(p, _)| *p).collect();
        let crossable: Vec<bool> = points.windows(2).map(|pair| pair[0].1 && pair[1].1).collect();
        stats.embeds += 1;
        let laid = embed(world, rules, hulls, track, class, &reference, &crossable, true, travel, stats, wanted)?;
        fresh.extend(path.segs.len()..path.segs.len() + laid.len() - 1);
        path.segs.extend(laid.into_iter().skip(1).map(Seg::Line));
        k = hi;
    }
    Some((path, fresh))
}

/// `reference` laid round everything in `world` but `track` itself, with a spike past each moved
/// part whose travel crosses its `crossable` segments an odd number of times, then tightened:
/// wholly (`whole`), or in its runs of legs that are not legs of `reference`, their ends fixed;
/// either way without vertices inside a straight line. `None` when it is blocked, a spike has no room, or a nearby shape has no hull.
fn embed(
    world: &World, rules: &RuleMatrix, hulls: &mut HullCache, track: &Track, class: ClassId, reference: &[P],
    crossable: &[bool], whole: bool, travel: &Movers, stats: &mut WaveStats, wanted: &Wanted,
) -> Option<Vec<P>> {
    let t = Instant::now();
    let laid = embed_in(world, rules, hulls, track, class, reference, crossable, whole, travel, &mut stats.spikes, wanted);
    stats.spent.embed_ns += ns(t);
    laid
}

fn embed_in(
    world: &World, rules: &RuleMatrix, hulls: &mut HullCache, track: &Track, class: ClassId, reference: &[P],
    crossable: &[bool], whole: bool, travel: &Movers, spiked_count: &mut usize, wanted: &Wanted,
) -> Option<Vec<P>> {
    let tile = tile_size(track.w, rules);
    let mut obstacles = WorldObstacles::new(world, rules, track.layer, track.net, class, track.w, track.id, hulls);
    let spiked = match travel {
        Movers::Moved(by_piece) => {
            let mut parts = Vec::new();
            let mut items = Vec::new();
            obstacles.near_items(grow(BBox::of_points(reference.iter().copied()), travel.reach() + 1), &mut items);
            for item in items {
                parts.extend(obstacles.parts(item).iter().map(|hull| Part {
                    key: crate::embed::band::PartKey { item, part: hull.part }, hull: hull.clone(),
                }));
            }
            let mut spikes = Vec::new();
            for part in &parts {
                let Some(WRef::Over(k)) = obstacles.reference(part.key.item) else { continue };
                let Some(&by) = by_piece.get(k as usize) else { continue };
                let c1 = part.hull.c;
                if let Some((seg, x)) = crossing(reference, crossable, c1.sub(by), c1) {
                    spikes.push((seg, x, c1, part.key));
                }
            }
            *spiked_count += spikes.len();
            if spikes.is_empty() { reference.to_vec() } else { spike(reference, spikes, &parts)? }
        }
        Movers::Drawn { runs, .. } => {
            let (fingered, fingers) = fingered(reference, crossable, track.layer, runs)?;
            *spiked_count += fingers;
            fingered
        }
    };
    // Between its parts it stops (`None`) once the wave is not wanted.
    if wanted.gone() {
        return None;
    }
    let input = BandInput { s: spiked[0], e: spiked[spiked.len() - 1], reference: &spiked, tile };
    let band = make_legal(&input, &mut obstacles);
    if band.blocked.is_some() || obstacles.failed() || wanted.gone() {
        return None;
    }
    let laid = band.points.clone();
    if whole {
        let tight = tighten(band, &input, &mut obstacles);
        return Some(straightened(if tight.blocked.is_none() && !obstacles.failed() { &tight.points } else { &laid }));
    }
    // The runs that changed, each tightened between its ends.
    let legs: BTreeSet<(P, P)> = reference.windows(2).map(|pair| (pair[0], pair[1])).collect();
    let kept = |i: usize| legs.contains(&(laid[i], laid[i + 1]));
    let mut out = vec![laid[0]];
    let mut i = 0;
    while i + 1 < laid.len() {
        if kept(i) {
            out.push(laid[i + 1]);
            i += 1;
            continue;
        }
        if wanted.gone() {
            return None;
        }
        let mut j = i + 1;
        while j + 1 < laid.len() && !kept(j) {
            j += 1;
        }
        let run = &laid[i..=j];
        let input = BandInput { s: run[0], e: run[run.len() - 1], reference: run, tile };
        let band = Band { points: run.to_vec(), wraps: Vec::new(), blocked: None, stats: BandStats::default() };
        let tight = tighten(band, &input, &mut obstacles);
        let run = if tight.blocked.is_some() || obstacles.failed() {
            run.to_vec()
        } else {
            let from = reference.iter().position(|&p| p == run[0]);
            let to = reference.iter().rposition(|&p| p == run[run.len() - 1]);
            match (from, to) {
                (Some(from), Some(to)) if from < to => hug(&tight, &reference[from..=to], &mut obstacles, tile, travel.count()),
                _ => tight.points,
            }
        };
        out.extend(straightened(&run).into_iter().skip(1));
        i = j;
    }
    Some(out)
}

/// A changed run `tight` (taut, between two vertices of its base path `base`) pulled back onto
/// `base` away from what it wraps, so a pushed track bulges round what pushed it and nowhere else.
/// Octilinear paths of one length are many, and a taut run may as well take its 45-degree ramps at
/// its far ends, moving the whole run; once moved, it grazes other parts further on. Instead the
/// base path is kept up to the foot P of the 45-degree line toward it from the run's first contact
/// with a moved part (a point on one of its vertices; the first `movers` pieces of the world's
/// overlay are the moved items') and from the foot Q of the line from its last, and the run is
/// laid again from P to Q through the points between those contacts, round anything else it then
/// meets. P and Q move out, by a tile and then twice as far each time, while that cannot be laid.
/// The run itself when P and Q reach its ends, it touches no moved part, or the base kept is not
/// legal. O(contacts * base + make_legal and tighten per try).
fn hug(tight: &Band, base: &[P], obstacles: &mut WorldObstacles, tile: Nm, movers: usize) -> Vec<P> {
    let points = &tight.points;
    let moved = |w: &&crate::embed::band::WrapSpan| matches!(obstacles.reference(w.key.item), Some(WRef::Over(k)) if (k as usize) < movers);
    let contacts: Vec<usize> = tight.wraps.iter().filter(moved).flat_map(|w| w.first..=w.last).filter(|&k| k > 0 && k + 1 < points.len()).collect();
    let (Some(&lo), Some(&hi)) = (contacts.iter().min(), contacts.iter().max()) else { return points.clone() };
    let total: f64 = base.windows(2).map(|pair| length(pair[0], pair[1])).sum();
    let (mut from, mut to) = (f64::MAX, f64::MIN);
    for &k in &contacts {
        let (along, off) = project(base, points[k]);
        from = from.min(along - off);
        to = to.max(along + off);
    }
    let mut extra = 0.0;
    loop {
        let (a, b) = ((from - extra).max(0.0), (to + extra).min(total));
        if a <= 0.0 && b >= total {
            return points.clone();
        }
        let ((ia, pa), (ib, pb)) = (at(base, a, false), at(base, b, true));
        let mut reference = vec![pa];
        reference.extend_from_slice(&points[lo..=hi]);
        reference.push(pb);
        reference.dedup();
        let input = BandInput { s: pa, e: pb, reference: &reference, tile };
        let band = make_legal(&input, obstacles);
        if band.blocked.is_none() && !obstacles.failed() {
            let band = tighten(band, &input, obstacles);
            if band.blocked.is_none() && !obstacles.failed() {
                let mut out: Vec<P> = base[..=ia].to_vec();
                out.extend(band.points);
                out.extend_from_slice(&base[ib + 1..]);
                out.dedup();
                // The base kept on either side must be legal as it is.
                let check = BandInput { s: out[0], e: out[out.len() - 1], reference: &out, tile };
                let checked = make_legal(&check, obstacles);
                return if checked.blocked.is_none() && !obstacles.failed() && checked.points == out { out } else { points.clone() };
            }
        }
        extra = if extra == 0.0 { tile as f64 } else { extra * 2.0 };
    }
}

/// Where `p` lies along `base`: the distance along it to its nearest point, and the distance to
/// that point. O(base).
fn project(base: &[P], p: P) -> (f64, f64) {
    let mut best = (f64::MAX, 0.0);
    let mut start = 0.0;
    for pair in base.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (dx, dy) = (b.x as f64 - a.x as f64, b.y as f64 - a.y as f64);
        let len2 = dx * dx + dy * dy;
        let t = if len2 == 0.0 { 0.0 } else { (((p.x as f64 - a.x as f64) * dx + (p.y as f64 - a.y as f64) * dy) / len2).clamp(0.0, 1.0) };
        let off = (p.x as f64 - a.x as f64 - t * dx).hypot(p.y as f64 - a.y as f64 - t * dy);
        if off < best.0 {
            best = (off, start + t * len2.sqrt());
        }
        start += len2.sqrt();
    }
    (best.1, best.0)
}

/// The grid point of `base`'s octilinear segment at distance `along` from its start, rounded back
/// toward the start, or on toward the end (`on`), and the segment's index. O(base).
fn at(base: &[P], along: f64, on: bool) -> (usize, P) {
    let mut start = 0.0;
    for (i, pair) in base.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        let len = length(a, b);
        if along <= start + len || i + 2 == base.len() {
            let (dx, dy) = (b.x as i64 - a.x as i64, b.y as i64 - a.y as i64);
            let steps = dx.abs().max(dy.abs());
            let step = if dx != 0 && dy != 0 { std::f64::consts::SQRT_2 } else { 1.0 };
            let k = (along - start) / step;
            let k = (if on { k.ceil() } else { k.floor() } as i64).clamp(0, steps);
            let p = if steps == 0 { a } else { P::new((a.x as i64 + k * dx.signum()) as Nm, (a.y as i64 + k * dy.signum()) as Nm) };
            return (i, p);
        }
        start += len;
    }
    (0, base[0])
}

fn length(a: P, b: P) -> f64 {
    (b.x as f64 - a.x as f64).hypot(b.y as f64 - a.y as f64)
}

/// `points` without the vertices inside a straight line (a leg going on in its direction).
/// O(points).
fn straightened(points: &[P]) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(points.len());
    for &p in points {
        if out.last() == Some(&p) {
            continue;
        }
        if let [.., a, b] = out[..] {
            let (u, v) = (b.sub(a), p.sub(b));
            let cross = u.x as i128 * v.y as i128 - u.y as i128 * v.x as i128;
            let dot = u.x as i128 * v.x as i128 + u.y as i128 * v.y as i128;
            if cross == 0 && dot > 0 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// Where the travel `c0`-`c1` of a part crosses `reference`'s `crossable` segments: when it
/// crosses them an odd number of times, the segment and the crossing nearest `c1` (in nm,
/// rounded). A point on the travel's line counts as left of it, a travel end on a segment's line
/// as left of that, so touching counts once. `None` with an even count, or when `c1` is on a
/// crossed segment's line (no side to keep). O(segments).
fn crossing(reference: &[P], crossable: &[bool], c0: P, c1: P) -> Option<(usize, (f64, f64))> {
    if c0 == c1 {
        return None;
    }
    let mut odd = false;
    let mut nearest: Option<(f64, usize)> = None;
    for (i, pair) in reference.windows(2).enumerate() {
        if !crossable.get(i).copied().unwrap_or(false) {
            continue;
        }
        let (a, b) = (pair[0], pair[1]);
        if (orient(c0, c1, a) >= 0) == (orient(c0, c1, b) >= 0) {
            continue;
        }
        let (o0, o1) = (orient(a, b, c0), orient(a, b, c1));
        if (o0 >= 0) == (o1 >= 0) {
            continue;
        }
        if o1 == 0 {
            return None;
        }
        odd = !odd;
        let t = o0 as f64 / (o0 - o1) as f64;
        if nearest.is_none_or(|(best, _)| t > best) {
            nearest = Some((t, i));
        }
    }
    let (t, seg) = nearest.filter(|_| odd)?;
    let x = (c0.x as f64 + t * (c1.x as f64 - c0.x as f64), c0.y as f64 + t * (c1.y as f64 - c0.y as f64));
    Some((seg, x))
}

/// `reference` with a spike per crossing: the crossing's segment point x replaced by its grid
/// neighbours on the segment and, between them, a tip just past the part's centre, so that the
/// centre lies strictly inside the thin triangle and on the reference's other side. Of the grid
/// points near x, one where the centre is straight across, or across at 45 degrees, gives a tip
/// 1 nm past it; any other, a tip as far past it as it is from x. A triangle that holds another
/// part's centre, or spikes that overlap, give `None`. O(spikes * parts).
fn spike(reference: &[P], mut spikes: Vec<(usize, (f64, f64), P, crate::embed::band::PartKey)>, parts: &[Part]) -> Option<Vec<P>> {
    let mut chosen: Vec<(usize, i64, [P; 3])> = Vec::with_capacity(spikes.len());
    spikes.sort_by(|x, y| (x.0, x.3).cmp(&(y.0, y.3)));
    for (seg, x, c1, key) in spikes {
        let (a, b) = (reference[seg], reference[seg + 1]);
        let (dx, dy) = (b.x as i64 - a.x as i64, b.y as i64 - a.y as i64);
        let steps = dx.abs().max(dy.abs());
        if steps < 2 || !(dx == 0 || dy == 0 || dx.abs() == dy.abs()) {
            return None;
        }
        let u = P::new(dx.signum() as Nm, dy.signum() as Nm);
        let at = |k: i64| P::new((a.x as i64 + k * u.x as i64) as Nm, (a.y as i64 + k * u.y as i64) as Nm);
        // The grid step nearest x along the segment, then those with the centre straight or
        // diagonally across.
        let diagonal = u.x != 0 && u.y != 0;
        let kx = (((x.0 - a.x as f64) * u.x as f64 + (x.1 - a.y as f64) * u.y as f64) / if diagonal { 2.0 } else { 1.0 }).round() as i64;
        let (cx, cy) = (c1.x as i64 - a.x as i64, c1.y as i64 - a.y as i64);
        let mut candidates: Vec<i64> = match (u.x, u.y) {
            (_, 0) => vec![cx * u.x as i64, (cx - cy) * u.x as i64, (cx + cy) * u.x as i64],
            (0, _) => vec![cy * u.y as i64, (cy - cx) * u.y as i64, (cy + cx) * u.y as i64],
            _ => {
                let mut ks = vec![cx * u.x as i64, cy * u.y as i64];
                let sum = cx * u.x as i64 + cy * u.y as i64;
                if sum % 2 == 0 {
                    ks.push(sum / 2);
                }
                ks
            }
        };
        candidates.sort_by_key(|k| (k - kx).abs());
        candidates.push(kx);
        let other = |p: P, tri: &[P; 3]| {
            let (o0, o1, o2) = (orient(tri[0], tri[1], p), orient(tri[1], tri[2], p), orient(tri[2], tri[0], p));
            (o0 >= 0 && o1 >= 0 && o2 >= 0) || (o0 <= 0 && o1 <= 0 && o2 <= 0)
        };
        let found = candidates.into_iter().filter(|&k| k >= 1 && k < steps).find_map(|k| {
            let base = at(k);
            let g = c1.sub(base);
            let d = gcd(g.x.unsigned_abs() as u64, g.y.unsigned_abs() as u64).max(1) as i64;
            let tip = P::new((c1.x as i64 + g.x as i64 / d) as Nm, (c1.y as i64 + g.y as i64 / d) as Nm);
            let tri = [at(k - 1), tip, at(k + 1)];
            let empty = parts.iter().all(|part| part.key == key || part.hull.c == c1 || !other(part.hull.c, &tri));
            empty.then_some((k, tri))
        });
        let (k, tri) = found?;
        chosen.push((seg, k, tri));
    }
    chosen.sort_by_key(|(seg, k, _)| (*seg, *k));
    if chosen.windows(2).any(|pair| pair[0].0 == pair[1].0 && pair[1].1 - 1 <= pair[0].1 + 1) {
        return None;
    }
    let mut out = Vec::with_capacity(reference.len() + 3 * chosen.len());
    let mut chosen = chosen.into_iter().peekable();
    for (i, &p) in reference.iter().enumerate() {
        if out.last() != Some(&p) {
            out.push(p);
        }
        while let Some((_, _, tri)) = chosen.next_if(|(seg, ..)| *seg == i) {
            for q in tri {
                if out.last() != Some(&q) {
                    out.push(q);
                }
            }
        }
    }
    Some(out)
}

/// Half the width of a finger (nm): wide enough that the copper drawn along a run, whose centres
/// lie on it, is strictly inside after rounding, thin enough to hold nothing else.
const FINGER: f64 = 50.0;

/// `reference` pushed ahead of a drawn route like a rubber band (the route head's Shove mode):
/// where a run of the route on `layer` crosses the reference's `crossable` segments once, the
/// reference leaves that segment there, follows the run on the side it came from to just past the
/// run's end, and comes back along the run's other side, so the run's copper from the crossing on
/// is inside the finger; `make_legal` then wraps it. With the number of fingers. `None` when a run
/// crosses it more than once (no one side to keep) or a finger cannot be built; the reference
/// itself when no run crosses it. A point on a line counts as left of it, as in `crossing`.
/// O(runs' segments * reference segments).
fn fingered(reference: &[P], crossable: &[bool], layer: LayerId, runs: &[(LayerId, Vec<P>)]) -> Option<(Vec<P>, usize)> {
    let mut points = reference.to_vec();
    let mut open = crossable.to_vec();
    let mut fingers = 0;
    for (_, run) in runs.iter().filter(|(l, _)| *l == layer) {
        let mut crossed = None;
        for (s, leg) in run.windows(2).enumerate() {
            let (c0, c1) = (leg[0], leg[1]);
            if c0 == c1 {
                continue;
            }
            for (i, pair) in points.windows(2).enumerate() {
                if !open.get(i).copied().unwrap_or(false) {
                    continue;
                }
                let (a, b) = (pair[0], pair[1]);
                let left = orient(c0, c1, a) >= 0;
                if left == (orient(c0, c1, b) >= 0) {
                    continue;
                }
                let (o0, o1) = (orient(a, b, c0), orient(a, b, c1));
                if (o0 >= 0) == (o1 >= 0) {
                    continue;
                }
                if crossed.is_some() {
                    return None;
                }
                crossed = Some((s, i, o0 as f64 / (o0 - o1) as f64, left));
            }
        }
        let Some((s, i, t, left)) = crossed else { continue };
        let (c0, c1) = (run[s], run[s + 1]);
        let x = (c0.x as f64 + t * (c1.x as f64 - c0.x as f64), c0.y as f64 + t * (c1.y as f64 - c0.y as f64));
        // The run from the crossing on, and its directions.
        let mut tail = vec![x];
        for p in &run[s + 1..] {
            let q = (p.x as f64, p.y as f64);
            if tail.last().is_some_and(|l: &(f64, f64)| (l.0 - q.0).abs() + (l.1 - q.1).abs() > 0.5) {
                tail.push(q);
            }
        }
        if tail.len() < 2 {
            return None;
        }
        let unit = |a: (f64, f64), b: (f64, f64)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = dx.hypot(dy);
            (dx / l, dy / l)
        };
        let dirs: Vec<(f64, f64)> = tail.windows(2).map(|w| unit(w[0], w[1])).collect();
        // The side the reference came from: the out leg's.
        let side = if left { 1.0 } else { -1.0 };
        let offset = |sign: f64| -> Option<Vec<(f64, f64)>> {
            let normal = |d: (f64, f64)| (-d.1 * sign * side, d.0 * sign * side);
            let mut out = Vec::with_capacity(tail.len() + 1);
            for (k, &v) in tail.iter().enumerate().skip(1) {
                let d1 = dirs[k - 1];
                let n1 = normal(d1);
                match dirs.get(k) {
                    Some(&d2) => {
                        let n2 = normal(d2);
                        let cos = d1.0 * d2.0 + d1.1 * d2.1;
                        if cos < -0.9 {
                            return None;
                        }
                        let f = FINGER / (1.0 + cos);
                        out.push((v.0 + (n1.0 + n2.0) * f, v.1 + (n1.1 + n2.1) * f));
                    }
                    // Past the run's end, round its last piece.
                    None => out.push((v.0 + (d1.0 + n1.0) * FINGER, v.1 + (d1.1 + n1.1) * FINGER)),
                }
            }
            Some(out)
        };
        let (out, back) = (offset(1.0)?, offset(-1.0)?);
        // Where the offsets of the crossing's leg meet the crossed segment.
        let (a, b) = (points[i], points[i + 1]);
        let n0 = (-dirs[0].1 * side, dirs[0].0 * side);
        let across = n0.0 * (b.x as f64 - a.x as f64) + n0.1 * (b.y as f64 - a.y as f64);
        let from = n0.0 * (a.x as f64 - x.0) + n0.1 * (a.y as f64 - x.1);
        let on = |d: f64| -> Option<(f64, f64)> {
            let u = (d - from) / across;
            (u > 0.0 && u < 1.0).then(|| (a.x as f64 + u * (b.x as f64 - a.x as f64), a.y as f64 + u * (b.y as f64 - a.y as f64)))
        };
        let (enter, leave) = (on(FINGER)?, on(-FINGER)?);
        let round = |p: (f64, f64)| P::new(p.0.round() as Nm, p.1.round() as Nm);
        let finger: Vec<P> = std::iter::once(enter).chain(out).chain(back.into_iter().rev()).chain(std::iter::once(leave)).map(round).collect();
        let mut next = points[..=i].to_vec();
        let mut next_open = open[..i].to_vec();
        for p in finger {
            if next.last() != Some(&p) {
                next.push(p);
                next_open.push(false);
            }
        }
        // The crossed segment's two ends of it are base path again.
        if let Some(first) = next_open.get_mut(i) {
            *first = open[i];
        }
        if next.last() != Some(&b) {
            next.push(b);
            next_open.push(open[i]);
        }
        next.extend_from_slice(&points[i + 2..]);
        next_open.extend_from_slice(&open[i + 1..]);
        (points, open) = (next, next_open);
        fingers += 1;
    }
    Some((points, fingers))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn grow(area: BBox, by: i64) -> BBox {
    super::obstacles::grow(area, by)
}
