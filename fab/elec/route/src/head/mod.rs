//! Synchronous stretch answers for route gestures (E6 3): the fixed legs and their vias, then the
//! head from the newest fixed point to the cursor, embedded by the band embedder against the
//! committed base plus what this answer adds before it; segment and corner drags (`drag`); and
//! part and via drags with their attached tracks stretched (`stretch`). A finishing route sample
//! (`Gesture::finish`) gets a second answer on the pool: the grid finder's rest of the net
//! (`finish`). In Shove mode the head is laid round the fixed things and the tracks the route
//! meets are pushed by a wave (`push`), all or none; else it walks, as in Walk mode.
//! Other gesture kinds and modes (Mark) are later tiers: such a sample is refused as unsupported,
//! never answered as something else.
//!
//! Every style is laid at 45 degrees for now (the octilinear embedder). Vias are through-hole
//! until the stackup declares other spans (`via`).
//!
//! An answer is a function of (the committed base, the gesture), so it is defined on any base the
//! worker holds: the start is re-verified by its position and the target by its net and layer
//! each sample, and the legs are verified every sample. Answers of one gesture are ordered by
//! (base generation, seq): a sample at or below the newest answered pair is covered by it.
//! Anchors only name items added before them: a via comes before the leg that ends on it, and a
//! track starts on the end of the track before it (`Anchor::Track`) or on the via between them.
//!
//! Cost per sample: each fixed leg and via is laid again (its hulls and pair checks), then the
//! band's `make_legal` and `tighten` over the trail, with the base's hulls cached per generation.

mod drag;
mod finish;
pub mod obstacles;
mod pair;
mod push;
mod ride;
mod shove;
mod stretch;
mod tune;
pub mod via;

use self::obstacles::{HullCache, WorldObstacles};
use crate::embed::band::{make_legal, tighten, BandInput};
use crate::embed::octi;
use crate::gesture::{Gesture, GestureKind, GestureSample, Mode, WidthRule};
use crate::proposal::{BlockCause, Blocked, Collision, HeadAnswer, ProposalId, ProposalSource, RouteProposal, SampleCause, Tier};
use crate::request::Base;
use crate::solution::RouteMetrics;
use crate::verify::{new_pieces, verify};
use crate::world::{RBase, World};
use fab_elec_board::{Anchor, Board, BoardOp, Flash, Item, Origin, RuleMatrix, Tent, Track, Via, ViaKind};
use fab_elec_core::{ClassId, ItemId, LayerId, NetId, Nm, OnTrack, Path, Seg, P};
use std::time::Instant;

pub use self::finish::finish;
pub use self::shove::{Spent, WaveStats};

/// What a route sample gets from `answer`.
#[derive(Debug)]
pub enum Answer {
    /// The proposal for the sample.
    Proposal(RouteProposal),
    /// No proposal, and why (canvas text).
    Refused(SampleCause, &'static str),
    /// A sample of the gesture at or above this one's (base generation, seq) was answered; that
    /// answer covers this one.
    Covered,
}

impl Answer {
    /// The proposal, if the sample got one.
    pub fn proposal(self) -> Option<RouteProposal> {
        match self {
            Self::Proposal(proposal) => Some(proposal),
            Self::Refused(..) | Self::Covered => None,
        }
    }
}

/// Order guard and generation-scoped geometry cache of one live gesture, and the shove wave its
/// newest answer left to step.
#[derive(Default)]
pub struct GestureState {
    /// The (base generation, seq) of the newest answer.
    pub newest: Option<(u64, u64)>,
    /// The generation the hull cache holds.
    pub base_generation: Option<u64>,
    pub hulls: HullCache,
    /// Pushable fanout vias and their moved ends, cleared when the base generation changes.
    fanout: ride::Fanout,
    wave: Option<Box<shove::Wave>>,
}

impl GestureState {
    /// Whether the newest answer left a shove wave to step: its Full answer follows (`shove_step`).
    pub fn waving(&self) -> bool {
        self.wave.is_some()
    }

