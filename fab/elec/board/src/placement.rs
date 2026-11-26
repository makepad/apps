//! Placement geometry shared by board DRC and the placer. Spatial queries select candidates;
//! only positive polygon area counts, so abutting courtyards and keepouts are legal.

use crate::drc::{board_keepouts, mm3, DrcKeepout, ScopeBox, KEEPOUT_MESSAGE};
use crate::{Board, BoardKeepout, CarriedKeepout, Hole, Origin, Placed, PortRoom, ARC_ERR};
use fab_elec_core::{Angle, BBox, Index, ItemId, LayerId, LayerSet, Nm, PolySet, Shape, Side, P};
use fab_elec_parts::Forbid;
use std::collections::{BTreeSet, HashMap};
use std::ops::ControlFlow;
use std::sync::{Mutex, OnceLock};

/// An empty courtyard stays empty. DRC has no courtyard rule for such a footprint.
pub fn placed_courtyard(p: &Placed) -> PolySet {
    polygon_at(&p.footprint.courtyard, p.at, p.rot, p.side)
}

/// The part's body at a pose: its courtyard. With `boxes`, a footprint without one takes its
/// box: DRC has no rule for such a part, but the placer still keeps them apart.
pub fn body_at(p: &Placed, at: P, rot: Angle, side: Side, boxes: bool) -> PolySet {
    if p.footprint.courtyard.is_empty() {
        let b = p.footprint.bbox();
        if !boxes || b.is_empty() { return PolySet::new() }
        return polygon_at(&PolySet::rect(b), at, rot, side);
    }
    polygon_at(&p.footprint.courtyard, at, rot, side)
}

/// The copper of each pad on the outer layers at a pose, in the board frame.
fn pads_at(p: &Placed, at: P, rot: Angle, side: Side) -> impl Iterator<Item = Shape> + '_ {
    p.footprint.pads.iter().flat_map(|pad| {
        [LayerId::F_CU, LayerId::B_CU].into_iter().filter_map(|l| Some(pad.stack.shape_on(l)?.shape(pad.at, pad.rot)))
    }).map(move |s| {
        let s = if side == Side::Bottom { s.mirrored_x(0) } else { s };
        s.rotated_about(P::ZERO, rot).translated(at)
    })
}

/// Where a part's drilled pads come out on the far side of the board at a pose: each pad's
/// copper on the far layer and its hole. A through-hole pin and its joint stand there, under
/// whatever sits on that side. Empty for a part without a drilled pad.
pub fn through_at(p: &Placed, at: P, rot: Angle, side: Side) -> PolySet {
    through_shape(p, rot, side).translated(at)
}

/// `through_at` about the part's origin: the placer moves one shape instead of joining the pads again.
fn through_shape(p: &Placed, rot: Angle, side: Side) -> PolySet {
    let shapes: Vec<PolySet> = p.footprint.pads.iter().filter_map(|pad| Some((pad, pad.drill.as_ref()?))).flat_map(|(pad, d)| {
        let c = pad.at.add(d.offset.rotated(pad.rot));
        let hole = if d.size.x == d.size.y {
            Shape::Circle { c, r: d.size.x / 2 }
        } else {
            Shape::Oval { c, half: P::new(d.size.x / 2, d.size.y / 2), rot: pad.rot }
        };
        // Footprints are drawn from the top: their far layer is the back copper.
        pad.stack.shape_on(LayerId::B_CU).map(|s| s.shape(pad.at, pad.rot)).into_iter().chain([hole])
    }).map(|s| {
        let s = if side == Side::Bottom { s.mirrored_x(0) } else { s };
        s.rotated_about(P::ZERO, rot).to_polyset(ARC_ERR)
    }).collect();
    PolySet::union_all(&shapes)
}

/// A box inside each drilled pad's hole, about the part's origin: a box-shaped courtyard on the
/// far side over one is under the part's through-hole pads for sure (`Conflict::Through`).
pub fn hole_cores(p: &Placed, rot: Angle, side: Side) -> Vec<BBox> {
    p.footprint.pads.iter().filter_map(|pad| {
        let d = pad.drill.as_ref()?;
        let c = pad.at.add(d.offset.rotated(pad.rot));
        let c = if side == Side::Bottom { c.mirrored_x(0) } else { c }.rotated(rot);
        // Half the hole's smaller radius: inside it at any rotation, with room for arc error.
        let half = d.size.x.min(d.size.y) / 4;
        (half > 0).then(|| BBox::new(c.sub(P::new(half, half)), c.add(P::new(half, half))))
    }).collect()
}

