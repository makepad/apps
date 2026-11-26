//! Scene3D layers: a glTF/GLB model drawn with Makepad's PBR renderer
//! (`DrawPbr` through `makepad_xr`'s `GltfRenderer`) into the layer's own
//! box with depth, from the frame's camera and lights, then placed by the
//! layer's transform. Everything comes from the frame (camera, lights,
//! node transforms, material factors), so a frame is a function of `t`.
//!
//! Node overrides apply about the node's own origin to the node and every
//! node under it; the node named `"*"` is the whole model. Material
//! overrides replace the named material's factors for the frame. DrawPbr
//! has one key light: the first light is it (a point light shines from its
//! position towards the camera's target); the others add to the ambient.

use super::MotionDraw;
use makepad_motion::frame::{CameraFrame, Item, ItemKind};
use makepad_motion::model::Media;
use makepad_gltf::{load_gltf_from_bytes, LoadedGltf};
use makepad_widgets::shader::draw_pbr::DrawPbr;
use makepad_widgets::*;
use makepad_xr::render::{GltfMaterialState, GltfRenderer};
use std::collections::HashMap;

struct Model {
    loaded: Option<LoadedGltf>,
    renderer: Option<GltfRenderer>,
    /// The model's own materials, restored after each frame's overrides.
    materials: Vec<GltfMaterialState>,
    node_names: Vec<Option<String>>,
    /// Children of each node.
    children: Vec<Vec<usize>>,
    material_names: Vec<Option<String>>,
}

#[derive(Default)]
pub(crate) struct Scenes {
    models: HashMap<Media, Model>,
    draw: Option<DrawPbr>,
    lists: Vec<DrawList>,
    errors: Vec<String>,
}

impl Scenes {
    pub fn set_model(&mut self, _cx: &mut Cx, media: Media, glb: &[u8]) -> Result<(), String> {
        let mut loaded = load_gltf_from_bytes(glb, None).map_err(|e| e.to_string())?;
        // A cache identity only: never a file lookup.
        loaded.source_path = Some(std::path::PathBuf::from(format!("mem:motion-{:016x}.glb", LiveId(LiveId::SEED).bytes_append(glb).0)));
        let doc = &loaded.document;
        let nodes = doc.nodes.as_deref().unwrap_or(&[]);
        let node_names = nodes.iter().map(|n| n.name.clone()).collect();
        let children = nodes.iter().map(|n| n.children.clone().unwrap_or_default()).collect();
        let material_names = doc.materials.as_ref().map_or(Vec::new(), |m| m.iter().map(|m| m.name.clone()).collect());
        self.models.insert(media, Model { loaded: Some(loaded), renderer: None, materials: Vec::new(), node_names, children, material_names });
        Ok(())
    }

    pub fn has(&self, media: &Media) -> bool {
        self.models.contains_key(media)
    }

    /// Whether every model's textures have arrived (a frame drawn before
    /// shows untextured surfaces).
    pub fn ready(&self) -> bool {
        self.models.values().all(|m| m.renderer.as_ref().map_or(m.loaded.is_some(), |r| r.textures.iter().all(Option::is_some)))
    }

    pub fn errors(&self) -> Vec<String> {
        self.errors.clone()
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        for model in self.models.values_mut() {
            if let Some(renderer) = &mut model.renderer {
                renderer.handle_event(cx, event);
            }
        }
    }
}

fn nodes_under(children: &[Vec<usize>], root: usize, out: &mut Vec<usize>) {
    if out.contains(&root) || root >= children.len() {
        return;
    }
    out.push(root);
    for &child in &children[root] {
        nodes_under(children, child, out);
    }
}

fn vec3_of(v: [f64; 3]) -> Vec3f {
    vec3f(v[0] as f32, v[1] as f32, v[2] as f32)
}

/// Degrees about x, then y, then z.
fn rotation(r: [f64; 3]) -> Mat4f {
    Mat4f::rotation(vec3f(r[0].to_radians() as f32, r[1].to_radians() as f32, r[2].to_radians() as f32))
}

