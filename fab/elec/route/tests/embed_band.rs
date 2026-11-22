use fab_elec_core::geom::orient;
use fab_elec_core::{clearance, mm, BBox, Nm, P, Shape};
use fab_elec_route::embed::band::{
    make_legal, strip_loops, swept, tighten, Band, BandInput, GapInfo, Obstacles, Part, PartKey,
};
use fab_elec_route::embed::hull::{covers, hull_parts};
use fab_elec_route::embed::octi;
use fab_elec_route::embed::side::{keep, Keep};
use fab_elec_route::proposal::BlockCause;
use std::cmp::Ordering;

// An odd width exposes the half-nm difference between exact clearance and integer hull inflation.
const WIDTH: Nm = 200_001;
const NEED: Nm = 99_999;

struct World {
    shapes: Vec<(Shape, Nm)>,
    /// Query hulls grouped by item, in construction order.
    by_item: Vec<Vec<fab_elec_route::embed::hull::Hull>>,
    w: Nm,
    queries: Vec<BBox>,
    gaps: usize,
    /// An item left out of every query box whose width plus height exceeds the bound: a part
    /// the sweeps miss while the tiles and point queries still meet it.
    hidden: Option<(u32, i64)>,
}

impl World {
    fn new(shapes: Vec<(Shape, Nm)>) -> Self {
        let by_item = shapes.iter().map(|(shape, need)| hull_parts(shape, *need, WIDTH)).collect();
        Self { shapes, by_item, w: WIDTH, queries: Vec::new(), gaps: 0, hidden: None }
    }

    fn parts(&self) -> Vec<Part> {
        self.shapes.iter().enumerate().flat_map(|(item, (shape, need))| {
            hull_parts(shape, *need, self.w).into_iter().map(move |hull| {
                Part { key: PartKey { item: item as u32, part: hull.part }, hull }
            })
        }).collect()
    }

    fn segment(&self, a: P, b: P) -> Shape { Shape::Segment { a, b, w: self.w } }
}

// All gap calls in successful fixtures are between circles. For a blocked circle/segment gap,
// nearest points are immaterial; the exact have/need comparison happens before they are used.
fn nearest(shape: &Shape, other: P) -> P {
    match shape {
        Shape::Circle { c, r } => {
            let dx = other.x as f64 - c.x as f64;
            let dy = other.y as f64 - c.y as f64;
            let d = dx.hypot(dy);
            if d == 0.0 { *c } else {
                P::from_f(c.x as f64 + dx * *r as f64 / d, c.y as f64 + dy * *r as f64 / d)
            }
        }
        Shape::Segment { a, b, .. } => {
            let dx = b.x as f64 - a.x as f64;
            let dy = b.y as f64 - a.y as f64;
            let d = dx * dx + dy * dy;
            let t = if d == 0.0 { 0.0 } else {
                (((other.x as f64 - a.x as f64) * dx + (other.y as f64 - a.y as f64) * dy) / d)
                    .clamp(0.0, 1.0)
            };
            P::from_f(a.x as f64 + t * dx, a.y as f64 + t * dy)
        }
        _ => shape.bbox().center(),
    }
}

impl Obstacles for World {
    fn near_items(&mut self, area: BBox, out: &mut Vec<u32>) {
        self.queries.push(area);
        // Reverse order and duplicates exercise the contract's arbitrary-order/superset allowance.
        let large = area.width() + area.height();
        for (item, hulls) in self.by_item.iter().enumerate().rev() {
            let item = item as u32;
            if self.hidden.is_some_and(|(hidden, bound)| item == hidden && large > bound) { continue; }
            if hulls.iter().any(|hull| BBox::of_points(hull.verts()).intersects(&area)) {
                out.push(item);
                out.push(item);
            }
        }
    }

    fn parts(&self, item: u32) -> &[fab_elec_route::embed::hull::Hull] {
        &self.by_item[item as usize]
    }

    fn leg_clear(&mut self, item: u32, a: P, b: P) -> bool {
        let (shape, need) = &self.shapes[item as usize];
        clearance(&self.segment(a, b), shape) >= *need
    }

    fn leg_room(&mut self, item: u32, a: P, b: P) -> Nm {
        let (shape, need) = &self.shapes[item as usize];
        if *need == 0 { return Nm::MAX / 2; }
        clearance(&self.segment(a, b), shape).saturating_sub(*need)
    }

    fn leg_gap(&mut self, item: u32, a: P, b: P) -> (Nm, Nm) {
        let (shape, need) = &self.shapes[item as usize];
        (clearance(&self.segment(a, b), shape), *need)
    }

