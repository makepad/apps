//! Translucent mesh faces over the finished opaque picture.
//!
//! Register after the shared lighting and display modules, before the viewport widget DSL.
//! The caller owns the target, excludes these parts from opaque depth, and supplies face groups
//! in far-to-near order. `GlassLayer` retains the plate's mesh groups between recordings;
//! the low-level draw itself neither gathers parts nor sorts them.

use super::{fab_placeholder_geom, fab_vertex_pod, render_from_fab, set_texture_named};
use super::part_glow::scene_pose_under;
use crate::api::{AppState, ElementId, PaneId, PanePoses, FILAMENT_ROUGHNESS};
use crate::model::aabb_transform;
use fab_sdf_tiles::draw::TileLight;
use makepad_render::Renderer;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    // The aux stream's exact POD and shared placeholder, independently of widget registration.
    mod.fab_glass_geom = {
        FabVertex: #(fab_vertex_pod(vm))
        FabGeom: #(fab_placeholder_geom(vm))
    }

    mod.draw.DrawFabGlass = mod.std.set_type_default() do #(DrawFabGlass::script_shader(vm)){
        ..mod.draw.SdfLit,
        ..mod.draw.ToneCurves,
        alpha_blend: true
        blend_op: @Over
        depth_write: false
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_glass_geom.FabVertex, mod.fab_glass_geom.FabGeom)
        opaque: texture_2d(float)
        world_from_obj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        normal_from_obj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        light_from_world: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        // x = element id in the batch, y = -1 for back faces, +1 for front faces.
        group_ctl: uniform(vec2(0.0, 1.0))
        // Linear body tint and transmitted share; normal-incidence reflectance and roughness.
        tint_transmission: uniform(vec4(1.0, 1.0, 1.0, 1.0))
        reflectance_roughness: uniform(vec4(0.04, 0.04, 0.04, 0.2))
        // The composite's display controls. Its HDR conversion follows the tile light's
        // exposure/to_display, which deliberately leaves HDR scene light unencoded.
        u_exposure: uniform(vec4(1.0, 0.0, 1.0, 0.0))
        u_balance: uniform(vec3(1.0, 1.0, 1.0))
        hdr_to_display: mod.draw.FabDisplay.display
        // x = section count; y = half-pixel world width; z = perspective.
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip_cut: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_obj: varying(vec3f)
        v_world: varying(vec3f)
        v_view: varying(vec3f)
        v_clip: varying(vec4f)
        v_eye: varying(vec3f)
        v_normal: varying(vec3f)

        vertex: fn() {
            let w = self.world_from_obj * vec4(self.geom.pos, self.vertex_w)
            let v = self.draw_pass.camera_view * w
            self.v_obj = self.geom.pos
            self.v_world = w.xyz
            self.v_view = v.xyz
            self.v_normal = (self.normal_from_obj * vec4(self.geom.normal, 0.0)).xyz
            self.v_clip = self.draw_pass.camera_projection * v
            self.vertex_pos = self.v_clip
            // Every triangle belongs to one element. As in aux, unwanted triangles collapse
            // before rasterisation; the shared geometry stays resident and unchanged.
            if abs(self.geom.element - self.group_ctl.x) > 0.5 {
                self.vertex_pos = vec4(0.0, 0.0, 2.0, 1.0)
            }
            if self.clip_ctl.z > 0.5 {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 0.0, 1.0)).xyz
            } else {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 1.0, 0.0)).xyz
            }
        }

        eye_dir: fn() -> vec3f {
            if self.clip_ctl.z > 0.5 { return normalize(self.v_eye - self.v_world) }
            return normalize(self.v_eye)
        }

        cut_margin: fn() -> float {
            if self.clip_ctl.z > 0.5 { return self.clip_ctl.y * length(self.v_view) }
            return self.clip_ctl.y
        }

        clipped: fn() -> bool {
            let m = 0.0 - self.cut_margin()
            if dot(self.clip_cut.xyz, self.v_world) + self.clip_cut.w < m { return true }
            let p = self.v_obj
            let c = self.clip_ctl.x
            if c > 0.5 && dot(self.clip0.xyz, p) + self.clip0.w < m { return true }
            if c > 1.5 && dot(self.clip1.xyz, p) + self.clip1.w < m { return true }
            if c > 2.5 && dot(self.clip2.xyz, p) + self.clip2.w < m { return true }
            if c > 3.5 && dot(self.clip3.xyz, p) + self.clip3.w < m { return true }
            if c > 4.5 && dot(self.clip4.xyz, p) + self.clip4.w < m { return true }
            if c > 5.5 && dot(self.clip5.xyz, p) + self.clip5.w < m { return true }
            return false
        }

        fragment: fn() {
            // Derivatives precede divergent discards. Orient the triangle's geometric normal
            // with its outward mesh normal, independent of the backend's screen-Y convention.
            var ng = cross(dFdx(self.v_world), dFdy(self.v_world))
            if dot(ng, self.v_normal) < 0.0 { ng = 0.0 - ng }
            if self.clipped() { discard() }
            if dot(self.v_normal, self.v_normal) <= 0.0 { discard() }
            let v_world = self.eye_dir()
            if dot(ng, v_world) * self.group_ctl.y <= 0.0 { discard() }

            let ndc = self.v_clip.xy / max(self.v_clip.w, 0.000001)
            let uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
            let g = self.opaque.sample_nearest(uv)
            let distance = length(self.v_view)
            // Zero distance or the zero element code denotes background. Negative element
            // codes are opaque back faces and must occlude just like positive codes.
            if g.x > 0.0 && g.w != 0.0 && distance > g.x + g.x * 0.0001 { discard() }

            var n_world = normalize(self.v_normal) * self.group_ctl.y
            if dot(n_world, v_world) < 0.0 { n_world = 0.0 - n_world }
            let n = normalize((self.light_from_world * vec4(n_world, 0.0)).xyz)
            let v = normalize((self.light_from_world * vec4(v_world, 0.0)).xyz)
            let f0 = clamp(self.reflectance_roughness.xyz, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0))
            let rough = clamp(self.reflectance_roughness.w, 0.04, 1.0)
            let transmission = clamp(self.tint_transmission.w, 0.0, 1.0)
            let f = self.schlick(f0, clamp(dot(n, v), 0.0, 1.0))
            let reflection = self.spec_sun(n, v, f0, rough) + self.spec_env(n, v, f0, rough)
            let rgb = reflection + self.tint_transmission.xyz * self.diffuse_light(n)
                * (vec3(1.0, 1.0, 1.0) - f) * (1.0 - transmission)
            let alpha = 1.0 - transmission * (1.0 - dot(f, vec3(0.2126, 0.7152, 0.0722)))
            // The lighting expression is already premultiplied. Nonlinear display curves
            // operate on straight colour; restore coverage after conversion and dithering.
            // A zero-alpha reflection remains additive rather than dividing by zero.
            var coverage = alpha
            if coverage <= 0.0 { coverage = 1.0 }
            var color = self.to_display(rgb * (self.exposure / coverage))
            color = self.hdr_to_display(color)
            if alpha > 0.0 && (self.display.x > 0.5 || self.u_exposure.y > 0.5) {
                color = self.output_dither(color, self.v_world)
            }
            self.fb0 = vec4(color * coverage, alpha)
        }
    }
}