/// The box of the part's pads about its origin, at a rotation and side.
pub fn pads_box(p: &Placed, rot: Angle, side: Side) -> BBox {
    pads_at(p, P::ZERO, rot, side).fold(BBox::EMPTY, |b, s| b.union(s.bbox()))
}

/// A footprint-frame polygon at a board pose, mirrored before turning on the bottom.
pub fn polygon_at(poly: &PolySet, at: P, rot: Angle, side: Side) -> PolySet {
    let mirrored;
    let poly = if side == Side::Bottom {
        mirrored = poly.mirrored_x(0);
        &mirrored
    } else {
        poly
    };
    poly.rotated_about(P::ZERO, rot).translated(at)
}

/// Height bands must share positive length. An unspecified band spans every height.
pub fn bands_overlap(a: Option<(Nm, Nm)>, b: Option<(Nm, Nm)>) -> bool {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => a0.max(b0) < a1.min(b1),
        _ => true,
    }
}

/// A finding relative to the queried part. Each id names the other item: for a moving
/// footprint's own keepout, `owner` is the owner of the courtyard that keepout reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conflict {
    Courtyard { other: ItemId, at: P },
    /// Drilled pads under a body on the far side: the part's own under `other`'s, or `other`'s
    /// under the part's.
    Through { other: ItemId, at: P },
    /// The body over a board hole.
    Hole { hole: ItemId, at: P },
    Keepout { owner: ItemId, at: P, message: String },
}

struct Keepout<'a> {
    owner: ItemId,
    area: PolySet,
    layers: LayerSet,
    side: Option<Side>,
    z: Option<(Nm, Nm)>,
    /// What keeps the space clear (`BoardKeepout::reason`), for the finding; empty for a
    /// footprint's own keepout.
    reason: &'a str,
}

/// Visit the footprint's keepouts and the additional ones owned by this moving part.
fn keepouts_at<'a>(part: &'a Placed, carried: &[(&'a CarriedKeepout, &'a str)], at: P, rot: Angle, side: Side, mut visit: impl FnMut(Keepout<'a>)) {
    for k in part.footprint.keepouts.iter().filter(|k| k.forbid.has(Forbid::FOOTPRINTS)) {
        visit(Keepout { owner: part.id, area: polygon_at(&k.area, at, rot, side), layers: k.layers, side: Some(side), z: None, reason: "" });
    }
    for &(k, reason) in carried {
        visit(Keepout {
            owner: part.id, area: k.area.translated(at), layers: LayerSet::EMPTY,
            side: Some(k.side), z: Some(k.band), reason,
        });
    }
}

fn overlap(a: &PolySet, b: &PolySet) -> Option<P> {
    let common = a.intersection(b);
    (!common.is_empty() && common.area() > 0.0).then(|| common.bbox().center())
}

fn courtyard_hit(a: &PolySet, side: Side, b: &PolySet, other_side: Side) -> Option<P> {
    if side != other_side { return None }
    overlap(a, b)
}

// Keep both roles until after sorting: a footprint can both own and violate a keepout.
type KeepoutHit = (ItemId, ItemId, P, String);

impl Keepout<'_> {
    fn hit(&self, p: &Placed, side: Side, courtyard: &PolySet) -> Option<KeepoutHit> {
        let same_side = self.side.map_or_else(
            || self.layers.has(if side == Side::Top { LayerId::F_CU } else { LayerId::B_CU }),
            |s| s == side,
        );
        if p.id == self.owner || !same_side || !bands_overlap(self.z, p.body_z) { return None }
        let at = overlap(courtyard, &self.area)?;
        let message = match (p.body_z, self.z) {
            (Some((b0, b1)), Some((k0, k1))) => format!(
                "body {}-{} mm high reaches into {} {}-{} mm band",
                mm3(b0), mm3(b1), if self.reason.is_empty() { "the keepout's".to_owned() } else { format!("{}'s", self.reason) }, mm3(k0), mm3(k1)
            ),
            _ => KEEPOUT_MESSAGE.into(),
        };
        Some((p.id, self.owner, at, message))
    }
}

fn dedup_keepouts(hits: &mut Vec<KeepoutHit>) {
    hits.sort();
    hits.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
}

