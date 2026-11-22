use fab_elec_mech::hull::BodyHull;

const CORNERS: [[f64; 3]; 8] = [
    [0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [2.0, 3.0, 0.0],
    [0.0, 0.0, 4.0], [2.0, 0.0, 4.0], [0.0, 3.0, 4.0], [2.0, 3.0, 4.0],
];

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}

fn rotate([x, y, z]: [f64; 3]) -> [f64; 3] {
    let r = std::f64::consts::FRAC_1_SQRT_2;
    [(x - y) * r, (x + y) * r, z]
}

#[test]
fn a_box_hull_drops_interior_points_and_measures_contact_clip_and_gap() {
    let mut points = CORNERS.to_vec();
    points.extend([
        [0.2, 0.3, 0.4], [0.4, 0.6, 0.8], [0.6, 0.9, 1.2], [0.8, 1.2, 1.6],
        [1.0, 1.5, 2.0], [1.2, 1.8, 2.4], [1.4, 2.1, 2.8], [1.6, 2.4, 3.2],
        [1.8, 2.7, 3.6], [0.2, 2.7, 0.4], [0.4, 2.4, 0.8], [0.6, 2.1, 1.2],
        [0.8, 1.8, 1.6], [1.2, 1.2, 2.4], [1.4, 0.9, 2.8], [1.6, 0.6, 3.2],
        [1.8, 0.3, 3.6], [0.5, 1.5, 3.0], [1.5, 1.0, 1.0], [1.0, 2.0, 0.5],
    ]);
    let hull = BodyHull::new(&points).expect("a box with interior points has a hull");
    assert_eq!(hull.points.len(), 8, "only the eight box corners remain");
    assert!(hull.points.iter().all(|p| CORNERS.contains(p)), "every hull point is an original corner");
    assert_eq!(hull.faces.len(), 12, "the box has twelve outward triangles");
    assert_eq!(hull.planes.len(), 6, "the coplanar triangle pairs merge into six planes");
    assert_eq!(hull.edges.len(), 18, "the box keeps twelve edges and six face diagonals");
    assert!(hull.edges.iter().all(|e| e[0] < e[1]), "edge endpoints are in ascending order");
    assert!(hull.edges.windows(2).all(|e| e[0] < e[1]), "edges are sorted without duplicates");
    assert_eq!(hull.min, [0.0, 0.0, 0.0], "the hull's lower bounds match the box");
    assert_eq!(hull.max, [2.0, 3.0, 4.0], "the hull's upper bounds match the box");
    assert_eq!(hull.centre(), [1.0, 1.5, 2.0], "the centre averages corners without interior-point bias");
    for &(n, d) in &hull.planes {
        assert!((distance(n, [0.0; 3]) - 1.0).abs() < 1e-9, "face plane normals have unit length");
        let centre = hull.centre();
        assert!((0..3).map(|i| n[i] * centre[i]).sum::<f64>() < d, "the centre is strictly inside every plane");
    }
    assert!(hull.contains([1.0, 1.0, 1.0]), "an interior point is contained");
    assert!(hull.contains([2.0, 1.0, 1.0]), "a surface point is contained");
    assert!(!hull.contains([2.01, 1.0, 1.0]), "a point 0.01 mm outside is not contained");
    assert!(hull.contains([2.0 + 0.5e-9, 1.0, 1.0]), "containment allows the fit tolerance");

    let crossing = [[1.0, 1.0, 1.0], [3.0, 1.0, 1.0], [1.0, 2.0, 1.0]];
    let above = [[0.0, 0.0, 5.0], [2.0, 0.0, 5.0], [0.0, 3.0, 5.0]];
    let touching = [[0.25, 0.25, 4.0], [1.75, 0.25, 4.0], [0.25, 2.5, 4.0]];
    let edge_touch = [[2.0, 0.5, 4.0], [2.0, 2.5, 4.0], [3.0, 1.5, 5.0]];
    assert!(hull.overlaps(&crossing), "a triangle crossing a face overlaps");
    assert!(!hull.overlaps(&above), "a triangle 1 mm above the top is separate");
    assert!(hull.overlaps(&touching), "a triangle in the top plane overlaps by touching");
    assert!(hull.overlaps(&edge_touch), "touching only an edge still overlaps");
    assert!(hull.clip(&above).is_empty(), "a separate triangle clips to nothing");
    let near = touching.map(|mut p| { p[2] += 0.5e-9; p });
    assert!(hull.overlaps(&near), "the overlap test allows a gap smaller than the fit tolerance");

    let middle = [[-10.0, -10.0, 2.0], [20.0, -10.0, 2.0], [-10.0, 20.0, 2.0]];
    let polygon = hull.clip(&middle);
    assert!(polygon.len() >= 4, "the middle slice keeps the full rectangular cross-section");
    assert!(polygon.iter().all(|&p| hull.contains(p)), "every clipped point is within the box");
    assert!(polygon.iter().all(|p| (p[2] - 2.0).abs() < 1e-9), "clipping preserves the triangle's plane");
    let area = polygon.iter().enumerate().map(|(i, p)| {
        let q = polygon[(i + 1) % polygon.len()];
        p[0] * q[1] - p[1] * q[0]
    }).sum::<f64>().abs() * 0.5;
    assert!((area - 6.0).abs() < 1e-9, "the middle slice has the box's 6 mm2 area, got {area}");

    let gap = [[0.5, 0.5, 5.5], [1.5, 0.5, 5.5], [0.5, 2.0, 5.5]];
    let (on_hull, on_triangle) = hull.closest(&gap);
    assert!((distance(on_hull, on_triangle) - 1.5).abs() < 1e-9, "the top gap is exactly 1.5 mm");
    assert!((on_hull[2] - 4.0).abs() < 1e-9 && hull.contains(on_hull), "the closest hull point is on its top face");
    assert_eq!(on_triangle, gap[0], "equal distances keep the first triangle vertex");
    assert_eq!(hull.closest(&gap), (on_hull, on_triangle), "the closest pair is deterministic");
}