    fn gap(&mut self, x: u32, y: u32) -> GapInfo {
        self.gaps += 1;
        let (x, nx) = &self.shapes[x as usize];
        let (y, ny) = &self.shapes[y as usize];
        GapInfo { have: clearance(x, y), need: self.w + nx + ny,
            near_x: nearest(x, y.bbox().center()), near_y: nearest(y, x.bbox().center()),
            offset_x: nx + self.w / 2 }
    }

    fn end_clear(&mut self, p: P) -> Option<(u32, Nm, Nm)> {
        self.shapes.iter().enumerate().find_map(|(item, (shape, need))| {
            let have = clearance(&self.segment(p, p), shape);
            (have < *need).then_some((item as u32, have, *need))
        })
    }

    fn covers(&mut self, p: P) -> bool {
        self.shapes.iter().any(|(shape, need)| covers(shape, *need, self.w, p))
    }
}

fn input(reference: &[P]) -> BandInput<'_> {
    BandInput { s: reference[0], e: reference[reference.len() - 1], reference, tile: mm(2.0) }
}

fn circle(x: f64, y: f64, radius: f64) -> (Shape, Nm) {
    (Shape::Circle { c: P::mm(x, y), r: mm(radius) }, NEED)
}

fn wall(a: P, b: P, width: f64) -> (Shape, Nm) {
    (Shape::Segment { a, b, w: mm(width) }, NEED)
}

fn on(a: P, b: P, p: P) -> bool {
    orient(a, b, p) == 0 && BBox::new(a, b).contains(p)
}

fn touches(band: &Band, part: &Part) -> bool {
    part.hull.verts().iter().any(|&v| band.points.windows(2).any(|p| on(p[0], p[1], v)))
}

fn check(name: &str, band: &Band, input: &BandInput<'_>, world: &mut World, limit: bool) {
    assert_eq!(band.points.first(), Some(&input.s), "{name}: start");
    let mut reference = strip_loops(input.reference);
    reference[0] = input.s;
    let last = reference.len() - 1;
    reference[last] = input.e;
    let parts = world.parts();
    for pair in band.points.windows(2) {
        let dx = pair[1].x as i128 - pair[0].x as i128;
        let dy = pair[1].y as i128 - pair[0].y as i128;
        assert!(dx == 0 || dy == 0 || dx.abs() == dy.abs(), "{name}: O at {pair:?}");
        for part in &parts {
            assert!(octi::enters(&part.hull, pair[0], pair[1]).is_none()
                || world.leg_clear(part.key.item, pair[0], pair[1]), "{name}: L at {pair:?}, {:?}", part.key);
        }
    }
    // A blocked prefix has another chord (and [s] has no side at all). Global S is defined
    // only on completed bands; requiring it on test 7/12/13's prefixes would be contradictory.
    for part in &parts {
        assert!(band.blocked.is_some() || keep(&band.points, part.hull.c) == keep(&reference, part.hull.c),
            "{name}: S for {:?}", part.key);
    }
    if band.blocked.is_none() {
        assert_eq!(band.points.last(), Some(&input.e), "{name}: end");
        assert_eq!(band.stats.s_failures, 0, "{name}: S diagnostic");
    }
    let bound: usize = band.stats.legs.iter().map(|leg| 2 * leg.seen + 1).sum();
    assert!(band.stats.rounds <= bound, "{name}: total round bound");
    assert_eq!(band.stats.rounds, band.stats.legs.iter().map(|leg| leg.rounds).sum::<usize>(),
        "{name}: rounds accounted per attempt");
    for leg in &band.stats.legs {
        assert!(leg.rounds <= 2 * leg.seen + 1, "{name}: per-leg round bound");
        assert!(leg.splits <= leg.seen, "{name}: split bound");
    }
    assert!(band.stats.merges <= reference.len().saturating_sub(2), "{name}: merge bound");
    assert!(band.stats.tighten_passes <= band.stats.tighten_bound, "{name}: tighten bound");
    assert!(limit || band.blocked.map(|b| b.cause) != Some(BlockCause::Unsettled),
        "{name}: no Unsettled: {:?}, {:?}", band.blocked, band.stats.limit);
    for span in &band.wraps {
        assert!(span.first <= span.last && span.last < band.points.len(), "{name}: wrap indices");
        let Some(part) = parts.iter().find(|part| part.key == span.key) else {
            panic!("{name}: known wrap part");
        };
        assert!(band.points[span.first..=span.last].iter().all(|p| part.hull.verts().contains(p)),
            "{name}: wrap vertices");
        let visits: Vec<_> = band.wraps.iter().filter(|v| v.key == span.key).collect();
        assert!(visits.iter().enumerate().all(|(i, v)| v.visit as usize == i), "{name}: wrap visits");
    }
}

