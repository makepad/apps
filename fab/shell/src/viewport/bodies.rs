//! Retained component meshes in aux and lit, plus lenses and their light in the glow layer.
//!
//! Each mesh pass copies instance data at draw time. Tessellation, mesh deduplication and material
//! conversion run on the worker pool when scene content changes. Holder moves use the pane's
//! transforms and only repack its LED pool rows when an emitting lens rides a moved board.
//! Mesh files emit only through a material named "lens". Generated emitter bodies use their top
//! faces as the lens, matching std's LED parts, whose light leaves straight up.

use super::board_geom::{body_mesh, BodyMesh, BODY_VERTEX_FLOATS};
use super::board_scene::look_of;
use super::*;
use fab_elec_core::{CompRef, ContentHash, Sel, Xform3};
use fab_elec_mech::mesh::file_body_meshes;
use fab_elec_mech::scene::{ElecScene, MeshRef, SceneBody};
use fab_elec_parts::{BodyPort, BodyPortKind};
use fab_elec_view::{live::sim::SimLive, ElecLive};
use fab_materials::{part_look::PartClass, MaterialId, Surface};
use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// The bed a seated body mirrors past its board's edge (the plate face): its open-area linear albedo and
/// its box on the plate, mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BedLook {
    pub albedo: [f32; 3],
    pub min: [f32; 2],
    pub max: [f32; 2],
}

/// Resolve the plate face once per scene change: 4096 texel reads on a fixed 64 x 64 grid.
pub(super) fn bed_look(state: &AppState) -> Option<BedLook> {
    let scene = &state.scene;
    let element = scene.elements.iter()
        .find(|e| e.name.strip_prefix(CONTEXT_PREFIX) == Some("plate-face"))?;
    let &(batch, _, _) = element.ranges.first()?;
    let material = scene.materials.get(scene.batches.get(batch as usize)?.material.index())?;
    let mut albedo = [material.base_color[0], material.base_color[1], material.base_color[2]];
    if let Some(index) = material.texture {
        let texture = scene.textures.get(index as usize)?;
        let (width, height) = (texture.width as usize, texture.height as usize);
        let bytes = width.checked_mul(height)?.checked_mul(4)?;
        if width == 0 || height == 0 || texture.rgba.len() < bytes { return None; }
        // Decode each possible byte once, then average in linear light.
        let linear: [f32; 256] = std::array::from_fn(|i| {
            let v = i as f32 / 255.0;
            makepad_render::srgb_to_linear(vec3f(v, v, v)).x
        });
        let mut sum = [0.0; 3];
        for gy in 0..64 {
            let y = (2 * gy + 1) * height / 128;
            for gx in 0..64 {
                let x = (2 * gx + 1) * width / 128;
                let offset = (y * width + x) * 4;
                for channel in 0..3 {
                    sum[channel] += linear[texture.rgba[offset + channel] as usize];
                }
            }
        }
        for channel in 0..3 { albedo[channel] *= sum[channel] / 4096.0; }
    }
    Some(BedLook {
        albedo,
        min: state.plate.bed_min.map(|v| v as f32),
        max: state.plate.bed_max.map(|v| v as f32),
    })
}

/// A lens's area in square millimetres projected along the light's direction `dir` (body frame):
/// a dome counts as its disc, a flat top as itself; faces turned away count nothing.
fn projected_area_mm2(mesh: &BodyMesh, dir: [f64; 3]) -> f32 {
    let d = vec3(dir[0] as f32, dir[1] as f32, dir[2] as f32);
    let length = d.length();
    if !(length > 0.0) {
        return 0.0;
    }
    let d = d / length;
    let at = |i: u32| {
        let o = i as usize * BODY_VERTEX_FLOATS;
        (vec3(mesh.vertices[o], mesh.vertices[o + 1], mesh.vertices[o + 2]), vec3(mesh.vertices[o + 4], mesh.vertices[o + 5], mesh.vertices[o + 6]))
    };
    mesh.indices.chunks_exact(3).map(|t| {
        let ((a, na), (b, nb), (c, nc)) = (at(t[0]), at(t[1]), at(t[2]));
        // the stored normals say which way a face looks, whatever a file's winding
        if (na + nb + nc).dot(d) <= 0.0 {
            return 0.0;
        }
        0.5 * Vec3f::cross(b - a, c - a).dot(d).abs()
    }).sum()
}

/// A lens's emission at full light output (a part row's y of 1: its LED at `LED_RATED_A`), linear
/// RGB in display whites; the glow pass scales it by the row's y. The part's flux at that current
/// leaves the lens's area projected along the light's direction as a Lambertian face, luminance
/// flux / (pi A), against a white face's KEY_LUX / pi. The colour carries luminance (Rec. 709 Y)
/// 1, so the stated flux alone sets how bright the lens is. Black for a part that states no flux.
pub(super) fn lens_emission(body: &SceneBody, lens: &BodyMesh) -> [f32; 3] {
    let Some(emitter) = &body.emitter else { return [0.0; 3] };
    // Preserve the missing-flux fast path and let the shared rule report it.
    let area_m2 = if emitter.flux_per_a.is_some() { projected_area_mm2(lens, emitter.dir) * 1e-6 } else { 0.0 };
    super::part_glow::emitter_whites(body, area_m2)
}

script_mod! {
    use mod.prelude.fab.*
    use mod.math.*
    use mod.shader.*
    use mod.shared.*
    use mod.draw
    use mod.elec_live.*

    mod.fab_body_geom = {
        BodyVertex: #(body_vertex_pod(vm))
        BodyGeom: #(body_placeholder_geom(vm))
    }

    mod.draw.FabBodySurface = {
        alpha_blend: false
        depth_write: true
        backface_culling: true
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_body_geom.BodyVertex, mod.fab_body_geom.BodyGeom)
        world_from_plate: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip_cut: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_plate: varying(vec3f)
        v_world: varying(vec3f)
        v_view: varying(vec3f)
        v_spos: varying(vec4f)
        v_eye: varying(vec3f)
        v_normal: varying(vec3f)
        v_seat_o: varying(vec3f)
        v_seat_n: varying(vec3f)
        v_seat_u: varying(vec3f)
        v_seat_v: varying(vec3f)

        surface_vertex: fn() -> vec4f {
            let p = self.m0 * self.geom.pos.x + self.m1 * self.geom.pos.y
                + self.m2 * self.geom.pos.z + self.m3
            let w = self.world_from_plate * p
            let v = self.draw_pass.camera_view * w
            self.v_plate = p.xyz * 0.001
            self.v_world = w.xyz
            self.v_view = v.xyz
            // Cofactor columns divided by the signed determinant are the inverse transpose.
            // Retaining its sign matters for a reflection; normalising alone loses that sign.
            let a = self.m0.xyz
            let b = self.m1.xyz
            let c = self.m2.xyz
            let det = dot(a, cross(b, c))
            var inv_det = 0.0
            if abs(det) > 0.000000000001 { inv_det = 1.0 / det }
            let n = (cross(b, c) * self.geom.normal.x + cross(c, a) * self.geom.normal.y
                + cross(a, b) * self.geom.normal.z) * inv_det
            var world_normal = (self.world_from_plate * vec4(n, 0.0)).xyz
            if dot(world_normal, world_normal) > 0.0 { world_normal = normalize(world_normal) }
            self.v_normal = world_normal
            self.v_seat_o = (self.world_from_plate * (self.m2 * self.seat_look.w + self.m3)).xyz
            var seat_normal = (self.world_from_plate * vec4(cross(a, b) * inv_det, 0.0)).xyz
            if dot(seat_normal, seat_normal) > 0.0 { seat_normal = normalize(seat_normal) }
            self.v_seat_n = seat_normal
            self.v_seat_u = (self.world_from_plate * vec4(a, 0.0)).xyz
            self.v_seat_v = (self.world_from_plate * vec4(b, 0.0)).xyz
            let pos = self.draw_pass.camera_projection * v
            self.v_spos = pos
            if self.clip_ctl.z > 0.5 {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 0.0, 1.0)).xyz
            } else {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 1.0, 0.0)).xyz
            }
            return pos
        }

        vertex: fn() { self.vertex_pos = self.surface_vertex() }

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
            let p = self.v_plate
            let c = self.clip_ctl.x
            if c > 0.5 && dot(self.clip0.xyz, p) + self.clip0.w < m { return true }
            if c > 1.5 && dot(self.clip1.xyz, p) + self.clip1.w < m { return true }
            if c > 2.5 && dot(self.clip2.xyz, p) + self.clip2.w < m { return true }
            if c > 3.5 && dot(self.clip3.xyz, p) + self.clip3.w < m { return true }
            if c > 4.5 && dot(self.clip4.xyz, p) + self.clip4.w < m { return true }
            if c > 5.5 && dot(self.clip5.xyz, p) + self.clip5.w < m { return true }
            return false
        }
    }

    mod.draw.DrawFabBody = mod.std.set_type_default() do #(DrawFabBody::script_shader(vm)){
        ..mod.draw.FabBodySurface,
        ..mod.draw.SdfLit,
        ..mod.draw.SunCascades,
        ..mod.draw.FabCortaoWeights,
        // Screen-space AO (CORTAO): ssao_ctl.x is its strength on ambient light and environment
        // reflection (never the sun), 0 skips the sample. After every inherited texture so the
        // fixed slots before it keep their places.
        ssao_map: texture_2d(float)
        csm_map: texture_depth(float)
        ssao_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        plate_from_world: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        bed_look: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        bed_box: uniform(vec4(0.0, 0.0, 0.0, 0.0))

        screen_visibility: fn() -> float {
            // Two nearest taps of the same texel per body pixel (fill and reflection), one pow
            // and one exp2 for the weights; nothing per frame on the CPU.
            if self.ssao_ctl.x <= 0.001 { return 1.0 }
            let sp = self.v_spos.xy / max(self.v_spos.w, 0.000001)
            return self.ssao_map.sample_nearest(vec2(sp.x * 0.5 + 0.5, 0.5 - sp.y * 0.5)).x
        }

        fill_occlusion: fn() -> float {
            return self.cortao_ambient(self.screen_visibility(), self.ssao_ctl.x)
        }

        sun_visibility: fn(n: vec3) -> float {
            let ndl = dot(n, normalize(self.sun_dir))
            if ndl <= 0.0 { return 1.0 }
            return self.csm_vis(self.v_world, n, ndl)
        }

        spec_occlusion: fn(ndv: float, rough: float) -> float {
            return self.cortao_specular(ndv, self.screen_visibility(), rough, self.ssao_ctl.x)
        }

        near_reflect: fn(r: vec3, rough: f32) -> vec3 {
            if self.seat.x > self.seat.z { return self.env_reflect(r, rough) }
            // The seated face, normal away from the board; a through-hole body's far side (beyond
            // the board's thickness, mat.z in body mm) mirrors the board's other face.
            let n0 = normalize(self.v_seat_n)
            let h0 = dot(self.v_world - self.v_seat_o, n0)
            let thick = self.mat.z * sqrt(max(dot(self.v_seat_u, self.v_seat_u), 0.000000000001))
            var n = n0
            var o = self.v_seat_o
            var flipped = false
            if h0 < 0.0 - 0.5 * thick {
                n = n0 * (0.0 - 1.0)
                o = self.v_seat_o - n0 * thick
                flipped = true
            }
            let rn = dot(r, n)
            // Into the board: the horizon blend widens with the prefiltered lobe's roughness.
            // Minimum horizon width: 0.02 cosine units; minimum box edge width below: 0.1 mm.
            let w = 0.02 + rough * rough
            let into = smoothstep(0.0 - w, w, 0.0 - rn)
            if into <= 0.0 { return self.env_reflect(r, rough) }
            let height = max(dot(self.v_world - o, n), 0.0)
            let t = height / max(0.0 - rn, 0.0001)
            let rel = self.v_world + r * t - o
            let uu = dot(self.v_seat_u, self.v_seat_u)
            let vv = dot(self.v_seat_v, self.v_seat_v)
            let x = dot(rel, self.v_seat_u) / max(uu, 0.000000000001)
            let y = dot(rel, self.v_seat_v) / max(vv, 0.000000000001)
            // Outside the outline box by out mm; soften with roughness and hit distance.
            let ox = max(max(self.seat.x - x, x - self.seat.z), 0.0)
            let oy = max(max(self.seat.y - y, y - self.seat.w), 0.0)
            let out = length(vec2(ox, oy))
            let t_mm = t / sqrt(max(uu, 0.000000000001))
            let inside = 1.0 - smoothstep(0.0, 0.1 + rough * t_mm, out)
            // The board mask in the parts' fill and key, on the board face's own normal.
            // The whole reflection takes specular occlusion in spec_env, so the plane is occluded once.
            let lit = self.env_fill(n) + self.sun_light(n)
            // Past the board's edge, the level seated face mirrors the bed's lit plate colour.
            // Extra cost: one box mask, one matrix multiply and one mix per body pixel, no texture taps.
            var bed_in = 0.0
            let plate_up = normalize((self.world_from_plate * vec4(0.0, 0.0, 1.0, 0.0)).xyz)
            if self.bed_look.w > 0.5 && !flipped && dot(n, plate_up) > 0.999 {
                let hit = (self.plate_from_world * vec4(self.v_world + r * t, 1.0)).xy
                let bx = max(max(self.bed_box.x - hit.x, hit.x - self.bed_box.z), 0.0)
                let by = max(max(self.bed_box.y - hit.y, hit.y - self.bed_box.w), 0.0)
                let out_bed = length(vec2(bx, by))
                bed_in = 1.0 - smoothstep(0.0, 0.1 + rough * t_mm, out_bed)
            }
            // The mask's clear coat mirrors the environment along r mirrored in the board's face, at
            // the wider of the two lobes. One lookup serves both: along that mirror where the board
            // covers the reflection, along r where nothing does; the two meet at the horizon
            // (rn = 0), so the blend band stays continuous. Only the part of r along n turns, so
            // the direction vanishes only for a ray straight into the board at half cover (the
            // outline's soft band), which keeps r. The bed has no coat.
            let on = into * inside
            var dir = r - n * (2.0 * rn * on)
            let dir_len2 = dot(dir, dir)
            if dir_len2 < 0.000001 { dir = r } else { dir = dir / sqrt(dir_len2) }
            let env = self.env_reflect(dir, mix(rough, max(rough, self.seat_coat.y), on))
            let g = 1.0 - clamp(self.seat_coat.y, 0.04, 1.0)
            let f0 = self.seat_coat.x
            let f_coat = f0 + (max(g, f0) - f0) * pow(1.0 - min(0.0 - rn, 1.0), 5.0)
            let board = self.seat_look.xyz * lit * (1.0 - f_coat) + env * f_coat
            let plane = mix(self.bed_look.xyz * lit, board, inside)
            return mix(env, plane, into * max(inside, bed_in))
        }

        pixel: fn() -> vec4f {
            if self.clipped() { discard() }
            if dot(self.v_normal, self.v_normal) == 0.0 { discard() }
            let n = normalize(self.v_normal)
            let v = self.eye_dir()
            // Also cull by the outward normal when a batch has reflected winding.
            if dot(n, v) <= 0.0 { discard() }
            var color = self.lit_surface_f0(n, v, self.base.xyz, self.base.w, self.mat.x, self.mat.w)
            color = self.to_display(color * self.exposure)
            if self.display.x > 0.5 { color = self.output_dither(color, self.v_world) }
            return vec4(color, 1.0)
        }

        fragment: fn() { self.fb0 = self.pixel() }
    }

    // Raw HDR writes from a lens or screen: aux admits only the visible body surface.
    mod.draw.FabBodyEmission = {
        ..mod.draw.FabBodySurface,
        color_format: @Rgba16F
        depth_write: false
        aux: texture_2d(float)

        pixel: fn() -> vec4f {
            if self.clipped() { discard() }
            if dot(self.v_normal, self.v_normal) == 0.0 { discard() }
            let n = normalize(self.v_normal)
            if dot(n, self.eye_dir()) <= 0.0 { discard() }
            let g = self.emission()
            if max(g.x, max(g.y, g.z)) <= 0.0 { discard() }
            let sp = self.v_spos.xy / max(self.v_spos.w, 0.000001)
            let uv = vec2(sp.x * 0.5 + 0.5, 0.5 - sp.y * 0.5)
            let a = self.aux.sample_nearest(uv)
            let distance = length(self.v_view)
            // Bodies share one aux code, so distance identifies the visible surface. The
            // same vertex stage, plate frame and camera (including jitter) give the same
            // surface point; 0.02% plus one micrometre allows interpolation roundoff.
            if !(abs(a.w - self.code) < 0.5 && abs(a.x - distance) <= distance * 0.0002 + 0.000001) { discard() }
            return vec4(g, 1.0)
        }

        fragment: fn() { self.fb0 = self.pixel() }
    }

    mod.draw.DrawFabBodyGlow = mod.std.set_type_default() do #(DrawFabBodyGlow::script_shader(vm)){
        ..mod.draw.FabBodyEmission,
        live_parts: texture_2d(float)
        live_on: uniform(0.0)
        glow_gain: uniform(1.0)

        emission: fn() -> vec3f {
            let row = self.emit.w
            if self.live_on < 0.5 || row < 0.0 { discard() }
            let y = self.live_parts.sample_nearest(Live.net_uv(row, self.live_parts.size())).y
            return self.emit.xyz * y * self.glow_gain
        }
    }

    mod.draw.DrawFabBodyScreen = mod.std.set_type_default() do #(DrawFabBodyScreen::script_shader(vm)){
        ..mod.draw.FabBodyEmission,
        v_uv: varying(vec2f)
        screen: texture_2d(float)
        screen_size: uniform(vec2(1.0, 1.0))
        screen_glass: uniform(vec4(0.0, 0.0, 1.0, 1.0))
        screen_white: uniform(1.0)

        vertex: fn() {
            self.vertex_pos = self.surface_vertex()
            self.v_uv = vec2(self.geom.pad, self.geom.pad2)
        }

        emission: fn() -> vec3f {
            let texel = self.screen_glass.xy + self.v_uv * self.screen_glass.zw
            let rgb = self.screen.sample_nearest(texel / self.screen_size).xyz
            return srgb_to_linear(rgb) * self.screen_white
        }
    }

    // The G-buffer write shared by the board and body lanes: view distance, octahedral view normal.
    mod.draw.FabAuxOut = {
        color_format: @Rgba32F
        oct_encode: fn(n: vec3) -> vec2 {
            let l1 = abs(n.x) + abs(n.y) + abs(n.z)
            let e = n / max(l1, 0.00000001)
            if e.z >= 0.0 {
                return vec2(e.x, e.y)
            }
            let sx = step(0.0, e.x) * 2.0 - 1.0
            let sy = step(0.0, e.y) * 2.0 - 1.0
            return vec2((1.0 - abs(e.y)) * sx, (1.0 - abs(e.x)) * sy)
        }
    }

    mod.draw.DrawFabBodyAux = mod.std.set_type_default() do #(DrawFabBodyAux::script_shader(vm)){
        ..mod.draw.FabBodySurface,
        ..mod.draw.FabAuxOut,

        pixel: fn() -> vec4f {
            if self.clipped() { discard() }
            if dot(self.v_normal, self.v_normal) == 0.0 { discard() }
            let n = normalize(self.v_normal)
            if dot(n, self.eye_dir()) <= 0.0 { discard() }
            let vn = normalize((self.draw_pass.camera_view * vec4(n, 0.0)).xyz)
            let e = self.oct_encode(vn)
            return vec4(length(self.v_view), e.x, e.y, self.code)
        }

        fragment: fn() { self.fb0 = self.pixel() }
    }
}