    /// Drop the shove wave unanswered (the gesture's base changed under it).
    pub fn drop_wave(&mut self) {
        self.wave = None;
    }

    /// What the wave has done so far, while it goes on (a benchmark's per-step view).
    pub fn wave_stats(&self) -> Option<WaveStats> {
        self.wave.as_ref().map(|wave| wave.stats())
    }
}

/// One step of the gesture's shove wave on `base`, which must be the base its stretch answer was
/// made on: the wave's Full answer (`id()` names it) once every member is laid or given up, which
/// ends the wave. `None` while it goes on, and without a wave. A base of another generation drops
/// the wave unanswered: the worker answers the gesture's newest sample on it again.
pub fn shove_step(
    base: &Base, world: &RBase, state: &mut GestureState, id: impl FnOnce() -> ProposalId,
) -> Option<(RouteProposal, WaveStats)> {
    shove_step_until(base, world, state, id, &|| false)
}

/// `shove_step` for the worker: `stale` says a newer sample waits, whose answer drops the wave
/// anyway. The step asks it between its parts (each lay, each via and track a blocked member
/// looks at, each round of a via's search, each track the wave settles) and drops the wave
/// unanswered at the first part it holds, so the sample waits only for the part in hand.
pub fn shove_step_until(
    base: &Base, world: &RBase, state: &mut GestureState, id: impl FnOnce() -> ProposalId, stale: &dyn Fn() -> bool,
) -> Option<(RouteProposal, WaveStats)> {
    let mut wave = state.wave.take()?;
    if wave.generation() != base.board.generation || world.generation() != base.board.generation {
        return None;
    }
    let wanted = shove::Wanted::new(stale);
    let done = wave.step(world, &base.board, &base.rules, &mut state.hulls, &wanted);
    if wanted.gone() {
        return None;
    }
    if !done {
        state.wave = Some(wave);
        return None;
    }
    Some(wave.answer(base, world, &mut state.hulls, id()))
}

/// The gesture's shove wave stepped to its end: its Full answer (tests and benchmarks; the worker
/// steps it between its other work).
pub fn shove(base: &Base, world: &RBase, state: &mut GestureState, id: ProposalId) -> Option<(RouteProposal, WaveStats)> {
    while state.waving() {
        if let Some(full) = shove_step(base, world, state, || id) {
            return Some(full);
        }
    }
    None
}

/// Query tile length, kept separate for routing benchmarks.
pub fn tile_size(w: Nm, rules: &RuleMatrix) -> Nm {
    w.saturating_add(obstacles::reach(rules)).max(1)
}

fn anchor_net(board: &Board, anchor: Anchor) -> Option<NetId> {
    match anchor {
        Anchor::Pad { placed, pad } => board.placed_item(placed)?.pad_nets.get(pad.ix()).copied().flatten(),
        Anchor::Via(id) => board.vias.iter().find(|v| v.id == id).map(|v| v.net),
        Anchor::Track { id, .. } => board.tracks.iter().find(|t| t.id == id).map(|t| t.net),
        Anchor::Free | Anchor::Hole(_) => None,
    }
}

fn target(board: &Board, anchor: Anchor, net: NetId, layer: LayerId) -> Option<P> {
    if anchor_net(board, anchor) != Some(net) { return None; }
    let on_layer = match anchor {
        Anchor::Pad { placed, pad } => board.placed_item(placed)?
            .pad_layers(pad, board.copper_set(), board.copper_count()).has(layer),
        Anchor::Via(id) => board.vias.iter().find(|v| v.id == id)?.span_layers(board.copper_set()).has(layer),
        Anchor::Track { id, .. } => board.tracks.iter().find(|t| t.id == id)?.layer == layer,
        Anchor::Free | Anchor::Hole(_) => false,
    };
    if on_layer { board.anchor_pos(anchor) } else { None }
}

fn points(path: &Path) -> Option<Vec<P>> {
    let mut points = vec![path.start];
    for segment in &path.segs {
        let Seg::Line(p) = segment else { return None };
        let a = *points.last()?;
        let (dx, dy) = (p.x as i64 - a.x as i64, p.y as i64 - a.y as i64);
        if dx != 0 && dy != 0 && dx.abs() != dy.abs() { return None; }
        points.push(*p);
    }
    Some(points)
}

pub(super) fn path(points: &[P], start: P) -> Path {
    Path { start: points.first().copied().unwrap_or(start), segs: points.iter().skip(1).copied().map(Seg::Line).collect() }
}

pub(super) fn allocate(next: &mut u32) -> Option<ItemId> {
    let id = ItemId(*next);
    *next = next.checked_add(1)?;
    Some(id)
}

pub(super) fn track(id: ItemId, net: NetId, layer: LayerId, w: Nm, path: Path, start: Anchor, end: Anchor) -> Item {
    Item::Track(Track { id, net, layer, w, widths: None, path, ends: [start, end], locked: false, origin: Origin::Authored })
}

fn end_anchor(item: &Item) -> Anchor {
    match item {
        Item::Track(t) => Anchor::Track {
            id: t.id, at: OnTrack { seg: t.path.segs.len().saturating_sub(1) as u32, frac: OnTrack::ONE },
        },
        Item::Via(v) => Anchor::Via(v.id),
        _ => Anchor::Free,
    }
}

/// Verify `item` against what is laid so far, then add it to the answer's world.
fn lay(over: &mut World, rules: &RuleMatrix, board: &Board, item: Item, collisions: &mut Vec<Collision>, items: &mut Vec<Item>) {
    let pieces = new_pieces(board, &item);
    collisions.extend(verify(over, rules, board, &pieces));
    for piece in pieces { over.add(piece); }
    items.push(item);
}

/// Answer a sample on the worker's committed base (`base` and its `world`), whatever generation
/// the sample was drawn on: a proposal, a refusal, or `Covered`. Proposals use the committed base,
/// never the previous answer's geometry. Each fixed leg keeps the width it was fixed with; the
/// head takes the sample's. After a via slides, only the next leg's first segment is reconnected,
/// inserting an octilinear corner when needed.
pub fn answer(
    base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState, id: ProposalId,
) -> Answer {
    answer_counted(base, world, sample, state, id).0
}

/// `answer`, and how many obstacle queries (`Obstacles::near_items`) the head's band made: for the bench.
pub fn answer_counted(
    base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState, id: ProposalId,
) -> (Answer, usize) {
    let started = Instant::now();
    let gesture = &sample.gesture;
    let board = &base.board;
    debug_assert_eq!(world.generation(), board.generation, "the worker's world follows its base");
    if state.base_generation != Some(board.generation) {
        state.hulls.clear();
        state.fanout.clear();
        state.base_generation = Some(board.generation);
    }
    if state.newest.is_some_and(|newest| (board.generation, sample.seq) <= newest) { return (Answer::Covered, 0); }
    // A newer sample's answer: the wave an older one left is worthless, and so are the hulls of the
    // pieces its answers added.
    state.wave = None;
    state.hulls.clear_added();
    let dragged = matches!(gesture.kind, GestureKind::DragSeg { .. } | GestureKind::DragCorner { .. });
    let stretched = matches!(gesture.kind, GestureKind::DragParts | GestureKind::DragVia(_));
    if !matches!(gesture.kind, GestureKind::Route | GestureKind::RoutePair { .. } | GestureKind::Tune { .. }) && !dragged && !stretched {
        return (Answer::Refused(SampleCause::Unsupported, "only drawing copper and dragging tracks, parts and vias are answered yet"), 0);
    }
    // Drags push in either mode (`drag`, `stretch`); a route pushes in Shove (`push`).
    if gesture.mode == Mode::Mark {
        return (Answer::Refused(SampleCause::Unsupported, "only walk and shove modes are answered yet"), 0);
    }
    // The board may have changed under the gesture: what was grabbed must still be where it was.
    if gesture.unit != base.unit
        || gesture.start != Anchor::Free && board.anchor_pos(gesture.start) != Some(gesture.start_at)
    {
        return (Answer::Refused(SampleCause::Invalid, "what you grabbed has moved"), 0);
    }
    if let GestureKind::Tune { track, from, to, circuit } = &gesture.kind {
        let result = tune::answer(base, world, sample, id, *track, *from, *to, circuit, started);
        state.newest = Some((board.generation, sample.seq));
        return (Answer::Proposal(result), 0);
    }
    if let GestureKind::RoutePair { mate, mate_start, mate_start_at } = gesture.kind {
        let result = pair::answer(base, world, sample, id, (mate, mate_start, mate_start_at), started);
        state.newest = Some((board.generation, sample.seq));
        return (Answer::Proposal(result), 0);
    }
    let mut queries = 0;
    if dragged || stretched {
        let answered = if dragged {
            drag::answer(base, world, sample, state, id, started, &mut queries)
        } else {
            stretch::answer(base, world, sample, state, id, started, &mut queries)
        };
        return match answered {
            Ok(proposal) => (Answer::Proposal(proposal), queries),
            Err(drag::Refusal::Unsupported(text)) => (Answer::Refused(SampleCause::Unsupported, text), queries),
            Err(drag::Refusal::Invalid) => (Answer::Refused(SampleCause::Invalid, "what you grabbed has moved"), queries),
        };
    }
    match solve_sample(base, world, sample, state, id, started, &mut queries) {
        Some(proposal) => (Answer::Proposal(proposal), queries),
        None => (
            Answer::Refused(SampleCause::Invalid, "the gesture names a layer, net, width or anchor this board does not have"),
            queries,
        ),
    }
}

/// A route sample's legs and vias as the person fixed them, verified as each is added: the world
/// with them in it, and where the head starts (or why a via could not be placed).
pub(super) struct Legs<'a> {
    pub(super) net: NetId,
    pub(super) class: ClassId,
    pub(super) w: Nm,
    pub(super) next: u32,
    pub(super) over: World<'a>,
    pub(super) items: Vec<Item>,
    pub(super) collisions: Vec<Collision>,
    pub(super) start: P,
    pub(super) start_anchor: Anchor,
    pub(super) blocked: Option<Blocked>,
}

