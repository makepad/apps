//! Board and component geometry in local millimetres, independent of the renderer.

use fab_elec_core::{Contour, Nm, PolySet, P};
use crate::scene::{HoleKind, MeshRef, SceneBoard, SceneHole};
use fab_materials::part_look::{self, FileMaterial, PartClass, Place};
use fab_materials::Surface;
use makepad_csg_cad::region::{area, clean_loop, oriented, Loop, Region, P2};
use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

/// Position, kind, normal, plating, board UV, thickness fraction, padding.
pub const BOARD_VERTEX_FLOATS: usize = 12;
/// Position, padding, normal, padding: the std140 record a derived vertex POD reads.
pub const BODY_VERTEX_FLOATS: usize = 8;
const SMOOTH_COS: f64 = 0.9396926207859084; // cos(20 degrees)

#[derive(Clone, Debug, Default)]
pub struct BoardMesh {
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
}

/// Position (mm) and normal, with independent vertices at hard edges.
#[derive(Clone, Debug, Default)]
pub struct BodyMesh {
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
}

/// One file primitive and its resolved appearance. The part converter names the material
/// reaching the model's top for an `@led` part exactly "lens"; no other material emits.
pub struct FileBodyPrim {
    pub mesh: BodyMesh,
    pub surface: Surface,
    pub lens: bool,
}

/// Mesh a board in its own XY frame, from bottom Z = 0 to top Z = thickness.
///
/// The faces are the board's material from the nm-grid booleans the board itself uses: the
/// even-odd outline minus every hole the mesh opens ([`opens_in_mesh`]), triangulated by the
/// region CDT, so a mounting hole or slot is a real opening from the first frame at any tile
/// level. Vias and plated round pad drills remain in the faces: the shader cuts them using drill
/// art. An opened hole's barrel is its wall (it shares the cut's points); one the boolean merged
/// with the outline or another hole gets plain walls along the merged edge instead.
///
/// Cost: the boolean and the CDT are O(n log n) in the outline and opened-hole vertices, each
/// barrel O(segments). Run this on the pool when the board's content key changes, never per
/// frame.
pub fn board_mesh(board: &SceneBoard, max_err: Nm) -> BoardMesh {
    let mut mesh = BoardMesh::default();
    let height = nm_to_mm(board.thickness);
    let profiles: Vec<Vec<(P2, P2)>> = board.holes.iter().map(|hole| hole_profile(hole, max_err)).collect();
    let rings: Vec<Vec<P>> = board.outline.iter().map(|c| c.to_ring(max_err)).collect();
    let mut material = PolySet::from_rings_even_odd(&rings);
    // Each opened hole's ring by its first vertex: a normalised ring starts at its lowest, then
    // leftmost vertex whichever way it winds, so an untouched cut reappears with that start.
    let mut cut_rings: HashMap<P, (usize, usize)> = HashMap::new();
    let opened: Vec<usize> = (0..board.holes.len()).filter(|&i| opens_in_mesh(&board.holes[i]) && profiles[i].len() >= 3).collect();
    if !opened.is_empty() {
        let cuts: Vec<PolySet> = opened.iter().map(|&i| PolySet::from_ring_nonzero(profiles[i].iter().map(|(p, _)| nm_point(*p)).collect())).collect();
        for (&i, cut) in opened.iter().zip(&cuts) {
            if let Some(ring) = cut.polygons().first().map(|poly| &poly.outer) {
                cut_rings.insert(ring[0], (i, ring.len()));
            }
        }
        material = material.difference(&PolySet::union_all(&cuts));
    }
    let mut barrel_walls = vec![false; board.holes.len()];
    for poly in material.polygons() {
        let region = Region { outer: ring_mm(&poly.outer), holes: poly.holes.iter().map(|h| ring_mm(h)).collect() };
        let (points, triangles) = region.triangulate();
        let top = (mesh.vertices.len() / BOARD_VERTEX_FLOATS) as u32;
        for p in &points {
            board_vertex(&mut mesh.vertices, [p[0], p[1], height], [0.0, 0.0, 1.0], 0.0, 0.0, 0.0);
        }
        let bottom = (mesh.vertices.len() / BOARD_VERTEX_FLOATS) as u32;
        for p in &points {
            board_vertex(&mut mesh.vertices, [p[0], p[1], 0.0], [0.0, 0.0, -1.0], 1.0, 0.0, 0.0);
        }
        for [a, b, c] in triangles {
            mesh.indices.extend_from_slice(&[top + a, top + b, top + c, bottom + a, bottom + c, bottom + b]);
        }
        // Outer rings wind CCW and holes CW: their right-hand normals both point away from board
        // material, including nested islands.
        for (i, ring) in region.loops().enumerate() {
            let cut = i.checked_sub(1).and_then(|h| cut_rings.get(&poly.holes[h][0]).filter(|(_, n)| *n == poly.holes[h].len()));
            if let Some(&(hole, _)) = cut {
                barrel_walls[hole] = true;
                continue;
            }
            append_walls(ring, height, &mut mesh.indices, |p, n| {
                let t = if height == 0.0 { 0.0 } else { (p[2] / height) as f32 };
                board_vertex(&mut mesh.vertices, p, n, 2.0, 0.0, t)
            });
        }
    }
    for (i, hole) in board.holes.iter().enumerate() {
        if !opens_in_mesh(hole) || barrel_walls[i] {
            append_barrel(&mut mesh, hole, &profiles[i], height);
        }
    }
    mesh
}

