//! World-space canvas instances, shared by EBOARD and EINST (the same names
//! and f32 lanes in both writers). DrawCanvas fields follow the DrawQuad
//! base in order; each shape appends its own fields after DrawCanvas:
//!
//! | Common field | Lanes | Meaning |
//! | --- | --- | --- |
//! | pose_a | 4 | Start x, y, CCW radians, alpha |
//! | pose_b | 4 | End x, y, CCW radians, alpha |
//! | span | 2 | Motion-clock start and duration; nonpositive duration is at rest |
//! | item | 1 | Exact hover and item-state index below 2^24, or -1 |
//! | look | 1 | Look code 0..=7 |
//! | ink | 4 | Straight-alpha stroke RGBA |
//! | live_ix | 4 | static live indices, -1 for none: x net row, y wire segment row (a `NetTree` segment id), z part row; w an LED's own colour packed 0xRRGGBB in display sRGB, 0 none ([`super::emitter::emitter_ix`]) |
//!
//! | Shape | Following fields, in order |
//! | --- | --- |
//! | Line | seg:4 (ax, ay, bx, by), along0:1 (world arc length along wire at segment start), width:2 (points, world floor), dash:2 (on, off points), cap:1 (0 round, 1 butt), follow:2 (endpoint drag weights), press:4 (end offsets while pane-state y holds its item pressed) |
//! | Box | A stroke-free box with a positive stroke.y and a live part row is a glow underlay. Its stroke.y is the world margin. rect:4 (cx, cy, half_w, half_h), corner:1 (world radius), stroke:2 (points, world floor), fill:4 (straight RGBA) |
//! | Mini | rect:4 (cx, cy, half_w, half_h), mini_slot:1 (strip slot, -1 none), stroke:2 (points, world floor), fill:4 (straight RGBA) |
//! | Pin | at:2 (outer pin end), dir:2 (outward unit vector), size:1 (world half size), live_pin:1 (pin row, -1 none) |
//! | Arc | circle:4 (cx, cy, radius, unused), sweep:2 (start, CCW sweep radians), along0:1 (world arc length along wire at arc start), stroke:2, fill:4 |
//! | Bridge | ends:4 (rest ax, ay, bx, by), b_pose:4 (B start/end xy offsets), b_span:2 (B clock start/duration), bend:1 (signed sagitta/chord), stroke:2 (points, world floor), fade:4 (in start/duration, out start/duration; linear, nonpositive duration steps) |
//!
//! Bridges join independently travelling pins and fade into their landed orthogonal routes.
//!
//! Geometry is item-local, world Y up. Poses mix component-wise with cubic
//! ease-out. All shaders take cam (offset x/y, points per unit, DPI),
//! motion_t, hover_item, hover_mix, drag_item (-1 off), drag_offset (world xy),
//! layer_alpha, time (stale crawl),
//! live_time (seconds since the live clock's epoch), live_light (1 light, 0 dark),
//! accent_hover, accent_focus, accent_ai, violation, stale,
//! and clip (x0, y0, x1, y1 in logical screen points).
//!
//! Live values are never instance data. The host writes row textures
//! once a frame (`live::LiveRows`, one RGBA32F texel per index, 256 a row) and
//! every canvas draw binds them ([`CanvasLive`]). Texture slots 0, 1 and 2 carry
//! nets (`live::net_row`: voltage ramp x or -1, volts, current level, emphasis drawn as an item's),
//! wire segments (x flow phase at the live clock's epoch in cycles, y signed
//! flow speed in cycles a second, 0 none) and parts (x glow 0..1, y an LED's
//! light output 0..1). The vertex stage samples them by `live_ix`. A part glows on
//! the magnitude ramp by its dissipation (x); an instance with an LED colour
//! (`live_ix.w`) glows in that colour by its light output (y) instead: a glow box
//! with one is a round bloom (rect's centre, radius half_w, stroke.y further reach),
//! and any other instance with one (the LED's strokes) takes that colour as it lights.
//!
//! Item and pane states use the same row layout and nearest sampling, indexed
//! by `item` (`electronics/shared/item-state.md`). Slot 3 is the host's
//! item-state texture: x selected, y ghost, z violation (0 none, 1 info,
//! 2 warning, 3 error), w emphasis 0..1 or, below zero, an outside edit's flash at strength -w
//! (the AI accent's rim). Slot 4 is the pane-state texture:
//! x dim 0..1, y held momentary input 0/1, z/w reserved zero. `item_states_on` and `pane_states_on`
//! enable them; an absent or out-of-bounds item reads zero state. `look`
//! remains for single-instance states; selection and other states that change
//! many items at once come from textures. Hover stays the two uniforms.
//!
//! Slot 5 holds mini traces (`live::mini`): 64 texels per slot, a scale/head/fraction
//! header followed by 63 ring columns (lo, hi, mean, voltage ramp x). Mini shows
//! 48 columns, with 15% vertical padding and a two-point rounded background.
//! Its `mini_hover` uniform names the hovered strip slot (-1 none), drawn as a shadow.
//!
//! Slot 6 holds `live_pins` (`live::pin_row`): firmware mode, level, PWM duty and emphasis.
//! Pin marks sample it in the pixel stage at their static `live_pin` index, using `Live.net_uv`
//! because pins share the same row layout. No instance changes when firmware state publishes.
//!
//! Strokes never fall under one device pixel; a box/mini/arc/bridge stroke with x = 0
//! is absent. A full arc can fill a disc. A frame changes uniforms only,
//! O(1) per retained layer; O(instances) publication work happens only on
//! set, with an upload on its next submission, never on camera/motion updates.
use super::{Camera2d, Tokens};
use makepad_widgets_core::{
    makepad_platform::retained_instances::{InstanceSegment, RetainedInstances},
    *,
};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    use mod.elec_live.*

    mod.draw.DrawCanvasGrid = mod.std.set_type_default() do #(DrawCanvasGrid::script_shader(vm)) {
        ..mod.draw.DrawQuad
        pixel: fn() {
            let bg = vec4(self.canvas.rgb*self.canvas.a, self.canvas.a)
            let scale = self.px_per_unit
            if 10.0*scale <= 6.0 { return bg }

            let p = self.pos*self.rect_size
            let world = self.top_left + vec2(p.x, -p.y)/scale
            let stride = if scale > 6.0 {1.0} else {10.0}
            let nearest = floor(world/stride + vec2(0.5))*stride
            let tenth = 10.0*floor(nearest/10.0 + vec2(0.5))
            let major_x = nearest.x == tenth.x
            let major_y = nearest.y == tenth.y
            let ink = if major_x || major_y {self.grid_major} else {self.grid}
            let radius = if major_x || major_y {1.0} else {0.75}
            // Only the intersections of two major lines survive the fine grid's fade.
            let spacing = if major_x && major_y {10.0*scale} else {scale}
            let edge = radius - length(world-nearest)*scale
            let cover = clamp(edge*self.draw_pass.dpi_factor + 0.5, 0.0, 1.0)
            return mix(bg, vec4(ink.rgb, 1.0), ink.a*cover*smoothstep(6.0, 10.0, spacing))
        }
    }

    mod.draw.DrawCanvas = mod.std.set_type_default() do #(DrawCanvas::script_shader(vm)) {
        ..mod.draw.DrawQuad
        depth_write: false
        pose_a: vec4(0.0, 0.0, 0.0, 1.0)
        pose_b: vec4(0.0, 0.0, 0.0, 1.0)
        span: vec2(0.0)
        item: -1.0
        look: 0.0
        ink: vec4(1.0)
        live_ix: vec4(-1.0, -1.0, -1.0, 0.0)
        live_nets: texture_2d(float)
        live_segs: texture_2d(float)
        live_parts: texture_2d(float)
        item_states: texture_2d(float)
        pane_states: texture_2d(float)
        live_mini: texture_2d(float)
        live_pins: texture_2d(float)
        item_states_on: uniform(0.0)
        pane_states_on: uniform(0.0)
        cam: uniform(vec4(0.0, 0.0, 1.0, 1.0))
        motion_t: uniform(0.0)
        hover_item: uniform(-1.0)
        hover_mix: uniform(0.0)
        drag_item: uniform(-1.0)
        drag_offset: uniform(vec2(0.0))
        layer_alpha: uniform(1.0)
        time: uniform(0.0)
        live_time: uniform(0.0)
        live_light: uniform(0.0)
        accent_hover: uniform(vec4(0.0))
        accent_focus: uniform(vec4(0.0))
        accent_ai: uniform(vec4(0.0))
        violation: uniform(vec4(0.0))
        stale: uniform(vec4(0.0))
        clip: uniform(vec4(-1e20, -1e20, 1e20, 1e20))
        screen: varying(vec2f)
        pose_alpha: varying(float)
        half_width: varying(float)
        // The net's texel, or (-1, 0, 0, 0) without a net row.
        live_net: varying(vec4f)
        // Flow phase and speed from the segment row.
        live_flow: varying(vec2f)
        // An LED's light on its own strokes: its colour and brightness, zero when dark or none.
        live_emit: varying(vec4f)
        item_st: varying(vec4f)
        pane_dim: varying(float)
        pane_press: varying(float)

        // Every vertex stage starts here: the instance's live values from the host's rows.
        canvas_live: fn() {
            var net = vec4(-1.0, 0.0, 0.0, 0.0)
            var seg = vec4(0.0)
            if self.live_ix.x >= 0.0 {
                net = self.live_nets.sample_nearest(Live.net_uv(self.live_ix.x, self.live_nets.size()))
            }
            if self.live_ix.y >= 0.0 {
                seg = self.live_segs.sample_nearest(Live.net_uv(self.live_ix.y, self.live_segs.size()))
            }
            self.live_net = net
            self.live_flow = seg.xy
            self.live_emit = vec4(0.0)
            if self.live_ix.w > 0.0 && self.live_ix.z >= 0.0 {
                let size = self.live_parts.size()
                if self.live_ix.z < size.x*size.y {
                    let part = self.live_parts.sample_nearest(Live.net_uv(self.live_ix.z, size))
                    self.live_emit = Live.part_glow(part.xy, self.live_ix.w, self.live_light)
                }
            }
            self.item_st = vec4(0.0)
            let n = self.item_states.size()
            if self.item_states_on > 0.5 && self.item >= 0.0 && self.item < n.x * n.y {
                self.item_st = self.item_states.sample_nearest(Live.net_uv(self.item, n))
            }
            self.pane_dim = 0.0
            self.pane_press = 0.0
            let p = self.pane_states.size()
            if self.pane_states_on > 0.5 && self.item >= 0.0 && self.item < p.x * p.y {
                let st = self.pane_states.sample_nearest(Live.net_uv(self.item, p))
                self.pane_dim = st.x
                self.pane_press = st.y
            }
        }
        canvas_pose: fn() -> vec4 {
            let t = if self.span.y <= 0.0 {1.0} else {clamp((self.motion_t-self.span.x)/self.span.y, 0.0, 1.0)}
            let u = 1.0-t
            return mix(self.pose_a, self.pose_b, 1.0-u*u*u)
        }
        canvas_drag: fn(pose: vec4) -> vec4 {
            if self.drag_item >= 0.0 && abs(self.item-self.drag_item) < 0.5 {
                return vec4(pose.xy+self.drag_offset, pose.zw)
            }
            return pose
        }
        canvas_point: fn(p: vec2, pose: vec4) -> vec2 {
            let c = cos(pose.z)
            let s = sin(pose.z)
            let w = pose.xy+vec2(c*p.x-s*p.y, s*p.x+c*p.y)
            return vec2(self.cam.x+w.x*self.cam.z, self.cam.y-w.y*self.cam.z)
        }
        canvas_margin: fn(half: float) -> float {
            let px = 1.0/max(self.cam.w, 0.0001)
            // Two pixels for the largest rim (a check's outline is 1.5), then 1.5 pixels of AA.
            return half+3.5*px
        }
        canvas_cover: fn(d: float) -> float {
            return clamp(-d*max(self.cam.w, 0.0001)+0.5, 0.0, 1.0)
        }
        canvas_violation: fn() -> float {
            return if abs(self.look-5.0) < 0.5 {3.0} else {self.item_st.z}
        }
        // A check's outline strength: an error full, a warning and a note less.
        canvas_violation_alpha: fn(level: float) -> float {
            return if level >= 2.5 {1.0} else if level >= 1.5 {0.8} else if level >= 0.5 {0.55} else {0.0}
        }
        canvas_rim: fn(outer: float, pixels: float) -> float {
            let width = pixels/max(self.cam.w, 0.0001)
            return max(self.canvas_cover(outer-width)-self.canvas_cover(outer), 0.0)
        }
        canvas_dash: fn(along: float, on: float, off: float) -> float {
            if on <= 0.0 || off <= 0.0 { return 1.0 }
            let period = on+off
            let d = abs(fract((along+off*0.5)/period)*period-period*0.5)-on*0.5
            return self.canvas_cover(d)
        }
        canvas_over: fn(dst: vec4, src: vec4, coverage: float) -> vec4 {
            let a = clamp(src.w*coverage, 0.0, 1.0)
            return vec4(src.xyz*a+dst.xyz*(1.0-a), a+dst.w*(1.0-a))
        }
        // d is the stroke band's distance; outer excludes the inward rim
        // of a closed shape. dot_d is a distance to round stale dots.
        // along is in screen points; along0 is world arc length along the wire at this stroke's start.
        canvas_paint: fn(d: float, outer: float, dot_d: float, along: float, along0: float, fill_d: float, fill: vec4, stroke_on: float, flow_on: float, dash: vec2) -> vec4 {
            // Moving preview geometry ignores its retained copy's ghost channel.
            let ghost = abs(self.look-3.0) < 0.5 || (self.item_st.y > 0.5 && abs(self.look-2.0) >= 0.5)
            let stale = abs(self.look-4.0) < 0.5
            let violation_level = self.canvas_violation()
            let selected = abs(self.look-1.0) < 0.5 || self.item_st.x > 0.5
            let mut emphasis = clamp(self.item_st.w, 0.0, 1.0)
            // an outside edit's flash: w below zero, fading back to it
            let flash = clamp(-self.item_st.w, 0.0, 1.0)
            let disabled = abs(self.look-6.0) < 0.5
            let mut color = self.ink
            if self.live_ix.x >= 0.0 {
                // A net's emphasis draws as an emphasised item's (the focus rim and tint below), so
                // its stroke keeps the voltage's colour.
                color = vec4(Live.net_color(vec4(self.live_net.xyz, 0.0), color.xyz, self.live_light, self.accent_focus.xyz), color.w)
                emphasis = max(emphasis, clamp(self.live_net.w, 0.0, 1.0))
            }
            if self.live_emit.w > 0.0 {
                // A lit LED's strokes take its light's colour as its bloom does (a few mA reads as lit).
                color = vec4(mix(color.xyz, self.live_emit.xyz, clamp(self.live_emit.w*1.8, 0.0, 1.0)), color.w)
            }
            if ghost {
                let luma = dot(color.xyz, vec3(0.2126, 0.7152, 0.0722))
                color = vec4(mix(color.xyz, vec3(luma), 0.6), color.w)
            }
            if stale { color = self.stale }
            let mut stroke_cover = self.canvas_cover(d)*stroke_on
            if ghost { stroke_cover *= self.canvas_dash(along, 4.0, 4.0) }
            if stale { stroke_cover = self.canvas_cover(max(d, dot_d))*stroke_on }
            stroke_cover *= self.canvas_dash(along, dash.x, dash.y)
            if flow_on > 0.5 && self.live_flow.y != 0.0 {
                let s = along0+along/max(self.cam.z, 0.000001)
                let a = Live.flow(self.live_time, s, self.cam.z*self.cam.w, self.live_flow.x, self.live_flow.y)
                color = vec4(Live.lit(color.xyz, a, self.live_light), color.w)
            }
            color = vec4(mix(color.xyz, self.accent_focus.xyz, 0.25*emphasis), color.w)
            color = vec4(mix(color.xyz, self.accent_ai.xyz, 0.4*flash), color.w)
            let mut result = vec4(0.0)
            result = self.canvas_over(result, fill, self.canvas_cover(fill_d))
            // A check finding: the item keeps its ink, outlined in the violation colour (no glow);
            // its badge is the sheet's warning mark. Hover and selection rims draw over it.
            if violation_level >= 0.5 {
                let rim = self.canvas_rim(outer, 1.5)*stroke_on
                result = self.canvas_over(result, self.violation, rim*self.canvas_violation_alpha(violation_level))
            }
            // Rims begin at the outside of the unpatterned stroke.
            if self.item >= 0.0 && self.hover_item >= 0.0 && abs(self.item-self.hover_item) < 0.5 {
                let rim = self.canvas_rim(outer, 1.0)*stroke_on
                result = self.canvas_over(result, self.accent_hover, rim*clamp(self.hover_mix, 0.0, 1.0))
            }
            if selected {
                let rim = self.canvas_rim(outer, 2.0)*stroke_on
                result = self.canvas_over(result, self.accent_focus, rim)
            } else if abs(self.look-2.0) < 0.5 {
                let rim = self.canvas_rim(outer, 1.0)*stroke_on
                result = self.canvas_over(result, self.accent_focus, rim)
            } else if abs(self.look-7.0) < 0.5 {
                let rim = self.canvas_rim(outer, 1.0)*stroke_on
                result = self.canvas_over(result, self.accent_ai, rim)
            }
            if !selected && emphasis > 0.0 {
                let rim = self.canvas_rim(outer, 1.0)*stroke_on
                result = self.canvas_over(result, self.accent_focus, rim*emphasis)
            }
            if !selected && flash > 0.0 {
                let rim = self.canvas_rim(outer, 2.0)*stroke_on
                result = self.canvas_over(result, self.accent_ai, rim*flash)
            }
            result = self.canvas_over(result, color, stroke_cover)
            let fade = if ghost {0.33} else if disabled {0.25} else {1.0}
            // Return straight RGB and coverage/ink alpha; the fragment
            // applies the pose and layer opacity once, after compositing.
            return vec4(result.xyz/max(result.w, 0.000001), result.w*fade*mix(1.0, 0.25, self.pane_dim))
        }
    }

    mod.draw.DrawCanvasLine = mod.std.set_type_default() do #(DrawCanvasLine::script_shader(vm)) {
        ..mod.draw.DrawCanvas
        seg: vec4(0.0)
        along0: 0.0
        width: vec2(1.0, 0.0)
        dash: vec2(0.0)
        cap: 0.0
        follow: vec2(0.0)
        press: vec4(0.0)
        line_a: varying(vec2f)
        line_b: varying(vec2f)

        vertex: fn() {
            self.canvas_live()
            let pose = self.canvas_drag(self.canvas_pose())
            // A pressed item's actuator lines move their ends by their press offsets.
            let pressed = if self.pane_press > 0.5 {1.0} else {0.0}
            let seg = self.seg+pressed*self.press
            let a = self.canvas_point(seg.xy, vec4(pose.xy+self.follow.x*self.drag_offset, pose.zw))
            let b = self.canvas_point(seg.zw, vec4(pose.xy+self.follow.y*self.drag_offset, pose.zw))
            let delta = b-a
            let len = length(delta)
            let dir = if len > 0.000001 {delta/len} else {vec2(1.0, 0.0)}
            let normal = vec2(-dir.y, dir.x)
            let px = 1.0/max(self.cam.w, 0.0001)
            let half = 0.5*max(max(self.width.x, self.width.y*self.cam.z), px)
            let margin = self.canvas_margin(half)
            let u = mix(-margin, len+margin, self.geom.pos.x)
            let v = mix(-margin, margin, self.geom.pos.y)
            let sp = a+dir*u+normal*v
            self.line_a = a
            self.line_b = b
            self.half_width = half
            self.pose_alpha = pose.w
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let delta = self.line_b-self.line_a
            let len = length(delta)
            let dir = if len > 0.000001 {delta/len} else {vec2(1.0, 0.0)}
            let p = sp-self.line_a
            let along = dot(p, dir)
            let across = dot(p, vec2(-dir.y, dir.x))
            let dx = max(max(-along, along-len), 0.0)
            let round_d = length(vec2(dx, across))-self.half_width
            let q = vec2(abs(along-len*0.5)-len*0.5, abs(across)-self.half_width)
            let butt_d = length(max(q, vec2(0.0)))+min(max(q.x, q.y), 0.0)
            let d = if self.cap > 0.5 {butt_d} else {round_d}
            // A 1.5-point dot every 4.5 points, advanced along A -> B.
            let mut dot_d = 1e20
            if abs(self.look-4.0) < 0.5 {
                let first = fract((self.time*6.0+0.75)/4.5)*4.5
                if first <= len {
                    let last = first+floor((len-first)/4.5)*4.5
                    let center = clamp(first+floor((along-first)/4.5+0.5)*4.5, first, last)
                    let dot_radius = max(0.75, 0.5/max(self.cam.w, 0.0001))
                    dot_d = length(vec2(along-center, across))-dot_radius
                }
            }
            let color = self.canvas_paint(d, d, dot_d, along, self.along0, 1e20, vec4(0.0), 1.0, 1.0, self.dash)
            let a = color.w*self.pose_alpha*self.layer_alpha
            self.fb0 = vec4(color.xyz*a, a)
        }
    }

    mod.draw.DrawCanvasBox = mod.std.set_type_default() do #(DrawCanvasBox::script_shader(vm)) {
        ..mod.draw.DrawCanvas
        rect: vec4(0.0)
        corner: 0.0
        stroke: vec2(1.0, 0.0)
        fill: vec4(0.0)
        box_center: varying(vec2f)
        box_half: varying(vec2f)
        box_turn: varying(vec2f)
        box_radius: varying(float)
        glow_ink: varying(vec4f)

        box_is_glow: fn() -> bool {
            return self.stroke.x == 0.0 && self.stroke.y > 0.0 && self.live_ix.z >= 0.0
        }

        // Clockwise from the top-left top tangent, in the rotated
        // screen frame: top, TR arc, right, BR arc, bottom, BL arc,
        // left, TL arc. Project onto the nearest outline feature.
        box_along: fn(p: vec2) -> float {
            let h = self.box_half
            let r = self.box_radius
            let b = h-vec2(r)
            let tx = 2.0*b.x
            let ty = 2.0*b.y
            let arc = 1.57079632679*r
            let v = abs(p)-b
            if v.x > 0.0 && v.y > 0.0 {
                if p.x >= 0.0 && p.y < 0.0 { return tx+atan2(v.x, v.y)*r }
                if p.x >= 0.0 && p.y >= 0.0 { return tx+arc+ty+atan2(v.y, v.x)*r }
                if p.x < 0.0 && p.y >= 0.0 { return 2.0*tx+2.0*arc+ty+atan2(v.x, v.y)*r }
                return 2.0*tx+3.0*arc+2.0*ty+atan2(v.y, v.x)*r
            }
            if abs(p.x)-h.x > abs(p.y)-h.y {
                if p.x >= 0.0 { return tx+arc+clamp(p.y+b.y, 0.0, ty) }
                return 2.0*tx+3.0*arc+ty+clamp(b.y-p.y, 0.0, ty)
            }
            if p.y < 0.0 { return clamp(p.x+b.x, 0.0, tx) }
            return tx+2.0*arc+ty+clamp(b.x-p.x, 0.0, tx)
        }
        box_point: fn(along: float) -> vec2 {
            let h = self.box_half
            let r = self.box_radius
            let b = h-vec2(r)
            let tx = 2.0*b.x
            let ty = 2.0*b.y
            let arc = 1.57079632679*r
            let perimeter = 2.0*tx+2.0*ty+4.0*arc
            let mut s = fract(along/max(perimeter, 0.000001))*perimeter
            if s < tx { return vec2(-b.x+s, -h.y) }
            s -= tx
            if s < arc {
                let a = -1.57079632679+s/max(r, 0.000001)
                return vec2(b.x, -b.y)+r*vec2(cos(a), sin(a))
            }
            s -= arc
            if s < ty { return vec2(h.x, -b.y+s) }
            s -= ty
            if s < arc {
                let a = s/max(r, 0.000001)
                return b+r*vec2(cos(a), sin(a))
            }
            s -= arc
            if s < tx { return vec2(b.x-s, h.y) }
            s -= tx
            if s < arc {
                let a = 1.57079632679+s/max(r, 0.000001)
                return vec2(-b.x, b.y)+r*vec2(cos(a), sin(a))
            }
            s -= arc
            if s < ty { return vec2(-h.x, b.y-s) }
            s -= ty
            let a = 3.14159265359+s/max(r, 0.000001)
            return -b+r*vec2(cos(a), sin(a))
        }
        vertex: fn() {
            self.canvas_live()
            let pose = self.canvas_drag(self.canvas_pose())
            let center = self.canvas_point(self.rect.xy, pose)
            let half_size = max(self.rect.zw, vec2(0.0))*self.cam.z
            let radius = clamp(self.corner*self.cam.z, 0.0, min(half_size.x, half_size.y))
            let px = 1.0/max(self.cam.w, 0.0001)
            let half = if self.stroke.x > 0.0 {0.5*max(max(self.stroke.x, self.stroke.y*self.cam.z), px)} else {0.0}
            let mut extent = half_size + vec2(self.canvas_margin(half))
            self.glow_ink = vec4(0.0)
            if self.box_is_glow() {
                let size = self.live_parts.size()
                if self.live_ix.z < size.x*size.y {
                    let part = self.live_parts.sample_nearest(Live.net_uv(self.live_ix.z, size))
                    self.glow_ink = Live.part_glow(part.xy, self.live_ix.w, self.live_light)
                }
                // A dark part collapses to a point and costs no fragments.
                extent = if self.glow_ink.w > 0.0 {half_size+vec2(self.stroke.y*self.cam.z)} else {vec2(0.0)}
            }
            let local = (self.geom.pos*2.0-vec2(1.0))*extent
            let turn = vec2(cos(pose.z), sin(pose.z))
            // Screen Y down reverses the rotation matrix's sine signs.
            let sp = center+vec2(turn.x*local.x+turn.y*local.y, -turn.y*local.x+turn.x*local.y)
            self.box_center = center
            self.box_half = half_size
            self.box_radius = radius
            self.box_turn = turn
            self.half_width = half
            self.pose_alpha = pose.w
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let delta = sp-self.box_center
            let turn = self.box_turn
            let p = vec2(turn.x*delta.x-turn.y*delta.y, turn.y*delta.x+turn.x*delta.y)
            let q = abs(p)-self.box_half+vec2(self.box_radius)
            let outline = length(max(q, vec2(0.0)))+min(max(q.x, q.y), 0.0)-self.box_radius
            if self.box_is_glow() {
                let fade = self.pose_alpha*self.layer_alpha*mix(1.0, 0.25, self.pane_dim)
                if self.live_ix.w > 0.0 {
                    // An LED's light: a round bloom, a saturated core over a wide soft halo, gone
                    // at the quad's edge. Added to a dark sheet (strokes stay crisp on top), laid
                    // over a light one.
                    let t = length(p)/max(self.box_half.x+self.stroke.y*self.cam.z, 0.0001)
                    let core = exp(-t*t*24.0)
                    let u = max(1.0-t*t, 0.0)
                    let shape = 0.45*core+0.55*u*u*u
                    // A few mA already reads as lit (the peak is 0.8 x sqrt of the light output).
                    let a = clamp(self.glow_ink.w*1.8, 0.0, 1.0)*shape*fade
                    let ink = mix(self.glow_ink.xyz, vec3(1.0), 0.2*core)
                    self.fb0 = vec4(ink*a, a*self.live_light)
                    return
                }
                let a = Live.glow_alpha(self.glow_ink.w, outline/self.cam.z, self.stroke.y)*fade
                self.fb0 = vec4(self.glow_ink.xyz*a, a)
                return
            }
            let d = abs(outline)-self.half_width
            let outer = outline-self.half_width
            let along = self.box_along(p)
            let mut dot_d = 1e20
            if abs(self.look-4.0) < 0.5 {
                let perimeter = 4.0*(self.box_half.x+self.box_half.y)+(6.28318530718-8.0)*self.box_radius
                let first = fract((self.time*6.0+0.75)/4.5)*4.5
                if first <= perimeter {
                    let last = first+floor((perimeter-first)/4.5)*4.5
                    let center = clamp(first+floor((along-first)/4.5+0.5)*4.5, first, last)
                    let dot_radius = max(0.75, 0.5/max(self.cam.w, 0.0001))
                    // Include both sides of the closed perimeter's seam.
                    dot_d = min(min(length(p-self.box_point(center)), length(p-self.box_point(first))), length(p-self.box_point(last)))-dot_radius
                }
            }
            let enabled = if self.stroke.x > 0.0 {1.0} else {0.0}
            let color = self.canvas_paint(d, outer, dot_d, along, 0.0, outline, self.fill, enabled, 0.0, vec2(0.0))
            let a = color.w*self.pose_alpha*self.layer_alpha
            self.fb0 = vec4(color.xyz*a, a)
        }
    }

    mod.draw.DrawCanvasPin = mod.std.set_type_default() do #(DrawCanvasPin::script_shader(vm)) {
        ..mod.draw.DrawCanvas
        at: vec2(0.0)
        dir: vec2(1.0, 0.0)
        size: 0.22
        live_pin: -1.0
        pin_center: varying(vec2f)
        pin_turn: varying(vec2f)
        pin_radius: varying(float)

        pin_segment: fn(p: vec2, a: vec2, b: vec2) -> float {
            let d = b-a
            return length(p-a-d*clamp(dot(p-a, d)/max(dot(d, d), 0.000001), 0.0, 1.0))
        }
        vertex: fn() {
            self.canvas_live()
            let pose = self.canvas_drag(self.canvas_pose())
            let center = self.canvas_point(self.at+self.dir*self.size*1.4, pose)
            let radius = max(self.size*self.cam.z, 0.000001)
            let turn = vec2(cos(pose.z), sin(pose.z))
            let local = (self.geom.pos*2.0-vec2(1.0))*self.canvas_margin(2.2*radius)
            let sp = center+vec2(turn.x*local.x+turn.y*local.y, -turn.y*local.x+turn.x*local.y)
            self.pin_center = center
            self.pin_turn = turn
            self.pin_radius = radius
            self.pose_alpha = pose.w
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let tex_size = self.live_pins.size()
            if self.live_pin < 0.0 || self.live_pin >= tex_size.x*tex_size.y { discard() }
            let row = self.live_pins.sample_nearest(Live.net_uv(self.live_pin, tex_size))
            if row.x < 0.0 { discard() }
            let delta = sp-self.pin_center
            let turn = self.pin_turn
            // Back to the glyph's frame, Y up, including the part's baked pose.
            let p = vec2(turn.x*delta.x-turn.y*delta.y, -turn.y*delta.x-turn.x*delta.y)
            let r = self.pin_radius
            let pull_up = abs(row.x-1.0) < 0.5
            let pwm = abs(row.x-3.0) < 0.5
            let peripheral = abs(row.x-4.0) < 0.5
            let stroke = max(0.25*r, 1.0/max(self.cam.w, 0.0001))
            let half = min(r, if pull_up {stroke} else {0.5*stroke})
            let radial = if peripheral {max(abs(p.x), abs(p.y))} else {length(p)}
            let outline = radial-r
            var d = abs(radial-(r-half))-half
            let angle = atan2(p.x, p.y)
            if row.y < -0.5 {
                // Twelve round dots, with the same outline for an unknown peripheral square.
                let a = floor(angle/0.5235987756+0.5)*0.5235987756
                var dot_at = vec2(sin(a), cos(a))
                if peripheral { dot_at /= max(abs(dot_at.x), abs(dot_at.y)) }
                d = length(p-dot_at*(r-half))-half
            } else if row.y > 0.5 && !pwm {
                d = outline
            }
            if pwm && row.z > 0.0 {
                // A clockwise sector from the top; both radial edges are antialiased.
                let sweep = clamp(row.z, 0.0, 1.0)*6.28318530718
                let end = p.x*cos(sweep)-p.y*sin(sweep)
                let sector = if row.z <= 0.5 {max(-p.x, end)} else {min(-p.x, end)}
                let pie = if row.z >= 1.0 {outline} else {max(outline, sector)}
                d = min(d, pie)
            }
            var outer = outline
            if row.x < 3.5 {
                let direction = if row.x < 1.5 {-1.0} else {1.0}
                let q = vec2(dot(p, self.dir), dot(p, vec2(-self.dir.y, self.dir.x)))*direction
                let tip = vec2(1.9*r, 0.0)
                let chevron = min(self.pin_segment(q, vec2(1.3*r, 0.5*r), tip),
                    self.pin_segment(q, vec2(1.3*r, -0.5*r), tip))-min(r*0.25, half)
                d = min(d, chevron)
                outer = min(outer, chevron)
            }
            let color = self.canvas_paint(d, outer, d, angle*r, 0.0, 1e20, vec4(0.0), 1.0, 0.0, vec2(0.0))
            let ink = mix(color.xyz, self.accent_focus.xyz, 0.25*clamp(row.w, 0.0, 1.0))
            let a = color.w*self.pose_alpha*self.layer_alpha
            self.fb0 = vec4(ink*a, a)
        }
    }

    mod.draw.DrawCanvasMini = mod.std.set_type_default() do #(DrawCanvasMini::script_shader(vm)) {
        ..mod.draw.DrawCanvas
        rect: vec4(0.0)
        mini_slot: -1.0
        stroke: vec2(1.0, 0.0)
        fill: vec4(0.0)
        mini_hover: uniform(-1.0)
        mini_uv: varying(vec2f)
        mini_size: varying(vec2f)
        mini_header: varying(vec4f)
        mini_hover_header: varying(vec4f)

        mini_read_header: fn(slot: float) -> vec4 {
            let size = self.live_mini.size()
            if slot >= 0.0 && (slot+1.0)*64.0 <= size.x*size.y {
                return self.live_mini.sample_nearest(Live.net_uv(slot*64.0, size))
            }
            return vec4(1.0, -1.0, 0.0, -1.0)
        }
        mini_cover: fn(column: vec4, header: vec4) -> float {
            if column.x > column.y { return 0.0 }
            let band = vec2(0.15)+0.7*(column.xy-vec2(header.x))/max(header.y-header.x, 0.000001)
            let distance = max(max(band.x-self.mini_uv.y, self.mini_uv.y-band.y), 0.0)*self.mini_size.y
            let half = self.half_width*max(self.cam.w, 0.0001)
            return clamp(half-distance+0.5, 0.0, 1.0)
        }
        vertex: fn() {
            self.canvas_live()
            let pose = self.canvas_drag(self.canvas_pose())
            let center = self.canvas_point(self.rect.xy, pose)
            let half_size = max(self.rect.zw, vec2(0.0))*self.cam.z
            let dpi = max(self.cam.w, 0.0001)
            let half = if self.stroke.x > 0.0 {0.5*max(max(self.stroke.x, self.stroke.y*self.cam.z), 1.0/dpi)} else {0.0}
            let local = (self.geom.pos*2.0-vec2(1.0))*(half_size+vec2(self.canvas_margin(half)))
            // Pose the anchor, but keep the strip and its time/voltage axes screen-aligned.
            let sp = center+local
            self.mini_uv = vec2(0.5)+vec2(local.x, -local.y)/max(2.0*half_size, vec2(0.000001))
            self.mini_size = 2.0*half_size*dpi
            self.mini_header = self.mini_read_header(self.mini_slot)
            self.mini_hover_header = self.mini_read_header(self.mini_hover)
            self.half_width = half
            self.pose_alpha = pose.w
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let dpi = max(self.cam.w, 0.0001)
            let half_size = self.mini_size*0.5
            let radius = min(2.0*dpi, min(half_size.x, half_size.y))
            let q = abs(self.mini_uv-vec2(0.5))*self.mini_size-half_size+vec2(radius)
            let outline = (length(max(q, vec2(0.0)))+min(max(q.x, q.y), 0.0)-radius)/dpi
            let mut result = self.canvas_over(vec4(0.0), self.fill, 1.0)
            let header = self.mini_header
            if self.mini_slot >= 0.0 && header.x <= header.y && self.stroke.x > 0.0 {
                let c = floor(48.0*clamp(self.mini_uv.x, 0.0, 1.0)-48.0+header.w)
                let ring = header.z+c+126.0
                let p = ring-floor(ring/63.0)*63.0
                let size = self.live_mini.size()
                let hovered = self.mini_hover_header
                if self.mini_hover >= 0.0 && abs(self.mini_hover-self.mini_slot) >= 0.5 && hovered.x <= hovered.y {
                    // Slots share the column clock: compare the same ring position on its own scale.
                    let column = self.live_mini.sample_nearest(Live.net_uv(self.mini_hover*64.0+1.0+p, size))
                    let shadow = vec4(mix(self.ink.xyz, self.fill.xyz, 0.55), 0.5)
                    result = self.canvas_over(result, shadow, self.mini_cover(column, hovered))
                }
                let column = self.live_mini.sample_nearest(Live.net_uv(self.mini_slot*64.0+1.0+p, size))
                let tint = if column.w >= 0.0 {Live.volt(column.w, self.live_light)} else {self.ink.xyz}
                let trace = vec4(Live.lit(tint, 0.5*self.live_net.w, self.live_light), self.ink.w)
                // The column's span (lo..hi) faint and its time-weighted mean as the line: a signal
                // switching faster than a column (PWM) reads as its duty level, a slow one as its line.
                result = self.canvas_over(result, vec4(trace.xyz, 0.35*trace.w), self.mini_cover(column, header))
                if column.x <= column.y {
                    result = self.canvas_over(result, trace, self.mini_cover(vec4(column.z, column.z, column.z, column.w), header))
                }
            }
            // Paint the composite as a fill, applying the common state fade and rounded AA once.
            let fill = vec4(result.xyz/max(result.w, 0.000001), result.w)
            let color = self.canvas_paint(1e20, outline, 1e20, 0.0, 0.0, outline, fill, 0.0, 0.0, vec2(0.0))
            let a = color.w*self.pose_alpha*self.layer_alpha
            self.fb0 = vec4(color.xyz*a, a)
        }
    }

    mod.draw.DrawCanvasArc = mod.std.set_type_default() do #(DrawCanvasArc::script_shader(vm)) {
        ..mod.draw.DrawCanvas
        circle: vec4(0.0)
        sweep: vec2(0.0, 6.28318530718)
        along0: 0.0
        stroke: vec2(1.0, 0.0)
        fill: vec4(0.0)
        arc_center: varying(vec2f)
        arc_radius: varying(float)
        arc_sweep: varying(vec2f)

        arc_dot: fn(p: vec2, along: float) -> float {
            let extent = min(abs(self.arc_sweep.y), 6.28318530718)
            if abs(self.arc_sweep.y) < 6.28318530718 && (along < 0.0 || along > extent*self.arc_radius) { return 1e20 }
            let direction = if self.arc_sweep.y < 0.0 {-1.0} else {1.0}
            let a = self.arc_sweep.x+direction*along/max(self.arc_radius, 0.000001)
            let center = self.arc_radius*vec2(cos(a), sin(a))
            return length(p-center)-max(0.75, 0.5/max(self.cam.w, 0.0001))
        }
        vertex: fn() {
            self.canvas_live()
            let pose = self.canvas_drag(self.canvas_pose())
            let center = self.canvas_point(self.circle.xy, pose)
            let radius = max(self.circle.z, 0.0)*self.cam.z
            let tau = 6.28318530718
            let start = fract((self.sweep.x+pose.z)/tau)*tau
            let direction = if self.sweep.y < 0.0 {-1.0} else {1.0}
            let extent = min(abs(self.sweep.y), tau)
            let end = start+direction*extent
            let a = radius*vec2(cos(start), sin(start))
            let b = radius*vec2(cos(end), sin(end))
            let mut lo = min(a, b)
            let mut hi = max(a, b)
            // Endpoints plus the included cardinal extrema give a tight
            // screen bounding box, including clockwise and reflex arcs.
            if abs(self.sweep.y) >= tau {
                lo = vec2(-radius)
                hi = vec2(radius)
            } else {
                if fract((0.0-start)*direction/tau)*tau <= extent { hi.x = radius }
                if fract((1.57079632679-start)*direction/tau)*tau <= extent { hi.y = radius }
                if fract((3.14159265359-start)*direction/tau)*tau <= extent { lo.x = -radius }
                if fract((4.71238898038-start)*direction/tau)*tau <= extent { lo.y = -radius }
            }
            let px = 1.0/max(self.cam.w, 0.0001)
            let half = if self.stroke.x > 0.0 {0.5*max(max(self.stroke.x, self.stroke.y*self.cam.z), px)} else {0.0}
            let margin = self.canvas_margin(half)
            let lower = center+vec2(lo.x, -hi.y)-vec2(margin)
            let upper = center+vec2(hi.x, -lo.y)+vec2(margin)
            let sp = mix(lower, upper, self.geom.pos)
            self.arc_center = center
            self.arc_radius = radius
            self.arc_sweep = vec2(start, self.sweep.y)
            self.half_width = half
            self.pose_alpha = pose.w
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let p = vec2(sp.x-self.arc_center.x, self.arc_center.y-sp.y)
            let tau = 6.28318530718
            let direction = if self.arc_sweep.y < 0.0 {-1.0} else {1.0}
            let extent = min(abs(self.arc_sweep.y), tau)
            let full = abs(self.arc_sweep.y) >= tau
            let angle = if length(p) > 0.000001 {atan2(p.y, p.x)} else {self.arc_sweep.x}
            let relative = fract((angle-self.arc_sweep.x)*direction/tau)*tau
            let end = self.arc_sweep.x+direction*extent
            let start_point = self.arc_radius*vec2(cos(self.arc_sweep.x), sin(self.arc_sweep.x))
            let end_point = self.arc_radius*vec2(cos(end), sin(end))
            let da = length(p-start_point)
            let db = length(p-end_point)
            let radial = length(p)-self.arc_radius
            let on_arc = full || relative <= extent
            let distance = if on_arc {abs(radial)} else {min(da, db)}
            let nearest = if on_arc {relative} else if da <= db {0.0} else {extent}
            let along = nearest*self.arc_radius
            let d = distance-self.half_width
            let outer = if full {radial-self.half_width} else {d}
            let mut dot_d = 1e20
            if abs(self.look-4.0) < 0.5 {
                let len = extent*self.arc_radius
                let first = fract((self.time*6.0+0.75)/4.5)*4.5
                if first <= len {
                    let last = first+floor((len-first)/4.5)*4.5
                    let center = clamp(first+floor((along-first)/4.5+0.5)*4.5, first, last)
                    dot_d = min(min(self.arc_dot(p, center), self.arc_dot(p, first)), self.arc_dot(p, last))
                }
            }
            let enabled = if self.stroke.x > 0.0 {1.0} else {0.0}
            let fill = if full {self.fill} else {vec4(0.0)}
            let color = self.canvas_paint(d, outer, dot_d, along, self.along0, radial, fill, enabled, 1.0, vec2(0.0))
            let a = color.w*self.pose_alpha*self.layer_alpha
            self.fb0 = vec4(color.xyz*a, a)
        }
    }

    mod.draw.DrawCanvasBridge = mod.std.set_type_default() do #(DrawCanvasBridge::script_shader(vm)) {
        ..mod.draw.DrawCanvas
        ends: vec4(0.0)
        b_pose: vec4(0.0)
        b_span: vec2(0.0)
        bend: 0.25
        stroke: vec2(1.0, 0.0)
        fade: vec4(0.0, 0.0, 1e9, 0.0)
        arc_center: varying(vec2f)
        arc_radius: varying(float)
        arc_sweep: varying(vec2f)
        bridge_a: varying(vec2f)
        bridge_b: varying(vec2f)
        bridge_straight: varying(float)

        arc_dot: fn(p: vec2, along: float) -> float {
            let extent = min(abs(self.arc_sweep.y), 6.28318530718)
            if abs(self.arc_sweep.y) < 6.28318530718 && (along < 0.0 || along > extent*self.arc_radius) { return 1e20 }
            let direction = if self.arc_sweep.y < 0.0 {-1.0} else {1.0}
            let a = self.arc_sweep.x+direction*along/max(self.arc_radius, 0.000001)
            let center = self.arc_radius*vec2(cos(a), sin(a))
            return length(p-center)-max(0.75, 0.5/max(self.cam.w, 0.0001))
        }
        vertex: fn() {
            self.canvas_live()
            let pose = self.canvas_drag(self.canvas_pose())
            let t = if self.b_span.y <= 0.0 {1.0} else {clamp((self.motion_t-self.b_span.x)/self.b_span.y, 0.0, 1.0)}
            let u = 1.0-t
            let b_offset = mix(self.b_pose.xy, self.b_pose.zw, 1.0-u*u*u)
            // Each pin has its own translation and clock. Pose angles do not rotate the chord.
            let a = self.canvas_point(self.ends.xy, vec4(pose.xy, 0.0, 1.0))
            let b = self.canvas_point(self.ends.zw, self.canvas_drag(vec4(b_offset, 0.0, 1.0)))
            let chord = vec2(b.x-a.x, a.y-b.y)
            let len = length(chord)
            let sagitta = self.bend*len
            let straight = len <= 0.000001 || abs(sagitta) < 0.001*len
            let mut center = (a+b)*0.5
            let mut radius = 0.0
            let mut start = 0.0
            let mut sweep = 0.0
            let mut lower = min(a, b)
            let mut upper = max(a, b)
            if !straight {
                let normal = vec2(-chord.y, chord.x)/len
                radius = (0.25*len*len+sagitta*sagitta)/(2.0*abs(sagitta))
                let side = if sagitta < 0.0 {-1.0} else {1.0}
                center += vec2(normal.x, -normal.y)*(sagitta-side*radius)
                let tau = 6.28318530718
                start = fract(atan2(center.y-a.y, a.x-center.x)/tau)*tau
                // Four times atan(sagitta / half-chord) passes through the apex,
                // including reflex arcs when |bend| > 0.5. Left bulges turn clockwise.
                let direction = -side
                let extent = 4.0*atan2(2.0*abs(self.bend), 1.0)
                sweep = direction*extent
                let end = start+sweep
                let pa = radius*vec2(cos(start), sin(start))
                let pb = radius*vec2(cos(end), sin(end))
                let mut lo = min(pa, pb)
                let mut hi = max(pa, pb)
                // Arc's endpoints and included cardinal extrema, in screen-scaled Y-up space.
                if extent >= tau {
                    lo = vec2(-radius)
                    hi = vec2(radius)
                } else {
                    if fract((0.0-start)*direction/tau)*tau <= extent { hi.x = radius }
                    if fract((1.57079632679-start)*direction/tau)*tau <= extent { hi.y = radius }
                    if fract((3.14159265359-start)*direction/tau)*tau <= extent { lo.x = -radius }
                    if fract((4.71238898038-start)*direction/tau)*tau <= extent { lo.y = -radius }
                }
                lower = center+vec2(lo.x, -hi.y)
                upper = center+vec2(hi.x, -lo.y)
            }
            let px = 1.0/max(self.cam.w, 0.0001)
            let half = if self.stroke.x > 0.0 {0.5*max(max(self.stroke.x, self.stroke.y*self.cam.z), px)} else {0.0}
            let margin = self.canvas_margin(half)
            let sp = mix(lower-vec2(margin), upper+vec2(margin), self.geom.pos)
            let ramp_in = if self.fade.y <= 0.0 {step(self.fade.x, self.motion_t)} else {clamp((self.motion_t-self.fade.x)/self.fade.y, 0.0, 1.0)}
            let ramp_out = if self.fade.w <= 0.0 {step(self.fade.z, self.motion_t)} else {clamp((self.motion_t-self.fade.z)/self.fade.w, 0.0, 1.0)}
            self.arc_center = center
            self.arc_radius = radius
            self.arc_sweep = vec2(start, sweep)
            self.bridge_a = a
            self.bridge_b = b
            self.bridge_straight = if straight {1.0} else {0.0}
            self.half_width = half
            self.pose_alpha = pose.w*ramp_in*(1.0-ramp_out)
            self.screen = sp
            self.world = vec4(sp.x, sp.y, self.draw_depth+self.draw_call.zbias, 1.0)
            self.vertex_pos = self.draw_pass.camera_projection*(self.draw_pass.camera_view*self.world)
        }
        fragment: fn() {
            let sp = self.screen
            if sp.x < self.clip.x || sp.y < self.clip.y || sp.x > self.clip.z || sp.y > self.clip.w { discard() }
            let mut d = 0.0
            let mut along = 0.0
            let mut dot_d = 1e20
            if self.bridge_straight > 0.5 {
                let delta = self.bridge_b-self.bridge_a
                let len = length(delta)
                let dir = if len > 0.000001 {delta/len} else {vec2(1.0, 0.0)}
                let p = sp-self.bridge_a
                along = dot(p, dir)
                let across = dot(p, vec2(-dir.y, dir.x))
                let dx = max(max(-along, along-len), 0.0)
                d = length(vec2(dx, across))-self.half_width
                if abs(self.look-4.0) < 0.5 {
                    let first = fract((self.time*6.0+0.75)/4.5)*4.5
                    if first <= len {
                        let last = first+floor((len-first)/4.5)*4.5
                        let center = clamp(first+floor((along-first)/4.5+0.5)*4.5, first, last)
                        dot_d = length(vec2(along-center, across))-max(0.75, 0.5/max(self.cam.w, 0.0001))
                    }
                }
            } else {
                // The same round-capped arc SDF and stale dots as DrawCanvasArc, without fill.
                let p = vec2(sp.x-self.arc_center.x, self.arc_center.y-sp.y)
                let tau = 6.28318530718
                let direction = if self.arc_sweep.y < 0.0 {-1.0} else {1.0}
                let extent = min(abs(self.arc_sweep.y), tau)
                let angle = if length(p) > 0.000001 {atan2(p.y, p.x)} else {self.arc_sweep.x}
                let relative = fract((angle-self.arc_sweep.x)*direction/tau)*tau
                let end = self.arc_sweep.x+direction*extent
                let start_point = self.arc_radius*vec2(cos(self.arc_sweep.x), sin(self.arc_sweep.x))
                let end_point = self.arc_radius*vec2(cos(end), sin(end))
                let da = length(p-start_point)
                let db = length(p-end_point)
                let radial = length(p)-self.arc_radius
                let on_arc = relative <= extent
                let distance = if on_arc {abs(radial)} else {min(da, db)}
                let nearest = if on_arc {relative} else if da <= db {0.0} else {extent}
                along = nearest*self.arc_radius
                d = distance-self.half_width
                if abs(self.look-4.0) < 0.5 {
                    let len = extent*self.arc_radius
                    let first = fract((self.time*6.0+0.75)/4.5)*4.5
                    if first <= len {
                        let last = first+floor((len-first)/4.5)*4.5
                        let center = clamp(first+floor((along-first)/4.5+0.5)*4.5, first, last)
                        dot_d = min(min(self.arc_dot(p, center), self.arc_dot(p, first)), self.arc_dot(p, last))
                    }
                }
            }
            let enabled = if self.stroke.x > 0.0 {1.0} else {0.0}
            let color = self.canvas_paint(d, d, dot_d, along, 0.0, 1e20, vec4(0.0), enabled, 1.0, vec2(0.0))
            let a = color.w*self.pose_alpha*self.layer_alpha
            self.fb0 = vec4(color.xyz*a, a)
        }
    }
}

