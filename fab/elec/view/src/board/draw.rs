//! The board's shaders: analytic, world-space, retained.
//!
//! - `DrawBoardStroke`: a track segment, a track arc or a graphic stroke with round caps. An arc
//!   is its chord plus a signed sagitta, and its distance is exact at every zoom (a ring distance
//!   written so a huge radius keeps its precision).
//! - `DrawBoardGlow`: one soft footprint underlay per part, on the substrate below the board layers.
//! - `DrawBoardPad`: a rounded box (circle, rect, oval, rounded rect) minus an optional stadium
//!   hole (a drill or a slot). It also draws vias, plated rings and the holes themselves.
//! - `DrawBoardTri`: one triangle of a polygon fill (zones, custom pads, filled graphics, the
//!   substrate), drawn exact to the geometry; the polygon's ring strokes anti-alias its edges.
//! - `DrawBoardStrokeMax`, `DrawBoardPadMax`, `DrawBoardTriMax`: the same shaders blended with a
//!   per-channel max, for a layer's own target (a layer is one premultiplied colour target; its
//!   shapes union by max, so joints never double their coverage).
//! - `DrawBoardLayer`: one layer's offscreen picture composited onto the pane at its opacity.
//!
//! Instance layout: see `inst.rs` (the board common block, then the shape). World units are mm
//! in the board frame, Y up. The camera is `canvas::Camera2d::uniforms()`: screen =
//! (cam.x + w.x * cam.z, cam.y - w.y * cam.z), and cam.w is the device pixel ratio. A layer target
//! is drawn at dpi 1 with the camera moved into its pixels.
//!
//! Coverage is analytic: `clamp(0.5 - d, 0, 1)` with `d` the signed distance in device pixels.
//! A shape is never drawn thinner than one device pixel; under that its alpha falls with its true
//! width down to `thin_alpha`, so a dense board zoomed out keeps its density without vanishing.
//!
//! Per frame a layer costs uniforms only: the vertex stage moves every instance with the camera.
//! With `live_on`, it also reads the host's net, segment and part rows by static instance index.
//! The footprint underlay reads part x for dissipation, or y for light in the emitter's own colour.
//! A net's emphasis mixes its ink toward `sel_ink`, the board's selection accent.
//!
//! In the vertex stage, `item` indexes the host's `item_states` texture (selection, ghost,
//! violation severity, emphasis; w below zero an outside edit's flash, mixed toward `flash_ink`,
//! the AI accent) and the pane's `pane_states` texture (dim in x; emphasis in y,
//! which marks items a key changed besides its target). Each enabled texture is sampled nearest
//! through `Live.net_uv`, only for an index inside its own capacity; disabled or missing rows
//! contribute zero state. A vec4 and a dim varying carry these values
//! to the fragment stage, which applies violation, selection, emphasis, ghost and dim in order.
//! Before them, an item at or past the `fresh_item` uniform (a preview's new items: ids the
//! committed board has not issued yet) mixes toward `fresh_ink` by `fresh_mix`; -1 marks none.
//! While those items are a running route job's partial (`fresh_pending`), they also draw faint
//! inside and bright along their edge (strokes and pads; polygons only faint).
//! A lit net (`lit_net`, the instance's board `net`) lifts toward white and everything else
//! fades, both by `lit_mix`: the net under a dragged probe, in every pane at once.
//! The hovered item (`hover_item`, faded in by `hover_mix`) lifts toward white, except a pour: a
//! zone's fill (look 1) stays as it is, and its ring hairlines (look 1) light only around the
//! pointer (`hover_at`: its point, then the radii where the light starts to fall and ends, all in
//! the pass's own pixels), brighter and at full strength. A plane as big as
//! the board shows where the pointer is, never by a change over all of it. Ring edges along a
//! fill tile's box are seams between tiles (look 2) and never light.
//!
//! A positive component of `art_mask` switches to layer-art coverage planes: the fragment writes
//! `art_mask * coverage * fade`, including drill as an ordinary channel, without ink, layer
//! opacity, hover, live effects or item state. The vertex stage skips all live and state reads,
//! and pads write only coverage. The `*Max` variants union each channel independently;
//! drill is not subtracted from the other channels at raster time.