fn camera_matrices(camera: &CameraFrame, aspect: f64) -> (Mat4f, Mat4f) {
    let view = Mat4f::look_at(vec3_of(camera.eye), vec3_of(camera.target), vec3f(0.0, 1.0, 0.0));
    let projection = Mat4f::perspective(camera.fov as f32, aspect as f32, 0.05, 1000.0);
    (view, projection)
}

/// Draw a Scene3D item into its own target at `scale` pixels per unit.
pub(super) fn render(md: &mut MotionDraw, cx: &mut Cx2d, item: &Item, scale: f64) -> Option<Texture> {
    let ItemKind::Scene3D { model: media, size, camera, lights, nodes, materials, background, exposure, .. } = &item.kind else { return None };
    let px = ((size[0] * scale).ceil().max(1.0) as u32, (size[1] * scale).ceil().max(1.0) as u32);
    if md.scenes.draw.is_none() {
        md.scenes.draw = Some(cx.with_vm(|vm| DrawPbr::script_new_with_default(vm)));
    }
    // Upload the model on first use (needs a draw context).
    {
        let scenes = &mut md.scenes;
        let model = scenes.models.get_mut(media)?;
        if model.renderer.is_none() {
            let loaded = model.loaded.take()?;
            let draw = scenes.draw.as_mut().unwrap();
            match GltfRenderer::from_loaded(draw, cx, &loaded) {
                Ok(renderer) => {
                    model.materials = renderer.materials.clone();
                    model.renderer = Some(renderer);
                }
                Err(error) => {
                    let error = format!("{}: model: {error}", item.path);
                    if !scenes.errors.contains(&error) {
                        scenes.errors.push(error);
                    }
                    return None;
                }
            }
        }
    }
    md.note_kind(&format!("scene {}", item.path));
    let index = md.target(cx, px, "motion_scene3d");
    // The depth buffer belongs to the pooled target (sized with it, freed
    // with it).
    let depth = md.pool[index].depth(cx);
    let clear = background.map_or(vec4(0.0, 0.0, 0.0, 0.0), |c| vec4((c[0] * c[3]) as f32, (c[1] * c[3]) as f32, (c[2] * c[3]) as f32, c[3] as f32));
    let lists_needed = index + 1;
    while md.scenes.lists.len() < lists_needed {
        md.scenes.lists.push(DrawList::new(cx));
    }
    let list_index = lists_needed - 1;
    let target = &mut md.pool[index];
    let pixels = dvec2(px.0 as f64, px.1 as f64);
    target.pass.set_size(cx, pixels);
    target.pass.set_color_texture(cx, &target.texture, DrawPassClearColor::ClearWith(clear));
    target.pass.set_depth_texture(cx, &depth, DrawPassClearDepth::ClearWith(1.0));
    target.pass.set_keep_camera_matrix(cx, true);
    cx.make_child_pass(&target.pass);
    cx.begin_pass(&target.pass, Some(1.0));
    // The pass camera, from the frame.
    let (view, projection) = camera_matrices(camera, size[0] / size[1].max(1e-9));
    target.pass.set_camera(cx.cx, view, projection);
    let scenes = &mut md.scenes;
    let draw = scenes.draw.as_mut().unwrap();
    let model = scenes.models.get_mut(media).unwrap();
    let renderer = model.renderer.as_mut().unwrap();
    // Lights.
    let exposure = exposure.max(0.0) as f32;
    match lights.first() {
        Some(light) => {
            let dir = if light.point {
                let p = light.position;
                let t = camera.target;
                [t[0] - p[0], t[1] - p[1], t[2] - p[2]]
            } else {
                light.direction
            };
            // DrawPbr's light_dir points towards the light.
            draw.light_dir = vec3_of([-dir[0], -dir[1], -dir[2]]).normalize();
            let k = (light.intensity * light.color[3]) as f32 * exposure;
            draw.light_color = vec3f(light.color[0] as f32 * k, light.color[1] as f32 * k, light.color[2] as f32 * k);
        }
        None => {
            draw.light_dir = vec3f(-0.55, 0.75, 0.55).normalize();
            draw.light_color = vec3f(1.35, 1.32, 1.27) * exposure;
        }
    }
    let extra: f64 = lights.iter().skip(1).map(|l| l.intensity * l.color[3]).sum();
    draw.ambient = (0.3 + 0.15 * extra as f32) * exposure;
    draw.env_intensity = 1.9 * exposure;
    draw.reset_matrix();
    // Material factors for this frame.
    renderer.materials = model.materials.clone();
    for m in materials {
        let Some(k) = model.material_names.iter().position(|n| n.as_deref() == Some(m.name.as_str())) else { continue };
        let Some(state) = renderer.materials.get_mut(k) else { continue };
        if let Some(c) = m.base_color {
            state.base_color_factor = vec4(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32);
        }
        if let Some(v) = m.metallic {
            state.metallic_factor = v as f32;
        }
        if let Some(v) = m.roughness {
            state.roughness_factor = v as f32;
        }
        if let Some(c) = m.emissive {
            state.emissive_factor = vec3f((c[0] * c[3]) as f32, (c[1] * c[3]) as f32, (c[2] * c[3]) as f32);
        }
    }
    // The whole model's transform, then each overridden node's objects.
    let whole = nodes.iter().find(|n| n.name == "*");
    let node_transform = |n: &makepad_motion::frame::NodeFrame, pivot: Vec3f| -> Mat4f {
        let s = n.scale;
        let scale = Mat4f { v: [s[0] as f32, 0.0, 0.0, 0.0, 0.0, s[1] as f32, 0.0, 0.0, 0.0, 0.0, s[2] as f32, 0.0, 0.0, 0.0, 0.0, 1.0] };
        let local = Mat4f::mul(&rotation(n.rotate), &scale);
        Mat4f::mul(&Mat4f::translation(pivot + vec3_of(n.translate)), &Mat4f::mul(&local, &Mat4f::translation(pivot * -1.0)))
    };
    let model_t = whole.map_or(Mat4f::identity(), |n| node_transform(n, vec3f(0.0, 0.0, 0.0)));
    let all_objects = std::mem::take(&mut renderer.draw_objects);
    let mut claimed = vec![false; all_objects.len()];
    let mut groups: Vec<(Mat4f, Vec<usize>)> = Vec::new();
    for n in nodes.iter().filter(|n| n.name != "*") {
        let Some(root) = model.node_names.iter().position(|name| name.as_deref() == Some(n.name.as_str())) else {
            let error = format!("{}: the model has no node \"{}\"", item.path, n.name);
            if !scenes.errors.contains(&error) {
                scenes.errors.push(error);
            }
            continue;
        };
        let mut under = Vec::new();
        nodes_under(&model.children, root, &mut under);
        let members: Vec<usize> = (0..all_objects.len()).filter(|&i| !claimed[i] && under.contains(&all_objects[i].node_index)).collect();
        for &i in &members {
            claimed[i] = true;
        }
        if !n.visible {
            continue;
        }
        let pivot = members.first().map_or(vec3f(0.0, 0.0, 0.0), |&i| {
            let w = &all_objects[i].world_transform;
            vec3f(w.v[12], w.v[13], w.v[14])
        });
        groups.push((Mat4f::mul(&model_t, &node_transform(n, pivot)), members));
    }
    let rest: Vec<usize> = (0..all_objects.len()).filter(|&i| !claimed[i]).collect();
    if whole.is_none_or(|n| n.visible) {
        groups.push((model_t, rest));
    }
    let list = &mut scenes.lists[list_index];
    {
        let cx3d = &mut Cx3d::new(cx.cx);
        list.begin_always(cx3d);
        for (transform, members) in &groups {
            renderer.draw_objects = members.iter().map(|&i| all_objects[i].clone()).collect();
            if let Err(error) = renderer.draw_with_transform(draw, cx3d, *transform) {
                let error = format!("{}: draw: {error}", item.path);
                if !scenes.errors.contains(&error) {
                    scenes.errors.push(error);
                }
            }
        }
        list.end(cx3d);
    }
    renderer.draw_objects = all_objects;
    let target = &mut md.pool[index];
    cx.end_pass(&target.pass);
    let id = target.pass.draw_pass_id();
    md.record(id);
    Some(md.pool[index].texture.clone())
}