/// The sheet background and its snap grid, drawn in one quad at every zoom.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvasGrid {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    canvas: Vec4f,
    #[live]
    grid: Vec4f,
    #[live]
    grid_major: Vec4f,
    #[live]
    top_left: Vec2f,
    #[live]
    px_per_unit: f32,
}

impl DrawCanvasGrid {
    /// Draws a plain canvas until the sheet has a camera. CPU work is constant per frame.
    pub fn draw_grid(&mut self, cx: &mut Cx2d, rect: Rect, cam: Option<&Camera2d>, tokens: &Tokens) {
        let rgba = |[r, g, b, a]: [f32; 4]| vec4(r, g, b, a);
        self.canvas = rgba(tokens.canvas);
        self.grid = rgba(tokens.grid);
        self.grid_major = rgba(tokens.grid_major);
        self.px_per_unit = 0.0;
        if let Some(cam) = cam {
            let [x, y] = cam.to_world(rect.pos);
            // The pattern repeats every ten units. Wrap before narrowing a distant origin to f32.
            self.top_left = vec2(x.rem_euclid(10.0) as f32, y.rem_euclid(10.0) as f32);
            self.px_per_unit = cam.px_per_unit as f32;
        }
        self.draw_abs(cx, rect);
    }
}

/// Shared canvas pose, appearance, uniforms, and stroke painting.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvas {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pose_a: Vec4f,
    #[live]
    pose_b: Vec4f,
    #[live]
    span: Vec2f,
    #[live]
    item: f32,
    #[live]
    look: f32,
    #[live]
    ink: Vec4f,
    #[live]
    live_ix: Vec4f,
}