/// The outward-facing or inward-facing half of a translucent surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassFaces {
    Back,
    Front,
}

/// The draw order of translucent parts' face groups, far to near, into `out` (cleared first):
/// (part index, faces).
///
/// `forward` is the unit view direction, for both orthographic and perspective views.
/// Back faces use the farthest corner depth; front faces use the nearest. Equal depths
/// put Back before Front, then the lower part index first.
///
/// Cost: O(n log n) in parts, recomputing the eight corner depths during an in-place sort.
/// No allocation when `out` has capacity for `2 * parts.len()` groups.
pub fn glass_order(parts: &[Aabb], eye: Vec3f, forward: Vec3f, out: &mut Vec<(usize, GlassFaces)>) {
    out.clear();
    for part in 0..parts.len() {
        out.push((part, GlassFaces::Back));
        out.push((part, GlassFaces::Front));
    }
    let depth = |&(part, faces): &(usize, GlassFaces)| {
        let bounds = &parts[part];
        let mut extent = match faces {
            GlassFaces::Back => f32::NEG_INFINITY,
            GlassFaces::Front => f32::INFINITY,
        };
        for bits in 0..8 {
            let corner = vec3(
                if bits & 1 == 0 { bounds.min.x } else { bounds.max.x },
                if bits & 2 == 0 { bounds.min.y } else { bounds.max.y },
                if bits & 4 == 0 { bounds.min.z } else { bounds.max.z },
            );
            let corner_depth = (corner - eye).dot(forward);
            extent = match faces {
                GlassFaces::Back => extent.max(corner_depth),
                GlassFaces::Front => extent.min(corner_depth),
            };
        }
        extent
    };
    out.sort_unstable_by(|a, b| {
        let a_depth = depth(a);
        let b_depth = depth(b);
        if a_depth != b_depth {
            return b_depth.total_cmp(&a_depth);
        }
        (a.1 == GlassFaces::Front)
            .cmp(&(b.1 == GlassFaces::Front))
            .then_with(|| a.0.cmp(&b.0))
    });
}

