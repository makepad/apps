//! A board press starts a route, a segment drag, an item hold, or the existing canvas action.
//! A routing drag keeps its gesture, samples, newest worker answer and release sequence here.
//! The UI thread never computes a route: the route worker answers each sample. A segment drag's
//! cursor is the pointer plus its grip (the press's offset to the segment), so the segment moves
//! by exactly the pointer's offset and springs back when the pointer returns; it never snaps.
//! `pour_cut` says what a gesture's preview cuts off other nets' pours.
//!
//! Cost: classification and snapping are O(hits) on an indexed board; `Board::locate` falls
//! back to O(items) after direct list edits. Pad classification also visits the copper stack.
//! Segment projection is O(1). Trail updates are amortised O(1); samples copy the trail since
//! the last leg and all fixed leg geometry. Via keys copy the head and scan the copper stack;
//! width keys are O(log widths), posture and answer ordering O(1). Width collection sorts
//! O(classes) entries in O(classes log classes).

use super::nodes::Pressed;
use super::pick::Hit;
use super::scene::Role;
use fab_elec_board::{Anchor, Board, ConnItem, Connectivity, ItemKind};
use fab_elec_core::{BoardId, ClassId, ItemId, LayerId, NetId, Nm, OnTrack, PadIx, Path, Seg, P};
use fab_elec_route::{
    Gesture, GestureId, GestureKind, GestureSample, Leg, Posture, ProposalSource, RouteProposal,
    Tier, WidthRule, Xform,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Draw copper from a pad with a net or a track's end.
    Route { start: Anchor, at: P, net: NetId, layer: LayerId },
    /// Move a segment; its neighbours walk round it.
    Seg { track: ItemId, seg: u32, net: NetId, layer: LayerId, at: P },
    /// Move a part or via, keeping its connections.
    Hold(ItemId),
    /// Keep the pane's existing canvas behaviour.
    Nothing,
}

/// What a press starts, from what it takes in order (`BoardItems::pressed`): the first pad, via or
/// track, or part body, decides. A track's end starts a route only when the press is inside the
/// track's width there, or, with the Route tool armed, within the pick radius `tol` of it.
/// O(presses + copper layers) with indexed items.
pub(crate) fn classify(board: &Board, pressed: &[Pressed], p: P, tol: Nm, active: LayerId, route_tool: bool) -> Press {
    for pressed in pressed {
        match *pressed {
            Pressed::Copper(hit) if matches!(hit.tag.role, Role::Pad | Role::Via | Role::Track) => {
                return copper(board, &hit, p, if route_tool { tol } else { 0 }, active);
            }
            Pressed::Body(id) if matches!(board.locate(id), Some((ItemKind::Placed, _))) => return Press::Hold(id),
            _ => {}
        }
    }
    Press::Nothing
}

/// Only the frontmost hit can start a Tune. In particular a pad or via over
/// a track owns its press, even if the track would otherwise be eligible.
pub(crate) fn tune_hit(board: &Board, hits: &[Pressed], p: P) -> Option<(ItemId, OnTrack, usize)> {
    let Pressed::Copper(hit) = hits.first()? else { return None };
    if hit.tag.role != Role::Track { return None; }
    let (ItemKind::Track, i) = board.locate(hit.tag.item)? else { return None };
    let t = &board.tracks[i];
    fab_elec_route::meander::uniform_width(t).ok()?;
    let seg = hit.tag.sub as usize;
    let Seg::Line(end) = *t.path.segs.get(seg)? else { return None };
    let start = if seg == 0 { t.path.start } else { t.path.segs[seg - 1].end() };
    if start == end { return None; }
    let far = p.dist2(end) < p.dist2(start);
    Some((t.id, OnTrack { seg: hit.tag.sub, frac: if far { OnTrack::ONE } else { 0 } }, seg + usize::from(far)))
}

/// The vertex nearest the projection onto the grabbed straight run's infinite line.
/// A sideways motion alone leaves the projection (and hence the span) fixed.
fn projected_vertex(path: &Path, seg: usize, p: P) -> Option<usize> {
    let end = path.segs.get(seg)?.end();
    let a = if seg == 0 { path.start } else { path.segs[seg - 1].end() };
    let delta = |p: P| (f64::from(p.x) - f64::from(a.x), f64::from(p.y) - f64::from(a.y));
    let ((dx, dy), (x, y)) = (delta(end), delta(p));
    let along = (dx * x + dy * y) / (dx * dx + dy * dy);
    let distance = |v: P| {
        let (x, y) = delta(v);
        (x - dx * along).powi(2) + (y - dy * along).powi(2)
    };
    std::iter::once(path.start).chain(path.segs.iter().map(|s| s.end())).enumerate()
        .min_by(|(_, a), (_, b)| distance(*a).total_cmp(&distance(*b))).map(|(i, _)| i)
}

/// What a press on copper `hit` starts; `reach` beyond a track's width still takes its end.
fn copper(board: &Board, hit: &Hit, p: P, reach: Nm, active: LayerId) -> Press {
    match hit.tag.role {
        Role::Pad => {
            let Some((ItemKind::Placed, i)) = board.locate(hit.tag.item) else { return Press::Nothing };
            let placed = &board.placed[i];
            let Ok(pad) = u16::try_from(hit.tag.sub).map(PadIx) else { return Press::Nothing };
            let Some(net) = placed.pad_nets.get(pad.ix()).copied().flatten() else { return Press::Hold(placed.id) };
            let Some(at) = placed.pad_at(pad) else { return Press::Nothing };
            let layer = if placed.pad_layers(pad, board.copper_set(), board.copper_count()).has(active) {
                active
            } else {
                hit.layer
            };
            Press::Route { start: Anchor::Pad { placed: placed.id, pad }, at, net, layer }
        }
        Role::Via => Press::Hold(hit.tag.item),
        Role::Track => {
            let Some((ItemKind::Track, i)) = board.locate(hit.tag.item) else { return Press::Nothing };
            let track = &board.tracks[i];
            let radius = (track.w / 2) as i128 + reach as i128;
            let ends = [
                (track.path.start, OnTrack { seg: 0, frac: 0 }),
                (track.path.end(), OnTrack { seg: track.path.segs.len().saturating_sub(1) as u32, frac: OnTrack::ONE }),
            ];
            for (at, position) in ends {
                if p.dist2(at) <= radius * radius {
                    return Press::Route { start: Anchor::Track { id: track.id, at: position }, at, net: track.net, layer: track.layer };
                }
            }
            Press::Seg { track: track.id, seg: hit.tag.sub, net: track.net, layer: track.layer, at: p }
        }
        _ => Press::Nothing,
    }
}