/// Position in body millimetres and outward normal, each padded to 16 bytes (the std140 record
/// `BODY_VERTEX_FLOATS` describes and `fab_elec_mech::mesh::body_mesh` writes).
#[repr(C)]
#[derive(Clone, Script, ScriptHook)]
pub struct BodyVertex {
    #[live]
    pub pos: Vec3f,
    #[live]
    pub pad: f32,
    #[live]
    pub normal: Vec3f,
    #[live]
    pub pad2: f32,
}

const _: () = assert!(std::mem::size_of::<BodyVertex>() == BODY_VERTEX_FLOATS * 4);

fn body_vertex_pod(vm: &mut ScriptVm) -> ScriptValue {
    let v = BodyVertex::script_pod(vm).expect("BodyVertex pod");
    vm.heap_mut().pod_type_name_set(v, id_lut!(BodyVertex));
    v.into()
}

fn body_placeholder_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(BodyGeom), |cx| {
        let geometry = Geometry::new(cx);
        let mut vertices = Vec::with_capacity(3 * BODY_VERTEX_FLOATS);
        for p in [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]] {
            vertices.extend_from_slice(&[p[0], p[1], 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        }
        geometry.update(cx, vec![0, 1, 2], vertices);
        geometry
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

/// Per-body appearance and placement; the transform maps body mm to plate mm.
pub struct BodyInstance {
    pub fab_from_body: Mat4f,
    pub base: [f32; 4],
    pub metal: f32,
    pub translucency: f32,
    /// The dielectric's reflectance at normal incidence (ignored for a metal).
    pub f0: f32,
    /// The emitter's linear colour per unit of LED output (static), black for no emitter.
    pub emit: [f32; 3],
    /// The part's row in the host's part rows (`ValueIndex::comp`), or -1: live values are sampled
    /// there, never written into the instance (one live mechanism: rows).
    pub part_row: f32,
    /// The board outer contour's box in body mm: [min_x, min_y, max_x, max_y].
    /// An empty box (min_x > max_x) disables near reflection for this body.
    pub seat: [f32; 4],
    /// Linear board mask albedo (rgb) and board face height in body mm (w), zero when seated.
    pub seat_look: [f32; 4],
    /// The board mask's clear coat: its reflectance at normal incidence and its roughness, which
    /// the mirrored board reflects the environment with.
    pub seat_coat: [f32; 2],
    /// The board's thickness in body mm (drawn as `mat.z`): where a through-hole body reaches past
    /// the board, that side mirrors the board's other face.
    pub seat_thickness: f32,
    pub code: f32,
}

impl BodyInstance {
    /// The installed pose supplies the seat and board look for every surface of the part.
    fn new(base: [f32; 4], metal: f32, translucency: f32, f0: f32, emit: [f32; 3]) -> Self {
        Self {
            fab_from_body: Mat4f::identity(), base, metal, translucency, f0, emit, part_row: -1.0,
            seat: [1.0, 1.0, -1.0, -1.0], seat_look: [0.0; 4], seat_coat: [0.0; 2],
            seat_thickness: 0.0, code: super::print::LIT_BEAD_AUX_ID,
        }
    }
}

macro_rules! body_draw {
    ($name:ident) => {
        #[derive(Script, ScriptHook, Debug)]
        #[repr(C)]
        pub struct $name {
            #[deref]
            pub draw_vars: DrawVars,
            #[live]
            pub m0: Vec4f,
            #[live]
            pub m1: Vec4f,
            #[live]
            pub m2: Vec4f,
            #[live]
            pub m3: Vec4f,
            #[live]
            pub base: Vec4f,
            /// x: metalness, y: translucency, z: board thickness in body mm, w: dielectric F0.
            #[live]
            pub mat: Vec4f,
            #[live]
            pub emit: Vec4f,
            #[live]
            pub seat: Vec4f,
            #[live]
            pub seat_look: Vec4f,
            #[live]
            pub seat_coat: Vec2f,
            #[live]
            pub code: f32,
        }

        impl $name {
            /// `selected`: the parts the Electronics views selected; their bodies take
            /// `SELECTED_BODY_AUX_ID` (a compare a drawn body while any is selected).
            fn draw(&mut self, cx: &mut CxDraw, batch: &BodyBatch, moves: &[Option<Mat4f>], selected: &[CompRef]) -> u32 {
                if batch.instances.is_empty() || !self.draw_vars.can_instance() {
                    return 0;
                }
                self.draw_vars.geometry_id = Some(batch.geometry.geometry_id());
                // Reflections reverse winding. Keep a single call even for a mixed batch:
                // hardware culling stays on otherwise, and dot(normal, eye) culls both cases.
                self.draw_vars.options.backface_culling = !batch.windings.iter().any(|&(board, rest_reflected)| {
                    rest_reflected != moves.get(board).and_then(Option::as_ref).is_some_and(reflected)
                });
                if cx.new_draw_call(&mut self.draw_vars).is_none() {
                    return 0;
                }
                for placed in &batch.instances {
                    let instance = &placed.instance;
                    let m = placed.pose_under(moves).v;
                    self.m0 = vec4(m[0], m[1], m[2], m[3]);
                    self.m1 = vec4(m[4], m[5], m[6], m[7]);
                    self.m2 = vec4(m[8], m[9], m[10], m[11]);
                    self.m3 = vec4(m[12], m[13], m[14], m[15]);
                    let b = instance.base;
                    self.base = vec4(b[0], b[1], b[2], b[3]);
                    self.mat = vec4(instance.metal, instance.translucency, instance.seat_thickness, instance.f0);
                    let e = instance.emit;
                    self.emit = vec4(e[0], e[1], e[2], instance.part_row);
                    let s = instance.seat;
                    self.seat = vec4(s[0], s[1], s[2], s[3]);
                    let s = instance.seat_look;
                    self.seat_look = vec4(s[0], s[1], s[2], s[3]);
                    let c = instance.seat_coat;
                    self.seat_coat = vec2(c[0], c[1]);
                    self.code = if !selected.is_empty() && placed.comp.is_some_and(|comp| selected.contains(&comp)) {
                        super::print::SELECTED_BODY_AUX_ID
                    } else {
                        instance.code
                    };
                    let area = cx.add_instance(&mut self.draw_vars);
                    self.draw_vars.area = cx.update_area_refs(self.draw_vars.area, area);
                }
                1
            }
        }
    };
}
body_draw!(DrawFabBody);
body_draw!(DrawFabBodyAux);
body_draw!(DrawFabBodyGlow);
body_draw!(DrawFabBodyScreen);

/// Linear base RGB and perceptual roughness, metalness, translucency, then dielectric reflectance,
/// from the table row `fab_materials` resolves for the body (the unspecified body's when it has
/// no surface).
pub fn body_look(surface: Option<&fab_materials::Surface>) -> ([f32; 4], f32, f32, f32) {
    let shade = fab_materials::shade::shade_body(surface);
    ([shade.base[0], shade.base[1], shade.base[2], shade.roughness], shade.metal, shade.translucency, shade.f0[0])
}

/// True when `a` and `b` draw the same bodies but for their poses: board by board, body by body,
/// the same mesh, part class, surface, emitter and component identity (what an upload builds from;
/// it bakes no pose, board look or live value). When soldered, pad inputs must also match.
/// An equal content hash establishes body equality at once; otherwise
/// O(bodies), comparing each mesh's data (a courtyard's outline points), plus O(pad points) when
/// soldered. Runs once per new scene (each preview sample of a drag), never per frame.
fn same_bodies(a: &ElecScene, b: &ElecScene, soldered: bool) -> bool {
    if std::ptr::eq(a, b) || a.hash == b.hash { return true; }
    a.boards.len() == b.boards.len()
        && a.boards.iter().zip(&b.boards).all(|(a, b)| {
            a.bodies.len() == b.bodies.len()
                && a.bodies.iter().zip(&b.bodies).all(|(a, b)| a.mesh == b.mesh && a.class == b.class
                    && a.surface == b.surface && a.emitter == b.emitter && a.ports == b.ports && a.sel == b.sel)
                && (!soldered || solder::same_pads(a, b))
        })
}

/// Map local mm to parent mm, converting only the transform's nm translation.
pub fn xform_mat(x: &Xform3) -> Mat4f {
    let a = x.rotate([1.0, 0.0, 0.0]);
    let b = x.rotate([0.0, 1.0, 0.0]);
    let c = x.rotate([0.0, 0.0, 1.0]);
    Mat4f {
        v: [
            a[0] as f32, a[1] as f32, a[2] as f32, 0.0,
            b[0] as f32, b[1] as f32, b[2] as f32, 0.0,
            c[0] as f32, c[1] as f32, c[2] as f32, 0.0,
            (x.t[0] as f64 * 1e-6) as f32,
            (x.t[1] as f64 * 1e-6) as f32,
            (x.t[2] as f64 * 1e-6) as f32,
            1.0,
        ],
    }
}

struct PlacedBody {
    board: usize,
    body: usize,
    comp: Option<CompRef>,
    /// Joint-local mm to the owning body; ordinary bodies use their mesh frame directly.
    local: Option<Mat4f>,
    instance: BodyInstance,
    /// Static lens geometry, kept in the body frame so pose edits can republish the pool.
    pool: Option<PoolLens>,
}

struct PoolLens {
    centre_mm: Vec3f,
    normal: Vec3f,
    area: f32,
    full_reach: f32,
}

impl PoolLens {
    /// The same projected emitting area as lens_emission, with its area-weighted centre.
    /// Only emitting lens instances get a pool, even when they share a batch with dark parts.
    fn of(body: &SceneBody, mesh: &BodyMesh, emit: [f32; 3]) -> Option<Self> {
        if emit == [0.0; 3] { return None; }
        let dir = body.emitter.as_ref()?.dir;
        let normal = vec3(dir[0] as f32, dir[1] as f32, dir[2] as f32).normalize();
        let area_mm2 = projected_area_mm2(mesh, dir);
        if !(area_mm2 > 0.0) { return None; }
        let at = |i: u32| {
            let o = i as usize * BODY_VERTEX_FLOATS;
            (vec3(mesh.vertices[o], mesh.vertices[o + 1], mesh.vertices[o + 2]),
                vec3(mesh.vertices[o + 4], mesh.vertices[o + 5], mesh.vertices[o + 6]))
        };
        let mut centre = vec3(0.0, 0.0, 0.0);
        for t in mesh.indices.chunks_exact(3) {
            let ((a, na), (b, nb), (c, nc)) = (at(t[0]), at(t[1]), at(t[2]));
            if (na + nb + nc).dot(normal) <= 0.0 { continue; }
            let weight = 0.5 * Vec3f::cross(b - a, c - a).dot(normal).abs();
            centre = centre + (a + b + c) * (weight / 3.0);
        }
        let area = area_mm2 * 1e-6;
        Some(Self { centre_mm: centre / area_mm2, normal, area, full_reach: pool::reach(emit, area) })
    }

    fn append(&self, body: &BodyInstance, m: Mat4f, data: &mut Vec<f32>) {
        let p = self.centre_mm;
        let c = m.transform_vec4(vec4(p.x, p.y, p.z, 1.0));
        let n = self.normal;
        // Body/board/plate poses are rigid, including reflections; inverse transpose = R.
        let n = m.transform_vec4(vec4(n.x, n.y, n.z, 0.0));
        let n = vec3(n.x, n.y, n.z).normalize();
        data.extend_from_slice(&[
            c.x * 0.001, c.y * 0.001, c.z * 0.001, self.area,
            n.x, n.y, n.z, self.full_reach,
            body.emit[0], body.emit[1], body.emit[2], body.part_row,
        ]);
    }
}

impl PlacedBody {
    fn pose_under(&self, moves: &[Option<Mat4f>]) -> Mat4f {
        board::moved_pose(self.instance.fab_from_body, moves.get(self.board).copied().flatten())
    }

    /// Resolve only when recording lit instances: a lowering can renumber parts without
    /// rebuilding meshes. Published values themselves stay entirely in the shared texture.
    fn resolve_part_row(&mut self, sim: Option<&SimLive>) {
        self.instance.part_row = sim.and_then(|sim| self.comp.and_then(|comp| sim.index.comp(comp)))
            .map_or(-1.0, |row| row as f32);
    }
}

struct BodyBatchUpload {
    mesh: BodyMesh,
    bounds: Aabb,
    /// A file's material named "lens", or the top faces of a generated emitter body.
    lens: bool,
    actuator: bool,
    turn: bool,
    instances: Vec<PlacedBody>,
}

struct BodyUpload {
    batches: Vec<BodyBatchUpload>,
    /// Body-local bounds of the mesh primitives, excluding solder joints.
    mesh_bounds: Vec<Vec<Aabb>>,
    skipped: usize,
    emitting_lens: bool,
    soldered: bool,
}

/// Why an emitter's body does not glow, once per mesh path or generated part and reason for the
/// process, including repeated scene builds.
pub(super) fn report_dark_emitter(body: &SceneBody, why: &str) {
    static REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = match &body.mesh {
        MeshRef::Mesh(path) | MeshRef::Script(path) => path.clone(),
        _ => format!("{:?}", body.sel),
    };
    if REPORTED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert(format!("{key}: {why}")) {
        log!("body: {key}: {why}: it does not glow");
    }
}

/// Split generated geometry into the case and top lens, retaining only each part's used vertices.
fn split_top_lens(mesh: BodyMesh) -> [BodyMesh; 2] {
    split_mesh(mesh, |mesh, triangle| {
        triangle.iter().all(|&index| mesh.vertices[index as usize * BODY_VERTEX_FLOATS + 6] > 0.5)
    })
}

fn split_mesh(mesh: BodyMesh, takes: impl Fn(&BodyMesh, &[u32]) -> bool) -> [BodyMesh; 2] {
    let mut parts = [BodyMesh::default(), BodyMesh::default()];
    let vertex_count = mesh.vertices.len() / BODY_VERTEX_FLOATS;
    let mut remaps = [vec![None; vertex_count], vec![None; vertex_count]];
    for triangle in mesh.indices.chunks_exact(3) {
        let part = usize::from(takes(&mesh, triangle));
        let out = &mut parts[part];
        for &index in triangle {
            let mapped = remaps[part][index as usize].get_or_insert_with(|| {
                let mapped = (out.vertices.len() / BODY_VERTEX_FLOATS) as u32;
                let offset = index as usize * BODY_VERTEX_FLOATS;
                out.vertices.extend_from_slice(&mesh.vertices[offset..offset + BODY_VERTEX_FLOATS]);
                mapped
            });
            out.indices.push(*mapped);
        }
    }
    parts
}

/// Whether a body is generated (no mesh file, no script) and states an emitter: its top faces are
/// its lens ([`generated_body_meshes`]).
pub(super) fn generated_emitter(body: &SceneBody) -> bool {
    body.emitter.is_some() && matches!(&body.mesh, MeshRef::Box { .. } | MeshRef::Extrude { .. })
}

/// The same case, lens and actuator primitives feed the raster and traced panes.
pub(super) struct BodyPrimitive {
    pub mesh: BodyMesh,
    pub surface: Option<Surface>,
    pub lens: bool,
    pub actuator: bool,
    /// A shaft (and what it carries) that turns with its part's place ([`shaft_turn`]).
    pub turn: bool,
}

pub(super) fn press_port(body: &SceneBody) -> Option<&BodyPort> {
    body.ports.iter().find(|p| p.kind == BodyPortKind::Press)
}

fn shaft_port(body: &SceneBody) -> Option<&BodyPort> {
    body.ports.iter().find(|p| p.kind == BodyPortKind::Shaft)
}

/// A press port's swept cylinder, in body millimetres. Cached with the scene, never on hover.
#[derive(Clone, Copy)]
struct PressCylinder {
    at: Vec3f,
    dir: Vec3f,
    radius: f32,
    stand: f32,
    travel: f32,
}

impl PressCylinder {
    fn of(port: &BodyPort) -> Option<Self> {
        let dir = vec3(port.dir[0] as f32, port.dir[1] as f32, port.dir[2] as f32);
        let length = dir.length();
        let radius = port.profile.rings().flatten().map(|p| {
            (p.x as f64).hypot(p.y as f64) * 1e-6
        }).fold(0.0, f64::max) as f32;
        if !length.is_finite() || length <= 0.0 || !radius.is_finite() || radius <= 0.0 { return None; }
        Some(Self {
            at: vec3(port.at[0] as f32 * 1e-6, port.at[1] as f32 * 1e-6, port.at[2] as f32 * 1e-6),
            dir: dir / length,
            radius,
            stand: port.stand as f32 * 1e-6,
            travel: port.travel as f32 * 1e-6,
        })
    }

    /// A shaft port's grip: its section from the tip back along the axis to the body's seat (the
    /// plane through the body's origin across the axis), so a drag on the housing under the shaft
    /// takes it too, drawn shaft or not. Never shorter than the shaft itself.
    fn shaft(port: &BodyPort) -> Option<Self> {
        let mut grip = Self::of(port)?;
        grip.travel = (grip.at.dot(grip.dir) - grip.stand).max(0.0);
        Some(grip)
    }

    /// The closed cylinder from housing minus travel to the unpressed top. Do not normalise
    /// `dir`: the incoming ray's parameter (metres in pick_ray) survives the inverse pose.
    fn hit(&self, origin: Vec3f, dir: Vec3f) -> Option<f32> {
        let o = origin - self.at;
        let oz = o.dot(self.dir);
        let dz = dir.dot(self.dir);
        let radial = o - self.dir * oz;
        let slope = dir - self.dir * dz;
        let lo = -self.stand - self.travel;
        let mut nearest = f32::INFINITY;
        let mut take = |t: f32| { if t >= 0.0 && t < nearest { nearest = t; } };
        if dz.abs() > 1e-12 {
            for z in [lo, 0.0] {
                let t = (z - oz) / dz;
                let p = radial + slope * t;
                if p.dot(p) <= self.radius * self.radius { take(t); }
            }
        }
        let a = slope.dot(slope);
        let b = radial.dot(slope);
        let c = radial.dot(radial) - self.radius * self.radius;
        let discriminant = b * b - a * c;
        if a > 1e-12 && discriminant >= 0.0 {
            let root = discriminant.sqrt();
            for t in [(-b - root) / a, (-b + root) / a] {
                let z = oz + t * dz;
                if z >= lo && z <= 0.0 { take(t); }
            }
        }
        nearest.is_finite().then_some(nearest)
    }
}

/// A file actuator contains only triangles wholly within the port axis's 1.1-radius cylinder
/// and reaching above the housing. Lens membership is kept on both halves unchanged.
fn split_actuator(mesh: BodyMesh, port: &BodyPort) -> [BodyMesh; 2] {
    let Some(p) = PressCylinder::of(port) else { return [mesh, BodyMesh::default()] };
    // Compute the housing plane before rounding to mesh floats, so a triangle exactly on
    // the housing is not promoted by subtracting separately rounded at/stand values.
    let h: [f32; 3] = std::array::from_fn(|i| ((port.at[i] as f64 - port.stand as f64 * port.dir[i]) * 1e-6) as f32);
    let housing = vec3(h[0], h[1], h[2]);
    split_mesh(mesh, |mesh, triangle| {
        let mut highest = f32::NEG_INFINITY;
        for &index in triangle {
            let o = index as usize * BODY_VERTEX_FLOATS;
            let vertex = vec3(mesh.vertices[o], mesh.vertices[o + 1], mesh.vertices[o + 2]);
            let v = vertex - p.at;
            let z = v.dot(p.dir);
            let radial = v - p.dir * z;
            if radial.dot(radial) > (1.1 * p.radius).powi(2) { return false; }
            highest = highest.max((vertex - housing).dot(p.dir));
        }
        highest > 0.0
    })
}

/// A typical D shaft's flat: across the flat it is this share of the round's radius from the axis
/// (a 6 mm shaft is 4.5 mm across its flat), so a drawn shaft shows how far it is turned.
const SHAFT_FLAT_AT: f32 = 0.5;

/// A generated pot's shaft: the shaft port's round with its D flat toward the profile's +v (the
/// board's +y for an upright shaft: up at mid travel, as a knob's pointer is), from the housing to
/// the tip, with its top cap. None is drawn for a shaft that does not stand out of its housing.
fn shaft_mesh(port: &BodyPort) -> BodyMesh {
    let Some(p) = PressCylinder::of(port) else { return BodyMesh::default() };
    let mut mesh = BodyMesh::default();
    if port.stand == 0 { return mesh; }
    let mut u = Vec3f::cross(vec3(0.0, 0.0, 1.0), p.dir);
    if u.dot(u) <= 1e-24 { u = Vec3f::cross(vec3(0.0, 1.0, 0.0), p.dir); }
    let u = u / u.length();
    let v = Vec3f::cross(p.dir, u);
    // The arc below the flat, anticlockwise from the flat's left end through -v to its right end.
    let (r, flat) = (p.radius, p.radius * SHAFT_FLAT_AT);
    let half = (flat / r).asin();
    const SEGMENTS: usize = 32;
    let ring: Vec<(f32, f32)> = (0..=SEGMENTS).map(|i| {
        let a = std::f32::consts::PI - half + (std::f32::consts::PI + 2.0 * half) * i as f32 / SEGMENTS as f32;
        (r * a.cos(), r * a.sin())
    }).collect();
    let mut vertex = |(x, y): (f32, f32), z: f32, n: Vec3f| {
        let at = p.at + u * x + v * y + p.dir * z;
        let index = (mesh.vertices.len() / BODY_VERTEX_FLOATS) as u32;
        mesh.vertices.extend_from_slice(&[at.x, at.y, at.z, 0.0, n.x, n.y, n.z, 0.0]);
        index
    };
    // The top cap: a fan over the convex D.
    let centre = vertex((0.0, 0.0), 0.0, p.dir);
    let cap: Vec<u32> = ring.iter().map(|&q| vertex(q, 0.0, p.dir)).collect();
    for i in 0..cap.len() {
        mesh.indices.extend_from_slice(&[centre, cap[i], cap[(i + 1) % cap.len()]]);
    }
    // The round's walls with radial normals, then the flat with its own.
    for i in 0..SEGMENTS {
        let (a, b) = (ring[i], ring[i + 1]);
        let (na, nb) = (u * (a.0 / r) + v * (a.1 / r), u * (b.0 / r) + v * (b.1 / r));
        let quad = [vertex(a, -p.stand, na), vertex(b, -p.stand, nb), vertex(b, 0.0, nb), vertex(a, 0.0, na)];
        mesh.indices.extend_from_slice(&[quad[0], quad[1], quad[2], quad[0], quad[2], quad[3]]);
    }
    let (a, b) = (ring[SEGMENTS], ring[0]);
    let quad = [vertex(a, -p.stand, v), vertex(b, -p.stand, v), vertex(b, 0.0, v), vertex(a, 0.0, v)];
    mesh.indices.extend_from_slice(&[quad[0], quad[1], quad[2], quad[0], quad[2], quad[3]]);
    mesh
}

/// Extrude the stated profile (including holes), with separate top and wall normals. Std's
/// 0.01 mm circle chords give about 24 segments at the tactile switch's radius.
fn actuator_mesh(port: &BodyPort) -> BodyMesh {
    use makepad_csg_cad::region::{oriented, Region};
    let Some(p) = PressCylinder::of(port) else { return BodyMesh::default() };
    let mut mesh = BodyMesh::default();
    // No invented plunger for stand == 0. Its swept cylinder remains pickable.
    if port.stand == 0 { return mesh; }
    // Same profile basis as board_mech::port_axes: +z reference, +y for a z axis.
    let mut u = Vec3f::cross(vec3(0.0, 0.0, 1.0), p.dir);
    if u.dot(u) <= 1e-24 { u = Vec3f::cross(vec3(0.0, 1.0, 0.0), p.dir); }
    let u = u / u.length();
    let v = Vec3f::cross(p.dir, u);
    let mut vertex = |xy: [f64; 2], z: f32, n: Vec3f| {
        let at = p.at + u * xy[0] as f32 + v * xy[1] as f32 + p.dir * z;
        let index = (mesh.vertices.len() / BODY_VERTEX_FLOATS) as u32;
        mesh.vertices.extend_from_slice(&[at.x, at.y, at.z, 0.0, n.x, n.y, n.z, 0.0]);
        index
    };
    for polygon in port.profile.polygons() {
        let ring = |points: &[fab_elec_core::P], ccw| oriented(points.iter()
            .map(|p| [p.x as f64 * 1e-6, p.y as f64 * 1e-6]).collect(), ccw);
        let region = Region {
            outer: ring(&polygon.outer, true),
            holes: polygon.holes.iter().map(|h| ring(h, false)).collect(),
        };
        let (points, triangles) = region.triangulate();
        for triangle in triangles {
            for i in triangle { mesh.indices.push(vertex(points[i as usize], 0.0, p.dir)); }
        }
        for ring in std::iter::once(&region.outer).chain(&region.holes) {
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                let n = u * (b[1] - a[1]) as f32 - v * (b[0] - a[0]) as f32;
                if n.length() == 0.0 { continue; }
                let n = n / n.length();
                let corners = [vertex(a, -p.stand, n), vertex(b, -p.stand, n), vertex(b, 0.0, n), vertex(a, 0.0, n)];
                mesh.indices.extend_from_slice(&[corners[0], corners[1], corners[2], corners[0], corners[2], corners[3]]);
            }
        }
    }
    mesh
}

/// Generated geometry at five micrometres on the integer nanometre grid. A +z press port
/// sets the case top to its housing height; the actuator uses the part's stated surface.
pub(super) fn generated_body_meshes(mesh: &MeshRef, emitter: bool, port: Option<&BodyPort>, shaft: Option<&BodyPort>) -> Option<Vec<BodyPrimitive>> {
    let mut housing = mesh.clone();
    if let Some(p) = port.filter(|p| p.dir == [0.0, 0.0, 1.0]) {
        let top = p.at[2] - p.stand;
        match &mut housing {
            MeshRef::Box { max, .. } => max[2] = top,
            MeshRef::Extrude { height, .. } => *height = top,
            _ => {}
        }
    }
    let mesh = body_mesh(&housing, 5_000)?;
    let mut primitives = if emitter {
        let [case, lens] = split_top_lens(mesh);
        let surface = Surface::new(MaterialId::lookup("epoxy_lens_diffused").expect("the LED lens material exists"));
        vec![BodyPrimitive { mesh: case, surface: None, lens: false, actuator: false, turn: false },
            BodyPrimitive { mesh: lens, surface: Some(surface), lens: true, actuator: false, turn: false }]
    } else {
        vec![BodyPrimitive { mesh, surface: None, lens: false, actuator: false, turn: false }]
    };
    if let Some(port) = port {
        primitives.push(BodyPrimitive { mesh: actuator_mesh(port), surface: None, lens: false, actuator: true, turn: false });
    }
    if let Some(shaft) = shaft {
        primitives.push(BodyPrimitive { mesh: shaft_mesh(shaft), surface: None, lens: false, actuator: false, turn: true });
    }
    Some(primitives)
}

pub(super) fn body_primitives(body: &SceneBody) -> Option<Vec<BodyPrimitive>> {
    let port = press_port(body);
    if let MeshRef::Package(package) = &body.mesh {
        return Some(match fab_elec_mech::package::package_meshes(package) {
            Ok(meshes) => meshes.into_iter().map(|(mesh, surface)| BodyPrimitive { mesh, surface: Some(surface), lens: false, actuator: false, turn: false }).collect(),
            Err(error) => {
                log!("body generation: package {:?}: {error}: drawn as its body box", package.kind);
                let [min, max] = package.body;
                let plain = MeshRef::Box { min: [min.x, min.y, 0], max: [max.x, max.y, package.height] };
                return generated_body_meshes(&plain, false, port, None);
            }
        });
    }
    let MeshRef::Mesh(path) = &body.mesh else {
        return generated_body_meshes(&body.mesh, generated_emitter(body), port, shaft_port(body));
    };
    let files = match file_body_meshes(path, body.class) {
        Ok(files) => files,
        Err(error) => {
            log!("body generation: mesh {path}: {error}");
            return Some(Vec::new());
        }
    };
    let shaft = shaft_port(body);
    let mut primitives = Vec::new();
    for file in files {
        let (case, actuator) = match port {
            Some(port) => { let [case, actuator] = split_actuator(file.mesh, port); (case, Some(actuator)) }
            None => (file.mesh, None),
        };
        // A shaft splits off as an actuator does: what stands out of the housing within its radius.
        let (case, turned) = match shaft {
            Some(shaft) => { let [case, turned] = split_actuator(case, shaft); (case, Some(turned)) }
            None => (case, None),
        };
        primitives.push(BodyPrimitive { mesh: case, surface: Some(file.surface), lens: file.lens, actuator: false, turn: false });
        if let Some(mesh) = actuator {
            primitives.push(BodyPrimitive { mesh, surface: Some(file.surface), lens: file.lens, actuator: true, turn: false });
        }
        if let Some(mesh) = turned {
            primitives.push(BodyPrimitive { mesh, surface: Some(file.surface), lens: file.lens, actuator: false, turn: true });
        }
    }
    Some(primitives)
}

impl BodyUpload {
    fn build(scene: &ElecScene, soldered: bool) -> Option<Self> {
        let mut upload = Self {
            batches: Vec::new(), skipped: 0, emitting_lens: false, soldered,
            mesh_bounds: scene.boards.iter().map(|board| vec![aabb_empty(); board.bodies.len()]).collect(),
        };
        let mut body_batches: Vec<Vec<Vec<(usize, bool)>>> = if soldered {
            scene.boards.iter().map(|board| vec![Vec::new(); board.bodies.len()]).collect()
        } else { Vec::new() };
        // Meshes share batches only with identical press-port geometry. Generated bodies also
        // key on emitter presence: an emitter splits into case and top lens.
        let mut meshes: Vec<(&MeshRef, PartClass, bool, Option<&BodyPort>, Option<&BodyPort>, Vec<(usize, Option<Surface>)>)> = Vec::new();
        for (board_index, board) in scene.boards.iter().enumerate() {
            for (body_index, body) in board.bodies.iter().enumerate() {
                checkpoint().ok()?;
                if matches!(&body.mesh, MeshRef::Script(_)) {
                    if body.emitter.is_some() { report_dark_emitter(body, "an emitter but no \"lens\" material"); }
                    upload.skipped += 1;
                    continue;
                }
                let generated_emitter = generated_emitter(body);
                let batches = if let Some((_, _, _, _, _, batches)) = meshes.iter().find(|(mesh, class, emitter, port, shaft, _)|
                    *mesh == &body.mesh && *class == body.class && *emitter == generated_emitter && *port == press_port(body) && *shaft == shaft_port(body)) {
                    batches.clone()
                } else {
                    let built = body_primitives(body)?;
                    let mut batches = Vec::new();
                    for BodyPrimitive { mesh, surface, lens, actuator, turn } in built {
                        if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                            continue;
                        }
                        let bounds = mesh_bounds(&mesh);
                        batches.push((upload.batches.len(), surface));
                        upload.batches.push(BodyBatchUpload { mesh, bounds, lens, actuator, turn, instances: Vec::new() });
                    }
                    meshes.push((&body.mesh, body.class, generated_emitter, press_port(body), shaft_port(body), batches.clone()));
                    batches
                };
                if batches.is_empty() && matches!(&body.mesh, MeshRef::Mesh(_)) {
                    upload.skipped += 1;
                }
                let has_lens = batches.iter().any(|(batch, _)| upload.batches[*batch].lens);
                if body.emitter.is_some() && !has_lens { report_dark_emitter(body, "an emitter but no \"lens\" material"); }
                let comp = match &body.sel { Sel::Comp(comp) => Some(*comp), _ => None };
                for (batch, surface) in batches {
                    let bounds = &mut upload.mesh_bounds[board_index][body_index];
                    *bounds = aabb_union(bounds, &upload.batches[batch].bounds);
                    let (base, metal, translucency, f0) = body_look(body.surface.as_ref().or(surface.as_ref()));
                    if soldered { body_batches[board_index][body_index].push((batch, metal >= 0.5)); }
                    let emit = if upload.batches[batch].lens { lens_emission(body, &upload.batches[batch].mesh) } else { [0.0; 3] };
                    let pool = PoolLens::of(body, &upload.batches[batch].mesh, emit);
                    upload.emitting_lens |= emit != [0.0; 3];
                    upload.batches[batch].instances.push(PlacedBody {
                        board: board_index,
                        body: body_index,
                        comp,
                        local: None,
                        pool,
                        instance: BodyInstance::new(base, metal, translucency, f0, emit),
                    });
                }
            }
        }
        if soldered {
            let joints = solder::scene_joints(scene, &mut |board, body| {
                solder::body_triangles(&scene.boards[board].bodies[body],
                    body_batches[board][body].iter().map(|&(batch, metal)| (&upload.batches[batch].mesh, metal)))
            })?;
            let mut batches = HashMap::new();
            for (board, joints) in joints.into_iter().enumerate() {
                for joint in joints {
                    let batch = *batches.entry(Arc::as_ptr(&joint.mesh)).or_insert_with(|| {
                        let bounds = mesh_bounds(&joint.mesh.mesh);
                        let batch = upload.batches.len();
                        upload.batches.push(BodyBatchUpload { mesh: joint.mesh.mesh.clone(), bounds,
                            lens: false, actuator: false, turn: false, instances: Vec::new() });
                        batch
                    });
                    let body = &scene.boards[board].bodies[joint.body];
                    let [r, g, b] = solder::SOLDER_BASE;
                    upload.batches[batch].instances.push(PlacedBody {
                        board, body: joint.body,
                        comp: match &body.sel { Sel::Comp(comp) => Some(*comp), _ => None },
                        local: Some(joint.body_from_joint), pool: None,
                        instance: BodyInstance::new([r, g, b, joint.roughness], solder::SOLDER_METAL, 0.0, 0.04, [0.0; 3]),
                    });
                }
            }
        }
        Some(upload)
    }
}

