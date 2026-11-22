use super::*;
use std::f64::consts::PI;

#[test]
fn figure_defaults_only_override_different_inherited_values() {
    let plate = [("wall_loops", "3")];
    let process = [("wall_loops", "8"), ("sparse_infill_density", "25.0%")];
    let inherited = |key: &str| {
        plate.iter().chain(process.iter()).find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
    };
    let existing = vec![
        ("wall_loops".into(), "9".into()),
        ("sparse_infill_density".into(), "10%".into()),
        ("support_threshold_angle".into(), "42".into()),
    ];
    let settings = crate::printable_tool::print_settings(&existing, &inherited);
    assert!(!settings.iter().any(|(k, _)| k == "wall_loops" || k == "sparse_infill_density"), "inherited defaults need no override");
    assert!(settings.contains(&existing[2]), "unrelated overrides survive");
    for &(key, value) in crate::img2mesh::PRINT_DEFAULTS {
        if key != "wall_loops" && key != "sparse_infill_density" {
            assert!(settings.contains(&(key.into(), value.into())), "missing default {key}={value}");
        }
    }
    assert_eq!(settings.len(), crate::img2mesh::PRINT_DEFAULTS.len() - 1);
}

fn merge(into: &mut MeshData, other: MeshData) {
    let offset = into.positions.len() as u32;
    into.positions.extend(other.positions);
    into.triangles.extend(other.triangles.into_iter().map(|t| t.map(|v| v + offset)));
}

fn icosphere(radius: f64, subdivisions: usize) -> MeshData {
    let phi = (1.0 + 5.0_f64.sqrt()) * 0.5;
    let mut vertices = vec![
        [-1.0, phi, 0.0], [1.0, phi, 0.0], [-1.0, -phi, 0.0], [1.0, -phi, 0.0],
        [0.0, -1.0, phi], [0.0, 1.0, phi], [0.0, -1.0, -phi], [0.0, 1.0, -phi],
        [phi, 0.0, -1.0], [phi, 0.0, 1.0], [-phi, 0.0, -1.0], [-phi, 0.0, 1.0],
    ];
    for v in &mut vertices { *v = unit(*v); }
    let mut triangles = vec![
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ];
    for _ in 0..subdivisions {
        let mut edges = HashMap::new();
        let mut midpoint = |a: u32, b: u32| {
            *edges.entry((a.min(b), a.max(b))).or_insert_with(|| {
                let p = unit(add(vertices[a as usize], vertices[b as usize]));
                let i = vertices.len() as u32;
                vertices.push(p);
                i
            })
        };
        let mut next = Vec::with_capacity(triangles.len() * 4);
        for [a, b, c] in triangles {
            let (ab, bc, ca) = (midpoint(a, b), midpoint(b, c), midpoint(c, a));
            next.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
        }
        triangles = next;
    }
    MeshData { positions: vertices.into_iter().map(|p| mul(p, radius).map(|x| x as f32)).collect(), triangles }
}

fn cuboid(lo: Point, hi: Point) -> MeshData {
    let positions = (0..8).map(|i| std::array::from_fn(|k| if i & (1 << k) == 0 { lo[k] as f32 } else { hi[k] as f32 })).collect();
    let faces = [[0, 4, 6, 2], [1, 3, 7, 5], [0, 1, 5, 4], [2, 6, 7, 3], [0, 2, 3, 1], [4, 5, 7, 6]];
    let triangles = faces.into_iter().flat_map(|[a, b, c, d]| [[a, b, c], [a, c, d]]).collect();
    MeshData { positions, triangles }
}

fn cup(radius: f64, height: f64, wall: f64) -> MeshData {
    let n = 64u32;
    let mut mesh = MeshData::default();
    for (r, y) in [(radius, 0.0), (radius, height), (radius - wall, height), (radius - wall, wall)] {
        for i in 0..n {
            let a = i as f64 * 2.0 * PI / n as f64;
            mesh.positions.push([(r * a.cos()) as f32, y as f32, (r * a.sin()) as f32]);
        }
    }
    // Traverse the exterior, across the lip, then down the cavity wall.
    for ring in 0..3 {
        for i in 0..n {
            let j = (i + 1) % n;
            let (a, b, c, d) = (ring * n + i, ring * n + j, (ring + 1) * n + j, (ring + 1) * n + i);
            mesh.triangles.extend([[a, d, c], [a, c, b]]);
        }
    }
    let bottom = mesh.positions.len() as u32;
    mesh.positions.extend([[0.0, 0.0, 0.0], [0.0, wall as f32, 0.0]]);
    for i in 0..n {
        let j = (i + 1) % n;
        mesh.triangles.extend([[bottom, i, j], [bottom + 1, 3 * n + j, 3 * n + i]]);
    }
    mesh
}