#[test]
fn one_pad_on_the_line() {
    // A unit inflated circle sits on the chord. The shallow apex is inside it, so the two
    // reference legs merge; its sign chooses the upper or lower horizontal hull edge at y=±1.
    for sign in [-1.0, 1.0] {
        let mut world = World::new(vec![circle(0.0, 0.0, 0.8)]);
        let trail = [P::mm(-4.0, 0.0), P::mm(0.0, sign * 0.2), P::mm(4.0, 0.0)];
        let input = input(&trail);
        let band = make_legal(&input, &mut world);
        check("one pad", &band, &input, &mut world, false);
        assert!(band.blocked.is_none(), "one pad: complete");
        let length = octi::len(&band.points);
        let taut = tighten(band, &input, &mut world);
        check("one pad tightened", &taut, &input, &mut world, false);
        assert!(octi::cmp_len(octi::len(&taut.points), length) != Ordering::Greater,
            "one pad: tightening never lengthens");
        assert!(touches(&taut, &world.parts()[0]), "one pad: settled onto a hull vertex");
        assert!(taut.points.iter().all(|p| sign * p.y as f64 >= 0.0), "one pad: selected half-plane");
        assert!(taut.points.iter().any(|p| p.y == mm(sign)), "one pad: touches y=±1 mm");
        let again = tighten(taut.clone(), &input, &mut world);
        check("one pad idempotent", &again, &input, &mut world, false);
        assert_eq!(again.points, taut.points, "one pad: tighten idempotent");
        assert_eq!(again.wraps, taut.wraps, "one pad: wraps idempotent");
        assert_eq!(again.stats.queries, world.queries.len(), "one pad: all near calls counted");
    }
    assert_eq!(octi::corners(P::mm(0.0, 0.0), P::mm(4.0, 1.0)),
        [P::mm(3.0, 0.0), P::mm(1.0, 1.0)], "one pad: exact elbow order");
    assert_eq!(octi::cmp_len((0, 5), (7, 0)), Ordering::Greater, "one pad: 5√2 > 7");
    assert_eq!(octi::cmp_len((0, 12), (17, 0)), Ordering::Less, "one pad: 12√2 < 17");
    let part = World::new(vec![circle(0.0, 0.0, 0.8)]).parts().remove(0);
    assert_eq!(octi::enters(&part.hull, P::mm(-2.0, 0.0), P::mm(2.0, 0.0)), Some((1_000_000, 4_000_000)),
        "one pad: exact entry rational");
    assert!(octi::enters(&part.hull, P::mm(-2.0, 1.0), P::mm(2.0, 1.0)).is_none(),
        "one pad: open hull excludes the top boundary");
}

#[test]
fn mutual_swallow() {
    // X is above the chord with radius 2 after inflation. Its lower detour at y=-1.8 runs
    // through j/k, radius .6, at (-3,-1.5)/(3,-1.5); the final band must go above both.
    let mut world = World::new(vec![circle(0.0, 0.2, 1.8), circle(-3.0, -1.5, 0.4), circle(3.0, -1.5, 0.4)]);
    let trail = [P::mm(-6.0, 0.0), P::mm(6.0, 0.0)];
    let input = input(&trail);
    let parts = world.parts();
    assert!(octi::enters(&parts[1].hull, trail[0], trail[1]).is_none(), "mutual swallow: j initially missed");
    assert!(octi::enters(&parts[2].hull, trail[0], trail[1]).is_none(), "mutual swallow: k initially missed");
    let band = make_legal(&input, &mut world);
    check("mutual swallow", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "mutual swallow: complete");
    assert!(parts.iter().all(|part| touches(&band, part)), "mutual swallow: touches X, j and k");
    assert!(band.stats.rounds >= 2, "mutual swallow: closure grows J");
}

#[test]
fn growth_sweep() {
    // Two overlapping unit hulls above y=0 pull the path to y=-.8. A tiny pad at (-3,-.4)
    // lies in the swept strip, clear of that lower path: only the move test discovers it.
    let mut world = World::new(vec![circle(-0.8, 0.2, 0.8), circle(0.8, 0.2, 0.8), circle(-3.0, -0.4, 0.0)]);
    let trail = [P::mm(-5.0, 0.0), P::mm(5.0, 0.0)];
    let outer = [trail[0], P::mm(-4.2, -0.8), P::mm(4.2, -0.8), trail[1]];
    assert!(outer.windows(2).all(|p| world.leg_clear(2, p[0], p[1])), "growth sweep: third pad is not a collision");
    let swept_parts = swept(&trail, &outer, &mut world);
    assert!(swept_parts.iter().any(|part| part.key.item == 2), "growth sweep: third centre is swept");
    let input = input(&trail);
    let band = make_legal(&input, &mut world);
    check("growth sweep", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "growth sweep: complete");
    assert!(touches(&band, &world.parts()[2]), "growth sweep: third pad joins the chain");
    assert!(band.stats.rounds >= 2, "growth sweep: closure round");
}