/// Snap `g.cursor` to the first eligible hit. O(hits) with indexed board items.
pub(crate) fn snap_target(board: &Board, hits: &[Hit], g: &Gesture) -> Option<(Anchor, P, String)> {
    let net = g.net?;
    for hit in hits {
        if hit.layer != g.layer || !matches!(hit.tag.role, Role::Pad | Role::Via | Role::Track) {
            continue;
        }
        let eligible = match g.kind {
            GestureKind::RoutePair { mate, .. } => hit.tag.role == Role::Pad && (hit.net == Some(net) || hit.net == Some(mate)),
            _ => hit.net == Some(net),
        };
        if !eligible {
            continue;
        }
        let target = match (hit.tag.role, board.locate(hit.tag.item)) {
            (Role::Pad, Some((ItemKind::Placed, i))) => {
                let placed = &board.placed[i];
                let Ok(pad) = u16::try_from(hit.tag.sub).map(PadIx) else { continue };
                let Some(at) = placed.pad_at(pad) else { continue };
                (Anchor::Pad { placed: placed.id, pad }, at, format!("{}.{}", placed.refdes, placed.footprint.pads[pad.ix()].no))
            }
            (Role::Via, Some((ItemKind::Via, i))) => {
                let via = &board.vias[i];
                (Anchor::Via(via.id), via.at, "via".into())
            }
            (Role::Track, Some((ItemKind::Track, i))) => {
                let track = &board.tracks[i];
                if matches!(g.start, Anchor::Track { id, .. } if id == track.id) {
                    continue;
                }
                let Some((at, point)) = track_position(&track.path, hit.tag.sub, g.cursor) else { continue };
                (Anchor::Track { id: track.id, at }, point, board.nets[net.ix()].name.clone())
            }
            _ => continue,
        };
        if target.0 != g.start && !matches!(g.kind, GestureKind::RoutePair { mate_start, .. } if target.0 == mate_start) {
            return Some(target);
        }
    }
    None
}

/// Project onto just the picked segment, without scanning the path. O(1).
pub(crate) fn track_position(path: &Path, seg: u32, p: P) -> Option<(OnTrack, P)> {
    let i = seg as usize;
    let step = path.segs.get(i)?;
    let from = if i == 0 { path.start } else { path.segs[i - 1].end() };
    let mid = match *step { Seg::Line(_) => None, Seg::Arc { mid, .. } => Some(mid) };
    let at = OnTrack::along(seg, from, mid, step.end(), p);
    Some((at, path.point_on(at)))
}

/// A new gesture's id, from the process's preview tag counter (`next_tag`): the host files the
/// gesture's answers as the preview tagged `id.0 as u64`, so a gesture and another pane's preview
/// never share a tag (until 2^32 tags).
pub(crate) fn next_gesture_id() -> GestureId {
    GestureId(crate::snapshot::next_tag() as u32)
}

pub(crate) struct Routing {
    pub(crate) id: GestureId,
    pub(crate) g: Gesture,
    /// Samples sent; 0 before the first.
    pub(crate) seq: u64,
    /// The newest answer for this gesture and its (base generation, seq, tier).
    pub(crate) answer: Option<Arc<RouteProposal>>,
    pub(crate) answered: Option<(u64, u64, Tier)>,
    /// The snapped target's chip name.
    pub(crate) target: Option<String>,
    /// Released at this seq: commit once an answer for it is in.
    pub(crate) ending: Option<u64>,
    /// A drag's offset from the pointer to the point it grabbed; zero when copper is drawn.
    pub(crate) grip: P,
}

impl Routing {
    fn new(g: Gesture, grip: P) -> Self {
        Self { id: next_gesture_id(), g, seq: 0, answer: None, answered: None, target: None, ending: None, grip }
    }

    pub(crate) fn pair(board: &Board, rules: &fab_elec_board::RuleMatrix, press: Press) -> Result<Option<Self>, String> {
        use fab_elec_route::pair::{mate_anchor, Pair};
        let Press::Route { start: start @ Anchor::Pad { .. }, at, net, layer } = press else { return Ok(None) };
        let Some((mate, _)) = rules.partner(net) else { return Ok(None) };
        let mate_start = mate_anchor(board, Pair::resolve(board, rules, net)?, start)?;
        let mate_start_at = board.anchor_pos(mate_start).ok_or("the mate endpoint has moved")?;
        let mut g = Gesture::new(GestureKind::RoutePair { mate, mate_start, mate_start_at }, board.unit, board.generation, start, at, layer);
        g.net = Some(net);
        Ok(Some(Self::new(g, P::ZERO)))
    }

    pub(crate) fn tune(board: &Board, hit: (ItemId, OnTrack, usize), circuit: Arc<fab_elec_circuit::Circuit>) -> Option<Self> {
        let (track, at, from) = hit;
        let (ItemKind::Track, i) = board.locate(track)? else { return None };
        let t = &board.tracks[i];
        let kind = GestureKind::Tune { track, from, to: from, circuit };
        let mut g = Gesture::new(kind, board.unit, board.generation, Anchor::Track { id: track, at }, t.path.point_on(at), t.layer);
        g.net = Some(t.net);
        Some(Self::new(g, P::ZERO))
    }

    pub(crate) fn set_span_end(&mut self, board: &Board, p: P) {
        if let (GestureKind::Tune { to, track, .. }, Anchor::Track { at, .. }) = (&mut self.g.kind, self.g.start) {
            if let Some((ItemKind::Track, i)) = board.locate(*track) {
                if let Some(end) = projected_vertex(&board.tracks[i].path, at.seg as usize, p) { *to = end; }
            }
        }
    }