fn side_cone(radius: f64, height: f64) -> MeshData {
    let n = 64u32;
    let mut mesh = MeshData::default();
    for i in 0..n {
        let a = i as f64 * 2.0 * PI / n as f64;
        mesh.positions.push([0.0, (radius * a.cos()) as f32, (radius * a.sin()) as f32]);
    }
    mesh.positions.extend([[0.0; 3], [height as f32, 0.0, 0.0]]);
    for i in 0..n {
        let j = (i + 1) % n;
        mesh.triangles.extend([[n, j, i], [i, j, n + 1]]);
    }
    mesh
}

fn options(cell: f64) -> Options {
    Options { cell_mm: Some(cell), max_triangles: 400_000, orient: false, flat_base: false, ..Options::default() }
}

fn assert_closed(mesh: &MeshData, shells: usize) {
    let s = stats(mesh);
    assert!(s.triangles > 0);
    assert_eq!((s.open_edges, s.nonmanifold_edges, s.shells), (0, 0, shells), "{s:?}");
    assert!(mesh.positions.iter().flatten().all(|x| x.is_finite()));
}

/// Actual triangle-plane intersections, rather than nearby vertex extrema.
fn section(mesh: &MeshData, height: f64) -> Vec<Point> {
    let mut points = Vec::new();
    for &t in &mesh.triangles {
        let p = triangle(mesh, t);
        for k in 0..3 {
            let (a, b) = (p[k], p[(k + 1) % 3]);
            if (a[1] < height) == (b[1] < height) { continue; }
            let t = (height - a[1]) / (b[1] - a[1]);
            points.push(add(a, mul(sub(b, a), t)));
        }
    }
    points
}

fn width(points: &[Point], axis: usize) -> f64 {
    assert!(!points.is_empty(), "the section must meet the feature");
    points.iter().map(|p| p[axis]).fold(f64::NEG_INFINITY, f64::max)
        - points.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min)
}

#[test]
fn holes_flipped_faces_and_an_inner_skin_become_one_solid_ball() {
    let mut outer = icosphere(20.0, 4);
    let positions = &outer.positions;
    outer.triangles.retain(|t| {
        let centre = unit(t.iter().fold([0.0; 3], |sum, &i| add(sum, positions[i as usize].map(f64::from))));
        !centre.iter().any(|v| v.abs() > 0.985)
    });
    for t in &mut outer.triangles { t.swap(1, 2); }
    let mut inner = icosphere(14.0, 3);
    for t in &mut inner.triangles { t.swap(1, 2); }
    merge(&mut outer, inner);
    let (mesh, report) = make_printable(&outer, &options(0.4)).unwrap();
    assert_closed(&mesh, 1);
    assert_eq!(report.loops_capped, 6, "{report:?}");
    let full_ball = 4.0 / 3.0 * PI * 20.0_f64.powi(3);
    assert!((report.after.volume_mm3 / full_ball - 1.0).abs() < 0.04, "{report:?}");
}

#[test]
fn a_cup_keeps_its_reachable_cavity() {
    let input = cup(10.0, 20.0, 2.0);
    assert_closed(&input, 1);
    let (mesh, report) = make_printable(&input, &options(0.3)).unwrap();
    assert_closed(&mesh, 1);
    assert_eq!(report.loops_capped, 0);
    let solid_cylinder = PI * 100.0 * 20.0;
    assert!(report.after.volume_mm3 < solid_cylinder * 0.65, "{report:?}");
    let section = section(&mesh, 10.0);
    let inner_radius = section.iter().map(|p| p[0].hypot(p[2])).fold(f64::INFINITY, f64::min);
    assert!(inner_radius > 7.0, "cavity radius {inner_radius}");
}

