//! Guards the pair-distance matrix and the one definition of legal spacing shared by DRC and routers.

use fab_elec_board::{check_pair, drc::DRC_EPSILON, required, resolve_rules, Board, ConnItem, Copper};
use fab_elec_core::{
    clearance, collides, mm, Angle, BBox, BoardId, ClassId, ItemId, LayerId, NetId, Nm, PolySet, Shape, P,
};

#[test]
fn pair_matrix() {
    struct Case {
        name: String,
        a: Shape,
        b: Shape,
        expected: Nm,
        has_arc: bool,
    }

    let poly = Shape::Poly(PolySet::from_ring_nonzero(vec![
        P::mm(1.0, 0.0),
        P::mm(0.5, 0.8),
        P::mm(-0.5, 0.8),
        P::mm(-1.0, 0.0),
        P::mm(-0.5, -0.8),
        P::mm(0.5, -0.8),
    ]));
    let rect = Shape::Rect {
        c: P::ZERO,
        half: P::new(600_000, 400_000),
        rot: Angle::ZERO,
        corner: 0,
    };
    let catalogue = [
        ("circle", Shape::Circle { c: P::ZERO, r: 400_000 }),
        (
            "seg_h",
            Shape::Segment { a: P::mm(-1.0, 0.0), b: P::mm(1.0, 0.0), w: 200_000 },
        ),
        (
            "seg_v",
            Shape::Segment { a: P::mm(0.0, -1.0), b: P::mm(0.0, 1.0), w: 200_000 },
        ),
        ("seg_pt", Shape::Segment { a: P::ZERO, b: P::ZERO, w: 300_000 }),
        (
            "arc",
            Shape::Arc {
                a: P::mm(0.0, -1.0),
                mid: P::new(300_000, 0),
                b: P::mm(0.0, 1.0),
                w: 200_000,
            },
        ),
        ("rect", rect.clone()),
        (
            "rrect",
            Shape::Rect {
                c: P::ZERO,
                half: P::new(600_000, 400_000),
                rot: Angle::ZERO,
                corner: 150_000,
            },
        ),
        (
            "rect_q",
            Shape::Rect {
                c: P::ZERO,
                half: P::new(400_000, 600_000),
                rot: Angle::QUARTER,
                corner: 0,
            },
        ),
        (
            "oval",
            Shape::Oval { c: P::ZERO, half: P::new(700_000, 300_000), rot: Angle::ZERO },
        ),
        (
            "oval_v",
            Shape::Oval { c: P::ZERO, half: P::new(300_000, 700_000), rot: Angle::ZERO },
        ),
        ("poly", poly.clone()),
    ];

    // Analytic x projections for this catalogue only; no bbox() supplies the expected gap.
    // A disc adds its radius to its centre; a capsule adds w/2 to its endpoint extrema.
    let x_extent = |shape: &Shape| -> (Nm, Nm) {
        match shape {
            Shape::Circle { c, r } => (c.x - r, c.x + r),
            Shape::Segment { a, b, w } => (a.x.min(b.x) - w / 2, a.x.max(b.x) + w / 2),
            Shape::Arc { a, mid, b, w } => {
                // These symmetric arcs attain their facing centreline extremum at mid.
                // A's max is mid.x + w/2; mirrored B's min is mid.x - w/2, both on the axis.
                // The opposite extrema are on the end caps, off the axis.
                (a.x.min(mid.x).min(b.x) - w / 2, a.x.max(mid.x).max(b.x) + w / 2)
            }
            Shape::Rect { c, half, rot, corner } => {
                // A quarter turn swaps half sizes; rounding shrinks the core by corner,
                // then adds that radius back. The extreme face still meets the axis.
                let half_x = if *rot == Angle::QUARTER { half.y } else { half.x };
                let reach = (half_x - corner) + corner;
                (c.x - reach, c.x + reach)
            }
            Shape::Oval { c, half, .. } => {
                // Both ovals have zero rotation: horizontal core half-length plus disc radius.
                // For oval_v that core half-length is zero, and the radius is half.x.
                let radius = half.x.min(half.y);
                let reach = (half.x - radius) + radius;
                (c.x - reach, c.x + reach)
            }
            Shape::Poly(set) => {
                // Linear edges attain x extrema at vertices: here (-1 mm, 0) and (+1 mm, 0).
                set.rings().flatten().fold((Nm::MAX, Nm::MIN), |(lo, hi), p| {
                    (lo.min(p.x), hi.max(p.x))
                })
            }
        }
    };

    // For B to the right of A, every pair of points is at least minx(B) - maxx(A) apart.
    // That bound is reached when A attains maxx on the x axis and B attains minx on it.
    // Every convex mirror-symmetric shape does: its extreme face is symmetric and convex,
    // so contains an axis point. The nonconvex arcs also do in the facing orientations above.
    let separations = [0, 1, 149_499, 149_500, 149_999, 150_000, 150_001, 1_000_000];
    let mut cases = Vec::with_capacity(catalogue.len() * catalogue.len() * 7 + 6);
    for (a_name, a) in &catalogue {
        let (_, max_a) = x_extent(a);
        for (b_name, base_b) in &catalogue {
            let b = if matches!(base_b, Shape::Arc { .. }) {
                base_b.mirrored_x(0)
            } else {
                base_b.clone()
            };
            let (min_b, _) = x_extent(&b);
            let touching = max_a - min_b;
            let has_arc = matches!(a, Shape::Arc { .. }) || matches!(&b, Shape::Arc { .. });
            for d in separations {
                cases.push(Case {
                    name: format!("{a_name} -> {b_name}, separation {d} nm"),
                    a: a.clone(),
                    b: b.translated(P::new(touching + d, 0)),
                    expected: d,
                    has_arc,
                });
            }
            cases.push(Case {
                name: format!("{a_name} -> {b_name}, overlap 100000 nm"),
                a: a.clone(),
                b: b.translated(P::new(touching - 100_000, 0)),
                expected: 0,
                has_arc,
            });
        }
    }

    let ring = Shape::Poly(
        PolySet::rect(BBox::new(P::mm(-3.0, -3.0), P::mm(3.0, 3.0)))
            .difference(&PolySet::rect(BBox::new(P::mm(-2.0, -2.0), P::mm(2.0, 2.0)))),
    );
    let hole_circle = Shape::Circle { c: P::ZERO, r: 500_000 };
    for (name, a, b, expected) in [
        ("circle inside poly", Shape::Circle { c: P::ZERO, r: 200_000 }, poly, 0),
        (
            "short seg_h inside rect",
            Shape::Segment { a: P::new(-200_000, 0), b: P::new(200_000, 0), w: 200_000 },
            rect.clone(),
            0,
        ),
        (
            "rect inside larger rect",
            rect,
            Shape::Rect { c: P::ZERO, half: P::mm(2.0, 1.0), rot: Angle::ZERO, corner: 0 },
            0,
        ),
        ("ring vs circle at hole centre", ring.clone(), hole_circle.clone(), 1_500_000),
        (
            "ring vs circle offset in hole",
            ring.clone(),
            hole_circle.translated(P::new(mm(1.2), 0)),
            300_000,
        ),
        (
            "ring vs circle in solid part",
            ring,
            hole_circle.translated(P::new(mm(2.5), 0)),
            0,
        ),
    ] {
        cases.push(Case { name: name.into(), a, b, expected, has_arc: false });
    }

    let board = Board::new(BoardId(0), "t");
    let m = resolve_rules(&board, None);
    let translation = P::new(mm(400.0), mm(-300.0));
    let layer = LayerId::F_CU;
    for Case { name, a, b, expected, has_arc } in cases {
        let gap = clearance(&a, &b);
        if has_arc && expected > 0 {
            // dist.rs bounds arc distance error below 1 nm; zero gaps must still be exactly zero.
            assert!((gap - expected).abs() <= 1, "{name}: arc clearance {gap} within 1 nm of {expected}");
        } else {
            assert_eq!(gap, expected, "{name}: exact clearance");
        }
        assert_eq!(gap, clearance(&b, &a), "{name}: clearance symmetry");
        for g in [expected, expected + 1, 150_000] {
            assert_eq!(collides(&a, &b, g), gap < g, "{name}: collides agrees with clearance < {g}");
        }
        assert_eq!(
            clearance(&a.translated(translation), &b.translated(translation)),
            gap,
            "{name}: translation invariance (+400 mm, -300 mm)"
        );
        assert_eq!(
            clearance(&a.rotated_about(P::ZERO, Angle::QUARTER), &b.rotated_about(P::ZERO, Angle::QUARTER)),
            gap,
            "{name}: exact quarter-turn invariance about the origin"
        );

        let mut a = Copper {
            item: ConnItem::Track { item: ItemId(1), seg: 0 },
            net: Some(NetId(0)),
            class: ClassId(0),
            layer,
            shape: a,
        };
        let mut b = Copper {
            item: ConnItem::Track { item: ItemId(2), seg: 0 },
            net: Some(NetId(1)),
            class: ClassId(0),
            layer,
            shape: b,
        };
        let matrix_gap = m.clearance(a.key(), b.key(), layer);
        assert_eq!(matrix_gap, 150_000, "{name}: standard rule matrix clearance");
        for (nets, a_net, b_net, need_gap) in [
            ("different nets", Some(NetId(0)), Some(NetId(1)), 150_000),
            ("same net", Some(NetId(0)), Some(NetId(0)), 0),
            ("both nets absent", None, None, matrix_gap),
        ] {
            a.net = a_net;
            b.net = b_net;
            assert_eq!(required(&m, &a, &b, layer).gap, need_gap, "{name}, {nets}: required gap");
            let fault = check_pair(&m, &a, &b, layer);
            // DRC allows a gap DRC_EPSILON short of a requirement larger than the allowance.
            let limit = if need_gap > DRC_EPSILON { need_gap - DRC_EPSILON } else { need_gap };
            assert_eq!(fault.is_some(), gap < limit, "{name}, {nets}: DRC agrees with clearance < {limit}");
            if let Some(fault) = fault {
                assert_eq!(fault.gap, gap, "{name}, {nets}: fault reports core clearance");
                assert_eq!(fault.need.gap, 150_000, "{name}, {nets}: fault requires standard clearance");
                assert!(
                    a.shape.bbox().inflate(150_000).contains(fault.at),
                    "{name}, {nets}: fault.at inside A's inflated box, the delta DRC merge rule"
                );
                assert!(
                    b.shape.bbox().inflate(150_000).contains(fault.at),
                    "{name}, {nets}: fault.at inside B's inflated box, the delta DRC merge rule"
                );
            }
        }
    }
}