/// A translucent surface's optics, linear: tint is the body colour light picks up passing through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassShade {
    pub tint: [f32; 3],
    pub translucency: f32,
    pub roughness: f32,
    pub f0: [f32; 3],
}

/// One element's faces in one resident aux batch. Emit a group for each batch the element uses.
///
/// The draw API has no index-subrange binding. `element` selects the stream's existing vertex
/// label, collapsing all other elements; multiple ranges of this element in this material batch
/// are drawn together. Geometry ownership remains with the caller for the recorded pass's life.
pub struct GlassGroup {
    pub geometry: GeometryId,
    /// The raw `ElementId.0`, not the G-buffer's signed `(id + 1)` code.
    pub element: u32,
    /// Affine map from the aux stream's already placed metre positions into the pass's Z-up
    /// metre world. Normally identity; supply an element's preview transform here when moved.
    pub world_from_obj: Mat4f,
    pub faces: GlassFaces,
    pub shade: GlassShade,
}

/// How much of the insert shows through the case wall.
pub const CASE_SEE_THROUGH: f32 = 0.72;

/// One element's surface look and its resident batches. An element with several scene
/// materials keeps one entry per look so an opaque patch keeps its own colour and coverage.
struct GlassPart {
    element: ElementId,
    /// Bounds at rest in Fab's Z-up metres.
    bounds: Aabb,
    shade: GlassShade,
    geometries: Vec<GeometryId>,
}

/// Glass candidates retained across panes. Each pane orders the groups it shows at its poses.
/// Geometry stays in `SceneGpu`.
#[derive(Default)]
pub struct GlassLayer {
    built: Option<super::elements::ViewKey>,
    parts: Vec<GlassPart>,
    /// Part index and world transform for each posed box in `bounds`.
    included: Vec<(usize, Mat4f)>,
    bounds: Vec<Aabb>,
    ordered: Vec<(usize, GlassFaces)>,
    groups: Vec<GlassGroup>,
}

impl GlassLayer {
    /// Collects resident aux meshes after upload and See-Through publication.
    pub fn sync(&mut self, state: &AppState, batches: &[Geometry]) {
        let mut key = super::elements::view_key(state, state.view.lead_plate());
        // Each pane applies its poses and Case toggle in `order`, so neither changes the candidates.
        key.poses = None;
        key.case.0 = false;
        if self.built == Some(key) {
            return;
        }
        self.built = Some(key);
        self.included.clear();
        self.bounds.clear();
        self.ordered.clear();
        self.groups.clear();
        if !state.view.glass {
            self.parts.clear();
            return;
        }
        // Reuse each entry's geometry vector too: a placement preview changes the view key
        // every frame, but does not allocate while the same surfaces keep being drawn.
        let mut used = 0;
        for part in &state.plate.parts {
            let Some(id) = part.element.filter(|id| state.glass_candidate(*id)) else { continue };
            let element = &state.scene.elements[id.index()];
            let case = state.glass_parts.hosts.contains(&id).then_some(GlassShade {
                tint: [part.color[0], part.color[1], part.color[2]],
                translucency: CASE_SEE_THROUGH,
                roughness: FILAMENT_ROUGHNESS,
                f0: [0.04; 3],
            });
            let first = used;
            for &(batch, _, count) in &element.ranges {
                if count == 0 { continue; }
                let Some(geometry) = batches.get(batch as usize).map(Geometry::geometry_id) else { continue };
                // An element may have several ranges in one batch. Its draw selects all of
                // them by element label, so emit that geometry only once per face group.
                if self.parts[first..used].iter().any(|p| p.geometries.contains(&geometry)) { continue; }
                let material = &state.scene.materials[state.scene.batches[batch as usize].material.index()];
                let shade = case.unwrap_or_else(|| {
                    let f0 = if material.ior > 0.0 {
                        ((material.ior - 1.0) / (material.ior + 1.0)).powi(2)
                    } else { 0.04 };
                    GlassShade {
                        tint: [material.base_color[0], material.base_color[1], material.base_color[2]],
                        translucency: (1.0 - material.base_color[3]).max(material.transmission),
                        roughness: material.roughness,
                        f0: [f0; 3],
                    }
                });
                if let Some(p) = self.parts[first..used].iter_mut().find(|p| p.shade == shade) {
                    p.geometries.push(geometry);
                } else {
                    if let Some(p) = self.parts.get_mut(used) {
                        p.element = id;
                        p.bounds = element.bounds;
                        p.shade = shade;
                        p.geometries.clear();
                        p.geometries.push(geometry);
                    } else {
                        self.parts.push(GlassPart { element: id, bounds: element.bounds, shade, geometries: vec![geometry] });
                    }
                    used += 1;
                }
            }
        }
        self.parts.truncate(used);
        self.included.reserve(used);
        self.bounds.reserve(used);
        self.ordered.reserve(2 * self.parts.len());
        self.groups.reserve(2 * self.parts.iter().map(|p| p.geometries.len()).sum::<usize>());
    }

