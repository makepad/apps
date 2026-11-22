//! Lambertian lens light on aux's visible surfaces, added to the lens glow before bloom.

use super::*;
use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;

/// A quarter of an 8-bit display level in linear whites, shared with `reach` in the shader.
const POOL_THRESHOLD: f32 = 0.25 / 255.0;

/// Metres at which even a white face with both cosines one falls below POOL_THRESHOLD.
/// Built once at full output for the common y=1 case; other live levels solve it on the GPU.
pub(super) fn reach(e: [f32; 3], area: f32) -> f32 {
    let e = e[0].max(e[1]).max(e[2]);
    (e * area / (std::f32::consts::PI * POOL_THRESHOLD) - area / std::f32::consts::PI)
        .max(0.0).sqrt()
}

/// Reference for `pool_light` below: metres, unit normals, radiance in display whites.
/// Only the GPU evaluates this during rendering; the CPU reference belongs to the logic tests.
#[cfg(test)]
pub(super) fn radiance(centre: Vec3f, lens_n: Vec3f, area: f32, e: [f32; 3], p: Vec3f, n: Vec3f, rho: [f32; 3]) -> [f32; 3] {
    let l = centre - p;
    let d2 = l.dot(l);
    if d2 <= 0.0 || area <= 0.0 { return [0.0; 3]; }
    let l_hat = l / d2.sqrt();
    let cos_emit = lens_n.dot(l_hat * -1.0).max(0.0);
    let cos_in = n.dot(l_hat).max(0.0);
    let irradiance = e.map(|c| c * area * cos_emit * cos_in / (d2 + area / std::f32::consts::PI));
    std::array::from_fn(|i| rho[i].clamp(0.0, 1.0) * irradiance[i] / std::f32::consts::PI)
}

