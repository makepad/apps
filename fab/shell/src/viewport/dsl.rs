//! Every shader the viewport owns, and the widget registration.
//!
//! The lit pass uses `libs/render`'s shaders. The viewport owns its aux and
//! composite shaders and the aux adapter for CORTAO.
//!
//! | shader | pass | what it is |
//! |---|---|---|
//! | `DrawFabAux` | aux (RGBA32F + D32) | the G-buffer: view depth, octahedral view normal, signed element id. Also the depth prepass the lit pass reuses, and the authority on visibility / drag offset / section clipping. |
//! | `DrawFabCortaoPrepass` | screen AO | CORTAO's host hooks: reconstruct aux's radial distance and decode its normal into CORTAO's rigid view frame (a print bead, which stores its colour there, takes its depth's normal). |
//! | `DrawFabComposite` | composite | Solid's clay and cavity, Realtime's screen AO and the traced parts' cascade shadow, selection outline, hover, x-ray, section caps, background gradient — one full-screen quad over `lit` + `aux` + the screen AO. |
//! | `DrawFabGrid` | composite | the infinite adaptive ground grid + axis lines, occluded against `aux` rather than a depth buffer. |
//! | `DrawFabResinLayer` | composite | a resin printer's layer image on the parts' cut at its top, occluded against `aux` like the grid. |
//! | `DrawFabSeeThrough` | composite | View › See-Through: a part's paint as a hologram through its box, marched front to back to what `aux` holds behind it, each material in its colour, the part's surface a thin shell (`see_through.rs`). On a hi-dpi display it marches in its own pass (`see`) at half the pixels each way. |
//! | `DrawFabSeeUp` | composite | the half-resolution See-Through taken up, edge-aware: a texel counts where its march stopped at this pixel's depth and looks like the nearest one, so no colour spreads across a shell's edge. |
//! | `DrawFabLines` | composite | the build volume's edges, the model's outline at the shown layer and the ring round the printing nozzle's tip, screen-width lines occluded against `aux` the same way. |
//! | `DrawFabPickLines` | composite | the picks' outlines and what a click would take, over a halo; what `aux` holds in front of is dimmed and dashed, not dropped. |
//! | `DrawFabGlowOver` | window | lens light and its halo over the finished picture as a display difference; LED levels leave the scene and still average untouched. |
//! | `DrawFabRebuild` | window | the rebuilding sweep over a part, masked by `aux` ids: a status animation never records a scene pass. |
//!
//! The aux buffer's channels, once, so the composite and the grid cannot
//! drift apart:
//!
//! ```text
//! r = length(view_position) in meters (radial distance, not -z; 0 = background)
//! g = octahedral normal x   (view space, folded toward the camera)
//! b = octahedral normal y
//! a = signed element id: +(id+1) on a front face, -(id+1) on a back face,
//!     0 = background. The sign is what makes a section cap findable: with a
//!     plane active the only back faces left on screen are the inside of a
//!     cut solid.
//! ```

use super::*;
use makepad_widgets_core::*;

pub mod cortao_weights {
    use super::*;

    script_mod! {
        use mod.prelude.fab.*
        use mod.widgets.*
        use mod.math.*
        use mod.shader.*
        use mod.draw

        // CORTAO-owned: the screen AO's weights for every lane that takes them (the composite's
        // screen_ao and the board's part bodies). Route any change to these through main.
        mod.draw.FabCortaoWeights = {
            // The ambient fill's share at visibility ao and strength s (the model lanes' weight).
            cortao_ambient: fn(ao: float, strength: float) -> float { return 1.0 - (1.0 - ao) * strength }
            // The environment reflection's share at n.v ndv and perceptual roughness rough (Lagarde
            // and de Rousiers 2014, on the GGX alpha = rough^2), from the same weighted visibility:
            // 1 at full visibility or strength 0; toward 1 as the surface gets glossier.
            cortao_specular: fn(ndv: float, ao: float, rough: float, strength: float) -> float {
                let a = self.cortao_ambient(ao, strength)
                return clamp(pow(ndv + a, exp2(-16.0 * rough * rough - 1.0)) - 1.0 + a, 0.0, 1.0)
            }
        }
    }
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*
    use mod.math.*
    use mod.shader.*
    use mod.draw
    use mod.elec_live.*

    mod.fab_geom = {
        FabVertex: #(fab_vertex_pod(vm))
        FabGeom: #(fab_placeholder_geom(vm))
    }

    // =====================================================================
    // AUX — the G-buffer
    // =====================================================================
    mod.draw.FabPartSurface = {
        alpha_blend: false
        depth_write: true
        // Back faces stay: with a section plane on, the inside of a cut
        // solid IS the cap, and it is the only thing left to draw there.
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_geom.FabVertex, mod.fab_geom.FabGeom)
        elem_map: texture_2d(float)
        // x = lut width, y = lut height, z unused, w = enabled
        elem_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // x = 1 in the ghost pass: only the parts drawn as ghosts (their eye off) draw, the rest
        // collapse; 0 in the main pass, where the ghosts collapse (their state is hidden)
        ghost_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // Pipe glow poses glass that aux hides and uses the pipe texels for visibility.
        admit_hidden: uniform(0.0)
        // x = plane count (0..6); planes keep a*x+b*y+c*z+d >= 0, Fab space
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // a resin layer's cut: one more plane (none: zero), moved in place between records
        clip_cut: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_view: varying(vec3f)
        v_vnormal: varying(vec3f)
        v_world: varying(vec3f)
        v_elem: varying(float)
        v_uncut: varying(float)

        elem_texel: fn(i: float) -> vec4f {
            let ew = max(self.elem_ctl.x, 1.0)
            let eh = max(self.elem_ctl.y, 1.0)
            return self.elem_map.sample_nearest(vec2((modf(i, ew) + 0.5) / ew, (floor(i / ew) + 0.5) / eh))
        }

        oct_encode: fn(n: vec3f) -> vec2f {
            let l1 = abs(n.x) + abs(n.y) + abs(n.z)
            if l1 < 0.00000001 {
                return vec2(0.0, 0.0)
            }
            let e = n / l1
            if e.z >= 0.0 {
                return vec2(e.x, e.y)
            }
            let sx = step(0.0, e.x) * 2.0 - 1.0
            let sy = step(0.0, e.y) * 2.0 - 1.0
            return vec2((1.0 - abs(e.y)) * sx, (1.0 - abs(e.x)) * sy)
        }

        vertex: fn() {
            var pos = self.geom.pos
            var nrm = self.geom.normal
            self.v_uncut = 0.0
            if self.elem_ctl.w > 0.5 {
                let stride = max(self.elem_ctl.z, 1.0)
                let base = self.geom.element * stride
                let f = self.elem_texel(base)
                if stride > 3.5 {
                    self.v_uncut = self.elem_texel(base + 3.0).w
                }
                var on = f.x >= 0.5 || self.admit_hidden > 0.5
                if self.ghost_ctl.x > 0.5 {
                    on = self.elem_texel(base + 2.0).w >= 0.5
                }
                if !on {
                    // Zero-area triangle: a hidden element costs no fragments.
                    pos = vec3(0.0, 0.0, 0.0)
                } else {
                    // The lookup is in the LIT model's space (Y up: render = (x, z, -y) of Fab's Z up);
                    // this pass draws the Fab stream, so the move goes there and back.
                    if stride > 3.5 {
                        let r0 = self.elem_texel(base + 1.0).xyz
                        let r1 = self.elem_texel(base + 2.0).xyz
                        let r2 = self.elem_texel(base + 3.0).xyz
                        let p = vec3(pos.x, pos.z, 0.0 - pos.y)
                        let q = vec3(dot(r0, p), dot(r1, p), dot(r2, p))
                        pos = vec3(q.x, 0.0 - q.z, q.y)
                        let n = vec3(nrm.x, nrm.z, 0.0 - nrm.y)
                        let m = vec3(dot(r0, n), dot(r1, n), dot(r2, n))
                        nrm = vec3(m.x, 0.0 - m.z, m.y)
                    }
                    pos = pos + vec3(f.y, 0.0 - f.w, f.z)
                }
            }
            self.v_world = pos
            self.v_elem = self.geom.element
            let view4 = self.draw_pass.camera_view * vec4(pos.x, pos.y, pos.z, 1.0)
            self.v_view = view4.xyz
            self.v_vnormal = (self.draw_pass.camera_view
                * vec4(nrm.x, nrm.y, nrm.z, 0.0)).xyz
            // Render-only part priority from the scene's measured coplanar
            // groups. One rank is slightly larger than a D32 ULP; the clamp
            // bounds the effect even for pathological conflict chains.
            let priority = min(max(self.geom.pad, 0.0), 4095.0)
            let bias = min(priority * 0.0000001, 0.00005)
            let clip = self.draw_pass.camera_projection * view4
            self.vertex_pos = vec4(clip.x, clip.y, clip.z - bias * clip.w, clip.w)
        }

        back_facing: fn(n: vec3f) -> bool {
            // Perspective rays start at the eye. Orthographic rays follow the view axis.
            let persp = clamp((self.draw_pass.camera_projection * vec4(0.0, 0.0, -1.0, 0.0)).w, 0.0, 1.0)
            return dot(n, normalize(self.v_view * persp + vec3(0.0, 0.0, persp - 1.0))) > 0.0
        }

