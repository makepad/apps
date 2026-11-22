use super::*;

fn raw_params() -> SliceParams {
    // Exercise contour extraction without offsets or simplification masking defects.
    SliceParams { closing_radius: 0.0, resolution: 0.0, max_gap: 2.0 }
}

fn cube() -> Mesh {
    Mesh {
        verts: vec![
            [0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0], [0.0, 10.0, 0.0],
            [0.0, 0.0, 10.0], [10.0, 0.0, 10.0], [10.0, 10.0, 10.0], [0.0, 10.0, 10.0],
        ],
        tris: vec![
            [0, 2, 1], [0, 3, 2], // bottom
            [4, 5, 6], [4, 6, 7], // top
            [0, 1, 5], [0, 5, 4], // front
            [1, 2, 6], [1, 6, 5], // right
            [2, 3, 7], [2, 7, 6], // back
            [3, 0, 4], [3, 4, 7], // left
        ],
        ..Mesh::default()
    }
}

fn cube_with_side_hole(width: f64) -> Mesh {
    let mut mesh = cube();
    drop(mesh.tris.drain(4..6));
    // Retile just the front wall, then remove the middle square's two triangles.
    let cuts = [0.0, (10.0 - width) * 0.5, (10.0 + width) * 0.5, 10.0];
    let mut missing = 0;
    for (zi, z) in cuts.windows(2).enumerate() {
        for (xi, x) in cuts.windows(2).enumerate() {
            let base = mesh.verts.len() as u32;
            mesh.verts.extend([[x[0], 0.0, z[0]], [x[1], 0.0, z[0]], [x[1], 0.0, z[1]], [x[0], 0.0, z[1]]]);
            if xi == 1 && zi == 1 {
                missing = mesh.tris.len();
            }
            mesh.tris.extend([[base, base + 1, base + 2], [base, base + 2, base + 3]]);
        }
    }
    drop(mesh.tris.drain(missing..missing + 2));
    mesh
}

fn cut(mesh: &Mesh, z: f64) -> Vec<(Pt, Pt)> {
    cut_triangles(mesh, 0..mesh.tris.len() as u32, z, plane_tolerance(mesh))
}

fn segments(ring: &Polygon) -> Vec<(Pt, Pt)> {
    (0..ring.len()).map(|i| (ring.pts[i], ring.pts[(i + 1) % ring.len()])).collect()
}

fn assert_closed(regions: &[ExPolygon]) {
    for region in regions {
        // Polygon rings close implicitly from their last point to their first.
        assert!(region.contour.len() >= 3 && region.contour.area2() > 0, "{region:?}");
        for hole in &region.holes {
            assert!(hole.len() >= 3 && hole.area2() < 0, "{hole:?}");
        }
        for ring in region.rings() {
            assert!(segments(ring).iter().all(|(a, b)| a != b), "zero-length edge: {ring:?}");
        }
    }
}

fn assert_solid(regions: &[ExPolygon], area: f64, tolerance: f64) {
    assert_closed(regions);
    assert_eq!(regions.len(), 1, "{regions:?}");
    assert!(regions[0].holes.is_empty(), "{regions:?}");
    assert!((regions[0].area_mm2() - area).abs() <= tolerance, "area {} vs {area}", regions[0].area_mm2());
}

#[test]
fn overlapping_outward_cubes_form_one_solid_union() {
    // Both crossing skins and coincident stretches of wall must add winding.
    for (shift, area) in [([5.0, 4.0, 0.0], 170.0), ([5.0, 0.0, 0.0], 150.0)] {
        let mut mesh = cube();
        let mut other = cube();
        other.translate(shift);
        mesh.append(&other);
        assert_eq!(cut(&mesh, 5.0).len(), 16);
        let layers = slice_mesh(&mesh, &[5.0], &raw_params());
        assert_solid(&layers[0], area, 1e-6);
    }
}

#[test]
fn missing_side_wall_pairs_close_even_beyond_max_gap() {
    for width in [1.0, 6.0] {
        let mesh = cube_with_side_hole(width);
        // Confirm that this fixture actually has an open cut of the requested width.
        let (loops, open) = chain_segments(&cut(&mesh, 5.0));
        assert!(loops.is_empty());
        assert!(!open.is_empty());
        let starts: Vec<Pt> = open.iter().map(|p| p[0]).collect();
        let ends: Vec<Pt> = open.iter().map(|p| *p.last().unwrap()).collect();
        let start = *starts.iter().find(|p| !ends.contains(p)).unwrap();
        let end = *ends.iter().find(|p| !starts.contains(p)).unwrap();
        assert!((end.dist(start) / SCALE - width).abs() < 1e-6);
        for params in [raw_params(), SliceParams::default()] {
            assert_eq!(params.max_gap, 2.0);
            let layers = slice_mesh(&mesh, &[5.0], &params);
            assert_solid(&layers[0], 100.0, 3.0);
        }
    }
}