    /// O(1) with indexed board items, including projection onto a dragged segment.
    pub(crate) fn start(press: &Press, board: &Board, unit: BoardId) -> Option<Routing> {
        let (kind, start, at, net, layer, grip) = match *press {
            Press::Route { start, at, net, layer } => (GestureKind::Route, start, at, net, layer, P::ZERO),
            Press::Seg { track, seg, net, layer, at } => {
                let (ItemKind::Track, i) = board.locate(track)? else { return None };
                let (position, point) = track_position(&board.tracks[i].path, seg, at)?;
                (GestureKind::DragSeg { track, seg }, Anchor::Track { id: track, at: position }, point, net, layer, point.sub(at))
            }
            _ => return None,
        };
        let mut g = Gesture::new(kind, unit, board.generation, start, at, layer);
        g.net = Some(net);
        Some(Self::new(g, grip))
    }

    /// Amortised O(1) trail update, followed by the sample's gesture copy.
    pub(crate) fn moved(&mut self, p: P, tol: Nm, target: Option<(Anchor, P, String)>) -> GestureSample {
        if !self.draws() {
            self.g.cursor = p.add(self.grip);
            return self.sample();
        }
        self.g.push_trail(p, tol);
        self.g.target = target.as_ref().map(|t| t.0);
        if !matches!(self.g.kind, GestureKind::RoutePair { .. }) {
            self.g.cursor = target.as_ref().map_or(p, |t| t.1);
        }
        self.target = target.map(|t| t.2);
        self.sample()
    }

    /// A part or via held at `at`, its attached tracks stretched by the worker (landing 4): the
    /// gesture's motion is the item's. None for any other item. O(1) with indexed board items.
    pub(crate) fn hold(board: &Board, unit: BoardId, item: ItemId, at: P) -> Option<Routing> {
        let kind = match board.locate(item)? {
            (ItemKind::Via, _) => GestureKind::DragVia(item),
            (ItemKind::Placed, _) => GestureKind::DragParts,
            _ => return None,
        };
        let g = Gesture::new(kind, unit, board.generation, Anchor::Free, at, LayerId::F_CU);
        Some(Self::new(g, P::ZERO))
    }

    /// The held `item` moved by `by` from its committed place, as the next sample. O(1).
    pub(crate) fn held(&mut self, item: ItemId, by: P) -> GestureSample {
        self.g.moved = vec![(item, Xform::translate(by))];
        self.g.cursor = self.g.start_at.add(by);
        self.sample()
    }

    /// Released where the last sample put it: commit once that sample is answered. False when no
    /// sample was sent (nothing moved). O(1).
    pub(crate) fn release_held(&mut self) -> bool {
        self.ending = (self.seq > 0).then_some(self.seq);
        self.ending.is_some()
    }

    /// Whether this gesture draws copper (snaps, keeps a trail, takes V, W and /), not drags it.
    pub(crate) fn draws(&self) -> bool {
        matches!(self.g.kind, GestureKind::Route | GestureKind::RoutePair { .. })
    }

    /// O(trail since the last leg + fixed leg geometry), copied by `Gesture::clone`.
    pub(crate) fn sample(&mut self) -> GestureSample {
        self.seq += 1;
        GestureSample { id: self.id, seq: self.seq, gesture: self.g.clone() }
    }

    /// Keeps `p` when it is newer than the kept answer by (base generation, seq, tier): an answer
    /// on a newer board supersedes every answer on an older one, and later samples name that board.
    pub(crate) fn offer(&mut self, p: &Arc<RouteProposal>) -> bool {
        let ProposalSource::Gesture { id, seq, tier } = p.source else { return false };
        let key = (p.base_generation, seq, tier);
        if id != self.id || self.answered.is_some_and(|kept| key <= kept) {
            return false;
        }
        self.answer = Some(Arc::clone(p));
        self.answered = Some(key);
        self.g.base_generation = self.g.base_generation.max(p.base_generation);
        true
    }

    /// The newest answer is older than the newest sample (or none is in): the pane draws a rubber
    /// line from the head's start to the cursor and tints the old head.
    pub(crate) fn stale(&self) -> bool {
        self.seq > 0 && self.answered.is_none_or(|(_, seq, _)| seq < self.seq)
    }

    /// Where the head starts: the last fixed leg's end, else the press.
    pub(crate) fn head_start(&self) -> P {
        self.g.legs.last().map_or(self.g.start_at, |leg| leg.path.end())
    }

    /// Same cost as `moved`: a trail update and gesture copy.
    pub(crate) fn release(&mut self, p: P, tol: Nm, target: Option<(Anchor, P, String)>) -> GestureSample {
        let sample = self.moved(p, tol, target);
        self.ending = Some(self.seq);
        sample
    }

    /// The release sample is answered by its last answer (`RouteProposal::last`: no further tier
    /// follows, such as a Finish after the stretch). O(1).
    pub(crate) fn commit_ready(&self) -> bool {
        self.ending.is_some_and(|end| self.answered.is_some_and(|(_, seq, _)| seq >= end))
            && self.answer.as_ref().is_some_and(|answer| answer.last)
    }

    /// F while copper is drawn: the router lays the rest of the net from where the head ends, as
    /// the next sample's second answer, and the pointer no longer moves the head. O(1).
    pub(crate) fn finish(&mut self) -> Result<(), &'static str> {
        if !self.draws() {
            return Err("only drawn copper is finished");
        }
        if self.g.finish {
            return Err("the router is finishing this track");
        }
        self.g.finish = true;
        Ok(())
    }

    /// O(copper layers + head segments), plus amortised O(1) to append the fixed leg.
    pub(crate) fn via(&mut self, copper: &[LayerId], down: bool) -> Result<LayerId, &'static str> {
        let head = self.answer.as_ref().and_then(|p| p.head.as_ref()).ok_or("no head to fix yet")?;
        let i = copper.iter().position(|&layer| layer == self.g.layer).ok_or("current layer is not copper")?;
        let next = copper[if down { (i + 1) % copper.len() } else { (i + copper.len() - 1) % copper.len() }];
        self.g.click(Leg { path: head.path.clone(), layer: head.layer, w: head.w, via_to: Some(next) });
        Ok(next)
    }

    /// Fixes the width carried by the next gesture sample, in O(1).
    pub(crate) fn set_width(&mut self, w: Nm) {
        self.g.width = WidthRule::Fixed(w);
    }

    /// O(log widths) in the sorted, deduplicated width list.
    pub(crate) fn width(&mut self, widths: &[Nm], rule_w: Nm, up: bool) -> Result<Nm, &'static str> {
        let current = match self.g.width { WidthRule::Rule => rule_w, WidthRule::Fixed(w) => w };
        let w = if up {
            *widths.get(widths.partition_point(|&w| w <= current)).ok_or("widest track width")?
        } else {
            let i = widths.partition_point(|&w| w < current).checked_sub(1).ok_or("narrowest track width")?;
            widths[i]
        };
        self.g.width = if w == rule_w { WidthRule::Rule } else { WidthRule::Fixed(w) };
        Ok(w)
    }

    pub(crate) fn posture(&mut self) -> Posture {
        self.g.posture = match self.g.posture {
            Posture::Auto => Posture::StraightFirst,
            Posture::StraightFirst => Posture::DiagonalFirst,
            Posture::DiagonalFirst => Posture::Auto,
        };
        self.g.posture
    }
}

