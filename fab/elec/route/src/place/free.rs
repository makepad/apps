//! The nearest legal spot for one part, against what stays put and what is placed so far.
//! The board's placement checks apply to bodies. Port rooms meet only other parts' rooms.

use super::Pose;
use fab_elec_board::placement::{body_at, exact_box, hole_cores, pads_box, Conflict, PlacementIndex, Region};
use fab_elec_board::{resolve_rules, Board, Placed, PortRoom};
use fab_elec_circuit::Edge;
use fab_elec_core::{BBox, ItemId, Nm, PolySet, P};
use std::collections::{BTreeMap, BTreeSet};

/// The generator allows an intersection up to 0.001 mm wide on either axis.
/// Test each connected intersection so gaps and holes between rooms stay free.
pub fn rooms_overlap(a: &PolySet, b: &PolySet) -> bool {
    const TOL: i64 = 1_000;
    let wide = |bounds: BBox| bounds.width() > TOL && bounds.height() > TOL;
    let (aa, bb) = (a.bbox(), b.bbox());
    let common = BBox {
        min: P::new(aa.min.x.max(bb.min.x), aa.min.y.max(bb.min.y)),
        max: P::new(aa.max.x.min(bb.max.x), aa.max.y.min(bb.max.y)),
    };
    wide(common) && (exact_box(a).is_some() && exact_box(b).is_some()
        || a.intersection(b).polygons().iter().any(|p| wide(BBox::of_points(p.outer.iter().copied()))))
}

pub struct Free<'a> {
    board: &'a Board,
    index: PlacementIndex<'a>,
    region: Option<Region>,
    local_rooms: BTreeMap<ItemId, Vec<&'a PolySet>>,
    standing_rooms: Vec<(ItemId, PolySet)>,
    /// Parts fixed to an edge: their bodies may overhang the outline.
    edge: BTreeSet<ItemId>,
    /// Room each part's courtyard box keeps round it in a spaced search, the most any part keeps,
    /// and the body each part belongs to.
    halo: Option<(Box<dyn Fn(&Placed) -> Nm + 'a>, Nm, Box<dyn Fn(&Placed) -> ItemId + 'a>)>,
}

impl<'a> Free<'a> {
    /// Everything but `moving` is an obstacle; each moving part counts once it is `put`.
    pub fn new(board: &'a Board, moving: &BTreeSet<ItemId>, edge: BTreeSet<ItemId>) -> Self {
        Self::with_rooms(board, moving, edge, &[])
    }

    pub fn with_rooms(board: &'a Board, moving: &BTreeSet<ItemId>, edge: BTreeSet<ItemId>, ports: &'a [PortRoom]) -> Self {
        let mut local_rooms = BTreeMap::<_, Vec<_>>::new();
        let mut standing_rooms = Vec::new();
        for port in ports {
            let (Some(room), Some(part)) = (&port.room, board.placed_of(port.comp)) else { continue };
            if moving.contains(&part.id) {
                local_rooms.entry(part.id).or_default().push(room);
            } else {
                standing_rooms.push((part.id, room.translated(part.at)));
            }
        }
        let region = Region::new(board, resolve_rules(board, None).edge());
        Self { board, index: PlacementIndex::with_carried_keepouts(board, moving, ports), region, local_rooms, standing_rooms, edge, halo: None }
    }

    /// Spaced searches keep `halo(part)` round each part's courtyard box, the halos of two
    /// neighbours adding up, except between parts of one `body` (an IC and its decoupling
    /// capacitors).
    pub fn with_halo(mut self, halo: impl Fn(&Placed) -> Nm + 'a, body: impl Fn(&Placed) -> ItemId + 'a) -> Self {
        let most = self.board.placed.iter().map(&halo).max().unwrap_or(0);
        self.halo = Some((Box::new(halo), most, Box::new(body)));
        self
    }

    pub fn fits(&self, id: ItemId, pose: Pose) -> bool {
        let Some(part) = self.board.placed_item(id) else { return false };
        self.region.as_ref().is_none_or(|r| r.holds(part, pose.at, pose.rot, pose.side, self.edge.contains(&id)))
            && self.blocker(id, pose).is_none()
    }