/// Whether the face mesh opens `hole` with real walls: board holes (mounting and tooling
/// holes), slots and unplated drills. Vias and plated round pad drills stay in the faces, cut by
/// the shader from the drill art.
pub fn opens_in_mesh(hole: &SceneHole) -> bool {
    hole.kind == HoleKind::Board || hole.slot_to.is_some_and(|end| end != hole.at) || !hole.plated
}

/// Mesh primitive component bodies in their local frame. Script and file meshes
/// are supplied by the evaluator. Caps and sharp wall edges have separate normals.
pub fn body_mesh(mesh: &MeshRef, max_err: Nm) -> Option<BodyMesh> {
    let mut out = BodyMesh::default();
    match mesh {
        MeshRef::Box { min, max } => {
            let lo = min.map(nm_to_mm);
            let hi = max.map(nm_to_mm);
            let corners = [
                [lo[0], lo[1], lo[2]], [hi[0], lo[1], lo[2]],
                [hi[0], hi[1], lo[2]], [lo[0], hi[1], lo[2]],
                [lo[0], lo[1], hi[2]], [hi[0], lo[1], hi[2]],
                [hi[0], hi[1], hi[2]], [lo[0], hi[1], hi[2]],
            ];
            for (face, normal) in [
                ([0, 3, 2, 1], [0.0, 0.0, -1.0]),
                ([4, 5, 6, 7], [0.0, 0.0, 1.0]),
                ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
                ([1, 2, 6, 5], [1.0, 0.0, 0.0]),
                ([2, 3, 7, 6], [0.0, 1.0, 0.0]),
                ([3, 0, 4, 7], [-1.0, 0.0, 0.0]),
            ] {
                let base = (out.vertices.len() / BODY_VERTEX_FLOATS) as u32;
                for i in face {
                    body_vertex(&mut out.vertices, corners[i], normal);
                }
                out.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        MeshRef::Extrude { outline, height } => {
            let ring = oriented(contour_ring(outline, max_err), true);
            if ring.len() < 3 || area(&ring) <= 1e-12 {
                return Some(out);
            }
            let height = nm_to_mm(*height);
            let region = Region { outer: ring, holes: Vec::new() };
            let (points, triangles) = region.triangulate();
            for [a, b, c] in triangles {
                for (face, z, normal) in [
                    ([a, b, c], height, [0.0, 0.0, 1.0]),
                    ([a, c, b], 0.0, [0.0, 0.0, -1.0]),
                ] {
                    for i in face {
                        let p = points[i as usize];
                        out.indices.push(body_vertex(&mut out.vertices, [p[0], p[1], z], normal));
                    }
                }
            }
            append_walls(&region.outer, height, &mut out.indices, |p, n| body_vertex(&mut out.vertices, p, n));
        }
        MeshRef::Script(_) | MeshRef::Mesh(_) | MeshRef::Package(_) => return None,
    }
    Some(out)
}

/// A part's mesh file (`MeshRef::Mesh`, a `.glb`) as body primitives in the part's body frame
/// (mm), each with its resolved Fab surface and lens flag. Read through the process's mesh cache:
/// parsed once per content and shared with the case checks. Runs on the caller's pool, never per
/// frame; once cached, a call costs a stat, the vertex copy, one box and area pass, and a few
/// comparisons per material.
pub fn file_body_meshes(path: &str, class: PartClass) -> Result<Vec<FileBodyPrim>, String> {
    let read = fab_elec_import::cache::MeshCache::shared().get(std::path::Path::new(path)).map_err(|issue| issue.to_string())?;
    let mesh = &read.mesh;
    let mut body_lo = [f32::INFINITY; 3];
    let mut body_hi = [f32::NEG_INFINITY; 3];
    let mut boxes = Vec::with_capacity(mesh.prims.len());
    let mut material_areas = HashMap::<usize, f64>::new();
    let mut total_area = 0.0;
    for prim in &mesh.prims {
        let mut lo = prim.positions.first().copied().unwrap_or([0.0; 3]);
        let mut hi = lo;
        for p in &prim.positions {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        if !prim.positions.is_empty() {
            for i in 0..3 {
                body_lo[i] = body_lo[i].min(lo[i]);
                body_hi[i] = body_hi[i].max(hi[i]);
            }
        }
        boxes.push((lo, hi));
        let mut area = 0.0;
        for tri in prim.indices.chunks_exact(3) {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| prim.positions[i as usize].map(f64::from));
            let ab = [0, 1, 2].map(|i| b[i] - a[i]);
            let ac = [0, 1, 2].map(|i| c[i] - a[i]);
            let cross = [ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0]];
            area += 0.5 * cross.iter().map(|v| v * v).sum::<f64>().sqrt();
        }
        *material_areas.entry(prim.material).or_default() += area;
        total_area += area;
    }
    let default = fab_elec_import::Material::named("default");
    Ok(mesh
        .prims
        .iter()
        .zip(boxes)
        .filter(|(prim, _)| !prim.indices.is_empty())
        .map(|(prim, (lo, hi))| {
            let mut out = BodyMesh::default();
            out.vertices.reserve(prim.positions.len() * BODY_VERTEX_FLOATS);
            for (p, n) in prim.positions.iter().zip(&prim.normals) {
                out.vertices.extend_from_slice(&[p[0], p[1], p[2], 0.0, n[0], n[1], n[2], 0.0]);
            }
            out.indices = prim.indices.clone();
            let m = mesh.materials.get(prim.material).unwrap_or(&default);
            let [r, g, b, a] = m.base_color;
            let area_share = if total_area > 0.0 { (material_areas[&prim.material] / total_area) as f32 } else { 0.0 };
            let material = FileMaterial {
                name: &m.name,
                base: [r, g, b],
                alpha: a,
                metallic: m.metallic,
                roughness: m.roughness,
                place: Place { lo, hi, body_lo, body_hi, area_share },
            };
            FileBodyPrim { mesh: out, surface: part_look::resolve(&material, class).surface, lens: m.name == "lens" }
        })
        .collect())
}

/// Every corner point of a part's mesh file in its body frame (mm): what a fit check takes the
/// convex hull of. Through the shared mesh cache, like [`file_body_meshes`].
pub fn file_body_points(path: &str) -> Result<Vec<[f64; 3]>, String> {
    let read = fab_elec_import::cache::MeshCache::shared().get(std::path::Path::new(path)).map_err(|issue| issue.to_string())?;
    Ok(read.mesh.prims.iter().flat_map(|prim| prim.positions.iter().map(|p| p.map(f64::from))).collect())
}

fn nm_to_mm(v: Nm) -> f64 {
    v as f64 * 1e-6
}

fn point_mm(p: P) -> P2 {
    [nm_to_mm(p.x), nm_to_mm(p.y)]
}

fn nm_point(p: P2) -> P {
    P::new((p[0] * 1e6).round() as Nm, (p[1] * 1e6).round() as Nm)
}

fn ring_mm(ring: &[P]) -> Loop {
    ring.iter().map(|p| point_mm(*p)).collect()
}

fn contour_ring(contour: &Contour, max_err: Nm) -> Loop {
    clean_loop(contour.to_ring(max_err).into_iter().map(point_mm).collect())
}

fn board_vertex(vertices: &mut Vec<f32>, p: [f64; 3], n: [f64; 3], kind: f32, plated: f32, t: f32) -> u32 {
    let index = (vertices.len() / BOARD_VERTEX_FLOATS) as u32;
    vertices.extend_from_slice(&[
        p[0] as f32, p[1] as f32, p[2] as f32, kind,
        n[0] as f32, n[1] as f32, n[2] as f32, plated,
        p[0] as f32, p[1] as f32, t, 0.0,
    ]);
    index
}

fn body_vertex(vertices: &mut Vec<f32>, p: [f64; 3], n: [f64; 3]) -> u32 {
    let index = (vertices.len() / BODY_VERTEX_FLOATS) as u32;
    vertices.extend_from_slice(&[p[0] as f32, p[1] as f32, p[2] as f32, 0.0, n[0] as f32, n[1] as f32, n[2] as f32, 0.0]);
    index
}

/// A loop's right-facing wall. Smooth corners share a bottom/top vertex pair;
/// sharp corners have one pair for each incident edge, preserving flat faces.
fn append_walls(ring: &[P2], height: f64, indices: &mut Vec<u32>, mut vertex: impl FnMut([f64; 3], [f64; 3]) -> u32) {
    let count = ring.len();
    if count < 3 || height == 0.0 {
        return;
    }
    let normals: Vec<P2> = (0..count).map(|i| {
        let a = ring[i];
        let b = ring[(i + 1) % count];
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let length = dx.hypot(dy);
        [dy / length, -dx / length]
    }).collect();
    let mut pairs = Vec::with_capacity(count);
    for (i, p) in ring.iter().enumerate() {
        let before = normals[(i + count - 1) % count];
        let after = normals[i];
        let mut pair = |n: P2| {
            let n = [n[0], n[1], 0.0];
            [vertex([p[0], p[1], 0.0], n), vertex([p[0], p[1], height], n)]
        };
        if before[0] * after[0] + before[1] * after[1] > SMOOTH_COS {
            let n = [before[0] + after[0], before[1] + after[1]];
            let length = n[0].hypot(n[1]);
            let shared = pair([n[0] / length, n[1] / length]);
            pairs.push((shared, shared));
        } else {
            pairs.push((pair(before), pair(after)));
        }
    }
    for i in 0..count {
        let [a0, a1] = pairs[i].1;
        let [b0, b1] = pairs[(i + 1) % count].0;
        indices.extend_from_slice(&[a0, b0, b1, a0, b1, a1]);
    }
}

/// A hole's wall in mm, clockwise (as a cutout winds), each point with its normal toward the
/// hole's axis. The points lie on the nm grid, so the face cut and the barrel share them exactly.
/// Empty for a hole without a diameter.
fn hole_profile(hole: &SceneHole, max_err: Nm) -> Vec<(P2, P2)> {
    if hole.d <= 0 {
        return Vec::new();
    }
    let radius_nm = hole.d as f64 * 0.5;
    let radius = radius_nm * 1e-6;
    // e = r * (1 - cos(pi / n)); the upper bound limits very fine requests.
    let error_ratio = (max_err.max(1) as f64 / radius_nm).min(1.0);
    let segments = ((PI / (1.0 - error_ratio).acos()).ceil() as usize).clamp(12, 64);
    let a = point_mm(hole.at);
    let mut profile = Vec::with_capacity(segments + 3);
    let mut arc_point = |centre: P2, angle: f64| {
        let (sin, cos) = angle.sin_cos();
        let p = nm_point([centre[0] + radius * cos, centre[1] + radius * sin]);
        profile.push((point_mm(p), [-cos, -sin]));
    };
    if let Some(end) = hole.slot_to.filter(|end| *end != hole.at) {
        let b = point_mm(end);
        let angle = (b[1] - a[1]).atan2(b[0] - a[0]);
        let half_segments = (segments + 1) / 2;
        // Clockwise: the far cap, a straight side, the near cap, the other side.
        // Both arc endpoints are retained so each flat side has tangent normals.
        for (centre, start) in [(b, angle + FRAC_PI_2), (a, angle - FRAC_PI_2)] {
            for i in 0..=half_segments {
                arc_point(centre, start - PI * i as f64 / half_segments as f64);
            }
        }
    } else {
        for i in 0..segments {
            arc_point(a, -TAU * i as f64 / segments as f64);
        }
    }
    profile
}

fn append_barrel(mesh: &mut BoardMesh, hole: &SceneHole, profile: &[(P2, P2)], height: f64) {
    if profile.is_empty() || height == 0.0 {
        return;
    }
    let base = (mesh.vertices.len() / BOARD_VERTEX_FLOATS) as u32;
    let plated = if hole.plated { 1.0 } else { 0.0 };
    for (p, n) in profile {
        let normal = [n[0], n[1], 0.0];
        board_vertex(&mut mesh.vertices, [p[0], p[1], 0.0], normal, 3.0, plated, 0.0);
        board_vertex(&mut mesh.vertices, [p[0], p[1], height], normal, 3.0, plated, 1.0);
    }
    for i in 0..profile.len() {
        let a0 = base + 2 * i as u32;
        let b0 = base + 2 * ((i + 1) % profile.len()) as u32;
        mesh.indices.extend_from_slice(&[a0, b0, b0 + 1, a0, b0 + 1, a0 + 1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{mm, BoardId, ItemId, Path, Sel, Xform3};
    use crate::scene::LayerArtSource;

    fn triangle_cross(a: &[f32], b: &[f32], c: &[f32]) -> [f64; 3] {
        let u = [b[0] as f64 - a[0] as f64, b[1] as f64 - a[1] as f64, b[2] as f64 - a[2] as f64];
        let v = [c[0] as f64 - a[0] as f64, c[1] as f64 - a[1] as f64, c[2] as f64 - a[2] as f64];
        [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
    }

    #[test]
    fn file_body_primitives_flag_only_the_named_lens() {
        use fab_elec_import::{cache::MeshCache, glb::write_glb, Material, MeshBuilder, Normals};
        let mut mesh = MeshBuilder::new();
        for (i, name) in ["case", "lens"].into_iter().enumerate() {
            let material = mesh.add_material(Material::named(name));
            let z = i as f32;
            assert!(mesh.polygon(material, &[[0.0, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z]],
                Normals::Crease { domain: 0, angle: 0.0 }), "each material has a triangle");
        }
        let mut mesh = mesh.finish();
        let path = "asset:mech-lens-test/case-and-lens.glb";
        MeshCache::shared().put(path, &write_glb(&mesh)).expect("two-material GLB reads back");
        let prims = file_body_meshes(path, PartClass::Other).expect("file body primitives read");
        assert_eq!(prims.iter().map(|p| p.lens).collect::<Vec<_>>(), [false, true],
            "only the primitive with material named exactly lens is flagged");
        assert!(prims.iter().all(|p| p.mesh.indices.len() == 3), "both primitives retain their geometry");

        mesh.materials[1].name = "Lens".into();
        let path = "asset:mech-lens-case-test/case-and-Lens.glb";
        MeshCache::shared().put(path, &write_glb(&mesh)).expect("capitalised material GLB reads back");
        assert!(file_body_meshes(path, PartClass::Other).expect("capitalised material reads").iter().all(|p| !p.lens),
            "lens material matching is case-sensitive");
    }

    #[test]
    fn board_and_body_geometry() {
        let max_err = mm(0.01);
        let board = SceneBoard {
            unit: BoardId(0),
            name: "test".into(),
            pose: Xform3::IDENTITY,
            outline: vec![
                Contour::rect(P::ZERO, P::mm(40.0, 30.0), mm(2.0)),
                Contour::rect(P::mm(15.0, 12.0), P::mm(25.0, 18.0), 0),
            ],
            thickness: mm(1.6),
            art: LayerArtSource { unit: BoardId(0), hash: Default::default(), stackup: Default::default() },
            holes: vec![
                // A via stays in the faces (the shader cuts it from drill art).
                SceneHole { sel: Sel::Item(BoardId(0), ItemId(0)), kind: HoleKind::Via, at: P::mm(8.0, 8.0), d: mm(1.0), slot_to: None, plated: true },
                // Centre segment 2 mm plus the diameter gives a 1 x 3 mm slot, cut in the mesh.
                SceneHole { sel: Sel::Item(BoardId(0), ItemId(1)), kind: HoleKind::Board, at: P::mm(30.0, 8.0), d: mm(1.0), slot_to: Some(P::mm(32.0, 8.0)), plated: false },
                // A plated 3.2 mm mounting hole, cut in the mesh with its plated barrel as the wall.
                SceneHole { sel: Sel::Item(BoardId(0), ItemId(2)), kind: HoleKind::Board, at: P::mm(8.0, 22.0), d: mm(3.2), slot_to: None, plated: true },
                // Overlapping the cutout's edge: merged into it, plain walls and no barrel.
                SceneHole { sel: Sel::Item(BoardId(0), ItemId(3)), kind: HoleKind::Board, at: P::mm(25.0, 15.0), d: mm(2.0), slot_to: None, plated: false },
            ],
            bodies: Vec::new(),
            pads: Vec::new(),
        };
        let mesh = board_mesh(&board, max_err);
        assert_eq!(mesh.vertices.len() % BOARD_VERTEX_FLOATS, 0, "board vertex stride");
        assert_eq!(mesh.indices.len() % 3, 0, "board triangle stride");
        let vertices: Vec<_> = mesh.vertices.chunks_exact(BOARD_VERTEX_FLOATS).collect();
        assert!(mesh.indices.iter().all(|i| (*i as usize) < vertices.len()), "every board index is in range");
        let mut counts = [0usize; 4];
        let mut cutout_walls = 0;
        let mut round_vertices = 0;
        let mut slot_vertices = 0;
        let mut mount_vertices = 0;
        // Distances from the opened holes' axes: the slot's centre segment, the mounting hole's.
        let slot = |x: f32, y: f32| (x.clamp(30.0, 32.0) - x).hypot(8.0 - y);
        let mount = |x: f32, y: f32| (x - 8.0).hypot(y - 22.0);
        for v in &vertices {
            assert!(v.iter().all(|x| x.is_finite()), "board attributes are finite");
            assert_eq!(&v[8..10], &v[0..2], "board UV is the position in mm");
            assert_eq!(v[11], 0.0, "board vertex padding is zero");
            assert!((v[4] * v[4] + v[5] * v[5] + v[6] * v[6] - 1.0).abs() < 1e-5, "board normals are unit length");
            let kind = v[3] as usize;
            assert!(kind < counts.len(), "board vertex kind is valid");
            counts[kind] += 1;
            if kind <= 1 {
                let normal = if kind == 0 { [0.0, 0.0, 1.0] } else { [0.0, 0.0, -1.0] };
                assert_eq!(&v[4..7], &normal, "face normals point along their Z side");
                assert_eq!(v[2], if kind == 0 { 1.6 } else { 0.0 }, "face Z is in the board frame");
                assert_eq!(v[10], 0.0, "faces have zero thickness fraction");
            } else {
                assert_eq!(v[6], 0.0, "wall and barrel normals are horizontal");
                assert!((v[10] - v[2] / 1.6).abs() < 1e-6, "wall and barrel thickness fraction");
                if kind == 2 {
                    assert!(slot(v[0], v[1]) > 0.5 + 1e-4 && mount(v[0], v[1]) > 1.6 + 1e-4,
                        "an opened hole's barrel is its only wall");
                    let dot = (v[0] - 20.0) * v[4] + (v[1] - 15.0) * v[5];
                    if (15.0..=26.0).contains(&v[0]) && (12.0..=18.0).contains(&v[1]) {
                        cutout_walls += 1;
                        assert!(dot < 0.0, "cutout wall normals point toward the cutout centre");
                    } else {
                        assert!(dot > 0.0, "outer wall normals point away from the board centre");
                    }
                } else {
                    let (axis, radius) = if (v[0] - 8.0).hypot(v[1] - 8.0) < 1.0 {
                        round_vertices += 1;
                        assert_eq!(v[7], 1.0, "the via barrel is plated");
                        ([8.0, 8.0], 0.5)
                    } else if (v[0] - 8.0).hypot(v[1] - 22.0) < 2.0 {
                        mount_vertices += 1;
                        assert_eq!(v[7], 1.0, "the mounting hole barrel is plated");
                        ([8.0, 22.0], 1.6)
                    } else {
                        slot_vertices += 1;
                        assert_eq!(v[7], 0.0, "the slot barrel is unplated");
                        ([v[0].clamp(30.0, 32.0), 8.0], 0.5)
                    };
                    let dx = axis[0] - v[0];
                    let dy = axis[1] - v[1];
                    assert!((dx.hypot(dy) - radius).abs() < 1e-5, "barrel vertices lie at the hole radius");
                    assert!(dx * v[4] + dy * v[5] > radius - 0.01, "barrel normals point at the hole axis");
                    assert!((dx * v[5] - dy * v[4]).abs() < 1e-5, "barrel normals align with the hole axis");
                }
            }
            if kind != 3 {
                assert_eq!(v[7], 0.0, "only barrels carry plating");
            }
        }
        assert!(counts.iter().all(|n| *n > 0), "all four board surface kinds are present");
        assert!(cutout_walls > 0 && cutout_walls < counts[2], "both cutout and outer walls are present");
        assert!((24..=128).contains(&round_vertices), "round barrel has between 12 and 64 segments and is plated");
        assert!(slot_vertices > 0, "unplated slot barrel is present");
        assert!((24..=128).contains(&mount_vertices), "the mounting hole has a barrel");
        // A cut shares the barrel's points: every barrel vertex of an opened hole is a face vertex.
        let face_points: std::collections::HashSet<[u32; 2]> = vertices.iter().filter(|v| v[3] == 0.0).map(|v| [v[0].to_bits(), v[1].to_bits()]).collect();
        assert!(vertices.iter().filter(|v| v[3] == 3.0 && (v[0] - 8.0).hypot(v[1] - 8.0) > 1.0)
            .all(|v| face_points.contains(&[v[0].to_bits(), v[1].to_bits()])), "opened holes' barrels meet the face cut exactly");
        // Nothing of the hole merged with the cutout's edge stands as a barrel of its own.
        assert!(vertices.iter().all(|v| v[3] != 3.0 || (v[0] - 25.0).hypot(v[1] - 15.0) > 1.0 + 1e-4), "a merged hole has no barrel");
        let mut face_areas = [0.0; 2];
        let mut via_covered = false;
        for triangle in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [vertices[triangle[0] as usize], vertices[triangle[1] as usize], vertices[triangle[2] as usize]];
            assert!(a[3] == b[3] && a[3] == c[3], "triangle vertices share a surface kind");
            let cross = triangle_cross(a, b, c);
            let dot: f64 = (0..3).map(|i| cross[i] * (a[4 + i] + b[4 + i] + c[4 + i]) as f64).sum();
            assert!(dot > 0.0, "every board triangle is wound toward its normals");
            if a[3] == 0.0 {
                assert!(cross[2] > 0.0, "top triangles are CCW from +Z");
                face_areas[0] += cross[2] * 0.5;
                let x = (a[0] + b[0] + c[0]) / 3.0;
                let y = (a[1] + b[1] + c[1]) / 3.0;
                assert!(!(x > 15.0 && x < 25.0 && y > 12.0 && y < 18.0), "no top triangle centroid lies in the cutout");
                // The chords cut inside each circle by at most the meshing error (0.01 mm).
                assert!(slot(x, y) > 0.49 && mount(x, y) > 1.59 && (x - 25.0).hypot(y - 15.0) > 0.99, "no top triangle centroid lies in an opened hole");
                // The via stays in the faces: some top triangle covers its centre.
                let side = |p: &[f32], q: &[f32]| (q[0] - p[0]) * (8.0 - p[1]) - (q[1] - p[1]) * (8.0 - p[0]);
                via_covered |= side(a, b) >= 0.0 && side(b, c) >= 0.0 && side(c, a) >= 0.0;
            } else if a[3] == 1.0 {
                assert!(cross[2] < 0.0, "bottom triangles are CCW from -Z");
                face_areas[1] -= cross[2] * 0.5;
            }
        }
        assert!(via_covered, "the via stays in the faces");
        // Outline minus cutout, the slot, the mounting hole and the half disc of the merged hole
        // outside the cutout; chords cut the circles inside by at most the meshing error.
        let opened = (2.0 * 1.0 + PI * 0.25) + PI * 1.6 * 1.6 + PI * 0.5;
        let expected = 40.0 * 30.0 - (4.0 - PI) * 2.0 * 2.0 - 10.0 * 6.0 - opened;
        assert!((face_areas[0] - expected).abs() < 0.3, "top area is the rounded outline minus cutout and opened holes, not the via: {} vs {expected}", face_areas[0]);
        assert!((face_areas[0] - face_areas[1]).abs() < 1e-8, "bottom and top face areas match");

        let box_mesh = body_mesh(&MeshRef::Box { min: [0, 0, 0], max: [mm(3.0), mm(2.0), mm(1.0)] }, max_err).expect("box has a body mesh");
        assert_eq!(box_mesh.vertices.len(), 24 * BODY_VERTEX_FLOATS, "box has 24 vertices");
        assert_eq!(box_mesh.indices.len(), 12 * 3, "box has 12 triangles");
        // Clockwise, concave L: the missing upper-right 2 x 2 square is not filled.
        let outline = Contour::new(Path::new(P::ZERO)
            .line(P::mm(0.0, 3.0)).line(P::mm(1.0, 3.0)).line(P::mm(1.0, 1.0))
            .line(P::mm(3.0, 1.0)).line(P::mm(3.0, 0.0)));
        let extrude = body_mesh(&MeshRef::Extrude { outline, height: mm(2.0) }, max_err).expect("extruded outline has a body mesh");
        for body in [&box_mesh, &extrude] {
            assert_eq!(body.vertices.len() % BODY_VERTEX_FLOATS, 0, "body vertex stride");
            assert_eq!(body.indices.len() % 3, 0, "body triangle stride");
            assert!(body.indices.iter().all(|i| (*i as usize) < body.vertices.len() / BODY_VERTEX_FLOATS), "every body index is in range");
            for triangle in body.indices.chunks_exact(3) {
                let [a, b, c] = [triangle[0], triangle[1], triangle[2]].map(|i| &body.vertices[i as usize * BODY_VERTEX_FLOATS..][..BODY_VERTEX_FLOATS]);
                let cross = triangle_cross(a, b, c);
                let dot: f64 = (0..3).map(|i| cross[i] * (a[4 + i] + b[4 + i] + c[4 + i]) as f64).sum();
                assert!(dot > 0.0, "body triangle winding agrees with outward normals");
            }
        }
        let mut cap_areas = [0.0; 2];
        for triangle in extrude.indices.chunks_exact(3) {
            let [a, b, c] = [triangle[0], triangle[1], triangle[2]].map(|i| &extrude.vertices[i as usize * BODY_VERTEX_FLOATS..][..BODY_VERTEX_FLOATS]);
            if a[6] != 0.0 {
                let cap = if a[6] > 0.0 { 0 } else { 1 };
                assert_eq!(a[2], if cap == 0 { 2.0 } else { 0.0 }, "extrude caps have the correct height");
                cap_areas[cap] += triangle_cross(a, b, c)[2].abs() * 0.5;
                let x = (a[0] + b[0] + c[0]) / 3.0;
                let y = (a[1] + b[1] + c[1]) / 3.0;
                assert!(!(x > 1.0 && y > 1.0), "concave cap does not fill the missing square");
            }
        }
        assert!((cap_areas[0] - 5.0).abs() < 1e-6, "L-shaped top cap area is five square millimetres");
        assert!((cap_areas[1] - cap_areas[0]).abs() < 1e-6, "L-shaped caps have matching area");
        assert!(body_mesh(&MeshRef::Script(String::new()), max_err).is_none(), "script meshes are left to the evaluator");
        assert!(body_mesh(&MeshRef::Mesh(String::new()), max_err).is_none(), "file meshes are left to the evaluator");
    }
}
