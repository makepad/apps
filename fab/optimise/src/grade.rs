//! Stress-graded parts (`m.voronoi(.., {adapt: @stress})`): the stress a solve of the object
//! carries with the graded part's source in its place, sampled on the grid the part asks for
//! ([`StressRequest`]), relative to a reference, for the lowering's [`Options::solved`].
//!
//! The object is solved where its study's loads are given (the printer frame, as Force solves
//! it), as plain solid plastic (stress under fixed loads does not depend on the modulus of an
//! even material, so it stands for every filament), at about a study's first cell count. The
//! reference is the 90th percentile of the cells' von Mises: one hot spot at a fixture does not
//! thin every other strut.
//!
//! [`Options::solved`]: makepad_model_script::lang::Options::solved

use fab_force::material::{Materials, Print, FILAMENTS};
use fab_force::results;
use fab_force::study::Load;
use fab_force::V3;
use makepad_model_script::lang::csg_sdf::{self, quat_from_degrees, SdfGrid, SdfNode, SdfPlace};
use makepad_model_script::lang::StressRequest;

/// The share of the solid's cells whose von Mises is under the reference.
pub const REFERENCE_SHARE: f64 = 0.9;

/// A graded part's stress, solved.
#[derive(Clone, Debug)]
pub struct Graded {
    /// Von Mises over the reference on the request's grid (the part's frame).
    pub grid: SdfGrid,
    /// The reference (MPa) and the cell size solved at (mm).
    pub reference: f64,
    pub h: f64,
}