    /// The other item preventing a fit inside the legal region.
    pub(super) fn blocker(&self, id: ItemId, pose: Pose) -> Option<ItemId> {
        self.index.pose_conflicts(id, pose.at, pose.rot, pose.side).next().map(|c| match c {
            Conflict::Courtyard { other, .. } | Conflict::Through { other, .. } => other,
            Conflict::Hole { hole, .. } => hole,
            Conflict::Keepout { owner, .. } => owner,
        }).or_else(|| {
            self.local_rooms.get(&id)?.iter().find_map(|r| {
                let area = r.translated(pose.at);
                self.standing_rooms.iter().find(|(other, b)| *other != id && rooms_overlap(&area, b)).map(|(other, _)| *other)
            })
        })
    }

    pub fn put(&mut self, id: ItemId, pose: Pose) {
        self.index.place(id, pose.at, pose.rot, pose.side);
        if let Some(rooms) = self.local_rooms.get(&id) {
            self.standing_rooms.extend(rooms.iter().map(|r| (id, r.translated(pose.at))));
        }
    }

    /// The legal spot nearest `pose.at` for the part at that rotation and side, by squared
    /// distance, ties by (y, x); None when the board has no room for it.
    ///
    /// The search grows a square window round the wanted point. The candidates in a window are
    /// the wanted point's own x and y crossed with every spot where the part's box abuts an
    /// obstacle's box or the region's edge: for box-shaped parts and obstacles the nearest legal
    /// spot is always one of those, so a part lands flush. A lattice at `grid` (coarser as the
    /// window grows) covers round and odd shapes. Each candidate is judged by `fits`; the first
    /// legal one in distance order wins. Only spots that keep the body's box in the outline's and
    /// the pads' box in the inset's are candidates, and those where a box-shaped body overlaps a
    /// box-shaped courtyard (or a drilled pad's core one on the far side) are skipped without an
    /// exact test. Cost: per window, O(k^2 log k) for k obstacles in it, plus one exact test per
    /// candidate tried.
    pub fn nearest(&self, id: ItemId, pose: Pose, grid: Nm) -> Option<P> {
        self.search(id, pose, grid, None, false, None)
    }

    /// As `nearest`, with every part's halo (`with_halo`) kept clear; where no spot has room for
    /// them, the nearest legal spot after all, so spacing never leaves a part without a place.
    pub fn nearest_spaced(&self, id: ItemId, pose: Pose, grid: Nm) -> Option<P> {
        if self.halo.is_none() { return self.nearest(id, pose, grid) }
        self.search(id, pose, grid, None, true, None).or_else(|| self.search(id, pose, grid, None, false, None))
    }

    /// As `nearest`, taking only spots `with` also accepts (within a `near` intent's max, where the
    /// parts that go with this one fit too); None when no legal spot is accepted.
    pub fn nearest_with(&self, id: ItemId, pose: Pose, grid: Nm, with: &dyn Fn(P) -> bool) -> Option<P> {
        self.search(id, pose, grid, None, false, Some(with))
    }

    /// As `nearest_spaced`, taking only spots `with` also accepts; None when no legal spot is.
    pub fn nearest_spaced_with(&self, id: ItemId, pose: Pose, grid: Nm, with: &dyn Fn(P) -> bool) -> Option<P> {
        self.halo.as_ref().and_then(|_| self.search(id, pose, grid, None, true, Some(with)))
            .or_else(|| self.nearest_with(id, pose, grid, with))
    }

    /// The legal spot nearest `pose.at` on `edge` of the outline's box: the part flush against
    /// it (its body's box meets that side, or its pads' box the inset's), sliding along it only.
    /// None when the board has no outline or the edge no room for the part.
    pub fn nearest_on_edge(&self, id: ItemId, pose: Pose, grid: Nm, edge: Edge) -> Option<P> {
        self.search(id, pose, grid, Some(edge), false, None)
    }