        clipped: fn(w: vec3f) -> bool {
            if dot(self.clip_cut.xyz, w) + self.clip_cut.w < 0.0 { return true }
            if self.clip_ctl.x < 0.5 {
                return false
            }
            if dot(self.clip0.xyz, w) + self.clip0.w < 0.0 { return true }
            if self.clip_ctl.x > 1.5 {
                if dot(self.clip1.xyz, w) + self.clip1.w < 0.0 { return true }
            }
            if self.clip_ctl.x > 2.5 {
                if dot(self.clip2.xyz, w) + self.clip2.w < 0.0 { return true }
            }
            if self.clip_ctl.x > 3.5 {
                if dot(self.clip3.xyz, w) + self.clip3.w < 0.0 { return true }
            }
            if self.clip_ctl.x > 4.5 {
                if dot(self.clip4.xyz, w) + self.clip4.w < 0.0 { return true }
            }
            if self.clip_ctl.x > 5.5 {
                if dot(self.clip5.xyz, w) + self.clip5.w < 0.0 { return true }
            }
            return false
        }
    }

    mod.draw.DrawFabAux = mod.std.set_type_default() do #(DrawFabAux::script_shader(vm)){
        ..mod.draw.FabPartSurface,
        color_format: @Rgba32F

        pixel: fn() -> vec4f {
            // (the printer's own geometry stays whole under a test print's cut: `elements::UNCUT`)
            if self.clipped(self.v_world) && self.v_uncut > 0.0 - 0.5 {
                discard()
            }
            let depth = length(self.v_view)
            var n = normalize(self.v_vnormal)
            var sign_id = 1.0
            if self.back_facing(n) {
                sign_id = 0.0 - 1.0
                n = 0.0 - n
            }
            let e = self.oct_encode(n)
            return vec4(depth, e.x, e.y, (self.v_elem + 1.0) * sign_id)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // Raw HDR emission from posed pipes, with aux occlusion and live LED levels.
    mod.draw.DrawFabPartGlow = mod.std.set_type_default() do #(part_glow::DrawFabPartGlow::script_shader(vm)){
        ..mod.draw.FabPartSurface,
        color_format: @Rgba16F
        depth_write: false
        pipe_glows: texture_2d(float)
        live_parts: texture_2d(float)
        aux: texture_2d(float)
        live_on: uniform(0.0)
        glow_gain: uniform(1.0)
        top_normal_z: uniform(#(part_glow::TOP_NORMAL_Z))
        side_bottom: uniform(#(part_glow::SIDE_BOTTOM))
        side_top: uniform(#(part_glow::SIDE_TOP))

        glow_texel: fn(i: float) -> vec4f {
            let size = self.pipe_glows.size()
            return self.pipe_glows.sample_nearest(vec2((modf(i, size.x) + 0.5) / size.x, (floor(i / size.x) + 0.5) / size.y))
        }

        pixel: fn() -> vec4f {
            if self.clipped(self.v_world) && self.v_uncut > 0.0 - 0.5 { discard() }
            let emit = self.glow_texel(self.v_elem * 2.0)
            if self.live_on < 0.5 || emit.w < 0.0 { discard() }
            // Reconstruct from the shared aux varyings so aux itself gains no vertex work.
            let spos = self.draw_pass.camera_projection * vec4(self.v_view, 1.0)
            let sp = spos.xy / max(spos.w, 0.000001)
            let uv = vec2(sp.x * 0.5 + 0.5, 0.5 - sp.y * 0.5)
            let a = self.aux.sample_nearest(uv)
            let distance = length(self.v_view)
            if !(a.w == self.v_elem + 1.0 && abs(a.x - distance) <= distance * 0.0002 + 0.000001) {
                // Glass has no id in aux. Only its front faces in front of the opaque picture glow.
                if a.w != 0.0 && a.x > 0.0 && distance >= a.x - a.x * 0.0001 { discard() }
                if self.glow_texel(self.v_elem * 2.0 + 1.0).z < 0.5 { discard() }
                if self.back_facing(normalize(self.v_vnormal)) { discard() }
            }
            let normal = (self.draw_pass.camera_inv * vec4(self.v_vnormal, 0.0)).xyz
            if dot(normal, normal) == 0.0 { discard() }
            var share = 1.0
            if normalize(normal).z < self.top_normal_z {
                let z = self.glow_texel(self.v_elem * 2.0 + 1.0)
                var t = 0.0
                if z.y > z.x { t = clamp((self.v_world.z - z.x) / (z.y - z.x), 0.0, 1.0) }
                share = self.side_bottom + (self.side_top - self.side_bottom) * t
            }
            let y = self.live_parts.sample_nearest(Live.net_uv(emit.w, self.live_parts.size())).y
            let g = emit.xyz * y * share * self.glow_gain
            if max(g.x, max(g.y, g.z)) <= 0.0 { discard() }
            return vec4(g, 1.0)
        }

        fragment: fn() { self.fb0 = self.pixel() }
    }

    // =====================================================================
    // CORTAO — aux supplies surfaces instead of hardware depth
    // =====================================================================
    mod.draw.DrawFabCortaoPrepass = mod.std.set_type_default() do #(DrawFabCortaoPrepass::script_shader(vm)){
        ..mod.draw.DrawCortaoPrepass
        // The inherited depth_tex is bound to aux (RGBA32F). Aux's camera is -z-forward,
        // in metres, but stores radial distance; CORTAO also chooses its own rigid origin.
        u_aux_inv_projection: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        u_cortao_from_aux: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))

        // The centre of the aux texel `pixel` reads: at half size (`u_size` the AO's lattice, aux
        // full size) a lattice pixel's centre falls between aux texels, and the depth must be
        // reconstructed along the ray of the texel it came from. At full size it is the pixel's.
        aux_uv: fn(pixel: vec2f) -> vec2f {
            let aux_size = self.depth_tex.size()
            let texel = min(floor((pixel + vec2(0.5, 0.5)) / self.u_size.xy * aux_size), aux_size - vec2(1.0, 1.0))
            return (texel + vec2(0.5, 0.5)) / aux_size
        }

        surface: fn(pixel: vec2f) -> vec4f {
            if pixel.x < 0.0 || pixel.y < 0.0 || pixel.x >= self.u_size.x || pixel.y >= self.u_size.y {
                return vec4(0.0, 0.0, 0.0, 0.0)
            }
            let uv = self.aux_uv(pixel)
            let aux = self.depth_tex.sample_nearest(uv)
            if aux.w == 0.0 || aux.x <= 0.0 { return vec4(0.0, 0.0, 0.0, 0.0) }
            let h = self.u_aux_inv_projection * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0)
            if abs(h.w) <= 0.000000000001 { return vec4(0.0, 0.0, 0.0, 0.0) }
            let ray = h.xyz / h.w
            var p = vec3(0.0, 0.0, 0.0)
            if self.u_proj_mode > 0.5 {
                // Parallel rays keep their x/y; remove their off-axis distance from |p|.
                p = vec3(ray.xy, -sqrt(max(aux.x * aux.x - dot(ray.xy, ray.xy), 0.0)))
            } else {
                p = normalize(ray) * aux.x
            }
            let view = self.u_cortao_from_aux * vec4(p, 1.0)
            if view.z >= 0.0 { return vec4(0.0, 0.0, 0.0, 0.0) }
            return vec4(view.xyz, 1.0)
        }

        surface_normal: fn(pixel: vec2f, p: vec3f) -> vec3f {
            let e = self.depth_tex.sample_nearest(self.aux_uv(pixel)).yz
            // A print bead stores its lit colour where the normal goes (y below -1.5, which no
            // octahedral normal reaches; `aux_pixel` in print.rs): its normal is its depth's.
            if e.x < -1.5 { return self.depth_normal(pixel, p) }
            // Same signed octahedral decode as the composite, already folded toward the eye.
            let nz = 1.0 - abs(e.x) - abs(e.y)
            let t = max(-nz, 0.0)
            let sx = step(0.0, e.x) * 2.0 - 1.0
            let sy = step(0.0, e.y) * 2.0 - 1.0
            let n = vec3(e.x - t * sx, e.y - t * sy, nz)
            return normalize((self.u_cortao_from_aux * vec4(n, 0.0)).xyz)
        }
    }

    // The same final dither for the retained picture and its display-space overlays.
    mod.draw.FabDither = {
        // xy = 1/pixel size, zw = pixel size
        u_texel: uniform(vec4(0.001, 0.001, 1000.0, 1000.0))

        // Triangular dither of one 8-bit step in screen space (interleaved gradient noise),
        // the last colour operation: a tone-mapped gradient lands on the 8-bit target without
        // bands. The legacy lane's lit pass dithered already.
        dithered: fn(c: vec3f, uv: vec2f) -> vec4f {
            if self.u_exposure.y < 0.5 || self.u_exposure.w > 0.5 {
                return vec4(c, 1.0)
            }
            let p = floor(uv * self.u_texel.zw)
            let a = fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715))))
            let b = fract(52.9829189 * fract(dot(p + vec2(47.0, 17.0), vec2(0.06711056, 0.00583715))))
            let n = (a + b - 1.0) / 255.0
            return vec4(max(c + vec3(n, n, n), vec3(0.0, 0.0, 0.0)), 1.0)
        }
    }

    // =====================================================================
    // COMPOSITE — Solid and Realtime, in one quad
    // =====================================================================
    mod.draw.DrawFabComposite = mod.std.set_type_default() do #(DrawFabComposite::script_shader(vm)){
        ..mod.draw.DrawQuad,
        ..mod.draw.SunCascades,
        ..mod.draw.FabCortaoWeights,
        // `display`, `u_exposure` and `u_balance`: on the HDR lane the composite exposes and
        // tone maps `lit`, then dithers to the 8-bit target (u_exposure.w = 1 leaves the dither
        // out, a debug switch)
        ..mod.draw.FabDisplay,
        ..mod.draw.FabDither,
        lit: texture_2d(float)
        aux: texture_2d(float)
        // x = realtime (0 Solid, 1 Realtime), y = x-ray, z = section active,
        // w = 1 under studio lighting (the viewport's own backdrop, not the renderer's sky)
        u_mode: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // x = outlines, y = cavity strength, z = disable field sun, w = edge light (Realtime)
        u_flags: uniform(vec4(1.0, 0.25, 0.0, 0.0))
        // x = hovered element + 1 (0 = none), yzw unused
        u_hover: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // the selection outline: x = its width in pixels (about 2 at the display's scale),
        // y = 1 while anything can be selected (0: no pixel looks for one)
        u_ring: uniform(vec4(2.0, 0.0, 0.0, 0.0))
        u_bg_top: uniform(vec4(0.247, 0.247, 0.247, 1.0))
        u_bg_bottom: uniform(vec4(0.169, 0.169, 0.169, 1.0))
        u_select: uniform(vec4(0.914, 0.416, 0.169, 1.0))
        u_select_dim: uniform(vec4(0.647, 0.275, 0.114, 1.0))
        u_hover_col: uniform(vec4(1.0, 1.0, 1.0, 1.0))
        u_cap: uniform(vec4(0.541, 0.541, 0.541, 1.0))
        elem_map: texture_2d(float)
        elem_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // The sun cascades, for the surfaces the renderer does not light (the SDF parts).
        csm_map: texture_depth(float)
        // The ghost layer: the G-buffer of the parts drawn see-through (their eye off), made
        // with only those parts; they are absent from `aux` and `lit`, so they cast no shadow.
        ghost_aux: texture_2d(float)
        // x = 1 when the layer has anything, y = the share of the part that shows, z = how near
        // the surface drawn a ghost is that surface (metres), w = the share of a part on its way in
        u_ghost: uniform(vec4(0.0, 0.25, 0.0003, 0.5))
        // x = plate face element (-1: none), y/z = minor/major line strength, w unused
        u_plate: uniform(vec4(-1.0, 0.0, 0.0, 0.0))
        // The plate's min xy and max xy in Fab world metres (Z up); its face is at z = 0.
        u_plate_rect: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // The aux pass's camera, inverted; the sun in the render frame.
        u_inv_proj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        u_inv_view: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        u_sun_dir: uniform(vec3(0.35, 0.8, 0.45))
        // x = key luminance, y = ambient luminance, zw unused
        u_key: uniform(vec4(3.0, 1.0, 0.0, 0.0))
        // The renderer's screen AO over aux (libs/render ssao.rs, half resolution; x =
        // visibility, 1 open); u_ao.x its strength on the ambient light (0: off), u_ao.y how
        // much of the direct light it takes (the model lanes' ao_direct). The meshes took the
        // ambient part in the lit pass.
        ssao_tex: texture_2d(float)
        u_ao: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // Nothing to clip against: this quad IS the pass.
        depth_clip: 0.0

        // STRAIGHT TO CLIP SPACE, and it is load-bearing.
        //
        // `DrawQuad::vertex` ends in `draw_pass.camera_projection *
        // (draw_pass.camera_view * world)` — which in a normal 2D pass is the
        // ORTHO pair makepad installs in `set_ortho_matrix`. This pass is not
        // a normal 2D pass: it carries `keep_camera_matrix = true` and
        // `set_pass_camera` writes the 3D PERSPECTIVE pair into exactly those
        // uniforms, because the grid drawn after this quad is real 3D
        // geometry and reads them. Left on the stock path the composite is
        // projected into the world as a flat billboard somewhere out in the
        // scene, the pass keeps its black clear colour, and the pane shows
        // nothing but the grid — the whole lit image never reaches the screen.
        //
        // The composite always covers the entire pass, so it needs no matrix
        // at all: `geom.pos` (0..1, y down) maps to NDC directly, and `pos`
        // stays the 0..1 uv the pixel stage samples `lit` / `aux` with.
        vertex: fn() {
            self.pos = self.geom.pos
            self.world = vec4(self.geom.pos.x, self.geom.pos.y, 0.0, 1.0)
            self.vertex_pos = vec4(
                self.geom.pos.x * 2.0 - 1.0
                1.0 - self.geom.pos.y * 2.0
                0.0
                1.0
            )
        }

        oct_decode: fn(e: vec2f) -> vec3f {
            let nz = 1.0 - abs(e.x) - abs(e.y)
            let t = max(0.0 - nz, 0.0)
            let sx = step(0.0, e.x) * 2.0 - 1.0
            let sy = step(0.0, e.y) * 2.0 - 1.0
            return normalize(vec3(e.x - t * sx, e.y - t * sy, nz))
        }

        // Selection state of an element id, straight out of the same
        // lookup the lit pass reads: 0 hidden, 1 visible, 2 selected, 3 active.
        elem_state: fn(id: float) -> float {
            if self.elem_ctl.w < 0.5 || id > 15999999.0 || id < 0.0 {
                return 1.0
            }
            let ew = max(self.elem_ctl.x, 1.0)
            let eh = max(self.elem_ctl.y, 1.0)
            let i = id * max(self.elem_ctl.z, 1.0)
            let ex = (modf(i, ew) + 0.5) / ew
            let ey = (floor(i / ew) + 0.5) / eh
            // abs: a part the SDF engine draws keeps its state negated
            return abs(self.elem_map.sample_nearest(vec2(ex, ey)).x)
        }

        // A print bead's aux id (viewport/print.rs `PRINT_AUX_ID`): 16000008 + 8 * its code, the
        // code 0 (no part) or 1 + 4 * the part's element + its selection state; the steps of 8
        // keep the code whole through the passes' float rounding. Its code, -1 for no bead.
        bead_code: fn(w: float) -> float {
            if w < 16000004.0 {
                return 0.0 - 1.0
            }
            return floor((w - 16000008.0) / 8.0 + 0.5)
        }

        // A bead the aux pass shaded carries its lit colour where others keep their normal
        // (viewport/print.rs `DrawFabPathAux`), marked by a y no octahedral normal reaches.
        bead_lit: fn(g: vec4f) -> vec3f {
            let v = 0.0 - g.y - 2.0
            let r = floor(v / 4096.0)
            let s = self.bead_shown(g)
            let e = vec3(r, v - r * 4096.0, g.z - s * 4096.0) * (4.0 / 4095.0)
            return e * e / (1.0 + 15.0 * s)
        }
        // 1 where a bead's colour is on the display already (a far bead's, `DrawFabPath::shade`),
        // else 0.
        bead_shown: fn(g: vec4f) -> float {
            return floor(g.z / 4096.0)
        }

        // The element a G-buffer id belongs to (a bead: its part's; -1: none).
        elem_of: fn(w: float) -> float {
            let c = self.bead_code(w)
            if c > 0.5 {
                return floor((c - 1.0) / 4.0 + 0.01)
            }
            if c > 0.0 - 0.5 {
                return 0.0 - 1.0
            }
            return abs(w) - 1.0
        }

        // The selection state of a G-buffer id: a bead carries its part's, an element's is
        // looked up (`elem_state`).
        state_of: fn(w: float) -> float {
            // a lit body the Electronics views selected (print.rs `SELECTED_BODY_AUX_ID`): active
            if abs(w - #(super::print::SELECTED_BODY_AUX_ID)) < 0.5 {
                return 3.0
            }
            let c = self.bead_code(w)
            if c > 0.5 {
                return c - 1.0 - 4.0 * floor((c - 1.0) / 4.0 + 0.01)
            }
            if c > 0.0 - 0.5 {
                return 1.0
            }
            return self.elem_state(abs(w) - 1.0)
        }

        // Whether the SDF engine draws element `id` (its state is negated in the lookup).
        field_drawn: fn(id: float) -> bool {
            if self.elem_ctl.w < 0.5 || id < 0.0 || id > 15999999.0 {
                return false
            }
            let ew = max(self.elem_ctl.x, 1.0)
            let eh = max(self.elem_ctl.y, 1.0)
            let i = id * max(self.elem_ctl.z, 1.0)
            return self.elem_map.sample_nearest(vec2((modf(i, ew) + 0.5) / ew, (floor(i / ew) + 0.5) / eh)).x < 0.0
        }

        // The point at view distance d (the aux pass's x) on the pixel's ray, in that pass's
        // view space.
        view_at: fn(uv: vec2f, d: float) -> vec3f {
            let ndc = vec2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0)
            let a4 = self.u_inv_proj * vec4(ndc.x, ndc.y, 0.0, 1.0)
            let b4 = self.u_inv_proj * vec4(ndc.x, ndc.y, 1.0, 1.0)
            let av = a4.xyz / a4.w
            let dv = b4.xyz / b4.w - av
            let qa = dot(dv, dv)
            let qb = 2.0 * dot(av, dv)
            let qc = dot(av, av) - d * d
            let s = ((0.0 - qb) + sqrt(max(qb * qb - 4.0 * qa * qc, 0.0))) / (2.0 * qa)
            return av + dv * s
        }

        // A view normal into the cascades' Y-up world: Fab's Z-up world (x, y, z) -> (x, z, -y).
        render_normal: fn(nv: vec3f) -> vec3f {
            let w = (self.u_inv_view * vec4(nv, 0.0)).xyz
            return normalize(vec3(w.x, w.z, 0.0 - w.y))
        }

        // What the cascades leave of the sun on a surface pixel, and its facing: (visibility,
        // ndl), (1, 0) facing away.
        sun_at: fn(g: vec4f, uv: vec2f) -> vec2f {
            let n = self.render_normal(self.oct_decode(g.yz))
            let ndl = dot(n, normalize(self.u_sun_dir))
            if ndl <= 0.0 {
                return vec2(1.0, 0.0)
            }
            let w4 = self.u_inv_view * vec4(self.view_at(uv, g.x), 1.0)
            let w = w4.xyz / w4.w
            return vec2(self.csm_vis(vec3(w.x, w.z, 0.0 - w.y), n, ndl), ndl)
        }

        // A print bead's view normal, which its G-buffer texel does not hold (its colour is
        // there): from the view distances round it, per axis the side nearer in depth, so a
        // silhouette's far side lends it no slope (the screen AO's own rule, libs/render
        // ssao.rs).
        depth_normal: fn(uv: vec2f, d: float, dl: float, dr: float, du: float, dd: float) -> vec3f {
            let tx = self.u_texel.x
            let ty = self.u_texel.y
            let p = self.view_at(uv, d)
            var ddx = self.view_at(uv + vec2(tx, 0.0), dr) - p
            if dr <= 0.0 || (dl > 0.0 && abs(dl - d) < abs(dr - d)) {
                ddx = p - self.view_at(uv - vec2(tx, 0.0), dl)
            }
            var ddy = self.view_at(uv + vec2(0.0, ty), dd) - p
            if dd <= 0.0 || (du > 0.0 && abs(du - d) < abs(dd - d)) {
                ddy = p - self.view_at(uv - vec2(0.0, ty), du)
            }
            var n = cross(ddy, ddx)
            if length(n) <= 0.000001 * length(ddx) * length(ddy) {
                return 0.0 - normalize(p)
            }
            n = normalize(n)
            if dot(n, p) > 0.0 {
                n = 0.0 - n
            }
            return n
        }

        // The screen AO on a pixel as the renderer's model lanes weigh an AO: the ambient fill
        // times 1 - (1 - ao) * u_ao.x, the direct light times mix(1, ao, u_ao.y). `direct`: the
        // key's light here (its luminance, facing and what the cascades leave of it) against
        // the fill's u_key.y. `mesh`: the renderer drew the pixel, its lane already took the
        // ambient part.
        screen_ao: fn(uv: vec2f, direct: float, mesh: bool) -> float {
            let fill = self.u_key.y
            let share = fill / max(direct + fill, 0.0001)
            let ao = self.ssao_tex.sample_nearest(uv).x
            let amb = self.cortao_ambient(ao, self.u_ao.x)
            let lit = share * amb + (1.0 - share) * mix(1.0, ao, self.u_ao.y)
            if mesh {
                return lit / (share * amb + 1.0 - share)
            }
            return lit
        }

        // The view ray's intersection with the plate face in Fab world metres.
        plate_at: fn(uv: vec2f) -> vec3f {
            let ndc = vec2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0)
            let a4 = self.u_inv_proj * vec4(ndc.x, ndc.y, 0.0, 1.0)
            let b4 = self.u_inv_proj * vec4(ndc.x, ndc.y, 1.0, 1.0)
            let av = a4.xyz / a4.w
            let dv = b4.xyz / b4.w - av
            let o4 = self.u_inv_view * vec4(av, 1.0)
            let o = o4.xyz / o4.w
            let d = (self.u_inv_view * vec4(dv, 0.0)).xyz
            let dz = (step(0.0, d.z) * 2.0 - 1.0) * max(abs(d.z), 0.000001)
            let t = (0.0 - o.z) / dz
            return o + d * t
        }

        // Box-filtered 10 / 50 mm lines, fading to their area mean below a pixel.
        plate_grid: fn(p: vec2f, fx: vec2f) -> vec2f {
            let f = max(fx, vec2(0.000001, 0.000001))
            let minor_d = abs(fract(p / 0.01 - vec2(0.5, 0.5)) - vec2(0.5, 0.5)) * 0.01
            let major_d = abs(fract(p / 0.05 - vec2(0.5, 0.5)) - vec2(0.5, 0.5)) * 0.05
            var minor = clamp((min(minor_d + 0.5 * f, vec2(0.00025, 0.00025))
                - max(minor_d - 0.5 * f, vec2(0.0 - 0.00025, 0.0 - 0.00025))) / f, vec2(0.0, 0.0), vec2(1.0, 1.0))
            var major = clamp((min(major_d + 0.5 * f, vec2(0.0004, 0.0004))
                - max(major_d - 0.5 * f, vec2(0.0 - 0.0004, 0.0 - 0.0004))) / f, vec2(0.0, 0.0), vec2(1.0, 1.0))
            minor = mix(minor, vec2(2.0 * 0.00025 / 0.01, 2.0 * 0.00025 / 0.01),
                smoothstep(vec2(0.15 * 0.01, 0.15 * 0.01), vec2(0.5 * 0.01, 0.5 * 0.01), f))
            major = mix(major, vec2(2.0 * 0.0004 / 0.05, 2.0 * 0.0004 / 0.05),
                smoothstep(vec2(0.15 * 0.05, 0.15 * 0.05), vec2(0.5 * 0.05, 0.5 * 0.05), f))
            return vec2(max(minor.x, minor.y), max(major.x, major.y))
        }

        // The studio sweep follows the camera in Fab's Z-up world: a soft, lifted horizon.
        studio_backdrop: fn(uv: vec2f) -> vec3f {
            let ndc = vec2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0)
            let a4 = self.u_inv_proj * vec4(ndc.x, ndc.y, 0.0, 1.0)
            let b4 = self.u_inv_proj * vec4(ndc.x, ndc.y, 1.0, 1.0)
            let ray = normalize((self.u_inv_view * vec4(b4.xyz / b4.w - a4.xyz / a4.w, 0.0)).xyz)
            let e = ray.z
            let horizon = self.u_bg_top.xyz * 1.12
            let above = horizon.mix(self.u_bg_bottom.xyz * 0.9, smoothstep(0.0, 0.7, e))
            let below = horizon.mix(self.u_bg_bottom.xyz, smoothstep(0.0, 0.25, 0.0 - e))
            return below.mix(above, smoothstep(0.0 - 0.15, 0.15, e))
        }

        // The selection state of a G-buffer id, 0 where nothing is drawn.
        ring_state: fn(w: float) -> float {
            if abs(w) < 0.5 {
                return 0.0
            }
            return self.state_of(w)
        }

        // The selection outline over a pixel that is not selected itself: an outer ring
        // u_ring.x pixels wide round every selected element (x) and the active one (y),
        // anti-aliased: a pixel's cover falls off over the last pixel of the reach from the
        // nearest selected pixel. A ring of taps at the full and half reach says first
        // whether one is near at all; only then the disc is searched.
        ring_cover: fn(uv: vec2f) -> vec2f {
            let reach = self.u_ring.x
            let tx = self.u_texel.x
            let ty = self.u_texel.y
            var near = 0.0
            for i in 0..12 {
                // eight at the full reach, four between them at half of it
                let fi = float(i)
                let half = step(7.5, fi)
                let a = mix(fi * 0.785398, (fi - 8.0) * 1.570796 + 0.392699, half)
                let r = reach * (1.0 - 0.5 * half)
                near = max(near, self.ring_state(self.aux.sample_nearest(uv + vec2(cos(a) * r * tx, sin(a) * r * ty)).w))
            }
            if near < 1.5 {
                return vec2(0.0, 0.0)
            }
            let k = ceil(reach + 0.5)
            let side = 2.0 * k + 1.0
            var sel = 0.0
            var act = 0.0
            for i in 0..121 {
                let fi = float(i)
                if fi >= side * side { break }
                let dx = modf(fi, side) - k
                let dy = floor(fi / side) - k
                let d = sqrt(dx * dx + dy * dy)
                let cover = clamp(reach + 0.5 - d, 0.0, 1.0)
                if cover > 0.0 {
                    let st = self.ring_state(self.aux.sample_nearest(uv + vec2(dx * tx, dy * ty)).w)
                    sel = max(sel, cover * step(1.5, st))
                    act = max(act, cover * step(2.5, st))
                }
            }
            return vec2(sel, act)
        }

        // The ring painted over `c` (a pixel that is not selected).
        ring_over: fn(c: vec3f, uv: vec2f) -> vec3f {
            if self.u_flags.x < 0.5 || self.u_ring.y < 0.5 {
                return c
            }
            let cover = self.ring_cover(uv)
            return c.mix(self.u_select_dim.xyz, cover.x).mix(self.u_select.xyz, cover.y)
        }

        // A ghost part over `c`: its colour lit by the way it faces the eye, mixed in at its
        // share where it is nearer than what the pass drew (`depth`, 0 for nothing) by more
        // than the tie (on that surface it is that surface: the part shows).
        ghost_over: fn(c: vec3f, uv: vec2f, depth: float) -> vec3f {
            if self.u_ghost.x < 0.5 {
                return c
            }
            let gg = self.ghost_aux.sample_nearest(uv)
            if gg.w == 0.0 {
                return c
            }
            if depth > 0.0 && gg.x > depth - self.u_ghost.z {
                return c
            }
            let ew = max(self.elem_ctl.x, 1.0)
            let eh = max(self.elem_ctl.y, 1.0)
            let i = (abs(gg.w) - 1.0) * max(self.elem_ctl.z, 1.0)
            let a2 = i + 2.0
            let st = self.elem_map.sample_nearest(vec2((modf(a2, ew) + 0.5) / ew, (floor(a2 / ew) + 0.5) / eh)).w
            let a3 = i + 3.0
            let pk = self.elem_map.sample_nearest(vec2((modf(a3, ew) + 0.5) / ew, (floor(a3 / ew) + 0.5) / eh)).w
            let r = floor(pk / 65536.0)
            let gr = floor((pk - r * 65536.0) / 256.0)
            let b = pk - r * 65536.0 - gr * 256.0
            let n = self.oct_decode(gg.yz)
            let facing = clamp(abs(n.z), 0.0, 1.0)
            let col = vec3(r, gr, b) / 255.0
            let shade = col * (0.55 + 0.45 * facing)
            if st < 4.5 {
                return c.mix(shade, self.u_ghost.y)
            }
            // A part on its way in (its state past ARRIVING): more of it, and its outline and
            // creases in its own colour, lighter, so it reads as that part.
            let edge = self.ghost_edge(uv, gg.w, n)
            return c.mix(shade, self.u_ghost.w).mix(col.mix(vec3(1.0, 1.0, 1.0), 0.3), edge * 0.85)
        }

        // 1 where the ghost layer's surface at `uv` (element `w`, normal `n`) ends or turns by
        // more than about 25 degrees within two pixels, else 0.
        ghost_edge: fn(uv: vec2f, w: float, n: vec3f) -> float {
            var edge = 0.0
            for i in 0..8 {
                let fi = float(i)
                let k = modf(fi, 4.0)
                let d = 1.0 + floor(fi / 4.0)
                // right, left, down, up; one pixel away, then two
                let dx = step(k, 0.5) - step(0.5, k) * step(k, 1.5)
                let dy = step(1.5, k) * step(k, 2.5) - step(2.5, k)
                let o = self.ghost_aux.sample_nearest(uv + vec2(dx * d * self.u_texel.x, dy * d * self.u_texel.y))
                let ends = step(0.5, abs(o.w - w))
                let turns = step(dot(self.oct_decode(o.yz), n), 0.9)
                edge = max(edge, max(ends, turns))
            }
            return edge
        }

        pixel: fn() -> vec4f {
            let uv = self.pos
            let g = self.aux.sample_nearest(uv)
            let realtime = self.u_mode.x > 0.5
            // Background: Solid's vertical gradient, the camera-aware studio sweep, or the
            // renderer's sky for Realtime under the sun and sky.
            var bg = self.u_bg_top.xyz.mix(self.u_bg_bottom.xyz, uv.y)
            if realtime && self.u_mode.w < 0.5 {
                bg = self.display(self.lit.sample(uv).xyz)
            } else if realtime && self.u_mode.w > 0.5 {
                bg = self.studio_backdrop(uv)
            }
            if g.w == 0.0 {
                // The selection's outer ring reaches onto the background too: a part's outline
                // against the sky is the one that matters most.
                return self.dithered(self.ghost_over(self.ring_over(bg, uv), uv, 0.0), uv)
            }
            // Print preview beads (viewport/print.rs): their own lit colour, with their
            // part's selection outline and hover feedback.
            let bead = g.w > 16000004.0

            let depth = g.x
            let n = self.oct_decode(g.yz)
            let front = g.w > 0.0
            let id = self.elem_of(g.w)
            let state = self.state_of(g.w)

            // ---- section cap ------------------------------------------
            // With a plane on, a surviving back face is the inside of a cut
            // solid: paint it flat so the part reads as a solid rather than
            // a hollow shell.
            let is_cap = (self.u_mode.z > 0.5) && (front == false)

            // ---- neighbours (one tap ring, shared by every effect) -----
            let tx = self.u_texel.x
            let ty = self.u_texel.y
            let gl = self.aux.sample_nearest(uv + vec2(0.0 - tx, 0.0))
            let gr = self.aux.sample_nearest(uv + vec2(tx, 0.0))
            let gu = self.aux.sample_nearest(uv + vec2(0.0, 0.0 - ty))
            let gd = self.aux.sample_nearest(uv + vec2(0.0, ty))

            // ---- cavity (full-res curvature, ~0.25 in Solid) -----------
            // A 1-pixel ring on a tessellated surface reads every triangle
            // edge as a crease and prints hatching. A few-pixel ring at
            // full resolution, gated by depth, keeps only real concave and
            // convex detail. u_flags.y is the strength (0 when off).
            var cavity = 1.0
            if (self.u_flags.y > 0.001 || self.u_flags.w > 0.001) && !bead {
                let stepx = tx * 3.0
                let stepy = ty * 3.0
                let cl = self.aux.sample_nearest(uv + vec2(0.0 - stepx, 0.0))
                let cr = self.aux.sample_nearest(uv + vec2(stepx, 0.0))
                let cu = self.aux.sample_nearest(uv + vec2(0.0, 0.0 - stepy))
                let cd = self.aux.sample_nearest(uv + vec2(0.0, stepy))
                let nl = self.oct_decode(cl.yz)
                let nr = self.oct_decode(cr.yz)
                let nu = self.oct_decode(cu.yz)
                let ndn = self.oct_decode(cd.yz)
                var curve = 0.0
                curve = curve + (dot(n, nl) - 1.0)
                curve = curve + (dot(n, nr) - 1.0)
                curve = curve + (dot(n, nu) - 1.0)
                curve = curve + (dot(n, ndn) - 1.0)
                // A shaded bead holds no normal: it counts as off the surface.
                let same = step(abs(cl.x - depth), depth * 0.03)
                    * step(abs(cr.x - depth), depth * 0.03)
                    * step(abs(cu.x - depth), depth * 0.03)
                    * step(abs(cd.x - depth), depth * 0.03)
                    * step(0.0 - 1.5, min(min(cl.y, cr.y), min(cu.y, cd.y)))
                if self.u_flags.w > 0.001 {
                    // Realtime: a convex edge (the view normal's x grows to the right and its
                    // y up the screen across the ring) catches a little light, as a fillet in
                    // a studio does; a shallow turn (a tessellated curve's facets) stays out,
                    // and creases are the screen AO's.
                    let k = (nr.x - nl.x) + (nu.y - ndn.y)
                    cavity = 1.0 + smoothstep(0.25, 0.9, k) * same * self.u_flags.w
                } else {
                    let valley = clamp(0.0 - curve * 1.4, 0.0, 1.0) * same
                    cavity = 1.0 - valley * self.u_flags.y
                }
            }

            // ---- the lit image ----------------------------------------
            // Solid is one neutral clay under a studio light. The lit pass
            // already carries the material tint, so Solid pulls the
            // saturation back out rather than running a second shading
            // pipeline.
            var col = self.lit.sample(uv).xyz
            // A far bead's colour is on the display already: no curve takes it again.
            var shown = false
            if g.y < 0.0 - 1.5 {
                col = self.bead_lit(g)
                shown = self.bead_shown(g) > 0.5
            }
            if !realtime && !bead {
                let luma = dot(col, vec3(0.2126, 0.7152, 0.0722))
                col = col.mix(vec3(luma, luma, luma), 0.85)
            }
            col = col * cavity
            if realtime && !bead {
                // The cascades' shadow on a part the SDF engine draws (its state is negated in
                // the lookup); a mesh took them in the lit pass.
                let field = self.field_drawn(id)
                let shadow = field && self.u_flags.z < 0.5
                var sun = vec2(1.0, 0.0)
                if shadow || self.u_ao.x > 0.0 {
                    sun = self.sun_at(g, uv)
                }
                let key = self.u_key.x * sun.y
                if shadow {
                    let share = key / max(key + self.u_key.y, 0.0001)
                    col = col * (1.0 - share * (1.0 - sun.x))
                }
                if self.u_ao.x > 0.0 {
                    col = col * self.screen_ao(uv, key * sun.x, !field)
                }
            }
            if realtime && bead && self.u_ao.x > 0.0 {
                // a bead's: its shade has the sun and the hemisphere, no cascades
                let nb = self.render_normal(self.depth_normal(uv, depth, gl.x, gr.x, gu.x, gd.x))
                let ao = self.screen_ao(uv, self.u_key.x * max(dot(nb, normalize(self.u_sun_dir)), 0.0), abs(g.w - #(super::print::LIT_BEAD_AUX_ID)) < 1.5 || abs(g.w - #(super::print::SELECTED_BODY_AUX_ID)) < 0.5)
                // on the display, a factor on linear light is its power 1 / 2.2
                if shown {
                    col = col * pow(max(ao, 0.0), 0.4545454)
                } else {
                    col = col * ao
                }
            }
            if !bead && front && self.u_plate.x >= 0.0 && abs(id - self.u_plate.x) < 0.5 {
                let p = self.plate_at(uv)
                let px = self.plate_at(uv + vec2(tx, 0.0))
                let py = self.plate_at(uv + vec2(0.0, ty))
                // Neighbouring rays give the footprint without derivatives in this branch.
                let fx = abs(px.xy - p.xy) + abs(py.xy - p.xy)
                let inside = step(self.u_plate_rect.x, p.x) * step(self.u_plate_rect.y, p.y)
                    * step(p.x, self.u_plate_rect.z) * step(p.y, self.u_plate_rect.w)
                let grid = self.plate_grid(p.xy - self.u_plate_rect.xy, fx) * inside
                col = col * (1.0 + max(grid.x * self.u_plate.y, grid.y * self.u_plate.z))
            }
            if !shown && !is_cap {
                col = self.display(col)
            }
            if is_cap {
                // The cut face: one flat shade, whatever back face shows through the cut, so
                // the cap reads as one clean surface at the plane, never as the shell's inside.
                col = self.u_cap.xyz * 0.8
            }

            // ---- silhouette edges --------------------------------------
            // Silhouette = id or background break (the selection outline
            // and x-ray read it).
            let idl = self.elem_of(gl.w)
            let idr = self.elem_of(gr.w)
            let idu = self.elem_of(gu.w)
            let idd = self.elem_of(gd.w)
            var edge = 0.0
            edge = max(edge, step(0.5, abs(idl - id)) * step(0.5, abs(gl.w)))
            edge = max(edge, step(0.5, abs(idr - id)) * step(0.5, abs(gr.w)))
            edge = max(edge, step(0.5, abs(idu - id)) * step(0.5, abs(gu.w)))
            edge = max(edge, step(0.5, abs(idd - id)) * step(0.5, abs(gd.w)))
            // Silhouette against the background counts too.
            edge = max(edge, step(0.5, 1.0 - step(0.5, abs(gl.w))))
            edge = max(edge, step(0.5, 1.0 - step(0.5, abs(gr.w))))
            edge = max(edge, step(0.5, 1.0 - step(0.5, abs(gu.w))))
            edge = max(edge, step(0.5, 1.0 - step(0.5, abs(gd.w))))

            // ---- x-ray ------------------------------------------------
            if self.u_mode.y > 0.5 && !bead {
                col = bg.mix(col, 0.38)
                col = col + vec3(edge, edge, edge) * 0.10
            }

            // ---- hover + selection outline ----------------------------
            if self.u_flags.x > 0.5 {
                // The outline is an OUTER ring, over what lies round a selected element.
                if state < 1.5 {
                    col = self.ring_over(col, uv)
                }
            }
            if self.u_hover.x > 0.5 {
                if abs(id + 1.0 - self.u_hover.x) < 0.5 {
                    col = col.mix(self.u_hover_col.xyz, 0.12)
                }
            }
            return self.dithered(self.ghost_over(col, uv, depth), uv)
        }
    }

    // The picture averages display values: a display difference adds in the same units.
    // Exact for a single sample; on a finished still L is the last sample's texel, giving
    // a second-order error. On the legacy lane display is the identity: the difference is light.
    mod.draw.DrawFabGlowOver = mod.std.set_type_default() do #(DrawFabGlowOver::script_shader(vm)){
        ..mod.draw.FabDisplay,
        ..mod.draw.FabDither,
        ..mod.draw.DrawQuad,
        ..mod.draw.BloomKnee,
        picture: texture_2d(float)
        lit: texture_2d(float)
        glow: texture_2d(float)
        halo: texture_2d(float)
        // x = the halo's share f (0 without a halo), y = f / levels, z = display white in the
        // layer's units (the bloom's threshold), w unused
        u_glow: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        mirror_x: 0.0

        pixel: fn() -> vec4f {
            let uv = vec2(mix(self.pos.x, 1.0 - self.pos.x, self.mirror_x), self.pos.y)
            // shown = g - f x + f blur(x): the halo spreads its share of only the light above
            // display white (x, what the bloom's first level took from g). A lens or a tip below
            // white stays whole and sharp; above it the halo grows from zero without a step.
            let g = max(self.glow.sample(uv).xyz, vec3(0.0))
            let light = g - self.bloom_knee(g, self.u_glow.z) * self.u_glow.x + self.halo.sample(uv).xyz * self.u_glow.y
            if max(light.x, max(light.y, light.z)) <= 0.0 { discard() }
            let l = self.lit.sample(uv).xyz
            let c = self.picture.sample(uv).xyz + self.display(l + light) - self.display(l)
            return self.dithered(c, uv)
        }
    }

    // =====================================================================
    // REBUILDING — a quad in the window pass over the retained composite
    // =====================================================================
    // A part being recalculated: a soft band sweeps up it, over fine moving scanlines. It
    // reads the aux pass's element ids and animates without recording any scene pass.
    mod.draw.DrawFabRebuild = mod.std.set_type_default() do #(DrawFabRebuild::script_shader(vm)){
        ..mod.draw.DrawQuad
        aux: texture_2d(float)
        // (the part's element id + 1, strength 0..1, seconds since it started, 0); 0 strength: off
        u_rebuild: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // (top, bottom as 0..1 uv, the pass's pixel height, 0)
        u_rebuild_span: uniform(vec4(0.0, 1.0, 1000.0, 0.0))
        u_rebuild_col: uniform(vec4(0.30, 0.62, 1.0, 1.0))
        mirror_x: 0.0

        pixel: fn() {
            let uv = vec2(mix(self.pos.x, 1.0 - self.pos.x, self.mirror_x), self.pos.y)
            let g = self.aux.sample_nearest(uv)
            let id = abs(g.w) - 1.0
            if g.w == 0.0 || g.w > 16000000.0 || abs(id + 1.0 - self.u_rebuild.x) > 0.5 {
                return vec4(0.0, 0.0, 0.0, 0.0)
            }
            let t = self.u_rebuild.z
            let top = self.u_rebuild_span.x
            let bottom = self.u_rebuild_span.y
            let span = max(bottom - top, 0.001)
            // 0 at the part's foot, 1 at its top
            let h = clamp((bottom - uv.y) / span, 0.0, 1.0)
            let front = fract(t * 0.55)
            let d = (h - front) / 0.12
            let band = exp(0.0 - d * d)
            let lines = 0.5 + 0.5 * sin(uv.y * self.u_rebuild_span.z * 0.9 - t * 9.0)
            let lit = clamp(band * 0.65 + lines * 0.12 + 0.08, 0.0, 1.0)
            let a = lit * self.u_rebuild.y
            return vec4(self.u_rebuild_col.xyz * a, a)
        }
    }

    // =====================================================================
    // GRID — infinite, adaptive, axis-coloured, occluded against `aux`
    // =====================================================================
    mod.draw.DrawFabGrid = mod.std.set_type_default() do #(DrawFabGrid::script_shader(vm)){
        alpha_blend: true
        depth_write: false
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_geom.FabVertex, mod.fab_geom.FabGeom)
        aux: texture_2d(float)
        u_grid: uniform(vec4(0.313, 0.313, 0.313, 1.0))
        u_grid_major: uniform(vec4(0.353, 0.353, 0.353, 1.0))
        u_axis_x: uniform(vec4(1.0, 0.2, 0.32, 1.0))
        u_axis_y: uniform(vec4(0.545, 0.863, 0.0, 1.0))
        // x = axes on, y = grid on, zw unused
        u_grid_flags: uniform(vec4(1.0, 1.0, 0.0, 0.0))
        v_world: varying(vec3f)
        v_view: varying(vec3f)
        v_clip: varying(vec4f)

        camera_world_pos: fn() -> vec3f {
            let c = self.draw_pass.camera_inv * vec4(0.0, 0.0, 0.0, 1.0)
            let w = max(c.w, 0.00001)
            return vec3(c.x / w, c.y / w, c.z / w)
        }

        vertex: fn() {
            let world = self.transform * vec4(self.geom.pos.x, self.geom.pos.y, self.geom.pos.z, 1.0)
            self.v_world = world.xyz
            let view4 = self.draw_pass.camera_view * world
            self.v_view = view4.xyz
            let clip = self.draw_pass.camera_projection * view4
            self.v_clip = clip
            self.vertex_pos = clip
        }

        grid_line: fn(p: vec2f, spacing: float, fw: vec2f) -> float {
            let g = abs(fract(p / spacing - 0.5) - 0.5) * spacing / max(fw, vec2(0.000001, 0.000001))
            return 1.0 - min(min(g.x, g.y), 1.0)
        }

        pixel: fn() -> vec4f {
            // Occlusion against the G-buffer: this pass owns no depth
            // buffer, so a grid line behind a part is rejected by comparing
            // view distances.
            let ndc = self.v_clip.xy / max(self.v_clip.w, 0.000001)
            let uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
            let g = self.aux.sample_nearest(uv)
            let my = length(self.v_view)
            if g.w != 0.0 {
                // The bias ADDS here: a part's bottom face sitting exactly on
                // z = 0 is coplanar with the grid, and the model has to win
                // that tie.
                if g.x < my + (my * 0.0015 + 0.01) {
                    discard()
                }
            }
            let cam = self.camera_world_pos()
            let p = self.v_world.xy
            // how far the line spacing and the fade are seen from: the eye, or orthographic (one
            // scale over the whole view) the distance its height would be seen at in perspective
            var dist = length(cam - self.v_world)
            if self.ortho_reach > 0.0 {
                dist = self.ortho_reach
            }
            let fw = abs(dFdx(p)) + abs(dFdy(p))
            // One decade per ~10 screen-relative minor lines, cross-faded so
            // zooming never pops a whole grid in or out.
            let l10 = log(max(dist * 0.12, 0.0001)) / log(10.0)
            let lvl = floor(l10)
            let t = l10 - lvl
            let minor = pow(10.0, lvl)
            let major = minor * 10.0
            let a_minor = self.grid_line(p, minor, fw) * (1.0 - t) * 0.55 * self.u_grid_flags.y
            let a_major = self.grid_line(p, major, fw) * 0.85 * self.u_grid_flags.y
            let ax = (1.0 - min(abs(p.y) / max(fw.y * 1.6, 0.000001), 1.0)) * self.u_grid_flags.x
            let ay = (1.0 - min(abs(p.x) / max(fw.x * 1.6, 0.000001), 1.0)) * self.u_grid_flags.x
            let fade = 1.0 - smoothstep(self.fade_far * 0.35, self.fade_far, dist)
            var col = self.u_grid.xyz
            var alpha = a_minor
            col = col.mix(self.u_grid_major.xyz, a_major)
            alpha = max(alpha, a_major)
            col = col.mix(self.u_axis_y.xyz, ay)
            alpha = max(alpha, ay)
            col = col.mix(self.u_axis_x.xyz, ax)
            alpha = max(alpha, ax)
            alpha = alpha * fade
            if alpha < 0.002 {
                discard()
            }
            return vec4(col * alpha, alpha)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // =====================================================================
    // RESIN LAYER — a resin printer's layer image on its plane at the cut
    // =====================================================================
    // The quad covers the image's pixels on the bed at the layer's top; `geom.uv` runs over
    // the image (row 0 at its lowest Y). Each texel is the grey the screen lights the pixel
    // with: lit pixels glow, the rest of the image's box is a faint screen. Occluded against
    // `aux` like the grid, but the cut's cap under it loses the tie: the image is drawn on it.
    mod.draw.DrawFabResinLayer = mod.std.set_type_default() do #(DrawFabResinLayer::script_shader(vm)){
        alpha_blend: true
        depth_write: false
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_geom.FabVertex, mod.fab_geom.FabGeom)
        aux: texture_2d(float)
        layer: texture_2d(float)
        v_uv: varying(vec2f)
        v_view: varying(vec3f)
        v_clip: varying(vec4f)

        vertex: fn() {
            let world = vec4(self.geom.pos.x, self.geom.pos.y, self.geom.pos.z + self.lift, 1.0)
            let view4 = self.draw_pass.camera_view * world
            self.v_view = view4.xyz
            self.v_uv = self.geom.uv
            let clip = self.draw_pass.camera_projection * view4
            self.v_clip = clip
            self.vertex_pos = clip
        }

        pixel: fn() -> vec4f {
            let ndc = self.v_clip.xy / max(self.v_clip.w, 0.000001)
            let uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
            let g = self.aux.sample_nearest(uv)
            let my = length(self.v_view)
            if g.w != 0.0 {
                if g.x < my - (my * 0.0015 + 0.0001) {
                    discard()
                }
            }
            let grey = self.layer.sample_nearest(self.v_uv).x
            let col = self.screen.xyz.mix(self.lit.xyz, grey)
            let alpha = max(self.screen.w, grey)
            return vec4(col * alpha, alpha)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // =====================================================================
    // SEE-THROUGH — a part's materials, marched front to back over the composite
    // =====================================================================
    // `see_through.rs` has the grid: per brick an index texel (x: -1 air, -2 solid in yzw's
    // linear colour, else its slot in the atlas) and per slot its nine slices of 9 x 9 RGBA
    // texels side by side, `PER_ROW` (100) slots a row (rgb the colour's square root, a the
    // occupancy: a half on the surface, a quarter more a cell further in). The box is the
    // grid's lattice (a cell 1): only its back faces draw, so a camera inside it still marches.
    // A ray runs from the back face toward the eye: from where it enters the box, or the eye, to
    // the back face or what `aux` holds nearer (the plate, the other parts). It crosses air and
    // a solid brick in one step each and steps half a cell through the bricks that hold
    // samples. Each surface it crosses lays a skin, lit from the eye; each millimetre of the
    // inside absorbs. `u_see.x`, 0 a ghost .. 1 solid, sets both.
    mod.draw.DrawFabSeeThrough = mod.std.set_type_default() do #(DrawFabSeeThrough::script_shader(vm)){
        alpha_blend: true
        depth_write: false
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_geom.FabVertex, mod.fab_geom.FabGeom)
        aux: texture_2d(float)
        atlas: texture_2d(float)
        occ_map: texture_2d(float)
        index_map: texture_2d(float)
        // x = opacity (0 a ghost .. 1 solid), y = 1 orthographic, zw = the pass in pixels
        u_see: uniform(vec4(0.35, 0.0, 1.0, 1.0))
        // x = the glow outside the part (0 none .. 1 bright)
        u_glow: uniform(vec4(0.35, 0.0, 0.0, 0.0))
        // the atlas's width and height, the index's (texels)
        u_sizes: uniform(vec4(1.0, 1.0, 1.0, 1.0))
        v_lat: varying(vec3f)
        v_view: varying(vec3f)
        v_clip: varying(vec4f)
        v_away: varying(float)

        vertex: fn() {
            let lat = self.geom.pos * self.bricks.xyz * 8.0
            let world = self.world_from_lattice * vec4(lat.x, lat.y, lat.z, 1.0)
            let view4 = self.draw_pass.camera_view * world
            // whether the face looks away from the eye (one value over a flat face)
            let nw = self.world_from_lattice * vec4(self.geom.normal.x, self.geom.normal.y, self.geom.normal.z, 0.0)
            let nv = (self.draw_pass.camera_view * vec4(nw.x, nw.y, nw.z, 0.0)).xyz
            var away = dot(nv, view4.xyz)
            if self.u_see.y > 0.5 {
                away = 0.0 - nv.z
            }
            self.v_away = away
            self.v_lat = lat
            self.v_view = view4.xyz
            let clip = self.draw_pass.camera_projection * view4
            self.v_clip = clip
            self.vertex_pos = clip
        }

        nonzero: fn(x: float) -> float {
            if abs(x) < 0.000001 {
                return 0.000001
            }
            return x
        }

        // brick k's index texel (k counts x first, then y, then z)
        brick_entry: fn(k: float) -> vec4f {
            let i = self.bricks.w + k
            let w = self.u_sizes.z
            let uv = vec2((modf(i, w) + 0.5) / w, (floor(i / w) + 0.5) / self.u_sizes.w)
            return self.index_map.sample_nearest(uv, 0.0)
        }

        // where slot s's samples for l (0..8 a side) lie: the two slices' x and their y in the
        // atlas (uv), and the share of the second
        tile_at: fn(s: float, l: vec3f) -> vec4f {
            let tx = modf(s, 100.0)
            let ty = floor(s / 100.0)
            let q = clamp(l, vec3(0.0, 0.0, 0.0), vec3(8.0, 8.0, 8.0))
            let z0 = min(floor(q.z), 7.0)
            let x = tx * 81.0 + z0 * 9.0 + q.x + 0.5
            let y = (ty * 9.0 + q.y + 0.5) / self.u_sizes.y
            return vec4(x / self.u_sizes.x, (x + 9.0) / self.u_sizes.x, y, q.z - z0)
        }

        // the paint there, filtered: its colour (square roots) and how much of it there is
        tile_colour: fn(a: vec4f) -> vec4f {
            return mix(self.atlas.sample_lod(vec2(a.x, a.z), 0.0), self.atlas.sample_lod(vec2(a.y, a.z), 0.0), a.w)
        }

        // the part's occupancy there, filtered: 0 outside, a half on its surface, 1 inside
        tile_occ: fn(a: vec4f) -> float {
            return mix(self.occ_map.sample_lod(vec2(a.x, a.z), 0.0).x, self.occ_map.sample_lod(vec2(a.y, a.z), 0.0).x, a.w)
        }

        occ_at: fn(s: float, l: vec3f) -> float {
            return self.tile_occ(self.tile_at(s, l))
        }

        pixel: fn() -> vec4f {
            if self.v_away < 0.0 {
                discard()
            }
            let ortho = self.u_see.y > 0.5
            var dir_v = normalize(self.v_view)
            var t_face = length(self.v_view)
            if ortho {
                dir_v = vec3(0.0, 0.0, 0.0 - 1.0)
                t_face = 0.0 - self.v_view.z
            }
            let dw = (self.draw_pass.camera_inv * vec4(dir_v.x, dir_v.y, dir_v.z, 0.0)).xyz
            let d = vec3(dot(self.lat_x.xyz, dw), dot(self.lat_y.xyz, dw), dot(self.lat_z.xyz, dw))
            let o = self.v_lat
            // s: metres along the ray from this back face (the eye at -t_face)
            let top = self.bricks.xyz * 8.0
            let inv = vec3(1.0 / self.nonzero(d.x), 1.0 / self.nonzero(d.y), 1.0 / self.nonzero(d.z))
            let ta = (vec3(0.0, 0.0, 0.0) - o) * inv
            let tb = (top - o) * inv
            let lo = min(ta, tb)
            let hi = max(ta, tb)
            let s0 = max(max(max(lo.x, lo.y), lo.z), 0.0 - t_face)
            var s1 = min(min(min(hi.x, hi.y), hi.z), 0.0)
            let ndc = self.v_clip.xy / max(self.v_clip.w, 0.000001)
            let uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
            let g = self.aux.sample_nearest(uv)
            if g.w != 0.0 {
                var t_occ = g.x
                if ortho {
                    t_occ = sqrt(max(g.x * g.x - self.v_view.x * self.v_view.x - self.v_view.y * self.v_view.y, 0.0))
                }
                s1 = min(s1, t_occ - t_face)
            }
            if s1 <= s0 {
                discard()
            }
            // a cell in metres along the ray; the steps are half one near the part's surface
            let cell = 1.0 / max(length(d), 0.000001)
            let stride = 0.5 * cell
            let u = self.u_see.x
            // the part's shell: thin, a paler line than the paint round it, its edge on to the eye
            // a crisp light line
            let skin = 0.25 + 0.75 * u * u
            let pale = 0.45 * (1.0 - u)
            // absorption a metre inside the part: 0.003 a millimetre for a ghost, 2 for solid;
            // the paint's glow outside it, and how much brighter the glow shows
            let sigma = 3.0 + 2000.0 * u * u * u
            let glow = 160.0 * self.u_glow.x * self.u_glow.x
            let bright = 1.35
            let eye = normalize(vec3(0.0, 0.0, 0.0) - d)
            let px = floor(uv * self.u_see.zw)
            let jitter = fract(sin(dot(px, vec2(12.9898, 78.233))) * 43758.5453)
            var s = s0 + jitter * stride
            var acc = vec4(0.0, 0.0, 0.0, 0.0)
            var prev = 0.0 - 1.0
            // the last step's length through a brick of samples (0 after a whole brick)
            var back = 0.0
            // where the shell was last laid: a ray grazing the surface crosses the half again and
            // again within a cell or two, and lays it once
            var laid = 0.0 - 1000.0
            var n = 0.0
            let ahead = step(vec3(0.0, 0.0, 0.0), d) * 8.0
            // the brick the last step was in, and its index texel
            var at = 0.0 - 1.0
            var e = vec4(0.0, 0.0, 0.0, 0.0)
            loop {
                if s >= s1 || acc.w > 0.985 || n > 1500.0 {
                    break
                }
                n = n + 1.0
                let p = o + d * s
                let b = clamp(floor(p / 8.0), vec3(0.0, 0.0, 0.0), self.bricks.xyz - vec3(1.0, 1.0, 1.0))
                let base = b * 8.0
                let ex = (base + ahead - o) * inv
                let s_exit = min(min(ex.x, ex.y), min(ex.z, s1))
                let k = (b.z * self.bricks.y + b.y) * self.bricks.x + b.x
                if k != at {
                    at = k
                    e = self.brick_entry(k)
                }
                if e.x < 0.0 - 2.5 {
                    // the paint's glow in one colour outside the part: across in one step
                    let a = 1.0 - exp((0.0 - glow) * max(s_exit - s, 0.0))
                    acc = acc + vec4(e.y * bright, e.z * bright, e.w * bright, 1.0) * (a * (1.0 - acc.w))
                    prev = 0.0
                    back = 0.0
                    s = s_exit + cell * 0.01
                } else if e.x < 0.0 - 1.5 {
                    // inside the part in one colour: absorbed across in one step
                    let a = 1.0 - exp((0.0 - sigma) * max(s_exit - s, 0.0))
                    acc = acc + vec4(e.y * 0.85, e.z * 0.85, e.w * 0.85, 1.0) * (a * (1.0 - acc.w))
                    prev = 1.0
                    back = 0.0
                    s = s_exit + cell * 0.01
                } else if e.x < 0.0 - 0.5 {
                    prev = 0.0
                    back = 0.0
                    s = s_exit + cell * 0.01
                } else {
                    let l = p - base
                    let t = self.tile_at(e.x, l)
                    let c = self.tile_colour(t)
                    let occ = self.tile_occ(t)
                    let col = c.xyz * c.xyz
                    if prev >= 0.0 && (prev - 0.5) * (occ - 0.5) <= 0.0 && abs(prev - occ) > 0.0001 && s - laid > 2.0 * cell {
                        laid = s
                        // the part's surface crossed: its shell in the paint's colour there, paled
                        // so it reads against the glow, lit from the eye, and where it turns away a
                        // crisp light line
                        // its normal where the step crossed the half, not a step past it: a
                        // curved surface's edge stays one clean line
                        let f = clamp((prev - 0.5) / (prev - occ), 0.0, 1.0)
                        let lc = clamp(l - d * ((1.0 - f) * back), vec3(0.5, 0.5, 0.5), vec3(7.5, 7.5, 7.5))
                        let gx = self.occ_at(e.x, lc + vec3(0.5, 0.0, 0.0)) - self.occ_at(e.x, lc - vec3(0.5, 0.0, 0.0))
                        let gy = self.occ_at(e.x, lc + vec3(0.0, 0.5, 0.0)) - self.occ_at(e.x, lc - vec3(0.0, 0.5, 0.0))
                        let gz = self.occ_at(e.x, lc + vec3(0.0, 0.0, 0.5)) - self.occ_at(e.x, lc - vec3(0.0, 0.0, 0.5))
                        let nrm = normalize(vec3(gx, gy, gz) + vec3(0.0, 0.0, 0.000001))
                        let facing = abs(dot(nrm, eye))
                        let edge = (1.0 - smoothstep(0.06, 0.26, facing)) * (1.0 - 0.6 * u)
                        let face = col * (1.0 - pale) + vec3(pale, pale, pale)
                        let lit = face * (0.4 + 0.6 * facing) + vec3(edge * 0.9, edge * 0.9, edge * 0.9)
                        let a = clamp(skin + edge * 0.6, 0.0, 1.0)
                        acc = acc + vec4(lit.x, lit.y, lit.z, 1.0) * (a * (1.0 - acc.w))
                    }
                    let cov = clamp((occ - 0.5) * 4.0 + 0.5, 0.0, 1.0)
                    // the occupancy ramps over two cells either side of the surface, so it tells
                    // how far off the surface is: the step goes to half a cell short of it, a
                    // cell at most (the paint may change within it), half a cell at least
                    let reach = clamp(abs(occ - 0.5) * 4.0 - 0.5, 0.5, 1.0) * cell
                    let ds = max(min(reach, s_exit - s + cell * 0.01), cell * 0.01)
                    let dens = sigma * cov + glow * c.w * (1.0 - cov)
                    let a = 1.0 - exp((0.0 - dens) * ds)
                    let shade = 0.85 * cov + bright * (1.0 - cov)
                    acc = acc + vec4(col.x * shade, col.y * shade, col.z * shade, 1.0) * (a * (1.0 - acc.w))
                    prev = occ
                    back = ds
                    s = s + ds
                }
            }
            if acc.w < 0.002 {
                discard()
            }
            let lin = acc.xyz / acc.w
            let shown = vec3(pow(lin.x, 0.4545), pow(lin.y, 0.4545), pow(lin.z, 0.4545))
            return vec4(shown * acc.w, acc.w)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // =====================================================================
    // SEE-THROUGH, HALF — the march at half the pixels, taken up into the composite
    // =====================================================================
    // On a hi-dpi display `DrawFabSeeThrough` marches half the pixels each way into its own
    // target (premultiplied), and this takes it up. A pixel weighs the four texels round it as
    // a bilinear filter would, but a texel whose march stopped at another depth than this
    // pixel's (`aux`: another part, the plate, in front) drops out, and of the rest only those
    // like the one weighing most count. So no colour spreads across an edge: a shell's edge
    // against the glow or the empty box stays a step, and smooth paint blends.
    mod.draw.DrawFabSeeUp = mod.std.set_type_default() do #(DrawFabSeeUp::script_shader(vm)){
        ..mod.draw.DrawQuad
        see_map: texture_2d(float)
        aux: texture_2d(float)
        // xy = the march's target in pixels
        u_half: uniform(vec4(1.0, 1.0, 0.0, 0.0))
        depth_clip: 0.0

        // straight to clip space, as `DrawFabComposite`: the pass holds the 3D camera
        vertex: fn() {
            self.pos = self.geom.pos
            self.world = vec4(self.geom.pos.x, self.geom.pos.y, 0.0, 1.0)
            self.vertex_pos = vec4(
                self.geom.pos.x * 2.0 - 1.0
                1.0 - self.geom.pos.y * 2.0
                0.0
                1.0
            )
        }

        // how far off what `aux` holds at uv is (metres; nothing: far)
        depth_at: fn(uv: vec2f) -> float {
            let g = self.aux.sample_nearest(uv)
            if g.w == 0.0 {
                return 1000000.0
            }
            return g.x
        }

        // 1 for a texel whose march stopped where this pixel's does, none a few percent off
        same: fn(d: float, t: float) -> float {
            let r = abs(d - t) / max(min(d, t), 0.001)
            return 1.0 - smoothstep(0.01, 0.04, r)
        }

        // 1 for a texel like the reference, none for one a step of the shell away
        like: fn(c: vec4f, r: vec4f) -> float {
            let e = c - r
            return exp(0.0 - 60.0 * dot(e, e))
        }

        pixel: fn() {
            let size = self.u_half.xy
            let p = self.pos * size - vec2(0.5, 0.5)
            let i = floor(p)
            let f = p - i
            let u0 = (i + vec2(0.5, 0.5)) / size
            let u1 = (i + vec2(1.5, 0.5)) / size
            let u2 = (i + vec2(0.5, 1.5)) / size
            let u3 = (i + vec2(1.5, 1.5)) / size
            let c0 = self.see_map.sample_nearest(u0)
            let c1 = self.see_map.sample_nearest(u1)
            let c2 = self.see_map.sample_nearest(u2)
            let c3 = self.see_map.sample_nearest(u3)
            let d = self.depth_at(self.pos)
            // a texel past the target's edge is its edge texel (the sampler clamps)
            let w0 = (1.0 - f.x) * (1.0 - f.y) * self.same(d, self.depth_at(u0)) + 0.0001
            let w1 = f.x * (1.0 - f.y) * self.same(d, self.depth_at(u1)) + 0.0001
            let w2 = (1.0 - f.x) * f.y * self.same(d, self.depth_at(u2)) + 0.0001
            let w3 = f.x * f.y * self.same(d, self.depth_at(u3)) + 0.0001
            var r = c0
            var m = w0
            if w1 > m {
                r = c1
                m = w1
            }
            if w2 > m {
                r = c2
                m = w2
            }
            if w3 > m {
                r = c3
            }
            let k0 = w0 * self.like(c0, r)
            let k1 = w1 * self.like(c1, r)
            let k2 = w2 * self.like(c2, r)
            let k3 = w3 * self.like(c3, r)
            return (c0 * k0 + c1 * k1 + c2 * k2 + c3 * k3) / (k0 + k1 + k2 + k3)
        }
    }

    // Screen-width lines over the composite (the build volume's edges, the model's outline at the
    // shown layer): `u_px.z * 2` pixels wide. A vertex is one end of its segment (`geom.pos`,
    // world), with the other end in `geom.normal`, which end in `geom.uv.x` (0 the first, 1 the
    // second) and its side of the line in `geom.element` (-1 or 1). `u_px.w` 1: each end reaches
    // past its point by the half width, so the segments of a polyline overlap at every joint and
    // it reads as one line (an opaque line's overlap does not show). Like the grid it is hidden
    // behind what `aux` holds nearer the eye (a part, a print, the bed), so a part stands in
    // front of it.
    mod.draw.DrawFabLines = mod.std.set_type_default() do #(DrawFabLines::script_shader(vm)){
        alpha_blend: true
        depth_write: false
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_geom.FabVertex, mod.fab_geom.FabGeom)
        aux: texture_2d(float)
        // xy = the pass in pixels, z = half the line's width in pixels, w = 1: capped ends
        u_px: uniform(vec4(1.0, 1.0, 0.5, 0.0))
        v_view: varying(vec3f)
        v_clip: varying(vec4f)
        v_side: varying(float)

        // How far an end moves toward the other to be in front of the eye (0: it is).
        to_front: fn(c0: vec4f, c1: vec4f) -> float {
            if c0.w >= 0.0001 { return 0.0 }
            return clamp((0.0001 - c0.w) / max(c1.w - c0.w, 0.000001), 0.0, 1.0)
        }

        vertex: fn() {
            let a = self.draw_pass.camera_view * vec4(self.geom.pos.x, self.geom.pos.y, self.geom.pos.z, 1.0)
            let b = self.draw_pass.camera_view * vec4(self.geom.normal.x, self.geom.normal.y, self.geom.normal.z, 1.0)
            let ca = self.draw_pass.camera_projection * a
            let cb = self.draw_pass.camera_projection * b
            let ta = self.to_front(ca, cb)
            let tb = self.to_front(cb, ca)
            let view = a.mix(b, ta)
            let c = ca.mix(cb, ta)
            let o = cb.mix(ca, tb)
            // Across the line in pixels (the first end's way at both ends), a pixel wider for
            // its antialiased edge.
            let d = (o.xy / o.w - c.xy / c.w) * self.u_px.xy
            let t = d / max(length(d), 0.000001)
            let n = vec2(0.0 - t.y, t.x) * (1.0 - 2.0 * self.geom.uv.x)
            // Across by the half width (and a pixel for the antialiased edge), and with caps out
            // along the segment by as much.
            let off = (n * self.geom.element - t * self.u_px.w) * ((self.u_px.z + 1.0) * 2.0) / self.u_px.xy
            var clip = vec4(c.x + off.x * c.w, c.y + off.y * c.w, c.z, c.w)
            // Both ends behind the eye: nothing.
            if c.w < 0.00005 {
                clip = vec4(2.0, 2.0, 2.0, 1.0)
            }
            self.v_view = view.xyz
            self.v_clip = clip
            self.v_side = self.geom.element
            self.vertex_pos = clip
        }

        pixel: fn() -> vec4f {
            let ndc = self.v_clip.xy / max(self.v_clip.w, 0.000001)
            let uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
            let g = self.aux.sample_nearest(uv)
            let my = length(self.v_view)
            // The bias SUBTRACTS here (the grid's adds): a line meets the bed it stands on, and
            // the line wins that tie.
            if g.w != 0.0 {
                if g.x < my - (my * 0.002 + 0.0005) {
                    discard()
                }
            }
            let half = self.u_px.z
            let cover = clamp(half + 0.5 - abs(self.v_side) * (half + 1.0), 0.0, 1.0)
            let alpha = self.color.w * cover
            if alpha < 0.002 {
                discard()
            }
            return vec4(self.color.xyz * alpha, alpha)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // The picks' outlines (`tools::picks`): an exact part's picked face or edge, and what a click
    // would take, screen-width lines over a dark halo as `DrawFabLines` makes them. A run that
    // `aux` holds something nearer in front of is not dropped: it draws dimmed and dashed, so an
    // edge round the back reads as behind the part. `uv.y` of a vertex is how far along its line
    // it is (metres), so a dash runs on through the line's joints.
    mod.draw.DrawFabPickLines = mod.std.set_type_default() do #(DrawFabPickLines::script_shader(vm)){
        alpha_blend: true
        depth_write: false
        backface_culling: false
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_geom.FabVertex, mod.fab_geom.FabGeom)
        aux: texture_2d(float)
        // xy = the pass in pixels, z = half the line's width in pixels, w = the halo's width
        u_px: uniform(vec4(1.0, 1.0, 1.0, 1.0))
        // x = a hidden run's dash period in pixels, y = its share of the alpha
        u_hidden: uniform(vec4(9.0, 0.5, 0.0, 0.0))
        v_view: varying(vec3f)
        v_clip: varying(vec4f)
        v_side: varying(float)
        v_along: varying(float)

        to_front: fn(c0: vec4f, c1: vec4f) -> float {
            if c0.w >= 0.0001 { return 0.0 }
            return clamp((0.0001 - c0.w) / max(c1.w - c0.w, 0.000001), 0.0, 1.0)
        }

        vertex: fn() {
            let a = self.draw_pass.camera_view * vec4(self.geom.pos.x, self.geom.pos.y, self.geom.pos.z, 1.0)
            let b = self.draw_pass.camera_view * vec4(self.geom.normal.x, self.geom.normal.y, self.geom.normal.z, 1.0)
            let ca = self.draw_pass.camera_projection * a
            let cb = self.draw_pass.camera_projection * b
            let ta = self.to_front(ca, cb)
            let tb = self.to_front(cb, ca)
            let view = a.mix(b, ta)
            let c = ca.mix(cb, ta)
            let o = cb.mix(ca, tb)
            let d = (o.xy / o.w - c.xy / c.w) * self.u_px.xy
            let t = d / max(length(d), 0.000001)
            let n = vec2(0.0 - t.y, t.x) * (1.0 - 2.0 * self.geom.uv.x)
            // the line, its halo and a pixel for the antialiased edge; capped out along the segment
            let reach = self.u_px.z + self.u_px.w + 1.0
            let off = (n * self.geom.element - t) * (reach * 2.0) / self.u_px.xy
            var clip = vec4(c.x + off.x * c.w, c.y + off.y * c.w, c.z, c.w)
            if c.w < 0.00005 {
                clip = vec4(2.0, 2.0, 2.0, 1.0)
            }
            // pixels along the line here: a millimetre across the view, measured on the screen
            let up = self.draw_pass.camera_projection * vec4(view.x, view.y + 0.001, view.z, 1.0)
            let per_m = length((up.xy / up.w - c.xy / c.w) * self.u_px.xy * 0.5) * 1000.0
            self.v_along = self.geom.uv.y * per_m
            self.v_view = view.xyz
            self.v_clip = clip
            self.v_side = self.geom.element
            self.vertex_pos = clip
        }

        pixel: fn() -> vec4f {
            let ndc = self.v_clip.xy / max(self.v_clip.w, 0.000001)
            let uv = vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
            let g = self.aux.sample_nearest(uv)
            let my = length(self.v_view)
            // as `DrawFabLines`: the line wins a tie with the surface it lies on
            var share = 1.0
            if g.w != 0.0 {
                if g.x < my - (my * 0.002 + 0.0005) {
                    let on = step(fract(self.v_along / max(self.u_hidden.x, 1.0)), 0.55)
                    share = self.u_hidden.y * on
                }
            }
            if share < 0.002 {
                discard()
            }
            let half = self.u_px.z
            let d = abs(self.v_side) * (half + self.u_px.w + 1.0)
            let core = clamp(half + 0.5 - d, 0.0, 1.0) * self.color.w * share
            let outer = clamp(half + self.u_px.w + 0.5 - d, 0.0, 1.0) * self.halo.w * share
            let alpha = core + outer * (1.0 - core)
            if alpha < 0.002 {
                discard()
            }
            return vec4(self.color.xyz * core + self.halo.xyz * outer * (1.0 - core), alpha)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // =====================================================================
    // The composite back into the 2D pass.
    // =====================================================================
    mod.draw.DrawFabSceneTexture = mod.std.set_type_default() do #(DrawFabSceneTexture::script_shader(vm)){
        ..mod.draw.DrawQuad
        scene_texture: texture_2d(float)
        u_ao_debug: uniform(0.0)
        mirror_x: 0.0
        scene_ready: 1.0
        pixel: fn() {
            if self.scene_ready < 0.5 { return vec4(0.0, 0.0, 0.0, 0.0) }
            let color = self.scene_texture.sample(vec2(mix(self.pos.x, 1.0 - self.pos.x, self.mirror_x), self.pos.y))
            // CORTAO promises only red visibility (history's green holds depth).
            if self.u_ao_debug > 0.5 { return vec4(color.xxx, 1.0) }
            return Pal.premul(color)
        }
    }

    mod.draw.DrawFabCrosshair = mod.std.set_type_default() do #(DrawFabCrosshair::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let c = self.rect_size * 0.5
            sdf.circle(c.x, c.y, 4.0)
            sdf.stroke(fab.color_vp_hover, 1.0)
            sdf.move_to(1.5, c.y)
            sdf.line_to(c.x - 6.0, c.y)
            sdf.stroke(fab.color_vp_hover, 1.0)
            sdf.move_to(c.x + 6.0, c.y)
            sdf.line_to(self.rect_size.x - 1.5, c.y)
            sdf.stroke(fab.color_vp_hover, 1.0)
            sdf.move_to(c.x, 1.5)
            sdf.line_to(c.x, c.y - 6.0)
            sdf.stroke(fab.color_vp_hover, 1.0)
            sdf.move_to(c.x, c.y + 6.0)
            sdf.line_to(c.x, self.rect_size.y - 1.5)
            sdf.stroke(fab.color_vp_hover, 1.0)
            return sdf.result
        }
    }

    mod.draw.DrawFabControlRing = mod.std.set_type_default() do #(DrawFabControlRing::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let c = self.rect_size * 0.5
            sdf.circle(c.x, c.y, c.x - 1.0)
            sdf.fill_keep(vec4(fab.color_vp_hover.rgb, 0.12))
            sdf.stroke(fab.color_vp_hover, 1.25)
            return sdf.result
        }
    }

    mod.widgets.FabViewportBase = #(FabViewport::register_widget(vm))
    mod.widgets.FabViewport = set_type_default() do mod.widgets.FabViewportBase{
        width: Fill
        height: Fill
        debug_view: 0.0
        debug_off: 0.0
        clear_color: fab.color_vp_bg_top
        draw_bg: mod.draw.DrawFabSceneTexture{}
        draw_aux: mod.draw.DrawFabAux{}
        // Field tiles write depth and normals into aux, then material into lit.
        sdf_tile: mod.draw.SdfTile{ color_format: @Rgba32F alpha_blend: false }
        sdf_tile_lit: mod.draw.SdfTileLit{ alpha_blend: false }
        color_pick: fab.color_accent
        draw_comp: mod.draw.DrawFabComposite{
            u_bg_top: fab.color_vp_bg_top
            u_bg_bottom: fab.color_vp_bg_bottom
            u_select: fab.color_vp_select
            u_select_dim: fab.color_vp_select_dim
            u_hover_col: fab.color_vp_hover
            u_cap: fab.color_vp_cap
        }
        draw_glow_over: mod.draw.DrawFabGlowOver{}
        draw_rebuild: mod.draw.DrawFabRebuild{}
        draw_crosshair: mod.draw.DrawFabCrosshair{}
        draw_control_ring: mod.draw.DrawFabControlRing{}
        draw_grid: mod.draw.DrawFabGrid{
            u_grid: fab.color_vp_grid
            u_grid_major: fab.color_vp_grid_major
            u_axis_x: fab.color_vp_axis_x
            u_axis_y: fab.color_vp_axis_y
        }
        draw_volume: mod.draw.DrawFabLines{ color: #xccd6eb38 }
        // The overlay colour of the slice view's contour (`print::overlay_palette`'s 57).
        draw_outline: mod.draw.DrawFabLines{ color: #x33d9f2ff }
        // The ring round the printing nozzle's tip.
        draw_head_ring: mod.draw.DrawFabLines{ color: #xffffffff }
        // The picks' outlines: what a click would take, the picks, the one whose chip is lit.
        draw_pick_under: mod.draw.DrawFabPickLines{ color: fab.color_vp_measure halo: #x0a0a0ad9 }
        draw_pick_lines: mod.draw.DrawFabPickLines{ color: fab.color_accent halo: #x0a0a0ad9 }
        draw_pick_lit: mod.draw.DrawFabPickLines{ color: #xffffffff halo: #x0a0a0ad9 }
        draw_resin_layer: mod.draw.DrawFabResinLayer{ lit: #x8cd9ff screen: #x0d121a73 }
        draw_see: mod.draw.DrawFabSeeThrough{}
        draw_see_up: mod.draw.DrawFabSeeUp{}
        draw_path_aux: mod.draw.DrawFabPathAux{}
        draw_far_aux: mod.draw.DrawFabFarAux{}
        draw_issue_spot: mod.draw.DrawFabIssueSpot{}
        draw_head_aux: mod.draw.DrawFabHeadAux{}
        draw_force: mod.draw.DrawFabForce{}
        draw_force_aux: mod.draw.DrawFabForceAux{}
        draw_board: mod.draw.DrawFabBoard{}
        draw_board_aux: mod.draw.DrawFabBoardAux{}
        draw_glass: mod.draw.DrawFabGlass{}
        draw_body: mod.draw.DrawFabBody{}
        draw_body_aux: mod.draw.DrawFabBodyAux{}
        draw_body_glow: mod.draw.DrawFabBodyGlow{}
        draw_glow_pool: mod.draw.DrawFabGlowPool{}
        board_raster: mod.widgets.BoardArtRaster{}
        // The libs/render lanes. Sun terms are written every frame from
        // `SkyState`; these are only the boot values.
        draw_sky: mod.draw.DrawSceneSky{}
        draw_sky_analytic: mod.draw.DrawSceneSkyAnalytic{}
        draw_shadow: mod.draw.DrawSceneShadow{}
        draw_models: mod.draw.DrawSceneSkinned{}
    }
}
