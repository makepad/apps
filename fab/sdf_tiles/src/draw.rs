//! The tiles into a G-buffer: (view distance, oct normal, id) per pixel, what a shade reads. One
//! instanced fan per tile from retained instances the tracer built off the UI thread.

use crate::surface::Chunk;
use crate::trace::{Cam, TILE_SLOTS};
use makepad_draw::*;

/// One tile's fan, a slot shared by every VM: four triangles around a centre, vertex x its index
/// (corners 0 to 3, centre 4). The shader takes the positions from the instance.
pub(crate) fn register_geometry(vm: &mut ScriptVm) {
    let id = vm.cx_mut().shared_geometry(id!(SdfTileGeom), |cx| {
        let mut g = makepad_draw::geometry::geometry_gen::GeometryGen::default();
        for k in 0..5 {
            g.vertices.extend_from_slice(&[k as f32, 0.0]);
        }
        for k in 0..4u32 {
            g.indices.extend_from_slice(&[4, k, (k + 1) % 4]);
        }
        g.into_geometry(cx)
    });
    let tile = Geometry::new_borrowed(id).into_script_handle(vm);
    // In a shadow a tile needs no centre: two triangles over its corners.
    let corners = vm.cx_mut().shared_geometry(id!(SdfTileCornersGeom), |cx| {
        let mut g = makepad_draw::geometry::geometry_gen::GeometryGen::default();
        for k in 0..4 {
            g.vertices.extend_from_slice(&[k as f32, 0.0]);
        }
        g.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        g.into_geometry(cx)
    });
    let corners = Geometry::new_borrowed(corners).into_script_handle(vm);
    let sdf_tiles = vm.new_module(id!(sdf_tiles));
    set_script_value!(vm, sdf_tiles.TileGeom = tile);
    set_script_value!(vm, sdf_tiles.TileCorners = corners);
}

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader.*
    use mod.draw
    use mod.geom
    use mod.sdf_tiles

    mod.draw.SdfTile = mod.std.set_type_default() do #(DrawSdfTile::script_shader(vm)){
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(geom.QuadVertex, sdf_tiles.TileGeom)
        alpha_blend: false
        depth_write: true
        world_from_obj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        gbuf_id: uniform(0.0)
        // 1: a cut face shows the part's paint (its material through the volume), lit as a face;
        // 0: it is the composite's flat cap
        cut_painted: uniform(0.0)
        // section planes in the OBJECT frame: keep a x + b y + c z + d >= 0; x = count (0..6)
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // the resin cut: one plane in the WORLD frame (none: zero), moved in place between records
        clip_cut: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_view: varying(vec3f)
        v_n: varying(vec3f)
        v_obj: varying(vec3f)
        v_cut: varying(f32)

        oct_decode: fn(e: vec2) -> vec3 {
            var n = vec3(e.x, e.y, 1.0 - abs(e.x) - abs(e.y))
            if n.z < 0.0 {
                let sx = step(0.0, n.x) * 2.0 - 1.0
                let sy = step(0.0, n.y) * 2.0 - 1.0
                n = vec3((1.0 - abs(n.y)) * sx, (1.0 - abs(n.x)) * sy, n.z)
            }
            return normalize(n)
        }

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

        vertex: fn() {
            let k = self.geom.pos.x
            // each corner's value on a section's face ([`Field::cuts_at`]), the centre their mean
            let c01 = floor(self.meta.y / 4096.0)
            let c23 = floor(self.meta.z / 4096.0)
            let c1 = self.meta.y - c01 * 4096.0
            let c3 = self.meta.z - c23 * 4096.0
            var a = self.pc
            var cut = (c01 + c1 + c23 + c3) * 0.25
            if k < 0.5 {
                a = self.p0
                cut = c01
            } else if k < 1.5 {
                a = self.p1
                cut = c1
            } else if k < 2.5 {
                a = self.p2
                cut = c23
            } else if k < 3.5 {
                a = self.p3
                cut = c3
            }
            let hi = floor(a.w / 4096.0)
            let lo = a.w - hi * 4096.0
            let n = self.oct_decode(vec2(hi, lo) / 4095.0 * 2.0 - vec2(1.0, 1.0))
            let w = self.world_from_obj * vec4(a.xyz, 1.0)
            let v = self.draw_pass.camera_view * w
            self.v_view = v.xyz
            self.v_obj = a.xyz
            self.v_n = (self.draw_pass.camera_view * (self.world_from_obj * vec4(n, 0.0))).xyz
            self.v_cut = cut
            self.vertex_pos = self.draw_pass.camera_projection * v
        }

        // half a pixel here, in the planes' units: a cut face lies on its plane only as nearly as
        // the mesh does, and must not be clipped for that
        cut_margin: fn() -> f32 {
            if self.clip_ctl.z > 0.5 { return self.clip_ctl.y * length(self.v_view) }
            return self.clip_ctl.y
        }

        clipped: fn(p: vec3) -> bool {
            let m = 0.0 - self.cut_margin()
            if dot(self.clip_cut.xyz, (self.world_from_obj * vec4(p, 1.0)).xyz) + self.clip_cut.w < m { return true }
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
            if self.clipped(self.v_obj) { discard() }
            var vn = normalize(self.v_n)
            // Past a silhouette, turn the normal to face across the view: across the pixel's ray,
            // from the eye in perspective, down the view's axis orthographic (w rises along the
            // axis only in perspective).
            let persp = clamp((self.draw_pass.camera_projection * vec4(0.0, 0.0, -1.0, 0.0)).w, 0.0, 1.0)
            let vd = normalize(self.v_view * persp + vec3(0.0, 0.0, persp - 1.0))
            let away = dot(vn, vd)
            if away > 0.0 {
                let side = vn - vd * away
                if dot(side, side) > 0.0001 { vn = normalize(side) } else { vn = 0.0 - vn }
            }
            let e = self.oct_encode(vn)
            // a cut face: id negated, as a mesh's back face, for the composite's cap, unless the
            // part's paint shows on it
            var id = self.gbuf_id
            if self.v_cut > 2047.5 && self.cut_painted < 0.5 { id = 0.0 - id }
            self.fb0 = vec4(length(self.v_view), e.x, e.y, id)
        }
    }

    // The mesh into the sun's cascades: depth from the sun, double sided. A section clips whole
    // tiles here in the vertex stage, so the pass pays for no discard.
    mod.draw.SdfTileShadow = mod.std.set_type_default() do #(DrawSdfTileShadow::script_shader(vm)){
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(geom.QuadVertex, sdf_tiles.TileCorners)
        alpha_blend: false
        cull_mode: @None
        color_format: @Rf32
        world_from_obj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        sun_rx: uniform(vec4(1.0, 0.0, 0.0, 0.0))
        sun_ry: uniform(vec4(0.0, 1.0, 0.0, 0.0))
        sun_rz: uniform(vec4(0.0, 0.0, 1.0, 0.0))
        tile_at: uniform(vec4(1.0, 1.0, 0.0, 0.0))
        depth_at: uniform(vec2(1.0, 0.0))
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_d: varying(f32)

        clipped: fn(p: vec3) -> bool {
            let c = self.clip_ctl.x
            if c > 0.5 && dot(self.clip0.xyz, p) + self.clip0.w < 0.0 { return true }
            if c > 1.5 && dot(self.clip1.xyz, p) + self.clip1.w < 0.0 { return true }
            if c > 2.5 && dot(self.clip2.xyz, p) + self.clip2.w < 0.0 { return true }
            if c > 3.5 && dot(self.clip3.xyz, p) + self.clip3.w < 0.0 { return true }
            if c > 4.5 && dot(self.clip4.xyz, p) + self.clip4.w < 0.0 { return true }
            if c > 5.5 && dot(self.clip5.xyz, p) + self.clip5.w < 0.0 { return true }
            return false
        }

        vertex: fn() {
            let k = self.geom.pos.x
            var a = self.p3
            if k < 0.5 {
                a = self.p0
            } else if k < 1.5 {
                a = self.p1
            } else if k < 2.5 {
                a = self.p2
            }
            let w = self.world_from_obj * vec4(a.xyz, 1.0)
            let nx = dot(self.sun_rx.xyz, w.xyz) + self.sun_rx.w
            let ny = dot(self.sun_ry.xyz, w.xyz) + self.sun_ry.w
            let nz = dot(self.sun_rz.xyz, w.xyz) + self.sun_rz.w
            self.v_d = nz
            var z = nz * self.depth_at.x + self.depth_at.y
            // past the far plane: a clipped tile goes whole
            if self.clipped(self.pc.xyz) { z = 2.0 }
            self.vertex_pos = vec4(nx * self.tile_at.x + self.tile_at.z, ny * self.tile_at.y + self.tile_at.w, z, 1.0)
        }

        fragment: fn() {
            self.fb0 = vec4(self.v_d, 0.0, 0.0, 1.0)
        }
    }

    // The host's environment lookups, shared by tile, print and force shading.
    mod.draw.SdfEnv = {
        detail_map: texture_2d(float)
        ibl_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        mat_ibl_meta: mod.draw.mat_ibl_meta
        mat_ibl_dir: mod.draw.mat_ibl_dir
        mat_ibl_level: mod.draw.mat_ibl_level
        mat_ibl_lookup: mod.draw.mat_ibl_lookup
        mat_ibl_sky_env: mod.draw.mat_ibl_sky_env
        mat_ibl_ambient: mod.draw.mat_ibl_ambient
        mat_ibl_lane_sky_env: mod.draw.mat_ibl_lane_sky_env
        mat_ibl_lane_ambient: mod.draw.mat_ibl_lane_ambient
        ggx_split_sum: mod.draw.ggx_split_sum
        sun_sky: uniform(vec3(0.28, 0.28, 0.28))
        sun_ground: uniform(vec3(0.28, 0.28, 0.28))
        world_up: uniform(vec3(0.0, 1.0, 0.0))
        // The fill on normal n and the reflection along r: the environment's lookups while one
        // is bound (as the model lanes take them), else the rig's hemisphere.
        env_fill: fn(n: vec3) -> vec3 {
            let up = normalize(self.world_up)
            let hemisphere = mix(self.sun_ground, self.sun_sky, clamp(dot(n, up) * 0.5 + 0.5, 0.0, 1.0))
            if self.ibl_ctl.x > 0.5 { return self.mat_ibl_lane_ambient(n, hemisphere) }
            return hemisphere
        }
        env_reflect: fn(r: vec3, rough: f32) -> vec3 {
            if self.ibl_ctl.x > 0.5 { return self.mat_ibl_lane_sky_env(r, rough) }
            let up = normalize(self.world_up)
            return mix(self.sun_ground, self.sun_sky, clamp(dot(r, up) * 0.5 + 0.5, 0.0, 1.0))
        }
    }

    // The parts' light, one copy for every Fab lane lit like a part (the field tiles, the board and
    // its bodies): the sun rig, GGX, the environment (`SdfEnv`), the display transform and dither.
    mod.draw.SdfLit = {
        ..mod.draw.SdfEnv,
        ..mod.draw.KeyLobe,
        sun_dir: uniform(vec3(0.35, 0.8, 0.45))
        sun_color: uniform(vec3(0.72, 0.72, 0.72))
        exposure: uniform(1.0)
        // x = 1: tone map + display gamma; y = the sun lobe's scale
        display: uniform(vec4(0.0, 1.0, 0.0, 0.0))
        output_dither_ctl: uniform(vec3(1.0, 1.0, 0.0))

        to_display: fn(linear: vec3) -> vec3 {
            if self.display.x < 0.5 { return linear }
            let mapped = clamp(
                (linear * (linear * 2.51 + vec3(0.03, 0.03, 0.03)))
                    / (linear * (linear * 2.43 + vec3(0.59, 0.59, 0.59)) + vec3(0.14, 0.14, 0.14)),
                vec3(0.0, 0.0, 0.0),
                vec3(1.0, 1.0, 1.0)
            )
            return pow(mapped, vec3(0.4545454, 0.4545454, 0.4545454))
        }

        output_dither: fn(color: vec3, wp: vec3) -> vec3 {
            if self.output_dither_ctl.z < 0.5 { return color }
            let clip = self.draw_pass.camera_projection * (self.draw_pass.camera_view * vec4(wp, 1.0))
            let p = floor(vec2(clip.x / clip.w * 0.5 + 0.5, 0.5 - clip.y / clip.w * 0.5) * self.output_dither_ctl.xy)
            let a = fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715))))
            let b = fract(52.9829189 * fract(dot(p + vec2(47.0, 17.0), vec2(0.06711056, 0.00583715))))
            let noise = (a + b - 1.0) / 255.0
            return max(color + vec3(noise, noise, noise), vec3(0.0, 0.0, 0.0))
        }

        schlick: fn(f0: vec3, cosine: f32) -> vec3 {
            return f0 + (vec3(1.0, 1.0, 1.0) - f0) * pow(1.0 - cosine, 5.0)
        }

        // The share of the fill a lane's own screen AO lets through at this pixel: 1 here; a lane
        // that takes AO in its lit draw overrides it (viewport/bodies.rs). The field tiles and the
        // board take it in the composite.
        fill_occlusion: fn() -> float { return 1.0 }
        // The share of the key a lane's shadow leaves on normal n; the fill and reflection stay.
        sun_visibility: fn(n: vec3) -> float { return 1.0 }
        // The screen AO's share of the environment's reflection, at n.v ndv and roughness rough.
        spec_occlusion: fn(ndv: float, rough: float) -> float { return 1.0 }

        // The sun on normal n, with no lobe (never occluded by a screen AO).
        sun_light: fn(n: vec3) -> vec3 {
            return self.sun_color * max(dot(n, normalize(self.sun_dir)), 0.0)
        }

        // The fill and the sun on normal n, with no lobe.
        diffuse_light: fn(n: vec3) -> vec3 {
            return self.diffuse_light_vis(n, self.sun_visibility(n))
        }

        diffuse_light_vis: fn(n: vec3, vis: float) -> vec3 {
            return self.env_fill(n) * self.fill_occlusion() + self.sun_light(n) * vis
        }

        // The sun's GGX lobe for reflectance f0 at roughness `rough` (n and v in the world).
        spec_sun: fn(n: vec3, v: vec3, f0: vec3, rough: f32) -> vec3 {
            return self.spec_sun_vis(n, v, f0, rough, self.sun_visibility(n))
        }

        spec_sun_vis: fn(n: vec3, v: vec3, f0: vec3, rough: f32, vis: float) -> vec3 {
            let r4 = clamp(rough, 0.04, 1.0)
            let l = normalize(self.sun_dir)
            let h = normalize(v + l)
            let ndl = max(dot(n, l), 0.0001)
            let ndv = max(dot(n, v), 0.0001)
            let ndh = max(dot(n, h), 0.0001)
            let vdh = max(dot(v, h), 0.0)
            let f = self.schlick(f0, vdh)
            let a2 = r4 * r4 * r4 * r4
            let den = ndh * ndh * (a2 - 1.0) + 1.0
            let dist = a2 / max(3.14159265 * den * den, 0.0001)
            let k = (r4 + 1.0) * (r4 + 1.0) * 0.125
            let geo = (ndv / max(ndv * (1.0 - k) + k, 0.0001)) * (ndl / max(ndl * (1.0 - k) + k, 0.0001))
            let direct = self.sun_color * max(dot(n, l), 0.0) * vis
            let sun_spec = direct * (dist * geo / max(4.0 * ndv * ndl, 0.0001)) * self.display.y * self.key_lobe_gain(r4)
            return sun_spec * f * self.ggx_energy(f0, self.ggx_split_sum(ndv, r4).x)
        }

        // The energy the GGX lobe's single scattering loses comes back from the light bouncing
        // between microfacets: 1 + f0 (1 / e - 1), with e the lobe's directional albedo at F0 = 1
        // (the split-sum fit's first term). Rough metals keep their brightness; dielectrics, at
        // f0 near 0.04, change by a few percent at most.
        ggx_energy: fn(f0: vec3, e: f32) -> vec3 {
            return vec3(1.0, 1.0, 1.0) + f0 * (1.0 / e - 1.0)
        }

        // The reflection along r at roughness `rough`: the environment's, unless a lane knows a
        // closer surface there. One environment lookup either way.
        near_reflect: fn(r: vec3, rough: f32) -> vec3 { return self.env_reflect(r, rough) }

        // Scale the environment reflection by the GGX lobe's split-sum terms for f0, with the
        // multiple-scattering energy back.
        spec_env: fn(n: vec3, v: vec3, f0: vec3, rough: f32) -> vec3 {
            let r4 = clamp(rough, 0.04, 1.0)
            let ndv = max(dot(n, v), 0.0001)
            let r = n * (2.0 * ndv) - v
            let t = self.ggx_split_sum(ndv, r4)
            let f_env = (f0 * (t.x - t.y) + vec3(t.y, t.y, t.y)) * self.ggx_energy(f0, t.x)
            return self.near_reflect(r, r4) * f_env * self.spec_occlusion(ndv, r4)
        }

        // A surface of albedo, roughness and metalness in the parts' light, before exposure.
        lit_surface: fn(n: vec3, v: vec3, albedo: vec3, rough: f32, metal: f32) -> vec3 {
            return self.lit_surface_f0(n, v, albedo, rough, metal, 0.04)
        }

        // Share one visibility across the key's terms and, for layered surfaces, every layer.
        lit_surface_vis: fn(n: vec3, v: vec3, albedo: vec3, rough: f32, metal: f32, vis: float) -> vec3 {
            return self.lit_surface_f0_vis(n, v, albedo, rough, metal, 0.04, vis)
        }

        // A surface in the parts' light with its dielectric reflectance at normal incidence.
        lit_surface_f0: fn(n: vec3, v: vec3, albedo: vec3, rough: f32, metal: f32, f0d: f32) -> vec3 {
            return self.lit_surface_f0_vis(n, v, albedo, rough, metal, f0d, self.sun_visibility(n))
        }

        // Share one visibility across the key's terms with the material's dielectric reflectance.
        lit_surface_f0_vis: fn(n: vec3, v: vec3, albedo: vec3, rough: f32, metal: f32, f0d: f32, vis: float) -> vec3 {
            let f0 = mix(vec3(f0d, f0d, f0d), albedo, metal)
            return albedo * ((1.0 - metal) * self.diffuse_light_vis(n, vis)) + self.spec_sun_vis(n, v, f0, rough, vis) + self.spec_env(n, v, f0, rough)
        }
    }

    mod.draw.SdfTileLit = mod.std.set_type_default() do #(DrawSdfTileLit::script_shader(vm)){
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(geom.QuadVertex, sdf_tiles.TileGeom)
        alpha_blend: false
        depth_write: true,
        ..mod.draw.SdfLit,
        // a part of several materials: its table (rows as the engine's `material_texture`)
        mat_tex: texture_2d(float)
        mat_on: uniform(0.0)
        // the paint's atlas (fab/paint_field, packed in the element lookup): this part's volume
        // header texel as the atlas stores texel numbers (`PaintLookup::paint_number`), -1 for none
        paint_map: texture_2d(float)
        paint_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        paint_hdr: uniform(0.0),
        ..mod.draw.PaintLookup,

        // a highlight of some of its features (`TileHighlight`): a weight per ordinal, in rows;
        // x: on, y: a row's width, z: rows; the colour shown pixels mix toward, by weight times its w
        pick_tex: texture_2d(float)
        pick_ctl: uniform(vec4(0.0, 1.0, 1.0, 0.0))
        pick_color: uniform(vec4(0.0, 0.0, 0.0, 0.0))

        // texel i, its row and column whole numbers to the end
        paint_texel: fn(i: u32) -> vec4 {
            let w = u32(max(self.paint_ctl.x, 1.0))
            let h = max(self.paint_ctl.y, 1.0)
            return self.paint_map.sample_nearest(vec2((f32(i % w) + 0.5) / f32(w), (f32(i / w) + 0.5) / h))
        }
        world_from_obj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        // one material: rgb and roughness; metal, unlit, and matte for the whole part; emissive light
        base: uniform(vec4(1.0, 1.0, 1.0, 0.5))
        finish: uniform(vec3(0.0, 0.0, 0.0))
        emit: uniform(vec3(0.0, 0.0, 0.0))
        // the scene light of a floored paint per unit of its share (`TileLight::floor_gain`)
        floor_gain: uniform(1.0)
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // the resin cut: one plane in the WORLD frame (none: zero), moved in place between records
        clip_cut: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_world: varying(vec3f)
        // the eye, or an orthographic view's axis toward it
        v_eye: varying(vec3f)
        v_n: varying(vec3f)
        v_obj: varying(vec3f)
        v_slot: varying(f32)
        // the tile's feature ordinal (`Tiles::drawn_ids`): flat per tile
        v_pick: varying(f32)

        oct_decode: fn(e: vec2) -> vec3 {
            var n = vec3(e.x, e.y, 1.0 - abs(e.x) - abs(e.y))
            if n.z < 0.0 {
                let sx = step(0.0, n.x) * 2.0 - 1.0
                let sy = step(0.0, n.y) * 2.0 - 1.0
                n = vec3((1.0 - abs(n.y)) * sx, (1.0 - abs(n.x)) * sy, n.z)
            }
            return normalize(n)
        }

        vertex: fn() {
            let k = self.geom.pos.x
            var a = self.pc
            if k < 0.5 {
                a = self.p0
            } else if k < 1.5 {
                a = self.p1
            } else if k < 2.5 {
                a = self.p2
            } else if k < 3.5 {
                a = self.p3
            }
            let hi = floor(a.w / 4096.0)
            let lo = a.w - hi * 4096.0
            let w = self.world_from_obj * vec4(a.xyz, 1.0)
            self.v_world = w.xyz
            self.v_obj = a.xyz
            self.v_n = self.oct_decode(vec2(hi, lo) / 4095.0 * 2.0 - vec2(1.0, 1.0))
            self.v_slot = self.meta.x
            self.v_pick = self.meta.w
            self.vertex_pos = self.draw_pass.camera_projection * (self.draw_pass.camera_view * w)
            if self.clip_ctl.z > 0.5 {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 0.0, 1.0)).xyz
            } else {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 1.0, 0.0)).xyz
            }
        }

        // half a pixel here, in the planes' units: a cut face lies on its plane only as nearly as
        // the mesh does, and must not be clipped for that
        cut_margin: fn() -> f32 {
            if self.clip_ctl.z > 0.5 { return self.clip_ctl.y * length(self.v_eye - self.v_world) }
            return self.clip_ctl.y
        }

        clipped: fn(p: vec3) -> bool {
            let m = 0.0 - self.cut_margin()
            if dot(self.clip_cut.xyz, (self.world_from_obj * vec4(p, 1.0)).xyz) + self.clip_cut.w < m { return true }
            let c = self.clip_ctl.x
            if c > 0.5 && dot(self.clip0.xyz, p) + self.clip0.w < m { return true }
            if c > 1.5 && dot(self.clip1.xyz, p) + self.clip1.w < m { return true }
            if c > 2.5 && dot(self.clip2.xyz, p) + self.clip2.w < m { return true }
            if c > 3.5 && dot(self.clip3.xyz, p) + self.clip3.w < m { return true }
            if c > 4.5 && dot(self.clip4.xyz, p) + self.clip4.w < m { return true }
            if c > 5.5 && dot(self.clip5.xyz, p) + self.clip5.w < m { return true }
            return false
        }

        mat_row: fn(slot: f32, r: f32) -> vec4 {
            return self.mat_tex.sample_nearest(vec2((slot + 0.5) / 256.0, (r + 0.5) / 3.0))
        }

        // The model lanes' shading with no occlusion and no shadow: n and v in the world, `no` the
        // normal in the part's frame, `px` the pixel's footprint there.
        pbr: fn(n: vec3, v: vec3, no: vec3, px: f32) -> vec3 {
            var albedo = self.base.xyz
            var rough = self.base.w
            var metal = self.finish.x
            var unlit = self.finish.y
            var emit = self.emit
            if self.mat_on > 0.5 {
                let slot = floor(self.v_slot + 0.5)
                let m0 = self.mat_row(slot, 0.0)
                let m1 = self.mat_row(slot, 1.0)
                albedo = m0.xyz
                rough = m0.w
                metal = m1.x
                unlit = m1.z
                emit = self.mat_row(slot, 2.0).xyz * m1.y
            }
            // its paint's light floor over the pixel (`PaintLookup::paint_shade`)
            var least = 0.0
            if self.paint_ctl.w > 0.5 {
                let painted = self.paint_shade(self.paint_hdr, self.v_obj, no, px, albedo)
                albedo = painted.xyz
                least = painted.w
                metal = self.paint_metal(self.paint_hdr, metal)
            }
            if unlit > 0.5 { return albedo + emit }
            rough = clamp(rough, 0.04, 1.0)
            // matte: the model lanes' diffuse lane, which a mesh with no material of its own takes:
            // its light on its colour, no lobes and no metal
            var lit = albedo * self.diffuse_light(n)
            if self.finish.z < 0.5 { lit = self.lit_surface(n, v, albedo, rough, metal) }
            // a floored paint (a support tint) reads where the light falls short: under an overhang
            return max(lit * self.exposure, albedo * (least * self.floor_gain)) + emit
        }

        fragment: fn() {
            if self.clipped(self.v_obj) { discard() }
            let no = normalize(self.v_n)
            var n = normalize((self.world_from_obj * vec4(no, 0.0)).xyz)
            var v = self.v_eye
            if self.clip_ctl.z > 0.5 { v = normalize(self.v_eye - self.v_world) }
            // past a silhouette: turned to face across the view, as in the G-buffer
            let away = dot(n, v)
            if away < 0.0 {
                let side = n - v * away
                if dot(side, side) > 0.0001 { n = normalize(side) } else { n = 0.0 - n }
            }
            let px = max(length(dFdx(self.v_obj)), length(dFdy(self.v_obj)))
            var color = self.to_display(self.pbr(n, v, no, px))
            if self.pick_ctl.x > 0.5 {
                let o = floor(self.v_pick + 0.5)
                let row = floor(o / self.pick_ctl.y)
                let w = self.pick_tex.sample_nearest(vec2((o - row * self.pick_ctl.y + 0.5) / self.pick_ctl.y, (row + 0.5) / self.pick_ctl.z)).x
                color = mix(color, self.pick_color.xyz, w * self.pick_color.w)
            }
            if self.display.x > 0.5 { color = self.output_dither(color, self.v_world) }
            self.fb0 = vec4(color, 1.0)
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawSdfTile {
    #[deref]
    pub draw_vars: DrawVars,
    /// The corners (object frame) and their packed normals.
    #[live]
    pub p0: Vec4f,
    #[live]
    pub p1: Vec4f,
    #[live]
    pub p2: Vec4f,
    #[live]
    pub p3: Vec4f,
    /// The centre.
    #[live]
    pub pc: Vec4f,
    #[live]
    pub meta: Vec4f,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawSdfTileShadow {
    #[deref]
    pub draw_vars: DrawVars,
    #[live]
    pub p0: Vec4f,
    #[live]
    pub p1: Vec4f,
    #[live]
    pub p2: Vec4f,
    #[live]
    pub p3: Vec4f,
    #[live]
    pub pc: Vec4f,
    #[live]
    pub meta: Vec4f,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawSdfTileLit {
    #[deref]
    pub draw_vars: DrawVars,
    #[live]
    pub p0: Vec4f,
    #[live]
    pub p1: Vec4f,
    #[live]
    pub p2: Vec4f,
    #[live]
    pub p3: Vec4f,
    #[live]
    pub pc: Vec4f,
    #[live]
    pub meta: Vec4f,
}

const _: () = assert!(TILE_SLOTS == 24);

/// Draws a part's tiles into the G-buffer, with the pass's camera: `view` leaves out the chunks it
/// cannot see, `proj` and `height` its pixel size for what a cut leaves, `planes` section planes in
/// the world frame; `cut_painted`: a cut face shows the part's paint instead of the flat cap.
#[allow(clippy::too_many_arguments)]
pub fn draw_tiles(cx: &mut Cx2d, draw: &mut DrawSdfTile, tiles: &[Option<Chunk>], view: Option<&Cam>, one_sided: bool, proj: &Mat4f, height: f32, world_from_obj: &Mat4f, planes: &[[f32; 4]], id: f32, cut_painted: bool) {
    set_frame(&*cx.cx, &mut draw.draw_vars, proj, height, world_from_obj, planes);
    draw.draw_vars.set_uniform(&*cx.cx, id!(gbuf_id), &[id]);
    draw.draw_vars.set_uniform(&*cx.cx, id!(cut_painted), &[if cut_painted { 1.0 } else { 0.0 }]);
    draw.draw_vars.options.cull_mode = if one_sided { CullMode::Back } else { CullMode::None };
    for t in tiles.iter().flatten().filter(|t| view.is_none_or(|v| v.sees_box(&t.quads))) {
        cx.add_retained_instances(&mut draw.draw_vars, &t.tiles);
    }
}

/// Where a cascade puts what it draws: the sun's rows (dot(row.xyz, world) + row.w = ndc x, y and
/// depth), the tile's place in clip space, the tile's depth window and its pixels.
#[derive(Clone, Copy)]
pub struct TileSun {
    pub rx: [f32; 4],
    pub ry: [f32; 4],
    pub rz: [f32; 4],
    pub tile: [f32; 4],
    pub depth: [f32; 2],
    pub scissor: Option<[u32; 4]>,
}

/// Draws a part's tiles into one cascade of the sun, those `keep` takes; `planes`: section planes
/// in the world frame. How many draws.
#[allow(clippy::too_many_arguments)]
pub fn draw_tiles_shadow(cx: &mut CxDraw, draw: &mut DrawSdfTileShadow, tiles: &[Option<Chunk>], keep: impl Fn(&Chunk) -> bool, sun: &TileSun, world_from_obj: &Mat4f, planes: &[[f32; 4]]) -> usize {
    let c = &*cx.cx;
    let dv = &mut draw.draw_vars;
    dv.set_uniform(c, id!(world_from_obj), &world_from_obj.v);
    dv.set_uniform(c, id!(clip_ctl), &[planes.len().min(6) as f32, 0.0, 0.0, 0.0]);
    set_clip(c, dv, world_from_obj, planes);
    dv.set_uniform(c, id!(sun_rx), &sun.rx);
    dv.set_uniform(c, id!(sun_ry), &sun.ry);
    dv.set_uniform(c, id!(sun_rz), &sun.rz);
    dv.set_uniform(c, id!(tile_at), &sun.tile);
    dv.set_uniform(c, id!(depth_at), &sun.depth);
    dv.options.scissor = sun.scissor;
    let mut draws = 0;
    for t in tiles.iter().flatten().filter(|t| keep(t)) {
        cx.add_retained_instances(&mut draw.draw_vars, &t.tiles);
        draws += 1;
    }
    draws
}

/// A part's look. Several materials: their table, `meta.x` of a tile its slot. One: its values.
#[derive(Clone, Copy)]
pub struct TileLook<'a> {
    pub table: Option<&'a Texture>,
    pub albedo: [f32; 3],
    pub rough: f32,
    pub metal: f32,
    pub unlit: bool,
    pub emit: [f32; 3],
    /// Shaded as the model lanes' diffuse lane shades a mesh with no material of its own: its
    /// colours lit, without highlights or metal, whatever its materials' finish.
    pub matte: bool,
    /// Some of its features lit up.
    pub highlight: Option<TileHighlight<'a>>,
}

/// Some of a part's features lit up: `mask` ([`highlight_texture`]) a weight per ordinal of the
/// drawn tiles' table ([`crate::trace::Tiles::drawn_ids`]); a shown pixel mixes toward `color`
/// by its feature's weight times `color`'s alpha.
#[derive(Clone, Copy)]
pub struct TileHighlight<'a> {
    pub mask: &'a Texture,
    /// How many weights the mask holds: the table's length and one.
    pub count: usize,
    pub color: [f32; 4],
}

/// The most texels in a row of a highlight's mask.
const MASK_ROW: usize = 4096;

/// The width and height of a highlight's mask of `count` weights.
fn mask_size(count: usize) -> (usize, usize) {
    let w = count.clamp(1, MASK_ROW);
    (w, count.max(1).div_ceil(w))
}

/// A highlight's mask: texel `o` (row by row) the weight of ordinal `o` (0: none, so
/// `weights[0]` is 0).
pub fn highlight_texture(cx: &mut Cx, weights: &[f32]) -> Texture {
    let (w, h) = mask_size(weights.len());
    let mut data = vec![0.0f32; w * h * 4];
    for (i, x) in weights.iter().enumerate() {
        data[i * 4] = *x;
    }
    Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width: w, height: h, data: Some(data), updated: TextureUpdated::Full })
}

