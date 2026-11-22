//! Lower panel-part programs to the kernel graph in their own z-up frame.

use crate::csg::{self, Csg, CsgOp, Shape};
use makepad_csg_graph::{
    BoolMode, Evaluator, Graph, NodeId, Op, Plane, Ring, Seg, Shape as SketchShape, Sketch,
};
use makepad_csg_mesh::mesh::TriMesh;
use std::sync::Arc;

/// A lowered program: its graph and the node of the finished solid.
pub struct Lowered {
    pub graph: Graph,
    pub root: NodeId,
}

/// Lower a program in millimetres, keeping its own z-up frame.
/// Surface slots (`Prim::finish`) are not lowered; kernel tagging comes later.
pub fn lower(csg: &Csg) -> Result<Lowered, String> {
    let mut graph = Graph::new();
    let root = lower_into(csg, &mut graph)?;
    Ok(Lowered { graph, root })
}

/// Lower into an existing graph; the caller can place the result with a Transform.
pub fn lower_into(csg: &Csg, g: &mut Graph) -> Result<NodeId, String> {
    let mut root = None;
    let segments = csg.segments.max(3) as u32;
    for (i, step) in csg.steps.iter().enumerate() {
        if root.is_none() && step.op != CsgOp::Add {
            return Err(format!(
                "step {i} ({:?}): the first step must be Add, got {:?}",
                step.prim.label, step.op
            ));
        }
        let b = lower_shape(&step.prim.shape, segments, g)
            .map_err(|e| format!("step {i} ({:?}): {e}", step.prim.label))?;
        root = Some(match root {
            None => b,
            Some(a) => g.push(Op::Boolean {
                mode: match step.op {
                    CsgOp::Add => BoolMode::Union,
                    CsgOp::Sub => BoolMode::Difference,
                    CsgOp::Intersect => BoolMode::Intersection,
                },
                a,
                b: vec![b],
            })?,
        });
    }
    root.ok_or_else(|| "cannot lower an empty CSG program".into())
}

/// Lower and evaluate with a fresh evaluator.
pub fn mesh(csg: &Csg) -> Result<Arc<TriMesh>, String> {
    let lowered = lower(csg)?;
    Evaluator::new()
        .mesh(&lowered.graph, lowered.root, None)
        .map_err(|e| e.to_string())
}

fn transform(
    g: &mut Graph,
    source: NodeId,
    at: [f64; 3],
    rotate: [f64; 3],
) -> Result<NodeId, String> {
    g.push(Op::Transform {
        source,
        at,
        rotate,
        scale: [1.0; 3],
    })
}