struct BodyBatch {
    geometry: Rc<Geometry>,
    bounds: Aabb,
    lens: bool,
    /// Index into the layer's body-local screen faces. Glass batches follow the body batches.
    screen: Option<usize>,
    actuator: bool,
    /// A shaft that turns with its part's place.
    turn: bool,
    instances: Vec<PlacedBody>,
    /// Instances are grouped by board. Cache each run's rest winding when the board is placed;
    /// live body edits rotate and translate in that frame and preserve its winding.
    windings: Vec<(usize, bool)>,
}

/// A body instance's plate, scene and board transforms: its board's pose after its own, the drag's
/// pose (`live`) winning over the scene's.
fn instance_pose(scene: &ElecScene, plate_from_scene: &[Mat4f], live: &HashMap<Sel, Xform3>, held: &[CompRef], turns: &[(CompRef, f32)], actuator: bool, turn: bool, board: usize, body: usize) -> (Mat4f, Mat4f, Mat4f) {
    let board_of = &scene.boards[board];
    let body = &board_of.bodies[body];
    let body_pose = live.get(&body.sel).unwrap_or(&body.pose);
    // Xform3::then(child) means self AFTER child: board.then(body) is board * body.
    // Compose matrices directly to avoid the integer-nm rounding in Xform3::then.
    let mut board_from_body = xform_mat(body_pose);
    if actuator && matches!(&body.sel, Sel::Comp(comp) if held.contains(comp)) {
        board_from_body = Mat4f::mul(&board_from_body, &actuator_offset(body));
    }
    if turn {
        if let Some(&(_, angle)) = turns.iter().find(|t| matches!(&body.sel, Sel::Comp(comp) if *comp == t.0)) {
            board_from_body = Mat4f::mul(&board_from_body, &shaft_turn(body, angle));
        }
    }
    let scene_from_body = Mat4f::mul(&xform_mat(&board_of.pose), &board_from_body);
    let pose = plate_from_scene.get(board).copied().unwrap_or_else(Mat4f::identity);
    (Mat4f::mul(&pose, &scene_from_body), scene_from_body, board_from_body)
}

