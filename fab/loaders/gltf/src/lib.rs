//! glTF/GLB reference loader for the editable Fab document seam.

use fab::document::{
    Document, DocumentBuilder, MaterialId, Mesh, MeshInstance, Object, ObjectId, PbrMaterial,
    Texture, TextureId, TextureSlot, Transform,
};
use fab::model::makepad_math::Mat4f;
use fab::model::{Handedness, LoadCancel, LoadError, LoadProgress, Loader, Units, UpAxis};
use makepad_gltf::{GltfNode, JsonValue, LoadedGltf, GLTF_MODE_TRIANGLES};
use makepad_raytrace::SceneInput;
use std::path::Path;

#[derive(Default)]
pub struct GltfLoader;

impl Loader for GltfLoader {
    fn extensions(&self) -> &[&str] {
        &["glb", "gltf"]
    }

    fn probe(&self, bytes: &[u8]) -> bool {
        makepad_gltf::is_glb_bytes(bytes)
            || std::str::from_utf8(bytes)
                .ok()
                .map(str::trim_start)
                .is_some_and(|text| text.starts_with('{') && text.contains("\"asset\""))
    }

    fn load_cancellable(
        &self,
        path: &Path,
        progress: &mut dyn FnMut(LoadProgress),
        cancel: LoadCancel,
    ) -> Result<Document, LoadError> {
        progress(LoadProgress::Opening);
        if cancel() {
            return Err(LoadError::Cancelled);
        }
        progress(LoadProgress::Parsing(0.1));
        let traced = makepad_raytrace::glb::load_glb(path)
            .map_err(|error| LoadError::Corrupt(error.to_string()))?;
        let loaded = makepad_gltf::load_gltf_from_path(path)
            .map_err(|error| LoadError::Corrupt(format!("{error:?}")))?;
        if cancel() {
            return Err(LoadError::Cancelled);
        }
        progress(LoadProgress::Meshing {
            done: loaded.document.meshes_slice().len(),
            total: loaded.document.meshes_slice().len(),
        });
        let document = build_document(path, traced, &loaded)?;
        progress(LoadProgress::Done);
        Ok(document)
    }
}