struct Courtyard<'a> {
    part: &'a Placed,
    side: Side,
    area: PolySet,
    bbox: BBox,
    /// The area is exactly its box.
    exact: bool,
}

/// The area a board hole takes on both sides: its copper ring, or the hole itself.
pub fn hole_area(h: &Hole) -> PolySet {
    Shape::Circle { c: h.at, r: h.pad.unwrap_or(0).max(h.drill) / 2 }.to_polyset(ARC_ERR)
}

struct BoardHole {
    id: ItemId,
    area: PolySet,
    bbox: BBox,
}

/// A part's drilled pads where they come out (`through_at`), on its `far` side.
struct Through<'a> {
    part: &'a Placed,
    far: Side,
    area: PolySet,
    bbox: BBox,
}

impl<'a> Through<'a> {
    fn at(part: &'a Placed, at: P, rot: Angle, side: Side) -> Option<Self> {
        Self::of(part, through_at(part, at, rot, side), side)
    }

    fn of(part: &'a Placed, area: PolySet, side: Side) -> Option<Self> {
        (!area.is_empty()).then(|| Through { part, far: side.flipped(), bbox: area.bbox(), area })
    }
}

/// A snapshot of the board's placement geometry. The scope selects courtyards as in
/// incremental DRC. A keepout can reach an in-scope courtyard from outside the scope.
pub struct PlacementIndex<'a> {
    board: &'a Board,
    scope: Option<BBox>,
    courts: Vec<Courtyard<'a>>,
    court_index: Index<usize>,
    keepouts: Vec<Keepout<'a>>,
    carried: HashMap<ItemId, Vec<(&'a CarriedKeepout, &'a str)>>,
    // Whole-board DRC queries the courtyard tree for each keepout. Only pose queries need
    // the reverse index, so DRC keeps its original build cost even on a keepout-heavy board.
    keepout_index: OnceLock<Index<usize>>,
    throughs: Vec<Through<'a>>,
    // Only pose queries look a body up among the drilled pads.
    through_index: OnceLock<Index<usize>>,
    holes: Vec<BoardHole>,
    hole_index: Index<usize>,
    /// Pose queries: each part's drilled pads about its origin, per rotation and side, joined once.
    through_shapes: Mutex<HashMap<(ItemId, Angle, Side), PolySet>>,
    /// Placer mode: a part without a courtyard takes its footprint's box.
    boxes: bool,
}

/// Match movers to their published geometry by derived source, independent of board order.
struct CarriedSources<'a> {
    replaced: BTreeSet<ItemId>,
    moving: Vec<(&'a Placed, &'a CarriedKeepout, Option<&'a BoardKeepout>)>,
}

impl<'a> CarriedSources<'a> {
    fn new(board: &'a Board, moving: &BTreeSet<ItemId>, ports: &'a [PortRoom]) -> Self {
        let movers: HashMap<_, _> = moving.iter().filter_map(|&id| board.placed_item(id).map(|p| (p.comp, p))).collect();
        let mut sets: HashMap<_, Vec<_>> = HashMap::new();
        for k in &board.keepouts {
            if let Origin::Derived { from } = &k.origin { sets.entry(from).or_default().push(k); }
        }
        let mut replaced = BTreeSet::new();
        let mut carried = Vec::new();
        for port in ports {
            let (Some(k), Some(&p)) = (&port.keepout, movers.get(&port.comp)) else { continue };
            let old = sets.get(&k.from).into_iter().flatten().copied();
            replaced.extend(old.clone().map(|k| k.id));
            carried.push((p, k, old.min_by_key(|k| k.id)));
        }
        Self { replaced, moving: carried }
    }
}

/// On a posed board snapshot, the carried keepouts to replace or append for a placement
/// check, each paired with its exempt owner, and any superseded ids to remove. Reuse a
/// published id when available. Every old copy of a moving source is accounted for.
pub fn projected_carried_keepouts(board: &Board, moving: &BTreeSet<ItemId>, ports: &[PortRoom]) -> (Vec<(ItemId, BoardKeepout)>, BTreeSet<ItemId>) {
    let CarriedSources { mut replaced, moving } = CarriedSources::new(board, moving, ports);
    let mut next = board.next_item;
    let projected = moving.into_iter().map(|(p, k, published)| {
        let id = published.map(|old| old.id).filter(|id| replaced.remove(id)).unwrap_or_else(|| {
            let id = ItemId(next);
            next += 1;
            id
        });
        (p.id, BoardKeepout {
            id, area: k.area.translated(p.at), z: Some(k.band),
            layers: LayerSet::one(LayerId::F_CU.on(k.side)), forbid: Forbid::FOOTPRINTS,
            reason: published.map_or_else(String::new, |old| old.reason.clone()), origin: Origin::Derived { from: k.from.clone() },
        })
    }).collect();
    (projected, replaced)
}

