//! The part mesh as glTF 2.0 binary (`.glb`): the one mesh format the runtime reads.
//!
//! What [`write_glb`] writes: one node named `part` holding one mesh with one primitive per
//! [`Prim`] (POSITION with its bounds, NORMAL, u32 indices, TRIANGLES) and one material per
//! [`Material`] (baseColorFactor, metallicFactor, roughnessFactor, emissiveFactor; alphaMode BLEND
//! under alpha 1; doubleSided, since vendor meshes are often open or wound both ways). The vertex
//! data stays in the part's body frame (millimetres, Z up); the node's matrix is [`BODY_TO_GLTF`],
//! which takes it to glTF's metres and Y up, so any glTF viewer, Fab's model loader included, shows
//! the part at its true size and upright.
//!
//! What [`read_glb`] reads: every mesh node reachable from the default scene, each with its node
//! transforms, back in the body frame. A file of ours (its mesh nodes' world matrix exactly
//! [`BODY_TO_GLTF`]) comes back bit for bit; any other glTF is taken as metres, Y up. Textures,
//! vertex colours and non-triangle primitives are left out with a warning; external buffers and
//! out-of-range or non-finite data are refused.

use crate::mesh::{Affine, Material, Mesh, MeshBuilder, Normals, Prim};
use crate::{Issue, MeshRead};
use makepad_gltf::{decode_mesh_primitive, load_gltf_from_bytes, GltfNode, GLTF_MODE_TRIANGLES};

/// Body frame (mm, Z up) to glTF (m, Y up), column-major: x stays, body Y goes to -Z, body Z to Y.
pub const BODY_TO_GLTF: [f32; 16] = [0.001, 0.0, 0.0, 0.0, 0.0, 0.0, -0.001, 0.0, 0.0, 0.001, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// glTF (m, Y up) to the body frame (mm, Z up): the inverse of [`BODY_TO_GLTF`].
const GLTF_TO_BODY: Affine = Affine { m: [[1000.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1000.0, 0.0], [0.0, 1000.0, 0.0, 0.0]] };

/// Writes `mesh` (body frame) as a `.glb`. Never fails: a primitive whose indices leave its
/// vertices, or with no triangles, is left out (the builder and the readers never make one).
pub fn write_glb(mesh: &Mesh) -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    let mut views: Vec<String> = Vec::new();
    let mut accessors: Vec<String> = Vec::new();
    let mut prims: Vec<String> = Vec::new();
    let mut view = |bin: &mut Vec<u8>, bytes: &[u8], target: u32| {
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        views.push(format!("{{\"buffer\":0,\"byteOffset\":{},\"byteLength\":{},\"target\":{target}}}", bin.len(), bytes.len()));
        bin.extend_from_slice(bytes);
        views.len() - 1
    };
    for prim in &mesh.prims {
        let n = prim.positions.len();
        if prim.indices.len() < 3
            || prim.indices.len() % 3 != 0
            || prim.normals.len() != n
            || prim.indices.iter().any(|&i| i as usize >= n)
        {
            continue;
        }
        let bytes: Vec<u8> = prim.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let v = view(&mut bin, &bytes, 34963);
        accessors.push(format!("{{\"bufferView\":{v},\"componentType\":5125,\"count\":{},\"type\":\"SCALAR\"}}", prim.indices.len()));
        let ia = accessors.len() - 1;

        let (lo, hi) = prim.positions.iter().fold(([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]), |(lo, hi), p| {
            ([0, 1, 2].map(|i| lo[i].min(p[i])), [0, 1, 2].map(|i| hi[i].max(p[i])))
        });
        let bytes: Vec<u8> = prim.positions.iter().flatten().flat_map(|c| c.to_le_bytes()).collect();
        let v = view(&mut bin, &bytes, 34962);
        accessors.push(format!(
            "{{\"bufferView\":{v},\"componentType\":5126,\"count\":{n},\"type\":\"VEC3\",\"min\":[{}],\"max\":[{}]}}",
            list(&lo),
            list(&hi)
        ));
        let pa = accessors.len() - 1;

        let bytes: Vec<u8> = prim.normals.iter().flatten().flat_map(|c| c.to_le_bytes()).collect();
        let v = view(&mut bin, &bytes, 34962);
        accessors.push(format!("{{\"bufferView\":{v},\"componentType\":5126,\"count\":{n},\"type\":\"VEC3\"}}"));
        let na = accessors.len() - 1;

        let material = if prim.material < mesh.materials.len() { format!(",\"material\":{}", prim.material) } else { String::new() };
        prims.push(format!("{{\"attributes\":{{\"POSITION\":{pa},\"NORMAL\":{na}}},\"indices\":{ia}{material},\"mode\":4}}"));
    }
    let materials: Vec<String> = mesh
        .materials
        .iter()
        .map(|m| {
            let m = m.clone().clamped();
            let blend = if m.base_color[3] < 1.0 { ",\"alphaMode\":\"BLEND\"" } else { "" };
            format!(
                "{{\"name\":\"{}\",\"pbrMetallicRoughness\":{{\"baseColorFactor\":[{}],\"metallicFactor\":{},\"roughnessFactor\":{}}},\"emissiveFactor\":[{}]{blend},\"doubleSided\":true}}",
                escape(&m.name),
                list(&m.base_color),
                num(m.metallic),
                num(m.roughness),
                list(&m.emissive)
            )
        })
        .collect();
    let mesh_ref = if prims.is_empty() { String::new() } else { ",\"mesh\":0".to_string() };
    let mut json = format!(
        "{{\"asset\":{{\"version\":\"2.0\",\"generator\":\"fab-elec-import\"}},\"scene\":0,\"scenes\":[{{\"nodes\":[0]}}],\"nodes\":[{{\"name\":\"part\"{mesh_ref},\"matrix\":[{}]}}]",
        list(&BODY_TO_GLTF)
    );
    if !prims.is_empty() {
        json.push_str(&format!(",\"meshes\":[{{\"name\":\"part\",\"primitives\":[{}]}}]", prims.join(",")));
    }
    if !materials.is_empty() {
        json.push_str(&format!(",\"materials\":[{}]", materials.join(",")));
    }
    if !bin.is_empty() {
        json.push_str(&format!(
            ",\"accessors\":[{}],\"bufferViews\":[{}],\"buffers\":[{{\"byteLength\":{}}}]",
            accessors.join(","),
            views.join(","),
            bin.len()
        ));
    }
    json.push('}');
    makepad_gltf::assemble_glb_with_rest(&json, (!bin.is_empty()).then_some(&bin[..]), &[])
}

