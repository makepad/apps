//! The part mesh: triangulation, normals, materials and the `.glb` round trip, on meshes written here.

use fab_elec_import::glb::{read_glb, write_glb, BODY_TO_GLTF};
use fab_elec_import::mesh::{linear_to_srgb, srgb_to_linear};
use fab_elec_import::{Affine, Material, Mesh, MeshBuilder, Normals};

const CREASE: Normals = Normals::Crease { domain: 0, angle: 0.5236 };

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The unit cube's six faces, counter-clockwise from outside; the top face (+Z) is `top`.
fn cube(b: &mut MeshBuilder, side: usize, top: usize) {
    let c = |i: usize| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32];
    for (face, material) in [([0, 2, 3, 1], side), ([4, 5, 7, 6], top), ([0, 1, 5, 4], side), ([2, 6, 7, 3], side), ([0, 4, 6, 2], side), ([1, 3, 7, 5], side)] {
        assert!(b.polygon(material, &face.map(c), CREASE));
    }
}

/// Every triangle faces out of `centre` and agrees with its corners' normals.
fn faces_out(mesh: &Mesh, centre: [f32; 3], what: &str) {
    for prim in &mesh.prims {
        for t in prim.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| prim.positions[t[k] as usize]);
            let n = cross(sub(b, a), sub(c, a));
            assert!(dot(n, sub(a, centre)) > 0.0, "{what}: a triangle faces inwards");
            for k in 0..3 {
                assert!(dot(n, prim.normals[t[k] as usize]) > 0.0, "{what}: a normal disagrees with its triangle");
            }
        }
    }
}