/// What a gesture's preview cuts off other nets (D7), as its chip says it, with the parts to mark:
/// each net other than `own` with more pieces in `preview` than in `committed` names the pads it
/// leaves off its pours ("cuts GND: 2 pads off the pour"), or its new open lines when a pour
/// itself parts. Tracks win and fills are derived, so the commit lays no repair copper; DRC lists
/// the cut after it. O(pieces of both connectivities).
pub(crate) fn pour_cut(board: &Board, committed: &Connectivity, preview: &Connectivity, own: Option<NetId>) -> Option<(String, Vec<ItemId>)> {
    // Per net: its pieces, and its pads in pieces without a pour.
    type Tally = BTreeMap<NetId, (usize, BTreeSet<(ItemId, PadIx)>)>;
    let tally = |conn: &Connectivity| {
        let mut out = Tally::new();
        for cluster in &conn.clusters {
            let [net] = cluster.nets[..] else { continue };
            if Some(net) == own {
                continue;
            }
            let entry = out.entry(net).or_default();
            entry.0 += 1;
            if !cluster.items.iter().any(|item| matches!(item, ConnItem::Zone { .. })) {
                entry.1.extend(cluster.items.iter().filter_map(|item| match *item {
                    ConnItem::Pad { placed, pad } => Some((placed, pad)),
                    _ => None,
                }));
            }
        }
        out
    };
    let (before, after) = (tally(committed), tally(preview));
    let none = (0, BTreeSet::new());
    let mut words = Vec::new();
    let mut marked = Vec::new();
    for (net, (pieces, off)) in &after {
        let (was, was_off) = before.get(net).unwrap_or(&none);
        if pieces <= was {
            continue;
        }
        let name = board.nets.get(net.ix()).map_or_else(|| format!("net {}", net.0), |n| n.name.clone());
        let cut: Vec<_> = off.difference(was_off).collect();
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        words.push(if cut.is_empty() {
            let lines = pieces - was;
            format!("cuts {name}: {lines} open line{}", plural(lines))
        } else {
            format!("cuts {name}: {} pad{} off the pour", cut.len(), plural(cut.len()))
        });
        marked.extend(cut.iter().map(|(placed, _)| *placed));
    }
    marked.dedup();
    (!words.is_empty()).then(|| (words.join("; "), marked))
}