/// Body-local displacement of a held actuator in the raster view.
pub(super) fn actuator_offset(body: &SceneBody) -> Mat4f {
    let delta = press_port(body).map_or([0.0; 3], |p| p.dir.map(|d| (-d * p.travel as f64 * 1e-6) as f32));
    Mat4f::translation(vec3(delta[0], delta[1], delta[2]))
}

/// A shaft's turn by `angle` (radians, right-handed about its port's direction) about its port's
/// axis, body-local; the identity for a body without a usable shaft port.
pub(super) fn shaft_turn(body: &SceneBody, angle: f32) -> Mat4f {
    let Some(p) = shaft_port(body).and_then(PressCylinder::of) else { return Mat4f::identity() };
    let turn = makepad_widgets_core::Pose::new(makepad_widgets_core::Quat::from_axis_angle(p.dir, angle), vec3(0.0, 0.0, 0.0)).to_mat4();
    Mat4f::mul(&Mat4f::translation(p.at), &Mat4f::mul(&turn, &Mat4f::translation(-p.at)))
}

/// Every knob's turn from mid travel: its place as the bench turned it, else where the lowering
/// starts it, as a share of [`fab_elec_parts::SHAFT_TURN_DEG`]; clockwise down the shaft as the
/// place rises, the rule the closed box turns printed knobs by. O(knobs x turned).
pub(super) fn knob_angles(turned: &[(CompRef, f64)], knobs: &[(CompRef, f64)]) -> Vec<(CompRef, f32)> {
    knobs.iter().map(|&(comp, start)| {
        let place = turned.iter().find(|t| t.0 == comp).map_or(start, |t| t.1);
        (comp, (-(place - 0.5) * fab_elec_parts::SHAFT_TURN_DEG.to_radians()) as f32)
    }).collect()
}

struct PressInstance {
    board: usize,
    body: usize,
    comp: CompRef,
    cylinder: PressCylinder,
    /// The unpressed body's inverse placement and grip point before its holder moves.
    rest_body_from_plate: Mat4f,
    rest_tip_mm: Vec3f,
    /// A shaft a drag turns ([`PressCylinder::shaft`]), not an actuator a press moves.
    turn: bool,
}

struct PlacedScreen {
    face: screen::ScreenFace,
    rest_body_from_plate: Mat4f,
}

fn inverse_moves(moves: &[Option<Mat4f>]) -> Vec<Option<Mat4f>> {
    if moves.iter().all(Option::is_none) { return Vec::new(); }
    moves.iter().map(|movement| movement.as_ref().map(Mat4f::invert)).collect()
}

/// Where the ray `o + t d` enters the box (0 from inside it), None when it misses or the box is
/// behind it.
fn box_hit(b: &Aabb, o: Vec3f, d: Vec3f) -> Option<f32> {
    let (mut near, mut far) = (0.0f32, f32::INFINITY);
    for (lo, hi, o, d) in [(b.min.x, b.max.x, o.x, d.x), (b.min.y, b.max.y, o.y, d.y), (b.min.z, b.max.z, o.z, d.z)] {
        if d.abs() < 1e-12 {
            if o < lo || o > hi { return None; }
            continue;
        }
        let (t0, t1) = ((lo - o) / d, (hi - o) / d);
        near = near.max(t0.min(t1));
        far = far.min(t0.max(t1));
        if near > far { return None; }
    }
    Some(near)
}

/// Map the plate ray to body millimetres while preserving its distance parameter in metres.
fn body_ray_mm(ray: &crate::model::query::Ray, rest_body_from_plate: &Mat4f, inverse_move: Option<&Mat4f>) -> (Vec3f, Vec3f) {
    let moved = inverse_move.map(|inverse| Mat4f::mul(rest_body_from_plate, inverse));
    let body_from_plate = moved.as_ref().unwrap_or(rest_body_from_plate);
    let o = ray.origin * 1000.0;
    let d = ray.dir * 1000.0;
    (
        body_from_plate.transform_vec4(vec4(o.x, o.y, o.z, 1.0)).to_vec3f(),
        body_from_plate.transform_vec4(vec4(d.x, d.y, d.z, 0.0)).to_vec3f(),
    )
}

/// The seated board face's box and height in body mm. Four corner transforms (a few dozen flops)
/// per posed body, never per frame; the outline box is already cached per scene.
fn seat_of(board_box: Option<[f32; 4]>, thickness_mm: f32, board_from_body: &Mat4f) -> ([f32; 4], f32) {
    let empty = ([1.0, 1.0, -1.0, -1.0], 0.0);
    let Some(board_box) = board_box else { return empty };
    let m = &board_from_body.v;
    let face_z = if m[10] > 0.0 { thickness_mm } else { 0.0 };
    let mut bounds = aabb_empty();
    let mut sum_z = 0.0;
    for x in [board_box[0], board_box[2]] {
        for y in [board_box[1], board_box[3]] {
            // R transposed times (p - t) is the rigid inverse, including reflected rotations.
            let d = vec3(x - m[12], y - m[13], face_z - m[14]);
            let p = vec3(
                m[0] * d.x + m[1] * d.y + m[2] * d.z,
                m[4] * d.x + m[5] * d.y + m[6] * d.z,
                m[8] * d.x + m[9] * d.y + m[10] * d.z,
            );
            include_point(&mut bounds, p);
            sum_z += p.z;
        }
    }
    if bounds.max.z - bounds.min.z > 0.01 { return empty; }
    ([bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y], sum_z * 0.25)
}

/// Grow `into` by the corners of `b` under `m`.
fn grow_bounds(b: &Aabb, m: &Mat4f, into: &mut Aabb) {
    for x in [b.min.x, b.max.x] {
        for y in [b.min.y, b.max.y] {
            for z in [b.min.z, b.max.z] {
                let p = m.transform_vec4(vec4(x, y, z, 1.0));
                include_point(into, vec3(p.x, p.y, p.z));
            }
        }
    }
}

fn include_point(bounds: &mut Aabb, p: Vec3f) {
    bounds.min.x = bounds.min.x.min(p.x);
    bounds.min.y = bounds.min.y.min(p.y);
    bounds.min.z = bounds.min.z.min(p.z);
    bounds.max.x = bounds.max.x.max(p.x);
    bounds.max.y = bounds.max.y.max(p.y);
    bounds.max.z = bounds.max.z.max(p.z);
}

fn mesh_bounds(mesh: &BodyMesh) -> Aabb {
    let mut bounds = aabb_empty();
    for p in mesh.vertices.chunks_exact(BODY_VERTEX_FLOATS) {
        include_point(&mut bounds, vec3(p[0], p[1], p[2]));
    }
    bounds
}

fn reflected(m: &Mat4f) -> bool {
    let m = &m.v;
    let a = vec3(m[0], m[1], m[2]);
    let b = vec3(m[4], m[5], m[6]);
    let c = vec3(m[8], m[9], m[10]);
    a.dot(Vec3f::cross(b, c)) < 0.0
}

/// A pane keeps its posed pool rows across glow recordings. None uses the layer's rest rows.
#[derive(Default)]
pub(super) struct PanePool {
    key: Option<(u64, u64)>,
    instances: Option<RetainedInstances>,
}

/// Meshes persist between frames. Each pass costs one draw call per distinct mesh, with only
/// instance data copied; no geometry or materials are built per frame.
#[derive(Default)]
pub struct BodyLayer {
    batches: Vec<BodyBatch>,
    mesh_bounds: Vec<Vec<Aabb>>,
    screen_layout: screen::ScreenLayout,
    screen_faces: Vec<PlacedScreen>,
    soldered: bool,
    rebuild_requested: bool,
    casters: Rc<RefCell<casters::ElecCasters>>,
    /// The plate face's mean linear albedo and box, resolved only when the scene or bed bounds change.
    pub(super) bed: Option<BedLook>,
    emitting_lens: bool,
    empty_parts: Option<Texture>,
    /// Lens batch indices in draw order, so packing pool rows only visits lenses.
    lens_batches: Vec<usize>,
    /// Packed pool instances at rest, republished on install, body edits and row-map changes.
    pool_instances: Option<RetainedInstances>,
    pool_revision: u64,
    part_rows: Option<(Option<TextureId>, u64)>,
    /// Background builds requested; value-only publications must never increase this.
    #[cfg(test)]
    build_requests: usize,
    /// The newest scene the batches stand for: its bodies have their meshes and looks
    /// ([`same_bodies`]); the poses are `requested`'s.
    uploaded: Option<Arc<ElecScene>>,
    uploaded_soldered: bool,
    /// The scene and each of its boards' plate pose (scene mm to plate mm, by board index).
    requested: Option<(Arc<ElecScene>, Vec<Mat4f>)>,
    /// The build in flight and the scene it builds.
    pending: Option<(Arc<ElecScene>, bool, TaskHandle<Option<BodyUpload>>)>,
    /// A drag's body poses by component (`AppState::elec_poses`, gathered), which win over the
    /// scene's until the first committed scene after the drag lands (`clear_live`).
    live: HashMap<Sel, Xform3>,
    /// The pose set `live` took last, by identity.
    live_from: Option<Arc<[(Sel, Xform3)]>>,
    held: Vec<CompRef>,
    /// Each knob's turn from mid travel ([`knob_angles`]).
    turns: Vec<(CompRef, f32)>,
    /// One entry per pressable body, including ports with no visible stand, and per shaft.
    presses: Vec<PressInstance>,
    /// Each component's uploaded instances (batch, instance), for posing a few bodies at once.
    by_sel: HashMap<Sel, Vec<(usize, usize)>>,
    reported_skips: HashSet<ContentHash>,
    /// Each board's bodies at their rest placement in plate mm.
    plate_bounds: Vec<Option<Aabb>>,
    /// Each board's bodies in the scene frame (mm), by board index: what a pose rests on.
    board_bounds: Vec<Option<Aabb>>,
    /// Outer contour boxes in board mm, by board index, cached only when the scene is set.
    board_boxes: Vec<Option<[f32; 4]>>,
    /// Each board's linear mask albedo and its coat (f0, roughness), by board index, cached with
    /// `board_boxes`: a mask colour or finish edit reaches the near reflection through a re-pose,
    /// without rebuilding meshes.
    board_masks: Vec<([f32; 3], [f32; 2])>,
    planes: [[f32; 4]; 6],
    plane_count: usize,
}

impl BodyLayer {
    pub(super) fn with_casters(casters: Rc<RefCell<casters::ElecCasters>>) -> Self {
        let mut layer = Self::default();
        layer.casters = casters;
        layer
    }

    /// Queue the same content rebuild as set_scene. The next sync supplies Cx for submission;
    /// installed batches stay visible until the replacement is ready.
    pub fn set_soldered(&mut self, on: bool) {
        if self.soldered == on { return; }
        self.soldered = on;
        self.rebuild_requested = self.requested.is_some();
    }

    /// Place the scene in plate mm, each board's bodies by its pose in `plate_from_scene` (by
    /// board index; a board without one keeps the scene frame). Bodies with the uploaded meshes
    /// and looks ([`same_bodies`]: an edit that moved parts, a pose change) rewrite instance
    /// transforms on this thread in O(bodies). Other bodies build once per distinct mesh on the
    /// background pool; the previous meshes remain drawable until the complete replacement is
    /// uploaded, and a build in flight for the same bodies carries on.
    pub fn set_scene(&mut self, cx: &mut Cx, scene: &Arc<ElecScene>, plate_from_scene: Vec<Mat4f>) {
        let same = self.requested.as_ref().is_some_and(|(requested, _)| Arc::ptr_eq(requested, scene));
        self.requested = Some((scene.clone(), plate_from_scene));
        // A new rest placement for the same scene only re-poses the bodies.
        if same {
            self.request_build(cx, scene);
            return;
        }
        self.board_boxes = scene.boards.iter().map(|board| board.outline.first().map(|outline| {
            let b = outline.bbox();
            [b.min.x, b.min.y, b.max.x, b.max.y].map(|v| v as f32 * 1e-6)
        })).collect();
        self.board_masks = scene.boards.iter().map(|board| {
            let look = look_of(&board.art.stackup);
            ([look.mask[0], look.mask[1], look.mask[2]], [look.mask_coat[0], look.mask[3]])
        }).collect();
        // The plane term's two colours (linear albedo, both under the same light on the same
        // normal), once per scene: their luminance ratio is what the mirrored bed and board
        // bands must show.
        if let (Some(bed), Some((mask, _))) = (self.bed, self.board_masks.first()) {
            let luminance = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            log!("bodies: plane colours: bed {:?}, board {:?}, luminance ratio {:.3}", bed.albedo, mask, luminance(bed.albedo) / luminance(*mask).max(1e-6));
        }
        self.request_build(cx, scene);
    }

    fn request_build(&mut self, cx: &mut Cx, scene: &Arc<ElecScene>) {
        self.rebuild_requested = false;
        if self.uploaded_soldered == self.soldered
            && self.uploaded.as_ref().is_some_and(|uploaded| same_bodies(uploaded, scene, self.soldered)) {
            if let Some((_, _, pending)) = self.pending.take() { pending.cancel(); }
            self.uploaded = Some(scene.clone());
            self.update_poses();
            return;
        }
        if self.pending.as_ref().is_some_and(|(building, soldered, _)| *soldered == self.soldered && same_bodies(building, scene, self.soldered)) {
            return;
        }
        if let Some((_, _, pending)) = self.pending.take() { pending.cancel(); }
        let build_scene = scene.clone();
        let soldered = self.soldered;
        #[cfg(test)]
        { self.build_requests += 1; }
        match cx.task_pool().submit_named(Class::Background, "fab-elec-bodies", move || BodyUpload::build(&build_scene, soldered)) {
            Ok(task) => self.pending = Some((scene.clone(), soldered, task)),
            Err(error) => log!("body generation: {error}"),
        }
    }

    /// Pose a drag's moved bodies by `poses` (each gesture step's, on this thread): O(the moved
    /// bodies), their bounds grown to cover them; a moved emitting lens also republishes the
    /// O(lens instances) pool. An empty set drops every drag pose (O(bodies), once).
    /// True when the drawn bodies moved.
    pub fn set_live_poses(&mut self, poses: Option<&Arc<[(Sel, Xform3)]>>) -> bool {
        let Some(poses) = poses else { return false };
        if self.live_from.as_ref().is_some_and(|from| Arc::ptr_eq(from, poses)) {
            return false;
        }
        self.live_from = Some(poses.clone());
        if poses.is_empty() {
            let had = !self.live.is_empty();
            self.live.clear();
            if had && self.uploaded.is_some() {
                self.update_poses();
            }
            return had && self.uploaded.is_some();
        }
        for (sel, pose) in poses.iter() {
            self.live.insert(sel.clone(), *pose);
        }
        let Some((scene, plate_from_scene)) = &self.requested else { return false };
        if self.uploaded_soldered != self.soldered || self.uploaded.as_ref().is_none_or(|uploaded| !Arc::ptr_eq(uploaded, scene)) {
            return false;
        }
        let mut moved = false;
        let mut pool_moved = false;
        for (sel, _) in poses.iter() {
            for &(b, i) in self.by_sel.get(sel).map_or(&[][..], |at| at.as_slice()) {
                let batch = &mut self.batches[b];
                let placed = &mut batch.instances[i];
                let (m, scene_from_body, board_from_body) = instance_pose(scene, plate_from_scene, &self.live, &self.held, &self.turns, batch.actuator && placed.local.is_none(), batch.turn && placed.local.is_none(), placed.board, placed.body);
                let (m, scene_from_body, board_from_body) = match &placed.local {
                    Some(local) => (Mat4f::mul(&m, local), Mat4f::mul(&scene_from_body, local), Mat4f::mul(&board_from_body, local)),
                    None => (m, scene_from_body, board_from_body),
                };
                if placed.instance.fab_from_body.v == m.v { continue; }
                placed.instance.fab_from_body = m;
                pool_moved |= placed.pool.is_some();
                if let Some(index) = batch.screen {
                    self.screen_faces[index].rest_body_from_plate = m.invert();
                } else {
                    self.casters.borrow_mut().set_body_pose(b, i, &m);
                }
                let thickness_mm = scene.boards[placed.board].thickness as f32 * 1e-6;
                let (seat, face_z) = seat_of(self.board_boxes.get(placed.board).copied().flatten(), thickness_mm, &board_from_body);
                placed.instance.seat = seat;
                placed.instance.seat_thickness = thickness_mm;
                let (mask, coat) = self.board_masks.get(placed.board).copied().unwrap_or(([0.0; 3], [0.0; 2]));
                placed.instance.seat_look = [mask[0], mask[1], mask[2], face_z];
                placed.instance.seat_coat = coat;
                let board = placed.board;
                grow_bounds(&batch.bounds, &m, self.plate_bounds[board].get_or_insert_with(aabb_empty));
                if let Some(in_scene) = self.board_bounds.get_mut(board) {
                    grow_bounds(&batch.bounds, &scene_from_body, in_scene.get_or_insert_with(aabb_empty));
                }
                moved = true;
            }
        }
        if pool_moved { self.rebuild_pool_instances(); }
        if moved { self.update_press_poses(); }
        moved
    }

    /// A changed held set moves only actuators. Nothing is rebuilt, and unchanged sets do no
    /// pose or caster work. The caller compares SimLive's Arc before calling this on sync.
    pub fn set_held(&mut self, held: &[CompRef]) -> bool {
        if self.held.len() == held.len() && held.iter().all(|comp| self.held.contains(comp)) { return false; }
        self.held.clear();
        self.held.extend_from_slice(held);
        if self.uploaded.is_some() { self.update_poses(); }
        true
    }

    /// Knobs turned to new places turn only their shafts, as a held set moves only actuators:
    /// nothing is rebuilt, and unchanged angles do no pose or caster work.
    pub fn set_turns(&mut self, turns: Vec<(CompRef, f32)>) -> bool {
        if self.turns == turns { return false; }
        self.turns = turns;
        if self.uploaded.is_some() { self.update_poses(); }
        true
    }