#[test]
fn duplicated_side_wall_triangles_do_not_change_the_region() {
    let mut mesh = cube();
    mesh.tris.extend_from_within(4..6);
    // Include orders where the duplicate starts or interrupts the first chain.
    for _ in 0..mesh.tris.len() {
        assert_eq!(cut(&mesh, 5.0).len(), 10);
        let layers = slice_mesh(&mesh, &[5.0], &raw_params());
        assert_solid(&layers[0], 100.0, 1e-6);
        mesh.tris.rotate_left(1);
    }
}

#[test]
fn nearby_open_chains_join_before_chord_closure() {
    let p = Pt::from_mm;
    // A square with two 1 mm gaps and a separate two-point chain on the left.
    let segs = vec![(p(1.0, 0.0), p(10.0, 0.0)), (p(10.0, 0.0), p(10.0, 10.0)), (p(10.0, 10.0), p(0.0, 10.0)), (p(0.0, 9.0), p(0.0, 0.0))];
    assert_solid(&segments_to_regions(segs.clone(), &raw_params()), 100.0, 1e-6);
    // With joining disabled, the larger chain still closes across its 10 mm gap.
    assert_solid(&segments_to_regions(segs, &SliceParams { max_gap: 0.0, ..raw_params() }), 95.0, 1e-6);
}

#[test]
fn repaired_loop_area_cutoff_preserves_both_orientations() {
    for (area, retained) in [(0.0, false), (0.639999, false), (0.64, true), (0.640001, true)] {
        for reverse in [false, true] {
            let mut pts = vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(2.0, 0.0), Pt::from_mm(0.0, area)];
            if reverse {
                pts.reverse();
            }
            let segs = pts.windows(2).map(|w| (w[0], w[1])).collect();
            let regions = segments_to_regions(segs, &SliceParams { max_gap: 0.0, ..raw_params() });
            assert_closed(&regions);
            if retained {
                assert_solid(&regions, area, 1e-9);
            } else {
                assert!(regions.is_empty(), "area {area}: {regions:?}");
            }
        }
    }
    // The repair cutoff must not erase already-closed fine details.
    let ring = Polygon::rect(Pt::from_mm(0.0, 0.0), Pt::from_mm(0.5, 0.5));
    assert_solid(&segments_to_regions(segments(&ring), &raw_params()), 0.25, 1e-9);
}

#[test]
fn nonzero_keeps_same_orientation_skins_and_reversed_holes() {
    let outer = Polygon::rect(Pt::from_mm(0.0, 0.0), Pt::from_mm(10.0, 10.0));
    let mut inner = Polygon::rect(Pt::from_mm(2.0, 2.0), Pt::from_mm(8.0, 8.0));
    let mut segs = segments(&outer);
    segs.extend(segments(&inner));
    assert_solid(&segments_to_regions(segs, &raw_params()), 100.0, 1e-6);

    inner.reverse();
    let mut segs = segments(&outer);
    segs.extend(segments(&inner));
    let regions = segments_to_regions(segs, &raw_params());
    assert_closed(&regions);
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].holes.len(), 1);
    assert!((regions[0].area_mm2() - 64.0).abs() < 1e-6);
    assert!(regions[0].contains(Pt::from_mm(1.0, 1.0)));
    assert!(!regions[0].contains(Pt::from_mm(5.0, 5.0)));
}