#[test]
fn thin_pin_and_fin_are_thickened_without_growing_the_box() {
    let mut input = cuboid([-10.0, 0.0, -10.0], [10.0, 20.0, 10.0]);
    merge(&mut input, cuboid([-5.15, 20.0, -0.15], [-4.85, 32.0, 0.15]));
    merge(&mut input, cuboid([4.875, 20.0, -4.0], [5.125, 28.0, 4.0]));
    let (mesh, report) = make_printable(&input, &options(0.2)).unwrap();
    assert_closed(&mesh, 1);
    let features = section(&mesh, 25.0);
    let pin: Vec<_> = features.iter().copied().filter(|p| p[0] < 0.0).collect();
    let fin: Vec<_> = features.iter().copied().filter(|p| p[0] > 0.0).collect();
    assert!(width(&pin, 0) >= 0.7 && width(&pin, 2) >= 0.7, "pin section {pin:?}");
    assert!(width(&fin, 0) >= 0.7, "fin section {fin:?}");
    let body = section(&mesh, 10.0);
    assert!((width(&body, 0) - 20.0).abs() < 0.5);
    assert!((width(&body, 2) - 20.0).abs() < 0.5);
    assert!(report.thickened_mm3 > 0.0, "{report:?}");
}

#[test]
fn a_nearby_speck_is_removed_but_another_large_body_is_kept() {
    let mut input = cuboid([0.0; 3], [20.0; 3]);
    merge(&mut input, cuboid([21.0, 8.0, 8.0], [21.4, 8.4, 8.4]));
    merge(&mut input, cuboid([35.0, 0.0, 0.0], [45.0, 10.0, 10.0]));
    let (mesh, report) = make_printable(&input, &options(0.35)).unwrap();
    assert_closed(&mesh, 2);
    assert_eq!(report.before.shells, 3);
    assert_eq!(report.islands_removed, 1, "{report:?}");
}

#[test]
fn a_ball_has_a_real_flat_contact_patch_and_stands_at_zero() {
    let (mesh, report) = make_printable(&icosphere(20.0, 3), &Options { flat_base: true, ..options(0.5) }).unwrap();
    assert_closed(&mesh, 1);
    assert_eq!(bounds(&mesh.positions).0[1], 0.0);
    assert!(contact_area(&mesh) >= 25.0, "{report:?}");
    assert!(report.base_area_mm2 >= 25.0, "{report:?}");
    assert!(report.base_cut_mm > 0.0 && report.base_cut_mm <= report.after.size[1] * 0.042);
}

#[test]
fn a_sideways_cone_is_stood_on_its_base() {
    let input = side_cone(10.0, 30.0);
    assert_closed(&input, 1);
    let (mesh, report) = make_printable(&input, &Options { orient: true, flat_base: true, ..options(0.4) }).unwrap();
    assert_closed(&mesh, 1);
    assert!(report.rotated.is_some(), "{report:?}");
    assert!(report.after.size[1] > 1.3 * report.after.size[0], "{report:?}");
    assert!(report.overhang_after_mm2 < report.overhang_before_mm2 * 0.75, "{report:?}");
}

#[test]
fn a_hundred_millimetre_sphere_respects_the_triangle_budget() {
    let options = Options { max_triangles: 50_000, orient: false, flat_base: false, ..Options::default() };
    let (mesh, report) = make_printable(&icosphere(50.0, 3), &options).unwrap();
    assert_closed(&mesh, 1);
    assert!(mesh.triangles.len() <= options.max_triangles, "{report:?}");
}

#[test]
fn distance_transform_is_exact_in_all_three_axes() {
    let grid = Grid { origin: [0.0; 3], n: [7, 5, 6], h: 0.3 };
    let points = [[1, 0, 4], [5, 3, 1], [0, 4, 5]];
    let mut seeds = vec![0; grid.len()];
    for p in points { seeds[grid.index(p)] = 1; }
    for (i, d2) in edt(grid, &seeds, 1).into_iter().enumerate() {
        let p = grid.coords(i);
        let exact = points.iter().map(|q| (0..3).map(|k| (p[k] as i32 - q[k] as i32).pow(2)).sum::<i32>()).min().unwrap();
        assert_eq!(d2, exact as f32, "at {p:?}");
    }
    assert!(edt(grid, &vec![0; grid.len()], 1).iter().all(|v| v.is_infinite()));
}