/// A segment shader with screen-space round/butt caps, dashes, and live flow.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvasLine {
    #[deref]
    pub draw_super: DrawCanvas,
    #[live]
    seg: Vec4f,
    #[live]
    along0: f32,
    #[live]
    width: Vec2f,
    #[live]
    dash: Vec2f,
    #[live]
    cap: f32,
    #[live]
    follow: Vec2f,
    #[live]
    press: Vec4f,
}

/// A rotated rounded rectangle with independent fill and constant-screen-width stroke.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvasBox {
    #[deref]
    pub draw_super: DrawCanvas,
    #[live]
    rect: Vec4f,
    #[live]
    corner: f32,
    #[live]
    stroke: Vec2f,
    #[live]
    fill: Vec4f,
}

/// A pin-end firmware mark read directly from the host's pin texture.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvasPin {
    /// Shared pose, live rows, appearance, and canvas draw state.
    #[deref]
    pub draw_super: DrawCanvas,
    #[live]
    at: Vec2f,
    #[live]
    dir: Vec2f,
    #[live]
    size: f32,
    #[live]
    live_pin: f32,
}

/// An axis-aligned rolling voltage strip at a posed anchor, with a rounded background.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvasMini {
    /// Shared pose, live rows, appearance, and canvas draw state.
    #[deref]
    pub draw_super: DrawCanvas,
    #[live]
    rect: Vec4f,
    #[live]
    mini_slot: f32,
    #[live]
    stroke: Vec2f,
    #[live]
    fill: Vec4f,
}