impl<'a> PlacementIndex<'a> {
    pub fn new(board: &'a Board, scope: Option<BBox>) -> Self {
        Self::with_keepouts(board, scope, board_keepouts(board))
    }

    // DRC already transformed the keepouts for its copper checks. Take those polygons as-is.
    pub(crate) fn with_keepouts(board: &'a Board, scope: Option<BBox>, source: Vec<DrcKeepout>) -> Self {
        Self::build(board, scope, source, &BTreeSet::new(), false)
    }

    /// The placer's snapshot: the `moving` parts and the keepouts they own are out until
    /// `place` puts each back at its pose. A part without a courtyard takes its footprint's box.
    pub fn without(board: &'a Board, moving: &BTreeSet<ItemId>) -> Self {
        Self::build(board, None, board_keepouts(board), moving, true)
    }

    /// The placer's snapshot with keepouts carried at their owners' positions. A moving owner's published
    /// source drops out until `place`. Stationary sources remain in board space.
    pub fn with_carried_keepouts(board: &'a Board, moving: &BTreeSet<ItemId>, ports: &'a [PortRoom]) -> Self {
        if ports.is_empty() || moving.is_empty() { return Self::without(board, moving) }
        let members = CarriedSources::new(board, moving, ports);
        let mut carried = HashMap::<_, Vec<_>>::new();
        for (p, k, published) in members.moving {
            carried.entry(p.id).or_default().push((k, published.map_or("", |old| old.reason.as_str())));
        }
        let mut source = board_keepouts(board);
        source.retain(|(owner, ..)| !members.replaced.contains(owner));
        let mut index = Self::build(board, None, source, moving, true);
        index.carried = carried;
        index
    }

    fn build(board: &'a Board, scope: Option<BBox>, source: Vec<DrcKeepout>, out: &BTreeSet<ItemId>, boxes: bool) -> Self {
        let courts: Vec<_> = board.placed.iter().filter_map(|part| {
            if out.contains(&part.id) || !scope.is_none_or_box(&part.bbox()) { return None }
            let area = body_at(part, part.at, part.rot, part.side, boxes);
            let bbox = area.bbox();
            let exact = exact_box(&area).is_some();
            (!area.is_empty() && scope.is_none_or_box(&bbox)).then_some(Courtyard { part, side: part.side, area, bbox, exact })
        }).collect();
        let court_index = Index::build(courts.iter().enumerate().map(|(i, c)| (c.bbox, i)).collect());
        // O(board keepouts) once: a finding names the board keepout a body reaches.
        let reasons: HashMap<ItemId, &'a str> = board.keepouts.iter().map(|k| (k.id, k.reason.as_str())).collect();
        let keepouts: Vec<_> = source.into_iter().filter_map(|(owner, area, layers, forbid, side, z)| {
            if !forbid.has(Forbid::FOOTPRINTS) || out.contains(&owner) { return None }
            let Shape::Poly(area) = area else { return None };
            Some(Keepout { owner, area, layers, side, z, reason: reasons.get(&owner).copied().unwrap_or("") })
        }).collect();
        let throughs = board.placed.iter().filter(|part| !out.contains(&part.id) && scope.is_none_or_box(&part.bbox()))
            .filter_map(|part| Through::at(part, part.at, part.rot, part.side)).collect();
        let holes: Vec<_> = board.holes.iter().map(|h| {
            let area = hole_area(h);
            BoardHole { id: h.id, bbox: area.bbox(), area }
        }).filter(|h| scope.is_none_or_box(&h.bbox)).collect();
        let hole_index = Index::build(holes.iter().enumerate().map(|(i, h)| (h.bbox, i)).collect());
        Self {
            board, scope, courts, court_index, keepouts, carried: HashMap::new(), keepout_index: OnceLock::new(), throughs,
            through_index: OnceLock::new(), holes, hole_index, through_shapes: Mutex::default(), boxes,
        }
    }

    fn through_index(&self) -> &Index<usize> {
        self.through_index.get_or_init(|| Index::build(self.throughs.iter().enumerate().map(|(i, t)| (t.bbox, i)).collect()))
    }