fn build_document(path: &Path, traced: SceneInput, loaded: &LoadedGltf) -> Result<Document, LoadError> {
    let name = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Model".to_string());
    let mut builder = DocumentBuilder::new(name.clone());
    builder
        .source_path(path)
        .coordinates(Units::default(), UpAxis::Y, Handedness::Right);

    let texture_ids: Vec<TextureId> = traced
        .images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            builder.add_texture(Texture {
                name: format!("Image {}", index + 1),
                width: image.width as u32,
                height: image.height as u32,
                rgba8: rgba8(image).into(),
                ..Default::default()
            })
        })
        .collect();

    let material_ids: Vec<MaterialId> = traced
        .materials
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let source_doc = loaded.document.materials_slice().get(index);
            let mut material = PbrMaterial::new(
                MaterialId::default(),
                source_doc
                    .and_then(|material| material.name.clone())
                    .unwrap_or_else(|| format!("Material {}", index + 1)),
            );
            material.base_color = source_doc
                .and_then(|material| material.pbr_metallic_roughness.as_ref())
                .and_then(|pbr| pbr.base_color_factor)
                .unwrap_or([source.albedo[0], source.albedo[1], source.albedo[2], 1.0]);
            material.metallic = source.metal;
            material.roughness = source.roughness;
            material.emissive = source.emission;
            material.ior = source.ior;
            material.transmission = source.transmission;
            material.double_sided = source.two_sided;
            material.base_color_texture = source
                .texture
                .and_then(|slot| texture_ids.get(slot).copied())
                .map(TextureSlot::new);
            if let Some(extensions) = source_doc.and_then(|material| material.extensions.as_ref()) {
                if let Some(value) = nested_number(
                    extensions,
                    &["KHR_materials_transmission", "transmissionFactor"],
                ) {
                    material.transmission = value as f32;
                }
                if let Some(value) = nested_number(extensions, &["KHR_materials_ior", "ior"]) {
                    material.ior = value as f32;
                }
            }
            if material.transmission > 0.0 {
                // A transmissive surface is seen from both sides even when
                // the source omits doubleSided.
                material.double_sided = true;
            }
            builder.add_material(material)
        })
        .collect();

    let mut source_meshes: Vec<Vec<fab::document::MeshId>> = Vec::new();
    for (mesh_index, source_mesh) in loaded.document.meshes_slice().iter().enumerate() {
        let mut primitives = Vec::new();
        for (primitive_index, source_primitive) in source_mesh.primitives.iter().enumerate() {
            if source_primitive.mode() != GLTF_MODE_TRIANGLES {
                continue;
            }
            let primitive = makepad_gltf::decode_mesh_primitive(loaded, mesh_index, primitive_index)
                .map_err(|error| LoadError::Corrupt(format!("{error:?}")))?;
            let material = primitive
                .material
                .and_then(|index| material_ids.get(index).copied())
                .or_else(|| material_ids.first().copied());
            primitives.push(builder.add_mesh(Mesh {
                name: source_mesh
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Mesh {}.{}", mesh_index + 1, primitive_index + 1)),
                positions: primitive.positions.into(),
                normals: primitive.normals.unwrap_or_default().into(),
                uvs: primitive.texcoords0.unwrap_or_default().into(),
                colours: primitive.colors0.unwrap_or_default(),
                indices: primitive.indices.into(),
                material_slots: material.into_iter().collect(),
                face_material_slots: Vec::new(),
                tags: Vec::new(),
                properties: Default::default(),
                ..Default::default()
            }));
        }
        source_meshes.push(primitives);
    }

    let nodes = loaded.document.nodes_slice();
    if nodes.is_empty() {
        let mesh = builder.add_mesh(flattened_mesh(&traced, &material_ids));
        let mut object = Object::new(ObjectId::default(), name);
        object.meshes.push(MeshInstance::new(mesh));
        builder.add_object(object);
        return Ok(builder.finish());
    }

    let parents = node_parents(nodes);
    let worlds = node_worlds(nodes, &parents);
    for (index, node) in nodes.iter().enumerate() {
        let node_name = node
            .name
            .clone()
            .unwrap_or_else(|| format!("Object {}", index + 1));
        let mut object = Object::new(ObjectId::default(), node_name.clone());
        object.parent = parents[index].map(|parent| ObjectId(parent as u64 + 1));
        object.transform = Transform { matrix: worlds[index] };
        if let Some(mesh_index) = node.mesh {
            if let Some(meshes) = source_meshes.get(mesh_index) {
                object.meshes.extend(meshes.iter().copied().map(MeshInstance::new));
            }
        }
        let id = builder.add_object(object);
        debug_assert_eq!(id, ObjectId(index as u64 + 1));
    }
    Ok(builder.finish())
}

fn flattened_mesh(scene: &SceneInput, materials: &[MaterialId]) -> Mesh {
    Mesh {
        name: "Scene".to_string(),
        positions: scene.positions.clone().into(),
        normals: scene.normals.clone().into(),
        uvs: scene.uvs.clone().into(),
        indices: scene.indices.clone().into(),
        material_slots: materials.to_vec(),
        face_material_slots: scene.tri_material.clone(),
        ..Default::default()
    }
}

fn rgba8(image: &makepad_raytrace::Image) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(image.data.len() * 4);
    for pixel in &image.data {
        rgba.extend_from_slice(&[
            (pixel >> 16) as u8,
            (pixel >> 8) as u8,
            *pixel as u8,
            (pixel >> 24) as u8,
        ]);
    }
    rgba
}

fn node_parents(nodes: &[GltfNode]) -> Vec<Option<usize>> {
    let mut parents = vec![None; nodes.len()];
    for (parent, node) in nodes.iter().enumerate() {
        for child in node.children.iter().flatten() {
            if let Some(slot) = parents.get_mut(*child) {
                *slot = Some(parent);
            }
        }
    }
    parents
}