    /// Distinct glass elements under `poses`, counted before a pane's Case toggle.
    pub fn shown_under(&self, state: &AppState, poses: PanePoses) -> usize {
        let mut previous = None;
        let mut count = 0;
        for part in &self.parts {
            if previous != Some(part.element) {
                previous = Some(part.element);
                count += usize::from(state.is_glass_under(poses, part.element));
            }
        }
        count
    }

    /// Orders the glass shown in `pane` under `poses`, using the buffers reserved by `sync`.
    pub fn order(&mut self, eye: Vec3f, forward: Vec3f, state: &AppState, poses: PanePoses, pane: PaneId) {
        self.bounds.clear();
        self.included.clear();
        for (index, part) in self.parts.iter().enumerate() {
            if state.is_glass_under(poses, part.element) && !state.case_hidden_in(pane, part.element) {
                let world_from_obj = scene_pose_under(state, poses, part.element);
                self.bounds.push(aabb_transform(&part.bounds, &world_from_obj));
                self.included.push((index, world_from_obj));
            }
        }
        glass_order(&self.bounds, eye, forward, &mut self.ordered);
        self.groups.clear();
        for &(index, faces) in &self.ordered {
            let (part, world_from_obj) = self.included[index];
            let p = &self.parts[part];
            self.groups.extend(p.geometries.iter().map(|&geometry| GlassGroup {
                geometry,
                element: p.element.0,
                world_from_obj,
                faces,
                shade: p.shade,
            }));
        }
    }