    fn keepout_index(&self) -> &Index<usize> {
        self.keepout_index.get_or_init(|| {
            Index::build(self.keepouts.iter().enumerate().map(|(i, k)| (k.area.bbox(), i)).collect())
        })
    }

    /// Put a part at a pose: queries then see its body there and the keepouts it owns.
    pub fn place(&mut self, id: ItemId, at: P, rot: Angle, side: Side) {
        let board: &'a Board = self.board;
        let Some(part) = board.placed_item(id) else { return };
        let area = body_at(part, at, rot, side, self.boxes);
        if !area.is_empty() {
            let bbox = area.bbox();
            let exact = exact_box(&area).is_some();
            self.court_index.insert(bbox, self.courts.len());
            self.courts.push(Courtyard { part, side, area, bbox, exact });
        }
        let carried = self.carried.get(&id).map(Vec::as_slice).unwrap_or_default();
        keepouts_at(part, carried, at, rot, side, |keepout| {
            if let Some(index) = self.keepout_index.get_mut() {
                index.insert(keepout.area.bbox(), self.keepouts.len());
            }
            self.keepouts.push(keepout);
        });
        if let Some(t) = Through::at(part, at, rot, side) {
            if let Some(index) = self.through_index.get_mut() {
                index.insert(t.bbox, self.throughs.len());
            }
            self.throughs.push(t);
        }
    }

    /// The boxes a part on `side` keeps clear of near `area`: the bodies on that side, the
    /// keepouts, the board's holes and the drilled pads coming out on that side. They give a
    /// search its candidate spots; `pose_conflicts` decides each one. The flag marks a box that
    /// is exactly a body on that side: a box-shaped body overlapping it always conflicts, so a
    /// search can drop such a spot without the exact test.
    pub fn obstacle_boxes(&self, area: BBox, side: Side, mut f: impl FnMut(BBox, bool)) {
        self.court_index.query(area, |b, i| {
            if self.courts[i].side == side { f(b, self.courts[i].exact) }
            ControlFlow::Continue(())
        });
        self.hole_index.query(area, |b, _| {
            f(b, false);
            ControlFlow::Continue(())
        });
        self.through_index().query(area, |b, i| {
            if self.throughs[i].far == side { f(b, false) }
            ControlFlow::Continue(())
        });
        self.keepout_index().query(area, |b, _| {
            f(b, false);
            ControlFlow::Continue(())
        });
    }

    /// The courtyards on `side` near `area`, with their parts.
    pub fn courts_near(&self, area: BBox, side: Side, mut f: impl FnMut(BBox, &Placed)) {
        self.court_index.query(area, |b, i| {
            if self.courts[i].side == side { f(b, self.courts[i].part) }
            ControlFlow::Continue(())
        });
    }

    /// The courtyards on `side` near `area` whose shape is their box.
    pub fn exact_courts(&self, area: BBox, side: Side, mut f: impl FnMut(BBox)) {
        self.court_index.query(area, |b, i| {
            if self.courts[i].side == side && self.courts[i].exact { f(b) }
            ControlFlow::Continue(())
        });
    }

    /// Everything the snapshot holds: bodies, keepouts and holes.
    pub fn bounds(&self) -> BBox {
        self.court_index.bbox().union(self.keepout_index().bbox()).union(self.hole_index.bbox())
    }

    fn bodies_in(&self, keepout: &Keepout, hits: &mut Vec<KeepoutHit>) {
        self.court_index.query(keepout.area.bbox(), |_, i| {
            let c = &self.courts[i];
            hits.extend(keepout.hit(c.part, c.side, &c.area));
            ControlFlow::Continue(())
        });
    }

    /// One witness per (body, keepout owner), in DRC order, before region filtering.
    pub fn keepout_pairs(&self) -> Vec<(ItemId, ItemId, P, String)> {
        let mut hits = Vec::new();
        for k in &self.keepouts {
            self.bodies_in(k, &mut hits);
        }
        dedup_keepouts(&mut hits);
        hits
    }