fn node_worlds(nodes: &[GltfNode], parents: &[Option<usize>]) -> Vec<Mat4f> {
    fn visit(
        index: usize,
        nodes: &[GltfNode],
        parents: &[Option<usize>],
        worlds: &mut [Option<Mat4f>],
        visiting: &mut [bool],
    ) -> Mat4f {
        if let Some(world) = worlds[index] {
            return world;
        }
        if visiting[index] {
            return node_matrix(&nodes[index]);
        }
        visiting[index] = true;
        let local = node_matrix(&nodes[index]);
        let world = parents[index]
            .map(|parent| Mat4f::mul(&visit(parent, nodes, parents, worlds, visiting), &local))
            .unwrap_or(local);
        visiting[index] = false;
        worlds[index] = Some(world);
        world
    }
    let mut worlds = vec![None; nodes.len()];
    let mut visiting = vec![false; nodes.len()];
    for index in 0..nodes.len() {
        visit(index, nodes, parents, &mut worlds, &mut visiting);
    }
    worlds.into_iter().map(Option::unwrap).collect()
}

fn node_matrix(node: &GltfNode) -> Mat4f {
    if let Some(matrix) = node.matrix {
        return Mat4f { v: matrix };
    }
    let translation = node.translation.unwrap_or([0.0; 3]);
    let rotation = node.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let scale = node.scale.unwrap_or([1.0; 3]);
    let (x, y, z, w) = (rotation[0], rotation[1], rotation[2], rotation[3]);
    Mat4f {
        v: [
            (1.0 - 2.0 * (y * y + z * z)) * scale[0],
            (2.0 * (x * y + z * w)) * scale[0],
            (2.0 * (x * z - y * w)) * scale[0],
            0.0,
            (2.0 * (x * y - z * w)) * scale[1],
            (1.0 - 2.0 * (x * x + z * z)) * scale[1],
            (2.0 * (y * z + x * w)) * scale[1],
            0.0,
            (2.0 * (x * z + y * w)) * scale[2],
            (2.0 * (y * z - x * w)) * scale[2],
            (1.0 - 2.0 * (x * x + y * y)) * scale[2],
            0.0,
            translation[0],
            translation[1],
            translation[2],
            1.0,
        ],
    }
}

fn nested<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    path.iter().try_fold(value, |value, key| value.key(key))
}

fn nested_number(value: &JsonValue, path: &[&str]) -> Option<f64> {
    nested(value, path).and_then(json_number)
}

fn json_number(value: &JsonValue) -> Option<f64> {
    match value {
        JsonValue::U64(value) => Some(*value as f64),
        JsonValue::U128(value) => Some(*value as f64),
        JsonValue::I64(value) => Some(*value as f64),
        JsonValue::I128(value) => Some(*value as f64),
        JsonValue::F64(value) => Some(*value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_glb_as_an_editable_document() {
        let bytes = makepad_gltf::write_glb_mesh(
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            &[0, 1, 2],
        );
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(5)
            .expect("loader crate is inside the workspace");
        let dir = repo_root.join("target/loader-tests");
        std::fs::create_dir_all(&dir).expect("create loader test directory");
        let path = dir.join(format!("triangle-{}.glb", std::process::id()));
        std::fs::write(&path, bytes).expect("write generated GLB");

        let document = GltfLoader.load(&path, &mut |_| {}).expect("generated GLB loads");
        let _ = std::fs::remove_file(&path);
        assert_eq!(document.meshes().len(), 1);
        assert_eq!(document.meshes()[0].indices.as_ref(), [0, 1, 2]);
        assert!(!document.can_undo(), "loader construction is one clean transaction");

        let scene = fab::model::Scene::from_document(document, &mut |_| {});
        assert_eq!(scene.stats.triangles, 1);
        assert_eq!(scene.stats.elements, 1);
    }
}