#[test]
fn junction_pairings_preserve_every_segment_and_nonzero_union() {
    let p = Pt::from_mm;
    // Two overlapping squares split at their shared wall endpoints. The
    // multiset includes coincident directed edges, not just unique segments.
    let rings = [
        Polygon::new(vec![p(0.0, 0.0), p(5.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(5.0, 10.0), p(0.0, 10.0)]),
        Polygon::new(vec![p(5.0, 0.0), p(10.0, 0.0), p(15.0, 0.0), p(15.0, 10.0), p(10.0, 10.0), p(5.0, 10.0)]),
    ];
    let original: Vec<_> = rings.iter().flat_map(segments).collect();
    let mut expected_edges = original.clone();
    expected_edges.sort();
    let probes = [(p(-1.0, 5.0), 0), (p(2.5, 5.0), 1), (p(7.5, 5.0), 2), (p(12.5, 5.0), 1), (p(16.0, 5.0), 0)];
    let mut pairings = Vec::new();
    for reverse_order in [false, true] {
        let mut segs = original.clone();
        if reverse_order {
            segs.reverse(); // Change traversal order, never edge orientation.
        }
        for _ in 0..segs.len() {
            let (loops, open) = chain_segments(&segs);
            assert!(open.is_empty(), "balanced junctions must close every chain");
            assert!(loops.iter().all(|r| r.len() >= 3));
            let mut actual_edges: Vec<_> = loops.iter().flat_map(segments).collect();
            actual_edges.sort();
            assert_eq!(actual_edges, expected_edges, "each directed segment is used exactly once");
            for &(point, winding) in &probes {
                assert_eq!(loops.iter().map(|r| r.winding(point)).sum::<i32>(), winding);
            }
            let mut lengths: Vec<_> = loops.iter().map(Polygon::len).collect();
            lengths.sort();
            if !pairings.contains(&lengths) {
                pairings.push(lengths);
            }
            assert_solid(&segments_to_regions(segs.clone(), &raw_params()), 150.0, 1e-6);
            segs.rotate_left(1);
        }
    }
    assert!(pairings.len() > 1, "the fixture must exercise different junction pairings");
}

#[test]
fn on_plane_vertices_edges_and_triangles_use_the_above_convention() {
    let p = Pt::from_mm;
    let cases = [
        // Coplanar triangle; isolated vertex touching from below or above.
        ([[0.0, 0.0, 5.0], [4.0, 0.0, 5.0], [0.0, 4.0, 5.0]], None),
        ([[0.0, 0.0, 5.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]], None),
        ([[0.0, 0.0, 5.0], [4.0, 0.0, 10.0], [0.0, 4.0, 10.0]], None),
        // One vertex on the plane, with the other two on opposite sides.
        ([[0.0, 0.0, 5.0], [4.0, 0.0, 0.0], [0.0, 4.0, 10.0]], Some((p(0.0, 0.0), p(2.0, 2.0)))),
        // An edge in the plane belongs only to the triangle below it.
        ([[0.0, 0.0, 5.0], [4.0, 0.0, 5.0], [0.0, 4.0, 0.0]], Some((p(4.0, 0.0), p(0.0, 0.0)))),
        ([[0.0, 0.0, 5.0], [4.0, 0.0, 5.0], [0.0, 4.0, 10.0]], None),
    ];
    for (verts, expected) in cases {
        for reverse in [false, true] {
            let mut tri = [0, 1, 2];
            let expected: Vec<_> = expected.map(|(a, b)| if reverse { (b, a) } else { (a, b) }).into_iter().collect();
            if reverse {
                tri.reverse();
            }
            for _ in 0..3 {
                let mesh = Mesh { verts: verts.to_vec(), tris: vec![tri], ..Mesh::default() };
                assert_eq!(cut(&mesh, 5.0), expected, "{mesh:?}");
                tri.rotate_left(1);
            }
        }
    }
}

#[test]
fn mesh_vertices_at_layer_heights_do_not_lose_or_duplicate_segments() {
    let mut mesh = cube();
    for v in &mut mesh.verts {
        v[2] *= 0.5;
    }
    let mut upper = mesh.clone();
    upper.translate([0.0, 0.0, 5.0]);
    mesh.append(&upper);
    mesh.weld();
    // Two outward half-cubes meet at z=5, including their coplanar lids.
    // The half-open rule cuts (min_z, max_z], so z=0 is intentionally empty.
    let zs = [10.0, 0.0, 5.0, 2.5, 7.5, 5.0];
    let layers = slice_mesh(&mesh, &zs, &raw_params());
    let square = Polygon::rect(Pt::from_mm(0.0, 0.0), Pt::from_mm(10.0, 10.0));
    let mut expected_edges = segments(&square);
    expected_edges.sort();
    for ((&z, regions), count) in zs.iter().zip(&layers).zip([4, 0, 4, 8, 8, 4]) {
        let mut segs = cut(&mesh, z);
        assert_eq!(segs.len(), count, "z={z}");
        segs.sort();
        segs.dedup();
        assert_eq!(segs.len(), count, "duplicate segment at z={z}");
        assert_closed(regions);
        if z == 0.0 {
            assert!(regions.is_empty());
        } else {
            assert_solid(regions, 100.0, 1e-6);
            if z == 5.0 || z == 10.0 {
                assert_eq!(segs, expected_edges, "z={z}");
            }
        }
    }
}

#[test]
fn faces_at_a_plane_with_rounding_noise_cut_like_exact_ones() {
    // Two outward half-cubes meeting at z=5 with their coplanar lids, then the same mesh with
    // every vertex at 5 and 10 moved 1 or 2 ulp up or down, as transforms and booleans leave a
    // face modelled at a layer's height: the noisy faces cut to the exact faces' segments.
    let mut exact = cube();
    for v in &mut exact.verts {
        v[2] *= 0.5;
    }
    let mut upper = exact.clone();
    upper.translate([0.0, 0.0, 5.0]);
    exact.append(&upper);
    exact.weld();
    let mut noisy = exact.clone();
    let mut nudges = [1i64, -1, 2, -2].into_iter().cycle();
    for v in &mut noisy.verts {
        if v[2] == 5.0 || v[2] == 10.0 {
            v[2] = f64::from_bits((v[2].to_bits() as i64 + nudges.next().unwrap()) as u64);
        }
    }
    assert!(noisy.verts.iter().any(|v| v[2] < 5.0 && v[2] > 4.0) && noisy.verts.iter().any(|v| v[2] > 5.0 && v[2] < 6.0), "the noise straddles the plane");
    for z in [5.0, 10.0] {
        let (mut want, mut got) = (cut(&exact, z), cut(&noisy, z));
        want.sort();
        got.sort();
        assert_eq!(got, want, "z={z}: the noisy faces cut to the exact segments");
        let layers = slice_mesh(&noisy, &[z], &raw_params());
        assert_solid(&layers[0], 100.0, 1e-6);
    }
}

#[test]
fn nested_loops_make_regions_by_depth() {
    let mm = |x: f64, y: f64| Pt::from_mm(x, y);
    let square = |a: f64, b: f64, ccw: bool| {
        let mut ring = Polygon::rect(mm(a, a), mm(b, b));
        if ring.is_ccw() != ccw { ring.reverse(); }
        ring
    };
    // An island inside a hole inside a contour, a second contour beside them touching the
    // first at a corner, every ring given in the wrong orientation and out of order.
    let corner = Polygon::rect(mm(10.0, 10.0), mm(14.0, 14.0));
    let loops = vec![square(4.0, 6.0, false), corner, square(2.0, 8.0, true), square(0.0, 10.0, false)];
    let regions = nested_loops_to_regions(loops, 0.0);
    assert_eq!(regions.len(), 3);
    assert!(regions.iter().all(|r| r.contour.is_ccw() && r.holes.iter().all(|h| !h.is_ccw())));
    let areas: Vec<(i128, usize)> = regions.iter().map(|r| (r.contour.area2(), r.holes.len())).collect();
    assert_eq!(areas, vec![(square(0.0, 10.0, true).area2(), 1), (square(10.0, 14.0, true).area2(), 0), (square(4.0, 6.0, true).area2(), 0)]);
}

/// Where loops touch or cross (a sheet tangent to a face), nesting covers what the union
/// covers with simple rings: a loop through one point twice splits there; crossing loops take
/// the union.
#[test]
fn nested_loops_split_pinches_and_union_crossings() {
    let ring = |pts: &[(f64, f64)]| Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect());
    let shape = |regions: &[ExPolygon]| {
        let mut s: Vec<(i128, Vec<i128>)> = regions.iter().map(|r| (r.contour.area2(), r.holes.iter().map(|h| h.area2()).collect())).collect();
        s.sort();
        s
    };
    let mm2 = |a: f64| (a * 2e12) as i128;
    let cases = [
        // two lobes touching at (2, 2), traced as one loop: two contours
        (vec![ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (4.0, 2.0), (4.0, 4.0), (2.0, 4.0), (2.0, 2.0), (0.0, 2.0)])],
            vec![(mm2(4.0), vec![]), (mm2(4.0), vec![])]),
        // a pocket closing at (5, 10) on the outline: a contour and a hole touching it
        (vec![ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (5.0, 10.0), (7.0, 7.0), (3.0, 7.0), (5.0, 10.0), (0.0, 10.0)])],
            vec![(mm2(100.0), vec![-mm2(6.0)])]),
        // two crossing squares: their union
        (vec![ring(&[(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)]), ring(&[(2.0, 1.0), (5.0, 1.0), (5.0, 4.0), (2.0, 4.0)])],
            vec![(mm2(16.0), vec![])]),
    ];
    for (loops, expected) in cases {
        let nested = nested_loops_to_regions(loops.clone(), 0.0);
        let union = loops_to_regions(loops, &SliceParams { closing_radius: 0.0, resolution: 0.0, max_gap: 0.0 });
        assert_eq!(shape(&nested), expected);
        let area = |r: &[ExPolygon]| r.iter().map(|e| e.area_mm2()).sum::<f64>();
        assert!(area(&clip::difference_ex(&nested, &union)) + area(&clip::difference_ex(&union, &nested)) < 1e-9, "nesting covers what the union covers");
    }
}