fn bind(cx: &Cx, dv: &mut DrawVars, name: LiveId, texture: &Texture) {
    if let Some(slot) = dv.draw_shader_id.and_then(|s| cx.draw_shaders[s.index].mapping.textures.iter().position(|t| t.id == name)) {
        dv.set_texture(slot, texture);
    }
}

/// Shades a part's tiles in the lit pass; `view` leaves out the chunks it cannot see; `paint`: its
/// paint's atlas, the atlas's controls and the part's volume in it.
#[allow(clippy::too_many_arguments)]
pub fn draw_tiles_lit(cx: &mut Cx2d, draw: &mut DrawSdfTileLit, tiles: &[Option<Chunk>], view: Option<&Cam>, one_sided: bool, proj: &Mat4f, height: f32, world_from_obj: &Mat4f, planes: &[[f32; 4]], look: TileLook, paint: Option<(&Texture, [f32; 4], f32)>, light: &TileLight) {
    let c = &*cx.cx;
    let dv = &mut draw.draw_vars;
    set_frame(c, dv, proj, height, world_from_obj, planes);
    if let Some(table) = look.table {
        bind(c, dv, id!(mat_tex), table);
    }
    dv.set_uniform(c, id!(mat_on), &[if look.table.is_some() { 1.0 } else { 0.0 }]);
    dv.set_uniform(c, id!(base), &[look.albedo[0], look.albedo[1], look.albedo[2], look.rough]);
    dv.set_uniform(c, id!(finish), &[look.metal, if look.unlit { 1.0 } else { 0.0 }, if look.matte { 1.0 } else { 0.0 }]);
    dv.set_uniform(c, id!(emit), &look.emit);
    match look.highlight {
        Some(h) => {
            bind(c, dv, id!(pick_tex), h.mask);
            let (w, rows) = mask_size(h.count);
            dv.set_uniform(c, id!(pick_ctl), &[1.0, w as f32, rows as f32, 0.0]);
            dv.set_uniform(c, id!(pick_color), &h.color);
        }
        None => dv.set_uniform(c, id!(pick_ctl), &[0.0, 1.0, 1.0, 0.0]),
    }
    match paint {
        Some((map, ctl, hdr)) => {
            bind(c, dv, id!(paint_map), map);
            dv.set_uniform(c, id!(paint_ctl), &ctl);
            dv.set_uniform(c, id!(paint_hdr), &[hdr]);
        }
        None => {
            dv.set_uniform(c, id!(paint_ctl), &[0.0; 4]);
            dv.set_uniform(c, id!(paint_hdr), &[-1.0]);
        }
    }
    dv.set_uniform(c, id!(sun_dir), &light.dir);
    dv.set_uniform(c, id!(sun_color), &light.color);
    dv.set_uniform(c, id!(sun_sky), &light.sky);
    dv.set_uniform(c, id!(sun_ground), &light.ground);
    dv.set_uniform(c, id!(world_up), &light.up);
    dv.set_uniform(c, id!(exposure), &[light.exposure]);
    dv.set_uniform(c, id!(floor_gain), &[light.floor_gain]);
    dv.set_uniform(c, id!(display), &[if light.display { 1.0 } else { 0.0 }, light.sun_spec, 0.0, 0.0]);
    dv.set_uniform(c, id!(key_spec), &light.key_spec);
    dv.set_uniform(c, id!(key_tint), &light.key_tint);
    draw.draw_vars.options.cull_mode = if one_sided { CullMode::Back } else { CullMode::None };
    for t in tiles.iter().flatten().filter(|t| view.is_none_or(|v| v.sees_box(&t.quads))) {
        cx.add_retained_instances(&mut draw.draw_vars, &t.tiles);
    }
}

