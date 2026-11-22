//! Source-published materials must survive the scene build: the albedo
//! image and the surface factors stay on the material.

use fab::model::*;

fn textured_quad() -> ModelData {
    let rgba = {
        let mut v = vec![0u8; 8 * 8 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let o = (y * 8 + x) * 4;
                v[o] = if x < 4 { 180 } else { 40 };
                v[o + 1] = 80;
                v[o + 2] = 20;
                v[o + 3] = 255;
            }
        }
        v
    };
    ModelData {
        name: "textured".into(),
        meshes: vec![MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            uvs: vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: Vec::new(),
        }],
        textures: vec![TextureData {
            width: 8,
            height: 8,
            rgba: rgba.into(),
        }],
        materials: vec![
            MaterialData {
                name: "Wood - 20 Vertical DARK".into(),
                base_color: [0.22, 0.22, 0.22, 1.0],
                roughness: 0.7,
                metallic: 0.0,
                texture: Some(0),
                ..Default::default()
            },
            MaterialData {
                name: "Metal: Stainless Steel".into(),
                base_color: [0.84, 0.83, 0.85, 1.0],
                roughness: 0.7,
                metallic: 1.0,
                ..Default::default()
            },
        ],
        elements: vec![ElementData {
            id: ElementId::from_index(0),
            name: "Textured part".into(),
            meshes: vec![MeshRef::with_material(
                MeshId(0),
                Default::default(),
                MaterialId::from_index(0),
            )],
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn published_textures_survive_the_scene_build() {
    let scene = Scene::from_model(textured_quad(), &mut |_| {});
    assert!(scene.textures.len() >= 1);
    let wood = scene
        .materials
        .iter()
        .find(|m| m.name.to_ascii_lowercase().contains("wood"))
        .expect("wood material");
    assert!(wood.texture.is_some());
    let steel = scene
        .materials
        .iter()
        .find(|m| m.name.to_ascii_lowercase().contains("steel"))
        .expect("steel material");
    assert!((steel.metallic - 1.0).abs() < 1e-5);
    assert!((wood.roughness - 0.7).abs() < 1e-5);
}
