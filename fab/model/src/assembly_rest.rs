//! The analytic solid beneath mesh booleans, for proving bed contact. Meshing a field may
//! put vertices outside its solid; those vertices cannot determine an assembly's rest.

use makepad_csg_graph::{BoolMode, Graph, NodeId, Op};
use makepad_csg_sdf::{bounds::map_bounds, node_bounds, SdfNode, SdfOp, SdfShape};

/// Only field-derived geometry needs a separate rest proof. Ordinary mesh parts keep their
/// mesh contact. Materials do not participate in distance or extent queries.
pub(crate) fn shape(graph: &Graph, id: NodeId) -> Option<SdfShape> {
    let (shape, field) = operand(graph, id)?;
    field.then_some(shape)
}

/// A part's analytic solid, field-derived or not: what judges whether a part above the plate
/// stands on it, or stands on another part.
pub(crate) fn solid(graph: &Graph, id: NodeId) -> Option<SdfShape> {
    operand(graph, id).map(|(shape, _)| shape)
}

fn operand(graph: &Graph, id: NodeId) -> Option<(SdfShape, bool)> {
    let root = match &graph.node(id).op {
        Op::Field { shape, .. } => return Some((shape.clone(), true)),
        Op::Box { size, radius } => SdfNode::Box { size: *size, radius: *radius },
        Op::Sphere { radius, .. } => SdfNode::Sphere { radius: *radius },
        Op::Cylinder { radius, top_radius, height, .. } => SdfNode::Cylinder { radius: *radius, top_radius: *top_radius, height: *height },
        Op::Transform { source, at, rotate, scale } if scale[0] == scale[1] && scale[1] == scale[2] && scale[0] > 0.0 => {
            let (s, field) = operand(graph, *source)?;
            let (min, max) = map_bounds(&(s.min, s.max), |p| makepad_csg_sdf::transform_out_of(*at, *rotate, scale[0], p));
            let root = SdfNode::Transform { at: *at, rotate: *rotate, scale: scale[0], of: Box::new(s.root) };
            return Some((SdfShape { root, min, max, materials: Vec::new() }, field));
        }
        Op::Boolean { mode, a, b } => {
            let (first, mut field) = operand(graph, *a)?;
            let (mut min, mut max) = (first.min, first.max);
            let mut of = vec![first.root];
            for id in b {
                let (s, has_field) = operand(graph, *id)?;
                field |= has_field;
                match mode {
                    BoolMode::Union => {
                        min = std::array::from_fn(|i| min[i].min(s.min[i]));
                        max = std::array::from_fn(|i| max[i].max(s.max[i]));
                    }
                    BoolMode::Intersection => {
                        min = std::array::from_fn(|i| min[i].max(s.min[i]));
                        max = std::array::from_fn(|i| max[i].min(s.max[i]));
                    }
                    BoolMode::Difference => {}
                }
                of.push(s.root);
            }
            if (0..3).any(|i| min[i] > max[i]) {
                return None;
            }
            let op = match mode {
                BoolMode::Union => SdfOp::Union,
                BoolMode::Intersection => SdfOp::Intersection,
                BoolMode::Difference => SdfOp::Difference,
            };
            return Some((SdfShape { root: SdfNode::Combine { op, blend: 0.0, of }, min, max, materials: Vec::new() }, field));
        }
        _ => return None,
    };
    let (min, max) = node_bounds(&root)?;
    Some((SdfShape { root, min, max, materials: Vec::new() }, false))
}