    /// Each pair once, smaller item id first, in DRC order.
    pub fn courtyard_pairs(&self) -> Vec<(&'a Placed, &'a Placed, P)> {
        let mut pairs = Vec::new();
        for (i, a) in self.courts.iter().enumerate() {
            self.court_index.query(a.bbox, |_, j| {
                let b = &self.courts[j];
                if j > i {
                    if let Some(at) = courtyard_hit(&a.area, a.side, &b.area, b.side) {
                        let (a, b) = if a.part.id <= b.part.id { (a, b) } else { (b, a) };
                        pairs.push((a.part, b.part, at));
                    }
                }
                ControlFlow::Continue(())
            });
        }
        pairs.sort_by_key(|(a, b, at)| (a.id, b.id, *at));
        pairs.dedup_by_key(|(a, b, _)| (a.id, b.id));
        pairs
    }

    /// Parts whose drilled pads come out under another part's courtyard on the far side, as
    /// (the drilled part, the other, a point in both), in DRC order. Each pair once: two
    /// through-hole parts on opposite sides can each be under the other, and DRC names a
    /// finding by its parts.
    pub fn through_pairs(&self) -> Vec<(&'a Placed, &'a Placed, P)> {
        let mut pairs = Vec::new();
        for t in &self.throughs {
            self.court_index.query(t.bbox, |_, j| {
                let c = &self.courts[j];
                if c.side == t.far && c.part.id != t.part.id {
                    if let Some(at) = overlap(&t.area, &c.area) {
                        pairs.push((t.part, c.part, at));
                    }
                }
                ControlFlow::Continue(())
            });
        }
        let key = |a: &Placed, b: &Placed| (a.id.min(b.id), a.id.max(b.id));
        pairs.sort_by_key(|(a, b, at)| (key(a, b), *at));
        pairs.dedup_by_key(|(a, b, _)| key(a, b));
        pairs
    }

    /// Parts whose body covers a board hole on either side, as (the part, the hole, a point in
    /// both), in DRC order.
    pub fn hole_pairs(&self) -> Vec<(&'a Placed, ItemId, P)> {
        let mut pairs = Vec::new();
        for h in &self.holes {
            self.court_index.query(h.bbox, |_, j| {
                let c = &self.courts[j];
                if let Some(at) = overlap(&c.area, &h.area) {
                    pairs.push((c.part, h.id, at));
                }
                ControlFlow::Continue(())
            });
        }
        pairs.sort_by_key(|(p, hole, at)| (p.id, *hole, *at));
        pairs.dedup_by_key(|(p, hole, _)| (p.id, *hole));
        pairs
    }

    /// Parts whose courtyard leaves the board outline, each with a point outside it. A part
    /// fixed to an edge (`Board::edge_parts`) may overhang; a board without an outline has none.
    pub fn outside_outline(&self) -> Vec<(&'a Placed, P)> {
        if self.board.outline.is_empty() { return Vec::new() }
        let outline = self.board.outline_polyset(ARC_ERR);
        let outline_box = exact_box(&outline);
        let mut out: Vec<_> = self.courts.iter().filter(|c| {
            self.board.edge_parts.binary_search(&c.part.comp).is_err() && !inside(&c.area, &outline, outline_box)
        }).map(|c| (c.part, c.area.difference(&outline).bbox().center())).collect();
        out.sort_by_key(|(p, _)| p.id);
        out.dedup_by_key(|(p, _)| p.id);
        out
    }