#[test]
fn wall_and_pad() {
    // East just above a horizontal wall, counter-clockwise round a pad beyond its east end,
    // then west above the wall. The two wall passes share a physical side but reverse local sides.
    let mut world = World::new(vec![wall(P::mm(-2.0, 0.0), P::mm(2.0, 0.0), 1.6), circle(4.0, 2.0, 0.8)]);
    let trail = [P::mm(-5.0, 0.4), P::mm(4.0, 0.4), P::mm(5.5, 2.0),
        P::mm(4.0, 3.6), P::mm(3.2, 2.0), P::mm(4.0, 0.6), P::mm(-5.0, 0.6)];
    let input = input(&trail);
    let band = make_legal(&input, &mut world);
    check("wall and pad", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "wall and pad: complete");
    let visits: Vec<_> = band.wraps.iter().filter(|span| span.key.item == 0).collect();
    assert_eq!(visits.len(), 2, "wall and pad: two visits of X");
    let c = world.parts()[0].hull.c;
    let local: Vec<_> = visits.iter().map(|span| {
        keep(&band.points[span.first.saturating_sub(1)..=(span.last + 1).min(band.points.len() - 1)], c)
    }).collect();
    assert_eq!(local, vec![Some(Keep::Right), Some(Keep::Left)], "wall and pad: opposite local sides");
    assert!(band.wraps.iter().any(|span| span.key.item == 1), "wall and pad: visits Y");
}

#[test]
fn stripped_circle() {
    // A clockwise square loop leaves and returns to (-2,2), enclosing the pad. Removing the
    // repeated anchor leaves the same three collinear points as the unlooped trail above the pad.
    let mut world = World::new(vec![circle(0.0, 0.0, 0.8)]);
    let plain = [P::mm(-4.0, 2.0), P::mm(-2.0, 2.0), P::mm(4.0, 2.0)];
    let looped = [plain[0], plain[1], P::mm(2.0, 2.0), P::mm(2.0, -2.0),
        P::mm(-2.0, -2.0), plain[1], plain[2]];
    assert_eq!(strip_loops(&looped), plain, "stripped circle: erases the closed square");
    let a = make_legal(&input(&plain), &mut world);
    check("stripped circle plain", &a, &input(&plain), &mut world, false);
    let b = make_legal(&input(&looped), &mut world);
    check("stripped circle loop", &b, &input(&looped), &mut world, false);
    assert!(a.blocked.is_none() && b.blocked.is_none(), "stripped circle: complete");
    assert_eq!(a.points, b.points, "stripped circle: identical bands");
    assert_eq!(a.wraps, b.wraps, "stripped circle: identical wraps");
}

#[test]
fn slalom() {
    // Three unit hulls alternate above/below/above the eastbound chord, four mm apart.
    // The path goes below the first and third and above the middle, with three separate runs.
    let mut world = World::new(vec![circle(-4.0, 0.5, 0.8), circle(0.0, -0.5, 0.8), circle(4.0, 0.5, 0.8)]);
    let trail = [P::mm(-7.0, 0.0), P::mm(7.0, 0.0)];
    let input = input(&trail);
    let band = make_legal(&input, &mut world);
    check("slalom", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "slalom: complete");
    for part in world.parts() {
        assert!(touches(&band, &part), "slalom: touches run {}", part.key.item);
        let side = if part.key.item == 1 { Keep::Right } else { Keep::Left };
        assert_eq!(keep(&band.points, part.hull.c), Some(side), "slalom: alternating S");
    }
    let visits: Vec<_> = band.wraps.iter().map(|span| span.key.item).collect();
    assert_eq!(visits, vec![0, 1, 2], "slalom: one chain per run in order");
    assert_eq!(band.stats.splits, 0, "slalom: initial order is correct");
}