/// A route sample's copper: its items in id order (vias, legs, the head), what they meet, and
/// the head.
pub(super) struct Routed {
    pub(super) items: Vec<Item>,
    pub(super) collisions: Vec<Collision>,
    pub(super) head: HeadAnswer,
}

fn solve_sample(
    base: &Base, world: &RBase, sample: &GestureSample, state: &mut GestureState, id: ProposalId, started: Instant,
    queries: &mut usize,
) -> Option<RouteProposal> {
    let gesture = &sample.gesture;
    let board = &base.board;
    let legs = legs(world, board, &base.rules, gesture)?;
    let source = ProposalSource::Gesture { id: sample.id, seq: sample.seq, tier: Tier::Stretch };
    // A finishing sample's second answer is its Finish (`finish`); a shoving one's, its wave's.
    if gesture.mode == Mode::Shove && !gesture.finish && legs.blocked.is_none() {
        if let Some(pushed) = push::answer(base, world, sample, state, id, started, queries, &legs) {
            state.newest = Some((board.generation, sample.seq));
            return Some(pushed);
        }
    }
    let routed = walk_head(legs, board, &base.rules, &mut state.hulls, gesture, queries)?;
    state.newest = Some((board.generation, sample.seq));
    let ops = routed.items.into_iter().map(BoardOp::Add).collect();
    Some(proposal(base, source, id, started, (ops, routed.collisions, routed.head), !gesture.finish))
}