/// A circular arc with round ends, or a filled/stroked full circle.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvasArc {
    #[deref]
    pub draw_super: DrawCanvas,
    #[live]
    circle: Vec4f,
    #[live]
    sweep: Vec2f,
    #[live]
    along0: f32,
    #[live]
    stroke: Vec2f,
    #[live]
    fill: Vec4f,
}

/// A travel arc whose two pins have independent translation clocks and linear in/out fades.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCanvasBridge {
    #[deref]
    pub draw_super: DrawCanvas,
    #[live]
    ends: Vec4f,
    #[live]
    b_pose: Vec4f,
    #[live]
    b_span: Vec2f,
    #[live]
    bend: f32,
    #[live]
    stroke: Vec2f,
    #[live]
    fade: Vec4f,
}

/// The common f32 instance fields; Default is all zero, while `at` makes visible rest poses.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Common {
    /// Transition start: world x/y, CCW angle in radians, and alpha.
    pub pose_a: [f32; 4],
    /// Transition end: world x/y, CCW angle in radians, and alpha.
    pub pose_b: [f32; 4],
    /// Motion-clock start and duration in seconds; duration <= 0 selects pose_b.
    pub span: [f32; 2],
    /// An exact integer hover and item-state index below 2^24, or -1 for none.
    pub item: f32,
    /// The `looks::Look` code, 0..=7.
    pub look: f32,
    /// Straight-alpha stroke RGBA.
    pub ink: [f32; 4],
    /// Static live indices, -1 for none: x net row, y wire segment row, z part row; w an LED's
    /// own colour ([`super::emitter::emitter_ix`], 0 none), which its glow takes.
    /// Live values come from the host's row textures ([`CanvasLive`]), never from instances.
    pub live_ix: [f32; 4],
}