#[test]
fn diagonal_gap_that_fits() {
    // Point pads at (-1,-1)/(1,1) have floor gap 2,828,427 nm. With odd w=200,001 and
    // needs 1,314,213 each, x+y=0 passes exactly. Integer hull radius 1,414,214 gives
    // diagonal support 2,000,001, so the hulls overlap by two nm although the gap fits.
    // The reference y=-2x enters both hulls; its steep approaches leave the fixed endpoints
    // on the chain boundaries. Growing the first radius by one nm makes have=2,828,426 < need.
    let trail = [P::mm(-4.0, 8.0), P::mm(4.0, -8.0)];
    let diagonal = input(&trail);
    for narrow in [false, true] {
        let mut world = World::new(vec![
            (Shape::Circle { c: P::mm(-1.0, -1.0), r: if narrow { mm(0.000001) } else { 0 } }, 1_314_213),
            (Shape::Circle { c: P::mm(1.0, 1.0), r: 0 }, 1_314_213),
        ]);
        let parts = world.parts();
        assert!(octi::octagons_overlap(&parts[0].hull, &parts[1].hull), "diagonal gap: hull overlap");
        let band = make_legal(&diagonal, &mut world);
        check("diagonal gap", &band, &diagonal, &mut world, false);
        assert!(world.gaps > 0, "diagonal gap: rule e used");
        if narrow {
            let blocked = band.blocked.expect("diagonal gap: narrower gap blocks");
            assert_eq!(blocked.cause, BlockCause::Gap, "diagonal gap: Gap cause");
            assert_eq!((blocked.have, blocked.need), (2_828_426, 2_828_427), "diagonal gap: exact one nm deficit");
        } else {
            assert!(band.blocked.is_none(), "diagonal gap: fixed leg passes");
            assert!(band.points.windows(2).any(|p| p[0] != p[1] && on(p[0], p[1], P::mm(0.0, 0.0))),
                "diagonal gap: fixed leg goes through g=origin");
        }
    }

    // At an exact horizontal fit the hulls only touch: ceil(need + 100,000.5) is both
    // the hull radius and the first legal integer height. Turn the 7:24 slant to a diagonal
    // normal: (31,17) has tangent/normal projections in the ratio (31-17):(31+17) = 7:24.
    // At scale 70,703 the pad's x+y is twice 1,696,872. The hull radius is 1,199,870,
    // with diagonal support 1,696,873, but floor(1,696,872/sqrt(2) - 100,000.5) = 1,099,869.
    // At g, x+y = 1,628,998 and the rooms are -47,994 and +47,994. Sliding 33,937 steps
    // along (1,1) clears both. Moving the second pad down one nm leaves a total room of -1.
    let trail = [P::mm(-4.0, 0.6), P::mm(5.0, 0.6)];
    let slanted = input(&trail);
    for short in [0, 1] {
        let y = P::new(31 * 70_703, 17 * 70_703 - short);
        let mut world = World::new(vec![
            (Shape::Circle { c: P::ZERO, r: 0 }, 1_099_869),
            (Shape::Circle { c: y, r: 0 }, 1_099_869),
        ]);
        assert!(octi::octagons_overlap(&world.by_item[0][0], &world.by_item[1][0]),
            "slanted gap: the inflated point pads must overlap");
        let band = make_legal(&slanted, &mut world);
        check("slanted gap", &band, &slanted, &mut world, false);
        assert!(world.gaps > 0, "slanted gap: the reference must thread the pair");
        if short == 0 {
            assert!(band.blocked.is_none(), "slanted gap: the neck must pass after the slide");
            assert!(band.points.windows(2).any(|leg| leg[0] != leg[1] && leg.iter().all(|p| p.x + p.y == 1_696_872)),
                "slanted gap: a leg must lie on the line that clears both pads");
        } else {
            assert_eq!(band.blocked.map(|b| b.cause), Some(BlockCause::Thread),
                "slanted gap: a one-nanometre deficit must block the thread");
        }
    }
}

#[test]
fn reference_point_inside_a_pad() {
    // The apex (0,.5) is inside a unit hull; the endpoints are below it at y=-1.
    // Merging preserves the triangular reference's upper pass, so the pad remains on the Right.
    let mut world = World::new(vec![circle(0.0, 0.0, 0.8)]);
    let trail = [P::mm(-4.0, -1.0), P::mm(0.0, 0.5), P::mm(4.0, -1.0)];
    let input = input(&trail);
    let band = make_legal(&input, &mut world);
    check("inside reference", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "inside reference: complete");
    assert_eq!(band.stats.merges, 1, "inside reference: exactly one merge");
    assert_eq!(keep(&band.points, P::mm(0.0, 0.0)), Some(Keep::Right), "inside reference: retained side");
    assert!(band.points.iter().any(|p| p.y == mm(1.0)), "inside reference: upper hull edge");
}