#[test]
fn components_join_diagonals_and_remove_only_small_islands() {
    let grid = Grid { origin: [0.0; 3], n: [20, 10, 10], h: 1.0 };
    let mut solid = vec![0; grid.len()];
    for z in 0..5 { for y in 0..5 { for x in 0..5 { solid[grid.index([x, y, z])] = 1; } } }
    for z in 0..4 { for y in 0..4 { for x in 10..14 { solid[grid.index([x, y, z])] = 1; } } }
    // These two cells touch only across a corner and are over 1% together.
    for p in [[7, 7, 7], [8, 8, 8], [19, 9, 9]] { solid[grid.index(p)] = 1; }
    let (removed, largest) = remove_islands(grid, &mut solid);
    assert_eq!((removed, largest), (1, 125.0));
    assert_eq!(solid[grid.index([19, 9, 9])], 0);
    assert_eq!(solid[grid.index([7, 7, 7])], 1);
    assert_eq!(solid[grid.index([8, 8, 8])], 1);
    let (_, _, sizes) = components(grid, &solid);
    assert_eq!(sizes.into_iter().filter(|&n| n > 0).count(), 3);
}

#[test]
fn requested_height_and_mesh_are_deterministic() {
    let input = cuboid([-2.0, 0.0, -2.0], [2.0, 4.0, 2.0]);
    let options = Options { height_mm: Some(12.0), flat_base: true, ..options(0.4) };
    let (first, report) = make_printable(&input, &options).unwrap();
    let (second, _) = make_printable(&input, &options).unwrap();
    assert_eq!(first, second);
    assert!((report.after.size[1] - 12.0).abs() < 1e-5);
    let (lo, hi) = bounds(&first.positions);
    assert_eq!(lo[1], 0.0);
    assert!((lo[0] + hi[0]).abs() < 1e-5 && (lo[2] + hi[2]).abs() < 1e-5);
    assert_closed(&first, 1);
}

// A single UV skin of the union, not four overlapping closed primitives. The
// body and head contain the ray origin; the legs splay along rays from it and
// their top caps lie in the body. Each ray therefore exits one solid interval.
const FIGURE_ORIGIN: Point = [0.0, 40.0, 0.0];
const FIGURE_HEAD: Point = [0.0, 49.4, 0.0];

fn ellipsoid_exit(origin: Point, dir: Point, centre: Point, radii: Point) -> f64 {
    let p = std::array::from_fn(|i| (origin[i] - centre[i]) / radii[i]);
    let d = std::array::from_fn(|i| dir[i] / radii[i]);
    let (a, b, c) = (dot(d, d), dot(p, d), dot(p, p) - 1.0);
    let disc = b * b - a * c;
    if disc < 0.0 { 0.0 } else { ((-b + disc.sqrt()) / a).max(0.0) }
}

fn figure_surface(dir: Point, legs: bool) -> Point {
    let o = FIGURE_ORIGIN;
    let mut t = ellipsoid_exit(o, dir, [0.0, 28.0, 0.0], [12.0, 18.0, 9.0])
        .max(ellipsoid_exit(o, dir, FIGURE_HEAD, [10.0; 3]));
    if legs {
        for x in [-5.0, 5.0] {
            // Radius-4 legs splay from (+/-2.5, 22) to (+/-5, 4). Making
            // their axes radial avoids hidden inner leg faces at the crotch.
            let (bottom, top) = ([x, 4.0, 0.0], [x * 0.5, 22.0, 0.0]);
            for centre in [bottom, top] {
                t = t.max(ellipsoid_exit(o, dir, centre, [4.0; 3]));
            }
            let axis = unit(sub(top, bottom));
            let p = sub(o, bottom);
            let radial_p = sub(p, mul(axis, dot(p, axis)));
            let radial_d = sub(dir, mul(axis, dot(dir, axis)));
            let (a, b) = (dot(radial_d, radial_d), dot(radial_p, radial_d));
            let disc = b * b - a * (dot(radial_p, radial_p) - 16.0);
            if a > 0.0 && disc >= 0.0 {
                let far = (-b + disc.sqrt()) / a;
                let along = dot(add(p, mul(dir, far)), axis);
                if (0.0..=length(sub(top, bottom))).contains(&along) { t = t.max(far); }
            }
        }
    }
    add(o, mul(dir, t)).map(|x| if x.abs() < 1e-10 { 0.0 } else { x })
}