fn set_frame(c: &Cx, dv: &mut DrawVars, proj: &Mat4f, height: f32, world_from_obj: &Mat4f, planes: &[[f32; 4]]) {
    dv.set_uniform(c, id!(world_from_obj), &world_from_obj.v);
    // half a pixel's width: at one unit from the eye, or anywhere in an orthographic view
    let half_pixel = 1.0 / (proj.v[5].abs() * height.max(1.0));
    dv.set_uniform(c, id!(clip_ctl), &[planes.len().min(6) as f32, half_pixel, if proj.v[11] == 0.0 { 0.0 } else { 1.0 }, 0.0]);
    set_clip(c, dv, world_from_obj, planes);
}

/// The section planes in the object frame.
fn set_clip(c: &Cx, dv: &mut DrawVars, world_from_obj: &Mat4f, planes: &[[f32; 4]]) {
    let m = &world_from_obj.v;
    for (k, name) in [id!(clip0), id!(clip1), id!(clip2), id!(clip3), id!(clip4), id!(clip5)].into_iter().enumerate() {
        let pl = planes.get(k).copied().unwrap_or([0.0; 4]);
        // (a b c) . (M p) + d: object normal = M^T n, offset = n . t + d (column-major M)
        let mut o = [0.0f32; 4];
        for j in 0..3 {
            o[j] = pl[0] * m[j * 4] + pl[1] * m[j * 4 + 1] + pl[2] * m[j * 4 + 2];
        }
        o[3] = pl[0] * m[12] + pl[1] * m[13] + pl[2] * m[14] + pl[3];
        dv.set_uniform(c, name, &o);
    }
}