#[test]
fn misorder() {
    // L1/L2 have inflated radii 1/3, with centres at (-3,.5)/(3,.5). Their descending
    // lower bridge elbows through R's tip at (0,-2.2). R continues southeast to (10,-12.2),
    // so its centre projects at x=5, after L2; the correct order is L1, R, L2. Starting at
    // x=-4.5 keeps L1 exposed on the initial convex chain instead of hiding it behind L2.
    let mut world = World::new(vec![circle(-3.0, 0.5, 0.8), circle(3.0, 0.5, 2.8),
        wall(P::mm(0.0, -2.2), P::mm(10.0, -12.2), 0.0)]);
    let trail = [P::mm(-4.5, 0.0), P::mm(10.0, 0.0)];
    let input = input(&trail);
    let parts = world.parts();
    assert!(parts[2].hull.c.x > parts[1].hull.c.x, "misorder: R projects last");
    let band = make_legal(&input, &mut world);
    check("misorder", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "misorder: complete");
    assert_eq!(band.stats.splits, 1, "misorder: one split");
    let visits: Vec<_> = band.wraps.iter().map(|span| span.key.item).collect();
    assert_eq!(visits, vec![0, 2, 1], "misorder: three runs L1, R, L2");
}

#[test]
fn pocket() {
    // Two unit hulls at (-1.5,.2)/(1.5,.5) leave p=(0,.2) outside both but inside their
    // convex bridge. q=(2,.59) is inside the right hull, so merging q exposes a two-part
    // Right run ending at p. Its chain has no boundary endpoint there: a second merge removes p.
    let mut world = World::new(vec![circle(-1.5, 0.2, 0.8), circle(1.5, 0.5, 0.8)]);
    let trail = [P::mm(-4.0, 0.0), P::mm(2.0, 0.59), P::mm(0.0, 0.2), P::mm(4.0, 0.0)];
    let input = input(&trail);
    assert!(world.parts().iter().all(|part| !part.hull.strictly_contains(trail[2])), "pocket: p outside individual hulls");
    let vertices: Vec<_> = world.parts().iter().flat_map(|part| {
        part.hull.verts().into_iter().map(move |p| (part.key, p))
    }).collect();
    assert!(octi::chain(trail[2], trail[2], &vertices, Keep::Right).is_none(),
        "pocket: p is inside the combined convex hull");
    let band = make_legal(&input, &mut world);
    check("pocket", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "pocket: complete");
    assert_eq!(band.stats.merges, 2, "pocket: q merge followed by pocket merge");
    assert!(!band.points.contains(&trail[2]), "pocket: interior chain end removed");
    for part in world.parts() {
        assert_eq!(keep(&band.points, part.hull.c), Some(Keep::Right), "pocket: both parts retain Right");
    }
}

#[test]
fn spiral() {
    // An inward rectangular spiral makes six quarter turns around the unit pad. Successive
    // west/bottom/east walls move inward by a mm, so no nonadjacent segments intersect.
    let mut world = World::new(vec![circle(0.0, 0.0, 0.8)]);
    let trail = [P::mm(-5.0, -4.0), P::mm(5.0, -4.0), P::mm(5.0, 4.0),
        P::mm(-4.0, 4.0), P::mm(-4.0, -3.0), P::mm(3.0, -3.0), P::mm(3.0, 3.0)];
    let input = input(&trail);
    assert_eq!(strip_loops(&trail), trail, "spiral: noncrossing turns retained");
    let band = make_legal(&input, &mut world);
    check("spiral", &band, &input, &mut world, false);
    assert!(band.blocked.is_none(), "spiral: complete");
    assert_eq!(band.points, trail, "spiral: already legal octilinear reference is unchanged");
    assert_eq!(band.stats.merges, 0, "spiral: no merge");
}

#[test]
fn documented_limit() {
    // L1 is a radius-3 hull at (-4,.5); R is radius .5 at (0,.1). L2 is a thin southeast
    // wall starting at (-1,-.74). The reference goes above R, then below L2's centre, making
    // L1/R/L2 local Left/Right/Left. L1's lower-right vertex (-1,-.742641) is inside L2;
    // entering that already-known same-side part of another run is the documented limit.
    let mut world = World::new(vec![circle(-4.0, 0.5, 2.8), circle(0.0, 0.1, 0.3),
        wall(P::mm(-1.0, -0.74), P::mm(5.0, -6.74), 0.0)]);
    let trail = [P::mm(-8.0, 0.0), P::mm(0.0, 0.4), P::mm(2.0, -3.8), P::mm(8.0, 0.0)];
    let input = input(&trail);
    let band = make_legal(&input, &mut world);
    check("documented limit", &band, &input, &mut world, true);
    assert_eq!(band.blocked.map(|b| b.cause), Some(BlockCause::Unsettled), "documented limit: Unsettled");
    let geometry = band.stats.limit.as_ref().expect("documented limit: collision geometry");
    assert_eq!(geometry.part.map(|key| key.item), Some(2), "documented limit: L2 entered");
    assert!(geometry.run.iter().any(|key| key.item == 0), "documented limit: entering L1's run");
    assert!(geometry.leg[0] != geometry.leg[1], "documented limit: actual entering leg recorded");
}