    /// The bodies of the uploaded scene drawn from a model file (the plate's or the part
    /// library's), not a plain or generated body: how a test sees a model that arrived reach the
    /// view. O(bodies); read only on an inspection.
    pub fn model_bodies(&self) -> usize {
        self.uploaded.as_ref().map_or(0, |scene| scene.boards.iter().flat_map(|board| &board.bodies)
            .filter(|body| matches!(body.mesh, MeshRef::Mesh(_))).count())
    }

    /// The part whose body the ray meets first, by each body's box in its own frame (the bounds of
    /// its mesh, solder joints aside), and the distance in the pane's plate metres. O(bodies), a
    /// pose inverse a body: a click's work.
    pub fn comp_at(&self, ray: &crate::model::query::Ray, moves: &[Option<Mat4f>]) -> Option<(CompRef, f32)> {
        let inverses = inverse_moves(moves);
        let mut nearest: Option<(CompRef, f32)> = None;
        for placed in self.batches.iter().flat_map(|batch| &batch.instances).filter(|placed| placed.local.is_none()) {
            let Some(comp) = placed.comp else { continue };
            let Some(bounds) = self.mesh_bounds.get(placed.board).and_then(|board| board.get(placed.body)) else { continue };
            if aabb_is_empty(bounds) { continue; }
            let (o, d) = body_ray_mm(ray, &placed.instance.fab_from_body.invert(), inverses.get(placed.board).and_then(Option::as_ref));
            if let Some(t) = box_hit(bounds, o, d) {
                if nearest.is_none_or(|(_, best)| t < best) { nearest = Some((comp, t)); }
            }
        }
        nearest
    }

    /// The centre of a part's body box in the pane's plate metres, None for a part with no body.
    pub fn comp_centre(&self, comp: CompRef, moves: &[Option<Mat4f>]) -> Option<Vec3f> {
        let placed = self.batches.iter().flat_map(|batch| &batch.instances)
            .find(|placed| placed.local.is_none() && placed.comp == Some(comp))?;
        let bounds = self.mesh_bounds.get(placed.board)?.get(placed.body).filter(|b| !aabb_is_empty(b))?;
        let c = (bounds.min + bounds.max) * 0.5;
        Some(placed.pose_under(moves).transform_vec4(vec4(c.x, c.y, c.z, 1.0)).to_vec3f() * 0.001)
    }

    /// Nearest swept actuator hit in the pane's plate metres. Invert each moved board once per pick.
    pub fn press_at(&self, ray: &crate::model::query::Ray, moves: &[Option<Mat4f>]) -> Option<(CompRef, f32)> {
        self.port_at(ray, false, moves)
    }

    /// Nearest shaft grip hit (a pot's shaft and the housing under it), as [`Self::press_at`].
    pub fn turn_at(&self, ray: &crate::model::query::Ray, moves: &[Option<Mat4f>]) -> Option<(CompRef, f32)> {
        self.port_at(ray, true, moves)
    }

    /// Each knob shaft's (`turn`) or button actuator's top centre in plate metres, where a grab
    /// near it on screen takes it. Only a moved board needs to transform its rest points.
    pub fn port_tips(&self, turn: bool, moves: &[Option<Mat4f>]) -> Vec<(CompRef, Vec3f)> {
        self.presses.iter().filter(|p| p.turn == turn).map(|press| {
            let at = press.rest_tip_mm;
            let p = moves.get(press.board).and_then(Option::as_ref)
                .map_or(at, |m| m.transform_vec4(at.to_vec4()).to_vec3f());
            (press.comp, p * 0.001)
        }).collect()
    }

    fn port_at(&self, ray: &crate::model::query::Ray, turn: bool, moves: &[Option<Mat4f>]) -> Option<(CompRef, f32)> {
        let inverses = inverse_moves(moves);
        let mut nearest = None;
        for press in self.presses.iter().filter(|p| p.turn == turn) {
            let (o, d) = body_ray_mm(ray, &press.rest_body_from_plate, inverses.get(press.board).and_then(Option::as_ref));
            if let Some(t) = press.cylinder.hit(o, d) {
                if nearest.is_none_or(|(_, best)| t < best) { nearest = Some((press.comp, t)); }
            }
        }
        nearest
    }

    /// Nearest glass in plate metres. A held finger follows its panel's whole plane.
    pub fn screen_at(&self, ray: &crate::model::query::Ray, held: Option<CompRef>, moves: &[Option<Mat4f>]) -> Option<(CompRef, f32, (f32, f32))> {
        let inverses = inverse_moves(moves);
        let mut nearest = None;
        for screen in &self.screen_faces {
            let face = &screen.face;
            if held.is_some_and(|comp| comp != face.comp) { continue; }
            let (origin, dir) = body_ray_mm(ray, &screen.rest_body_from_plate, inverses.get(face.board).and_then(Option::as_ref));
            let slope = dir.dot(face.dir);
            if slope.abs() <= 1e-12 || (held.is_none() && slope >= 0.0) { continue; }
            let distance = (face.glass_centre() - origin).dot(face.dir) / slope;
            if distance < 0.0 || nearest.is_some_and(|(_, best, _)| distance >= best) { continue; }
            let point = origin + dir * distance;
            if held.is_none() {
                let offset = point - face.centre;
                if offset.dot(face.right).abs() > face.half_width || offset.dot(face.up).abs() > face.half_height { continue; }
            }
            nearest = Some((face.comp, distance, face.texel(point)));
        }
        nearest
    }

    fn update_press_poses(&mut self) {
        let Some((scene, poses)) = &self.requested else { return };
        for press in &mut self.presses {
            let (pose, ..) = instance_pose(scene, poses, &self.live, &[], &[], false, false, press.board, press.body);
            press.rest_body_from_plate = pose.invert();
            press.rest_tip_mm = pose.transform_vec4(press.cylinder.at.to_vec4()).to_vec3f();
        }
    }

    /// Drop every drag pose: the scene's poses are in force again from the next re-pose (the
    /// caller's `set_scene`). O(1).
    pub fn clear_live(&mut self) {
        self.live.clear();
    }

    /// True when the batches stand for `scene`: its bodies are uploaded and posed.
    pub(super) fn shows(&self, scene: &Arc<ElecScene>) -> bool {
        self.uploaded_soldered == self.soldered && self.uploaded.as_ref().is_some_and(|uploaded| Arc::ptr_eq(uploaded, scene))
    }

    /// Completion wakes the viewport through the task pool's signal; no animation loop is needed.
    pub(super) fn ready(&self) -> bool {
        self.rebuild_requested || self.pending.as_ref().is_some_and(|(_, _, task)| task.is_finished())
    }

    /// Install finished meshes or a changed panel layout. True when the drawn set changed.
    pub fn sync(&mut self, cx: &mut Cx) -> bool {
        if self.sync_upload(cx) { return true; }
        // Faces are fitted to the uploaded scene, whose bodies `mesh_bounds` holds.
        let Some(scene) = self.uploaded.clone() else { return false };
        if !self.shows(&scene) || !self.screen_layout.update(cx) { return false; }
        if !self.update_screen_batches(cx, &scene) { return false; }
        self.update_poses();
        true
    }

    fn sync_upload(&mut self, cx: &mut Cx) -> bool {
        if self.rebuild_requested {
            if let Some(scene) = self.requested.as_ref().map(|(scene, _)| scene.clone()) { self.request_build(cx, &scene); }
        }
        let Some((built, soldered, task)) = self.pending.as_mut() else { return false };
        let Some(result) = task.try_take() else { return false };
        let built = built.clone();
        let soldered = *soldered;
        self.pending = None;
        let data = match result {
            Ok(Some(data)) => data,
            Ok(None) => return false,
            Err(error) => { log!("body generation: {error}"); return false; }
        };
        let Some(requested) = self.requested.as_ref().map(|(scene, _)| scene.clone())
            .filter(|scene| soldered == self.soldered && same_bodies(scene, &built, soldered)) else {
            return false;
        };
        if data.skipped != 0 && self.reported_skips.insert(built.hash) {
            log!("body generation: scene {} skipped {} script bodies and unreadable mesh files", built.hash.hex(), data.skipped);
        }
        self.install(cx, data, requested);
        true
    }

    fn install(&mut self, cx: &mut Cx, data: BodyUpload, requested: Arc<ElecScene>) {
        self.casters.borrow_mut().ensure_shaders(cx);
        self.emitting_lens = data.emitting_lens;
        self.uploaded_soldered = data.soldered;
        self.mesh_bounds = data.mesh_bounds;
        let mut batches = Vec::with_capacity(data.batches.len() + self.screen_faces.len());
        self.lens_batches.clear();
        for batch in data.batches {
            if batch.lens { self.lens_batches.push(batches.len()); }
            let geometry = Geometry::new(cx);
            geometry.update(cx, batch.mesh.indices, batch.mesh.vertices);
            batches.push(BodyBatch {
                geometry: Rc::new(geometry),
                bounds: batch.bounds,
                lens: batch.lens,
                screen: None,
                actuator: batch.actuator,
                turn: batch.turn,
                instances: batch.instances,
                windings: Vec::new(),
            });
        }
        self.part_rows = None;
        self.pool_instances = None;
        self.casters.borrow_mut().set_bodies(batches.iter().map(|batch| {
            casters::BodyCasters::new(batch.geometry.clone(), batch.bounds, batch.instances.iter().map(|p| {
                (p.instance.translucency, p.board, p.instance.fab_from_body)
            }))
        }).collect());
        // Keep the glass geometry across a body upload if its fitted faces are still the same.
        batches.extend(self.batches.drain(self.batches.len() - self.screen_faces.len()..));
        self.batches = batches;
        self.screen_layout.update(cx);
        self.update_screen_batches(cx, &requested);
        self.presses.clear();
        self.uploaded = Some(requested.clone());
        self.update_poses();
        for (board, scene_board) in requested.boards.iter().enumerate() {
            for (body, scene_body) in scene_board.bodies.iter().enumerate() {
                let Sel::Comp(comp) = &scene_body.sel else { continue };
                // Unreadable files and unsupported scripts have nothing visible to press.
                if !self.by_sel.contains_key(&scene_body.sel) { continue; }
                if let Some(cylinder) = press_port(scene_body).and_then(PressCylinder::of) {
                    self.presses.push(PressInstance { board, body, comp: *comp, cylinder, rest_body_from_plate: Mat4f::identity(), rest_tip_mm: cylinder.at, turn: false });
                }
                if let Some(cylinder) = shaft_port(scene_body).and_then(PressCylinder::shaft) {
                    self.presses.push(PressInstance { board, body, comp: *comp, cylinder, rest_body_from_plate: Mat4f::identity(), rest_tip_mm: cylinder.at, turn: true });
                }
            }
        }
        self.update_press_poses();
        self.rebuild_pool_instances();
    }

    fn update_screen_batches(&mut self, cx: &mut Cx, scene: &ElecScene) -> bool {
        let faces = screen::screen_faces(scene, &self.screen_layout, &self.mesh_bounds);
        if self.screen_faces.iter().map(|screen| &screen.face).eq(&faces) { return false; }
        self.batches.truncate(self.batches.len() - self.screen_faces.len());
        self.screen_faces.clear();
        for (index, face) in faces.into_iter().enumerate() {
            let mesh = face.quad();
            let bounds = mesh_bounds(&mesh);
            let geometry = Geometry::new(cx);
            geometry.update(cx, mesh.indices, mesh.vertices);
            let glass = PlacedBody {
                board: face.board, body: face.body, comp: Some(face.comp), local: None, pool: None,
                instance: BodyInstance::new([0.006, 0.006, 0.007, 0.06], 0.0, 0.0, 0.04, [0.0; 3]),
            };
            self.batches.push(BodyBatch {
                geometry: Rc::new(geometry), bounds, lens: false, screen: Some(index), actuator: false, turn: false,
                instances: vec![glass], windings: Vec::new(),
            });
            self.screen_faces.push(PlacedScreen { face, rest_body_from_plate: Mat4f::identity() });
        }
        true
    }

    fn update_poses(&mut self) {
        let Some((scene, plate_from_scene)) = &self.requested else { return };
        // A content build may be pending while a press changes. Keep the old instances until
        // install can apply the held set to the replacement's own board/body indices.
        if self.uploaded_soldered != self.soldered || self.uploaded.as_ref().is_none_or(|uploaded| !Arc::ptr_eq(uploaded, scene)) { return; }
        self.plate_bounds.clear();
        self.plate_bounds.resize(scene.boards.len(), None);
        let mut pool_moved = false;
        let mut board_bounds = vec![None::<Aabb>; scene.boards.len()];
        self.by_sel.clear();
        for (bi, batch) in self.batches.iter_mut().enumerate() {
            batch.windings.clear();
            for (i, placed) in batch.instances.iter_mut().enumerate() {
                self.by_sel.entry(scene.boards[placed.board].bodies[placed.body].sel.clone()).or_default().push((bi, i));
                let (m, scene_from_body, board_from_body) = instance_pose(scene, plate_from_scene, &self.live, &self.held, &self.turns, batch.actuator && placed.local.is_none(), batch.turn && placed.local.is_none(), placed.board, placed.body);
                let (m, scene_from_body, board_from_body) = match &placed.local {
                    Some(local) => (Mat4f::mul(&m, local), Mat4f::mul(&scene_from_body, local), Mat4f::mul(&board_from_body, local)),
                    None => (m, scene_from_body, board_from_body),
                };
                if placed.instance.fab_from_body.v != m.v {
                    if let Some(index) = batch.screen {
                        self.screen_faces[index].rest_body_from_plate = m.invert();
                    } else {
                        self.casters.borrow_mut().set_body_pose(bi, i, &m);
                    }
                    pool_moved |= placed.pool.is_some();
                }
                placed.instance.fab_from_body = m;
                let thickness_mm = scene.boards[placed.board].thickness as f32 * 1e-6;
                let (seat, face_z) = seat_of(self.board_boxes.get(placed.board).copied().flatten(), thickness_mm, &board_from_body);
                placed.instance.seat = seat;
                placed.instance.seat_thickness = thickness_mm;
                let (mask, coat) = self.board_masks.get(placed.board).copied().unwrap_or(([0.0; 3], [0.0; 2]));
                placed.instance.seat_look = [mask[0], mask[1], mask[2], face_z];
                placed.instance.seat_coat = coat;
                let winding = (placed.board, reflected(&m));
                if batch.windings.last() != Some(&winding) { batch.windings.push(winding); }
                grow_bounds(&batch.bounds, &m, self.plate_bounds[placed.board].get_or_insert_with(aabb_empty));
                grow_bounds(&batch.bounds, &scene_from_body, board_bounds[placed.board].get_or_insert_with(aabb_empty));
            }
        }
        self.board_bounds = board_bounds;
        self.casters.borrow_mut().refit_bodies();
        // Install publishes once after all poses are ready. Later pose edits replace it here.
        if pool_moved && self.pool_instances.is_some() { self.rebuild_pool_instances(); }
        self.update_press_poses();
    }

    fn rebuild_pool_instances(&mut self) {
        self.pool_instances = self.pack_pool_instances(|placed| placed.instance.fab_from_body);
        // A pane can outlive the body layer when the electronics scene is cleared or replaced.
        static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
        self.pool_revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
    }

    fn pool_lenses(&self) -> impl Iterator<Item = (&PlacedBody, &PoolLens)> {
        self.lens_batches.iter().flat_map(move |&batch| &self.batches[batch].instances)
            .filter_map(|placed| placed.pool.as_ref().map(|lens| (placed, lens)))
    }

    fn pack_pool_instances(&self, pose: impl Fn(&PlacedBody) -> Mat4f) -> Option<RetainedInstances> {
        let mut data = Vec::with_capacity(self.pool_instances.as_ref().map_or(0, |p| p.count() * 12));
        for (placed, lens) in self.pool_lenses() {
            lens.append(&placed.instance, pose(placed), &mut data);
        }
        if data.is_empty() { None } else {
            Some(RetainedInstances::new(12, data.into()).expect("pool instances have three vec4s"))
        }
    }

