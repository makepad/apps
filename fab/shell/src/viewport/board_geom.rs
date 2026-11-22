//! Board and component geometry in local millimetres, independent of the renderer.

pub use fab_elec_mech::mesh::{board_mesh, body_mesh, BoardMesh, BodyMesh, BOARD_VERTEX_FLOATS, BODY_VERTEX_FLOATS};
use fab_elec_core::Nm;
use fab_elec_view::board::tiles::TileId;

fn nm_to_mm(v: Nm) -> f64 {
    v as f64 * 1e-6
}

/// Clip one side's faces to the tile rectangle, grown by `grow_mm` on all sides.
/// Winding and constant face attributes survive clipping. Coordinates round to
/// the nearest f32, never toward the interior: a one-texel growth overlaps mixed
/// levels and samples their aprons instead of opening cracks at tile boundaries.
/// Built once per cached tile on the UI thread; work is O(face triangles).
pub fn face_tile(mesh: &BoardMesh, tile: TileId, bottom: bool, grow_mm: f64) -> BoardMesh {
    let kind = if bottom { 1.0 } else { 0.0 };
    let rect = tile.rect_nm();
    let min = [nm_to_mm(rect.min.x) - grow_mm, nm_to_mm(rect.min.y) - grow_mm];
    let max = [nm_to_mm(rect.max.x) + grow_mm, nm_to_mm(rect.max.y) + grow_mm];
    let mut out = BoardMesh::default();
    let mut lower = Vec::with_capacity(8);
    let mut strip = Vec::with_capacity(8);
    let mut left = Vec::with_capacity(8);
    let mut clipped = Vec::with_capacity(8);
    for triangle in mesh.indices.chunks_exact(3) {
        let source = |index: u32| {
            &mesh.vertices[index as usize * BOARD_VERTEX_FLOATS..][..BOARD_VERTEX_FLOATS]
        };
        let first = source(triangle[0]);
        if first[3] != kind {
            continue;
        }
        let points = [triangle[0], triangle[1], triangle[2]].map(|i| {
            let v = source(i);
            [v[0] as f64, v[1] as f64, v[2] as f64]
        });
        let winding = face_cross(points[0], points[1], points[2]);
        if winding == 0.0 || (0..2).any(|axis| {
            points.iter().all(|p| p[axis] < min[axis])
                || points.iter().all(|p| p[axis] > max[axis])
        }) {
            continue;
        }
        clip_face_edge(&points, &mut lower, 1, min[1], true);
        clip_face_edge(&lower, &mut strip, 1, max[1], false);
        clip_face_edge(&strip, &mut left, 0, min[0], true);
        clip_face_edge(&left, &mut clipped, 0, max[0], false);
        for i in 1..clipped.len().saturating_sub(1) {
            // Check the actual stored coordinates: conversion must not introduce
            // degenerate triangles or reverse winding at nearly coincident edges.
            let fan = [clipped[0], clipped[i], clipped[i + 1]]
                .map(|p| p.map(|v| v as f32 as f64));
            if face_cross(fan[0], fan[1], fan[2]) * winding <= 0.0 {
                continue;
            }
            for p in fan {
                let mut vertex = [0.0; BOARD_VERTEX_FLOATS];
                vertex.copy_from_slice(first);
                vertex[..3].copy_from_slice(&p.map(|v| v as f32));
                vertex[8] = vertex[0];
                vertex[9] = vertex[1];
                out.indices.push((out.vertices.len() / BOARD_VERTEX_FLOATS) as u32);
                out.vertices.extend_from_slice(&vertex);
            }
        }
    }
    out
}