#[test]
fn ends() {
    // e=(.5,0) lies inside the exact pad clearance; (.95,.35) is inside the unit octagon
    // but outside the exact circle (its squared radius is 1.025). A horizontal approach from
    // the right is legal; the same slack endpoint approached through the pad from the left is End.
    let mut world = World::new(vec![circle(0.0, 0.0, 0.8)]);
    let bad = [P::mm(3.0, 0.0), P::mm(0.5, 0.0)];
    let band = make_legal(&input(&bad), &mut world);
    check("ends inside", &band, &input(&bad), &mut world, false);
    let blocked = band.blocked.expect("ends: exact endpoint blocks");
    assert_eq!(blocked.cause, BlockCause::End, "ends: End cause");
    assert_eq!(blocked.at, bad[1], "ends: exact end location");
    assert!(blocked.have < blocked.need, "ends: exact deficit");
    assert_eq!(band.points, vec![bad[0]], "ends: only start on end failure");
    let slack = [P::mm(3.0, 0.35), P::mm(0.95, 0.35)];
    assert!(world.parts()[0].hull.strictly_contains(slack[1]), "ends: strictly in hull slack");
    let band = make_legal(&input(&slack), &mut world);
    check("ends slack", &band, &input(&slack), &mut world, false);
    assert!(band.blocked.is_none(), "ends: exact slack endpoint passes");
    assert_eq!(band.points, slack, "ends: exact straight approach");
    let through = [P::mm(-3.0, 0.35), slack[1]];
    let band = make_legal(&input(&through), &mut world);
    check("ends slack leg", &band, &input(&through), &mut world, false);
    let blocked = band.blocked.expect("ends: slack leg blocks");
    assert_eq!(blocked.cause, BlockCause::End, "ends: slack leg End cause");
    assert!(blocked.have < blocked.need, "ends: exact leg deficit");
    // Full-range deltas and length differences exercise widened signs/squares without huge trails.
    let p = P::mm(-2147.0, -2147.0);
    let q = P::mm(2147.0, 2146.0);
    let corners = octi::corners(p, q);
    assert!(corners.iter().all(|&c| BBox::new(p, q).contains(c)), "ends: full-range corners stay representable");
    assert_eq!(octi::cmp_len((i64::MAX, i64::MIN), (i64::MIN, i64::MAX)), Ordering::Less,
        "ends: full-range exact length comparison");
    let mut empty = World::new(Vec::new());
    let diagonal = [P::mm(-12.0, -12.0), P::mm(12.0, 12.0)];
    let input = input(&diagonal);
    let band = make_legal(&input, &mut empty);
    check("ends tiled diagonal", &band, &input, &mut empty, false);
    assert!(band.blocked.is_none(), "ends tiled diagonal: complete");
    // Every query, the final legality check's included, stays inside one short tile: no query
    // grows with the band's box.
    assert!(empty.queries.iter().all(|b| {
        b.width() + b.height() <= input.tile as i64
    }), "ends tiled diagonal: every query is bounded by one tile");
    assert_eq!(band.stats.queries, empty.queries.len(), "ends tiled diagonal: near calls counted");
}

#[test]
fn touch_at_a_hidden_endpoint() {
    // Misorder's thin southeast wall: an incoming bridge touches its tip at the top edge's east
    // vertex a. The far end b = (10, 0) hides a from the hull of the wall plus both ends, so the path
    // touches the wall at a and leaves straight, keeping the wall on its right. Kept on the left, the
    // wall ahead of a is on the wrong side: no touch, a is interior. So is a point inside the hull.
    let world = World::new(vec![wall(P::mm(0.0, -2.2), P::mm(10.0, -12.2), 0.0)]);
    let parts = world.parts();
    let vertices: Vec<_> = parts.iter().flat_map(|part| {
        part.hull.verts().into_iter().map(move |p| (part.key, p))
    }).collect();
    let &(key, a) = vertices.iter().max_by_key(|(_, p)| (p.y, p.x)).expect("the wall has vertices");
    let b = P::mm(10.0, 0.0);
    assert_eq!(octi::chain(a, b, &vertices, Keep::Right), Some(vec![(a, Some(key)), (b, None)]),
        "touch: the hidden tip vertex leaves straight toward b");
    assert_eq!(octi::chain(a, b, &vertices, Keep::Left), None, "touch: the wall ahead on the wrong side is no touch");
    assert_eq!(octi::chain(parts[0].hull.c, b, &vertices, Keep::Right), None, "touch: an endpoint inside the hull is interior");
}