    #[cfg(test)]
    pub(super) fn instance_poses(&self) -> impl Iterator<Item = Mat4f> + '_ {
        self.batches.iter().flat_map(|batch| batch.instances.iter().map(|p| p.instance.fab_from_body))
    }

    /// Every uploaded instance's board and body index and the plate transform of that body: a
    /// solder joint's own transform less its place on the body, so a joint that did not follow its
    /// body reads as a body out of place.
    #[cfg(test)]
    pub(super) fn placed(&self) -> Vec<(usize, usize, Mat4f)> {
        self.batches.iter().flat_map(|batch| batch.instances.iter().map(|p| {
            let fab_from_body = match &p.local {
                Some(local) => Mat4f::mul(&p.instance.fab_from_body, &local.invert()),
                None => p.instance.fab_from_body,
            };
            (p.board, p.body, fab_from_body)
        })).collect()
    }

    /// A body build is queued or still on the pool: the bodies drawn are not yet the scene's.
    pub(super) fn building(&self) -> bool {
        self.rebuild_requested || self.pending.is_some()
    }

    /// The installed bodies have an emitting lens: the glow layer reads live part rows and
    /// repaints when they change. A cached bit, never an instance scan.
    pub fn emitting_lens(&self) -> bool {
        self.emitting_lens && !self.batches.is_empty()
    }

    pub fn has_screen_glass(&self) -> bool {
        // The glass batches form the suffix installed by update_screen_batches.
        self.batches.last().is_some_and(|batch| batch.screen.is_some())
    }

    #[cfg(test)]
    pub(super) fn build_requests(&self) -> usize {
        self.build_requests
    }

    pub fn board_bounds(&self, board: usize) -> Option<Aabb> {
        self.board_bounds.get(board).copied().flatten()
    }

    /// Section half-spaces in the plate frame, in metres, identical for aux and lit.
    pub(super) fn set_clip_planes(&mut self, planes: &[[f32; 4]]) {
        self.plane_count = planes.len().min(6);
        self.planes = [[0.0; 4]; 6];
        self.planes[..self.plane_count].copy_from_slice(&planes[..self.plane_count]);
    }

    pub(super) fn plate_bounds(&self, board: usize) -> Option<Aabb> {
        self.plate_bounds.get(board).copied().flatten()
    }

    /// Bind the shared surface stage in the pass's frame, including aux's clip margin.
    fn bind_surface(&self, cx: &mut Cx3d, world: Mat4f, vars: &mut DrawVars) -> bool {
        let Some(scene) = cx.scene_state_3d() else { return false };
        let half_pixel = 1.0 / (scene.projection.v[5].abs() * (cx.current_pass_size().y as f32).max(1.0));
        vars.set_uniform(cx.cx, id!(world_from_plate), &world.v);
        vars.set_uniform(cx.cx, id!(clip_ctl), &[self.plane_count as f32, half_pixel, if scene.projection.v[11] == 0.0 { 0.0 } else { 1.0 }, 0.0]);
        for (name, plane) in [id!(clip0), id!(clip1), id!(clip2), id!(clip3), id!(clip4), id!(clip5)].into_iter().zip(self.planes) {
            vars.set_uniform(cx.cx, name, &plane);
        }
        true
    }

    /// One call per distinct mesh per pass; only instance data is copied, nothing is built.
    /// Aux uses plate metres; lit uses the render frame and the board's exact light bindings.
    /// The caller supplies `clip_cut` in that pass's world frame, as for the board.
    pub fn draw(&mut self, cx: &mut Cx3d, lit: Option<(&TileLight, &Renderer)>, ao: Option<(&Texture, f32)>, bed_shown: bool, aux: &mut DrawFabBodyAux, shade: &mut DrawFabBody, moves: &[Option<Mat4f>], selected: &[CompRef]) -> u32 {
        if self.batches.is_empty() { return 0; }
        let scale = Mat4f::scale(0.001);
        let world = if lit.is_some() { Mat4f::mul(&render_from_fab(), &scale) } else { scale };
        let vars = if lit.is_some() { &mut shade.draw_vars } else { &mut aux.draw_vars };
        if !self.bind_surface(cx, world, vars) { return 0; }
        if let Some((light, renderer)) = lit {
            // A valid zero texture for absent AO, allocated once. ssao_ctl skips its sample.
            let empty_parts = &*self.empty_parts.get_or_insert_with(|| {
                Texture::new_with_format(cx.cx, TextureFormat::VecBGRAu8_32 {
                    width: 4, height: 4, data: Some(vec![0; 16]), updated: TextureUpdated::Full,
                })
            });
            let (ssao_map, strength) = ao.unwrap_or((empty_parts, 0.0));
            set_texture_named(vars, cx.cx, id!(ssao_map), ssao_map);
            vars.set_uniform(cx.cx, id!(ssao_ctl), &[strength, 0.0, 0.0, 0.0]);
            let (bed_look, bed_box) = self.bed.filter(|_| bed_shown).map_or(([0.0; 4], [0.0; 4]), |bed| {
                ([bed.albedo[0], bed.albedo[1], bed.albedo[2], 1.0], [bed.min[0], bed.min[1], bed.max[0], bed.max[1]])
            });
            vars.set_uniform(cx.cx, id!(plate_from_world), &world.invert().v);
            vars.set_uniform(cx.cx, id!(bed_look), &bed_look);
            vars.set_uniform(cx.cx, id!(bed_box), &bed_box);
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
            renderer.bind_sun_cascades(cx.cx, vars);
            renderer.bind_output_dither(cx.cx, vars);
            self.batches.iter().map(|batch| shade.draw(cx.cx, batch, moves, selected)).sum()
        } else {
            self.batches.iter().map(|batch| aux.draw(cx.cx, batch, moves, selected)).sum()
        }
    }

    /// Draw only lens batches into the glow layer, in aux's plate frame and camera.
    /// `selected` as the aux pass drew it: a lens shows where aux holds its own code.
    pub fn draw_glow(&mut self, cx: &mut Cx3d, aux: &Texture, glow_gain: f32, glow: &mut DrawFabBodyGlow, moves: &[Option<Mat4f>], selected: &[CompRef]) -> u32 {
        if !self.emitting_lens() { return 0; }
        let vars = &mut glow.draw_vars;
        if !self.bind_surface(cx, Mat4f::scale(0.001), vars) { return 0; }
        // A cut-away lens is absent from aux; its visibility test rejects it.
        vars.set_uniform(cx.cx, id!(clip_cut), &[0.0; 4]);
        let parts = if cx.has_global::<ElecLive>() {
            self.resolve_parts(Some(&cx.get_global::<ElecLive>().sim))
        } else {
            self.resolve_parts(None)
        };
        vars.set_uniform(cx.cx, id!(live_on), &[if parts.is_some() { 1.0 } else { 0.0 }]);
        let empty_parts = &*self.empty_parts.get_or_insert_with(|| {
            Texture::new_with_format(cx.cx, TextureFormat::VecBGRAu8_32 {
                width: 4, height: 4, data: Some(vec![0; 16]), updated: TextureUpdated::Full,
            })
        });
        set_texture_named(vars, cx.cx, id!(live_parts), parts.as_ref().unwrap_or(empty_parts));
        set_texture_named(vars, cx.cx, id!(aux), aux);
        vars.set_uniform(cx.cx, id!(glow_gain), &[glow_gain]);
        self.batches.iter().filter(|batch| batch.lens).map(|batch| glow.draw(cx.cx, batch, moves, selected)).sum()
    }

    /// Draw each glass batch with its panel's current frame memory, in aux's frame.
    pub fn draw_screens(&self, cx: &mut Cx3d, aux: &Texture, glow_gain: f32, draw: &mut DrawFabBodyScreen, moves: &[Option<Mat4f>], selected: &[CompRef]) -> u32 {
        if !self.has_screen_glass() { return 0; }
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return 0 };
        let screens = live.sim.screens.clone();
        let vars = &mut draw.draw_vars;
        if !self.bind_surface(cx, Mat4f::scale(0.001), vars) { return 0; }
        vars.set_uniform(cx.cx, id!(clip_cut), &[0.0; 4]);
        set_texture_named(vars, cx.cx, id!(aux), aux);
        let white = screen::SCREEN_WHITE_NITS / (part_glow::KEY_LUX / std::f32::consts::PI) * glow_gain;
        let mut draws = 0;
        for batch in &self.batches[self.batches.len() - self.screen_faces.len()..] {
            let Some(index) = batch.screen else { continue };
            let comp = self.screen_faces[index].face.comp;
            let Some(panel) = screens.iter().find(|panel| panel.comp == comp) else { continue };
            if !panel.on || panel.brightness <= 0.0 { continue; }
            let vars = &mut draw.draw_vars;
            set_texture_named(vars, cx.cx, id!(screen), &panel.texture);
            vars.set_uniform(cx.cx, id!(screen_size), &[panel.size.0 as f32, panel.size.1 as f32]);
            vars.set_uniform(cx.cx, id!(screen_glass), &panel.glass.map(|n| n as f32));
            vars.set_uniform(cx.cx, id!(screen_white), &[white * panel.brightness]);
            draws += draw.draw(cx.cx, batch, moves, selected);
        }
        draws
    }

    /// Draw all lens pools in one retained call. LED level changes update the parts texture.
    pub(super) fn draw_glow_pool(&mut self, cx: &mut Cx3d, aux: &Texture, lit: &Texture, glow_gain: f32, pool: &mut pool::DrawFabGlowPool, moves: &[Option<Mat4f>], pane: &mut PanePool) -> u32 {
        if !self.emitting_lens() { return 0; }
        let parts = if cx.has_global::<ElecLive>() {
            self.resolve_parts(Some(&cx.get_global::<ElecLive>().sim))
        } else {
            self.resolve_parts(None)
        };
        let (Some(parts), Some(rest)) = (parts, &self.pool_instances) else { return 0 };
        let instances = if moves.iter().any(Option::is_some) {
            let key = (self.pool_revision, board::moves_hash(moves));
            if pane.key != Some(key) {
                let lens_moved = self.pool_lenses().any(|(placed, _)| moves.get(placed.board).is_some_and(Option::is_some));
                pane.instances = if lens_moved { self.pack_pool_instances(|placed| placed.pose_under(moves)) } else { None };
                pane.key = Some(key);
            }
            pane.instances.as_ref().unwrap_or(rest)
        } else {
            rest
        };
        pool.draw(cx, instances, &parts, aux, lit, glow_gain)
    }

    fn resolve_parts(&mut self, sim: Option<&SimLive>) -> Option<Texture> {
        let rows = BodyLiveKeys::of_sim(sim).rows;
        if rows != self.part_rows {
            for batch in self.batches.iter_mut().filter(|batch| batch.lens) {
                for placed in &mut batch.instances { placed.resolve_part_row(sim); }
            }
            self.part_rows = rows;
            self.rebuild_pool_instances();
        }
        sim.and_then(|sim| sim.parts.clone())
    }
}