#[test]
fn builder_triangulates_welds_and_shades() {
    assert!((srgb_to_linear(0.5) - 0.21404).abs() < 1e-4, "sRGB mid-grey decodes to linear light");
    let boundary = srgb_to_linear(0.04045);
    assert!((boundary - srgb_to_linear(0.04045 - 1e-7)).abs() < 1e-6
        && (srgb_to_linear(0.04045 + 1e-7) - boundary).abs() < 1e-6,
        "sRGB decoding is continuous across the transfer-function boundary");
    assert_eq!(srgb_to_linear(1.5), 1.5, "sRGB values above one stay unclamped");
    assert_eq!(srgb_to_linear(-0.2), -0.2, "negative sRGB values stay unclamped");
    for v in [0.0, 0.02, 0.04045, 0.5, 1.0] {
        assert!((linear_to_srgb(srgb_to_linear(v)) - v).abs() < 1e-6,
            "sRGB round trip preserves display value {v}");
    }
    assert_eq!(linear_to_srgb(1.5), 1.5, "linear values above one stay unclamped when encoded");
    assert_eq!(linear_to_srgb(-0.2), -0.2, "negative linear values stay unclamped when encoded");

    // Cube: flat at 90 degree edges, two materials, an unused third dropped.
    let mut b = MeshBuilder::new();
    let side = b.add_material(Material { base_color: [0.1, 0.1, 0.1, 1.0], ..Material::named("body") });
    let top = b.add_material(Material { base_color: [0.9, 0.9, 0.9, 1.0], metallic: 2.0, ..Material::named("mark") });
    b.add_material(Material::named("unused"));
    cube(&mut b, side, top);
    let mesh = b.finish();
    assert_eq!(mesh.materials.len(), 2, "materials without faces are dropped");
    assert_eq!(mesh.materials[1].metallic, 1.0, "factors are clamped");
    assert_eq!((mesh.prims.len(), mesh.triangle_count(), mesh.vertex_count()), (2, 12, 24), "a cube's corners split at its edges");
    assert!(mesh.prims.iter().flat_map(|p| &p.normals).all(|n| n.iter().filter(|v| v.abs() == 1.0).count() == 1), "flat cube normals are axis-aligned");
    faces_out(&mesh, [0.5; 3], "cube");
    assert_eq!(mesh.bounds(), Some(([0.0; 3], [1.0; 3])));

    // A mirrored copy keeps its fronts outside.
    let mut mirrored = mesh.clone();
    mirrored.transform(&Affine::scale([-1.0, 1.0, 1.0]));
    faces_out(&mirrored, [-0.5, 0.5, 0.5], "mirrored cube");

    // A 16-sided prism: 22.5 degree walls smooth into one surface, the caps stay sharp.
    let mut b = MeshBuilder::new();
    let ring = |z: f32| -> Vec<[f32; 3]> {
        (0..16).map(|i| (i as f32 * std::f32::consts::TAU / 16.0).sin_cos()).map(|(s, c)| [c, s, z]).collect()
    };
    let (lo, hi) = (ring(0.0), ring(2.0));
    for i in 0..16 {
        let j = (i + 1) % 16;
        assert!(b.polygon(0, &[lo[i], lo[j], hi[j], hi[i]], CREASE));
    }
    assert!(b.polygon(0, &hi, CREASE));
    assert!(b.polygon(0, &lo.iter().rev().copied().collect::<Vec<_>>(), CREASE));
    let prism = b.finish();
    assert_eq!((prism.triangle_count(), prism.vertex_count()), (32 + 2 * 14, 32 + 32), "walls share vertices, caps do not");
    let wall_normals = prism.prims[0].normals.iter().filter(|n| n[2] == 0.0).count();
    assert_eq!(wall_normals, 32, "a wall vertex's normal is horizontal");
    faces_out(&prism, [0.0, 0.0, 1.0], "prism");

    // A concave L in a tilted plane: clipped into four triangles covering its area once.
    let tilt = Affine::rotate([1.0, 1.0, 0.0], 0.7).mul(&Affine::translate([3.0, -2.0, 5.0]));
    let l = [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 2.0, 0.0], [0.0, 2.0, 0.0]].map(|p| tilt.point(p));
    let up = tilt.normal([0.0, 0.0, 1.0]);
    let mut b = MeshBuilder::new();
    assert!(b.polygon(0, &l, Normals::Crease { domain: 0, angle: 0.0 }));
    // Degenerate polygons add nothing and are counted.
    assert!(!b.polygon(0, &[l[0], l[0], l[1]], CREASE), "two distinct corners");
    assert!(!b.polygon(0, &[[0.0; 3], [1.0, 0.0, 0.0], [3.0, 0.0, 0.0]], CREASE), "collinear");
    assert_eq!(b.degenerate, 2);
    let lmesh = b.finish();
    assert_eq!(lmesh.triangle_count(), 4);
    let p = &lmesh.prims[0];
    let area: f32 = p
        .indices
        .chunks_exact(3)
        .map(|t| {
            let [a, b, c] = [0, 1, 2].map(|k| p.positions[t[k] as usize]);
            let n = cross(sub(b, a), sub(c, a));
            assert!(dot(n, up) > 0.0, "a clipped triangle turned over");
            dot(n, up) / 2.0
        })
        .sum();
    assert!((area - 3.0).abs() < 1e-4, "the L's area is covered once: {area}");
}

