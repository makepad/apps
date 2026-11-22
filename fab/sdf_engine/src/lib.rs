//! Fab's field evaluation, GPU tracing and slicing engine.
//!
//! - [`program`]: a `csg_sdf` shape lowered to a flat f32 program;
//! - [`twin`]: the CPU interpreter of that program, bit-exact with the GPU;
//! - [`kernel`]: Splash and mesh kernels shared by GPU field evaluation;
//! - [`plane_abi`]: the GPU records handed between the slicer's stages;
//! - [`plane`]: plane tracing (coarse grid, nodes, crossings, spans, the outline FeatureSet);
//! - [`list_rank`]: roots, ranks and ids of successor lists (cycles and open chains);
//! - [`chain`]: pieces to loops and open chains, their metrics and containment;
//! - [`rings`]: chained wall pieces to the layer plan's rings, the thick test and the readback;
//! - [`feature_set`]: the FeatureSet for contours traced on the cores;
//! - [`feature_field`]: a FeatureSet's band, nearest features and candidate lists;
//! - [`wall_pieces`]: the level curves of a feature field as keyed pieces per fine cell;
//! - [`thin`]: thin walls, the skeleton of a FeatureSet's sites and its bead pieces;
//! - [`regions_compose`]: a round of the class program, compositions of FeatureSets into keyed pieces;
//! - [`regions_label`]: packed class labels at fine nodes, with overlap counts;
//! - [`regions_program`]: the class program as rounds over a batch of planes;
//! - [`regions_classes`]: the class rounds of a batch, from the outline's and the fill's fields;
//! - [`regions_concentric`]: S1b, the concentric solid fill's rounds over a region's own field levels;
//! - [`regions_overhang`]: S3, the overhang labels of a batch's wall loops against the layer below;
//! - [`regions_set`]: a region's FeatureSet from a compose round's output and its chain;
//! - [`regions_keep`]: a Keep round's per-component decision over a chain.
//!
//! Buffer-backed compute shaders are available in [`compute`]; frame dispatch
//! remains with the host integration.

pub use makepad_csg_sdf;
pub use makepad_draw;

pub mod chain;
pub mod compute;
pub mod feature_field;
pub mod feature_set;
pub mod gpu_jobs;
pub mod kernel;
pub mod list_rank;
pub mod mesh;
pub mod plane;
pub mod plane_abi;
pub mod program;
pub mod regions_classes;
pub mod regions_compose;
pub mod rings;
pub mod regions_label;
pub mod regions_concentric;
pub mod regions_overhang;
pub mod regions_keep;
pub mod regions_program;
pub mod regions_set;
pub mod thin;
pub mod twin;
pub mod trace;
pub mod wall_pieces;

use makepad_draw::*;

/// The GPU slicer's kernels: the job runtime, the layer trace, plane tracing, chains, rings,
/// feature fields, wall pieces, thin walls and the region rounds. They build on `mod.sdf.Eval`,
/// so they register after [`compute::script_mod`], and only where the GPU slicer runs.
pub fn slicer_script_mod(vm: &mut ScriptVm) -> ScriptValue {
    crate::gpu_jobs::script_mod(vm);
    crate::trace::script_mod(vm);
    crate::plane::script_mod(vm);
    crate::list_rank::script_mod(vm);
    crate::chain::script_mod(vm);
    crate::rings::script_mod(vm);
    crate::feature_field::script_mod(vm);
    crate::regions_compose::script_mod(vm);
    crate::regions_concentric::script_mod(vm);
    crate::regions_overhang::script_mod(vm);
    crate::regions_set::script_mod(vm);
    crate::regions_keep::script_mod(vm);
    crate::wall_pieces::script_mod(vm);
    crate::regions_label::script_mod(vm);
    crate::thin::script_mod(vm)
}

/// Marks the bodies a cut adds: a shape under a cut is an intersection of it and bodies of this
/// material slot, each with a face on a plane. The views colour those faces as cut.
pub const CUT_SLOT: u8 = 255;

/// What half-spaces leave of a shape.
pub enum Cut {
    /// All of it: no plane reaches it.
    Whole,
    /// Nothing.
    Gone,
    Left(makepad_csg_sdf::SdfShape),
}

/// `shape` under the half-spaces (`n . p + d >= 0`, unit normals, in the shape's frame): the field
/// the rays see, so what a cut opens up and the cut's faces are traced as any surface is. Each
/// plane is one more operand of an intersection, a box whose face lies on the plane and whose other
/// faces are out of the shape's reach: a true distance around it, so what proves emptiness holds.
/// Only its bounds are read: nothing is evaluated.
pub fn cut_shape(shape: &makepad_csg_sdf::SdfShape, planes: &[[f64; 4]]) -> Cut {
    use makepad_csg_sdf::{node_bounds, SdfNode, SdfOp, SdfShape};
    let Some((lo, hi)) = node_bounds(&shape.root) else { return Cut::Whole };
    let centre: [f64; 3] = std::array::from_fn(|i| (lo[i] + hi[i]) * 0.5);
    let radius = (0..3).map(|i| (hi[i] - lo[i]) * 0.5).map(|h| h * h).sum::<f64>().sqrt();
    let mut of = Vec::new();
    for p in planes {
        let [nearest, farthest] = [f64::min, f64::max].map(|side| (0..3).map(|i| side(p[i] * lo[i], p[i] * hi[i])).sum::<f64>() + p[3]);
        if nearest >= 0.0 {
            continue;
        }
        if farthest <= 0.0 {
            return Cut::Gone;
        }
        let from = p[0] * centre[0] + p[1] * centre[1] + p[2] * centre[2] + p[3];
        let half = 2.0 * (radius + from.abs());
        let at: [f64; 3] = std::array::from_fn(|i| centre[i] + p[i] * (half - from));
        // +Y of the box along the normal: turned about X, then Z
        let rotate = [p[2].clamp(-1.0, 1.0).asin().to_degrees(), 0.0, (-p[0]).atan2(p[1]).to_degrees()];
        let body = SdfNode::Transform { at, rotate, scale: 1.0, of: Box::new(SdfNode::Box { size: [half * 2.0; 3], radius: 0.0 }) };
        of.push(SdfNode::Material { slot: CUT_SLOT, within: None, of: Box::new(body) });
    }
    if of.is_empty() {
        return Cut::Whole;
    }
    of.insert(0, shape.root.clone());
    Cut::Left(SdfShape { root: SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of }, min: shape.min, max: shape.max, materials: shape.materials.clone() })
}