/// The route the sample draws in Walk mode, laid round everything (a Shove-mode sample that cannot
/// push a track falls back to it). `None` when the gesture names what the board does not have.
pub(super) fn walked(world: &RBase, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, gesture: &Gesture) -> Option<Routed> {
    let legs = legs(world, board, rules, gesture)?;
    walk_head(legs, board, rules, hulls, gesture, &mut 0)
}

/// The sample's legs and vias, laid and verified in order.
fn legs<'a>(world: &'a RBase, board: &Board, rules: &RuleMatrix, gesture: &Gesture) -> Option<Legs<'a>> {
    world.layer_index(gesture.layer)?;
    let net = gesture.net.or_else(|| anchor_net(board, gesture.start))?;
    let class = board.nets.get(net.ix())?.class;
    let w = match gesture.width {
        WidthRule::Fixed(w) => w,
        WidthRule::Rule => rules.class(class).track.unwrap_or(0).max(rules.track_min(class)),
    };
    if w <= 0 { return None; }
    let layers = world.layers();
    let span = (*layers.first()?, *layers.last()?);
    let mut next = board.next_item;
    let mut over = World::new(world);
    let mut items = Vec::new();
    let mut collisions = Vec::new();
    let mut start = gesture.start_at;
    let mut start_anchor = gesture.start;
    let mut blocked = None;
    let mut expected_layer = None;
    for leg in &gesture.legs {
        world.layer_index(leg.layer)?;
        if leg.w <= 0 || expected_layer.is_some_and(|layer| layer != leg.layer) { return None; }
        let mut line = points(&leg.path)?;
        if line.first().copied() != Some(start) {
            line[0] = start;
            if let Some(&end) = line.get(1) {
                let corner = octi::corners(start, end)[0];
                if corner != start && corner != end { line.insert(1, corner); }
            }
        }
        let mut placed_via = None;
        if let Some(to) = leg.via_to {
            world.layer_index(to)?;
            let via_id = ItemId(next);
            match via::place_via(&line, &over, rules, board, net, class, via_id) {
                Some((at, segment)) => {
                    allocate(&mut next)?;
                    line.truncate(segment + 1);
                    line.push(at);
                    line.dedup();
                    let size = rules.class(class).via.unwrap_or_else(|| rules.via(class));
                    placed_via = Some(Via {
                        id: via_id, net, at, drill: size.drill, pad: size.pad, span, kind: ViaKind::Through,
                        tent: Tent::Both, backdrill: None, locked: false, origin: Origin::Authored, flash: Flash::All,
                    });
                }
                None => blocked = Some(Blocked { at: leg.path.end(), cause: BlockCause::Via, have: 0, need: 0 }),
            }
        }
        let via_missing = leg.via_to.is_some() && placed_via.is_none();
        let end = placed_via.as_ref().map_or(Anchor::Free, |v| Anchor::Via(v.id));
        // The via's id precedes its leg's: ops are in id order and name only earlier items. A
        // one-point leg (a via where the head starts) lays no track.
        let fixed = match line.len() >= 2 {
            true => Some(track(allocate(&mut next)?, net, leg.layer, leg.w, path(&line, start), start_anchor, end)),
            false => None,
        };
        start = line.last().copied()?;
        start_anchor = match (&placed_via, &fixed) {
            (Some(v), _) => Anchor::Via(v.id),
            (None, _) if via_missing => Anchor::Free,
            (None, Some(fixed)) => end_anchor(fixed),
            (None, None) => start_anchor,
        };
        if let Some(v) = placed_via { lay(&mut over, rules, board, Item::Via(v), &mut collisions, &mut items); }
        if let Some(fixed) = fixed { lay(&mut over, rules, board, fixed, &mut collisions, &mut items); }
        // Later legs would hang off a via that is not there.
        if via_missing { break; }
        expected_layer = Some(leg.via_to.unwrap_or(leg.layer));
    }
    if blocked.is_none() && expected_layer.is_some_and(|layer| layer != gesture.layer) { return None; }

    Some(Legs { net, class, w, next, over, items, collisions, start, start_anchor, blocked })
}