    /// Test one replacement pose against this snapshot. Both its courtyard and its own
    /// keepouts move. The part's old geometry can never conflict with the replacement.
    pub fn pose_conflicts(&self, id: ItemId, at: P, rot: Angle, side: Side) -> impl Iterator<Item = Conflict> {
        let mut hits = Vec::new();
        let mut courts = Vec::new();
        let mut throughs = Vec::new();
        let mut holes = Vec::new();
        if let Some(part) = self.board.placed_item(id) {
            let area = body_at(part, at, rot, side, self.boxes);
            let bbox = area.bbox();
            if !area.is_empty() && self.scope.is_none_or_box(&bbox) {
                self.keepout_index().query(bbox, |_, i| {
                    hits.extend(self.keepouts[i].hit(part, side, &area));
                    ControlFlow::Continue(())
                });
                self.court_index.query(bbox, |_, i| {
                    let c = &self.courts[i];
                    if c.part.id != id {
                        if let Some(at) = courtyard_hit(&area, side, &c.area, c.side) {
                            courts.push((c.part.id, at));
                        }
                    }
                    ControlFlow::Continue(())
                });
                self.hole_index.query(bbox, |_, i| {
                    if let Some(at) = overlap(&area, &self.holes[i].area) {
                        holes.push((self.holes[i].id, at));
                    }
                    ControlFlow::Continue(())
                });
                self.through_index().query(bbox, |_, i| {
                    let t = &self.throughs[i];
                    if t.far == side && t.part.id != id {
                        if let Some(at) = overlap(&area, &t.area) {
                            throughs.push((t.part.id, at));
                        }
                    }
                    ControlFlow::Continue(())
                });
            }
            let shape = self.through_shapes.lock().unwrap_or_else(|e| e.into_inner())
                .entry((id, rot, side)).or_insert_with(|| through_shape(part, rot, side)).translated(at);
            if let Some(t) = Through::of(part, shape, side) {
                self.court_index.query(t.bbox, |_, i| {
                    let c = &self.courts[i];
                    if c.side == t.far && c.part.id != id {
                        if let Some(at) = overlap(&t.area, &c.area) {
                            throughs.push((c.part.id, at));
                        }
                    }
                    ControlFlow::Continue(())
                });
            }
            let carried = self.carried.get(&id).map(Vec::as_slice).unwrap_or_default();
            keepouts_at(part, carried, at, rot, side, |moved| self.bodies_in(&moved, &mut hits));
        }
        dedup_keepouts(&mut hits);
        courts.sort_unstable();
        holes.sort_unstable();
        // Once per other part, either way round, as `through_pairs` counts them.
        throughs.sort_unstable();
        throughs.dedup_by_key(|t| t.0);
        hits.into_iter().map(move |(body, owner, at, message)| Conflict::Keepout {
            owner: if owner == id { body } else { owner }, at, message,
        }).chain(courts.into_iter().map(|(other, at)| Conflict::Courtyard { other, at }))
            .chain(throughs.into_iter().map(|(other, at)| Conflict::Through { other, at }))
            .chain(holes.into_iter().map(|(hole, at)| Conflict::Hole { hole, at }))
    }
}

/// Where a part may sit on the board: its body inside the outline (a part fixed to an edge may
/// overhang it) and its pads' copper inside the outline less the edge rule. DRC reports these as
/// `PlacementIndex::outside_outline` and edge clearance.
pub struct Region {
    outline: PolySet,
    /// The outline when it is exactly a box: containment is then a box test.
    outline_box: Option<BBox>,
    inset: PolySet,
    inset_box: Option<BBox>,
}

/// The set's box when the set is exactly that box.
pub fn exact_box(set: &PolySet) -> Option<BBox> {
    let b = set.bbox();
    let area = b.width() as f64 * b.height() as f64;
    (!b.is_empty() && (set.area() - area).abs() <= area * 1e-12).then_some(b)
}

fn inside(a: &PolySet, region: &PolySet, b: Option<BBox>) -> bool {
    match b {
        Some(b) => b.contains_box(&a.bbox()),
        None => a.difference(region).area() <= 0.0,
    }
}

impl Region {
    /// None for a board with no outline: a part may go anywhere.
    pub fn new(board: &Board, edge: Nm) -> Option<Region> {
        if board.outline.is_empty() { return None }
        let outline = board.outline_polyset(ARC_ERR);
        let inset = outline.offset(-edge, ARC_ERR);
        Some(Region { outline_box: exact_box(&outline), inset_box: exact_box(&inset), outline, inset })
    }

    pub fn outline_box(&self) -> BBox {
        self.outline.bbox()
    }

    /// The box the pads keep inside; empty when the edge rule leaves no room.
    pub fn inset_box(&self) -> BBox {
        self.inset.bbox()
    }

    /// Whether `part` at this pose sits in the region.
    pub fn holds(&self, part: &Placed, at: P, rot: Angle, side: Side, edge_part: bool) -> bool {
        let body = body_at(part, at, rot, side, true);
        if !body.is_empty() && !edge_part && !inside(&body, &self.outline, self.outline_box) { return false }
        let pads: Vec<Shape> = pads_at(part, at, rot, side).collect();
        let all = pads.iter().fold(BBox::EMPTY, |b, s| b.union(s.bbox()));
        if all.is_empty() || self.inset_box.is_some_and(|b| b.contains_box(&all)) { return true }
        if self.inset.is_empty() { return false }
        pads.iter().all(|s| {
            self.inset_box.is_some_and(|b| b.contains_box(&s.bbox()))
                || s.to_polyset(ARC_ERR).difference(&self.inset).area() <= 0.0
        })
    }
}
