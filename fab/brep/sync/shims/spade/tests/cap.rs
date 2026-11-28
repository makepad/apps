use makepad_csg_math::thread_pool::{with_cancel, CancelToken};
use makepad_solidsync_spade as spade;
use spade::handles::{DirectedEdgeHandle, FixedFaceHandle, FixedVertexHandle, InnerTag};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};
use std::collections::HashMap;
use std::ops::Range;

type Cdt = ConstrainedDelaunayTriangulation<Point2<f64>>;

fn key(a: usize, b: usize) -> [usize; 2] {
    [a.min(b), a.max(b)]
}

// Fill by walking face adjacency from the outer face. Crossing a side toggles
// parity exactly when the rings traverse that side an odd number of times.
fn fill(points: &[[f64; 2]], rings: &[Range<usize>]) -> Option<Vec<[usize; 3]>> {
    let mut cdt = Cdt::new();
    let mut original = HashMap::<FixedVertexHandle, usize>::new();
    let mut vertices = Vec::new();
    for (i, &[x, y]) in points.iter().enumerate() {
        let handle = cdt.insert(spade::mitigate_underflow(Point2::new(x, y))).ok()?;
        original.entry(handle).or_insert(i);
        vertices.push(handle);
    }
    let mut multiplicity = HashMap::<[usize; 2], usize>::new();
    for ring in rings {
        let polygon = vertices.get(ring.clone())?;
        if polygon.len() < 3 { return None; }
        for i in 0..polygon.len() {
            let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
            if a == b { continue; }
            *multiplicity.entry(key(a.index(), b.index())).or_default() += 1;
            if !cdt.exists_constraint(a, b) {
                if !cdt.can_add_constraint(a, b) { return None; }
                cdt.add_constraint(a, b);
            }
        }
    }

    let crosses = |edge: DirectedEdgeHandle<'_, _, _, _, _>| {
        let [a, b] = edge.vertices().map(|v| v.fix().index());
        edge.is_constraint_edge() && multiplicity[&key(a, b)] % 2 == 1
    };
    let mut inside = vec![None; cdt.num_all_faces()];
    let mut queue: Vec<(FixedFaceHandle<InnerTag>, bool)> = Vec::new();
    for edge in cdt.convex_hull() {
        assert!(edge.face().as_inner().is_none());
        if let Some(face) = edge.rev().face().as_inner() {
            queue.push((face.fix(), crosses(edge)));
        }
    }
    while let Some((fixed, parity)) = queue.pop() {
        if let Some(previous) = inside[fixed.index()] {
            assert_eq!(previous, parity, "face parity depends on the path");
            continue;
        }
        inside[fixed.index()] = Some(parity);
        for edge in cdt.face(fixed).adjacent_edges() {
            let twin = edge.rev();
            assert!(twin.rev() == edge);
            assert_eq!(twin.is_constraint_edge(), edge.is_constraint_edge());
            if let Some(neighbor) = twin.face().as_inner() {
                queue.push((neighbor.fix(), parity ^ crosses(edge)));
            }
        }
    }
    Some(cdt.inner_faces().filter(|f| inside[f.index()].unwrap()).map(|f| {
        f.vertices().map(|v| original[&v.fix()])
    }).collect())
}

fn twice_area([a, b, c]: [[f64; 2]; 3]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn check_fill(points: &[[f64; 2]], rings: &[Range<usize>], area: f64) -> Vec<[usize; 3]> {
    let faces = fill(points, rings).expect("valid boundary");
    let mut sum = 0.0;
    let mut edges = HashMap::<[usize; 2], usize>::new();
    let first = |i: usize| points.iter().position(|p| *p == points[i]).unwrap();
    for &face in &faces {
        assert!(face[0] <= face[1] && face[0] <= face[2], "face rotation");
        let signed = twice_area(face.map(|v| points[v]));
        assert!(signed > 0.0, "clockwise or degenerate triangle {face:?}");
        sum += signed * 0.5;
        for i in 0..3 {
            assert_eq!(face[i], first(face[i]), "duplicate displaced the first index");
            *edges.entry(key(face[i], face[(i + 1) % 3])).or_default() += 1;
        }
    }
    assert_eq!(sum, area, "filled area");
    assert!(faces.windows(2).all(|p| p[0] < p[1]), "face order");
    let mut runs = HashMap::<[usize; 2], usize>::new();
    for ring in rings {
        for i in ring.clone() {
            let a = first(i);
            let b = first(if i + 1 == ring.end { ring.start } else { i + 1 });
            if a != b {
                *runs.entry(key(a, b)).or_default() += 1;
            }
        }
    }
    for (edge, count) in runs {
        assert_eq!(edges.get(&edge), Some(&count), "filled triangles on the sides of {edge:?}");
    }
    faces
}

#[test]
fn spade_cap_is_correct_or_refuses() {
    let square = [[0., 0.], [8., 0.], [8., 8.], [0., 8.]];
    let outer = [0..4];
    let baseline = check_fill(&square, &outer, 64.);
    assert_eq!(baseline.len(), 2);

    let mut holed = square.to_vec();
    holed.extend([[2., 2.], [5., 2.], [5., 5.], [2., 5.]]);
    for tri in check_fill(&holed, &[0..4, 4..8], 55.) {
        let [a, b, c] = tri.map(|i| holed[i]);
        let x = (a[0] + b[0] + c[0]) / 3.;
        let y = (a[1] + b[1] + c[1]) / 3.;
        assert!(!(2. < x && x < 5. && 2. < y && y < 5.), "triangle in hole");
    }

    let slit = [[0., 0.], [8., 0.], [8., 8.], [3., 6.], [8., 8.], [0., 8.]];
    check_fill(&slit, &[0..6], 64.);
    let mut repeated = square.to_vec();
    repeated.push([-0., -0.]);
    assert_eq!(check_fill(&repeated, &[0..5], 64.), baseline);

    let mut touched = square.to_vec();
    touched.push([3., 0.]);
    assert!(fill(&touched, &outer).is_none(), "side passed through another vertex");
    let crossing = [square[0], square[2], square[1], square[3]];
    assert!(fill(&crossing, &outer).is_none(), "crossing sides");

    let huge = f64::from_bits((1023 + 201) << 52);
    let tiny = f64::from_bits((1023 - 142) << 52);
    for scale in [huge, tiny] {
        check_fill(&[[0., 0.], [scale, 0.], [scale, scale], [0., scale]], &outer, scale * scale);
    }
    let mut coords = Cdt::new();
    for invalid in [f64::MAX, -f64::MAX, f64::NAN, f64::INFINITY, tiny * 0.5] {
        assert!(coords.insert(Point2::new(invalid, 0.)).is_err());
    }
    let origin = coords.insert(Point2::new(-0., 0.)).unwrap();
    let small = spade::mitigate_underflow(Point2::new(tiny * 0.5, -tiny * 0.5));
    assert_eq!(coords.insert(small).unwrap(), origin);

    let cancelled = CancelToken::new();
    cancelled.cancel();
    with_cancel(&cancelled, || {
        assert!(makepad_csg_math::thread_pool::cancelled());
        assert_eq!(check_fill(&square, &outer, 64.), baseline, "pool cancellation changed the cap");
    });
}