/// Where the head ends, and its reference: the target when the sample snapped to one this board
/// has on the head's net and layer, else the cursor; the trail from `start` to it.
pub(super) fn head_input(board: &Board, gesture: &Gesture, net: NetId, start: P) -> (Anchor, P, Vec<P>) {
    let end = gesture.target.and_then(|anchor| target(board, anchor, net, gesture.layer).map(|p| (anchor, p)));
    let (anchor, end) = end.unwrap_or((Anchor::Free, gesture.cursor));
    let mut trail = if start == end { vec![start] } else { gesture.trail.clone() };
    if trail.first().copied() != Some(start) { trail.insert(0, start); }
    if trail.last().copied() != Some(end) { trail.push(end); }
    (anchor, end, trail)
}

/// The head laid after `legs`, round everything (Walk).
fn walk_head(legs: Legs, board: &Board, rules: &RuleMatrix, hulls: &mut HullCache, gesture: &Gesture, queries: &mut usize) -> Option<Routed> {
    let Legs { net, class, w, mut next, over, mut items, mut collisions, start, start_anchor, mut blocked } = legs;
    let mut end_anchor = Anchor::Free;
    let mut head_points = vec![start];
    if blocked.is_none() {
        let (anchor, end, trail) = head_input(board, gesture, net, start);
        let input = BandInput { s: start, e: end, reference: &trail, tile: tile_size(w, rules) };
        let mut obstacles = WorldObstacles::new(&over, rules, gesture.layer, net, class, w, ItemId(next), hulls);
        let band = make_legal(&input, &mut obstacles);
        let mut band = tighten(band, &input, &mut obstacles);
        if obstacles.failed() {
            band.points = vec![start];
            band.blocked = Some(Blocked { at: start, cause: BlockCause::Unsettled, have: 0, need: 0 });
        }
        *queries = band.stats.queries;
        blocked = band.blocked;
        head_points = band.points;
        if blocked.is_none() { end_anchor = anchor; }
    }
    head_points.dedup();
    let head_path = path(&head_points, start);
    let head_length = head_path.length().round() as Nm;
    if head_points.len() >= 2 {
        let head_id = allocate(&mut next)?;
        let head = track(head_id, net, gesture.layer, w, head_path.clone(), start_anchor, end_anchor);
        collisions.extend(verify(&over, rules, board, &new_pieces(board, &head)));
        items.push(head);
    }
    let head = HeadAnswer { path: head_path, layer: gesture.layer, w, end: end_anchor, length: head_length, blocked, unpushed: None };
    Some(Routed { items, collisions, head })
}

