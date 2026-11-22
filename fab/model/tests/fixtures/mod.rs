//! Existing Rust-only fixtures, transcribed with their original dimensions and parameters.
//! The source paths and test names below are the inventory: these tests cannot be called
//! from an integration test (their modules are cfg(test) and private). No mesher runs here.
use fab_sdf_engine::makepad_csg_sdf::{self as sdf, *};
use std::sync::Arc;

type Visitor<'a> = dyn FnMut(&str, &str, SdfNode, Option<&str>) + 'a;

fn at(at: [f64; 3], of: SdfNode) -> SdfNode {
    SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) }
}
fn combine(op: SdfOp, blend: f64, of: Vec<SdfNode>) -> SdfNode { SdfNode::Combine { op, blend, of } }

// fab/sdf_engine/src/volume.rs::tests::vase
fn vase(height: f64, belly: f64) -> SdfNode {
    let (neck, wall) = (9.5, 1.6);
    let body = combine(SdfOp::Union, 9.0, vec![
        at([0.0, height * 0.34, 0.0], SdfNode::Ellipsoid { radii: [belly, height * 0.36, belly] }),
        at([0.0, height * 0.74, 0.0], SdfNode::Cylinder { radius: neck, top_radius: neck, height: height * 0.42 }),
    ]);
    let footed = combine(SdfOp::Difference, 0.0, vec![body, at([0.0, -7.0, 0.0], SdfNode::Box { size: [4.0 * belly, 20.0, 4.0 * belly], radius: 0.0 })]);
    let hollow = SdfNode::Shell { thickness: wall, of: Box::new(footed) };
    let rimmed = combine(SdfOp::Union, 1.5, vec![hollow, at([0.0, height * 0.94, 0.0], SdfNode::Torus { radius: neck + 1.2, tube: 1.8 })]);
    let (low, high) = (height * 0.87 - wall, height + 20.0);
    combine(SdfOp::Difference, 0.0, vec![rimmed, at([0.0, (low + high) / 2.0, 0.0], SdfNode::Cylinder { radius: neck - wall, top_radius: neck - wall, height: high - low })])
}

// libs/csg/csg_sdf/src/mesh_field.rs::tests::cube
fn cube(open_top: bool) -> sdf::mesh_field::MeshData {
    let positions = (0..8).map(|i| [if i & 1 == 0 { -1.0 } else { 1.0 }, if i & 2 == 0 { -1.0 } else { 1.0 }, if i & 4 == 0 { -1.0 } else { 1.0 }]).collect();
    let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
    let mut triangles = Vec::new();
    for (k, q) in quads.iter().enumerate() {
        if open_top && k == 3 { continue; }
        triangles.extend([[q[0], q[1], q[2]], [q[0], q[2], q[3]]]);
    }
    sdf::mesh_field::MeshData { positions, triangles }
}

/// The STL created by app/tests/sdf_view.rs::imported_stl_carves_as_a_field.
pub fn imported_stl_cube() -> Arc<sdf::mesh_field::MeshField> {
    let mut data = cube(false);
    for p in &mut data.positions { *p = [p[0] * 10.0, p[1] * 10.0, p[2] * 10.0 + 10.0]; }
    let bytes = makepad_fab_slice::bench::stl_bytes(&makepad_fab_slicer::mesh::Mesh {
        verts: data.positions.iter().map(|p| p.map(|v| v as f64)).collect(), tris: data.triangles, paint: Vec::new(),
    });
    let mesh = makepad_fab_slicer::mesh::load_stl(&bytes).unwrap();
    Arc::new(sdf::mesh_field::MeshField::new(sdf::mesh_field::MeshData {
        positions: mesh.verts.iter().map(|p| p.map(|v| v as f32)).collect(), triangles: mesh.tris,
    }))
}

fn splash(param: bool) -> SdfSplashExpr {
    use makepad_script::*;
    let mut host = ScriptVmHost::new(0i32, ());
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    vm.capture_errors();
    let text = if param { "use mod.math.*\nlet f = |p, r| length(p) - r\n(f)" }
        else { "use mod.math.*\nlet f = |p| length(p) - 1.0\n(f)" };
    let value = vm.eval(ScriptMod::new("", "", "sphere_field", 0, 0, text, &[]));
    assert!(vm.take_errors().is_empty());
    let f = value.as_object().unwrap();
    let _keep = vm.new_object_ref(f);
    SdfSplashExpr::compile(&mut vm, f, SplashPoint::Vec3, if param { &[1] } else { &[] }).unwrap()
}