script_mod! {
    use mod.prelude.fab.*
    use mod.math.*
    use mod.shader.*
    use mod.draw
    use mod.geom
    use mod.elec_live.*

    mod.draw.DrawFabGlowPool = mod.std.set_type_default() do #(DrawFabGlowPool::script_shader(vm)){
        color_format: @Rgba16F
        blend_op: @Add
        alpha_blend: true
        depth_write: false
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(geom.QuadVertex, geom.QuadGeom)
        live_parts: texture_2d(float)
        aux: texture_2d(float)
        lit: texture_2d(float)
        glow_gain: uniform(1.0)
        l_white: uniform(1.0)
        u_aux_inv_projection: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        // Projection rows' xyz: clip component gradients in aux's view frame.
        u_projection_x: uniform(vec3(1.0, 0.0, 0.0))
        u_projection_y: uniform(vec3(0.0, 1.0, 0.0))
        u_projection_w: uniform(vec3(0.0, 0.0, 0.0))
        u_ortho: uniform(0.0)
        v_uv: varying(vec2f)
        v_aux_ray: varying(vec4f)
        v_centre: varying(vec3f)
        v_normal: varying(vec3f)
        v_emit_reach: varying(vec4f)

        // Mirrors pool::reach. The installed radius avoids a sqrt at full output.
        reach: fn(e: vec3f, y: float) -> float {
            if y == 1.0 { return self.normal_reach.w }
            let peak = max(e.x, max(e.y, e.z))
            let q = 0.25 / 255.0
            return sqrt(max(peak * self.centre_area.w / (3.14159265359 * q)
                - self.centre_area.w / 3.14159265359, 0.0))
        }

        // Exact projected sphere extrema: (c - t*w)^2 = r^2 * |axis - t*w_axis|^2.
        // Works for perspective (including jitter/off-axis) and orthographic projections.
        sphere_bounds: fn(c: float, w: float, axis: vec3f, r2: float) -> vec2f {
            let wa = self.u_projection_w
            let den = w * w - r2 * dot(wa, wa)
            if den <= 0.0 { return vec2(-1.0, 1.0) }
            let mid = c * w - r2 * dot(axis, wa)
            // Expanded discriminant avoids subtracting two nearly equal c²w² terms
            // when a distant LED projects to a very small sphere.
            let tangent = w * axis - c * wa
            let cross_axis = cross(axis, wa)
            let spread = sqrt(max(r2 * (dot(tangent, tangent) - r2 * dot(cross_axis, cross_axis)), 0.0))
            return vec2((mid - spread) / den, (mid + spread) / den)
        }

        vertex: fn() {
            self.v_uv = vec2(0.5, 0.5)
            self.v_aux_ray = vec4(0.0, 0.0, 0.0, 1.0)
            self.v_centre = (self.draw_pass.camera_view * vec4(self.centre_area.xyz, 1.0)).xyz
            self.v_normal = normalize((self.draw_pass.camera_view * vec4(self.normal_reach.xyz, 0.0)).xyz)
            self.v_emit_reach = vec4(0.0, 0.0, 0.0, 0.0)
            self.vertex_pos = vec4(0.0, 0.0, 0.0, 1.0)
            if self.emit.w >= 0.0 {
                // Explicit LOD is required for vertex-stage sampling; the parts row is live.
                let y = self.live_parts.sample_nearest(Live.net_uv(self.emit.w, self.live_parts.size()), 0.0).y
                if y > 0.0 {
                    let e = self.emit.xyz * y
                    let r = self.reach(e, y)
                    let c = self.draw_pass.camera_projection * vec4(self.v_centre, 1.0)
                    let wr = r * length(self.u_projection_w)
                    if r > 0.0 && c.w + wr > 0.0 {
                        // A sphere crossing the eye plane can cover the whole screen. Otherwise
                        // tangent bounds are tight; clamp them to the viewport before rasterising.
                        var bx = vec2(-1.0, 1.0)
                        var by = vec2(-1.0, 1.0)
                        if c.w > wr {
                            // Half a pixel in NDC covers floating-point rounding of the tangent.
                            let pad = vec2(1.0) / self.aux.size()
                            bx = clamp(self.sphere_bounds(c.x, c.w, self.u_projection_x, r * r) + vec2(-pad.x, pad.x), vec2(-1.0), vec2(1.0))
                            by = clamp(self.sphere_bounds(c.y, c.w, self.u_projection_y, r * r) + vec2(-pad.y, pad.y), vec2(-1.0), vec2(1.0))
                        }
                        let ndc = vec2(mix(bx.x, bx.y, self.geom.pos.x), mix(by.y, by.x, self.geom.pos.y))
                        self.v_uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
                        // Homogeneous unprojection is affine in screen xy. Interpolate it with
                        // clip w=1, then divide in the pixel stage: exactly the aux inverse
                        // projection at that pixel, without a per-pixel matrix multiply.
                        self.v_aux_ray = self.u_aux_inv_projection * vec4(ndc, 0.0, 1.0)
                        self.v_emit_reach = vec4(e, r * r)
                        self.vertex_pos = vec4(ndc, 0.0, 1.0)
                    }
                }
            }
        }

        oct_decode: fn(e: vec2f) -> vec3f {
            let nz = 1.0 - abs(e.x) - abs(e.y)
            let t = max(-nz, 0.0)
            let sx = step(0.0, e.x) * 2.0 - 1.0
            let sy = step(0.0, e.y) * 2.0 - 1.0
            return normalize(vec3(e.x - t * sx, e.y - t * sy, nz))
        }

        // Line-for-line reference: pool::radiance. No artistic strength or distance gain.
        pool_light: fn(p: vec3f, n: vec3f, rho: vec3f) -> vec3f {
            let l = self.v_centre - p
            let d2 = dot(l, l)
            let area = self.centre_area.w
            if d2 <= 0.0 || area <= 0.0 { return vec3(0.0) }
            let l_hat = l / sqrt(d2)
            let cos_emit = max(dot(self.v_normal, -l_hat), 0.0)
            let cos_in = max(dot(n, l_hat), 0.0)
            let irradiance = self.v_emit_reach.xyz * area * cos_emit * cos_in / (d2 + area / 3.14159265359)
            return clamp(rho, vec3(0.0), vec3(1.0)) * irradiance / 3.14159265359
        }

        pixel: fn() -> vec4f {
            let uv = self.v_uv
            let a = self.aux.sample_nearest(uv)
            if a.w == 0.0 || a.x <= 0.0 { discard() }
            // Print beads store colour instead of an oct normal; they are outside this layer.
            if a.y < -1.5 { discard() }
            let h = self.v_aux_ray
            if abs(h.w) <= 0.000000000001 { discard() }
            let ray = h.xyz / h.w
            var p = vec3(0.0)
            if self.u_ortho > 0.5 {
                p = vec3(ray.xy, -sqrt(max(a.x * a.x - dot(ray.xy, ray.xy), 0.0)))
            } else {
                p = normalize(ray) * a.x
            }
            let l = self.v_centre - p
            if dot(l, l) > self.v_emit_reach.w { discard() }
            let n = self.oct_decode(a.yz)
            // One display white is glow_gain lit units: the lens writes emit*y*glow_gain,
            // then the overlay applies display_exposure = 1/glow_gain on the HDR lane.
            // Thus L_white = glow_gain (1 on the legacy lane), NOT its reciprocal. Lens,
            // reflected light and the halo all use that same display-white calibration.
            let rho = self.lit.sample_nearest(uv).xyz / self.l_white
            return vec4(self.pool_light(p, n, rho) * self.glow_gain, 0.0)
        }

        // Add also sums alpha: preserve the lens coverage by contributing zero alpha.
        fragment: fn() { self.fb0 = self.pixel() }
    }
}