    /// Draws the groups selected and sorted for the current pane.
    pub fn draw(&self, cx: &mut Cx3d, draw: &mut DrawFabGlass, opaque: &Texture, light: &TileLight, renderer: &Renderer, planes: &[[f32; 4]]) -> u32 {
        draw_glass(cx, draw, &self.groups, opaque, light, renderer, planes)
    }
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabGlass {
    #[deref]
    pub draw_vars: DrawVars,
    /// Homogeneous position W; the draw API needs at least one instance slot.
    #[live(1.0)]
    pub vertex_w: f32,
}

/// Draws groups in the supplied order into the current composite target, after its opaque quad.
/// `opaque` is the matching aux G-buffer (R = eye distance in metres, A = element code).
///
/// Begin a scene with the same Z-up camera/projection as this pass and aux. `planes` and the
/// separate `clip_cut` uniform use that world; `light` and the environment use the Y-up render
/// frame. Bind `clip_cut` and the composite's identical `u_exposure` / `u_balance` uniforms before
/// calling (zero plane disables the resin cut). They are preserved here so the caller can also
/// update retained calls through `set_uniform_on_draw_list`, as in the board lane.
///
/// Cost: one draw per group, all vertices of its resident batch, and fragment work proportional
/// to translucent pixels times face groups. No per-frame geometry, sorting, or temporary heap
/// allocations; draw recording reuses the framework's buffers. Returns the number of calls.
pub fn draw_glass(
    cx: &mut Cx3d,
    draw: &mut DrawFabGlass,
    groups: &[GlassGroup],
    opaque: &Texture,
    light: &TileLight,
    renderer: &Renderer,
    planes: &[[f32; 4]],
) -> u32 {
    if groups.is_empty() {
        return 0;
    }
    let Some(scene) = cx.scene_state_3d() else {
        return 0;
    };
    draw.vertex_w = 1.0;
    let vars = &mut draw.draw_vars;
    vars.refresh_after_reload(cx.cx);
    if !vars.can_instance() {
        return 0;
    }
    vars.options.alpha_blend = true;
    vars.options.depth_write = false;
    vars.options.depth_test_greater = false;
    set_texture_named(vars, cx.cx, id!(opaque), opaque);
    vars.set_uniform(cx.cx, id!(light_from_world), &render_from_fab().v);
    vars.set_uniform(cx.cx, id!(sun_dir), &light.dir);
    vars.set_uniform(cx.cx, id!(sun_color), &light.color);
    vars.set_uniform(cx.cx, id!(sun_sky), &light.sky);
    vars.set_uniform(cx.cx, id!(sun_ground), &light.ground);
    vars.set_uniform(cx.cx, id!(world_up), &light.up);
    vars.set_uniform(cx.cx, id!(exposure), &[light.exposure]);
    vars.set_uniform(cx.cx, id!(display), &[if light.display { 1.0 } else { 0.0 }, light.sun_spec, 0.0, 0.0]);
    vars.set_uniform(cx.cx, id!(key_spec), &light.key_spec);
    vars.set_uniform(cx.cx, id!(key_tint), &light.key_tint);
    renderer.bind_ibl(cx.cx, vars);
    renderer.bind_output_dither(cx.cx, vars);
    // The renderer disables lane dither under HDR because tiles feed a float target. Here
    // the target is already the display: use the composite's dither switch and the binder's
    // actual target dimensions, keeping the shared output_dither implementation unchanged.
    if let (Some((display_at, _)), Some((dither_at, _))) = (
        vars.uniform_range(cx.cx, id!(u_exposure)),
        vars.uniform_range(cx.cx, id!(output_dither_ctl)),
    ) {
        if vars.dyn_uniforms[display_at + 1] > 0.5 {
            let ctl = [
                vars.dyn_uniforms[dither_at],
                vars.dyn_uniforms[dither_at + 1],
                if vars.dyn_uniforms[display_at + 3] < 0.5 { 1.0 } else { 0.0 },
            ];
            vars.set_uniform(cx.cx, id!(output_dither_ctl), &ctl);
        }
    }
    let half_pixel = 1.0 / (scene.projection.v[5].abs() * (cx.current_pass_size().y as f32).max(1.0));
    vars.set_uniform(cx.cx, id!(clip_ctl), &[
        planes.len().min(6) as f32,
        half_pixel,
        if scene.projection.v[11] == 0.0 { 0.0 } else { 1.0 },
        0.0,
    ]);

    let mut calls = 0;
    for group in groups {
        let m = &group.world_from_obj.v;
        vars.set_uniform(cx.cx, id!(world_from_obj), m);
        vars.set_uniform(cx.cx, id!(normal_from_obj), &group.world_from_obj.invert().transpose().v);
        for (k, name) in [id!(clip0), id!(clip1), id!(clip2), id!(clip3), id!(clip4), id!(clip5)].into_iter().enumerate() {
            let plane = planes.get(k).copied().unwrap_or([0.0; 4]);
            let mut object_plane = [0.0; 4];
            for j in 0..4 {
                object_plane[j] = plane[0] * m[j * 4] + plane[1] * m[j * 4 + 1]
                    + plane[2] * m[j * 4 + 2] + plane[3] * m[j * 4 + 3];
            }
            vars.set_uniform(cx.cx, name, &object_plane);
        }
        let front = group.faces == GlassFaces::Front;
        vars.set_uniform(cx.cx, id!(group_ctl), &[group.element as f32, if front { 1.0 } else { -1.0 }]);
        let shade = &group.shade;
        vars.set_uniform(cx.cx, id!(tint_transmission), &[shade.tint[0], shade.tint[1], shade.tint[2], shade.translucency]);
        vars.set_uniform(cx.cx, id!(reflectance_roughness), &[shade.f0[0], shade.f0[1], shade.f0[2], shade.roughness]);
        // Only backface culling exists in DrawVars. Front groups use it where the transform
        // preserves winding. Back groups (and reflected transforms) use the geometric dot
        // test in the fragment shader; their surviving normals are turned toward the eye.
        let determinant = m[0] * (m[5] * m[10] - m[9] * m[6])
            - m[4] * (m[1] * m[10] - m[9] * m[2])
            + m[8] * (m[1] * m[6] - m[5] * m[2]);
        vars.options.backface_culling = front && determinant > 0.0;
        vars.geometry_id = Some(group.geometry);
        // A fresh call prevents batching from moving a group across an earlier group.
        if cx.new_draw_call(vars).is_none() {
            break;
        }
        let area = cx.add_instance(vars);
        vars.area = cx.update_area_refs(vars.area, area);
        calls += 1;
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glass_order_shows_an_unprinted_cover_only_in_its_assembly() {
        use crate::api::{PlatePart, PoseSetId, Scene};
        let mut state = AppState::default();
        state.scene = Scene::from_model(crate::model::fixture::parts(), &mut |_| {}).into();
        state.plate.parts.push(PlatePart { object: "cover".into(), printed: false, ..Default::default() });
        state.sync_plate_elements();
        let cover = state.plate.parts[0].element.expect("fixture has a transparent cover");
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let batches: Vec<_> = state.scene.batches.iter().map(|_| Geometry::new(&mut cx)).collect();
        let mut layer = GlassLayer::default();
        layer.sync(&state, &batches);

        state.pose_set_mut(PoseSetId::DEVICE).set(cover, crate::model::state::POSE_IDENTITY);
        let camera = state.view.camera(PaneId::PRIMARY);
        layer.order(camera.eye, camera.forward(), &state, PanePoses::Set(PoseSetId::DEVICE), PaneId::PRIMARY);
        assert!(layer.groups.iter().any(|g| g.element == cover.0), "the device set draws the cover as glass");
        layer.order(camera.eye, camera.forward(), &state, PanePoses::Stage, PaneId::PRIMARY);
        assert!(layer.groups.is_empty(), "the stage leaves the ghost cover out of its glass groups");
    }

    #[test]
    fn glass_order_uses_view_extents() {
        use GlassFaces::{Back, Front};

        let bounds = |half_xy: f32, min_z: f32, max_z: f32| Aabb {
            min: vec3(-half_xy, -half_xy, min_z),
            max: vec3(half_xy, half_xy, max_z),
        };
        let eye = vec3(0.0, 0.0, 10.0);
        let forward = vec3(0.0, 0.0, -1.0);
        let mut parts = [bounds(2.0, -2.0, 2.0), bounds(1.0, -1.0, 1.0)];
        let mut out = Vec::with_capacity(2 * parts.len());
        let nested = [(0, Back), (1, Back), (1, Front), (0, Front)];

        glass_order(&parts, eye, forward, &mut out);
        assert_eq!(out.as_slice(), &nested, "nested: outer back first, outer front last");

        glass_order(&parts, eye, forward, &mut out);
        assert_eq!(out.as_slice(), &nested, "repeat: output is cleared, not appended");

        glass_order(&parts, -eye, -forward, &mut out);
        assert_eq!(out.as_slice(), &nested, "reversed view: outer back first, outer front last");

        parts = [bounds(1.0, 3.0, 4.0), bounds(1.0, -4.0, -3.0)];
        glass_order(&parts, eye, forward, &mut out);
        assert_eq!(
            out.as_slice(),
            &[(1, Back), (1, Front), (0, Back), (0, Front)],
            "separated: both far groups precede both near groups",
        );

        // The base and thin lid share their XY centre and meet at Z = 0.
        parts = [bounds(1.0, -2.0, 0.0), bounds(1.0, 0.0, 0.25)];
        glass_order(&parts, eye, forward, &mut out);
        assert_eq!(
            out.as_slice(),
            &[(0, Back), (1, Back), (0, Front), (1, Front)],
            "lid and base: base back first, lid front last, tied Back before Front",
        );

        parts = [bounds(1.0, 0.0, 0.0); 2];
        glass_order(&parts, eye, forward, &mut out);
        assert_eq!(
            out.as_slice(),
            &[(0, Back), (1, Back), (0, Front), (1, Front)],
            "ties: Back before Front, then lower part index first",
        );
    }
}