pub fn visit(v: &mut Visitor<'_>) {
    let body = SdfNode::Material { slot: 2, within: None, of: Box::new(SdfNode::Sphere { radius: 5.0 }) };
    let region = at([5.0, 0.0, 0.0], SdfNode::Sphere { radius: 2.0 });
    v("fab/sdf_engine/src/twin.rs", "a_material_within_a_region_follows_csg_sdf", SdfNode::Material { slot: 4, within: Some(Box::new(region)), of: Box::new(body) }, None);
    let body = SdfNode::Id { id: 1, of: Box::new(at([0.0; 3], SdfNode::Sphere { radius: 5.0 })) };
    let boss = SdfNode::Id { id: 2, of: Box::new(at([0.0, 0.0, 6.0], SdfNode::Sphere { radius: 2.0 })) };
    v("fab/sdf_engine/src/pick.rs", "a_ray_names_the_feature_it_hits", combine(SdfOp::Union, 0.0, vec![body, boss]), None);
    for (h, b) in [(70.0, 21.0), (70.0, 36.0), (70.0, 51.0), (82.0, 51.0), (106.0, 51.0), (142.0, 51.0)] {
        v("fab/sdf_engine/src/volume.rs", &format!("a_dragged_vase_keeps_every_surface_brick/h={h}/belly={b}"), vase(h, b), None);
    }

    let loops: Vec<LinkLoop> = (0..20).map(|i| {
        let x = i as f64 * 12.0;
        LinkLoop { pts: vec![[x - 4.0, 1.0, -4.0], [x + 4.0, 1.0, -4.0], [x + 4.0, 1.0, 4.0], [x - 4.0, 1.0, 4.0]], slot: (i % 3) as u8 }
    }).collect();
    for (name, r, clip) in [("exact_near_and_lipschitz_far", 0.8, Some(([0.0, 1.0, 0.0], 0.0))), ("exact_near_and_lipschitz_far/key_variant", 0.9, None)] {
        v("libs/csg/csg_sdf/src/links.rs", name, SdfNode::Links(Arc::new(Links::new(loops.clone(), r, clip))), None);
    }
    let pts = (0..24).map(|k| { let a = k as f64 / 24.0 * std::f64::consts::TAU; [10.0 * a.cos(), 2.0, 10.0 * a.sin()] }).collect();
    v("libs/csg/csg_sdf/src/links.rs", "mesh_is_closed_and_outward", SdfNode::Links(Arc::new(Links::new(vec![LinkLoop { pts, slot: 0 }], 1.0, Some(([0.0, 1.0, 0.0], 1.5))))), None);
    for (name, open) in [("a_watertight_cube_is_an_exact_field", false), ("an_open_lid_cup_keeps_its_sign_below_the_rim", true)] {
        v("libs/csg/csg_sdf/src/mesh_field.rs", name, SdfNode::Mesh(Arc::new(sdf::mesh_field::MeshField::new(cube(open)))), None);
    }
    let mut mesh = cube(false);
    let n = mesh.positions.len() as u32;
    mesh.positions.extend(mesh.positions.clone().iter().map(|p| [p[0] + 1.0, p[1], p[2]]));
    mesh.triangles.extend(mesh.triangles.clone().iter().map(|t| [t[0] + n, t[1] + n, t[2] + n]));
    v("libs/csg/csg_sdf/src/mesh_field.rs", "overlapping_shells_read_as_their_union", SdfNode::Mesh(Arc::new(sdf::mesh_field::MeshField::new(mesh))), None);
    for name in ["sphere_mesh_matches_mirror_exactly_and_analytic_within_tolerance", "batch_matches_pointwise"] {
        v("libs/csg/csg_sdf/tests/splash_field.rs", name, SdfNode::Kernel(SdfKernel::splash(splash(false), Some(([-1.6; 3], [1.6; 3])))), None);
    }
    for r in [0.5, 1.0] {
        let mut field = splash(true); field.set_uniforms(&[r]);
        v("libs/csg/csg_sdf/tests/splash_field.rs", &format!("parametric_radius_remesh/r={r}"), SdfNode::Kernel(SdfKernel::splash(field, Some(([-1.6; 3], [1.6; 3])))), None);
    }
    // csg_graph's bounded radius-10 Splash sphere fixture.
    let mut ball = splash(true); ball.set_uniforms(&[10.0]);
    v("libs/csg/csg_graph/src/tests.rs", "a_field_meshes_inside_its_bounds", SdfNode::Kernel(SdfKernel::splash(ball, Some(([-10.0; 3], [10.0; 3])))), None);
}