#[test]
fn rotated_and_tetrahedral_hulls_measure_true_faces_and_degenerate_inputs_keep_the_box() {
    let hull = BodyHull::new(&CORNERS.map(rotate)).expect("rotating a box preserves its volume");
    let wall = [[3.0, 0.5, 1.0], [3.0, 2.5, 1.0], [3.0, 0.5, 3.0]].map(rotate);
    let (on_hull, on_triangle) = hull.closest(&wall);
    assert!(!hull.overlaps(&wall), "the parallel rotated wall is separate");
    assert!((distance(on_hull, on_triangle) - 1.0).abs() < 1e-9, "the rotated face has a 1 mm perpendicular gap");
    let face_coordinate = (on_hull[0] + on_hull[1]) * std::f64::consts::FRAC_1_SQRT_2;
    assert!((face_coordinate - 2.0).abs() < 1e-9, "the nearest point is on the rotated side face");
    let outside = [1.3, 0.1, 2.0];
    assert!((0..3).all(|i| outside[i] >= hull.min[i] && outside[i] <= hull.max[i]), "the probe lies inside the bbox");
    assert!(!hull.contains(outside), "a point near the plan's bbox edge can lie outside the rotated box");
    assert!(hull.contains(hull.centre()), "the rotated hull contains its centre");

    let tetra = BodyHull::new(&[
        [1.0, 1.0, 1.0], [1.0, -1.0, -1.0], [-1.0, 1.0, -1.0], [-1.0, -1.0, 1.0],
    ]).expect("the regular tetrahedron spans a volume");
    assert_eq!(tetra.points.len(), 4, "the tetrahedron has four corners");
    assert_eq!(tetra.faces.len(), 4, "the tetrahedron has four triangles");
    assert_eq!(tetra.planes.len(), 4, "the tetrahedron has four distinct face planes");
    assert_eq!(tetra.edges.len(), 6, "the tetrahedron has six edges");
    assert_eq!(tetra.centre(), [0.0; 3], "the regular tetrahedron is centred at the origin");
    // The face x + y - z = 1 faces a tiny parallel triangle in x + y - z = 3.
    let small = [[1.0, 1.0, -1.0], [1.001, 0.999, -1.0], [1.001, 1.0, -0.999]];
    let (on_hull, on_triangle) = tetra.closest(&small);
    let analytic = 2.0 / 3.0_f64.sqrt();
    assert!(!tetra.overlaps(&small), "the small triangle is outside the tetrahedron");
    assert!((distance(on_hull, on_triangle) - analytic).abs() < 1e-9, "the tetrahedron gap matches the analytic face distance");
    assert!((on_hull[0] + on_hull[1] - on_hull[2] - 1.0).abs() < 1e-9, "the nearest hull point is on that face");
    assert!(tetra.contains(on_hull), "the nearest point lies within the tetrahedron's face");

    assert!(BodyHull::new(&CORNERS[..3]).is_none(), "three points cannot span a volume");
    let coplanar = [
        [0.0, 0.0, 2.0], [1.0, 0.0, 2.0], [2.0, 0.0, 2.0], [3.0, 0.0, 2.0], [4.0, 0.0, 2.0],
        [0.0, 1.0, 2.0], [1.0, 1.0, 2.0], [2.0, 1.0, 2.0], [3.0, 1.0, 2.0], [4.0, 1.0, 2.0],
    ];
    assert!(BodyHull::new(&coplanar).is_none(), "ten coplanar points keep the box fallback");
    let mut nonfinite = CORNERS;
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        nonfinite[0][0] = value;
        assert!(BodyHull::new(&nonfinite).is_none(), "a non-finite coordinate keeps the box fallback: {value}");
    }
    let collinear = [[0.0; 3], [1.0, 1.0, -1.0], [2.0, 2.0, -2.0], [3.0, 3.0, -3.0], [4.0, 4.0, -4.0]];
    assert!(BodyHull::new(&collinear).is_none(), "five collinear points keep the box fallback");
}