use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.elec_live.*

    mod.draw.DrawBoardStroke = mod.std.set_type_default() do #(DrawBoardStroke::script_shader(vm)) {
        ..mod.draw.DrawQuad
        depth_write: false
        pose_a: vec4(0.0, 0.0, 0.0, 1.0)
        pose_b: vec4(0.0, 0.0, 0.0, 1.0)
        span: vec2(0.0)
        item: -1.0
        look: 0.0
        ink: vec4(0.0)
        live_net: -1.0
        live_seg: -1.0
        live_part: -1.0
        net: -1.0
        seg: vec4(0.0)
        sag: 0.0
        width: 0.1
        along0: 0.0
        live_nets: texture_2d(float)
        live_segs: texture_2d(float)
        live_parts: texture_2d(float)
        live_on: uniform(0.0)
        live_time: uniform(0.0)
        live_light: uniform(0.0)
        item_states: texture_2d(float)
        item_states_on: uniform(0.0)
        pane_states: texture_2d(float)
        pane_states_on: uniform(0.0)
        sel_ink: uniform(vec4(0.72, 0.96, 1.0, 1.0))
        warn_ink: uniform(vec4(1.0, 0.77, 0.33, 1.0))
        err_ink: uniform(vec4(1.0, 0.32, 0.39, 1.0))
        info_ink: uniform(vec4(0.57, 0.73, 1.0, 1.0))
        ghost_alpha: uniform(0.25)
        dim_alpha: uniform(0.2)
        emph_gain: uniform(0.35)
        flash_ink: uniform(vec4(0.706, 0.549, 1.0, 1.0))
        fresh_item: uniform(-1.0)
        fresh_mix: uniform(0.0)
        fresh_pending: uniform(0.0)
        fresh_ink: uniform(vec4(0.5, 1.0, 0.78, 1.0))
        lit_net: uniform(-1.0)
        lit_mix: uniform(0.0)
        art_mask: uniform(vec4(0.0))
        cam: uniform(vec4(0.0, 0.0, 1.0, 1.0))
        motion_t: uniform(0.0)
        hover_item: uniform(-1.0)
        hover_mix: uniform(0.0)
        hover_at: uniform(vec4(-1e6, -1e6, 1.0, 2.0))
        layer_alpha: uniform(1.0)
        layer_ink: uniform(vec4(1.0))
        thin_alpha: uniform(0.4)
        clip: uniform(vec4(-1e20, -1e20, 1e20, 1e20))
        local: varying(vec2f)
        shape: varying(vec4f)
        live_ink: varying(vec3f)
        item_state: varying(vec4f)
        item_dim: varying(float)
        live_flow: varying(vec2f)
        fade: varying(float)
        screen: varying(vec2f)

        vertex: fn() {
            let art = self.art_mask.x > 0.0 || self.art_mask.y > 0.0 || self.art_mask.z > 0.0 || self.art_mask.w > 0.0
            self.item_state = vec4(0.0)
            self.item_dim = 0.0
            let base = if self.ink.w > 0.0 {self.ink} else {self.layer_ink}
            self.live_ink = base.xyz
            self.live_flow = vec2(0.0)
            if !art && self.live_on > 0.5 {
                if self.live_net >= 0.0 {
                    let net = self.live_nets.sample_nearest(Live.net_uv(self.live_net, self.live_nets.size()))
                    self.live_ink = Live.net_color(net, base.xyz, self.live_light, self.sel_ink.xyz)
                }
                if self.live_seg >= 0.0 {
                    self.live_flow = self.live_segs.sample_nearest(Live.net_uv(self.live_seg, self.live_segs.size())).xy
                }
            }
            if !art && self.item >= 0.0 {
                if self.item_states_on > 0.5 {
                    let size = self.item_states.size()
                    if self.item < size.x*size.y {
                        self.item_state = self.item_states.sample_nearest(Live.net_uv(self.item, size))
                    }
                }
                if self.pane_states_on > 0.5 {
                    let size = self.pane_states.size()
                    if self.item < size.x*size.y {
                        let pane = self.pane_states.sample_nearest(Live.net_uv(self.item, size))
                        self.item_dim = pane.x
                        // a key's emphasis, over an outside edit's flash (w below zero) only where set
                        if pane.y > 0.0 { self.item_state.w = max(self.item_state.w, pane.y) }
                    }
                }
            }
            let t = if self.span.y <= 0.0 {1.0} else {clamp((self.motion_t-self.span.x)/self.span.y, 0.0, 1.0)}
            let t1 = 1.0-t
            let pose = mix(self.pose_a, self.pose_b, 1.0-t1*t1*t1)
            let co = cos(pose.z)
            let si = sin(pose.z)
            let a = pose.xy+vec2(co*self.seg.x-si*self.seg.y, si*self.seg.x+co*self.seg.y)
            let b = pose.xy+vec2(co*self.seg.z-si*self.seg.w, si*self.seg.z+co*self.seg.w)
            let ppd = max(self.cam.z*self.cam.w, 0.000001)
            let delta = b-a
            let len = length(delta)
            let dir = if len > 0.0000001 {delta/len} else {vec2(1.0, 0.0)}
            let flip = if self.sag < 0.0 {-1.0} else {1.0}
            let nrm = vec2(-dir.y, dir.x)*flip
            let sag = abs(self.sag)
            // An arc whose bulge is under a quarter pixel is drawn as its chord.
            let arc = if sag*ppd > 0.25 && len > 0.0 {1.0} else {0.0}
            let r = if arc > 0.5 {(sag*sag+0.25*len*len)/(2.0*sag)} else {0.0}
            let ext = if arc > 0.5 && sag > 0.5*len {r-0.5*len} else {0.0}
            let top = if arc > 0.5 {sag} else {0.0}
            let half = max(0.5*self.width, 0.5/ppd)
            let m = half+1.5/ppd
            let u = mix(-m-ext, len+m+ext, self.geom.pos.x)
            let v = mix(-m, top+m, self.geom.pos.y)
            let w = a+dir*u+nrm*v
            self.local = vec2(u, v)
            self.shape = vec4(len, top, r, half)
            self.fade = pose.w*clamp(self.width*ppd, self.thin_alpha, 1.0)
            let sp = vec2(self.cam.x+w.x*self.cam.z, self.cam.y-w.y*self.cam.z)
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let ppd = max(self.cam.z*self.cam.w, 0.000001)
            let len = self.shape.x
            let sag = self.shape.y
            let r = self.shape.z
            // The chord frame: u along a->b, v toward the bulge.
            let hx = abs(self.local.x-0.5*len)
            let ex = hx-0.5*len
            let dline = length(vec2(max(ex, 0.0), self.local.y))
            // The arc: |q| - r as (|q|^2 - r^2) / (|q| + r), whose terms stay small for a huge r;
            // past the arc's ends, the distance to the nearer end.
            let dv = self.local.y-sag
            let qy = dv+r
            let dring = abs(hx*hx+dv*(dv+2.0*r))/(sqrt(hx*hx+qy*qy)+r)
            let dend = length(vec2(ex, self.local.y))
            let darc = if (r-sag)*hx > 0.5*len*qy {dend} else {dring}
            let dist = if r > 0.0 {darc} else {dline}
            let cover = clamp(0.5-(dist-self.shape.w)*ppd, 0.0, 1.0)
            if cover <= 0.0 { discard() }
            if self.art_mask.x > 0.0 || self.art_mask.y > 0.0 || self.art_mask.z > 0.0 || self.art_mask.w > 0.0 {
                self.fb0 = self.art_mask*(cover*self.fade)
            } else {
                let base = if self.ink.w > 0.0 {self.ink} else {self.layer_ink}
                let mut color = if self.live_on > 0.5 {self.live_ink} else {base.xyz}
                if self.live_flow.y != 0.0 {
                    let mut along = self.local.x
                    if r > 0.0 {
                        // In the bulge frame, angle increases from a to b for either sagitta sign.
                        let half_angle = atan2(0.5*len, r-sag)
                        let angle = atan2(self.local.x-0.5*len, qy)
                        along = clamp(angle+half_angle, 0.0, 2.0*half_angle)*r
                    }
                    let a = Live.flow(self.live_time, self.along0+along, ppd, self.live_flow.x, self.live_flow.y)
                    color = Live.lit(color, a, self.live_light)
                }
                let lit = if self.item >= 0.0 && self.look < 1.5 && abs(self.item-self.hover_item) < 0.5 {self.hover_mix} else {0.0}
                // A pour lights only its edge around the pointer, brighter and at full strength.
                let near = 1.0-smoothstep(self.hover_at.z, self.hover_at.w, length(sp-self.hover_at.xy))
                let edge = if self.look > 0.5 {lit*near} else {0.0}
                let hover = if self.look > 0.5 {edge} else {lit}
                let rgb = mix(color, vec3(1.0, 1.0, 1.0), hover*(if self.look > 0.5 {0.6} else {0.3}))
                let alpha = cover*mix(self.fade, 1.0, edge)*base.w*self.layer_alpha
                let mut result = vec4(rgb*alpha, alpha)
                // The straight-colour operations expressed in premultiplied space.
                if self.fresh_item >= 0.0 && self.item >= self.fresh_item {
                    result = vec4(mix(result.xyz, self.fresh_ink.xyz*result.w, self.fresh_mix), result.w)
                    // A running job's copper: faint inside, bright along its edge, until it commits.
                    if self.fresh_pending > 0.0 {
                        let inside = (self.shape.w-dist)*ppd
                        result *= mix(1.0, mix(0.3, 1.0, clamp(1.75-inside, 0.0, 1.0)), self.fresh_pending)
                    }
                }
                if self.lit_net >= 0.0 {
                    if abs(self.net-self.lit_net) < 0.5 {
                        result = vec4(result.xyz+(vec3(result.w)-result.xyz)*0.4*self.lit_mix, result.w)
                    } else {
                        result *= 1.0-0.5*self.lit_mix
                    }
                }
                if self.item_state.z > 0.5 {
                    let ink = if self.item_state.z > 2.5 {self.err_ink} else {
                        if self.item_state.z > 1.5 {self.warn_ink} else {self.info_ink}
                    }
                    result = vec4(mix(result.xyz, ink.xyz*result.w, 0.65), result.w)
                }
                if self.item_state.x > 0.5 {
                    result = vec4(mix(result.xyz, self.sel_ink.xyz*result.w, 0.55), result.w)
                }
                if self.item_state.w > 0.0 {
                    result = vec4(result.xyz+(vec3(result.w)-result.xyz)*self.item_state.w*self.emph_gain, result.w)
                }
                if self.item_state.w < 0.0 {
                    let flash = clamp(-self.item_state.w, 0.0, 1.0)
                    result = vec4(mix(result.xyz, self.flash_ink.xyz*result.w, 0.6*flash), result.w)
                }
                if self.item_state.y > 0.5 {
                    result *= self.ghost_alpha
                }
                if self.item_dim > 0.0 {
                    result *= mix(1.0, self.dim_alpha, self.item_dim)
                }
                self.fb0 = result
            }
        }
    }

    mod.draw.DrawBoardPad = mod.std.set_type_default() do #(DrawBoardPad::script_shader(vm)) {
        ..mod.draw.DrawQuad
        depth_write: false
        pose_a: vec4(0.0, 0.0, 0.0, 1.0)
        pose_b: vec4(0.0, 0.0, 0.0, 1.0)
        span: vec2(0.0)
        item: -1.0
        look: 0.0
        ink: vec4(0.0)
        live_net: -1.0
        live_seg: -1.0
        live_part: -1.0
        net: -1.0
        rect: vec4(0.0)
        rot: 0.0
        corner: 0.0
        hole: vec4(0.0)
        hole_rot: 0.0
        live_nets: texture_2d(float)
        live_segs: texture_2d(float)
        live_parts: texture_2d(float)
        live_on: uniform(0.0)
        live_time: uniform(0.0)
        live_light: uniform(0.0)
        item_states: texture_2d(float)
        item_states_on: uniform(0.0)
        pane_states: texture_2d(float)
        pane_states_on: uniform(0.0)
        sel_ink: uniform(vec4(0.72, 0.96, 1.0, 1.0))
        warn_ink: uniform(vec4(1.0, 0.77, 0.33, 1.0))
        err_ink: uniform(vec4(1.0, 0.32, 0.39, 1.0))
        info_ink: uniform(vec4(0.57, 0.73, 1.0, 1.0))
        ghost_alpha: uniform(0.25)
        dim_alpha: uniform(0.2)
        emph_gain: uniform(0.35)
        flash_ink: uniform(vec4(0.706, 0.549, 1.0, 1.0))
        fresh_item: uniform(-1.0)
        fresh_mix: uniform(0.0)
        fresh_pending: uniform(0.0)
        fresh_ink: uniform(vec4(0.5, 1.0, 0.78, 1.0))
        lit_net: uniform(-1.0)
        lit_mix: uniform(0.0)
        art_mask: uniform(vec4(0.0))
        cam: uniform(vec4(0.0, 0.0, 1.0, 1.0))
        motion_t: uniform(0.0)
        hover_item: uniform(-1.0)
        hover_mix: uniform(0.0)
        layer_alpha: uniform(1.0)
        layer_ink: uniform(vec4(1.0))
        thin_alpha: uniform(0.4)
        clip: uniform(vec4(-1e20, -1e20, 1e20, 1e20))
        local: varying(vec2f)
        half: varying(vec2f)
        hole_at: varying(vec4f)
        hole_dir: varying(vec2f)
        live_ink: varying(vec3f)
        item_state: varying(vec4f)
        item_dim: varying(float)
        fade: varying(float)
        screen: varying(vec2f)

        vertex: fn() {
            let art = self.art_mask.x > 0.0 || self.art_mask.y > 0.0 || self.art_mask.z > 0.0 || self.art_mask.w > 0.0
            self.item_state = vec4(0.0)
            self.item_dim = 0.0
            let base = if self.ink.w > 0.0 {self.ink} else {self.layer_ink}
            self.live_ink = base.xyz
            if !art && self.live_on > 0.5 {
                if self.live_net >= 0.0 {
                    let net = self.live_nets.sample_nearest(Live.net_uv(self.live_net, self.live_nets.size()))
                    self.live_ink = Live.net_color(net, base.xyz, self.live_light, self.sel_ink.xyz)
                }
            }
            if !art && self.item >= 0.0 {
                if self.item_states_on > 0.5 {
                    let size = self.item_states.size()
                    if self.item < size.x*size.y {
                        self.item_state = self.item_states.sample_nearest(Live.net_uv(self.item, size))
                    }
                }
                if self.pane_states_on > 0.5 {
                    let size = self.pane_states.size()
                    if self.item < size.x*size.y {
                        let pane = self.pane_states.sample_nearest(Live.net_uv(self.item, size))
                        self.item_dim = pane.x
                        // a key's emphasis, over an outside edit's flash (w below zero) only where set
                        if pane.y > 0.0 { self.item_state.w = max(self.item_state.w, pane.y) }
                    }
                }
            }
            let t = if self.span.y <= 0.0 {1.0} else {clamp((self.motion_t-self.span.x)/self.span.y, 0.0, 1.0)}
            let t1 = 1.0-t
            let pose = mix(self.pose_a, self.pose_b, 1.0-t1*t1*t1)
            let co = cos(pose.z)
            let si = sin(pose.z)
            let c0 = self.rect.xy
            let center = pose.xy+vec2(co*c0.x-si*c0.y, si*c0.x+co*c0.y)
            let ang = self.rot+pose.z
            let ax = vec2(cos(ang), sin(ang))
            let ay = vec2(-ax.y, ax.x)
            let ppd = max(self.cam.z*self.cam.w, 0.000001)
            let hw = max(self.rect.z, 0.5/ppd)
            let hh = max(self.rect.w, 0.5/ppd)
            let m = 1.5/ppd
            let lx = mix(-hw-m, hw+m, self.geom.pos.x)
            let ly = mix(-hh-m, hh+m, self.geom.pos.y)
            let w = center+ax*lx+ay*ly
            // The hole in the pad's own frame (the pose turns both alike).
            let hd = self.hole.xy-self.rect.xy
            let cr = cos(self.rot)
            let sr = sin(self.rot)
            let hl = vec2(cr*hd.x+sr*hd.y, -sr*hd.x+cr*hd.y)
            let ha = self.hole_rot-self.rot
            self.local = vec2(lx, ly)
            self.half = vec2(hw, hh)
            self.hole_at = vec4(hl.x, hl.y, self.hole.z, self.hole.w)
            self.hole_dir = vec2(cos(ha), sin(ha))
            self.fade = pose.w*clamp(2.0*min(self.rect.z, self.rect.w)*ppd, self.thin_alpha, 1.0)
            let sp = vec2(self.cam.x+w.x*self.cam.z, self.cam.y-w.y*self.cam.z)
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let ppd = max(self.cam.z*self.cam.w, 0.000001)
            let rc = clamp(self.corner, 0.0, min(self.half.x, self.half.y))
            let q = abs(self.local)-self.half+vec2(rc, rc)
            let dpad = length(max(q, vec2(0.0, 0.0)))+min(max(q.x, q.y), 0.0)-rc
            let hp = self.local-self.hole_at.xy
            let along = clamp(dot(hp, self.hole_dir), -self.hole_at.z, self.hole_at.z)
            let dhole = length(hp-self.hole_dir*along)-self.hole_at.w
            let d = if self.hole_at.w > 0.0 {max(dpad, -dhole)} else {dpad}
            let cover = clamp(0.5-d*ppd, 0.0, 1.0)
            if self.art_mask.x > 0.0 || self.art_mask.y > 0.0 || self.art_mask.z > 0.0 || self.art_mask.w > 0.0 {
                self.fb0 = self.art_mask*(cover*self.fade)
            } else {
                if cover <= 0.0 { discard() }
                let base = if self.ink.w > 0.0 {self.ink} else {self.layer_ink}
                let hover = if self.item >= 0.0 && abs(self.item-self.hover_item) < 0.5 {self.hover_mix} else {0.0}
                let color = if self.live_on > 0.5 {self.live_ink} else {base.xyz}
                let rgb = mix(color, vec3(1.0, 1.0, 1.0), hover*0.3)
                let alpha = cover*self.fade*base.w*self.layer_alpha
                let mut result = vec4(rgb*alpha, alpha)
                // The straight-colour operations expressed in premultiplied space.
                if self.fresh_item >= 0.0 && self.item >= self.fresh_item {
                    result = vec4(mix(result.xyz, self.fresh_ink.xyz*result.w, self.fresh_mix), result.w)
                    if self.fresh_pending > 0.0 {
                        result *= mix(1.0, mix(0.3, 1.0, clamp(1.75+dpad*ppd, 0.0, 1.0)), self.fresh_pending)
                    }
                }
                if self.lit_net >= 0.0 {
                    if abs(self.net-self.lit_net) < 0.5 {
                        result = vec4(result.xyz+(vec3(result.w)-result.xyz)*0.4*self.lit_mix, result.w)
                    } else {
                        result *= 1.0-0.5*self.lit_mix
                    }
                }
                if self.item_state.z > 0.5 {
                    let ink = if self.item_state.z > 2.5 {self.err_ink} else {
                        if self.item_state.z > 1.5 {self.warn_ink} else {self.info_ink}
                    }
                    result = vec4(mix(result.xyz, ink.xyz*result.w, 0.65), result.w)
                }
                if self.item_state.x > 0.5 {
                    result = vec4(mix(result.xyz, self.sel_ink.xyz*result.w, 0.55), result.w)
                }
                if self.item_state.w > 0.0 {
                    result = vec4(result.xyz+(vec3(result.w)-result.xyz)*self.item_state.w*self.emph_gain, result.w)
                }
                if self.item_state.w < 0.0 {
                    let flash = clamp(-self.item_state.w, 0.0, 1.0)
                    result = vec4(mix(result.xyz, self.flash_ink.xyz*result.w, 0.6*flash), result.w)
                }
                if self.item_state.y > 0.5 {
                    result *= self.ghost_alpha
                }
                if self.item_dim > 0.0 {
                    result *= mix(1.0, self.dim_alpha, self.item_dim)
                }
                self.fb0 = result
            }
        }
    }

    mod.draw.DrawBoardTri = mod.std.set_type_default() do #(DrawBoardTri::script_shader(vm)) {
        ..mod.draw.DrawQuad
        depth_write: false
        pose_a: vec4(0.0, 0.0, 0.0, 1.0)
        pose_b: vec4(0.0, 0.0, 0.0, 1.0)
        span: vec2(0.0)
        item: -1.0
        look: 0.0
        ink: vec4(0.0)
        live_net: -1.0
        live_seg: -1.0
        live_part: -1.0
        net: -1.0
        tri_ab: vec4(0.0)
        tri_c: vec2(0.0)
        live_nets: texture_2d(float)
        live_segs: texture_2d(float)
        live_parts: texture_2d(float)
        live_on: uniform(0.0)
        live_time: uniform(0.0)
        live_light: uniform(0.0)
        item_states: texture_2d(float)
        item_states_on: uniform(0.0)
        pane_states: texture_2d(float)
        pane_states_on: uniform(0.0)
        heat: texture_2d(float)
        heat_on: uniform(0.0)
        heat_box: uniform(vec4(0.0, 0.0, 1.0, 1.0))
        heat_mix: uniform(0.0)
        sel_ink: uniform(vec4(0.72, 0.96, 1.0, 1.0))
        warn_ink: uniform(vec4(1.0, 0.77, 0.33, 1.0))
        err_ink: uniform(vec4(1.0, 0.32, 0.39, 1.0))
        info_ink: uniform(vec4(0.57, 0.73, 1.0, 1.0))
        ghost_alpha: uniform(0.25)
        dim_alpha: uniform(0.2)
        emph_gain: uniform(0.35)
        flash_ink: uniform(vec4(0.706, 0.549, 1.0, 1.0))
        fresh_item: uniform(-1.0)
        fresh_mix: uniform(0.0)
        fresh_pending: uniform(0.0)
        fresh_ink: uniform(vec4(0.5, 1.0, 0.78, 1.0))
        lit_net: uniform(-1.0)
        lit_mix: uniform(0.0)
        // A zone's fill (look 1) under the layer's pads and tracks: they read on top of it.
        pour_alpha: uniform(0.42)
        art_mask: uniform(vec4(0.0))
        cam: uniform(vec4(0.0, 0.0, 1.0, 1.0))
        motion_t: uniform(0.0)
        hover_item: uniform(-1.0)
        hover_mix: uniform(0.0)
        layer_alpha: uniform(1.0)
        layer_ink: uniform(vec4(1.0))
        thin_alpha: uniform(0.4)
        clip: uniform(vec4(-1e20, -1e20, 1e20, 1e20))
        live_ink: varying(vec3f)
        item_state: varying(vec4f)
        item_dim: varying(float)
        fade: varying(float)
        screen: varying(vec2f)
        heat_uv: varying(vec2f)

        vertex: fn() {
            let art = self.art_mask.x > 0.0 || self.art_mask.y > 0.0 || self.art_mask.z > 0.0 || self.art_mask.w > 0.0
            self.item_state = vec4(0.0)
            self.item_dim = 0.0
            let base = if self.ink.w > 0.0 {self.ink} else {self.layer_ink}
            self.live_ink = base.xyz
            if !art && self.live_on > 0.5 && self.live_net >= 0.0 {
                let net = self.live_nets.sample_nearest(Live.net_uv(self.live_net, self.live_nets.size()))
                self.live_ink = Live.net_color(net, base.xyz, self.live_light, self.sel_ink.xyz)
            }
            if !art && self.item >= 0.0 {
                if self.item_states_on > 0.5 {
                    let size = self.item_states.size()
                    if self.item < size.x*size.y {
                        self.item_state = self.item_states.sample_nearest(Live.net_uv(self.item, size))
                    }
                }
                if self.pane_states_on > 0.5 {
                    let size = self.pane_states.size()
                    if self.item < size.x*size.y {
                        let pane = self.pane_states.sample_nearest(Live.net_uv(self.item, size))
                        self.item_dim = pane.x
                        // a key's emphasis, over an outside edit's flash (w below zero) only where set
                        if pane.y > 0.0 { self.item_state.w = max(self.item_state.w, pane.y) }
                    }
                }
            }
            let t = if self.span.y <= 0.0 {1.0} else {clamp((self.motion_t-self.span.x)/self.span.y, 0.0, 1.0)}
            let t1 = 1.0-t
            let pose = mix(self.pose_a, self.pose_b, 1.0-t1*t1*t1)
            let co = cos(pose.z)
            let si = sin(pose.z)
            // The quad's corners (0,0), (1,0), (1,1), (0,1) become a, b, c, c: one triangle and
            // one degenerate.
            let p = mix(mix(self.tri_ab.xy, self.tri_ab.zw, self.geom.pos.x), self.tri_c, self.geom.pos.y)
            let w = pose.xy+vec2(co*p.x-si*p.y, si*p.x+co*p.y)
            self.heat_uv = (w-self.heat_box.xy)/(self.heat_box.zw-self.heat_box.xy)
            self.fade = pose.w
            let sp = vec2(self.cam.x+w.x*self.cam.z, self.cam.y-w.y*self.cam.z)
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            if self.art_mask.x > 0.0 || self.art_mask.y > 0.0 || self.art_mask.z > 0.0 || self.art_mask.w > 0.0 {
                self.fb0 = self.art_mask*self.fade
            } else {
                let base = if self.ink.w > 0.0 {self.ink} else {self.layer_ink}
                // A pour's fill never lights with hover: its edge does (the stroke shader).
                let hover = if self.item >= 0.0 && self.look < 0.5 && abs(self.item-self.hover_item) < 0.5 {self.hover_mix} else {0.0}
                let color = if self.live_on > 0.5 {self.live_ink} else {base.xyz}
                let rgb = mix(color, vec3(1.0, 1.0, 1.0), hover*0.3)
                let alpha = self.fade*base.w*self.layer_alpha*(if self.look > 0.5 {self.pour_alpha} else {1.0})
                let mut result = vec4(rgb*alpha, alpha)
                // The heat glows over the substrate only.
                if self.heat_on > 0.5 {
                    let uv = self.heat_uv
                    if uv.x >= 0.0 && uv.y >= 0.0 && uv.x <= 1.0 && uv.y <= 1.0 {
                        let s = self.heat.sample(uv)
                        if s.y > 0.004 {
                            let k = s.x/s.y
                            let m = self.heat_mix*(0.3+0.6*k)*s.y
                            result = vec4(mix(result.xyz, Live.mag(k, self.live_light)*result.w, m), result.w)
                        }
                    }
                }
                // The straight-colour operations expressed in premultiplied space.
                if self.fresh_item >= 0.0 && self.item >= self.fresh_item {
                    result = vec4(mix(result.xyz, self.fresh_ink.xyz*result.w, self.fresh_mix), result.w)
                    if self.fresh_pending > 0.0 {
                        result *= mix(1.0, 0.3, self.fresh_pending)
                    }
                }
                if self.lit_net >= 0.0 {
                    if abs(self.net-self.lit_net) < 0.5 {
                        result = vec4(result.xyz+(vec3(result.w)-result.xyz)*0.4*self.lit_mix, result.w)
                    } else {
                        result *= 1.0-0.5*self.lit_mix
                    }
                }
                if self.item_state.z > 0.5 {
                    let ink = if self.item_state.z > 2.5 {self.err_ink} else {
                        if self.item_state.z > 1.5 {self.warn_ink} else {self.info_ink}
                    }
                    result = vec4(mix(result.xyz, ink.xyz*result.w, 0.65), result.w)
                }
                if self.item_state.x > 0.5 {
                    result = vec4(mix(result.xyz, self.sel_ink.xyz*result.w, 0.55), result.w)
                }
                if self.item_state.w > 0.0 {
                    result = vec4(result.xyz+(vec3(result.w)-result.xyz)*self.item_state.w*self.emph_gain, result.w)
                }
                if self.item_state.w < 0.0 {
                    let flash = clamp(-self.item_state.w, 0.0, 1.0)
                    result = vec4(mix(result.xyz, self.flash_ink.xyz*result.w, 0.6*flash), result.w)
                }
                if self.item_state.y > 0.5 {
                    result *= self.ghost_alpha
                }
                if self.item_dim > 0.0 {
                    result *= mix(1.0, self.dim_alpha, self.item_dim)
                }
                self.fb0 = result
            }
        }
    }

    // A dedicated footprint stream shares the pad instance layout. The rectangle is in
    // board mm, rot is the placement angle and ink.x holds the packed emitter colour.
    mod.draw.DrawBoardGlow = mod.std.set_type_default() do #(DrawBoardGlow::script_shader(vm)) {
        ..mod.draw.DrawBoardPad
        glow_ink: varying(vec4f)
        glow_drag_item: uniform(-1.0)
        glow_drag_at: uniform(vec2(0.0))
        vertex: fn() {
            self.glow_ink = vec4(0.0)
            let size = self.live_parts.size()
            if self.live_on > 0.5 && self.live_part >= 0.0 && self.live_part < size.x*size.y {
                let part = self.live_parts.sample_nearest(Live.net_uv(self.live_part, size))
                self.glow_ink = Live.part_glow(part.xy, self.ink.x, self.live_light)
            }
            // One default board grid step (0.5 mm) outside the courtyard.
            let extent = if self.glow_ink.w > 0.0 {self.rect.zw+vec2(0.5)} else {vec2(0.0)}
            self.local = (2.0*self.geom.pos-vec2(1.0))*extent
            let ax = vec2(cos(self.rot), sin(self.rot))
            let drag = if self.item >= 0.0 && self.item == self.glow_drag_item {self.glow_drag_at-self.pose_a.xy} else {vec2(0.0)}
            let w = self.rect.xy+ax*self.local.x+vec2(-ax.y, ax.x)*self.local.y+drag
            let sp = vec2(self.cam.x+w.x*self.cam.z, self.cam.y-w.y*self.cam.z)
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let d = length(max(abs(self.local)-self.rect.zw, vec2(0.0)))
            let a = Live.glow_alpha(self.glow_ink.w, d, 0.5)
            self.fb0 = vec4(self.glow_ink.xyz*a, a)
        }
    }

    // A layer target unions its shapes with a per-channel max (premultiplied colour, all four
    // channels): a capsule end into a pad or two overlapping segments cover exactly once, where
    // over-blending fattens every joint. Same instance layout as the shaders they extend.
    mod.draw.DrawBoardStrokeMax = mod.std.set_type_default() do #(DrawBoardStrokeMax::script_shader(vm)) {
        ..mod.draw.DrawBoardStroke
        blend_op: @Max
    }
    mod.draw.DrawBoardPadMax = mod.std.set_type_default() do #(DrawBoardPadMax::script_shader(vm)) {
        ..mod.draw.DrawBoardPad
        blend_op: @Max
    }
    mod.draw.DrawBoardTriMax = mod.std.set_type_default() do #(DrawBoardTriMax::script_shader(vm)) {
        ..mod.draw.DrawBoardTri
        blend_op: @Max
    }

    mod.draw.DrawBoardLayer = mod.std.set_type_default() do #(DrawBoardLayer::script_shader(vm)) {
        ..mod.draw.DrawQuad
        tex: texture_2d(float)
        opacity: 1.0
        pixel: fn() {
            return self.tex.sample(self.pos)*self.opacity
        }
    }
}