fn lower_shape(shape: &Shape, segments: u32, g: &mut Graph) -> Result<NodeId, String> {
    match shape {
        Shape::Loft {
            rings,
            crease,
            prism,
        } => {
            if !crease.is_empty() && crease.len() != rings.len() {
                return Err("loft crease flags must be empty or have one flag per ring".into());
            }
            g.push(Op::RingLoft {
                rings: rings
                    .iter()
                    .enumerate()
                    .map(|(i, ring)| {
                        let (sin, cos) = ring.tilt.to_radians().sin_cos();
                        Ring {
                            origin: [ring.cx, ring.cy, ring.z],
                            u: [1.0, 0.0, 0.0],
                            v: [0.0, cos, sin],
                            points: ring.pts.clone(),
                            crease: crease.get(i).copied().unwrap_or(false),
                        }
                    })
                    .collect(),
                prism: *prism,
            })
        }
        Shape::Box {
            center,
            size,
            rot_z,
            rot_x,
            taper_x,
            taper_y,
            shift_x,
        } => {
            if shape.plain_box() {
                let source = g.push(Op::Box {
                    size: *size,
                    radius: 0.0,
                })?;
                transform(g, source, *center, [0.0, 0.0, *rot_z])
            } else {
                g.push(Op::Hull {
                    points: csg::box_corners(
                        *center, *size, *rot_z, *rot_x, *taper_x, *taper_y, *shift_x,
                    )
                    .to_vec(),
                })
            }
        }
        Shape::Cylinder {
            center,
            r,
            length,
            axis,
        } => {
            let source = g.push(Op::Cylinder {
                radius: *r,
                top_radius: *r,
                height: *length,
                segments,
            })?;
            let (tilt, turn) = csg::axis_angles(*axis);
            let at = std::array::from_fn(|i| {
                center[i] + axis[i] * *length * 0.5
            });
            // The kernel cylinder is centred on Y. First turn +Y onto +Z.
            transform(g, source, at, [90.0, tilt, turn])
        }
        Shape::Sphere { center, r } => {
            let source = g.push(Op::Sphere {
                radius: *r,
                segments,
                rings: segments / 2,
            })?;
            transform(g, source, *center, [0.0; 3])
        }
        Shape::Hull { pts } => g.push(Op::Hull {
            points: pts.clone(),
        }),
        Shape::Group(csg) => lower_into(csg, g),
        Shape::Extrude {
            z,
            height,
            contours,
        } => {
            let mut shapes = Vec::with_capacity(contours.len());
            for (i, contour) in contours.iter().enumerate() {
                let (start, rest) = contour
                    .split_first()
                    .ok_or_else(|| format!("extrude contour {i} is empty"))?;
                // Paths close back to start; nesting makes clockwise contours holes.
                shapes.push(SketchShape::Path {
                    start: *start,
                    segs: rest.iter().map(|p| Seg::Line(*p)).collect(),
                });
            }
            let source = g.push(Op::Extrude {
                sketch: Sketch::new(shapes),
                plane: Plane::Xy,
                height: *height,
                draft: 0.0,
                both: false,
            })?;
            transform(g, source, [0.0, 0.0, *z], [0.0; 3])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg::{Prim, Ring as PartRing};
    use makepad_csg_mesh::{
        validate::{pieces, validate_mesh},
        volume::mesh_volume,
    };
    use std::f64::consts::{PI, TAU};

    fn single(prim: Prim, segments: i32) -> Csg {
        let mut csg = Csg {
            segments,
            ..Csg::default()
        };
        csg.add(prim);
        csg
    }

    fn close(actual: f64, expected: f64, tolerance: f64, property: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{property}: got {actual}, expected {expected}, tolerance {tolerance}"
        );
    }

    fn checked(csg: &Csg, volume: f64, relative: f64, name: &str) -> Arc<TriMesh> {
        let m = mesh(csg).unwrap_or_else(|e| panic!("{name} evaluates: {e}"));
        let report = validate_mesh(&m);
        assert!(report.is_closed, "{name} is closed: {report:?}");
        assert!(report.is_manifold, "{name} is manifold: {report:?}");
        assert!(
            report.is_consistently_oriented,
            "{name} has consistent winding: {report:?}"
        );
        assert_eq!(report.degenerate_triangles, 0, "{name} has no degenerate faces");
        assert_eq!(
            (report.near_duplicate_pairs, report.coincident_pairs), (0, 0),
            "{name}: no vertex pairs within the near-duplicate radius: {report:?}"
        );
        assert_eq!(pieces(&m), 1, "{name} is one piece");
        close(
            mesh_volume(&m),
            volume,
            volume * relative,
            &format!("{name} signed volume"),
        );
        m
    }

    fn square(half: f64) -> Vec<[f64; 2]> {
        vec![[-half, -half], [half, -half], [half, half], [-half, half]]
    }

    fn cylinder_volume(n: i32, r: f64, length: f64) -> f64 {
        f64::from(n) * 0.5 * r * r * (TAU / f64::from(n)).sin() * length
    }

    #[test]
    fn boxes_and_hulls_keep_volume_and_placement() {
        for (angle, extents) in [(0.0, [2.0, 4.0, 6.0]), (90.0, [4.0, 2.0, 6.0])] {
            let csg = single(Prim::cuboid("box", [1.0, 2.0, 3.0], [2.0, 4.0, 6.0]).rot_z(angle), 8);
            let m = checked(&csg, 48.0, 1e-12, "box");
            let bounds = m.bounding_box();
            let min = [bounds.min.x, bounds.min.y, bounds.min.z];
            let max = [bounds.max.x, bounds.max.y, bounds.max.z];
            for i in 0..3 {
                close(max[i] - min[i], extents[i], 1e-12, "box rotated extent");
                close((min[i] + max[i]) * 0.5, [1.0, 2.0, 3.0][i], 1e-12, "box centre");
            }
        }
        let volume = 5.0 / 6.0 * (4.0 * 3.0 + 4.0 * 5.0 * 3.5 + 6.0 * 4.0);
        for (rot_z, rot_x) in [(0.0, 0.0), (25.0, 30.0)] {
            let csg = single(
                Prim::cuboid("drafted box", [1.0, 2.0, 3.0], [4.0, 3.0, 5.0])
                    .taper(2.0, 1.0)
                    .shift_x(0.5)
                    .rot_z(rot_z)
                    .rot_x(rot_x),
                8,
            );
            checked(&csg, volume, 1e-6, "drafted box prismatoid");
        }
        let csg = single(Prim::hull("tetrahedron", vec![
            [0.0; 3], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 4.0],
        ]), 8);
        checked(&csg, 4.0, 1e-12, "tetrahedron hull");
    }

    #[test]
    fn cylinders_keep_the_base_axis_and_polygon_volume() {
        let center = [2.0, -3.0, 4.0];
        for axis in [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [2.0 / 3.0, -1.0 / 3.0, 2.0 / 3.0]] {
            let csg = single(Prim::cylinder("cylinder", center, 1.5, 5.0, axis), 32);
            let m = checked(&csg, cylinder_volume(32, 1.5, 5.0), 1e-6, "cylinder");
            let mut along = (f64::INFINITY, f64::NEG_INFINITY);
            for p in &m.vertices {
                let t: f64 = [p.x, p.y, p.z].iter().enumerate()
                    .map(|(i, p)| (p - center[i]) * axis[i]).sum();
                along = (along.0.min(t), along.1.max(t));
            }
            close(along.0, 0.0, 1e-6, "cylinder base projection");
            close(along.1, 5.0, 1e-6, "cylinder tip projection");
            if axis == [1.0, 0.0, 0.0] {
                let bounds = m.bounding_box();
                close(bounds.min.x, 2.0, 1e-12, "+x cylinder bounding box starts at base");
                close(bounds.max.x, 7.0, 1e-12, "+x cylinder bounding box ends at base plus length");
            }
        }
        let csg = single(Prim::cylinder("coarse cylinder", [0.0; 3], 1.0, 2.0, [0.0, 0.0, 1.0]), -1);
        checked(&csg, cylinder_volume(3, 1.0, 2.0), 1e-6, "minimum circle segments");
    }

    #[test]
    fn sphere_keeps_radius_and_centre() {
        let csg = single(Prim::sphere("sphere", [2.0, -3.0, 4.0], 2.0), 64);
        let m = checked(&csg, 4.0 / 3.0 * PI * 8.0, 0.03, "sphere");
        let bounds = m.bounding_box();
        for (actual, expected) in [
            (bounds.min.x, 0.0), (bounds.max.x, 4.0),
            (bounds.min.y, -5.0), (bounds.max.y, -1.0),
            (bounds.min.z, 2.0), (bounds.max.z, 6.0),
        ] {
            close(actual, expected, 1e-12, "sphere translated bounds");
        }
    }

    #[test]
    fn lofts_keep_ring_frames_flags_and_volume() {
        let bottom = PartRing { z: -1.0, cx: 1.0, cy: -2.0, pts: square(2.0), ..PartRing::default() };
        let top = PartRing { z: 5.0, pts: square(1.0), ..bottom.clone() };
        let frustum_volume = 6.0 / 3.0 * (16.0 + 4.0 + 8.0);
        let prism_top = PartRing { z: 5.0, ..bottom.clone() };
        let mut tilted = PartRing { z: 5.0, tilt: 20.0, ..bottom.clone() };
        // Keep the top's xy projection square: the sloped cap has mean height six.
        let cos = tilted.tilt.to_radians().cos();
        for point in &mut tilted.pts {
            point[1] /= cos;
        }
        for (name, rings, crease, prism, volume) in [
            ("square frustum", vec![bottom.clone(), top], vec![true, false], false, frustum_volume),
            ("tilted loft", vec![bottom.clone(), tilted], vec![], false, 16.0 * 6.0),
            ("square prism", vec![bottom, prism_top], vec![], true, 16.0 * 6.0),
        ] {
            let mut prim = Prim::loft(name, rings.clone()).crease(crease.clone());
            if prism { prim = prim.prism(); }
            let csg = single(prim, 8);
            let lowered = lower(&csg).expect("loft lowers");
            let Op::RingLoft { rings: lowered_rings, prism: lowered_prism } = &lowered.graph.node(lowered.root).op else {
                panic!("loft root is a ring loft");
            };
            assert_eq!(*lowered_prism, prism, "loft keeps its prism flag");
            let m = checked(&csg, volume, 1e-6, name);
            for (i, (ring, lowered_ring)) in rings.iter().zip(lowered_rings).enumerate() {
                assert_eq!(lowered_ring.crease, crease.get(i).copied().unwrap_or(false), "loft keeps crease flags or defaults to false");
                for (p, q) in ring.pts.iter().zip(&lowered_ring.points) {
                    let expected = csg::ring_point(ring, *p);
                    for (axis, expected) in expected.iter().enumerate() {
                        let actual = lowered_ring.origin[axis] + lowered_ring.u[axis] * q[0] + lowered_ring.v[axis] * q[1];
                        close(actual, *expected, 1e-5, "lowered ring vertex equals ring_point");
                    }
                    assert!(m.vertices.iter().any(|v| {
                        (v.x - expected[0]).abs() < 1e-5 && (v.y - expected[1]).abs() < 1e-5 && (v.z - expected[2]).abs() < 1e-5
                    }), "loft mesh retains each ring_point vertex");
                }
            }
        }
    }

    #[test]
    fn extrude_keeps_holes_and_z_range() {
        let mut hole = square(1.0);
        hole.reverse();
        let csg = single(Prim::extrude("square hole", -2.0, 3.0, vec![square(3.0), hole]), 8);
        let m = checked(&csg, (36.0 - 4.0) * 3.0, 1e-12, "extruded square hole");
        let bounds = m.bounding_box();
        close(bounds.min.z, -2.0, 1e-12, "extrusion starts at z");
        close(bounds.max.z, 1.0, 1e-12, "extrusion ends at z plus height");
    }

    #[test]
    fn booleans_fold_in_order_and_groups_keep_their_resolution() {
        let mut bore = single(Prim::cuboid("body", [0.0; 3], [6.0, 6.0, 4.0]), 16);
        bore.sub(Prim::cylinder("bore", [0.0, 0.0, -3.0], 1.0, 6.0, [0.0, 0.0, 1.0]));
        let drilled_volume = 144.0 - cylinder_volume(16, 1.0, 4.0);
        checked(&bore, drilled_volume, 1e-6, "box minus through cylinder");

        // A box cuts the sphere at its equator, giving half its polygonal volume.
        let ball = single(Prim::sphere("sphere", [0.0; 3], 2.0), 16);
        let ball_volume = mesh_volume(&mesh(&ball).expect("intersection reference sphere evaluates"));
        let mut intersection = single(Prim::cuboid("half space", [0.0, 2.0, 0.0], [6.0, 4.0, 6.0]), 16);
        intersection.intersect(ball.steps[0].prim.clone());
        checked(&intersection, ball_volume * 0.5, 1e-6, "box intersected with sphere");

        let mut grouped = single(Prim::group("drilled group", bore), 8);
        // Filling the bore partly tests union after the nested difference.
        grouped.add(Prim::cuboid("fill", [0.0, 0.0, -1.0], [6.0, 6.0, 2.0]));
        checked(&grouped, 144.0 - cylinder_volume(16, 1.0, 2.0), 1e-6, "group with its own resolution");
        // Applying the intersection last must also clip the filled region.
        grouped.intersect(Prim::cuboid("clip", [0.0; 3], [8.0, 8.0, 2.0]));
        checked(&grouped, 72.0 - cylinder_volume(16, 1.0, 1.0), 1e-6, "left-folded nested program");

        let mut graph = Graph::new();
        let existing = graph.push(Op::Box { size: [1.0; 3], radius: 0.0 }).expect("existing graph node builds");
        let key = graph.node(existing).key;
        let root = lower_into(&grouped, &mut graph).expect("program lowers into existing graph");
        assert_eq!(graph.node(existing).key, key, "lower_into preserves existing nodes");
        let placed = transform(&mut graph, root, [0.0, 10.0, 0.0], [-90.0, 0.0, 0.0]).expect("caller can place lowered part");
        let m = Evaluator::new().mesh(&graph, placed, None).expect("placed part evaluates");
        close(m.bounding_box().min.y, 9.0, 1e-6, "caller alone maps part z to model y");
        close(m.bounding_box().max.y, 11.0, 1e-6, "caller placement preserves the part extent");
    }

    #[test]
    fn empty_and_non_add_first_steps_are_errors() {
        assert!(lower(&Csg::default()).is_err(), "empty program is rejected");
        for op in [CsgOp::Sub, CsgOp::Intersect] {
            let mut csg = single(Prim::cuboid("first primitive", [0.0; 3], [1.0; 3]), 8);
            csg.steps[0].op = op;
            let error = lower(&csg).err().expect("non-Add first step is rejected");
            assert!(error.contains("step 0") && error.contains("first primitive") && error.contains("Add"), "first-step error names the step and required operation: {error}");
        }
    }
}