fn uv_skin(latitudes: usize, longitudes: usize, surface: impl Fn(Point) -> Point) -> MeshData {
    let mut mesh = MeshData::default();
    mesh.positions.push(surface([0.0, 1.0, 0.0]).map(|v| v as f32));
    let mut angles: Vec<_> = (1..latitudes).map(|i| PI * i as f64 / latitudes as f64).collect();
    // Sample both feet's lowest point exactly, so the fixture stands at Y=0.
    angles.push(PI - (5.0_f64 / 40.0).atan());
    angles.sort_by(f64::total_cmp);
    for &theta in &angles {
        for j in 0..longitudes {
            let phi = 2.0 * PI * j as f64 / longitudes as f64;
            mesh.positions.push(surface([theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin()]).map(|v| v as f32));
        }
    }
    let bottom = mesh.positions.len() as u32;
    mesh.positions.push(surface([0.0, -1.0, 0.0]).map(|v| v as f32));
    let index = |row: usize, col: usize| (1 + row * longitudes + col % longitudes) as u32;
    for j in 0..longitudes {
        mesh.triangles.push([0, index(0, j + 1), index(0, j)]);
        for row in 0..angles.len() - 1 {
            let (a, b, c, d) = (index(row, j), index(row, j + 1), index(row + 1, j + 1), index(row + 1, j));
            mesh.triangles.extend([[a, b, c], [a, c, d]]);
        }
        mesh.triangles.push([bottom, index(angles.len() - 1, j), index(angles.len() - 1, j + 1)]);
    }
    mesh
}

fn clean_figure() -> MeshData {
    // 20,480 triangles, with shared indices only within this skin. No weld or
    // repair is used to manufacture the clean reference or the damaged input.
    uv_skin(80, 128, |d| figure_surface(d, true))
}

fn round_holes(mesh: &mut MeshData, holes: &[(Point, f64)]) {
    let positions = &mesh.positions;
    mesh.triangles.retain(|t| {
        let centre = mul(t.iter().fold([0.0; 3], |p, &i| add(p, positions[i as usize].map(f64::from))), 1.0 / 3.0);
        !holes.iter().any(|&(p, radius)| length(sub(centre, p)) < radius)
    });
}