#[test]
fn a_part_the_sweeps_missed_is_counted_on_its_new_side() {
    // The trail arches over a small pad at (5, 0.3) under the arch: the reference keeps it on its
    // right. The world hides the pad from every query larger than one tile, so the sweep of the
    // shortcut under the arch misses it and tightening pulls the band straight along y = 0, which
    // keeps it on the left. The pad's hull bottom lies exactly on y = 0 (radius 0.1 mm + need +
    // half the width = 0.3 mm), so the straight band is legal and its own tiles meet the pad:
    // the side check must count exactly this one flip.
    let pad = (Shape::Circle { c: P::new(mm(5.0), mm(0.3)), r: mm(0.1) }, NEED);
    let mut world = World::new(vec![pad]);
    let trail = [P::mm(0.0, 0.0), P::mm(5.0, 5.0), P::mm(10.0, 0.0)];
    let input = input(&trail);
    world.hidden = Some((0, input.tile as i64));
    let legal = make_legal(&input, &mut world);
    assert!(legal.blocked.is_none(), "missed part: the arch is complete");
    assert_eq!(legal.stats.s_failures, 0, "missed part: the arch keeps the reference's sides");
    let band = tighten(legal, &input, &mut world);
    assert!(band.blocked.is_none(), "missed part: the tightened band is complete");
    assert_eq!(band.points, vec![trail[0], trail[2]], "missed part: the band is pulled straight");
    let c = world.parts()[0].hull.c;
    assert_eq!((keep(&trail, c), keep(&band.points, c)), (Some(Keep::Right), Some(Keep::Left)),
        "missed part: the pad really changed sides");
    assert_eq!(band.stats.s_failures, 1, "missed part: the side change is counted once");
}

#[test]
fn a_spike_past_a_moved_parts_centre_keeps_its_old_side() {
    // Shove's transport (the shove answer's S3): a pad moved from (0, 1.5) to (0, -1.5) mm crossed
    // the straight reference y = 0. A pushed track must still pass it on the side it passed it
    // before, below it now. The reference carries that as a spike: the crossing x replaced by a
    // point 1 nm before it, a tip just beyond the new centre on the line from x through it, and a
    // point 1 nm after it, so the centre lies strictly inside the thin triangle and on the
    // reference's other side. The band embeds below the pad, and tightening it again changes
    // nothing. The tip may lie inside the pad's hull (just beyond the centre) or past it. Without
    // the spike the band keeps the side of the new centre: it passes above, untouched.
    let c1 = P::mm(0.0, -1.5);
    let (s, e, x) = (P::mm(-4.0, 0.0), P::mm(4.0, 0.0), P::mm(0.0, 0.0));
    let spiked = |tip: P| vec![s, P::new(x.x - 1, x.y), tip, P::new(x.x + 1, x.y), e];
    for (name, reference, below) in [
        ("spike just beyond the centre", spiked(P::new(c1.x, c1.y - 1)), true),
        ("spike past the hull", spiked(P::new(2 * c1.x - x.x, 2 * c1.y - x.y)), true),
        ("no spike", vec![s, e], false),
    ] {
        let mut world = World::new(vec![circle(0.0, -1.5, 0.8)]);
        let c = world.parts()[0].hull.c;
        assert_eq!(keep(&reference, c), Some(if below { Keep::Left } else { Keep::Right }),
            "{name}: fixture setup: the spike puts the moved centre on the reference's other side");
        let input = input(&reference);
        let band = make_legal(&input, &mut world);
        check(name, &band, &input, &mut world, false);
        assert!(band.blocked.is_none(), "{name}: complete");
        let taut = tighten(band, &input, &mut world);
        check(name, &taut, &input, &mut world, false);
        assert_eq!(taut.points.iter().any(|p| p.y < c1.y), below, "{name}: passes the moved pad below (its old side): {:?}", taut.points);
        if !below {
            assert_eq!(taut.points, vec![s, e], "{name}: the band stays straight above the pad");
        }
        let again = tighten(taut.clone(), &input, &mut world);
        check(name, &again, &input, &mut world, false);
        assert_eq!(again.points, taut.points, "{name}: tightening again changes nothing");
    }
}
