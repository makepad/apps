use fab_sdf_engine::{kernel, mesh::MeshAsset, program, twin};
use fab_sdf_engine::makepad_csg_sdf::SdfNode;
use std::sync::Arc;

#[path = "support/meshes.rs"]
mod meshes;
use meshes::corpus;

static GPU_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn mesh_atan2_matches_f64_dense() {
    use fab_sdf_engine::makepad_csg_sdf::mesh_field::mesh_atan2;
    let mut worst = 0.0f64;
    let mut count = 0;
    let mut check = |y: f32, x: f32| {
        let got = mesh_atan2(y, x);
        let expected = (y as f64).atan2(x as f64);
        let error = (got as f64 - expected).abs();
        worst = worst.max(error);
        count += 1;
        assert!(error <= 1e-6, "atan2({y:?}, {x:?})={got:?}, f64={expected:?}, error={error}");
        if y == 0.0 || x == 0.0 {
            assert_eq!(got.to_bits(), (expected as f32).to_bits(), "axis/zero atan2({y:?}, {x:?})");
        }
    };
    for y in -128..=128 { for x in -128..=128 { check(y as f32 / 128.0, x as f32 / 128.0); } }
    for i in 0..16384 {
        let a = i as f64 * std::f64::consts::TAU / 16384.0;
        for scale in [1e-30f32, 1e-10, 1.0, 1e30] { check(a.sin() as f32 * scale, a.cos() as f32 * scale); }
    }
    for y in [-f32::INFINITY, -1.0, -0.0, 0.0, 1.0, f32::INFINITY] {
        for x in [-f32::INFINITY, -1.0, -0.0, 0.0, 1.0, f32::INFINITY] { check(y, x); }
    }
    println!("MESH atan2: {count} samples, worst {worst:e} rad against f64");
}

fn points() -> Vec<[f32; 3]> {
    let mut ps = Vec::new();
    // Faces, edges, vertices, both sides of the surface, interior and far dipoles.
    for a in 0..3 { for side in [-1.0, 1.0] { for offset in [-0.001, 0.0, 0.001] {
        for u in [-1.0, -0.37, 0.0, 0.63, 1.0] { for v in [-1.0, -0.29, 0.0, 0.71, 1.0] {
            let mut p = [0.0; 3]; p[a] = side + offset; p[(a + 1) % 3] = u; p[(a + 2) % 3] = v;
            ps.push(p);
        } }
    } } }
    ps.extend([[0.0; 3], [0.5, 0.25, -0.1], [8.0, -9.0, 12.0]]);
    // Include a regular lattice through the mesh interior and beyond its faces.
    ps.extend((0..729).map(|i| [(i % 9) as f32 * 0.25, (i / 81) as f32 * 0.25, ((i / 9) % 9) as f32 * 0.25]));
    for x in [-1.0, -0.0, 0.0, 1.0] { for y in [-1.0, -0.0, 0.0, 1.0] { ps.push([x, y, 0.3]); } }
    ps
}

#[test]
fn mesh_twin_matches_signed_and_signed64() {
    let ps = points();
    for (name, mesh) in corpus() {
        let asset = MeshAsset::new(&mesh);
        if name == "cup" {
            let p = [-0.29, 1.0, -0.37];
            // The missing lid lies exactly on the winding threshold. The fixed
            // arithmetic chooses the inside sign here on every backend.
            assert_eq!(mesh.signed(p).to_bits(), (-0.63f32).to_bits());
            println!("MESH cup threshold regression: distance {:?}, winding {:?}", mesh.signed(p), mesh.winding(p));
        }
        for p in &ps {
            let got = asset.signed(*p);
            assert_eq!(got.to_bits(), mesh.signed(*p).to_bits(), "{name} {p:?}: packed traversal");
            let truth = mesh.signed64(makepad_csg_math::Vec3d::new(p[0] as f64, p[1] as f64, p[2] as f64));
            assert!((got as f64 - truth).abs() < 1e-5, "{name} {p:?}: f64 error {}", (got as f64 - truth).abs());
        }
        let node = SdfNode::Mesh(Arc::new(mesh));
        let mut slots = Vec::new(); kernel::collect(&node, &mut slots).unwrap();
        let prog = program::lower_with(&node, &slots, 1.0).unwrap();
        assert_eq!(prog.texels[1][0], program::op::MESH);
        let many = twin::eval_many(&prog.texels, &ps, &slots);
        for (p, got) in ps.iter().zip(many) {
            assert_eq!(got.0.to_bits(), asset.signed(*p).to_bits(), "{name} batch {p:?}");
            assert_eq!(twin::eval_with(&prog.texels, *p, &slots).0.to_bits(), got.0.to_bits());
        }
        println!("MESH twin {name}: {} points bit-exact signed, signed64 error < 1e-5 mm", ps.len());
    }
}

#[path = "support/eval_program.rs"]
mod eval_program;

#[test]
fn gpu_eval_program_matches_mesh_twin() {
    let _gpu_test = GPU_TEST.lock().unwrap_or_else(|p| p.into_inner());
    eval_program::check(corpus().into_iter().map(|(name, mesh)|
        (name.into(), SdfNode::Mesh(Arc::new(mesh)), points())).collect());
}