/// The places from part `id`'s body into `root`'s frame, innermost first: the moves down to its
/// `SdfNode::Id` and its own placement right under it. An error when it is not there once,
/// rigidly (under a deform or as instances it has no one frame).
pub fn part_places(root: &SdfNode, id: u32) -> Result<Vec<SdfPlace>, String> {
    fn walk(n: &SdfNode, id: u32, out: &mut Vec<SdfPlace>, found: &mut usize, rigid: bool) -> Result<(), String> {
        match n {
            SdfNode::Id { id: i, of } if *i == id => {
                if !rigid {
                    return Err("the part is under a deform or instanced: it has no one frame".into());
                }
                *found += 1;
                let mut own = Vec::new();
                let mut n = &**of;
                while let SdfNode::Transform { at, rotate, scale, of } = n {
                    own.push(SdfPlace { at: *at, rotate: quat_from_degrees(*rotate), scale: *scale });
                    n = of;
                }
                out.extend(own.into_iter().rev());
                return Ok(());
            }
            SdfNode::Transform { at, rotate, scale, of } => {
                let before = *found;
                walk(of, id, out, found, rigid)?;
                if *found > before {
                    out.push(SdfPlace { at: *at, rotate: quat_from_degrees(*rotate), scale: *scale });
                }
            }
            SdfNode::Combine { of, .. } => {
                for c in of {
                    walk(c, id, out, found, rigid)?;
                }
            }
            SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Material { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Id { of, .. } | SdfNode::Sculpt { of, .. } | SdfNode::Texture { of, .. } => walk(of, id, out, found, rigid)?,
            SdfNode::Deform { of, .. } | SdfNode::Instance { of, .. } => walk(of, id, out, found, false)?,
            _ => {}
        }
        Ok(())
    }
    let (mut out, mut found) = (Vec::new(), 0);
    walk(root, id, &mut out, &mut found, true)?;
    match found {
        1 => Ok(out),
        0 => Err("the part is not in the object".into()),
        _ => Err("the part is in the object more than once".into()),
    }
}

/// `root` with part `id`'s body (under its own placement) replaced by `source`.
pub fn with_source(root: &SdfNode, id: u32, source: &SdfNode) -> SdfNode {
    fn body(n: &SdfNode, source: &SdfNode) -> SdfNode {
        match n {
            SdfNode::Transform { at, rotate, scale, of } => SdfNode::Transform { at: *at, rotate: *rotate, scale: *scale, of: Box::new(body(of, source)) },
            _ => source.clone(),
        }
    }
    let sub = |n: &SdfNode| Box::new(with_source(n, id, source));
    match root {
        SdfNode::Id { id: i, of } if *i == id => SdfNode::Id { id, of: Box::new(body(of, source)) },
        SdfNode::Id { id: i, of } => SdfNode::Id { id: *i, of: sub(of) },
        SdfNode::Transform { at, rotate, scale, of } => SdfNode::Transform { at: *at, rotate: *rotate, scale: *scale, of: sub(of) },
        SdfNode::Combine { op, blend, of } => SdfNode::Combine { op: *op, blend: *blend, of: of.iter().map(|c| with_source(c, id, source)).collect() },
        SdfNode::Shell { thickness, of } => SdfNode::Shell { thickness: *thickness, of: sub(of) },
        SdfNode::Round { radius, of } => SdfNode::Round { radius: *radius, of: sub(of) },
        SdfNode::Material { slot, within, of } => SdfNode::Material { slot: *slot, within: within.clone(), of: sub(of) },
        SdfNode::Bone { id: b, rest, of } => SdfNode::Bone { id: *b, rest: *rest, of: sub(of) },
        SdfNode::Sculpt { strokes, of } => SdfNode::Sculpt { strokes: strokes.clone(), of: sub(of) },
        SdfNode::Texture { tex, of } => SdfNode::Texture { tex: tex.clone(), of: sub(of) },
        other => other.clone(),
    }
}

/// A rigid map with a uniform scale, row-major (`p_out = m * p`), and back.
#[derive(Clone, Copy, Debug)]
struct Affine {
    m: [[f64; 4]; 3],
    scale: f64,
}

impl Affine {
    fn new(m: [[f64; 4]; 3]) -> Affine {
        Affine { m, scale: (0..3).map(|i| m[i][0] * m[i][0]).sum::<f64>().sqrt().max(1e-12) }
    }
    fn out(&self, p: V3) -> V3 {
        std::array::from_fn(|i| self.m[i][0] * p[0] + self.m[i][1] * p[1] + self.m[i][2] * p[2] + self.m[i][3])
    }
    fn back(&self, p: V3) -> V3 {
        let d: V3 = std::array::from_fn(|i| p[i] - self.m[i][3]);
        let s2 = self.scale * self.scale;
        std::array::from_fn(|c| (self.m[0][c] * d[0] + self.m[1][c] * d[1] + self.m[2][c] * d[2]) / s2)
    }
}

/// The stress in `object` (its shape frame, mm, `to_frame` taking it to the study's) with
/// `request`'s part solid (its source), under `loads` (the study's frame), over the solid's
/// box at about `cells` cells: relative to its reference, on the request's grid.
pub fn stress_grid(object: &SdfNode, to_frame: [[f64; 4]; 3], request: &StressRequest, loads: &[Load], cells: f64) -> Result<Graded, String> {
    let id = csg_sdf::part_id(&request.part);
    let places = part_places(object, id)?;
    let solid = with_source(object, id, &request.source);
    let to = Affine::new(to_frame);
    let Some(b) = csg_sdf::node_bounds(&solid) else { return Err("the solid has no bounds".into()) };
    let b = csg_sdf::bounds::map_bounds(&b, |p| to.out(p));
    let h = ((0..3).map(|a| (b.1[a] - b.0[a]).max(1e-3)).product::<f64>() / cells).cbrt();
    let (lo, hi) = (b.0.map(|x| x - h), b.1.map(|x| x + h));
    let field = |p: V3| {
        let q = to.back(p);
        csg_sdf::distance(&solid, makepad_csg_math::Vec3d::new(q[0], q[1], q[2])) * to.scale
    };
    let m = Materials::new(FILAMENTS[0], &Print::default(), false);
    let (_, outcome) = fab_force::opt::solve_field(lo, hi, h, &field, loads, &m, &fab_force::solve::Options::default())?;
    let grid = &outcome.grid;
    let vm = results::nodal(grid, &outcome.results, 0);
    // the reference: the cells' von Mises at the share
    let mut cells: Vec<f32> = outcome.results.cells.iter().filter(|c| c[3] > 0.0 && c[0].is_finite()).map(|c| c[0]).collect();
    if cells.is_empty() {
        return Err("the solve left no stressed cells".into());
    }
    cells.sort_by(f32::total_cmp);
    let reference = (cells[((cells.len() - 1) as f64 * REFERENCE_SHARE) as usize] as f64).max(1e-9);
    // trilinear in the nodes, clamped to the grid
    let nd = grid.node_dims();
    let sample = |p: V3| -> f64 {
        let mut c = [0usize; 3];
        let mut f = [0f64; 3];
        for a in 0..3 {
            let u = ((p[a] - grid.origin[a]) / grid.h[a]).clamp(0.0, (nd[a] - 1) as f64);
            c[a] = (u.floor() as usize).min(nd[a] - 2);
            f[a] = u - c[a] as f64;
        }
        let mut v = 0.0;
        for k in 0..8 {
            let o = [k & 1, (k >> 1) & 1, k >> 2];
            let w: f64 = (0..3).map(|a| if o[a] == 1 { f[a] } else { 1.0 - f[a] }).product();
            v += w * vm[grid.node(c[0] + o[0], c[1] + o[1], c[2] + o[2])] as f64;
        }
        v
    };
    let n: [u32; 3] = std::array::from_fn(|a| ((request.hi[a] - request.lo[a]) / request.spacing).ceil().max(1.0) as u32 + 1);
    let mut data = Vec::with_capacity((n[0] * n[1] * n[2]) as usize);
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                let q = [i, j, k];
                let mut p: V3 = std::array::from_fn(|a| request.lo[a] + q[a] as f64 * request.spacing);
                for place in &places {
                    p = place.out_of(p);
                }
                let p = to.out(p);
                data.push((sample(p) / reference) as f32);
            }
        }
    }
    let grid = SdfGrid::new(request.lo.map(|x| x as f32), request.spacing as f32, n, 0.0, data.into());
    Ok(Graded { grid, reference, h })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_force::bc::Kind;
    use fab_force::study::Spot;
    use fab_force::surface::Pick;
    use makepad_model_script::lang::{evaluate, Options, Program};
    use makepad_model_script::lang::csg_sdf::SdfGrid;
    use std::collections::HashMap;
    use std::sync::Arc;

    const SRC: &str = "use mod.fab.*\nlet m = mod.model\nlet bar = m.box({size: [60mm 10mm 10mm]})\nlet v = m.voronoi(bar, {size: 5mm strut: 1mm adapt: @stress core: @solid})\nobject(v)\n";

    fn eval(stress: HashMap<String, Arc<SdfGrid>>) -> Program {
        let assets = makepad_model_script::lang::Assets::new();
        evaluate(SRC, &Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &makepad_model_script::lang::NO_INPUTS, unit: fab_model::program::UNIT, roots: fab_model::program::ROOTS, overrides: HashMap::new(), weave: None, solved: stress }).unwrap()
    }

    /// A cantilever bar as a graded web: held at one end, loaded at the other, its stress (the
    /// bending moment's, highest at the held end) on the part's grid, then the web graded by it.
    #[test]
    fn a_cantilevers_stress_grades_its_web() {
        let plain = eval(HashMap::new());
        let request = plain.stress_requests.first().expect("the graded part asks for its stress").clone();
        let makepad_model_script::lang::csg_graph::Op::Field { shape, .. } = &plain.graph.node(plain.solid("v").unwrap().node).op else { panic!("a field") };
        let loads = [
            Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Face, at: [-30.0, 0.0, 0.0] }),
            Load::new(Kind::Force([0.0, -50.0, 0.0]), Spot::Seed { pick: Pick::Face, at: [30.0, 0.0, 0.0] }),
        ];
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let graded = stress_grid(&shape.root, identity, &request, &loads, 3000.0).unwrap();
        let g = &graded.grid;
        // the stress along the bar, over each end's third
        let n = g.dims.map(|n| n as usize);
        let mean = |x0: f32, x1: f32| {
            let v: Vec<f32> = (0..g.data.len()).filter(|i| (x0..x1).contains(&(g.origin[0] + (i % n[0]) as f32 * g.h))).map(|i| g.data[i]).collect();
            v.iter().sum::<f32>() / v.len() as f32
        };
        let (held, free) = (mean(-30.0, -10.0), mean(10.0, 30.0));
        assert!(held > 2.0 * free, "the held end carries more: {held} against {free}");
        assert!(graded.reference > 0.0 && graded.h > 0.0, "{graded:?}");
        let graded = eval(HashMap::from([("v".to_string(), Arc::new(graded.grid))]));
        assert!(graded.notes.is_empty(), "graded once given its stress: {:?}", graded.notes);
    }
}