    fn search(&self, id: ItemId, pose: Pose, grid: Nm, line: Option<Edge>, spaced: bool, with: Option<&dyn Fn(P) -> bool>) -> Option<P> {
        let halo = self.halo.as_ref().filter(|_| spaced);
        let accept = |at: P| with.is_none_or(|w| w(at));
        if line.is_none() && halo.is_none() && accept(pose.at) && self.fits(id, pose) { return Some(pose.at) }
        let part = self.board.placed_item(id)?;
        let shape = body_at(part, P::ZERO, pose.rot, pose.side, true);
        let body_exact = exact_box(&shape).is_some();
        let body = shape.bbox();
        let body = if body.is_empty() { BBox::point(P::ZERO) } else { body };
        let room_boxes: Vec<_> = self.local_rooms.get(&id).into_iter().flatten()
            .map(|r| r.bbox()).filter(|b| !b.is_empty()).collect();
        let pads = pads_box(part, pose.rot, pose.side);
        let cores = hole_cores(part, pose.rot, pose.side);
        let reach_cores = cores.iter().fold(BBox::EMPTY, |b, c| b.union(*c));
        // The anchor points where the body's box stays in the outline's and the pads' box in the
        // inset's: only those can be legal.
        let mut allowed = BBox::new(P::new(Nm::MIN, Nm::MIN), P::new(Nm::MAX, Nm::MAX));
        if let Some(region) = &self.region {
            let (outline, inset) = (region.outline_box(), region.inset_box());
            let mut within = |edge: BBox, own: BBox| allowed = BBox::new(
                P::new(allowed.min.x.max(edge.min.x - own.min.x), allowed.min.y.max(edge.min.y - own.min.y)),
                P::new(allowed.max.x.min(edge.max.x - own.max.x), allowed.max.y.min(edge.max.y - own.max.y)));
            if !shape.is_empty() && !self.edge.contains(&id) && !outline.is_empty() { within(outline, body) }
            if !pads.is_empty() {
                if inset.is_empty() { return None }
                within(inset, pads);
            }
            if allowed.min.x > allowed.max.x || allowed.min.y > allowed.max.y { return None }
        }
        // On an edge, the anchor is where the part's far side meets it, and only the other axis
        // is searched.
        let want = match line {
            None => pose.at,
            Some(_) if self.region.is_none() => return None,
            Some(Edge::Left) => P::new(allowed.min.x, pose.at.y),
            Some(Edge::Right) => P::new(allowed.max.x, pose.at.y),
            Some(Edge::Bottom) => P::new(pose.at.x, allowed.min.y),
            Some(Edge::Top) => P::new(pose.at.x, allowed.max.y),
        };
        let (fixed_x, fixed_y) = (matches!(line, Some(Edge::Left | Edge::Right)), matches!(line, Some(Edge::Bottom | Edge::Top)));
        if line.is_some() && halo.is_none() && accept(want) && self.fits(id, Pose { at: want, ..pose }) { return Some(want) }
        // Past `reach` the window holds everything: no obstacle or edge is left to find.
        let world = self.region.as_ref().map_or(BBox::EMPTY, Region::outline_box).union(self.index.bounds()).union(BBox::point(want));
        let mut world = BBox::new(world.min.sub(body.max), world.max.sub(body.min));
        for own in &room_boxes {
            for (_, room) in &self.standing_rooms {
                let b = room.bbox();
                if !b.is_empty() { world = world.union(BBox::new(b.min.sub(own.max), b.max.sub(own.min))); }
            }
        }
        let reach = [world.min, world.max, P::new(world.min.x, world.max.y), P::new(world.max.x, world.min.y)].iter()
            .map(|c| (f64::from(c.x) - f64::from(want.x)).hypot(f64::from(c.y) - f64::from(want.y)).ceil() as i64).max().unwrap_or(0);
        let grid = i64::from(grid.max(1));
        let mut tried = -1i128;
        let mut r = (grid * 8).max(500_000);
        loop {
            let (mut xs, mut ys) = (vec![want.x], vec![want.y]);
            let mut abut = |o: BBox, own: BBox| {
                xs.extend([o.min.x - own.max.x, o.max.x - own.min.x]);
                ys.extend([o.min.y - own.max.y, o.max.y - own.min.y]);
            };
            for &own in &room_boxes {
                for (_, room) in &self.standing_rooms {
                    let b = room.bbox();
                    if !b.is_empty() { abut(b, own); }
                }
            }
            let r32 = r.min(i64::from(i32::MAX / 4)) as Nm;
            let window = BBox::new(P::new(want.x.saturating_sub(r32), want.y.saturating_sub(r32)), P::new(want.x.saturating_add(r32), want.y.saturating_add(r32)));
            let near = BBox::new(window.min.add(body.min), window.max.add(body.max));
            // Anchor points where the part overlaps a box-shaped obstacle for sure, as open boxes:
            // its body on a courtyard of its side, a drilled pad's core on one of the far side.
            let mut forbidden = Vec::new();
            let mut forbid = |own: BBox, o: BBox| forbidden.push(BBox::new(o.min.sub(own.max), o.max.sub(own.min)));
            self.index.obstacle_boxes(near, pose.side, |o, exact| {
                abut(o, body);
                if exact && body_exact { forbid(body, o) }
            });
            if !cores.is_empty() {
                let near = BBox::new(window.min.add(reach_cores.min), window.max.add(reach_cores.max));
                self.index.exact_courts(near, pose.side.flipped(), |o| for &c in &cores { forbid(c, o) });
            }
            if let Some((halo, most, of)) = halo {
                // Each courtyard grown by both halos: a spot inside is crowded. A halo is routing
                // room, and routing uses both layers and the vias between them: a part on the other
                // side takes that room as surely as one on this side (its pads block the other
                // layer and the via sites the escapes need). So the halos apply across the sides;
                // where neither part keeps one, the sides pass each other as the courtyard rule allows.
                let (own, body_of) = (halo(part), of(part));
                let reach = own + most;
                let near = BBox::new(near.min.sub(P::new(reach, reach)), near.max.add(P::new(reach, reach)));
                for side in [pose.side, pose.side.flipped()] {
                    self.index.courts_near(near, side, |o, other| {
                        if of(other) == body_of { return }
                        let h = own + halo(other);
                        if side != pose.side && h == 0 { return }
                        let o = BBox::new(o.min.sub(P::new(h, h)), o.max.add(P::new(h, h)));
                        abut(o, body);
                        forbid(body, o);
                    });
                }
            }
            forbidden.sort_unstable_by_key(|f| f.min.y);
            if let Some(region) = &self.region {
                // Inside an edge, the body's far side and the pads' far side abut it from within.
                for (edge, own) in [(region.outline_box(), body), (region.inset_box(), pads)] {
                    if edge.is_empty() || own.is_empty() { continue }
                    xs.extend([edge.min.x - own.min.x, edge.max.x - own.max.x]);
                    ys.extend([edge.min.y - own.min.y, edge.max.y - own.max.y]);
                }
            }
            let step = (r / 8).max(grid);
            let lattice = |lo: Nm, hi: Nm| (-(-i64::from(lo)).div_euclid(step)..=i64::from(hi).div_euclid(step)).map(move |k| (k * step) as Nm);
            let mut points: Vec<(i128, Nm, Nm)> = Vec::new();
            let mut keep = |x: Nm, y: Nm| {
                let (dx, dy) = (i128::from(x) - i128::from(want.x), i128::from(y) - i128::from(want.y));
                let d = dx * dx + dy * dy;
                if d > tried && d <= i128::from(r) * i128::from(r) { points.push((d, y, x)) }
            };
            if fixed_x { xs = vec![want.x] }
            if fixed_y { ys = vec![want.y] }
            xs.sort_unstable();
            xs.dedup();
            ys.sort_unstable();
            ys.dedup();
            let window = BBox::new(
                P::new(window.min.x.max(allowed.min.x), window.min.y.max(allowed.min.y)),
                P::new(window.max.x.min(allowed.max.x), window.max.y.min(allowed.max.y)));
            let (xs, ys) = (in_window(&xs, window.min.x, window.max.x), in_window(&ys, window.min.y, window.max.y));
            // Per column, the forbidden spans of y are skipped before any point is kept or tested.
            let column = |x: Nm, ys: &mut dyn Iterator<Item = Nm>, keep: &mut dyn FnMut(Nm, Nm)| {
                let mut spans = forbidden.iter().filter(|f| f.min.x < x && x < f.max.x).peekable();
                let mut hi = Nm::MIN;
                for y in ys {
                    while let Some(f) = spans.next_if(|f| f.min.y < y) { hi = hi.max(f.max.y) }
                    if y >= hi { keep(x, y) }
                }
            };
            for &x in xs {
                column(x, &mut ys.iter().copied(), &mut keep);
            }
            let lattice_ys: Vec<Nm> = if fixed_y { vec![want.y] } else { lattice(window.min.y, window.max.y).collect() };
            let lattice_xs: Vec<Nm> = if fixed_x { vec![want.x] } else { lattice(window.min.x, window.max.x).collect() };
            for &x in &lattice_xs {
                column(x, &mut lattice_ys.iter().copied(), &mut keep);
            }
            points.sort_unstable();
            points.dedup();
            if let Some(&(_, y, x)) = points.iter().find(|&&(_, y, x)| accept(P::new(x, y)) && self.fits(id, Pose { at: P::new(x, y), ..pose })) {
                return Some(P::new(x, y));
            }
            if r >= reach { return None }
            tried = i128::from(r) * i128::from(r);
            r *= 2;
        }
    }
}

fn in_window(sorted: &[Nm], lo: Nm, hi: Nm) -> &[Nm] {
    let a = sorted.partition_point(|&v| v < lo);
    let b = sorted.partition_point(|&v| v <= hi);
    &sorted[a..b.max(a)]
}