/// Three vec4s per retained lens: plate centre (m) and area (m²), unit normal and full-output
/// reach (m), full-output emission (display whites) and live part row. No live levels are baked.
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabGlowPool {
    #[deref]
    pub draw_vars: DrawVars,
    #[live]
    pub centre_area: Vec4f,
    #[live]
    pub normal_reach: Vec4f,
    #[live]
    pub emit: Vec4f,
}

impl DrawFabGlowPool {
    pub(super) fn draw(&mut self, cx: &mut Cx3d, instances: &RetainedInstances, parts: &Texture, aux: &Texture, lit: &Texture, glow_gain: f32) -> u32 {
        let Some(scene) = cx.scene_state_3d() else { return 0 };
        let vars = &mut self.draw_vars;
        if instances.count() == 0 || !vars.can_instance() { return 0; }
        set_texture_named(vars, cx.cx, id!(live_parts), parts);
        set_texture_named(vars, cx.cx, id!(aux), aux);
        set_texture_named(vars, cx.cx, id!(lit), lit);
        vars.set_uniform(cx.cx, id!(glow_gain), &[glow_gain]);
        vars.set_uniform(cx.cx, id!(l_white), &[glow_gain]);
        vars.set_uniform(cx.cx, id!(u_aux_inv_projection), &scene.projection.invert().v);
        let p = &scene.projection.v;
        vars.set_uniform(cx.cx, id!(u_projection_x), &[p[0], p[4], p[8]]);
        vars.set_uniform(cx.cx, id!(u_projection_y), &[p[1], p[5], p[9]]);
        vars.set_uniform(cx.cx, id!(u_projection_w), &[p[3], p[7], p[11]]);
        vars.set_uniform(cx.cx, id!(u_ortho), &[if p[11] == 0.0 { 1.0 } else { 0.0 }]);
        vars.options.alpha_blend = true;
        let area = cx.add_retained_instances(vars, instances);
        if matches!(area, Area::Empty) { return 0; }
        vars.area = cx.update_area_refs(vars.area, area);
        1
    }
}
