use fab_elec_core::dist::core_distance;
use fab_elec_core::shape::Core;
use fab_elec_core::{clearance, mm, Angle, BBox, Nm, P, PolySet, Shape};
use fab_elec_route::embed::hull::{covers, hull_parts, Hull, NORMALS};
use fab_elec_route::embed::side::{keep, Keep};

#[test]
fn the_side_rule_is_a_function_of_the_reference_alone() {
    let straight = vec![P::mm(0.0, 0.0), P::mm(10.0, 0.0)];
    let bulge = vec![P::mm(0.0, 0.0), P::mm(5.0, 5.0), P::mm(10.0, 0.0)];
    let reverse: Vec<_> = bulge.iter().rev().copied().collect();
    let vertex = vec![P::mm(0.0, 0.0), P::mm(5.0, 0.0), P::mm(5.0, 5.0), P::mm(10.0, 5.0)];
    let top = vec![P::mm(0.0, 0.0), P::mm(0.0, 5.0), P::mm(10.0, 5.0), P::mm(10.0, 0.0)];
    let looping = vec![
        P::mm(0.0, 0.0), P::mm(6.0, 0.0), P::mm(6.0, 2.0),
        P::mm(4.0, 2.0), P::mm(4.0, -1.0), P::mm(10.0, 0.0),
    ];
    let large = vec![P::mm(-1000.0, -500.0), P::mm(0.0, 500.0), P::mm(1000.0, -500.0)];
    let zero = vec![P::mm(1.0, 1.0), P::mm(1.0, 1.0)];
    let single = vec![P::mm(1.0, 1.0)];
    let empty = vec![];
    let repeated = vec![P::ZERO, P::ZERO, P::mm(10.0, 0.0)];
    let extreme = vec![P::new(i32::MIN, 0), P::new(0, i32::MAX), P::new(i32::MAX, 0)];
    let nearer_left = vec![
        P::mm(-1000.0, 0.0), P::mm(-1000.0, 1000.0), P::mm(1000.0, 1000.0),
        P::mm(1000.0, -1000.0), P::mm(100.0, -1000.0), P::mm(100.0, 1000.0), P::mm(1000.0, 0.0),
    ];
    let opposite_tie = vec![
        P::mm(-1000.0, 0.0), P::mm(-1000.0, 1000.0), P::mm(1000.0, 1000.0),
        P::mm(1000.0, -1000.0), P::mm(1000.0, 1000.0), P::mm(1000.0, 0.0),
    ];
    let rows = [
        ("straight left", &straight, P::mm(5.0, 2.0), Some(Keep::Left)),
        ("straight right", &straight, P::mm(5.0, -2.0), Some(Keep::Right)),
        ("below the bulge", &bulge, P::mm(5.0, 2.0), Some(Keep::Right)),
        ("above the bulge", &bulge, P::mm(5.0, 7.0), Some(Keep::Left)),
        ("below the chord", &bulge, P::mm(5.0, -2.0), Some(Keep::Right)),
        ("reversed bulge", &reverse, P::mm(5.0, 2.0), Some(Keep::Left)),
        ("zero-length head", &zero, P::ZERO, None),
        ("single point", &single, P::ZERO, None),
        ("empty reference", &empty, P::ZERO, None),
        ("beyond the straight chord", &straight, P::mm(15.0, 0.0), Some(Keep::Left)),
        ("equidistant chord point: first segment wins", &bulge, P::mm(5.0, 0.0), Some(Keep::Right)),
        // Row 6: chord orientation is negative. The vertical edge crosses at c, not strictly
        // right of it; the closing edge also crosses at x = 0. No crossings, hence Right.
        ("reference vertex", &vertex, P::mm(5.0, 0.0), Some(Keep::Right)),
        // Row 7: chord orientation is positive. At y = 5 both vertical edges exclude their
        // upper endpoint, and the top edge is horizontal: no crossings, hence Left.
        ("reference edge", &top, P::mm(5.0, 5.0), Some(Keep::Left)),
        // Row 8: c is left of the chord. At y = 1 only x = 6 crosses to its right;
        // x = 4 is left of c, so odd parity flips Left to Right despite the full loop.
        ("self-crossing loop", &looping, P::mm(5.0, 1.0), Some(Keep::Right)),
        ("large below bulge", &large, P::mm(0.0, -100.0), Some(Keep::Right)),
        ("large above bulge", &large, P::mm(0.0, 900.0), Some(Keep::Left)),
        ("large below chord", &large, P::mm(0.0, -900.0), Some(Keep::Right)),
        ("large exact rational tie", &large, P::mm(0.0, -500.0), Some(Keep::Right)),
        ("zero segment wins a tie as a point", &repeated, P::ZERO, Some(Keep::Left)),
        ("full-range rational comparison", &extreme, P::ZERO, Some(Keep::Right)),
        ("large nearer segment changes side", &nearer_left, P::ZERO, Some(Keep::Left)),
        ("large equal distances with opposite sides keep the first", &opposite_tie, P::ZERO, Some(Keep::Right)),
    ];
    for (name, reference, c, expected) in rows {
        assert_eq!(keep(reference, c), expected, "{name}");
        for repetition in 0..100 {
            assert_eq!(keep(reference, c), expected, "{name}: repeat {repetition}");
        }
    }
}