/// A route answer from `source`: its ops (the route's `Add`s, then any pushed tracks'), what they
/// meet and its head, with the route's metrics; `last` when no other answer follows for its sample.
pub(super) fn proposal(
    base: &Base, source: ProposalSource, id: ProposalId, started: Instant, (ops, collisions, head): (Vec<BoardOp>, Vec<Collision>, HeadAnswer),
    last: bool,
) -> RouteProposal {
    let mut metrics = RouteMetrics::default();
    let mut direction = None;
    for op in &ops {
        let BoardOp::Add(item) = op else { continue };
        match item {
            Item::Track(t) => {
                metrics.length = metrics.length.saturating_add(t.path.length().round() as i64);
                for (a, seg) in t.path.steps() {
                    let b = seg.end();
                    if a == b { continue; }
                    let d = (t.layer, (b.x as i64 - a.x as i64).signum(), (b.y as i64 - a.y as i64).signum());
                    if direction.is_some_and(|(layer, dx, dy)| layer == t.layer && (dx, dy) != (d.1, d.2)) {
                        metrics.bends += 1;
                    }
                    direction = Some(d);
                }
            }
            Item::Via(_) => { metrics.vias += 1; direction = None; }
            _ => {}
        }
    }
    RouteProposal {
        pair: None,
        id, source, unit: base.unit, base_generation: base.board.generation, base_hash: base.hash,
        ops, checked: None, metrics, verified: collisions.is_empty(), collisions, open: Vec::new(), head: Some(head),
        solve_us: started.elapsed().as_micros().min(u32::MAX as u128) as u32,
        last, label: String::new(),
    }
}