#[test]
fn glb_round_trips_and_reads_foreign_files() {
    let mut b = MeshBuilder::new();
    let lens = b.add_material(Material { base_color: [0.9, 0.1, 0.1, 0.5], roughness: 0.1, emissive: [0.5, 0.0, 0.0], ..Material::named("lens \"red\"") });
    let pin = b.add_material(Material { base_color: [0.8, 0.8, 0.75, 1.0], metallic: 1.0, roughness: 0.3, ..Material::named("pin") });
    cube(&mut b, pin, lens);
    let mut mesh = b.finish();
    mesh.transform(&Affine::translate([-0.5, -0.5, 0.0]).mul(&Affine::scale([1.6, 0.8, 0.45])));
    let bytes = write_glb(&mesh);

    // Ours comes back bit for bit.
    let back = read_glb(&bytes).expect("our own file reads");
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.mesh, mesh, "write then read is exact");

    // Any glTF reader sees metres, Y up, and the material factors.
    let loaded = makepad_gltf::load_gltf_from_bytes(&bytes, None).expect("valid glTF");
    let node = &loaded.document.nodes_slice()[0];
    assert_eq!(node.matrix, Some(BODY_TO_GLTF));
    let mats = loaded.document.materials_slice();
    assert_eq!(mats.len(), 2);
    let named = |n: &str| mats.iter().find(|m| m.name.as_deref() == Some(n)).expect("material by name");
    let (lens, pin) = (named("lens \"red\""), named("pin"));
    assert_eq!(lens.alpha_mode.as_deref(), Some("BLEND"), "a see-through material blends");
    assert_eq!(lens.emissive_factor, Some([0.5, 0.0, 0.0]));
    assert_eq!(pin.alpha_mode, None);
    assert_eq!(pin.pbr_metallic_roughness.as_ref().and_then(|p| p.metallic_factor), Some(1.0));

    // A foreign file (metres, Y up, no material) lands in the body frame in millimetres.
    let foreign = makepad_gltf::write_glb_mesh(&[[0.0, 0.0, 0.0], [0.002, 0.0, 0.0], [0.0, 0.003, -0.001]], &[0, 1, 2]);
    let read = read_glb(&foreign).expect("foreign file reads");
    let prim = &read.mesh.prims[0];
    let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-4);
    assert!(close(prim.positions[1], [2.0, 0.0, 0.0]) && close(prim.positions[2], [0.0, 1.0, 3.0]), "{:?}", prim.positions);
    assert_eq!(read.mesh.materials, vec![Material::named("default")]);

    // Damage is a refusal, never a panic; an empty mesh survives the trip.
    let err = read_glb(&bytes[..bytes.len() / 2]).expect_err("a truncated file is refused");
    assert_eq!(err.field, "glb");
    assert_eq!(read_glb(&write_glb(&Mesh::default())).expect("empty").mesh, Mesh::default());
}

#[test]
fn cache_parses_each_file_content_once() {
    use fab_elec_import::cache::MeshCache;
    use std::sync::Arc;
    let dir = std::env::temp_dir().join(format!("fab-elec-import-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("C1.glb");
    let mut b = MeshBuilder::new();
    let grey = b.add_material(Material::named("body"));
    cube(&mut b, grey, grey);
    let one = b.finish();
    std::fs::write(&path, write_glb(&one)).unwrap();

    let cache = MeshCache::new();
    let first = cache.get(&path).expect("reads");
    assert_eq!(*first.mesh, one);
    assert!(Arc::ptr_eq(&first.mesh, &cache.get(&path).unwrap().mesh), "an unchanged file is not parsed again");

    // The same bytes written again keep the parsed mesh; other bytes replace it.
    std::fs::write(&path, write_glb(&one)).unwrap();
    assert!(Arc::ptr_eq(&first.mesh, &cache.get(&path).unwrap().mesh), "a rewrite with the same content keeps it");
    // Two materials make a longer file, so the stat sees the change at any clock resolution.
    let mut b = MeshBuilder::new();
    let (side, top) = (b.add_material(Material::named("body")), b.add_material(Material::named("cap")));
    cube(&mut b, side, top);
    let two = b.finish();
    std::fs::write(&path, write_glb(&two)).unwrap();
    let got = cache.get(&path).expect("the new file reads");
    assert!(*got.mesh == two && got.sha256 != first.sha256 && cache.len() == 1, "a changed file reads again and replaces the entry");

    // A broken or missing file is an issue naming the path, and leaves nothing cached.
    std::fs::write(&path, b"not a glb").unwrap();
    let err = cache.get(&path).expect_err("a broken file is refused");
    assert!(err.field.starts_with(&path.display().to_string()), "{err}");
    assert!(cache.is_empty(), "a refused file is not kept");
    std::fs::remove_file(&path).unwrap();
    assert!(cache.get(&path).is_err() && cache.is_empty(), "a deleted file is not served");
    let _ = std::fs::remove_dir_all(&dir);
}