impl Common {
    /// At rest at (x, y, CCW angle), with alpha 1, item -1,
    /// no live indices, and the given straight-alpha ink.
    pub fn at(pose: [f32; 3], ink: [f32; 4]) -> Self {
        let pose = [pose[0], pose[1], pose[2], 1.0];
        Self {
            pose_a: pose,
            pose_b: pose,
            item: -1.0,
            ink,
            live_ix: [-1.0, -1.0, -1.0, 0.0],
            ..Self::default()
        }
    }
}

/// One item-local segment, written without conversion by `LineTemplate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineInst {
    /// Pose, identity, state, and ink shared by the canvas shaders.
    pub c: Common,
    /// Local endpoints (ax, ay, bx, by), in world units.
    pub seg: [f32; 4],
    /// world arc length at the start along its wire (flow dashes run on through corners)
    pub along0: f32,
    /// Width in screen points and its world-unit floor; at least one device pixel.
    pub width: [f32; 2],
    /// On/off lengths in screen points; on = 0 means solid.
    pub dash: [f32; 2],
    /// Cap code: 0 round, 1 butt.
    pub cap: f32,
    /// Endpoint weights for the world-space drag offset, independent of item identity.
    pub follow: [f32; 2],
    /// Item-local offsets of its ends (ax, ay, bx, by) while pane-state y holds its item pressed:
    /// a button's actuator; zero for everything else.
    pub press: [f32; 4],
}