/// The material table: albedo and roughness, finish, and emissive colour, one column per slot.
fn material_texels(materials: &[makepad_csg_sdf::SdfMaterial]) -> Vec<f32> {
    let mut data = vec![0.0f32; 256 * 4 * 3];
    let dflt = makepad_csg_sdf::SdfMaterial::default();
    for i in 0..256 {
        let m = materials.get(i).unwrap_or(&dflt);
        let rows = [
            [m.albedo[0], m.albedo[1], m.albedo[2], m.rough],
            [m.metal, m.emissive, if m.unlit { 1.0 } else { 0.0 }, m.fade],
            [m.emissive_color[0], m.emissive_color[1], m.emissive_color[2], 0.0],
        ];
        for (r, row) in rows.iter().enumerate() {
            let o = (r * 256 + i) * 4;
            data[o..o + 4].copy_from_slice(row);
        }
    }
    data
}

pub fn material_texture(cx: &mut Cx, materials: &[makepad_csg_sdf::SdfMaterial]) -> Texture {
    let data = material_texels(materials);
    Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width: 256, height: 3, data: Some(data), updated: TextureUpdated::Full })
}

/// The light the shaded output uses (world frame): `dir` toward the sun.
#[derive(Clone, Copy, Debug)]
pub struct TileLight {
    pub dir: [f32; 3],
    pub color: [f32; 3],
    pub sky: [f32; 3],
    pub ground: [f32; 3],
    pub up: [f32; 3],
    pub exposure: f32,
    /// Tone map + display gamma (a BGRA8 target).
    pub display: bool,
    /// The sun's specular lobe scale: 1 where the light's units carry the
    /// diffuse lobe's 1/π (a display-space rig), π for a radiometric rig
    /// whose diffuse lobe drops it (the renderer's HDR lane, which lifts its
    /// meshes' lobe the same way): an SDF part's highlight matches a mesh's.
    pub sun_spec: f32,
    /// The renderer's bound key levels for this sun; each lobe uses its own roughness.
    pub key_spec: [f32; 2],
    /// The renderer's bound key colour and fit (`Renderer::ibl_key_tint`), for the reflection in the key's shadow.
    pub key_tint: [f32; 4],
    /// The light a floored paint (`PaintLookup::paint_shade`) shows at per unit of its share of
    /// its colour: 1 over the exposure that takes this light to the display (the renderer's
    /// `lin_ctl.z`, as its meshes floor theirs).
    pub floor_gain: f32,
}

impl Default for TileLight {
    fn default() -> Self {
        TileLight { dir: [0.35, 0.8, 0.45], color: [0.72; 3], sky: [0.28; 3], ground: [0.28; 3], up: [0.0, 1.0, 0.0], exposure: 1.0, display: false, sun_spec: 1.0, key_spec: [0.0; 2], key_tint: [0.0; 4], floor_gain: 1.0 }
    }
}