fn face_cross(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Sutherland-Hodgman clipping against one axis-aligned half-plane.
fn clip_face_edge(input: &[[f64; 3]], output: &mut Vec<[f64; 3]>, axis: usize, edge: f64, above: bool) {
    output.clear();
    let Some(&last) = input.last() else { return };
    let inside = |p: [f64; 3]| if above { p[axis] >= edge } else { p[axis] <= edge };
    let mut previous = last;
    let mut previous_inside = inside(previous);
    for &current in input {
        let current_inside = inside(current);
        if current_inside != previous_inside {
            let t = (edge - previous[axis]) / (current[axis] - previous[axis]);
            let mut intersection = std::array::from_fn(|i| previous[i] + t * (current[i] - previous[i]));
            intersection[axis] = edge;
            output.push(intersection);
        }
        if current_inside { output.push(current); }
        previous = current;
        previous_inside = current_inside;
    }
}

#[cfg(test)]
mod face_tile_tests {
    use super::*;
    use fab_elec_core::{mm, BoardId, Contour, ItemId, P, Sel, Xform3};
    use fab_elec_mech::scene::{HoleKind, LayerArtSource, SceneBoard, SceneHole};

    fn vertex(mesh: &BoardMesh, index: u32) -> &[f32] {
        &mesh.vertices[index as usize * BOARD_VERTEX_FLOATS..][..BOARD_VERTEX_FLOATS]
    }

    fn cross(mesh: &BoardMesh, triangle: &[u32]) -> f64 {
        let [a, b, c] = [triangle[0], triangle[1], triangle[2]].map(|i| {
            let v = vertex(mesh, i);
            [v[0] as f64, v[1] as f64, v[2] as f64]
        });
        face_cross(a, b, c)
    }

    #[test]
    fn tiled_faces_preserve_area_winding_and_growth() {
        // The geometry test's rounded board, cutout, round drill and slotted drill.
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
                SceneHole { sel: Sel::Item(BoardId(0), ItemId(0)), kind: HoleKind::Via, at: P::mm(8.0, 8.0), d: mm(1.0), slot_to: None, plated: true },
                SceneHole { sel: Sel::Item(BoardId(0), ItemId(1)), kind: HoleKind::Board, at: P::mm(30.0, 8.0), d: mm(1.0), slot_to: Some(P::mm(32.0, 8.0)), plated: false },
            ],
            bodies: Vec::new(),
            pads: Vec::new(),
        };
        let mesh = board_mesh(&board, mm(0.01));
        let mut original_area = [0.0; 2];
        for triangle in mesh.indices.chunks_exact(3) {
            let kind = vertex(&mesh, triangle[0])[3] as usize;
            if kind < 2 {
                original_area[kind] += cross(&mesh, triangle).abs() * 0.5;
            }
        }
        let bounds = fab_elec_core::BBox::new(P::ZERO, P::mm(40.0, 30.0));
        for lod in [4, 6] {
            for grow in [0.0, TileId::span(lod) as f64 * 1e-6 / 256.0] {
                let mut tiled_area = [0.0; 2];
                let mut crosses_nominal_edge = false;
                for bottom in [false, true] {
                    for tile in TileId::covering(lod, bounds) {
                        let face = face_tile(&mesh, tile, bottom, grow);
                        let rect = tile.rect_nm();
                        let min = [nm_to_mm(rect.min.x), nm_to_mm(rect.min.y)];
                        let max = [nm_to_mm(rect.max.x), nm_to_mm(rect.max.y)];
                        for v in face.vertices.chunks_exact(BOARD_VERTEX_FLOATS) {
                            for axis in 0..2 {
                                // Nearest-f32 rounding is allowed on either side of a boundary.
                                let tolerance = 2.0 * f32::EPSILON as f64
                                    * (min[axis].abs().max(max[axis].abs()) + grow).max(1.0);
                                let p = v[axis] as f64;
                                assert!(p >= min[axis] - grow - tolerance && p <= max[axis] + grow + tolerance,
                                    "vertex is inside the grown rectangle at lod {lod}, axis {axis}");
                                crosses_nominal_edge |= p < min[axis] - tolerance || p > max[axis] + tolerance;
                            }
                            assert_eq!(&v[8..10], &v[..2], "clipped UV equals board position");
                            assert_eq!(v[3], if bottom { 1.0 } else { 0.0 }, "face kind matches tile side");
                            assert_eq!(&v[4..8], &[0.0, 0.0, if bottom { -1.0 } else { 1.0 }, 0.0], "normal and plating survive clipping");
                            assert_eq!(&v[10..12], &[0.0, 0.0], "thickness fraction and padding survive clipping");
                        }
                        for triangle in face.indices.chunks_exact(3) {
                            let cross = cross(&face, triangle);
                            assert!(if bottom { cross < 0.0 } else { cross > 0.0 }, "winding is kept at lod {lod}, grow {grow}");
                            tiled_area[usize::from(bottom)] += cross.abs() * 0.5;
                        }
                    }
                }
                for side in 0..2 {
                    if grow == 0.0 {
                        assert!((tiled_area[side] - original_area[side]).abs() <= original_area[side] * 1e-6,
                            "tile area is conserved at lod {lod}, side {side}");
                    } else {
                        assert!(tiled_area[side] > original_area[side], "grown neighbours overlap at lod {lod}, side {side}");
                    }
                }
                assert_eq!(crosses_nominal_edge, grow > 0.0, "growth crosses nominal tile boundaries at lod {lod}");
            }
        }
    }
}