/// One item-local rounded rectangle, written by `BoxTemplate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoxInst {
    /// Pose, identity, state, and ink shared by the canvas shaders.
    pub c: Common,
    /// Local centre and half sizes (cx, cy, half_w, half_h), in world units.
    pub rect: [f32; 4],
    /// World corner radius, clamped to the half sizes.
    pub corner: f32,
    /// Stroke points and world-unit floor; x = 0 disables the stroke.
    pub stroke: [f32; 2],
    /// Straight-alpha fill RGBA; alpha = 0 disables the fill.
    pub fill: [f32; 4],
}

impl BoxInst {
    /// One part's glow-only rectangle. The shader samples its live row once per vertex.
    /// The rectangle holds the part centre and half sizes. Its margin is in the same world units.
    pub fn glow(mut c: Common, rect: [f32; 4], margin: f32, part: f32, emitter: f32) -> Self {
        c.live_ix = [-1.0, -1.0, part, emitter];
        Self { c, rect, stroke: [0.0, margin], ..Self::default() }
    }
}

/// One pin-end firmware mark, written once per scene build by `PinTemplate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct PinInst {
    /// The pin stub's pose, identity, state, and ink.
    pub c: Common,
    /// Outer pin end, in the same frame as the part's glyphs.
    pub at: [f32; 2],
    /// Unit vector pointing away from the body in that frame.
    pub dir: [f32; 2],
    /// Mark half size in world units.
    pub size: f32,
    /// Static `ValueIndex::pin` row, or -1 for none.
    pub live_pin: f32,
}

/// One item-local rolling voltage strip, written by `MiniTemplate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MiniInst {
    /// Pose, identity, state, and ink shared by the canvas shaders.
    pub c: Common,
    /// Local centre and half sizes (cx, cy, half_w, half_h), in world units.
    /// The pose transforms the centre; the strip's time/voltage axes stay screen-aligned.
    pub rect: [f32; 4],
    /// Slot in the mini texture (64 texels each), or -1 for none.
    pub slot: f32,
    /// Stroke points and world-unit floor; at least one device pixel, or absent when x = 0.
    pub stroke: [f32; 2],
    /// Straight-alpha background RGBA; alpha = 0 disables the background.
    pub fill: [f32; 4],
}

/// One item-local arc or full disc, written by `ArcTemplate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ArcInst {
    /// Pose, identity, state, and ink shared by the canvas shaders.
    pub c: Common,
    /// Local centre and radius (cx, cy, r, unused), in world units.
    pub circle: [f32; 4],
    /// Start and signed CCW sweep in radians; |sweep| >= 2*pi is full.
    pub sweep: [f32; 2],
    /// world arc length at the start along its wire (flow dashes run on through corners)
    pub along0: f32,
    /// Stroke points and world-unit floor; x = 0 disables the stroke.
    pub stroke: [f32; 2],
    /// Straight-alpha fill RGBA, used only for full circles.
    pub fill: [f32; 4],
}

/// One world-space travel arc, written by `BridgeTemplate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BridgeInst {
    /// End A's start/end xy offsets and alpha, clock, identity, state, and ink; angles are ignored.
    pub c: Common,
    /// Landed endpoints (ax, ay, bx, by), in world units, before each end's pose offsets.
    pub ends: [f32; 4],
    /// End B's start xy and end xy offsets, in world units.
    pub b_pose: [f32; 4],
    /// End B's clock start and duration; duration <= 0 selects its end offset.
    pub b_span: [f32; 2],
    /// Signed sagitta as a fraction of chord length; positive bulges left of A -> B.
    pub bend: f32,
    /// Stroke points and world-unit floor, as on Arc; x = 0 disables the stroke.
    pub stroke: [f32; 2],
    /// In start/duration, out start/duration on the motion clock. Linear ramps; duration <= 0 steps at its start.
    pub fade: [f32; 4],
}