fn figure_hash(mut x: u32) -> u32 {
    x = (x ^ (x >> 16)).wrapping_mul(0x7feb_352d);
    x = (x ^ (x >> 15)).wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

fn bad_figure() -> MeshData {
    let mut outer = clean_figure();
    let mut holes = Vec::new();
    for i in 0..14 {
        let (y, phi) = if i < 6 {
            (49.4 + if i % 2 == 0 { 0.6 } else { 4.0 }, 2.0 * PI * i as f64 / 6.0)
        } else {
            (24.0 + 5.0 * ((i - 6) / 4) as f64, 2.0 * PI * ((i - 6) % 4) as f64 / 4.0 + 0.3)
        };
        let p = if i < 6 {
            let r = (100.0_f64 - (y - FIGURE_HEAD[1]).powi(2)).sqrt();
            [r * phi.cos(), y, r * phi.sin()]
        } else {
            let r = (1.0 - ((y - 28.0) / 18.0).powi(2)).sqrt();
            [12.0 * r * phi.cos(), y, 9.0 * r * phi.sin()]
        };
        holes.push((p, 0.75 + 1.25 * i as f64 / 13.0));
    }
    // Engrave along the undamaged skin's area-weighted inward normal. Hashes
    // choose about 20% of vertices independently, with depths in [0, 0.6] mm.
    let mut normals = vec![[0.0; 3]; outer.positions.len()];
    for &t in &outer.triangles {
        let n = normal(triangle(&outer, t));
        for i in t { normals[i as usize] = add(normals[i as usize], n); }
    }
    round_holes(&mut outer, &holes);
    for (i, p) in outer.positions.iter_mut().enumerate() {
        let h = figure_hash(i as u32);
        // Keep the exact foot contacts; all other vertices use the same hash.
        if h % 5 == 0 && p[1] > 1e-6 {
            let depth = 0.6 * figure_hash(h ^ 0xa511_e9b3) as f64 / u32::MAX as f64;
            *p = sub(p.map(f64::from), mul(unit(normals[i]), depth)).map(|v| v as f32);
        }
    }
    let shrink = |p| add(FIGURE_ORIGIN, mul(sub(p, FIGURE_ORIGIN), 0.8));
    let mut inner = uv_skin(48, 96, |d| shrink(figure_surface(d, false)));
    let inner_holes: Vec<_> = [0, 3, 7, 10].map(|i| (shrink(holes[i].0), 1.2)).into();
    round_holes(&mut inner, &inner_holes);
    for t in &mut inner.triangles { t.swap(1, 2); }
    merge(&mut outer, inner);
    // The extra head is 6% larger and shifted 2 mm sideways. Its crown is
    // exactly Y=60; its winding is flipped after making an outward UV sphere.
    let mut overlapping = uv_skin(48, 96, |d| add(add(FIGURE_HEAD, [2.0, 0.0, 0.0]), mul(d, 10.6)));
    for t in &mut overlapping.triangles { t.swap(1, 2); }
    merge(&mut outer, overlapping);
    for (centre, radius) in [([17.0, 25.0, 0.0], 0.35), ([-17.0, 34.0, 2.0], 0.45), ([0.0, 44.0, 15.0], 0.3)] {
        let mut speck = icosphere(radius, 1);
        for p in &mut speck.positions { *p = add(p.map(f64::from), centre).map(|v| v as f32); }
        merge(&mut outer, speck);
    }
    outer
}

#[test]
fn bad_figure_repairs_to_a_closed_solid_of_the_clean_union_volume() {
    let clean = clean_figure();
    assert_closed(&clean, 1);
    let reference = stats(&clean).volume_mm3;
    let bad = bad_figure();
    assert!(stats(&bad).open_edges > 0, "the fixture must actually contain holes");
    let (lo, hi) = bounds(&bad.positions);
    assert!(lo[1].abs() < 1e-5 && (hi[1] - 60.0).abs() < 1e-5);
    let (repaired, report) = make_printable(&bad, &Options::default()).expect("repair the damaged figure");
    assert_closed(&repaired, 1);
    let volume = stats(&repaired).volume_mm3;
    // The clean outward skin estimates the UNION of body, head and capsules,
    // so overlapping balls are not double counted and inner skins cannot cancel it.
    assert!((volume / reference - 1.0).abs() <= 0.15, "clean union {reference:.3} mm3, repaired {volume:.3} mm3: {report:#?}");
}

use makepad_fab_toolpath::{ExtrusionPath, ObjectInput, ObjectLayer};

fn layer_grab_input(fitted: &crate::img2mesh::Fitted) -> (ObjectInput, makepad_fab_settings::machine::Job) {
    use crate::{img2mesh, Session};
    use makepad_fab_settings::profiles::{JobSetup, Preset};
    let mut session = Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::path::PathBuf::new());
    session.rebuild().expect("start a session");
    let from = img2mesh::Provenance { image: "figure.png".into(), model: img2mesh::MODEL.into() };
    let reply = session.import_mesh("figure", fitted, &from, None, vec![]);
    assert!(reply.ok, "{}", reply.text);
    let evaluated = session.last.as_ref().expect("import evaluated the plate");
    assert_eq!(evaluated.plate.parts.len(), 1);
    let part = &evaluated.plate.parts[0];
    for &(k, v) in img2mesh::PRINT_DEFAULTS {
        let inherited = session.model.project().settings.iter().any(|(key, value)| key == k && fab_model::project::same_setting(value, v));
        assert!(part.print.iter().any(|(key, value)| key == k && fab_model::project::same_setting(value, v)) || inherited, "missing import default {k}={v}");
    }
    // Same mesh conversion as slice::plate::part_mesh: the evaluated part is
    // already in printer millimetres with bed-relative Z. MeshSource cuts it
    // through SliceIndex::cut with SliceParams::default().
    let mut mesh = makepad_fab_slicer::mesh::Mesh {
        verts: part.mesh.positions.clone(), tris: part.mesh.triangles.clone(), paint: Vec::new(),
    };
    mesh.fix_orientation();
    let source = makepad_fab_slicer::source::MeshSource::new(mesh);
    let mut machine = Preset::default();
    machine.values.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
    let mut process = Preset::default();
    for (k, v) in &evaluated.plate.settings { process.values.set_str(k, v); }
    for key in ["layer_height", "initial_layer_print_height"] { process.values.set_str(key, "0.2"); }
    let setup = JobSetup {
        machine, process, filaments: vec![Preset::default()], filament_map: vec![1],
        nozzle_volume_type: vec!["Standard".into(), "Standard".into()], bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(), filament_colours: vec![None],
    };
    let input = ObjectInput { name: part.name.clone(), source: Box::new(source), filament: part.filament as usize - 1, overrides: part.print.clone() };
    let job = makepad_fab_slice::prepare_job(&setup).expect("prepare the slicer job");
    makepad_fab_slice::check_inputs(&job, std::slice::from_ref(&input)).expect("valid imported part settings");
    (input, job)
}