/// Reads a `.glb` (or a glTF JSON with its buffers embedded) into the body frame; see the module
/// documentation for what is kept, warned about and refused.
pub fn read_glb(bytes: &[u8]) -> Result<MeshRead, Issue> {
    let loaded = load_gltf_from_bytes(bytes, None).map_err(|e| Issue::new(0, "glb", e.to_string()))?;
    let doc = &loaded.document;
    let nodes = doc.nodes_slice();
    let mut warnings = Vec::new();
    let roots: Vec<usize> = match doc.scenes_slice().get(doc.scene.unwrap_or(0)) {
        Some(scene) => scene.nodes.clone().unwrap_or_default(),
        None => {
            let mut child = vec![false; nodes.len()];
            for n in nodes {
                for &c in n.children.iter().flatten() {
                    if c < child.len() {
                        child[c] = true;
                    }
                }
            }
            (0..nodes.len()).filter(|&i| !child[i]).collect()
        }
    };

    let mut mesh = Mesh::default();
    let mut material_of: Vec<Option<usize>> = vec![None; doc.materials_slice().len()];
    let mut default_material: Option<usize> = None;
    let mut seen = vec![false; nodes.len()];
    let mut stack: Vec<(usize, [[f64; 4]; 4])> = roots.iter().rev().map(|&r| (r, IDENTITY4)).collect();
    while let Some((ni, parent)) = stack.pop() {
        let Some(node) = nodes.get(ni) else {
            return Err(Issue::new(0, format!("node {ni}"), "is not in the file"));
        };
        if std::mem::replace(&mut seen[ni], true) {
            continue;
        }
        let world = mul4(&parent, &node_matrix(node));
        for &c in node.children.iter().flatten().rev() {
            stack.push((c, world));
        }
        let Some(mi) = node.mesh else { continue };
        let Some(gmesh) = doc.meshes_slice().get(mi) else {
            return Err(Issue::new(0, format!("node {ni} mesh"), format!("mesh {mi} is not in the file")));
        };
        let world32: [f32; 16] = std::array::from_fn(|i| world[i / 4][i % 4] as f32);
        let xf = if world32 == BODY_TO_GLTF {
            None
        } else {
            let a = Affine { m: [0, 1, 2].map(|r| [world[0][r], world[1][r], world[2][r], world[3][r]]) };
            Some(GLTF_TO_BODY.mul(&a))
        };
        for pi in 0..gmesh.primitives.len() {
            let at = format!("mesh {mi} primitive {pi}");
            if gmesh.primitives[pi].mode() != GLTF_MODE_TRIANGLES {
                warnings.push(Issue::new(0, at, "is not triangles; left out"));
                continue;
            }
            let d = decode_mesh_primitive(&loaded, mi, pi).map_err(|e| Issue::new(0, at.clone(), e.to_string()))?;
            if d.positions.iter().flatten().any(|v| !v.is_finite()) {
                return Err(Issue::new(0, format!("{at} POSITION"), "has a non-finite value"));
            }
            if d.indices.len() % 3 != 0 || d.indices.iter().any(|&i| i as usize >= d.positions.len()) {
                return Err(Issue::new(0, format!("{at} indices"), "are not whole triangles within the vertices"));
            }
            if d.colors0.is_some() {
                warnings.push(Issue::new(0, format!("{at} COLOR_0"), "vertex colours are left out; the material's colour is kept"));
            }
            let material = match d.material {
                Some(gi) => match material_of.get(gi) {
                    Some(Some(m)) => *m,
                    Some(None) => {
                        let gm = &doc.materials_slice()[gi];
                        let pbr = gm.pbr_metallic_roughness.as_ref();
                        if pbr.is_some_and(|p| p.base_color_texture.is_some() || p.metallic_roughness_texture.is_some())
                            || gm.emissive_texture.is_some()
                            || gm.normal_texture.is_some()
                        {
                            warnings.push(Issue::new(0, format!("material {gi}"), "textures are left out; its factors are kept"));
                        }
                        let mut m = Material::named(gm.name.clone().unwrap_or_else(|| format!("material {gi}")));
                        if let Some(c) = pbr.and_then(|p| p.base_color_factor) {
                            m.base_color = c;
                        }
                        // glTF's defaults when a factor is absent: fully metallic, fully rough.
                        m.metallic = pbr.and_then(|p| p.metallic_factor).unwrap_or(1.0);
                        m.roughness = pbr.and_then(|p| p.roughness_factor).unwrap_or(1.0);
                        m.emissive = gm.emissive_factor.unwrap_or([0.0; 3]);
                        mesh.materials.push(m.clamped());
                        material_of[gi] = Some(mesh.materials.len() - 1);
                        mesh.materials.len() - 1
                    }
                    None => return Err(Issue::new(0, format!("{at} material"), format!("material {gi} is not in the file"))),
                },
                None => *default_material.get_or_insert_with(|| {
                    mesh.materials.push(Material::named("default"));
                    mesh.materials.len() - 1
                }),
            };
            let mut prim = match d.normals {
                Some(normals) => Prim { material, positions: d.positions, normals, indices: d.indices },
                None => {
                    // glTF without normals means flat faces.
                    let mut b = MeshBuilder::new();
                    b.add_material(Material::named(""));
                    for t in d.indices.chunks_exact(3) {
                        b.polygon(0, &[0, 1, 2].map(|k| d.positions[t[k] as usize]), Normals::Crease { domain: 0, angle: 0.0 });
                    }
                    let flat = b.finish().prims.pop().unwrap_or_default();
                    Prim { material, ..flat }
                }
            };
            if let Some(xf) = &xf {
                prim.transform(xf);
            }
            mesh.prims.push(prim);
        }
    }
    Ok(MeshRead { mesh, warnings })
}