/// A compiled shader's default f32 row and named offsets, captured on the UI
/// thread. Its owned plain data is Clone + Send + Sync for worker row writers.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    data: Vec<f32>,
    offs: Vec<usize>,
}

impl Template {
    /// Capture the default row and each named field's offset in `fields` order.
    /// Check the stride, f32 lane format, byte offsets, slot counts, and bounds;
    /// log and return None on any mismatch or if the shader is not compiled.
    pub fn capture(cx: &Cx, vars: &DrawVars, fields: &[(LiveId, usize)]) -> Option<Template> {
        let Some(shader) = vars.draw_shader_id else {
            log!("fab.canvas: the instance shader is not compiled");
            return None;
        };
        let mapping = &cx.draw_shaders[shader.index].mapping.instances;
        let data = vars.as_slice().to_vec();
        if data.len() != mapping.total_slots || mapping.stride_bytes != data.len() * 4 {
            log!(
                "fab.canvas: instance layout {} slots, mapping {} slots / {} bytes",
                data.len(), mapping.total_slots, mapping.stride_bytes
            );
            return None;
        }
        let mut offs = Vec::with_capacity(fields.len());
        for &(id, slots) in fields {
            let Some(input) = mapping.inputs.iter().find(|input| input.id == id) else {
                log!("fab.canvas: missing instance input {:?}", id);
                return None;
            };
            if input.slots != slots
                || !input.attr_format.is_f32_lane()
                || input.byte_offset != input.offset * 4
                || input.offset.checked_add(slots).is_none_or(|end| end > data.len())
            {
                log!(
                    "fab.canvas: instance input {:?} needs {} f32 lanes; got {} at slot {} / byte {}",
                    id, slots, input.slots, input.offset, input.byte_offset
                );
                return None;
            }
            offs.push(input.offset);
        }
        Some(Self { data, offs })
    }

    /// Number of f32 slots in a complete row, including the DrawQuad base.
    pub fn slots(&self) -> usize {
        self.data.len()
    }

    fn append_common<'a>(&self, out: &'a mut Vec<f32>, c: &Common) -> &'a mut [f32] {
        let start = out.len();
        out.extend_from_slice(&self.data);
        let row = &mut out[start..];
        let values: [&[f32]; 7] = [
            &c.pose_a,
            &c.pose_b,
            &c.span,
            std::slice::from_ref(&c.item),
            std::slice::from_ref(&c.look),
            &c.ink,
            &c.live_ix,
        ];
        for (&offset, value) in self.offs.iter().zip(values) {
            row[offset..offset + value.len()].copy_from_slice(value);
        }
        row
    }

    fn write(&self, row: &mut [f32], field: usize, value: &[f32]) {
        let offset = self.offs[field];
        row[offset..offset + value.len()].copy_from_slice(value);
    }
}

/// A worker-cloneable instance writer for `DrawCanvasLine`.
#[derive(Clone, Debug, PartialEq)]
pub struct LineTemplate(Template);

impl LineTemplate {
    /// Capture and validate the compiled shader's common and shape fields.
    pub fn capture(cx: &Cx, d: &DrawCanvasLine) -> Option<Self> {
        let fields = [
            (live_id!(pose_a), 4),
            (live_id!(pose_b), 4),
            (live_id!(span), 2),
            (live_id!(item), 1),
            (live_id!(look), 1),
            (live_id!(ink), 4),
            (live_id!(live_ix), 4),
            (live_id!(seg), 4),
            (live_id!(along0), 1),
            (live_id!(width), 2),
            (live_id!(dash), 2),
            (live_id!(cap), 1),
            (live_id!(follow), 2),
            (live_id!(press), 4),
        ];
        Template::capture(cx, &d.draw_vars, &fields).map(Self)
    }

    /// Append exactly one complete row, preserving the captured DrawQuad defaults.
    pub fn append(&self, out: &mut Vec<f32>, i: &LineInst) {
        let row = self.0.append_common(out, &i.c);
        self.0.write(row, 7, &i.seg);
        self.0.write(row, 8, std::slice::from_ref(&i.along0));
        self.0.write(row, 9, &i.width);
        self.0.write(row, 10, &i.dash);
        self.0.write(row, 11, std::slice::from_ref(&i.cap));
        self.0.write(row, 12, &i.follow);
        self.0.write(row, 13, &i.press);
    }

    /// Number of f32 slots per row, including the DrawQuad base.
    pub fn slots(&self) -> usize {
        self.0.slots()
    }
}

/// A worker-cloneable instance writer for `DrawCanvasBox`.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxTemplate(Template);

impl BoxTemplate {
    /// Capture and validate the compiled shader's common and shape fields.
    pub fn capture(cx: &Cx, d: &DrawCanvasBox) -> Option<Self> {
        let fields = [
            (live_id!(pose_a), 4),
            (live_id!(pose_b), 4),
            (live_id!(span), 2),
            (live_id!(item), 1),
            (live_id!(look), 1),
            (live_id!(ink), 4),
            (live_id!(live_ix), 4),
            (live_id!(rect), 4),
            (live_id!(corner), 1),
            (live_id!(stroke), 2),
            (live_id!(fill), 4),
        ];
        Template::capture(cx, &d.draw_vars, &fields).map(Self)
    }

    /// Append exactly one complete row, preserving the captured DrawQuad defaults.
    pub fn append(&self, out: &mut Vec<f32>, i: &BoxInst) {
        let row = self.0.append_common(out, &i.c);
        self.0.write(row, 7, &i.rect);
        self.0.write(row, 8, std::slice::from_ref(&i.corner));
        self.0.write(row, 9, &i.stroke);
        self.0.write(row, 10, &i.fill);
    }

    /// Number of f32 slots per row, including the DrawQuad base.
    pub fn slots(&self) -> usize {
        self.0.slots()
    }
}

/// A worker-cloneable instance writer for `DrawCanvasPin`.
#[derive(Clone, Debug, PartialEq)]
pub struct PinTemplate(Template);

impl PinTemplate {
    /// Capture and validate the compiled shader's common and shape fields.
    pub fn capture(cx: &Cx, d: &DrawCanvasPin) -> Option<Self> {
        let fields = [
            (live_id!(pose_a), 4),
            (live_id!(pose_b), 4),
            (live_id!(span), 2),
            (live_id!(item), 1),
            (live_id!(look), 1),
            (live_id!(ink), 4),
            (live_id!(live_ix), 4),
            (live_id!(at), 2),
            (live_id!(dir), 2),
            (live_id!(size), 1),
            (live_id!(live_pin), 1),
        ];
        Template::capture(cx, &d.draw_vars, &fields).map(Self)
    }

    /// Append exactly one complete row, preserving the captured DrawQuad defaults.
    pub fn append(&self, out: &mut Vec<f32>, i: &PinInst) {
        let row = self.0.append_common(out, &i.c);
        self.0.write(row, 7, &i.at);
        self.0.write(row, 8, &i.dir);
        self.0.write(row, 9, std::slice::from_ref(&i.size));
        self.0.write(row, 10, std::slice::from_ref(&i.live_pin));
    }

    /// Number of f32 slots per row, including the DrawQuad base.
    pub fn slots(&self) -> usize {
        self.0.slots()
    }
}

/// A worker-cloneable instance writer for `DrawCanvasMini`.
#[derive(Clone, Debug, PartialEq)]
pub struct MiniTemplate(Template);

impl MiniTemplate {
    /// Capture and validate the compiled shader's common and shape fields.
    pub fn capture(cx: &Cx, d: &DrawCanvasMini) -> Option<Self> {
        let fields = [
            (live_id!(pose_a), 4),
            (live_id!(pose_b), 4),
            (live_id!(span), 2),
            (live_id!(item), 1),
            (live_id!(look), 1),
            (live_id!(ink), 4),
            (live_id!(live_ix), 4),
            (live_id!(rect), 4),
            (live_id!(mini_slot), 1),
            (live_id!(stroke), 2),
            (live_id!(fill), 4),
        ];
        Template::capture(cx, &d.draw_vars, &fields).map(Self)
    }

    /// Append exactly one complete row, preserving the captured DrawQuad defaults.
    pub fn append(&self, out: &mut Vec<f32>, i: &MiniInst) {
        let row = self.0.append_common(out, &i.c);
        self.0.write(row, 7, &i.rect);
        self.0.write(row, 8, std::slice::from_ref(&i.slot));
        self.0.write(row, 9, &i.stroke);
        self.0.write(row, 10, &i.fill);
    }

    /// Number of f32 slots per row, including the DrawQuad base.
    pub fn slots(&self) -> usize {
        self.0.slots()
    }
}

/// A worker-cloneable instance writer for `DrawCanvasArc`.
#[derive(Clone, Debug, PartialEq)]
pub struct ArcTemplate(Template);

impl ArcTemplate {
    /// Capture and validate the compiled shader's common and shape fields.
    pub fn capture(cx: &Cx, d: &DrawCanvasArc) -> Option<Self> {
        let fields = [
            (live_id!(pose_a), 4),
            (live_id!(pose_b), 4),
            (live_id!(span), 2),
            (live_id!(item), 1),
            (live_id!(look), 1),
            (live_id!(ink), 4),
            (live_id!(live_ix), 4),
            (live_id!(circle), 4),
            (live_id!(sweep), 2),
            (live_id!(along0), 1),
            (live_id!(stroke), 2),
            (live_id!(fill), 4),
        ];
        Template::capture(cx, &d.draw_vars, &fields).map(Self)
    }

    /// Append exactly one complete row, preserving the captured DrawQuad defaults.
    pub fn append(&self, out: &mut Vec<f32>, i: &ArcInst) {
        let row = self.0.append_common(out, &i.c);
        self.0.write(row, 7, &i.circle);
        self.0.write(row, 8, &i.sweep);
        self.0.write(row, 9, std::slice::from_ref(&i.along0));
        self.0.write(row, 10, &i.stroke);
        self.0.write(row, 11, &i.fill);
    }

    /// Number of f32 slots per row, including the DrawQuad base.
    pub fn slots(&self) -> usize {
        self.0.slots()
    }
}

/// A worker-cloneable instance writer for `DrawCanvasBridge`.
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeTemplate(Template);