/// A board stroke (module docs; fields as `inst::StrokeInst`).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardStroke {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub pose_a: Vec4f,
    #[live]
    pub pose_b: Vec4f,
    #[live]
    pub span: Vec2f,
    #[live]
    pub item: f32,
    #[live]
    pub look: f32,
    #[live]
    pub ink: Vec4f,
    #[live]
    pub live_net: f32,
    #[live]
    pub live_seg: f32,
    #[live]
    pub live_part: f32,
    #[live]
    pub seg: Vec4f,
    #[live]
    pub sag: f32,
    #[live]
    pub width: f32,
    #[live]
    pub along0: f32,
    /// The board net (`NetId`), -1 none: last, after the shape (inst.rs).
    #[live]
    pub net: f32,
}

/// A board pad, via, ring or hole (module docs; fields as `inst::PadInst`).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardPad {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub pose_a: Vec4f,
    #[live]
    pub pose_b: Vec4f,
    #[live]
    pub span: Vec2f,
    #[live]
    pub item: f32,
    #[live]
    pub look: f32,
    #[live]
    pub ink: Vec4f,
    #[live]
    pub live_net: f32,
    #[live]
    pub live_seg: f32,
    #[live]
    pub live_part: f32,
    #[live]
    pub rect: Vec4f,
    #[live]
    pub rot: f32,
    #[live]
    pub corner: f32,
    #[live]
    pub hole: Vec4f,
    #[live]
    pub hole_rot: f32,
    /// The board net (`NetId`), -1 none: last, after the shape (inst.rs).
    #[live]
    pub net: f32,
}