struct Xorshift64(u64);

impl Xorshift64 {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn nm(&mut self, lo: Nm, hi: Nm) -> Nm {
        (lo as i64 + (self.next() % (hi as i64 - lo as i64 + 1) as u64) as i64) as Nm
    }

    fn angle(&mut self) -> f64 {
        self.unit() * std::f64::consts::TAU
    }

    fn point(&mut self) -> P {
        P::new(self.nm(-mm(4.0), mm(4.0)), self.nm(-mm(4.0), mm(4.0)))
    }
}

fn free(parts: &[Hull], p: P) -> bool {
    parts.iter().all(|part| {
        NORMALS.iter().zip(part.s).any(|(&(nx, ny), s)| {
            nx as i128 * p.x as i128 + ny as i128 * p.y as i128 >= s as i128
        })
    })
}

fn check_hulls(kind: &str, seed: usize, shape: &Shape, need: Nm, w: Nm, rng: &mut Xorshift64) {
    let parts = hull_parts(shape, need, w);
    assert!(!parts.is_empty(), "{kind} seed {seed}: nonempty keep-out parts");
    assert_eq!(parts, hull_parts(shape, need, w), "{kind} seed {seed}: determinism");
    assert!(hull_parts(shape, 0, w).is_empty(), "{kind} seed {seed}: zero clearance is passable");
    assert!(hull_parts(shape, -1, w).is_empty(), "{kind} seed {seed}: negative clearance is passable");
    assert!(!covers(shape, 0, w, parts[0].c), "{kind} seed {seed}: covers agrees for passable items");

    let (core, core_radius) = shape.core();
    let r_true = core_radius + need as f64 + w as f64 / 2.0;
    let r = r_true.ceil();
    let bound = 2.0 * r * ((4.0 - 2.0 * 2.0_f64.sqrt()).sqrt() - 1.0) + 2.0;
    let filled = matches!(&core, Core::Convex(_) | Core::Set(_));
    let check_point = |p: P, property: &str| {
        if free(&parts, p) {
            let gap = clearance(&Shape::Segment { a: p, b: p, w }, shape);
            // For unfilled cores also check without covers, so its clearance fallback cannot
            // conceal a missing boundary band in the octagons themselves.
            if !filled || !covers(shape, need, w, p) {
                assert!(gap >= need, "{kind} seed {seed}: conservative {property} at {p:?}, gap {gap} < {need}");
            }
        }
    };
    for (i, part) in parts.iter().enumerate() {
        assert_eq!(part.part as usize, i, "{kind} seed {seed}: part numbering {i}");
        for (k, v) in part.verts().into_iter().enumerate() {
            assert!(!part.strictly_contains(v), "{kind} seed {seed}: vertex boundary is outside, part {i} vertex {k}");
            for plane in [k, (k + 1) % 8] {
                let (nx, ny) = NORMALS[plane];
                assert_eq!(nx * v.x as i64 + ny * v.y as i64, part.s[plane],
                    "{kind} seed {seed}: exact vertex intersection, part {i} vertex {k} plane {plane}");
            }
            let slack = core_distance(&Core::Point(v), &core) - r;
            assert!(slack <= bound + 2.0, "{kind} seed {seed}: slack {slack} exceeds {}, part {i} vertex {k}", bound + 2.0);
            check_point(v, "vertex free of other parts and not covered");
        }
    }
    let bb = BBox::of_points(parts.iter().flat_map(|part| part.verts())).inflate(mm(1.0));
    for _ in 0..128 {
        let p = P::new(rng.nm(bb.min.x, bb.max.x), rng.nm(bb.min.y, bb.max.y));
        assert_eq!(free(&parts, p), !parts.iter().any(|part| part.strictly_contains(p)),
            "{kind} seed {seed}: strict integer containment agrees with the planes at {p:?}");
        check_point(p, "sample in grown box");
    }

    // Coordinates of each offset truncate toward its core point, never out of the true band.
    let inward = |c: P, dx: f64, dy: f64| {
        P::new(c.x + ((r_true - 1.0) * dx).trunc() as Nm, c.y + ((r_true - 1.0) * dy).trunc() as Nm)
    };
    let check_tight = |p: P| {
        assert!(clearance(&Shape::Segment { a: p, b: p, w }, shape) < need,
            "{kind} seed {seed}: tight sample is inside the true inflated shape at {p:?}");
        assert!(parts.iter().any(|part| part.strictly_contains(p)),
            "{kind} seed {seed}: tight sample is strictly inside a part at {p:?}");
    };
    match shape {
        Shape::Circle { c, .. } => {
            assert_eq!(parts.len(), 1, "{kind} seed {seed}: circle count");
            for _ in 0..32 {
                let angle = rng.angle();
                check_tight(inward(*c, angle.cos(), angle.sin()));
            }
        }
        Shape::Segment { a, b, .. } => {
            let dx = b.x as i64 - a.x as i64;
            let dy = b.y as i64 - a.y as i64;
            if dx == 0 || dy == 0 || dx.abs() == dy.abs() {
                assert_eq!(parts.len(), 1, "{kind} seed {seed}: axis or diagonal segment count");
            }
            if kind == "Segment 22.5 degrees" {
                assert!(parts.len() > 1, "{kind} seed {seed}: long off-axis segment splits");
            }
            let length = (dx as f64).hypot(dy as f64);
            for _ in 0..32 {
                let angle = rng.angle();
                let (ux, uy) = (angle.cos(), angle.sin());
                let cap = if ux * dx as f64 + uy * dy as f64 >= 0.0 { *b } else { *a };
                check_tight(inward(cap, ux, uy));
                // Also exercise the sides and joins of split parts. Rounding the anchor adds
                // at most sqrt(0.5) nm, still less than the one-nm inward margin.
                let t = rng.unit();
                let anchor = P::from_f(a.x as f64 + dx as f64 * t, a.y as f64 + dy as f64 * t);
                let sign = if rng.next() & 1 == 0 { 1.0 } else { -1.0 };
                check_tight(inward(anchor, -sign * dy as f64 / length, sign * dx as f64 / length));
            }
        }
        _ => {}
    }
    if kind == "Arc round outline" {
        let p = P::mm(0.0, 28.0);
        assert!(free(&parts, p), "{kind} seed {seed}: round outline leaves room 2 mm inside");
        assert!(!covers(shape, need, w, p), "{kind} seed {seed}: round outline interior is routable");
    }
}

