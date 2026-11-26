use super::*;
use makepad_geom3d::math::{quat_axis_angle, transform_point};

/// Assembles a glTF JSON + BIN into GLB bytes, one accessor at a time.
#[derive(Default)]
struct Builder {
    bin: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

impl Builder {
    fn align(&mut self) {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
    }

    fn view(&mut self, bytes: &[u8], stride: Option<usize>) -> usize {
        self.align();
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        let stride = stride.map(|s| format!(",\"byteStride\":{s}")).unwrap_or_default();
        self.views.push(format!("{{\"buffer\":0,\"byteOffset\":{offset},\"byteLength\":{}{stride}}}", bytes.len()));
        self.views.len() - 1
    }

    fn floats(&mut self, data: &[f32], ty: &str) -> usize {
        let comps = accessor::type_comps(ty).unwrap();
        let bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes()).collect();
        let view = self.view(&bytes, None);
        self.accessors.push(format!(
            "{{\"bufferView\":{view},\"componentType\":5126,\"count\":{},\"type\":\"{ty}\"}}",
            data.len() / comps
        ));
        self.accessors.len() - 1
    }

    fn u16s(&mut self, data: &[u16], ty: &str) -> usize {
        let comps = accessor::type_comps(ty).unwrap();
        let bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes()).collect();
        let view = self.view(&bytes, None);
        self.accessors.push(format!(
            "{{\"bufferView\":{view},\"componentType\":5123,\"count\":{},\"type\":\"{ty}\"}}",
            data.len() / comps
        ));
        self.accessors.len() - 1
    }

    fn raw_accessor(&mut self, json: String) -> usize {
        self.accessors.push(json);
        self.accessors.len() - 1
    }

    /// `body` is the rest of the top-level object (nodes, meshes, ...).
    fn glb(mut self, body: &str) -> Vec<u8> {
        self.align();
        let json = format!(
            "{{\"asset\":{{\"version\":\"2.0\"}},\"buffers\":[{{\"byteLength\":{}}}],\"bufferViews\":[{}],\"accessors\":[{}],{body}}}",
            self.bin.len(),
            self.views.join(","),
            self.accessors.join(",")
        );
        let mut json = json.into_bytes();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let total = 12 + 8 + json.len() + 8 + self.bin.len();
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&json);
        out.extend_from_slice(&(self.bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&self.bin);
        out
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|k| close(a[k], b[k]))
}