/// O(classes log classes) time and O(classes) storage.
pub(crate) fn track_widths(board: &Board, rule_of: impl Fn(ClassId) -> Nm, base: Nm) -> Vec<Nm> {
    let mut widths = Vec::with_capacity(board.classes.len() + 1);
    widths.push(base);
    widths.extend(board.classes.iter().map(|c| rule_of(c.id)));
    widths.sort_unstable();
    widths.dedup();
    widths
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::scene::Tag;
    use fab_elec_board::connect::Cluster;
    use fab_elec_board::zone::TileKey;
    use fab_elec_route::{HeadAnswer, JobId, ProposalId};

    fn fixture() -> Board {
        let design = fab_elec_fixtures::ledctl();
        design.units.iter().find_map(|unit| unit.board.as_deref()).expect("ledctl has a board").clone()
    }

    fn hit(layer: LayerId, item: ItemId, sub: u32, role: Role, net: Option<NetId>) -> Hit {
        Hit { layer, tag: Tag { item, sub, role }, net, index: 0, poly: false, dist: 0 }
    }

    fn pad_hits(board: &Board) -> Vec<Hit> {
        let mut hits = Vec::new();
        let copper = board.copper_set();
        let count = board.copper_count();
        for placed in &board.placed {
            for (i, &net) in placed.pad_nets.iter().enumerate() {
                if net.is_some() {
                    for layer in placed.pad_layers(PadIx(i as u16), copper, count).iter() {
                        hits.push(hit(layer, placed.id, i as u32, Role::Pad, net));
                    }
                }
            }
        }
        hits
    }

    fn answer(routing: &Routing, seq: u64, tier: Tier) -> Arc<RouteProposal> {
        Arc::new(RouteProposal {
            pair: None,
            id: ProposalId(1),
            source: ProposalSource::Gesture { id: routing.id, seq, tier },
            unit: routing.g.unit,
            base_generation: routing.g.base_generation,
            base_hash: Default::default(),
            ops: Vec::new(),
            checked: None,
            metrics: Default::default(),
            verified: false,
            collisions: Vec::new(),
            open: Vec::new(),
            head: None,
            solve_us: 0,
            last: true,
            label: String::new(),
        })
    }

    #[test]
    fn a_press_starts_what_the_grammar_says() {
        let mut board = fixture();
        let pads = pad_hits(&board);
        let front = *pads.iter().find(|h| {
            h.layer == LayerId::F_CU && pads.iter().any(|other| other.tag == h.tag && other.layer == LayerId::B_CU)
        }).expect("ledctl has a connected pad on both copper layers");
        let (_, placed_i) = board.locate(front.tag.item).expect("the pad belongs to a placement");
        let pad = PadIx(front.tag.sub as u16);
        let at = board.placed[placed_i].pad_at(pad).expect("the pad has a centre");
        let start = Anchor::Pad { placed: front.tag.item, pad };
        assert_eq!(
            classify(&board, &[Pressed::Copper(front)], at.add(P::new(1, 0)), 10, LayerId::B_CU, false),
            Press::Route { start, at, net: front.net.unwrap(), layer: LayerId::B_CU },
            "a connected pad routes from its centre on the active copper layer"
        );

        let smd = *pads.iter().find(|h| {
            h.layer == LayerId::F_CU && !pads.iter().any(|other| other.tag == h.tag && other.layer == LayerId::B_CU)
        }).expect("ledctl has a connected front-only pad");
        let (_, smd_i) = board.locate(smd.tag.item).unwrap();
        let smd_pad = PadIx(smd.tag.sub as u16);
        let smd_at = board.placed[smd_i].pad_at(smd_pad).unwrap();
        assert_eq!(
            classify(&board, &[Pressed::Copper(smd)], smd_at, 10, LayerId::B_CU, false),
            Press::Route { start: Anchor::Pad { placed: smd.tag.item, pad: smd_pad }, at: smd_at, net: smd.net.unwrap(), layer: LayerId::F_CU },
            "a pad off the active layer routes on its own layer"
        );
        board.placed[placed_i].pad_nets[pad.ix()] = None;
        assert_eq!(classify(&board, &[Pressed::Copper(front)], at, 10, front.layer, false), Press::Hold(front.tag.item), "an unconnected pad moves its placement");

        let via = &board.vias[0];
        let via_hit = hit(via.span.0, via.id, 0, Role::Via, Some(via.net));
        assert_eq!(classify(&board, &[Pressed::Copper(via_hit)], via.at, 10, via.span.0, false), Press::Hold(via.id), "a via starts the existing hold");

        board.tracks[0].path = Path::new(P::new(0, 0)).line(P::new(4_000_000, 0)).line(P::new(4_000_000, 4_000_000));
        let track = &board.tracks[0];
        let track_hit = hit(track.layer, track.id, 1, Role::Track, Some(track.net));
        let tol = 25_000;
        let radius = track.w / 2 + tol;
        for (point, near, at, position) in [
            (track.path.start.add(P::new(track.w / 2, 0)), track.path.start.add(P::new(radius, 0)), track.path.start, OnTrack { seg: 0, frac: 0 }),
            (track.path.end().add(P::new(0, track.w / 2)), track.path.end().add(P::new(0, radius)), track.path.end(), OnTrack { seg: 1, frac: OnTrack::ONE }),
        ] {
            let route = Press::Route { start: Anchor::Track { id: track.id, at: position }, at, net: track.net, layer: track.layer };
            assert_eq!(classify(&board, &[Pressed::Copper(track_hit)], point, tol, LayerId::B_CU, false), route,
                "an endpoint within the track's width routes from the exact endpoint and OnTrack position");
            assert_eq!(classify(&board, &[Pressed::Copper(track_hit)], near, tol, LayerId::B_CU, true), route,
                "with the Route tool armed, an endpoint within width/2 + the pick radius routes");
            assert!(matches!(classify(&board, &[Pressed::Copper(track_hit)], near, tol, LayerId::B_CU, false), Press::Seg { .. }),
                "without it, the pick radius does not make a near miss a track's end: the segment drags");
        }
        let middle = P::new(4_000_000, 2_000_000);
        let segment_press = Press::Seg { track: track.id, seg: 1, net: track.net, layer: track.layer, at: middle };
        assert_eq!(classify(&board, &[Pressed::Copper(track_hit)], middle, tol, track.layer, false), segment_press, "the middle drags the hit's segment");
        let outside = track.path.start.add(P::new(radius, radius));
        assert_eq!(
            classify(&board, &[Pressed::Copper(track_hit)], outside, tol, track.layer, false),
            Press::Seg { track: track.id, seg: 1, net: track.net, layer: track.layer, at: outside },
            "endpoint proximity uses a circle, not an axis-aligned box"
        );
        let distant = P::new(Nm::MIN, Nm::MAX);
        assert_eq!(
            classify(&board, &[Pressed::Copper(track_hit)], distant, tol, track.layer, false),
            Press::Seg { track: track.id, seg: 1, net: track.net, layer: track.layer, at: distant },
            "endpoint distance stays exact across the nm coordinate range"
        );
        let zone = hit(track.layer, board.zones[0].id, 0, Role::Zone, None);
        assert_eq!(classify(&board, &[Pressed::Pour(zone), Pressed::Copper(track_hit)], middle, tol, track.layer, false), segment_press, "a zone before a track is skipped");
        assert_eq!(classify(&board, &[Pressed::Copper(via_hit), Pressed::Copper(track_hit)], middle, tol, track.layer, false), Press::Hold(via_hit.tag.item), "the first relevant copper hit wins");
        assert_eq!(classify(&board, &[Pressed::Body(front.tag.item)], at, tol, front.layer, false), Press::Hold(front.tag.item), "a body press off the pads holds the part");
        assert_eq!(classify(&board, &[], at, tol, front.layer, false), Press::Nothing, "empty canvas keeps its existing behaviour");
        assert_eq!(classify(&board, &[Pressed::Pour(zone)], at, tol, front.layer, false), Press::Nothing, "a zone alone starts no item gesture");
        let hole = hit(front.layer, board.holes[0].id, 0, Role::HoleRing, None);
        assert_eq!(classify(&board, &[Pressed::Copper(hole), Pressed::Body(hole.tag.item)], at, tol, front.layer, false), Press::Nothing, "a hole starts no routing or hold gesture");
    }

    #[test]
    fn the_cursor_snaps_only_to_its_own_net_and_layer() {
        let mut board = fixture();
        let pads = pad_hits(&board);
        let pad_hit = pads[0];
        let (_, i) = board.locate(pad_hit.tag.item).unwrap();
        let placed = &board.placed[i];
        let pad = PadIx(pad_hit.tag.sub as u16);
        let anchor = Anchor::Pad { placed: placed.id, pad };
        let at = placed.pad_at(pad).unwrap();
        let name = format!("{}.{}", placed.refdes, placed.footprint.pads[pad.ix()].no);
        let mut g = Gesture::new(GestureKind::Route, board.unit, board.generation, Anchor::Free, P::ZERO, pad_hit.layer);
        g.net = pad_hit.net;
        assert_eq!(snap_target(&board, &[pad_hit], &g), Some((anchor, at, name.clone())), "a same-net pad snaps to its centre and refdes.pad name");
        let foreign = *pads.iter().find(|h| h.layer == g.layer && h.net != g.net).expect("ledctl has pads on other nets");
        assert_eq!(snap_target(&board, &[foreign], &g), None, "another net's pad is not a target");
        let mut other_layer = pad_hit;
        other_layer.layer = if g.layer == LayerId::F_CU { LayerId::B_CU } else { LayerId::F_CU };
        assert_eq!(snap_target(&board, &[other_layer], &g), None, "the right net on another layer is not a target");
        assert_eq!(snap_target(&board, &[foreign, other_layer, pad_hit], &g), Some((anchor, at, name)), "ineligible hits do not hide the next eligible pad");
        g.start = anchor;
        assert_eq!(snap_target(&board, &[pad_hit], &g), None, "the starting pad is not a target");

        let via = &board.vias[0];
        let via_hit = hit(via.span.0, via.id, 0, Role::Via, Some(via.net));
        g.net = Some(via.net);
        g.layer = via_hit.layer;
        g.start = Anchor::Free;
        assert_eq!(snap_target(&board, &[via_hit], &g), Some((Anchor::Via(via.id), via.at, "via".into())), "a via target uses its centre and chip name");
        g.start = Anchor::Via(via.id);
        assert_eq!(snap_target(&board, &[via_hit], &g), None, "the starting via is not a target");

        board.tracks[0].path = Path::new(P::ZERO).line(P::new(4_000_000, 0)).line(P::new(4_000_000, 4_000_000));
        let track = &board.tracks[0];
        let track_hit = hit(track.layer, track.id, 1, Role::Track, Some(track.net));
        g.net = Some(track.net);
        g.layer = track.layer;
        g.start = Anchor::Free;
        g.cursor = P::new(4_100_000, 2_000_000);
        let position = OnTrack { seg: 1, frac: OnTrack::ONE / 2 };
        assert_eq!(
            snap_target(&board, &[track_hit], &g),
            Some((Anchor::Track { id: track.id, at: position }, P::new(4_000_000, 2_000_000), board.nets[track.net.ix()].name.clone())),
            "track snapping projects the cursor onto the picked segment and names its net"
        );
        g.start = Anchor::Track { id: track.id, at: OnTrack { seg: 0, frac: 0 } };
        assert_eq!(snap_target(&board, &[track_hit], &g), None, "every segment of the starting track is excluded");
        g.start = Anchor::Free;
        g.net = None;
        assert_eq!(snap_target(&board, &[track_hit], &g), None, "a gesture without a net cannot snap to copper");
    }

    #[test]
    fn tune_picks_a_straight_run_and_projects_onto_its_vertices() {
        let mut b = fixture();
        let t = &mut b.tracks[0];
        t.path = Path::new(P::ZERO).line(P::mm(4., 0.)).line(P::mm(9., 0.));
        t.locked = false;
        t.widths = None;
        let track = Pressed::Copper(hit(t.layer, t.id, 0, Role::Track, Some(t.net)));
        let press = P::mm(0.6, 0.);
        let name = t.id;
        let pad = Pressed::Copper(pad_hits(&b)[0]);
        let via = &b.vias[0];
        let via = Pressed::Copper(hit(LayerId::F_CU, via.id, 0, Role::Via, Some(via.net)));
        assert!(tune_hit(&b, &[pad, track], press).is_none());
        assert!(tune_hit(&b, &[via, track], press).is_none());
        let hit = tune_hit(&b, &[track], press).unwrap();
        assert_eq!((hit.0, hit.2), (name, 0));
        let circuit = Arc::default();
        let mut routing = Routing::tune(&b, hit, Arc::clone(&circuit)).unwrap();
        for (p, end) in [(P::mm(4., 1.), 1), (P::mm(4., 10.), 1), (P::mm(8., -2.), 2)] {
            routing.set_span_end(&b, p);
            let sample = routing.moved(p, 0, None);
            assert!(matches!(sample.gesture.kind, GestureKind::Tune { track, from: 0, to, circuit: held }
                if track == name && to == end && Arc::ptr_eq(&held, &circuit)));
        }
        b.tracks[0].locked = true;
        assert!(tune_hit(&b, &[track], press).is_none());
    }

    #[test]
    fn the_routing_state_follows_samples_answers_and_keys() {
        let mut board = fixture();
        let pad_hit = pad_hits(&board)[0];
        let press = classify(&board, &[Pressed::Copper(pad_hit)], P::ZERO, 0, pad_hit.layer, false);
        let mut routing = Routing::start(&press, &board, board.unit).expect("a pad route starts routing");
        assert_eq!(routing.seq, 0, "a new gesture has sent no samples");
        assert_eq!((routing.g.unit, routing.g.base_generation, routing.g.net), (board.unit, board.generation, pad_hit.net), "a route captures its board, generation and net");
        assert!(matches!(routing.g.kind, GestureKind::Route), "a pad press creates a Route gesture");
        assert!(routing.answer.is_none() && routing.answered.is_none() && routing.target.is_none() && routing.ending.is_none(), "a new gesture has no answer, chip or release");
        assert!(!routing.commit_ready(), "an unreleased gesture is not ready to commit");
        let copper = board.stackup.copper();
        assert_eq!(routing.via(&copper, true), Err("no head to fix yet"), "a via needs an answered head");

        let first = routing.g.start_at.add(P::new(1_000, 0));
        let snapped = board.vias[0].at;
        let target = Anchor::Via(board.vias[0].id);
        let one = routing.moved(first, 10, Some((target, snapped, "via".into())));
        assert_eq!((one.id, one.seq), (routing.id, 1), "the first move sends sequence 1 for this gesture");
        assert_eq!((one.gesture.target, one.gesture.cursor, routing.target.as_deref()), (Some(target), snapped, Some("via")), "a snapped sample ends on the target and retains its chip name");
        assert_eq!(one.gesture.trail.last(), Some(&first), "the trail records the unsnapped pointer");
        let second = first.add(P::new(1_000, 1_000));
        let two = routing.moved(second, 10, None);
        assert_eq!(two.seq, 2, "the second move sends sequence 2");
        assert_eq!((two.gesture.target, two.gesture.cursor, routing.target.as_deref()), (None, second, None), "leaving a target clears its anchor and chip");

        assert!(routing.stale(), "two samples and no answer: the rubber line is drawn");
        let stretch = answer(&routing, 1, Tier::Stretch);
        assert!(routing.offer(&stretch), "the first answer is accepted even when a newer sample is pending");
        assert!(routing.stale(), "an answer to an older sample keeps the rubber line");
        assert!(!routing.offer(&stretch), "an equal answer key is not newer");
        let full = answer(&routing, 1, Tier::Full);
        assert!(routing.offer(&full), "Full supersedes Stretch for the same sample");
        assert!(!routing.offer(&stretch), "Stretch cannot replace Full for the same sample");
        let mut foreign = answer(&routing, 2, Tier::Full);
        let other_id = next_gesture_id();
        assert_ne!(other_id, routing.id, "process-wide gesture ids are not reused");
        Arc::make_mut(&mut foreign).source = ProposalSource::Gesture { id: other_id, seq: 2, tier: Tier::Full };
        assert!(!routing.offer(&foreign), "an answer from another gesture is rejected");
        Arc::make_mut(&mut foreign).source = ProposalSource::Job { job: JobId(1), step: 2 };
        assert!(!routing.offer(&foreign), "job proposals do not answer a gesture");
        assert!(Arc::ptr_eq(routing.answer.as_ref().unwrap(), &full), "rejected answers preserve the retained proposal");
        assert_eq!(routing.via(&copper, true), Err("no head to fix yet"), "an answer without a head cannot fix a via");

        let three = routing.release(second.add(P::new(1_000, 0)), 10, None);
        assert_eq!((three.seq, routing.ending), (3, Some(3)), "release sends a final sample and records its sequence");
        assert!(!routing.commit_ready(), "a stale answer cannot commit a release");
        let head_start = routing.g.start_at;
        let head_end = head_start.add(P::new(1_000_000, 0));
        let head = HeadAnswer { path: Path::new(head_start).line(head_end), layer: routing.g.layer, w: 200_000, end: Anchor::Free, length: 1_000_000, blocked: None, unpushed: None };
        let mut final_answer = answer(&routing, 3, Tier::Stretch);
        Arc::make_mut(&mut final_answer).head = Some(head.clone());
        assert!(routing.offer(&final_answer), "a newer sequence supersedes an older Full tier");
        assert!(routing.commit_ready(), "the released sequence's answer permits a commit");
        assert!(!routing.offer(&full), "an older sequence cannot replace the released answer");
        let generation = routing.g.base_generation;
        assert_eq!(routing.answered, Some((generation, 3, Tier::Stretch)), "the retained key describes the newest accepted answer");
        assert!(!routing.stale(), "the newest sample is answered: no rubber line");
        let mut rebased = answer(&routing, 3, Tier::Stretch);
        Arc::make_mut(&mut rebased).base_generation = generation + 1;
        Arc::make_mut(&mut rebased).head = Some(head.clone());
        assert!(routing.offer(&rebased), "the same sample answered on a newer board supersedes its answer");
        assert_eq!(routing.g.base_generation, generation + 1, "later samples name the board the answer was made on");
        let mut older = answer(&routing, 4, Tier::Full);
        Arc::make_mut(&mut older).base_generation = generation;
        assert!(!routing.offer(&older), "an answer on an older board never replaces one on a newer board");
        assert!(routing.commit_ready(), "the release is answered on the newer board");

        assert_eq!(copper.len(), 2, "ledctl exercises a two-layer via cycle");
        let layer = routing.g.layer;
        let next = *copper.iter().find(|&&candidate| candidate != layer).unwrap();
        assert_eq!(routing.via(&copper, true), Ok(next), "the via key moves to the next copper layer");
        assert_eq!(routing.g.legs, vec![Leg { path: head.path.clone(), layer: head.layer, w: head.w, via_to: Some(next) }], "the via fixes the worker's path, layer and width as a leg");
        assert_eq!((routing.g.layer, routing.g.head_start(), routing.g.trail.clone()), (next, head_end, vec![head_end]), "a fixed via restarts the head and trail on the next layer");
        let after_via = routing.sample();
        assert_eq!(after_via.seq, 4, "the caller sends a fresh sample after a via key");
        let mut next_answer = answer(&routing, after_via.seq, Tier::Full);
        Arc::make_mut(&mut next_answer).head = Some(HeadAnswer { path: Path::new(head_end).line(head_end.add(P::new(0, 1_000_000))), layer: next, ..head });
        assert!(routing.offer(&next_answer), "the new layer's head replaces the previous answer");
        assert!(routing.commit_ready(), "an answer newer than the release also satisfies its sequence");
        assert_eq!(routing.via(&copper, true), Ok(layer), "moving down wraps around the two-layer stack");
        assert_eq!(routing.via(&copper, false), Ok(next), "moving up also wraps around the two-layer stack");

        let rule_w = 200_000;
        assert_eq!(track_widths(&board, |_| rule_w, rule_w), vec![rule_w], "duplicate class and base widths are removed");
        let widths = track_widths(&board, |_| 300_000, rule_w);
        assert_eq!(widths, vec![rule_w, 300_000], "class widths and the base are collected in ascending order");
        assert_eq!(routing.width(&widths, rule_w, true), Ok(300_000), "width up selects the next larger class width");
        assert_eq!(routing.g.width, WidthRule::Fixed(300_000), "a non-rule width is a fixed override");
        assert_eq!(routing.width(&widths, rule_w, true), Err("widest track width"), "width up reports the widest end");
        assert_eq!(routing.g.width, WidthRule::Fixed(300_000), "the widest refusal keeps the current width");
        assert_eq!(routing.width(&widths, rule_w, false), Ok(rule_w), "width down returns to the rule width");
        assert_eq!(routing.g.width, WidthRule::Rule, "landing on the rule width restores the rule");
        assert_eq!(routing.width(&widths, rule_w, false), Err("narrowest track width"), "width down reports the narrowest end");
        assert_eq!(routing.g.width, WidthRule::Rule, "the narrowest refusal keeps the current rule");
        assert_eq!([routing.posture(), routing.posture(), routing.posture()], [Posture::StraightFirst, Posture::DiagonalFirst, Posture::Auto], "three posture presses cycle back to Auto");
        assert_eq!(routing.finish(), Ok(()), "F finishes drawn copper");
        assert_eq!(routing.finish(), Err("the router is finishing this track"), "a second F says the Finish is under way");
        let finishing = routing.sample();
        assert!(finishing.gesture.finish, "the next sample carries the Finish");
        assert!(routing.release_held() && routing.ending == Some(finishing.seq), "a finishing release sends no motion: it commits the Finish sample");
        let mut stretch = answer(&routing, finishing.seq, Tier::Stretch);
        Arc::make_mut(&mut stretch).last = false;
        assert!(routing.offer(&stretch), "the Finish sample's stretch answer is taken");
        assert!(!routing.commit_ready(), "a finishing release waits for its last answer, the Finish, not its stretch answer");
        assert!(routing.offer(&answer(&routing, finishing.seq, Tier::Full)) && routing.commit_ready(), "the Finish commits it");

        board.tracks[0].path = Path::new(P::ZERO).line(P::new(4_000_000, 0)).line(P::new(4_000_000, 4_000_000));
        let track = &board.tracks[0];
        let drag = Press::Seg { track: track.id, seg: 1, net: track.net, layer: track.layer, at: P::new(4_100_000, 2_000_000) };
        let mut dragging = Routing::start(&drag, &board, board.unit).expect("a segment press starts routing");
        assert_eq!(dragging.finish(), Err("only drawn copper is finished"), "a segment drag has nothing to finish");
        assert_eq!(dragging.g.kind, GestureKind::DragSeg { track: track.id, seg: 1 }, "the dragged segment is preserved");
        assert_eq!(dragging.g.start, Anchor::Track { id: track.id, at: OnTrack { seg: 1, frac: OnTrack::ONE / 2 } }, "a segment drag anchors to the nearest position on that segment");
        assert_eq!((dragging.g.start_at, dragging.g.net), (P::new(4_000_000, 2_000_000), Some(track.net)), "a segment drag starts on the centreline with its net");
        assert_ne!(dragging.id, routing.id, "separate routing states have distinct gesture ids");
        let moved = dragging.moved(P::new(4_600_000, 2_300_000), 10, Some((target, snapped, "via".into())));
        assert_eq!((moved.gesture.cursor, moved.gesture.target, dragging.target.as_deref()), (P::new(4_500_000, 2_300_000), None, None),
            "a drag moves the grabbed point by the pointer's offset and never snaps");
        let back = dragging.moved(P::new(4_100_000, 2_000_000), 10, None);
        assert_eq!(back.gesture.cursor, back.gesture.start_at, "the pointer back at the press puts the segment back: it springs back");
        assert!(Routing::start(&Press::Hold(pad_hit.tag.item), &board, board.unit).is_none(), "a hold does not start routing");
        let part = board.placed[0].id;
        let mut holding = Routing::hold(&board, board.unit, part, board.placed[0].at).expect("a part hold is a worker gesture");
        assert!(!holding.release_held(), "a hold that never moved has nothing to commit");
        let by = P::new(500_000, 0);
        let sample = holding.held(part, by);
        assert_eq!((&sample.gesture.kind, &sample.gesture.moved[..], sample.seq), (&GestureKind::DragParts, &[(part, Xform::translate(by))][..], 1),
            "a held part's sample carries its motion");
        assert!(holding.release_held() && holding.ending == Some(1), "the release commits the answer to the last sample");
        let via = board.vias[0].id;
        assert_eq!(Routing::hold(&board, board.unit, via, board.vias[0].at).map(|r| r.g.kind), Some(GestureKind::DragVia(via)), "a held via is a via drag");
        assert!(Routing::hold(&board, board.unit, board.tracks[0].id, P::ZERO).is_none(), "a track is not held");
        assert!(Routing::start(&Press::Nothing, &board, board.unit).is_none(), "canvas behaviour does not start routing");
    }

    #[test]
    fn a_preview_that_cuts_a_pour_says_so() {
        let board = fixture();
        let gnd = board.net("GND").expect("ledctl has a ground net");
        let own = (0..board.nets.len() as u32).map(NetId).find(|id| *id != gnd).expect("ledctl has another net");
        let parts: Vec<ItemId> = board.placed.iter().take(3).map(|p| p.id).collect();
        let pad = |i: usize| ConnItem::Pad { placed: parts[i], pad: PadIx(0) };
        let zone = |x: i32| ConnItem::Zone { item: ItemId(900), layer: LayerId::F_CU, tile: TileKey { x, y: 0 }, ord: 0 };
        let cluster = |net: NetId, items: Vec<ConnItem>| Cluster { nets: vec![net], items };
        let conn = |clusters: Vec<Cluster>| Connectivity { clusters, ..Connectivity::default() };
        let whole = conn(vec![cluster(gnd, vec![pad(0), pad(1), pad(2), zone(0)]), cluster(own, vec![pad(0)])]);
        assert_eq!(pour_cut(&board, &whole, &whole, Some(own)), None, "an unchanged pour says nothing");
        let off = conn(vec![cluster(gnd, vec![pad(0), zone(0)]), cluster(gnd, vec![pad(1), pad(2)]), cluster(own, vec![pad(0)])]);
        assert_eq!(pour_cut(&board, &whole, &off, Some(own)), Some(("cuts GND: 2 pads off the pour".into(), vec![parts[1], parts[2]])),
            "two pads left off the pour are counted and their parts marked");
        let parted = conn(vec![cluster(gnd, vec![pad(0), zone(0)]), cluster(gnd, vec![pad(1), pad(2), zone(1)]), cluster(own, vec![pad(0)])]);
        assert_eq!(pour_cut(&board, &whole, &parted, Some(own)), Some(("cuts GND: 1 open line".into(), Vec::new())),
            "a pour that parts with pads on both sides is an open line");
        let mine = conn(vec![cluster(gnd, vec![pad(0), pad(1), pad(2), zone(0)]), cluster(own, vec![pad(0)]), cluster(own, vec![pad(1)])]);
        assert_eq!(pour_cut(&board, &whole, &mine, Some(own)), None, "the gesture's own net is the ratsnest's, not the chip's");
    }
}