impl BridgeTemplate {
    /// Capture and validate the compiled shader's common and shape fields.
    pub fn capture(cx: &Cx, d: &DrawCanvasBridge) -> Option<Self> {
        let fields = [
            (live_id!(pose_a), 4),
            (live_id!(pose_b), 4),
            (live_id!(span), 2),
            (live_id!(item), 1),
            (live_id!(look), 1),
            (live_id!(ink), 4),
            (live_id!(live_ix), 4),
            (live_id!(ends), 4),
            (live_id!(b_pose), 4),
            (live_id!(b_span), 2),
            (live_id!(bend), 1),
            (live_id!(stroke), 2),
            (live_id!(fade), 4),
        ];
        Template::capture(cx, &d.draw_vars, &fields).map(Self)
    }

    /// Append exactly one complete row, preserving the captured DrawQuad defaults.
    pub fn append(&self, out: &mut Vec<f32>, i: &BridgeInst) {
        let row = self.0.append_common(out, &i.c);
        self.0.write(row, 7, &i.ends);
        self.0.write(row, 8, &i.b_pose);
        self.0.write(row, 9, &i.b_span);
        self.0.write(row, 10, std::slice::from_ref(&i.bend));
        self.0.write(row, 11, &i.stroke);
        self.0.write(row, 12, &i.fade);
    }

    /// Number of f32 slots per row, including the DrawQuad base.
    pub fn slots(&self) -> usize {
        self.0.slots()
    }
}

/// One shader's instances as one retained publication in its own DrawList2d.
/// Instances upload when replaced; a frame only sets uniforms, so camera
/// moves and transitions cost O(1) per layer.
#[derive(Default)]
pub struct CanvasLayer {
    publication: Option<RetainedInstances>,
    list: Option<DrawList2d>,
    dirty: bool,
    count: usize,
    slots: usize,
    /// The live and state textures the list was recorded with; a replacement re-records it.
    bound: [Option<TextureId>; 7],
}

/// The row textures bound to every canvas draw: nets, wire segments and parts in slots 0..2,
/// host item states in slot 3, pane states in slot 4, mini traces in slot 5 and pins in slot 6. Textures are shared
/// handles: the host updates their contents in place once a frame, and a layer re-records its
/// list only when a texture is replaced (a row set grew).
#[derive(Clone, Debug)]
pub struct CanvasLive {
    pub nets: Texture,
    pub segs: Texture,
    pub parts: Texture,
    pub items: Texture,
    pub items_on: bool,
    pub pane: Texture,
    pub pane_on: bool,
    /// Mini trace headers and ring columns, 64 texels per slot (`live::mini`).
    pub mini: Texture,
    /// Firmware pin mode, level, PWM duty and emphasis (`live::pin_row`).
    pub pins: Texture,
}

impl CanvasLive {
    /// Live off, for a view no host feeds: one row of off texels in each texture.
    pub fn off(cx: &mut Cx) -> CanvasLive {
        let off = |cx: &mut Cx| crate::live::LiveRows::new().texture(cx).clone();
        CanvasLive {
            nets: off(cx), segs: off(cx), parts: off(cx),
            items: off(cx), items_on: false, pane: off(cx), pane_on: false,
            // live::mini::MINI_EMPTY
            mini: crate::live::LiveRows::with_off([1.0, -1.0, 0.0, -1.0]).texture(cx).clone(),
            pins: crate::live::LiveRows::with_off(crate::live::PIN_OFF).texture(cx).clone(),
        }
    }

    /// Bind the host's item states and enable their sampling.
    pub fn with_items(mut self, items: Texture) -> Self {
        self.items = items;
        self.items_on = true;
        self
    }

    /// Bind this pane's states and enable their sampling.
    pub fn with_pane(mut self, pane: Texture) -> Self {
        self.pane = pane;
        self.pane_on = true;
        self
    }

    /// Bind the host's mini trace headers and ring columns.
    pub fn with_mini(mut self, mini: Texture) -> Self {
        self.mini = mini;
        self
    }

    /// Bind the host's firmware pin rows without changing any instances.
    pub fn with_pins(mut self, pins: Texture) -> Self {
        self.pins = pins;
        self
    }

    fn ids(&self) -> [Option<TextureId>; 7] {
        [Some(self.nets.texture_id()), Some(self.segs.texture_id()), Some(self.parts.texture_id()),
            Some(self.items.texture_id()), Some(self.pane.texture_id()), Some(self.mini.texture_id()), Some(self.pins.texture_id())]
    }
}

impl CanvasLayer {
    /// Replace rows of `slots` floats each. Empty rows clear the layer.
    /// An invalid stride logs an error and leaves the layer empty.
    pub fn set(&mut self, slots: usize, rows: Vec<f32>) {
        self.publication = None;
        self.dirty = true;
        self.count = 0;
        self.slots = slots.max(1);
        if rows.is_empty() {
            return;
        }
        let segment = InstanceSegment::new(rows.into());
        match RetainedInstances::from_segments(slots.max(1), vec![segment]) {
            Ok(publication) => {
                self.count = publication.count();
                self.publication = Some(publication);
            }
            Err(error) => {
                log!("fab.canvas: the layer is not drawn: {error}");
            }
        }
    }

    /// Number of published instances, or zero for an empty/invalid publication.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Submit the retained list, bound to the live textures, and set every named uniform on every
    /// draw item. Empty layers and layer_alpha below 0.004 draw nothing. The list's view
    /// transform stays at identity; only the `cam` uniform transforms the scene.
    pub fn draw(&mut self, cx: &mut Cx2d, draw_vars: &mut DrawVars, uniforms: &[(LiveId, &[f32])], live: &CanvasLive) {
        if self.count == 0
            || uniforms.iter()
                .find(|(id, _)| *id == live_id!(layer_alpha))
                .and_then(|(_, value)| value.first())
                .is_some_and(|alpha| *alpha < 0.004)
        {
            return;
        }
        let Some(publication) = &self.publication else { return };
        let Some(shader) = draw_vars.draw_shader_id else {
            log!("fab.canvas: the layer shader is not compiled");
            return;
        };
        let shader_slots = cx.draw_shaders[shader.index].mapping.instances.total_slots;
        if shader_slots != self.slots {
            log!("fab.canvas: layer has {} slots, shader has {}; not drawn", self.slots, shader_slots);
            return;
        }
        let list = self.list.get_or_insert_with(|| DrawList2d::new(cx));
        let ids = live.ids();
        if ids != self.bound {
            self.bound = ids;
            self.dirty = true;
        }
        if list.begin_maybe(cx, self.dirty).is_ok() {
            cx.begin_root_turtle(dvec2(1.0e7, 1.0e7), Layout::flow_overlay());
            draw_vars.options.depth_write = false;
            draw_vars.set_texture(0, &live.nets);
            draw_vars.set_texture(1, &live.segs);
            draw_vars.set_texture(2, &live.parts);
            draw_vars.set_texture(3, &live.items);
            draw_vars.set_texture(4, &live.pane);
            draw_vars.set_texture(5, &live.mini);
            draw_vars.set_texture(6, &live.pins);
            cx.add_retained_instances(draw_vars, publication);
            cx.end_pass_sized_turtle();
            list.end(cx);
        }
        self.dirty = false;
        let item_states_on = if live.items_on { 1.0 } else { 0.0 };
        let pane_states_on = if live.pane_on { 1.0 } else { 0.0 };
        for item in 0..cx.draw_lists[list.id()].draw_items.len() {
            for &(name, value) in uniforms {
                cx.set_draw_item_uniform(list.id(), item, name, value);
            }
            cx.set_draw_item_uniform(list.id(), item, live_id!(item_states_on), std::slice::from_ref(&item_states_on));
            cx.set_draw_item_uniform(list.id(), item, live_id!(pane_states_on), std::slice::from_ref(&pane_states_on));
        }
    }
}

/// The per-frame camera, clocks, hover, opacity, and pane clip for every canvas shader.
#[derive(Clone, Copy, Debug)]
pub struct CanvasUniforms {
    /// Camera2d::uniforms(): screen offset x/y, points per unit, and DPI.
    pub cam: [f32; 4],
    /// Motion clock in seconds, used with each instance's span.
    pub motion_t: f32,
    /// Exact hovered item id, or -1 for none.
    pub hover_item: f32,
    /// Hover-rim opacity, 0..1.
    pub hover_mix: f32,
    /// Moving copy identity, or -1; connection endpoints follow independently.
    pub drag_item: f32,
    /// World-space translation of the moving copy and attached endpoints.
    pub drag_offset: [f32; 2],
    /// Layer opacity, normally 1.
    pub layer_alpha: f32,
    /// Seconds driving the stale crawl.
    pub time: f32,
    /// Seconds since the live clock's epoch, driving flow.
    pub live_time: f32,
    /// Whether the canvas uses the light theme.
    pub light: bool,
    /// Pane clip (x0, y0, x1, y1) in logical screen points.
    pub clip: [f32; 4],
}

impl CanvasUniforms {
    /// How many uniforms `with` passes, so a caller can append its own on the stack.
    pub const COUNT: usize = 16;

    /// Call `f` with every shader uniform, borrowing this frame and the tokens:
    /// hover/selection/AI accents, violation, and stale.
    /// The list is stack-local and can be passed directly to CanvasLayer::draw.
    pub fn with<R>(&self, tokens: &crate::canvas::looks::Tokens, f: impl FnOnce(&[(LiveId, &[f32])]) -> R) -> R {
        let live_light = if self.light { 1.0 } else { 0.0 };
        let values: [(LiveId, &[f32]); Self::COUNT] = [
            (live_id!(cam), &self.cam),
            (live_id!(motion_t), std::slice::from_ref(&self.motion_t)),
            (live_id!(hover_item), std::slice::from_ref(&self.hover_item)),
            (live_id!(hover_mix), std::slice::from_ref(&self.hover_mix)),
            (live_id!(drag_item), std::slice::from_ref(&self.drag_item)),
            (live_id!(drag_offset), &self.drag_offset),
            (live_id!(layer_alpha), std::slice::from_ref(&self.layer_alpha)),
            (live_id!(time), std::slice::from_ref(&self.time)),
            (live_id!(live_time), std::slice::from_ref(&self.live_time)),
            (live_id!(live_light), std::slice::from_ref(&live_light)),
            (live_id!(clip), &self.clip),
            (live_id!(accent_hover), &tokens.hover),
            (live_id!(accent_focus), &tokens.selection),
            (live_id!(accent_ai), &tokens.ai),
            (live_id!(violation), &tokens.violation),
            (live_id!(stale), &tokens.stale),
        ];
        f(&values)
    }
}