/// The walls and fills the live app's layer planner makes for layer `number` (0.2 mm layers):
/// (regions, wall length mm, infill length mm). Its neighbour context for walls, solid shells
/// and gyroid is included; support-only layers are not.
fn planned_layer(number: usize, input: &ObjectInput, job: &makepad_fab_settings::machine::Job) -> (usize, f64, f64) {
    let layers = makepad_fab_toolpath::plan_window(Default::default(), 0, input, job, &makepad_fab_toolpath::plan_heights(input, job), number - 1, number).expect("plan the requested layer");
    let mut layer = ObjectLayer::default();
    for (z, h, planned) in layers {
        assert!((z - number as f64 * 0.2).abs() < 1e-6 && (h - 0.2).abs() < 1e-6);
        assert!(!planned.support_only);
        Arc::make_mut(&mut layer.slices).extend(Arc::unwrap_or_clone(planned.slices));
        layer.islands.extend(planned.islands);
    }
    let length = |paths: &[ExtrusionPath]| -> f64 {
        paths.iter().flat_map(|path| path.pts.windows(2)).map(|e| (e[1].x_mm() - e[0].x_mm()).hypot(e[1].y_mm() - e[0].y_mm())).sum()
    };
    let walls = layer.islands.iter().map(|i| length(&i.walls)).sum();
    let infill = layer.islands.iter().map(|i| length(&i.fills)).sum();
    (layer.slices.len(), walls, infill)
}

/// A generated figure, made printable and fitted as the image-to-mesh import does, slices into
/// walls and infill on the layers the damaged one is cut at. `FAB_PRINTABLE_MESH` (a `.mesh`
/// asset) replaces the synthetic figure when set.
#[test]
fn a_repaired_figure_slices_into_walls_and_infill() {
    use crate::img2mesh;
    let mesh = match std::env::var_os("FAB_PRINTABLE_MESH") {
        Some(path) => fab_model::project::mesh_of_bytes(&std::fs::read(path).expect("read FAB_PRINTABLE_MESH")).expect("decode the .mesh asset"),
        None => bad_figure(),
    };
    let before = img2mesh::fit(&mesh, 60.0).expect("fit the input to 60 mm");
    let (repaired, report) = make_printable(&before.mesh, &Options::default()).expect("make the fitted mesh printable");
    let after = img2mesh::restand(repaired, 60.0).expect("restand as image_to_mesh does");
    assert_eq!(after.open_edges, 0, "{report:#?}");
    let (input, job) = layer_grab_input(&after);
    for number in [200, 250] {
        let (regions, walls, infill) = planned_layer(number, &input, &job);
        assert!(regions > 0 && walls > 0.0 && infill > 0.0, "layer {number}: {regions} regions, walls {walls:.1} mm, infill {infill:.1} mm: {report:#?}");
    }
}