const IDENTITY4: [[f64; 4]; 4] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// A node's local matrix, column-major as `[column][row]`.
fn node_matrix(node: &GltfNode) -> [[f64; 4]; 4] {
    if let Some(m) = node.matrix {
        return std::array::from_fn(|c| std::array::from_fn(|r| m[c * 4 + r] as f64));
    }
    let t = node.translation.unwrap_or([0.0; 3]).map(|v| v as f64);
    let [x, y, z, w] = node.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]).map(|v| v as f64);
    let s = node.scale.unwrap_or([1.0; 3]).map(|v| v as f64);
    let r = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w)],
        [2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w)],
        [2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    [
        [r[0][0] * s[0], r[0][1] * s[0], r[0][2] * s[0], 0.0],
        [r[1][0] * s[1], r[1][1] * s[1], r[1][2] * s[1], 0.0],
        [r[2][0] * s[2], r[2][1] * s[2], r[2][2] * s[2], 0.0],
        [t[0], t[1], t[2], 1.0],
    ]
}

fn mul4(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|c| std::array::from_fn(|r| (0..4).map(|k| a[k][r] * b[c][k]).sum()))
}

fn num(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        s + ".0"
    }
}

fn list(vals: &[f32]) -> String {
    vals.iter().map(|&v| num(v)).collect::<Vec<_>>().join(",")
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}