#[test]
fn octagon_hulls_are_conservative_and_tight() {
    const SEED: u64 = 0x8b8b_4d3c_2a19_0765;
    for seed in 0..4 {
        let mut rng = Xorshift64(SEED ^ (seed as u64 + 1));
        let need = match seed { 0 => mm(0.1), 1 => mm(0.3), _ => rng.nm(mm(0.1), mm(0.3)) };
        let w = match seed { 0 => mm(0.1), 1 => mm(0.4), 2 => 200_001, _ => rng.nm(mm(0.1), mm(0.4)) };
        let c = rng.point();
        let circle = Shape::Circle { c, r: rng.nm(mm(0.1), mm(1.0)) };
        check_hulls("Circle", seed, &circle, need, w, &mut rng);

        let a = rng.point();
        let length = match seed { 0 => mm(0.001), 1 => mm(500.0), _ => rng.nm(mm(1.0), mm(25.0)) };
        let random_angle = rng.angle();
        let track_width = rng.nm(mm(0.1), mm(0.4)) | 1;
        for (kind, delta) in [
            ("Segment axis", P::new(length, 0)),
            ("Segment diagonal", P::new(length, length)),
            ("Segment 22.5 degrees", P::mm(20.0 * std::f64::consts::FRAC_PI_8.cos(), 20.0 * std::f64::consts::FRAC_PI_8.sin())),
            ("Segment random", P::mm(20.0 * random_angle.cos(), 20.0 * random_angle.sin())),
        ] {
            let shape = Shape::Segment { a, b: a.add(delta), w: track_width };
            check_hulls(kind, seed, &shape, need, w, &mut rng);
        }

        let arc = Shape::Arc { a: rng.point(), mid: rng.point(), b: rng.point(), w: track_width };
        check_hulls("Arc random three points", seed, &arc, need, w, &mut rng);
        if seed == 0 {
            let outline = Shape::Arc { a: P::mm(30.0, 0.0), mid: P::mm(0.0, 30.0), b: P::mm(-30.0, 0.0), w: 0 };
            check_hulls("Arc round outline", seed, &outline, need, w, &mut rng);
        }

        let half = P::new(rng.nm(mm(0.4), mm(1.5)), rng.nm(mm(0.3), mm(1.0)));
        let rect = Shape::Rect { c, half, rot: Angle::rad(rng.angle()), corner: rng.nm(0, half.x.min(half.y)) };
        check_hulls("Rect rotated rounded", seed, &rect, need, w, &mut rng);
        let oval = Shape::Oval {
            c, half: P::new(rng.nm(mm(1.0), mm(2.0)), rng.nm(mm(0.2), mm(0.6))), rot: Angle::rad(rng.angle()),
        };
        check_hulls("Oval rotated", seed, &oval, need, w, &mut rng);

        let extent = rng.nm(mm(6.0), mm(9.0));
        let arm = mm(2.0);
        let l_ring = vec![P::ZERO, P::new(extent, 0), P::new(extent, arm), P::new(arm, arm), P::new(arm, extent), P::new(0, extent)];
        let l = Shape::Poly(PolySet::from_rings_even_odd(&[l_ring]).translated(c));
        check_hulls("Poly concave L", seed, &l, need, w, &mut rng);
        let inside_l = c.add(P::mm(1.0, 1.0));
        assert!(free(&hull_parts(&l, need, w), inside_l), "Poly concave L seed {seed}: deep interior is outside boundary parts");
        assert!(covers(&l, need, w, inside_l), "Poly concave L seed {seed}: covers protects filled interior");

        let square = |r: f64| vec![P::mm(-r, -r), P::mm(r, -r), P::mm(r, r), P::mm(-r, r)];
        let ring = Shape::Poly(PolySet::from_rings_even_odd(&[square(5.0), square(2.0)]).translated(c));
        check_hulls("Poly square with hole", seed, &ring, need, w, &mut rng);
        assert!(free(&hull_parts(&ring, need, w), c), "Poly square with hole seed {seed}: hole interior is free of boundary parts");
        assert!(!covers(&ring, need, w, c), "Poly square with hole seed {seed}: covers preserves the hole");
        assert!(covers(&ring, need, w, c.add(P::mm(4.0, 0.0))), "Poly square with hole seed {seed}: covers protects filled ring");
    }
}