/// A filled triangle of a polygon (module docs; fields as `inst::TriInst`). Its edges get their
/// anti-aliasing from the polygon's hairline ring strokes, so shared inner edges never seam.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardTri {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub pose_a: Vec4f,
    #[live]
    pub pose_b: Vec4f,
    #[live]
    pub span: Vec2f,
    #[live]
    pub item: f32,
    #[live]
    pub look: f32,
    #[live]
    pub ink: Vec4f,
    #[live]
    pub live_net: f32,
    #[live]
    pub live_seg: f32,
    #[live]
    pub live_part: f32,
    #[live]
    pub tri_ab: Vec4f,
    #[live]
    pub tri_c: Vec2f,
    /// The board net (`NetId`), -1 none: last, after the shape (inst.rs).
    #[live]
    pub net: f32,
}

/// A footprint underlay with the same instance layout and texture slots as a board pad.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardGlow {
    #[deref]
    pub draw_super: DrawBoardPad,
}

/// `DrawBoardStroke` max-blended into a layer target (no field of its own).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardStrokeMax {
    #[deref]
    pub draw_super: DrawBoardStroke,
}

/// `DrawBoardPad` max-blended into a layer target (no field of its own).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardPadMax {
    #[deref]
    pub draw_super: DrawBoardPad,
}

/// `DrawBoardTri` max-blended into a layer target (no field of its own).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardTriMax {
    #[deref]
    pub draw_super: DrawBoardTri,
}

/// One layer's offscreen picture composited at an opacity (premultiplied in, premultiplied out).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardLayer {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub opacity: f32,
}