/// A triangle on a skinned two-joint chain, with a rotation clip (LINEAR)
/// on joint 1 and a translation clip (STEP) on the root joint.
fn skinned() -> Vec<u8> {
    let mut b = Builder::default();
    let pos = b.floats(&[0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 1.0, 2.0, 0.0], "VEC3");
    let idx = b.u16s(&[0, 1, 2], "SCALAR");
    let joints = b.u16s(&[0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], "VEC4");
    let weights = b.floats(&[1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], "VEC4");
    // Joint 1 sits at y = 1; its inverse bind moves it back.
    let ib = b.floats(
        &[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
          1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 1.0],
        "MAT4",
    );
    let times = b.floats(&[0.0, 1.0], "SCALAR");
    let q = quat_axis_angle([0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_2);
    let rots = b.floats(&[0.0, 0.0, 0.0, 1.0, q[0], q[1], q[2], q[3]], "VEC4");
    let moves = b.floats(&[0.0, 0.0, 0.0, 5.0, 0.0, 0.0], "VEC3");
    b.glb(&format!(
        "\"scene\":0,\"scenes\":[{{\"nodes\":[2,0]}}],\
         \"nodes\":[{{\"name\":\"root\",\"children\":[1]}},{{\"name\":\"tip\",\"translation\":[0,1,0]}},\
                    {{\"name\":\"body\",\"mesh\":0,\"skin\":0}}],\
         \"meshes\":[{{\"name\":\"tri\",\"primitives\":[{{\"attributes\":{{\"POSITION\":{pos},\"JOINTS_0\":{joints},\"WEIGHTS_0\":{weights}}},\"indices\":{idx}}}]}}],\
         \"skins\":[{{\"joints\":[0,1],\"inverseBindMatrices\":{ib}}}],\
         \"animations\":[{{\"name\":\"Bend\",\"samplers\":[{{\"input\":{times},\"output\":{rots}}},{{\"input\":{times},\"output\":{moves},\"interpolation\":\"STEP\"}}],\
             \"channels\":[{{\"sampler\":0,\"target\":{{\"node\":1,\"path\":\"rotation\"}}}},{{\"sampler\":1,\"target\":{{\"node\":0,\"path\":\"translation\"}}}}]}}]"
    ))
}

#[test]
fn loads_skinned_model() {
    let m = GlbModel::load(&skinned(), None).unwrap();
    assert_eq!(m.nodes.len(), 3);
    assert_eq!(m.nodes[1].parent, Some(0));
    assert_eq!(m.roots, vec![2, 0]);
    assert_eq!(m.scene_nodes(), vec![(2, 0)]);
    assert_eq!(m.skins[0].joints, vec![0, 1]);
    assert!(close(m.skins[0].inverse_bind[1].v[13], -1.0));
    let p = &m.meshes[0].primitives[0];
    assert_eq!(p.joints[1], [1, 0, 0, 0]);
    // Weights normalised.
    assert_eq!(p.weights[1], [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(p.mesh.normals.len(), 3);
    p.mesh.validate().unwrap();
    assert_eq!(m.clip_by_name("Bend"), Some(0));
    assert!(close(m.clips[0].duration, 1.0));
    assert_eq!(closest_name("bnd", m.clips.iter().map(|c| c.name.as_str())), Some("Bend"));
    assert_eq!(closest_name("zzzzzzzz", ["Bend"]), None);
}

#[test]
fn linear_rotation_slerps_and_step_holds() {
    let m = GlbModel::load(&skinned(), None).unwrap();
    let mut pose = m.rest_pose();
    m.apply_clip(&mut pose, 0, 0.5, 1.0);
    let expect = quat_axis_angle([0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_4);
    assert!((0..4).all(|k| close(pose.r[1][k], expect[k])), "{:?}", pose.r[1]);
    // STEP: still the first key at 0.5.
    assert!(close3(pose.t[0], [0.0, 0.0, 0.0]));
    m.apply_clip(&mut pose, 0, 1.0, 1.0);
    assert!(close3(pose.t[0], [5.0, 0.0, 0.0]));
    // Past the end clamps.
    m.apply_clip(&mut pose, 0, 9.0, 1.0);
    assert!(close3(pose.t[0], [5.0, 0.0, 0.0]));
    // Half-weight blend over the rest pose.
    let mut half = m.rest_pose();
    m.apply_clip(&mut half, 0, 1.0, 0.5);
    assert!(close3(half.t[0], [2.5, 0.0, 0.0]));
}

#[test]
fn skinning_follows_the_joint() {
    let m = GlbModel::load(&skinned(), None).unwrap();
    let rest = m.rest_pose();
    let world = m.world_matrices(&rest);
    let (mesh, space) = m.posed_primitive(2, 0, &rest, &world);
    assert_eq!(space, MeshSpace::World);
    // Rest pose: bind pose, unchanged.
    assert!(close3(mesh.positions[1], [0.0, 2.0, 0.0]), "{:?}", mesh.positions[1]);
    let mut pose = m.rest_pose();
    m.apply_clip(&mut pose, 0, 1.0, 1.0);
    let world = m.world_matrices(&pose);
    let (mesh, _) = m.posed_primitive(2, 0, &pose, &world);
    // Joint 1 turned 90° about z at (5, 1, 0): the vertex one unit above it goes to -x.
    assert!(close3(mesh.positions[1], [4.0, 1.0, 0.0]), "{:?}", mesh.positions[1]);
    // The root vertex just moves with the root.
    assert!(close3(mesh.positions[0], [5.0, 0.0, 0.0]));
    let (min, max) = m.bounds(&pose).unwrap();
    assert!(close3(min, [4.0, 0.0, 0.0]) && close3(max, [5.0, 2.0, 0.0]), "{min:?} {max:?}");
}

/// Morph targets (with names), CUBICSPLINE weights, a sparse accessor, a
/// matrix node, lights, a camera, and nodes listed children-first.
fn morphs() -> Vec<u8> {
    let mut b = Builder::default();
    let pos = b.floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0], "VEC3");
    // Sparse target: only vertex 2 moves, by +z 1.
    let sparse_idx = b.view(&[2u16.to_le_bytes(), [0, 0]].concat(), None);
    let sparse_val = b.view(&[0.0f32, 0.0, 1.0].iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<_>>(), None);
    let target = b.raw_accessor(format!(
        "{{\"componentType\":5126,\"count\":3,\"type\":\"VEC3\",\"sparse\":{{\"count\":1,\"indices\":{{\"bufferView\":{sparse_idx},\"componentType\":5123}},\"values\":{{\"bufferView\":{sparse_val}}}}}}}"
    ));
    let times = b.floats(&[0.0, 2.0], "SCALAR");
    // CUBICSPLINE: (in, value, out) per key, 1 weight each. Out-tangent 1 at key 0.
    let w = b.floats(&[0.0, 0.0, 1.0, 0.0, 1.0, 0.0], "SCALAR");
    b.glb(&format!(
        "\"nodes\":[{{\"name\":\"child\",\"mesh\":0,\"translation\":[0,0,1]}},\
                    {{\"name\":\"parent\",\"children\":[0],\"matrix\":[2,0,0,0, 0,2,0,0, 0,0,2,0, 10,0,0,1],\
                      \"extensions\":{{\"KHR_lights_punctual\":{{\"light\":0}}}}}},\
                    {{\"name\":\"cam\",\"camera\":0}}],\
         \"meshes\":[{{\"name\":\"face\",\"weights\":[0.0],\"extras\":{{\"targetNames\":[\"smile\"]}},\
             \"primitives\":[{{\"attributes\":{{\"POSITION\":{pos}}},\"targets\":[{{\"POSITION\":{target}}}]}}]}}],\
         \"animations\":[{{\"samplers\":[{{\"input\":{times},\"output\":{w},\"interpolation\":\"CUBICSPLINE\"}}],\
             \"channels\":[{{\"sampler\":0,\"target\":{{\"node\":0,\"path\":\"weights\"}}}}]}}],\
         \"cameras\":[{{\"type\":\"perspective\",\"perspective\":{{\"yfov\":0.8,\"znear\":0.1}}}}],\
         \"extensionsUsed\":[\"KHR_lights_punctual\"],\
         \"extensions\":{{\"KHR_lights_punctual\":{{\"lights\":[{{\"type\":\"spot\",\"color\":[1,0.5,0],\"intensity\":20,\"spot\":{{\"outerConeAngle\":0.5}}}}]}}}},\
         \"materials\":[{{\"name\":\"glass\",\"alphaMode\":\"MASK\",\"pbrMetallicRoughness\":{{\"baseColorFactor\":[1,0,0,1],\"roughnessFactor\":0.2}},\
             \"extensions\":{{\"KHR_materials_transmission\":{{\"transmissionFactor\":0.9}},\"KHR_materials_clearcoat\":{{\"clearcoatFactor\":1.0}},\"KHR_materials_emissive_strength\":{{\"emissiveStrength\":4}}}}}}]"
    ))
}

#[test]
fn morph_targets_blend_and_cubic_weights() {
    let m = GlbModel::load(&morphs(), None).unwrap();
    assert_eq!(m.meshes[0].target_names, vec!["smile".to_string()]);
    assert_eq!(m.target_by_name(0, "smile"), Some(0));
    let t = &m.meshes[0].primitives[0].targets[0];
    assert_eq!(t.positions, vec![[0.0; 3], [0.0; 3], [0.0, 0.0, 1.0]]);
    let mut pose = m.rest_pose();
    assert_eq!(pose.weights[0], vec![0.0]);
    // Hermite at s = 0.5 over dt = 2: h00*0 + h10*dt*1 + h01*1 + h11*dt*0
    // = 0.125*2 + 0.5 = 0.75.
    m.apply_clip(&mut pose, 0, 1.0, 1.0);
    assert!(close(pose.weights[0][0], 0.75), "{:?}", pose.weights[0]);
    let world = m.world_matrices(&pose);
    let (mesh, space) = m.posed_primitive(0, 0, &pose, &world);
    assert_eq!(space, MeshSpace::NodeLocal);
    assert!(close3(mesh.positions[2], [0.0, 1.0, 0.75]));
}

#[test]
fn matrix_nodes_reverse_order_lights_cameras_materials() {
    let m = GlbModel::load(&morphs(), None).unwrap();
    // Parent: scale 2 at x 10, decomposed.
    assert!(close3(m.nodes[1].translation, [10.0, 0.0, 0.0]));
    assert!(close3(m.nodes[1].scale, [2.0, 2.0, 2.0]));
    // Child listed first: world = parent * child.
    let world = m.world_matrices(&m.rest_pose());
    assert!(close3(transform_point(&world[0], [0.0, 0.0, 0.0]), [10.0, 0.0, 2.0]));
    // No scene: roots are the parentless nodes.
    assert_eq!(m.roots, vec![1, 2]);
    assert_eq!(m.nodes[1].light, Some(0));
    assert_eq!(m.lights[0].kind, GlbLightKind::Spot { inner: 0.0, outer: 0.5 });
    assert!(close(m.lights[0].intensity, 20.0));
    assert!(matches!(m.cameras[0].projection, GlbProjection::Perspective { yfov, .. } if close(yfov, 0.8)));
    let mat = &m.materials[0];
    assert_eq!(mat.alpha_mode, AlphaMode::Mask(0.5));
    assert!(close(mat.roughness, 0.2) && close(mat.transmission, 0.9) && close(mat.clearcoat, 1.0) && close(mat.emissive_strength, 4.0));
    assert_eq!(m.material_by_name("glass"), Some(0));
    // Bounds include the morph target and the parent's scale.
    let (_, max) = m.bounds(&m.rest_pose()).unwrap();
    assert!(close3(max, [12.0, 2.0, 2.0]), "{max:?}");
}

#[test]
fn normalized_and_strided_accessors() {
    let mut b = Builder::default();
    // Interleaved: u8 normalised pairs at stride 4.
    let view = b.view(&[255, 0, 9, 9, 128, 255, 9, 9], Some(4));
    let a = b.raw_accessor(format!("{{\"bufferView\":{view},\"componentType\":5121,\"normalized\":true,\"count\":2,\"type\":\"VEC2\"}}"));
    let sview = b.view(&(-32767i16).to_le_bytes(), None);
    let s = b.raw_accessor(format!("{{\"bufferView\":{sview},\"componentType\":5122,\"normalized\":true,\"count\":1,\"type\":\"SCALAR\"}}"));
    let bytes = b.glb("\"nodes\":[]");
    let gltf = makepad_gltf::load_gltf_from_bytes(&bytes, None).unwrap();
    let acc = read_accessor(&gltf, a).unwrap();
    assert_eq!(acc.vec2()[0], [1.0, 0.0]);
    assert!(close(acc.vec2()[1][0], 128.0 / 255.0) && close(acc.vec2()[1][1], 1.0));
    assert!(close(read_accessor(&gltf, s).unwrap().get(0, 0), -1.0));
}

#[test]
fn triangle_strips_become_lists() {
    let mut b = Builder::default();
    let pos = b.floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0], "VEC3");
    let bytes = b.glb(&format!(
        "\"nodes\":[{{\"mesh\":0}}],\"meshes\":[{{\"primitives\":[{{\"attributes\":{{\"POSITION\":{pos}}},\"mode\":5}},{{\"attributes\":{{\"POSITION\":{pos}}},\"mode\":1}}]}}]"
    ));
    let m = GlbModel::load(&bytes, None).unwrap();
    assert_eq!(m.meshes[0].primitives.len(), 1);
    assert_eq!(m.meshes[0].primitives[0].mesh.indices, vec![0, 1, 2, 2, 1, 3]);
    assert!(m.warnings.iter().any(|w| w.contains("not triangles")));
    // Both strip triangles face +z.
    assert!(m.meshes[0].primitives[0].mesh.normals.iter().all(|n| n[2] > 0.99));
}