impl Drop for BodyLayer {
    fn drop(&mut self) {
        if let Some((_, _, task)) = &self.pending { task.cancel(); }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use super::super::part_glow::KEY_LUX;
    use fab_elec_bench::models::LED_RATED_A;
    use fab_elec_view::canvas::emitter::emitter_rgb;
    use fab_elec_core::{mm, Angle};

    /// Two triangles shared by a red LED and an ordinary component. The small typed probe
    /// fixture needs no document evaluation; in-memory GLBs need no filesystem or renderer.
    /// A lens's emission at full light output by the closed form: the fixture LED's 15 lm/A at the
    /// rated current, leaving `area_mm2` (facing the light's way) as a Lambertian face, against a
    /// white face under the key, in the colour `rgb` carried at luminance 1.
    fn full_output(rgb: [f32; 3], area_mm2: f32) -> [f32; 3] {
        let luminance = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        let whites = 15.0 * LED_RATED_A as f32 / (area_mm2 * 1e-6 * KEY_LUX);
        rgb.map(|c| c / luminance * whites)
    }

    /// Equal within float rounding, channel by channel.
    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() <= 1e-5 * b[i].abs().max(1.0))
    }

    pub(in super::super) fn led_scene(lens: bool) -> (Arc<ElecScene>, [f32; 3]) {
        use fab_elec_import::{cache::MeshCache, glb::write_glb, Material, MeshBuilder, Normals};
        use fab_elec_mech::{scene::scene_of, Pose};
        let mut mesh = MeshBuilder::new();
        for (i, name) in ["case", if lens { "lens" } else { "Lens" }].into_iter().enumerate() {
            let material = mesh.add_material(Material::named(name));
            let z = i as f32;
            assert!(mesh.polygon(material, &[[0.0, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z]],
                Normals::Crease { domain: 0, angle: 0.0 }), "each test primitive has a triangle");
        }
        let path = if lens { "asset:body-lens-test/part.glb" } else { "asset:body-no-lens-test/part.glb" };
        MeshCache::shared().put(path, &write_glb(&mesh.finish())).expect("body test GLB reads back");
        let design = fab_elec_design::Design {
            units: vec![fab_elec_design::fixture::probe::probe()],
            ..Default::default()
        };
        let mut scene = scene_of(&design, &Pose::default());
        let bodies = &mut scene.boards[0].bodies;
        // The probe's indicator explicitly uses the generic red (630 nm) Light.
        let mut led = bodies.iter().find(|body| body.emitter.is_some()).expect("fixture has a red LED").clone();
        let mut other = bodies.iter().find(|body| body.emitter.is_none()).expect("fixture has a non-LED").clone();
        let rgb = emitter_rgb(&led.emitter.as_ref().unwrap().light);
        assert_eq!(rgb[0], 1.0, "known red Light has unit red at full brightness");
        for body in [&mut led, &mut other] {
            body.mesh = MeshRef::Mesh(path.into());
            body.class = PartClass::Other;
        }
        *bodies = vec![led, other];
        (Arc::new(scene), rgb)
    }

    #[test]
    fn glow_pool_obeys_lambertian_light_and_retains_lens_instances() {
        let (scene, _) = led_scene(true);
        let mut scene = (*scene).clone();
        let mut second_led = scene.boards[0].bodies[0].clone();
        second_led.pose.t[0] += mm(2.0);
        scene.boards[0].bodies.push(second_led);
        let scene = Arc::new(scene);
        let upload = BodyUpload::build(&scene, false).expect("pool fixture builds");
        let emitters = upload.batches.iter().filter(|b| b.lens)
            .flat_map(|b| &b.instances).filter(|p| p.instance.emit != [0.0; 3]).count();
        assert_eq!(emitters, 2, "the shared lens batch contains two emitters and one dark part");
        assert!(upload.batches.iter().flat_map(|b| &b.instances)
            .all(|p| p.pool.is_some() == (p.instance.emit != [0.0; 3])),
            "only emitting lens instances build pool geometry, never cases or dark parts");
        let lens = upload.batches.iter().flat_map(|b| &b.instances).find_map(|p| p.pool.as_ref()).unwrap();
        let area = lens.area;
        assert!((area - 0.5e-6).abs() < 1e-12, "pool area is the calibration's projected half-square-mm lens in m²");
        assert!((lens.centre_mm - vec3(1.0 / 3.0, 1.0 / 3.0, 1.0)).length() < 1e-6,
            "the emitting triangle's centre is weighted by projected area");
        let up = vec3(0.0, 0.0, 1.0);
        let down = up * -1.0;
        assert!((lens.normal - up).length() < 1e-6, "the part's emission lobe leaves the top face");
        let h = 0.001;
        let centre = vec3(0.0, 0.0, h);
        let point = centre + up * h;
        let e = [2.0, 3.0, 4.0];
        let white = [1.0; 3];
        let value = pool::radiance(centre, up, area, e, point, down, white);
        let pi = std::f32::consts::PI;
        for i in 0..3 {
            let irradiance = e[i] * area / (h * h + area / pi);
            assert!((value[i] - irradiance / pi).abs() < 1e-6,
                "on-axis reflected radiance equals the Lambertian disc closed form divided by pi, channel {i}");
        }
        assert_eq!(pool::radiance(centre, up, area, e, vec3(0.0, 0.0, 0.0), up, white), [0.0; 3],
            "cos_emit rejects the board behind an upward lens plane");
        assert_eq!(pool::radiance(centre, up, area, e, point, up, white), [0.0; 3],
            "cos_in rejects a receiving face turned away from the lens");
        assert_eq!(pool::radiance(centre, up, area, e, centre + vec3(h, 0.0, 0.0), down, white), [0.0; 3],
            "coplanar lens points have zero emission cosine without a lens-ID exception");
        assert_eq!(pool::radiance(centre, up, area, e, centre, down, white), [0.0; 3],
            "the coincident point is finite and contributes no light");
        // A board below an upward lobe is dark, as checked above. The h²/d⁴ law applies
        // when the emitter faces the board: both cosines then supply h/d.
        let d = 0.02;
        let board_at = |distance: f32| vec3((distance * distance - h * h).sqrt(), 0.0, 0.0);
        let near = pool::radiance(centre, down, area, e, board_at(d), up, white);
        let far = pool::radiance(centre, down, area, e, board_at(2.0 * d), up, white);
        assert!((near[0] / far[0] / 16.0 - 1.0).abs() < 0.03,
            "far board illumination falls as h²/d⁴ for a lens facing the board");
        let r = pool::reach(e, area);
        assert!(pool::reach(e.map(|c| c * 2.0), area) > r,
            "doubling output increases reach");
        assert_eq!(pool::reach([0.0; 3], area), 0.0, "a dark emitter has zero reach");
        let edge = pool::radiance(centre, up, area, e, centre + up * r, down, white);
        assert!((edge[2] / (0.25 / 255.0) - 1.0).abs() < 1e-5,
            "reach solves the quarter-level threshold in the brightest channel");
        let clamped = pool::radiance(centre, up, area, e, point, down, [-0.5, 0.25, 1.5]);
        assert_eq!(clamped, [0.0, value[1] * 0.25, value[2]],
            "reflectance clamps each channel to [0, 1] and preserves intermediate values");

        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut layer = BodyLayer::default();
        layer.requested = Some((scene.clone(), vec![Mat4f::identity()]));
        layer.install(&mut cx, upload, scene.clone());
        let packed = layer.pool_instances.as_ref().expect("emitting lenses install a retained pool");
        assert_eq!(packed.count(), emitters, "install emits exactly one quad instance per emitting lens instance");
        for (placed, row) in layer.batches.iter().filter(|b| b.lens).flat_map(|b| &b.instances)
            .filter(|p| p.pool.is_some()).zip(packed.data().chunks_exact(12)) {
            let lens = placed.pool.as_ref().unwrap();
            let p = lens.centre_mm;
            let p = placed.instance.fab_from_body.transform_vec4(vec4(p.x, p.y, p.z, 1.0));
            assert_eq!(&row[..4], &[p.x * 0.001, p.y * 0.001, p.z * 0.001, area],
                "retained centre and area use the posed plate frame in metres");
            assert_eq!(&row[8..11], &placed.instance.emit,
                "pool and lens retain exactly the same full-output radiance");
            assert_eq!(row[11], -1.0, "install never guesses a live part row");
        }
        let comp = match &scene.boards[0].bodies[0].sel { Sel::Comp(comp) => *comp, _ => panic!("fixture LED has a component") };
        let mut sim = SimLive::default();
        Arc::make_mut(&mut sim.index).add_comp(comp, 0, None);
        layer.resolve_parts(Some(&sim));
        let packed = layer.pool_instances.as_ref().unwrap();
        assert!(packed.data().chunks_exact(12).all(|row| row[11] == 0.0),
            "the installed pool takes the lens's live row mapping");
        let retained = packed.id();
        sim.lights += 1;
        layer.resolve_parts(Some(&sim));
        layer.set_scene(&mut cx, &scene, vec![Mat4f::identity()]);
        assert_eq!(layer.pool_instances.as_ref().unwrap().id(), retained,
            "a level publication and unchanged poses preserve the retained pool publication");
        layer.set_scene(&mut cx, &scene, vec![Mat4f::translation(vec3(2.0, 0.0, 0.0))]);
        assert_ne!(layer.pool_instances.as_ref().unwrap().id(), retained,
            "a board pose change republishes pool centres");
        layer.resolve_parts(None);
        assert!(layer.pool_instances.as_ref().unwrap().data().chunks_exact(12).all(|row| row[11] == -1.0),
            "removing the simulator clears retained pool row indices");

        let (dark, _) = led_scene(false);
        layer.requested = Some((dark.clone(), vec![Mat4f::identity()]));
        layer.install(&mut cx, BodyUpload::build(&dark, false).expect("dark fixture builds"), dark);
        assert!(layer.pool_instances.is_none(), "non-emitting batches install no pool instances");
    }

    #[test]
    fn buttons_and_shafts_split_move_and_pick_in_the_body_frame() {
        use fab_elec_core::{BoardId, CompId, Contour, PolySet, P};
        use fab_elec_import::{cache::MeshCache, glb::write_glb, Material, MeshBuilder, Normals};
        use crate::model::query::Ray;
        let port = BodyPort {
            kind: BodyPortKind::Press,
            profile: PolySet::from_ring_nonzero(Contour::circle(P::ZERO, mm(1.25)).to_ring(mm(0.01))),
            at: [0, 0, mm(1.55)], dir: [0.0, 0.0, 1.0], reach: mm(10.0), margin: mm(0.75),
            travel: mm(0.25), stand: mm(0.3),
        };
        let design = fab_elec_design::Design {
            units: vec![fab_elec_design::fixture::probe::probe()], ..Default::default()
        };
        let mut scene = fab_elec_mech::scene::scene_of(&design, &fab_elec_mech::Pose::default());
        let mut button = scene.boards[0].bodies[0].clone();
        button.emitter = None;
        button.ports = vec![port.clone()];
        button.mesh = MeshRef::Box { min: [mm(-2.55), mm(-2.55), 0], max: [mm(2.55), mm(2.55), mm(1.55)] };
        button.surface = Some(Surface::new(MaterialId::lookup("stainless_303").expect("test surface exists")));
        button.sel = Sel::Comp(CompRef { board: BoardId(0), comp: CompId(0) });
        button.pose = Xform3::IDENTITY;
        let extents = |mesh: &BodyMesh| mesh.vertices.chunks_exact(BODY_VERTEX_FLOATS)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v[2]), hi.max(v[2])));
        for shape in [button.mesh.clone(), MeshRef::Extrude {
            outline: Contour::rect(P::mm(-2.55, -2.55), P::mm(2.55, 2.55), 0), height: mm(1.55)
        }] {
            let generated = generated_body_meshes(&shape, false, Some(&port), None).expect("generated button builds");
            let case = generated.iter().find(|p| !p.actuator).expect("housing exists");
            let actuator = generated.iter().find(|p| p.actuator).expect("actuator exists");
            assert!((extents(&case.mesh).1 - 1.25).abs() < 1e-6, "generated housing stops at at minus stand");
            let (bottom, top) = extents(&actuator.mesh);
            assert!((bottom - 1.25).abs() < 1e-6 && (top - 1.55).abs() < 1e-6, "actuator spans the housing top to the port");
            assert!(actuator.mesh.vertices.chunks_exact(BODY_VERTEX_FLOATS).any(|v| v[6] > 0.99), "actuator has an upward top cap");
            assert!(actuator.mesh.vertices.chunks_exact(BODY_VERTEX_FLOATS).any(|v| v[6].abs() < 0.01), "actuator has side walls");
        }

        // A triangle above the housing but with one vertex beyond the cylinder stays case;
        // so does a triangle inside the radius but exactly on the housing.
        let mut file = MeshBuilder::new();
        let material = file.add_material(Material::named("case"));
        for triangle in [
            [[-0.4, -0.4, 1.55], [0.4, -0.4, 1.55], [0.0, 0.4, 1.55]],
            [[0.0, 0.0, 1.55], [2.0, 0.0, 1.55], [0.0, 0.4, 1.55]],
            [[-0.4, -0.4, 1.25], [0.4, -0.4, 1.25], [0.0, 0.4, 1.25]],
        ] {
            assert!(file.polygon(material, &triangle, Normals::Crease { domain: 0, angle: 0.0 }), "file fixture triangles are nondegenerate");
        }
        let path = "asset:body-button-test/switch.glb";
        MeshCache::shared().put(path, &write_glb(&file.finish())).expect("button GLB reads back");
        let mut file_body = button.clone();
        file_body.mesh = MeshRef::Mesh(path.into());
        let file_prims = body_primitives(&file_body).expect("file button builds");
        assert_eq!(file_prims.iter().filter(|p| p.actuator).map(|p| p.mesh.indices.len()).sum::<usize>(), 3,
            "only the triangle wholly inside the cylinder and above the housing is actuator");
        assert_eq!(file_prims.iter().filter(|p| !p.actuator).map(|p| p.mesh.indices.len()).sum::<usize>(), 6,
            "outside triangles and triangles on the housing plane stay in the housing");

        let mut other = button.clone();
        other.sel = Sel::Comp(CompRef { board: BoardId(0), comp: CompId(1) });
        other.pose = Xform3::translation([mm(8.0), 0, 0]);
        // A pot: a housing box with a 3 mm shaft standing 12 mm out of it, its tip 19 mm up.
        let mut pot = button.clone();
        pot.sel = Sel::Comp(CompRef { board: BoardId(0), comp: CompId(2) });
        pot.pose = Xform3::translation([mm(30.0), 0, 0]);
        pot.mesh = MeshRef::Box { min: [mm(-5.0), mm(-5.0), 0], max: [mm(5.0), mm(5.0), mm(7.0)] };
        pot.ports = vec![BodyPort {
            kind: BodyPortKind::Shaft,
            profile: PolySet::from_ring_nonzero(Contour::circle(P::ZERO, mm(3.0)).to_ring(mm(0.01))),
            at: [0, 0, mm(19.0)], dir: [0.0, 0.0, 1.0], reach: mm(10.0), margin: mm(0.5), travel: 0, stand: mm(12.0),
        }];
        scene.boards[0].pose = Xform3::IDENTITY;
        scene.boards[0].bodies = vec![button, other, pot];
        let scene = Arc::new(scene);
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut layer = BodyLayer::default();
        layer.requested = Some((scene.clone(), vec![Mat4f::identity()]));
        let data = BodyUpload::build(&scene, false).expect("two generated buttons build");
        let expected_look = body_look(scene.boards[0].bodies[0].surface.as_ref());
        assert!(data.batches.iter().flat_map(|b| &b.instances).all(|p|
            (p.instance.base, p.instance.metal, p.instance.translucency, p.instance.f0) == expected_look),
            "the generated case and actuator both use the part's look");
        // Allocates Cx geometry handles only: no renderer, window or GPU is run.
        layer.install(&mut cx, data, scene.clone());
        let comp = CompRef { board: BoardId(0), comp: CompId(0) };
        assert!(layer.set_held(&[comp]), "down reports a changed held set");
        assert!(!layer.set_held(&[comp]), "the same held set does not record again");
        for batch in &layer.batches {
            for p in &batch.instances {
                let expected = if batch.actuator && p.comp == Some(comp) { -0.25 } else { 0.0 };
                assert!((p.instance.fab_from_body.v[14] - expected).abs() < 1e-6, "only the held component's actuator moves by travel");
            }
        }
        let above = Ray { origin: vec3(0.0, 0.0, 0.01), dir: vec3(0.0, 0.0, -1.0) };
        let at_rest = [None];
        let (hit, distance) = layer.press_at(&above, &at_rest).expect("the actuator is pickable from above while held");
        assert_eq!(hit, comp, "the nearest actuator reports its component");
        assert!((distance - 0.00845).abs() < 1e-7, "pick distance is in plate metres");
        assert!(layer.press_at(&Ray { origin: vec3(0.004, 0.0, 0.01), ..above }, &at_rest).is_none(), "a ray beside the body misses");
        assert!(layer.press_at(&Ray { origin: vec3(0.002, 0.0, 0.01), ..above }, &at_rest).is_none(), "a ray on the housing outside the actuator misses");
        assert!(layer.turn_at(&above, &at_rest).is_none(), "a button is no shaft");
        let pot = CompRef { board: BoardId(0), comp: CompId(2) };
        let over_pot = Ray { origin: vec3(0.031, 0.0, 0.03), ..above };
        let (hit, distance) = layer.turn_at(&over_pot, &at_rest).expect("the shaft's tip turns its pot");
        assert_eq!(hit, pot, "the shaft reports its pot");
        assert!((distance - 0.011).abs() < 1e-7, "the grip's top is the shaft's tip, in plate metres");
        assert!(layer.press_at(&over_pot, &at_rest).is_none(), "a shaft is no actuator");
        let through_housing = Ray { origin: vec3(0.05, 0.0, 0.003), dir: vec3(-1.0, 0.0, 0.0) };
        assert_eq!(layer.turn_at(&through_housing, &at_rest).map(|h| h.0), Some(pot), "the housing under the shaft takes the drag too");
        assert!(layer.turn_at(&Ray { origin: vec3(0.05, 0.0, -0.001), ..through_housing }, &at_rest).is_none(), "the grip stops at the body's seat");
        assert!(layer.set_held(&[]), "release reports a change");
        assert!(!layer.set_held(&[]), "unchanged release does not record again");
        assert!(layer.batches.iter().flat_map(|b| &b.instances).all(|p| p.instance.fab_from_body.v[14].abs() < 1e-6),
            "release restores every actuator");
        let live_pose = Xform3::axis_angle([mm(2.0), mm(3.0), mm(4.0)], [0.0, 1.0, 0.0], Angle::QUARTER);
        let live: Arc<[(Sel, Xform3)]> = vec![(Sel::Comp(comp), live_pose)].into();
        assert!(layer.set_live_poses(Some(&live)), "a drag moves the button and its pick frame");
        let sideways = Ray { origin: vec3(0.012, 0.003, 0.004), dir: vec3(-1.0, 0.0, 0.0) };
        let shift_mm = vec3(12.0, 20.0, 5.0);
        let moved = [Some(Mat4f::translation(shift_mm))];
        let shifted = |ray: Ray| Ray { origin: ray.origin + shift_mm * 0.001, ..ray };
        assert_eq!(layer.press_at(&shifted(sideways), &moved).map(|h| h.0), Some(comp),
            "the rotated button is pickable where the board carries it");
        assert!(layer.press_at(&sideways, &moved).is_none(), "the moved button misses at its rest position");
        assert_eq!(layer.turn_at(&shifted(over_pot), &moved).map(|h| h.0), Some(pot),
            "the shaft is pickable where the board carries it");
        assert!(layer.turn_at(&over_pot, &moved).is_none(), "the moved shaft misses at its rest position");
        for (turn, component, rest_tip_mm) in [
            (false, comp, vec3(3.55, 3.0, 4.0)),
            (true, pot, vec3(30.0, 0.0, 19.0)),
        ] {
            let (_, tip) = layer.port_tips(turn, &moved).into_iter().find(|(id, _)| *id == component)
                .expect("the moved control keeps its grip point");
            assert!((tip - (rest_tip_mm + shift_mm) * 0.001).length() < 1e-7,
                "the grip point for {component:?} follows the board's lift and slide in plate metres");
        }
        assert_eq!(layer.press_at(&sideways, &at_rest).map(|h| h.0), Some(comp), "picking follows a translated, rotated body");
        layer.set_held(&[comp]);
        let actuator = layer.batches.iter().filter(|b| b.actuator).flat_map(|b| &b.instances)
            .find(|p| p.comp == Some(comp)).expect("held actuator exists");
        assert!((actuator.instance.fab_from_body.v[12] - 1.75).abs() < 1e-6, "travel follows the body's rotated press axis");

        let flat = BodyPort { stand: 0, ..port };
        assert!(actuator_mesh(&flat).indices.is_empty(), "zero stand invents no plunger geometry");
        assert!(PressCylinder::of(&flat).unwrap().hit(vec3(0.0, 0.0, 10.0), vec3(0.0, 0.0, -1.0)).is_some(),
            "zero stand still supplies a pickable actuator");
    }

    #[test]
    fn caster_revision_tracks_content_and_poses_but_not_live_rows_or_still_samples() {
        use makepad_render::gpu_lightmap::CascadeCasters;
        use fab_elec_view::live::{LiveRows, PART_OFF};
        let (scene, _) = led_scene(true);
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut gpu = SceneGpu::default();
        let member = gpu.board_layer.casters.clone();
        let mut layer = BodyLayer::with_casters(member.clone());
        let mut state = AppState::default();
        let sun = [0.35, 0.8, 0.45];
        gpu.sdf.borrow_mut().set_casters(&state, &render_from_fab(), sun);
        let initial = gpu.casters.borrow().revision();
        layer.requested = Some((scene.clone(), vec![Mat4f::identity()]));
        layer.install(&mut cx, BodyUpload::build(&scene, false).expect("LED caster fixture builds"), scene.clone());
        assert_ne!(gpu.casters.borrow().revision(), initial, "a body upload moves the scene caster revision");
        let stable = gpu.casters.borrow().revision();
        for _ in 0..3 {
            layer.set_scene(&mut cx, &scene, vec![Mat4f::identity()]);
            layer.set_clip_planes(&[]);
            member.borrow_mut().set_clip_planes(&[]);
            gpu.sdf.borrow_mut().set_casters(&state, &render_from_fab(), sun);
            assert_eq!(gpu.casters.borrow().revision(), stable, "repeated frames and unchanged full poses retain the cascades");
        }
        // StillAa changes the noise and the lit projection, while cascade fitting retains the
        // unjittered projection. Its repeated caster publication is the ordinary lit pass's.
        let projection = Mat4f::identity();
        let jittered = makepad_render::jittered_projection(projection, makepad_render::still_aa_offset(1), (800.0, 600.0));
        assert_ne!(projection.v, jittered.v, "the still-view sample has a different lit projection");
        gpu.renderer.set_noise_seed(1);
        gpu.renderer.set_csm_fit_projection(Some(projection));
        member.borrow_mut().set_clip_planes(&sdf::planes_in_world(view_planes(&state).as_slice(), &render_from_fab()));
        gpu.sdf.borrow_mut().set_casters(&state, &render_from_fab(), sun);
        assert_eq!(gpu.casters.borrow().revision(), stable, "a still-view sample retains the caster revision");

        let comp = match &scene.boards[0].bodies[0].sel { Sel::Comp(comp) => *comp, _ => panic!("LED fixture has a component") };
        let mut sim = SimLive::default();
        Arc::make_mut(&mut sim.index).add_comp(comp, 0, None);
        let mut rows = LiveRows::with_off(PART_OFF);
        rows.resize(1);
        for level in [1.0, 0.25] {
            rows.set(0, [0.7, level, 0.0, 0.0]);
            sim.parts = Some(rows.texture(&mut cx).clone());
            layer.resolve_parts(Some(&sim));
            layer.set_scene(&mut cx, &scene, vec![Mat4f::identity()]);
            assert_eq!(gpu.casters.borrow().revision(), stable, "LED levels and live part rows do not invalidate cascades");
        }
        layer.resolve_parts(None);
        assert_eq!(gpu.casters.borrow().revision(), stable, "resolving a different part-row mapping leaves shadows alone");
        let moved = Mat4f::translation(vec3(2.0, 0.0, 0.0));
        layer.set_scene(&mut cx, &scene, vec![moved]);
        let posed = gpu.casters.borrow().revision();
        assert_ne!(posed, stable, "update_poses moves the revision when a body pose changes");
        layer.set_scene(&mut cx, &scene, vec![moved]);
        assert_eq!(gpu.casters.borrow().revision(), posed, "the same full poses again preserve the revision");
        let mut pose = scene.boards[0].bodies[0].pose;
        pose.t[0] += mm(1.0);
        let live: Arc<[(Sel, Xform3)]> = vec![(scene.boards[0].bodies[0].sel.clone(), pose)].into();
        assert!(layer.set_live_poses(Some(&live)), "a changed live pose moves a body");
        let dragged = gpu.casters.borrow().revision();
        assert_ne!(dragged, posed, "a moved live body invalidates cascades");
        let same_live: Arc<[(Sel, Xform3)]> = live.to_vec().into();
        assert!(!layer.set_live_poses(Some(&same_live)), "equal live poses with a new identity move no body");
        assert_eq!(gpu.casters.borrow().revision(), dragged, "equal live poses preserve the revision");
        let empty: Arc<[(Sel, Xform3)]> = Vec::new().into();
        layer.set_live_poses(Some(&empty));
        assert_ne!(gpu.casters.borrow().revision(), dragged, "clearing a moved live pose restores the scene and invalidates cascades");

        let content = member.borrow().content();
        let moves_a = [Some(Mat4f::translation(vec3(0.0, 0.0, 3.0)))];
        let moves_b = [Some(Mat4f::translation(vec3(0.0, 0.0, 7.0)))];
        member.borrow_mut().set_moves(&moves_a);
        let pane_a = gpu.casters.borrow().revision();
        member.borrow_mut().set_moves(&moves_a);
        assert_eq!(gpu.casters.borrow().revision(), pane_a, "repeating a pane's moves keeps its cascade revision");
        member.borrow_mut().set_moves(&moves_b);
        assert_ne!(gpu.casters.borrow().revision(), pane_a, "a different holder pose changes the cascade revision");
        assert_eq!(member.borrow().content(), content, "holder pose changes preserve the caster content counter");
        member.borrow_mut().set_moves(&moves_a);
        assert_eq!(gpu.casters.borrow().revision(), pane_a, "returning to the first pane recovers its cascade revision");

        let mut added = (*scene).clone();
        let extra = added.boards[0].bodies[0].clone();
        added.boards[0].bodies.push(extra);
        let added = Arc::new(added);
        for (scene, property) in [(&added, "adding a body"), (&scene, "removing a body")] {
            let before = gpu.casters.borrow().revision();
            layer.requested = Some((scene.clone(), vec![moved]));
            layer.install(&mut cx, BodyUpload::build(scene, false).expect("edited caster fixture builds"), scene.clone());
            assert_ne!(gpu.casters.borrow().revision(), before, "{property} invalidates cascades at upload");
        }
        let before = gpu.casters.borrow().revision();
        member.borrow_mut().set_clip_planes(&[[1.0, 0.0, 0.0, -0.002]]);
        let cut = gpu.casters.borrow().revision();
        assert_ne!(cut, before, "changing an electronics section plane invalidates cascades");
        member.borrow_mut().set_clip_planes(&[[1.0, 0.0, 0.0, -0.002]]);
        assert_eq!(gpu.casters.borrow().revision(), cut, "publishing the same section plane preserves the revision");
        let elec = member.borrow().revision();
        state.scene_state.section.enabled = true;
        state.scene_state.section.boxed = Some(Aabb { min: vec3(0.0, 0.0, 0.0), max: vec3(0.01, 0.01, 0.01) });
        gpu.sdf.borrow_mut().set_casters(&state, &render_from_fab(), sun);
        assert_eq!(member.borrow().revision(), elec, "an SDF-only revision leaves the electronics member unchanged");
        assert_ne!(gpu.casters.borrow().revision(), cut, "SceneCasters includes the SDF layer's own revision");
    }

    #[test]
    fn led_lens_samples_live_parts_without_rebuilding_bodies() {
        use fab_elec_view::live::{LiveRows, PART_OFF};
        let (mut scene, rgb) = led_scene(true);
        // the test lens is one triangle of half a square millimetre facing up, the light's way
        let full = full_output(rgb, 0.5);
        let stated = Surface::new(fab_materials::MaterialId::lookup("gold_contact").expect("the stated material exists"));
        Arc::make_mut(&mut scene).boards[0].bodies[0].surface = Some(stated);
        let upload = BodyUpload::build(&scene, false).expect("LED scene builds");
        assert_eq!(upload.batches.len(), 2, "case and lens are shared batches");
        assert!(upload.emitting_lens, "a coloured LED lens enables live repaints");
        for batch in &upload.batches {
            assert_eq!(batch.instances.len(), 2, "LED and non-LED share each primitive");
            for placed in &batch.instances {
                let expected = if batch.lens && placed.body == 0 { full } else { [0.0; 3] };
                assert!(near(placed.instance.emit, expected),
                    "only the LED lens emits: its flux from its area against the key, in emitter_rgb(light)'s colour ({:?} against {expected:?})", placed.instance.emit);
                assert_eq!(placed.instance.part_row, -1.0, "build never bakes simulation numbering");
                if placed.body == 0 {
                    let instance = &placed.instance;
                    assert_eq!((instance.base, instance.metal, instance.translucency, instance.f0), body_look(Some(&stated)),
                        "a stated surface overrides both case and lens looks while the file keeps its lens flag");
                }
            }
        }
        let comp = |body: usize| match &scene.boards[0].bodies[body].sel {
            Sel::Comp(comp) => *comp,
            _ => panic!("fixture bodies must retain their CompRef"),
        };
        // SimLive owns an Arc<ValueIndex>; deliberately number the other part before the LED.
        let mut sim = SimLive::default();
        let index = Arc::make_mut(&mut sim.index);
        index.generation = 7;
        index.add_comp(comp(1), 0, None);
        let (k, _) = index.add_comp(comp(0), 0, None);
        assert_eq!(k, 1, "the LED is numbered at row k, not its body index");
        let mut rows = LiveRows::with_off(PART_OFF);
        rows.resize(2);
        rows.set(k, [0.7, 1.0, 0.0, 0.0]);
        let mut cx = Cx::new(Box::new(|_, _| {}));
        sim.parts = Some(rows.texture(&mut cx).clone());
        let mut layer = BodyLayer::default();
        layer.requested = Some((scene.clone(), vec![Mat4f::identity()]));
        layer.install(&mut cx, upload, scene.clone());
        layer.resolve_parts(Some(&sim));
        let sample = |placed: &PlacedBody, rows: &LiveRows| {
            let row = placed.instance.part_row;
            let brightness = if row < 0.0 { 0.0 } else { rows.row(row as usize)[1] };
            placed.instance.emit.map(|channel| channel * brightness)
        };
        for batch in &layer.batches {
            for placed in &batch.instances {
                let expected = if batch.lens && placed.body == 0 { full } else { [0.0; 3] };
                if batch.lens && placed.body == 0 {
                    assert_eq!(placed.comp, Some(comp(0)), "the LED lens retains the body's CompRef");
                    assert_eq!(placed.instance.part_row, k as f32, "glow recording resolves ValueIndex::comp to row k");
                }
                assert!(near(sample(placed, &rows), expected), "row y=1 lights only the LED lens, irrespective of row x");
            }
        }
        let hash = scene.hash;
        let builds = layer.build_requests();
        let before = BodyLiveKeys::of_sim(Some(&sim));
        rows.set(k, [0.7, 0.25, 0.0, 0.0]);
        sim.parts = Some(rows.texture(&mut cx).clone());
        sim.t = 0.01;
        sim.lights += 1;
        let after = BodyLiveKeys::of_sim(Some(&sim));
        assert_eq!(before.rows, after.rows, "row publication preserves texture and numbering");
        assert_ne!(before.lights, after.lights, "changed LED levels request a glow repaint");
        layer.set_scene(&mut cx, &scene, vec![Mat4f::identity()]);
        assert_eq!(scene.hash, hash, "changing LED brightness leaves ElecScene hash unchanged");
        assert_eq!(layer.build_requests(), builds, "changing a live row requests no BodyUpload build");
        assert!(!layer.building(), "a value-only publication leaves no body build in flight");
        for batch in &layer.batches {
            for placed in &batch.instances {
                let expected = if batch.lens && placed.body == 0 { full.map(|c| c * 0.25) } else { [0.0; 3] };
                assert!(near(sample(placed, &rows), expected), "retained instances sample changed brightness without re-resolving rows");
            }
        }

        let mut renumbered = SimLive::default();
        renumbered.parts = sim.parts.clone();
        let index = Arc::make_mut(&mut renumbered.index);
        index.generation = 8;
        index.add_comp(comp(0), 0, None);
        assert_ne!(after.rows, BodyLiveKeys::of_sim(Some(&renumbered)).rows,
            "a new numbering re-records glow even with the same texture");
        layer.resolve_parts(Some(&renumbered));
        let lens = layer.batches.iter().find(|batch| batch.lens).unwrap();
        assert_eq!(lens.instances[0].instance.part_row, 0.0, "the next recording takes the LED's new row");
        assert_eq!(lens.instances[1].instance.part_row, -1.0, "an unnumbered component resolves to -1");
        layer.resolve_parts(None);
        assert!(layer.batches.iter().flat_map(|batch| &batch.instances).all(|p| p.instance.part_row == -1.0),
            "without ElecLive every part row is -1");
        assert_eq!(layer.build_requests(), builds, "renumbering also reuses body geometry");
    }

    #[test]
    fn an_emitter_glows_through_its_lens_only() {
        let (scene, rgb) = led_scene(false);
        let upload = BodyUpload::build(&scene, false).expect("older LED model builds");
        assert_eq!(upload.batches.len(), 2, "a model without a lens still draws both primitives");
        assert!(upload.batches.iter().all(|batch| !batch.lens), "only the exact file material name lens marks a lens");
        assert!(!upload.emitting_lens, "no lens means no live repaint loop");
        assert!(upload.batches.iter().flat_map(|batch| &batch.instances).all(|p| p.instance.emit == [0.0; 3]),
            "an emitter never falls back to glowing the case or a differently named material");

        let mut generated = (*scene).clone();
        for body in &mut generated.boards[0].bodies {
            body.mesh = MeshRef::Box { min: [0; 3], max: [mm(1.0); 3] };
            body.surface = None;
        }
        let upload = BodyUpload::build(&generated, false).expect("generated emitter body builds");
        assert_eq!(upload.batches.len(), 3, "a generated emitter's two batches stay separate from a non-emitter with the same mesh");
        assert!(upload.emitting_lens, "a generated emitter's top lens enables live repaints");
        let lens = upload.batches.iter().find(|batch| batch.lens).expect("the generated emitter has a lens batch");
        // the generated 1 mm cube's top is one square millimetre
        assert!(lens.instances.iter().all(|p| p.body == 0 && near(p.instance.emit, full_output(rgb, 1.0))),
            "only the generated emitter's lens instances emit: the flux from its top's area against the key");
        assert!(lens.mesh.indices.chunks_exact(3).all(|triangle|
            triangle.iter().all(|&index| lens.mesh.vertices[index as usize * BODY_VERTEX_FLOATS + 6] > 0.5)),
            "every lens triangle has three upward body-frame normals");
        let surface = Surface::new(MaterialId::lookup("epoxy_lens_diffused").expect("the LED lens material exists"));
        for placed in &lens.instances {
            let instance = &placed.instance;
            assert_eq!((instance.base, instance.metal, instance.translucency, instance.f0), body_look(Some(&surface)),
                "the generated lens uses the diffused epoxy lens material");
        }
        let case = upload.batches.iter().find(|batch| !batch.lens && batch.instances.iter().any(|p| p.body == 0))
            .expect("the generated emitter retains a non-lens case batch");
        assert!(case.mesh.indices.chunks_exact(3).all(|triangle|
            triangle.iter().any(|&index| case.mesh.vertices[index as usize * BODY_VERTEX_FLOATS + 6] <= 0.5)),
            "the generated case excludes every top lens triangle");
        assert!(upload.batches.iter().filter(|batch| !batch.lens).flat_map(|batch| &batch.instances)
            .all(|p| p.instance.emit == [0.0; 3]), "generated sides, bottom and non-emitters emit nothing");
        for batch in [case, lens] {
            let vertex_count = batch.mesh.vertices.len() / BODY_VERTEX_FLOATS;
            assert!(batch.mesh.indices.iter().all(|&index| (index as usize) < vertex_count),
                "each split primitive indexes only its own vertices");
            assert_eq!(batch.mesh.indices.iter().copied().collect::<HashSet<_>>().len(), vertex_count,
                "each split primitive retains only vertices used by its triangles");
        }
        let ordinary: Vec<_> = upload.batches.iter().filter(|batch| batch.instances.iter().any(|p| p.body == 1)).collect();
        assert_eq!(ordinary.len(), 1, "a generated non-emitter keeps one batch beside an emitter with the same mesh");
        assert!(!ordinary[0].lens, "the generated non-emitter's batch has no lens");

        generated.boards[0].bodies[0].emitter = None;
        let upload = BodyUpload::build(&generated, false).expect("generated non-emitter bodies build");
        assert_eq!(upload.batches.len(), 1, "generated non-emitters share one unchanged batch");
        assert!(!upload.batches[0].lens && !upload.emitting_lens, "generated non-emitters have no lens or live repaint loop");
        assert!(upload.batches[0].instances.iter().all(|p| p.instance.emit == [0.0; 3]),
            "generated non-emitters emit nothing");
    }

    #[test]
    fn body_transforms_and_appearance() {
        use fab_elec_core::{Contour, P};

        let turn = Xform3::axis_angle([mm(3.0), mm(-2.0), mm(1.6)], [0.0, 0.0, 1.0], Angle::QUARTER);
        let flip = Xform3::flipped_over([mm(-1.0), mm(4.0), mm(-0.2)]);
        for transform in [turn, flip] {
            for p in [[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [-0.5, 0.25, 1.6]] {
                let expected = transform.apply_f(p.map(|v| v * 1e6)).map(|v| (v * 1e-6) as f32);
                let actual = xform_mat(&transform).transform_vec4(vec4(p[0] as f32, p[1] as f32, p[2] as f32, 1.0));
                for (a, e) in [actual.x, actual.y, actual.z].into_iter().zip(expected) {
                    assert!((a - e).abs() < 1e-5, "matrix must agree with apply_f after nm-to-mm conversion: {transform:?}, {p:?}");
                }
                assert_eq!(actual.w, 1.0, "point transform must preserve homogeneous W");
            }
        }
        let both = Mat4f::mul(&xform_mat(&turn), &xform_mat(&flip));
        let origin = both.transform_vec4(vec4(0.0, 0.0, 0.0, 1.0));
        let top = both.transform_vec4(vec4(0.0, 0.0, 1.0, 1.0));
        assert!((top.x - origin.x).abs() < 1e-6 && (top.y - origin.y).abs() < 1e-6 && (top.z - origin.z + 1.0).abs() < 1e-6,
            "board * bottom body must point the body's +Z toward plate -Z");
        let composed = turn.then(&flip).apply_f([0.0; 3]).map(|v| (v * 1e-6) as f32);
        for (a, e) in [origin.x, origin.y, origin.z].into_iter().zip(composed) {
            assert!((a - e).abs() < 1e-5, "matrix composition must use Xform3::then's child-first order");
        }

        let (scene, _) = led_scene(true);
        let mut scene = (*scene).clone();
        let board = &mut scene.boards[0];
        board.outline = vec![Contour::rect(P::ZERO, P::mm(10.0, 6.0), 0)];
        board.thickness = mm(1.6);
        let top_pose = Xform3::translation([mm(2.0), mm(1.0), board.thickness]);
        board.bodies[0].pose = top_pose;
        board.bodies[1].pose = Xform3::new([mm(2.0), mm(1.0), 0], [0.0, 0.0, 1.0, 0.0]);
        let mask = look_of(&board.art.stackup).mask;
        let scene = Arc::new(scene);
        let upload = BodyUpload::build(&scene, false).expect("seated bodies build");
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut layer = BodyLayer::default();
        layer.install(&mut cx, upload, scene.clone());
        layer.set_scene(&mut cx, &scene, vec![Mat4f::identity()]);
        assert_eq!(layer.board_boxes, [Some([0.0, 0.0, 10.0, 6.0])],
            "scene setup caches the outer contour box in board mm");
        let check_seat = |layer: &BodyLayer, body: usize, expected: [f32; 4], property: &str| {
            let mut found = false;
            for placed in layer.batches.iter().flat_map(|batch| &batch.instances).filter(|p| p.body == body) {
                found = true;
                for (actual, expected) in placed.instance.seat.into_iter().zip(expected) {
                    assert!((actual - expected).abs() < 1e-5, "{property}: body-frame box coordinate {actual} must be {expected}");
                }
                assert!(placed.instance.seat_look[3].abs() < 1e-5,
                    "{property}: the seated board face has zero height in the body frame");
                assert!((placed.instance.seat_thickness - 1.6).abs() < 1e-5,
                    "{property}: the board's thickness rides with the seat, for a through-hole body's far side");
                assert_eq!(placed.instance.seat_look[..3], mask[..3],
                    "{property}: the near reflection retains the board mask's linear albedo");
            }
            assert!(found, "{property}: the posed body has instances");
        };
        check_seat(&layer, 0, [-2.0, -1.0, 8.0, 5.0], "top body sees the board box shifted by its XY position");
        check_seat(&layer, 1, [-8.0, -1.0, 2.0, 5.0], "bottom body sees mirrored X and sits on the bottom face");

        let poses: Arc<[(Sel, Xform3)]> = vec![(scene.boards[0].bodies[0].sel.clone(),
            Xform3::axis_angle(top_pose.t, [0.0, 0.0, 1.0], Angle::QUARTER))].into();
        assert!(layer.set_live_poses(Some(&poses)), "a live quarter turn updates the seated body");
        check_seat(&layer, 0, [-1.0, -8.0, 5.0, 2.0], "a live quarter turn swaps the board box axes");
        layer.update_poses();
        check_seat(&layer, 0, [-1.0, -8.0, 5.0, 2.0], "full re-posing preserves the live quarter turn's board box");
        layer.clear_live();
        layer.update_poses();
        check_seat(&layer, 0, [-2.0, -1.0, 8.0, 5.0], "clearing the live turn restores the scene's board box");

        let empty = ([1.0, 1.0, -1.0, -1.0], 0.0);
        assert_eq!(seat_of(None, 1.6, &xform_mat(&top_pose)), empty,
            "a missing board outline disables near reflection");
        let tilted = Xform3::axis_angle(top_pose.t, [1.0, 0.0, 0.0], Angle::QUARTER);
        assert_eq!(seat_of(layer.board_boxes[0], 1.6, &xform_mat(&tilted)), empty,
            "a tilted body disables near reflection when the face corners differ in height");
    }

    /// A part's mesh file draws as one batch per resolved material, shared by every body of the
    /// same class that names the file; an unreadable file is skipped, never a failed build.
    #[test]
    fn mesh_file_bodies_draw_one_batch_per_material() {
        use fab_elec_import::{glb::write_glb, Material, MeshBuilder, Normals};
        use fab_elec_mech::{scene::scene_of, Pose};
        use fab_materials::part_look::{resolve, FileMaterial, Place};
        let mut b = MeshBuilder::new();
        let body = b.add_material(Material { base_color: [0.2, 0.2, 0.2, 1.0], ..Material::named("body") });
        let lead = b.add_material(Material { base_color: [0.9, 0.9, 0.9, 1.0], metallic: 1.0, ..Material::named("lead") });
        let crease = Normals::Crease { domain: 0, angle: 0.5 };
        assert!(b.polygon(body, &[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 1.0, 0.0], [0.0, 1.0, 0.0]], crease));
        assert!(b.polygon(lead, &[[2.0, 0.0, 0.0], [3.0, 0.0, 0.0], [3.0, 1.0, 0.0]], crease));
        let dir = std::env::temp_dir().join(format!("fab-shell-mesh-bodies-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let glb = dir.join("part.glb");
        std::fs::write(&glb, write_glb(&b.finish())).expect("write the mesh");

        let mut scene = scene_of(&fab_elec_fixtures::ledctl(), &Pose::default());
        let bodies = &mut scene.boards[0].bodies;
        assert!(bodies.len() >= 3, "the fixture has bodies to swap");
        bodies[0].mesh = MeshRef::Mesh(glb.display().to_string());
        bodies[0].class = PartClass::Other;
        bodies[1].mesh = MeshRef::Mesh(glb.display().to_string());
        bodies[1].class = PartClass::Other;
        let stated = Surface::new(fab_materials::MaterialId::lookup("gold_contact").expect("the stated material exists"));
        bodies[1].surface = Some(stated);
        bodies[2].mesh = MeshRef::Mesh(dir.join("missing.glb").display().to_string());
        let upload = BodyUpload::build(&scene, false).expect("builds");
        let file: Vec<_> = upload.batches.iter().filter(|b| b.instances.iter().any(|p| p.body < 2)).collect();
        assert_eq!(file.len(), 2, "one batch per material of the file");
        let place = Place { lo: [0.0; 3], hi: [2.0, 1.0, 0.0], body_lo: [0.0; 3], body_hi: [3.0, 1.0, 0.0], area_share: 0.8 };
        let materials = [
            FileMaterial { name: "body", base: [0.2; 3], alpha: 1.0, metallic: 0.0, roughness: 0.5, place },
            FileMaterial { name: "lead", base: [0.9; 3], alpha: 1.0, metallic: 1.0, roughness: 0.5,
                place: Place { lo: [2.0, 0.0, 0.0], hi: [3.0, 1.0, 0.0], area_share: 0.2, ..place } },
        ];
        for (batch, material) in file.iter().zip(materials) {
            let bodies: Vec<usize> = batch.instances.iter().map(|p| p.body).collect();
            assert_eq!(bodies, [0, 1], "both bodies naming the file share its batches");
            let instance = &batch.instances[0].instance;
            let resolved = resolve(&material, PartClass::Other).surface;
            assert_eq!((instance.base, instance.metal, instance.translucency, instance.f0), body_look(Some(&resolved)),
                "each batch draws its resolved material, including roughness, translucency and reflectance");
            let instance = &batch.instances[1].instance;
            assert_eq!((instance.base, instance.metal, instance.translucency, instance.f0), body_look(Some(&stated)),
                "every shared file batch uses the stated surface for the body that declares it");
            assert_ne!(body_look(Some(&stated)), body_look(Some(&resolved)),
                "the stated look differs from each inferred file material");
        }
        assert_eq!(upload.skipped, 1, "the missing file is skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
