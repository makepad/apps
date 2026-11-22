//! The print preview in the viewport: every move of the sliced plate as an
//! instanced bead (a capped four-sided tube of the move's width and height, smooth-shaded),
//! drawn from ONE retained instance publication built on the slicing thread.
//! A frame records one draw call of it with the instances started by the playhead
//! (every layer before the printing one, and that layer's shell and core runs so far) and a
//! handful of uniforms: the playhead time and the nozzle's
//! position for the move in progress (drawn up to the nozzle), the layer range,
//! the colour mode. Travel and wipe moves are a publication of their own, drawn
//! only while Travel is on. Issue markers are one more, filtered by layer in the
//! same shader. Nothing is rebuilt per frame; the two nozzle heads are two plain
//! instances.
//!
//! The vertices are the cost of this view (a plate has a million beads, several to a pixel),
//! so the desktop draws them once: the aux pass shades each bead and writes the result into
//! the G-buffer in place of its normal (`DrawFabPathAux`), the composite shows it as is. The
//! lit pass never sees a bead. XR has no G-buffer: it draws them forward (`DrawFabPath`), with
//! a room placement shared by the plate model.

use super::*;
use crate::print::{palette, BeadStream, LiveChunk, PreviewBlock, PrintPreview, RunCut, PREVIEW_SLOTS};
use fab_sdf_tiles::draw::TileLight;
use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;
use std::sync::Arc;

/// The bead shader's colour mode for the printed surface: every printed bead in its filament,
/// no travel, so the layers read as they print, for every kind of part.
pub const SURFACE_MODE: f32 = 6.0;

/// Aux-buffer ids (`w` / `a` channel): element ids are below 16_000_000. Every id over
/// 16_000_004 is a bead to the composite, with code `floor((w - PRINT_AUX_ID) / 8 + 0.5)`.
/// Print beads use `PRINT_AUX_ID + 8 * code`; code 0 means no part. `LIT_BEAD_AUX_ID` also
/// decodes to code 0, but marks a lane that took the screen AO's ambient part in its own lit
/// draw (the bodies), so the composite applies only the direct share. The board keeps
/// `PRINT_AUX_ID`. The lit marker is two levels below that base; steps of 8 keep them apart.
/// The aux target is 32-bit float, exact to 1 at these base ids.
pub const PRINT_AUX_ID: f32 = 16_000_008.0;
pub const LIT_BEAD_AUX_ID: f32 = 16_000_006.0;
/// A lit body the Electronics views have selected (`AppState::elec_selected`): code 0 like the
/// lit marker, two levels above the board's base. The composite shades it as a lit body and
/// rings it as the active selection.
pub const SELECTED_BODY_AUX_ID: f32 = 16_000_010.0;
/// Nozzle head height drawn above the tip (mm) and its widest diameter: a marker small beside
/// the part, its tip on the nozzle's point.
const HEAD_HEIGHT: f32 = 4.5;
const HEAD_WIDTH: f32 = 2.25;

/// A capped tube of `sides` sides along the move. `pos` is (along 0/1, cos, sin) of the
/// cross-section corner; a cap vertex carries its axial normal sign in `normal.x`. Wound for the
/// shader's frame (along, side, up), which is left-handed.
fn prism_vertices(sides: u32) -> (Vec<f32>, Vec<u32>) {
    let mut v: Vec<f32> = Vec::new();
    let mut push = |pos: [f32; 3], axial: f32| {
        v.extend_from_slice(&[pos[0], pos[1], pos[2], 0.0, axial, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    };
    let ring: Vec<(f32, f32)> = (0..sides).map(|k| {
        let a = k as f32 * std::f32::consts::TAU / sides as f32;
        (a.cos(), a.sin())
    }).collect();
    for e in 0..2 {
        for &(c, s) in &ring {
            push([e as f32, c, s], 0.0);
        }
    }
    for (e, axial) in [(0.0f32, -1.0f32), (1.0, 1.0)] {
        for &(c, s) in &ring {
            push([e, c, s], axial);
        }
    }
    let mut idx: Vec<u32> = Vec::new();
    for k in 0..sides {
        let (a, b) = (k, (k + 1) % sides);
        idx.extend_from_slice(&[a, sides + b, b, a, sides + a, sides + b]);
    }
    let (near, far) = (2 * sides, 3 * sides);
    for k in 1..sides - 1 {
        idx.extend_from_slice(&[near, near + k, near + k + 1]);
        idx.extend_from_slice(&[far, far + k + 1, far + k]);
    }
    (v, idx)
}

/// The front of a four-sided tube and its ends: three corners of its section at each end, the
/// two facets between them, and each end's far corner, which closes that end's section into its
/// cap. `pos.y` is a corner's place round from the corner nearest the eye (-1, 0, 1, and 2 the
/// far one), which `DrawFabStrip`'s `facing_corner` picks. The facets that face the eye and the
/// caps are the tube's own, so they draw what its front draws, with half the vertices and none
/// of the back. Keep it at eight vertices: an Apple GPU's vertex stage costs by each instance's
/// vertex count, packed by powers of two, not by its indices. Six vertices a bead cost 2.5 times
/// what eight do, more than the tube's sixteen; four or eight are cheap, and padding to the next
/// power of two is cheaper than any count between.
fn strip_vertices() -> (Vec<f32>, Vec<u32>) {
    let mut v: Vec<f32> = Vec::new();
    for e in 0..2 {
        for j in 0..3 {
            v.extend_from_slice(&[e as f32, j as f32 - 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        }
    }
    for e in 0..2 {
        v.extend_from_slice(&[e as f32, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    }
    // the tube's winding: corner a to the next round b, the near end then the far one
    let mut idx: Vec<u32> = Vec::new();
    for a in 0..2 {
        let b = a + 1;
        idx.extend_from_slice(&[a, 3 + b, b, a, 3 + a, 3 + b]);
    }
    // the caps, fanned from the far corner round the ring as the tube's are: the start's as its
    // near cap, the end's reversed
    idx.extend_from_slice(&[6, 0, 1, 6, 1, 2, 7, 4, 3, 7, 5, 4]);
    (v, idx)
}

/// The bead: four sides and two-triangle caps, 12 triangles for the hundreds of thousands of
/// moves a plate has; a bead is a few pixels across and its normals read round up close.
pub(crate) fn fab_prism_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(FabPrismGeom), |cx| {
        let g = Geometry::new(cx);
        let (v, idx) = prism_vertices(4);
        g.update(cx, idx, v);
        g
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

/// The bead as its front and caps (`strip_vertices`): the headset's, where a plate's beads
/// are a few to an eye's pixel and the vertices are the frame.
pub(crate) fn fab_strip_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(FabStripGeom), |cx| {
        let g = Geometry::new(cx);
        let (v, idx) = strip_vertices();
        g.update(cx, idx, v);
        g
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

/// The far tier's bead (`DrawFabFar`): one quad, its two ends (`pos.x`) by the section's two
/// outline corners across the eye's rays (`pos.y` -1 and 1, which its `facing_corner` places).
/// Four vertices: a power of two, as the strip's eight (`strip_vertices`), half as many.
fn quad_vertices() -> (Vec<f32>, Vec<u32>) {
    let mut v: Vec<f32> = Vec::new();
    for e in 0..2 {
        for j in [-1.0f32, 1.0] {
            v.extend_from_slice(&[e as f32, j, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        }
    }
    (v, vec![0, 2, 1, 1, 2, 3])
}

/// The bead as one quad (`quad_vertices`): the headset's beads a pixel or less across.
pub(crate) fn fab_quad_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(FabQuadGeom), |cx| {
        let g = Geometry::new(cx);
        let (v, idx) = quad_vertices();
        g.update(cx, idx, v);
        g
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

/// The nozzle head: a real hexagon, drawn once or twice a frame.
pub(crate) fn fab_head_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(FabHeadGeom), |cx| {
        let g = Geometry::new(cx);
        let (v, idx) = prism_vertices(6);
        g.update(cx, idx, v);
        g
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

script_mod! {
    use mod.prelude.fab.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    mod.fab_print_geom = {
        FabVertex: #(fab_vertex_pod(vm))
        FabPrismGeom: #(fab_prism_geom(vm))
        FabStripGeom: #(fab_strip_geom(vm))
        FabQuadGeom: #(fab_quad_geom(vm))
        FabHeadGeom: #(fab_head_geom(vm))
    }

    mod.draw.DrawFabPath = mod.std.set_type_default() do #(DrawFabPath::script_shader(vm)){
        alpha_blend: false
        depth_write: true
        backface_culling: true
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_print_geom.FabVertex, mod.fab_print_geom.FabPrismGeom)
        palette: texture_2d(float)
        // Per plate part (the beads' part index): x = its element + 1 (0: none), y = its
        // selection state (1 shown, 2 selected, 3 active)
        parts: texture_2d(float)
        // x = the parts row's width (0: no parts)
        u_parts: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // Per part (the beads' part index): xy = the shift onto the plate of beads built in the
        // part's own frame (a live preview's), `PrintPreview::part_offsets`
        part_offsets: texture_2d(float)
        // x = the part_offsets row's width (0: the beads are placed)
        u_offsets: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // Texel 1 + n: part n's toolpaths are out of date (x > 0.5), when u_faded.x is 1.
        faded_parts: texture_2d(float)
        // x = 1: the parts `faded_parts` marks are faded, y = its width
        u_faded: uniform(vec4(0.0, 1.0, 0.0, 0.0))
        // x = the playhead (s), yzw = the nozzle there (mm)
        u_now: uniform(vec4(1000000000.0, 0.0, 0.0, 0.0))
        // x = the time the sent job has printed up to: later beads are still to print, and dim;
        // y = 1: the head shows through the layers around it
        u_live: uniform(vec4(1000000000.0, 0.0, 0.0, 0.0))
        // x = lowest layer, y = highest layer, z = 1 + the highlighted (current) layer or 0,
        // w = the highest layer drawn whole (`PrintFrame::layer_uniform`)
        u_layers: uniform(vec4(0.0, 1000000000.0, 0.0, 1000000000.0))
        // x = colour mode, y/z = speed range (mm/s), w = top layer index
        u_mode: uniform(vec4(0.0, 0.0, 1.0, 1.0))
        // x = 1: the lit pass's Y-up turn, y = metres per mm, z = 1: a nozzle head
        u_space: uniform(vec4(0.0, 0.001, 0.0, 0.0))
        // Lit-space placement after the mm -> m and Y-up turn. Desktop: identity.
        u_place: uniform(vec4(0.0, 0.0, 0.0, 1.0))
        u_yaw: uniform(vec4(1.0, 0.0, 0.0, 0.0))
        // x = the hovered marker (-1: none), y = 1: non-planar tops drawn as their planar
        // terraces, z = 1: the supports' beads left out (Show supports off)
        u_marker: uniform(vec4(-1.0, 0.0, 0.0, 0.0))
        // xy = 2 over the target's size in pixels (`view_point`)
        u_px: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        // The fragment's place on the target (`view_point`)
        frag_at: fragment_position()
        // The light rig the plate's parts are lit by (`TileLight`, in this shader's Z-up frame):
        // xyz toward the sun, w exposure; the sun's colour, w = 1: tone map + display gamma
        // (the lit target is display-encoded); the sky's and the ground's hemisphere colours.
        u_sun_dir: uniform(vec4(0.35, 0.0 - 0.45, 0.8, 1.0))
        u_sun_color: uniform(vec4(0.72, 0.72, 0.72, 1.0))
        u_sun_sky: uniform(vec4(0.28, 0.28, 0.28, 0.0))
        u_sun_ground: uniform(vec4(0.28, 0.28, 0.28, 0.0))
        // The bead's colour as its source (x): a palette slot, or 128 + 100 * a ramp's place, +
        // 256 * how it is changed (`bead_colour`); and (y) a wall bead's section (0.25 + its half
        // width over 32 half heights, 2 more on a bead ending in a tip, 0 on any other bead) + 4 *
        // what the aux pass writes above the print id: 0, or 1 + 4 * the part's element + its
        // selection state (the composite outlines a selected part's beads as the part). Numbers,
        // not the colour itself: the fragment looks it up, and every float a vertex hands on
        // costs the whole plate's beads. They are the bead's, the same on all its vertices.
        v_color: varying(vec2f)
        // The section's normal, round the bead; `v_joint`: how far the start's and the end's
        // sheared joint swing it forward (see `shade`).
        v_normal: varying(vec3f)
        v_joint: varying(vec2f)
        // A wall bead's creases with its neighbours in the stack, across the eye's rays in its
        // section's half heights: the nearer end of its front (x) and the further (y), ±8192
        // where no neighbour lies that way (`vertex`, the same on all the bead's vertices). A
        // tipped bead's are its steps instead (`shade` reads its creases at each pixel).
        v_stack: varying(vec2f)
        // The eye's ray in the plane across a wall bead, its section's right and up (`vertex`,
        // the bead's), and where across the section a pixel is: the corner's -1 left to 1 right,
        // so its sign says which side's face it is.
        v_ray: varying(vec2f)
        v_across: varying(float)

        ramp: fn(x: float) -> vec3f {
            let f = clamp(x, 0.0, 1.0) * 4.0
            return vec3(0.15, 0.30, 0.95).mix(vec3(0.10, 0.80, 0.85), clamp(f, 0.0, 1.0))
                .mix(vec3(0.20, 0.85, 0.25), clamp(f - 1.0, 0.0, 1.0))
                .mix(vec3(0.95, 0.85, 0.15), clamp(f - 2.0, 0.0, 1.0))
                .mix(vec3(0.95, 0.20, 0.15), clamp(f - 3.0, 0.0, 1.0))
        }

        pal: fn(i: float) -> vec4f {
            return self.palette.sample_nearest(vec2((i + 0.5) / 80.0, 0.5))
        }

        // The bead's colour from `v_color.x`, and in w how it is encoded (1: sRGB, 2: linear):
        // its source, then changed by 256 * (1: a picked marker's, lightened; 2: the current
        // layer's, lightened a little; 4: faded; 8: a filament's, linear).
        bead_colour: fn() -> vec4f {
            var how = floor((self.v_color.x + 0.5) / 256.0)
            let source = self.v_color.x - 256.0 * how
            var col = self.pal(floor(source + 0.5))
            if source > 127.5 {
                col = vec4(self.ramp((source - 128.0) / 100.0), 1.0)
            }
            let linear = floor(how / 8.0 + 0.01)
            how = how - 8.0 * linear
            let faded = floor(how / 4.0 + 0.01)
            how = how - 4.0 * faded
            let current = floor(how / 2.0 + 0.01)
            let marker = how - 2.0 * current
            if linear > 0.5 {
                col = vec4(col.xyz, 2.0)
            }
            if marker > 0.5 {
                col = col.mix(vec4(1.0, 1.0, 1.0, 1.0), 0.5)
            }
            if current > 0.5 {
                col = vec4(col.xyz.mix(vec3(1.0, 1.0, 1.0), 0.2), col.w)
            }
            if faded > 0.5 {
                col = vec4(col.xyz * 0.40 + vec3(0.14, 0.14, 0.15), col.w)
            }
            return col
        }

        vertex: fn() {
            // The bead's packed lanes (`fab::print::bead`): seg_b.w its duration and speed,
            // seg_k.x its width and height (f16s, negative at a free end), seg_k.y its layer + 1
            // and its tag's low 16 bits, seg_k.z its role with its steps (24 bits) and its kind,
            // seg_k.w its filament, its two joints and its tag's high byte.
            let ts = unpack2f16(self.seg_b.w)
            let t0 = self.seg_a.w
            let t1 = t0 + ts.x
            let speed = ts.y
            let wh = unpack2f16(self.seg_k.x)
            let lt = floor(unpack4u8(self.seg_k.y) * 255.0 + 0.5)
            let rk = floor(unpack4u8(self.seg_k.z) * 255.0 + 0.5)
            let fj = floor(unpack4u8(self.seg_k.w) * 255.0 + 0.5)
            var a = self.seg_a.xyz
            var b = self.seg_b.xyz
            var rs = abs(wh.x) * 0.5
            var ru = abs(wh.y) * 0.5
            let layer = lt.x + 256.0 * lt.y - 1.0
            // nozzle + 2 * (1 + part), or an overlay's tag
            let tag = lt.z + 256.0 * lt.w + 65536.0 * fj.w - 16.0
            // a bead's role, + 32 * its wall's steps (`fab::print::stack`)
            let role_steps = rk.x + 256.0 * (rk.y + 256.0 * rk.z)
            // kind + 8 * the move's experimental mode; a Z hop (`fab::print::kind::ZHOP`) draws as travel
            let xmode = floor(rk.w / 8.0 + 0.001)
            let kind = if rk.w - 8.0 * xmode > 4.5 { 1.0 } else { rk.w - 8.0 * xmode }
            let role = role_steps - 32.0 * floor(role_steps / 32.0 + 0.00001)
            let head = self.u_space.z > 0.5
            var hide = 0.0
            let lit_layer = self.u_layers.z - 1.0
            let surface = self.u_mode.x > 5.5
            // its filament, the start's joint and the end's, `print::join_beads`
            let filament = fj.x
            var joint_end = fj.z
            let joint_start = fj.y
            // The printed surface: every bead that prints, in its filament, no travel or wipe.
            // The inside stays drawn under the outside: a seam's gap or a line's end shows the
            // wall behind it, as on the part, never the background; the cut layer reads whole.
            // The picked branch's lit beads (tag -3, `print::trees::PICKED_TAG`) stay: what is
            // picked shows in every look.
            if !head && surface && kind > 0.5 && (kind < 2.5 || kind > 3.5 || abs(tag + 3.0) > 0.5) {
                hide = 1.0
            }
            let part = floor(tag / 2.0 + 0.001)
            let nozzle = tag - 2.0 * part
            // A live preview's beads are in their part's frame: its place on the plate is a
            // texel, so a part that only moves keeps its bead buffers (not the overlay kind).
            if !head && (kind < 2.5 || kind > 3.5) && part > 0.5 && part < self.u_offsets.x + 0.5 {
                let o = self.part_offsets.sample_nearest(vec2((part - 0.5) / self.u_offsets.x, 0.5))
                a = a + vec3(o.x, o.y, 0.0)
                b = b + vec3(o.x, o.y, 0.0)
            }
            var elem = 0.0
            if !head && (kind < 2.5 || kind > 3.5) && part > 0.5 && part < self.u_parts.x + 0.5 {
                let e = self.parts.sample_nearest(vec2((part - 0.5) / self.u_parts.x, 0.5))
                if kind < 2.5 && e.x > 0.5 {
                    elem = 1.0 + 4.0 * (e.x - 1.0) + e.y
                }
                // Its eye went off since this slice: gone now, ahead of the slice without it.
                if e.z > 0.5 {
                    hide = 1.0
                }
            }
            if !head {
                if layer >= 0.0 && layer < self.u_layers.x - 0.5 {
                    hide = 1.0
                }
                if layer > self.u_layers.y + 0.5 {
                    hide = 1.0
                }
                // Comparing a non-planar top with its planar terraces: the terraces stand in.
                if self.u_marker.y > 0.5 && xmode > 7.5 && xmode < 8.5 {
                    hide = 1.0
                }
                // Show supports off: a support's beads (its roles, `print::trees::SUPPORT_ROLES`)
                // go, the trees growing while they plan among them.
                if self.u_marker.z > 0.5 && kind < 0.5 && role > 13.5 && role < 16.5 {
                    hide = 1.0
                }
                if t0 > self.u_now.x {
                    hide = 1.0
                }
                // The move in progress ends at the nozzle.
                if t1 > self.u_now.x {
                    b = self.u_now.yzw
                    joint_end = 127.0
                }
            }
            let d = b - a
            var len = length(d)
            if len < 0.0001 {
                hide = 1.0
                len = 1.0
            }
            let fwd = d / len
            var side = vec3(1.0, 0.0, 0.0)
            let cz = cross(fwd, vec3(0.0, 0.0, 1.0))
            if length(cz) > 0.001 {
                side = normalize(cz)
            }
            let up = cross(side, fwd)
            var g = self.geom.pos
            // Beads hang below the nozzle point by half their height.
            var drop = ru
            if kind > 0.5 {
                drop = 0.0
            }
            if head {
                rs = rs * mix(0.08, 1.0, g.x)
                ru = rs
                drop = 0.0
            }
            // A printed bead's section reaches a little into its neighbours': beads that only
            // touch (the layer above at a corner, the line beside at a side) leave pinholes
            // along the seam where the rasteriser covers neither, and the print's inside or the
            // background shows through as dark specks.
            var cs = rs
            var cu = ru
            if !head && kind < 0.5 {
                cs = rs * 1.15
                cu = ru * 1.3
            }
            g = self.facing_corner(g, (a + b) * 0.5, side, up, cs, cu)
            // Beads of one path meet in one tube. Up to a right angle both end on the plane halfway
            // between them: `shear` is the tangent of half the turn, reaching the outer edge
            // forward and drawing the inner back, so the two sections coincide and their normals
            // run on through the corner. Past a right angle that plane's point would run away:
            // both beads end in one tip instead, on the outside of the corner, inside the nozzle's
            // round: `tip_along` and `tip_side` are the sine and cosine of half the turn.
            var shear = 0.0
            var tip_along = 0.0
            var tip_side = 0.0
            if !head && kind < 0.5 {
                let turn = mix(joint_start, joint_end, g.x) - 127.0
                if abs(turn) > 100.5 {
                    let out = (126.0 - abs(turn)) * 0.028284
                    tip_along = sqrt(1.0 - out * out)
                    tip_side = out
                    if turn < 0.0 {
                        tip_side = -out
                    }
                } else {
                    shear = turn / 100.0
                }
            }
            let into = 1.0 - 2.0 * g.x
            // The inner edge stops at the bead's middle: past it the side would turn inside out on
            // a bead shorter than the corner is wide, and it is inside the neighbour there anyway.
            // A hair past the plane they share, so the raster covers the seam from one side or the
            // other and no pinholes show the caps behind it.
            let seal = if !head && kind < 0.5 { rs * 0.02 } else { 0.0 }
            // The whole front's quad is one section the bead's whole length: no tip leans its end
            // aside (its `end_reach` covers one).
            var tip = rs * (1.0 - abs(g.y))
            if self.whole_front() {
                tip = 0.0
            }
            let along = into * min(g.y * cs * shear * into, len * 0.5) - into * (seal + tip_along * tip + self.end_reach(rs))
            let across = g.y * cs - into * tip_side * tip
            let p = a + d * g.x + fwd * along + side * across + up * (g.z * cu) - vec3(0.0, 0.0, drop)
            // The facet's normal round the section, and the cap's out of the bead's end: a tube's
            // vertex is one or the other (`geom.normal.x`: -1 the start's cap, 1 the end's), a
            // strip's corner both (`cap_out`).
            let axial = if abs(self.geom.normal.x) > 0.5 { self.geom.normal.x } else { 2.0 * g.x - 1.0 }
            let e = normalize(vec2(g.y / max(rs, 0.0001), g.z / max(ru, 0.0001)))
            var n = side * e.x + up * e.y
            var n_cap = fwd * axial
            var joint = vec2(0.0, 0.0)
            var joint_cap = vec2(0.0, 0.0)
            // At a joint the side swings forward by the half turn, so the two beads' normals
            // agree where they meet and run on round the corner's outside, as the nozzle's round
            // turns it there. The corner's inside is sharp (the beads' walls meet in a crease):
            // its side keeps the bead's own normal, so a wall reads flat right into an inside
            // corner. It swings over the stretch a nozzle's round takes to turn the corner (the
            // half width times the half turn, a right angle's at most), and the bead's own side
            // shows past it: a flat wall reads flat to its corners, the same on every layer
            // wherever its moves break. A chord of a curve the path follows in short steps (its
            // bulge, the length times the half turn over 4, under 0.02 mm) swings along its whole
            // length on both sides, so the curve reads smooth, concave or convex. Past a right
            // angle the caps make the tip on the outside: they take the corner's outside
            // section, swung whole, so a sharp corner rounds as a right angle does.
            if !head && kind < 0.5 {
                let ta = joint_start - 127.0
                let tb = joint_end - 127.0
                // The tangent of half the turn, signed as the shear, and half the turn: the
                // shear's arctangent (within 0.004 rad, the shear is at most 1), or past a right
                // angle the tip's cosine, the tangent up to 8 (a turn of 166 degrees).
                var sa = ta / 100.0
                var sb = tb / 100.0
                var ha = abs(sa) * (0.7854 + 0.273 * (1.0 - abs(sa)))
                var hb = abs(sb) * (0.7854 + 0.273 * (1.0 - abs(sb)))
                if abs(ta) > 100.5 {
                    let o = clamp((126.0 - abs(ta)) * 0.028284, 0.124, 1.0)
                    ha = acos(o)
                    sa = sign(ta) * sqrt(1.0 - o * o) / o
                }
                if abs(tb) > 100.5 {
                    let o = clamp((126.0 - abs(tb)) * 0.028284, 0.124, 1.0)
                    hb = acos(o)
                    sb = sign(tb) * sqrt(1.0 - o * o) / o
                }
                let corner_a = smoothstep(0.02, 0.05, len * ha * 0.25)
                let corner_b = smoothstep(0.02, 0.05, len * hb * 0.25)
                let reach_a = mix(len, min(cs * min(ha, 0.7854), len), corner_a)
                let reach_b = mix(len, min(cs * min(hb, 0.7854), len), corner_b)
                // Each end's swing: its tangent at the joint, down to 0 at its reach and below
                // zero past it, linear along the bead so it interpolates exactly. Lifted by 8192
                // (the swing never takes it below -7000) and signed by the shear, one number
                // carries both: the fragment takes the lift off and keeps what is above zero. A
                // corner's inside (by `joint_side`) drops 512 below it, so no swing reaches its side. The start's
                // outside is right of the bead (g.y > 0) after a left turn (sa < 0), the end's
                // before one (sb > 0).
                let ra = max(abs(sa) * (1.0 - g.x * len / max(reach_a, 0.0001)), -7000.0)
                let rb = max(abs(sb) * (1.0 - (1.0 - g.x) * len / max(reach_b, 0.0001)), -7000.0)
                var wa = 8192.0 + ra
                var wb = 8192.0 + rb
                let js = self.joint_side(g, (a + b) * 0.5, side, up)
                if js * sign(sa) > 0.5 {
                    wa = 8192.0 + ra * (1.0 - corner_a) - 512.0 * corner_a
                }
                if js * sign(sb) < -0.5 {
                    wb = 8192.0 + rb * (1.0 - corner_b) - 512.0 * corner_b
                }
                joint = vec2(sign(sa) * wa, sign(sb) * wb)
                if axial > 0.0 {
                    if abs(tb) > 100.5 {
                        n_cap = side * (sign(sb) * abs(e.x)) + up * e.y
                        joint_cap = vec2(0.0, sign(sb) * (8192.0 + abs(sb)))
                    }
                } else if abs(ta) > 100.5 {
                    n_cap = side * (0.0 - sign(sa) * abs(e.x)) + up * e.y
                    joint_cap = vec2(sign(sa) * (8192.0 + abs(sa)), 0.0)
                }
            }
            if abs(self.geom.normal.x) > 0.5 {
                n = n_cap
                joint = joint_cap
            }

            // The colour's source and how it is changed (`bead_colour`): a filament's colour is
            // linear reflectance, as the parts' materials, the others display colours (the
            // legend's).
            var source = 0.0
            var how = 0.0
            let mode = self.u_mode.x
            if head {
                source = filament
                // the waiting nozzle: its own colour, dimmed (`palette::HEADS`)
                if role_steps > 0.5 {
                    how = 4.0
                }
            } else if kind > 3.5 {
                source = 73.0
            } else if kind > 2.5 {
                source = 54.0 + role_steps
                if abs(tag - self.u_marker.x) < 0.5 {
                    how = 1.0
                }
            } else if kind > 1.5 {
                source = 51.0
            } else if kind > 0.5 {
                source = 50.0
            } else if mode < 0.5 {
                source = role
            } else if mode < 1.5 {
                source = 128.0 + 100.0 * clamp((speed - self.u_mode.y) / max(self.u_mode.z - self.u_mode.y, 0.001), 0.0, 1.0)
            } else if mode < 2.5 {
                source = 48.0 + nozzle
            } else if mode < 3.5 {
                source = 32.0 + max(filament - 1.0, 0.0)
                how = 8.0
            } else if mode < 4.5 {
                source = 128.0 + 100.0 * clamp(layer / max(self.u_mode.w, 1.0), 0.0, 1.0)
            } else if mode > 5.5 {
                source = 32.0 + max(filament - 1.0, 0.0)
                how = 8.0
            } else {
                source = 64.0 + xmode
            }
            // The current layer stands out a little (the cut face reads up close); the layers
            // under it keep their colour, lit as the parts are.
            if !head && lit_layer > -0.5 && layer >= 0.0 && abs(layer - lit_layer) < 0.5 {
                how = how + 2.0
            }
            // An out-of-date slice (a newer one is on its way) stays, faded: all of it, or
            // the parts that changed.
            var faded = self.u_space.w > 0.5
            if self.u_faded.x > 0.5 && part > 0.5 && part < self.u_faded.y - 0.5 {
                faded = self.faded_parts.sample_nearest(vec2((part + 0.5) / self.u_faded.y, 0.5)).x > 0.5
            }
            if !head && (faded || t1 > self.u_live.x) {
                how = how + 4.0
            }
            // A wall's bead (`shade` filters its creases) carries its section: its half width
            // over its half height, up to 16. So does every printed bead the far tier draws: its
            // pixel reads its whole front (`whole_front`).
            var wall = 0.0
            if !head && kind < 0.5 && ((role > 0.5 && role < 3.5) || self.whole_front()) {
                wall = 0.25 + clamp(cs / max(ru, 0.0001), 0.0, 16.0) / 32.0
            }
            // A bead ending in a tip (a turn past a right angle) leans its sides into it, so its
            // front is not one section along it: `shade` reads its creases at each pixel, from
            // its steps (2 above its section's code says so). A far bead's pixel holds all of it.
            let tipped = wall > 0.0 && !self.whole_front() && (abs(joint_start - 127.0) > 100.5 || abs(joint_end - 127.0) > 100.5)
            if tipped {
                wall = wall + 2.0
            }
            n = self.seen_normal(n, (a + b) * 0.5, side, up, cs, cu, wall)
            self.v_color = vec2(source + 256.0 * how, wall + 4.0 * elem)
            self.v_normal = self.placed_normal(n)
            self.v_joint = joint
            self.cap_out(self.placed_normal(n_cap), joint_cap)
            var wp = p * self.u_space.y
            if self.u_space.x > 0.5 {
                wp = vec3(wp.x, wp.z, 0.0 - wp.y)
                let c = self.u_yaw.x
                let s = self.u_yaw.y
                wp = vec3(c * wp.x + s * wp.z, wp.y, c * wp.z - s * wp.x) * self.u_place.w + self.u_place.xyz
            }
            let view4 = self.draw_pass.camera_view * vec4(wp.x, wp.y, wp.z, 1.0)
            // A wall bead's creases with the beads stacked on it and under it (`shade`), read once
            // for the bead: its front across the eye's rays in the plane across it, the ray to its
            // middle (along a level bead the rays keep their direction in that plane), in its
            // section's half heights. Each neighbour's steps (`fab::print::stack`: 127ths of the
            // width out to the right from 128, 0 none) put its section beside this one, a layer
            // up or down; 8192 where no neighbour lies to that end across the rays.
            var stack = vec2(8192.0, 0.0 - 8192.0)
            var ray_across = vec2(0.0, 1.0)
            let steps = floor(role_steps / 32.0 + 0.00001)
            var above = floor(steps / 256.0 + 0.00001)
            var below = steps - 256.0 * above
            // Solid fill and skins (roles 5..7) hold their side neighbours there instead: the
            // distance across to the right (`below`'s bits) and to the left, 0 none.
            let skin = role > 4.5 && role < 7.5
            let side_right = below
            let side_left = above
            if skin {
                above = 0.0
                below = 0.0
            }
            // Only a neighbour that is drawn hides this bead's front: none stands on the highest
            // layer drawn whole (the slider's top, or the last one done at the playhead), or
            // under the lowest layer shown. A step to a layer left out would shade the bead's
            // top or underside as hidden by a bead that is not there.
            if layer > self.u_layers.w - 0.5 {
                above = 0.0
            }
            if layer < self.u_layers.x + 0.5 {
                below = 0.0
            }
            if tipped {
                // Its steps, in its section's half heights, times the normal's share along the
                // right (`shade` divides by its level part, so each side of the bead reads the
                // steps out of that side), 8192 where there is none.
                let right = dot(n.xy, side.xy) * rs * 2.0 / (127.0 * max(ru, 0.0001))
                stack = vec2(8192.0, 8192.0)
                if above > 0.5 {
                    stack.x = (above - 128.0) * right
                }
                if below > 0.5 {
                    stack.y = (below - 128.0) * right
                }
            } else if wall > 0.0 {
                var mid_w = ((a + b) * 0.5 - vec3(0.0, 0.0, drop)) * self.u_space.y
                var side_w = side
                var level = vec4(0.0, 0.0, 1.0, 0.0)
                if self.u_space.x > 0.5 {
                    mid_w = vec3(mid_w.x, mid_w.z, 0.0 - mid_w.y)
                    side_w = vec3(side.x, side.z, 0.0 - side.y)
                    let c = self.u_yaw.x
                    let s = self.u_yaw.y
                    mid_w = vec3(c * mid_w.x + s * mid_w.z, mid_w.y, c * mid_w.z - s * mid_w.x) * self.u_place.w + self.u_place.xyz
                    side_w = vec3(c * side_w.x + s * side_w.z, side_w.y, c * side_w.z - s * side_w.x)
                    level = vec4(0.0, 1.0, 0.0, 0.0)
                }
                // The ray, the side and the vertical as the view turns them: the side is level and
                // as long as the vertical, so the plane's two axes stay square and alike under the
                // view (a turn, maybe scaled), and only the ray's way in it is kept.
                let mid_v = (self.draw_pass.camera_view * vec4(mid_w.x, mid_w.y, mid_w.z, 1.0)).xyz
                let up_v = (self.draw_pass.camera_view * level).xyz
                let persp = clamp((self.draw_pass.camera_projection * vec4(0.0, 0.0, -1.0, 0.0)).w, 0.0, 1.0)
                let ray = mid_v * persp + vec3(0.0, 0.0, persp - 1.0)
                let outward = (self.draw_pass.camera_view * vec4(side_w.x, side_w.y, side_w.z, 0.0)).xyz
                var dv = vec2(dot(ray, outward), dot(ray, up_v))
                dv = dv / max(length(dv), 0.000001)
                ray_across = dv
                var ev = vec2(dv.y, 0.0 - dv.x)
                if ev.y < 0.0 {
                    ev = 0.0 - ev
                }
                // (the half width as `shade` decodes it)
                let code = wall + 4.0 * elem
                let hs = (code - 4.0 * floor((code + 0.5) * 0.25) - 0.25) * 32.0
                let hu = 1.3
                var hi = vec2(0.0, hu)
                var near = vec2(hs, 0.0)
                if dv.x > 0.0 {
                    near = vec2(0.0 - hs, 0.0)
                }
                if hs * abs(ev.x) > hu * ev.y {
                    hi = vec2(hs, 0.0)
                    if ev.x < 0.0 {
                        hi = vec2(0.0 - hs, 0.0)
                    }
                    near = vec2(0.0, hu)
                    if dv.y > 0.0 {
                        near = vec2(0.0, 0.0 - hu)
                    }
                }
                let lo = 0.0 - hi
                let at = vec3(dot(lo, ev), dot(near, ev), dot(hi, ev))
                let z = vec3(dot(lo, dv), dot(near, dv), dot(hi, dv))
                let k = vec2((z.y - z.x) / max(at.y - at.x, 0.000001), (z.z - z.y) / max(at.z - at.y, 0.000001))
                let right = rs * 2.0 / (127.0 * max(ru, 0.0001))
                let qa = vec2((above - 128.0) * right, 2.0)
                let qb = vec2((below - 128.0) * right, -2.0)
                let pa = dot(qa, ev)
                let pb = dot(qb, ev)
                if above > 0.5 && pa > 0.0 {
                    stack.x = min(stack.x, self.crease(qa, z, k, at, ev, dv))
                }
                if above > 0.5 && pa < 0.0 {
                    stack.y = max(stack.y, pa + self.crease(0.0 - qa, z, k, at, ev, dv))
                }
                if below > 0.5 && pb > 0.0 {
                    stack.x = min(stack.x, self.crease(qb, z, k, at, ev, dv))
                }
                if below > 0.5 && pb < 0.0 {
                    stack.y = max(stack.y, pb + self.crease(0.0 - qb, z, k, at, ev, dv))
                }
                // Solid fill and skins (roles 5..7, the far tier's: near reads them per fragment):
                // the beads of their own layer that lie beside them (`fab::print::stack`), level
                // with them, each hiding the front past its crease. None beside: the outline.
                if skin {
                    let qr = vec2(side_right * right, 0.0)
                    let ql = vec2(0.0 - side_left * right, 0.0)
                    let pr = dot(qr, ev)
                    let pl = dot(ql, ev)
                    if side_right > 0.5 && pr > 0.0 {
                        stack.x = min(stack.x, self.crease(qr, z, k, at, ev, dv))
                    }
                    if side_right > 0.5 && pr < 0.0 {
                        stack.y = max(stack.y, pr + self.crease(0.0 - qr, z, k, at, ev, dv))
                    }
                    if side_left > 0.5 && pl > 0.0 {
                        stack.x = min(stack.x, self.crease(ql, z, k, at, ev, dv))
                    }
                    if side_left > 0.5 && pl < 0.0 {
                        stack.y = max(stack.y, pl + self.crease(0.0 - ql, z, k, at, ev, dv))
                    }
                }
            }
            self.v_stack = stack
            self.v_ray = ray_across
            self.v_across = self.across_here(g, n, side)
            // A far bead that is no wall (near shades it per fragment) takes the display curve
            // over its front's taps (`shade`): it says so twice across.
            if wall > 0.0 && self.whole_front() && !(role > 0.5 && role < 3.5) {
                self.v_across = self.v_across * 2.0
            }
            var clip = self.draw_pass.camera_projection * view4
            // Monitor's job: nothing hides where the printer is. Its depth moves in front of the
            // scene and stays in order within the head, so the head still looks solid.
            if head && self.u_live.y > 0.5 {
                clip = vec4(clip.x, clip.y, clip.z * 0.02, clip.w)
            }
            if hide > 0.5 {
                clip = vec4(0.0, 0.0, 0.0 - 2.0, 1.0)
            }
            self.vertex_pos = clip
        }

        // A normal turned with the placement (`u_space.x`): its yaw about the vertical, in the
        // print's Z-up frame, where the key and fill directions are; as it is on the desktop.
        placed_normal: fn(n: vec3f) -> vec3f {
            if self.u_space.x > 0.5 {
                let c = self.u_yaw.x
                let s = self.u_yaw.y
                return vec3(c * n.x - s * n.y, s * n.x + c * n.y, n.z)
            }
            return n
        }

        // A strip's corner (`DrawFabStrip`) is its facets' and its cap's: it hands on the cap's
        // normal and joints as well. A tube's vertex is one or the other.
        cap_out: fn(n: vec3f, joint: vec2f) {
        }

        // The section's normal and its joints' swing at this pixel (`vertex`).
        normal_here: fn() -> vec3f {
            return self.v_normal
        }
        swing_here: fn() -> float {
            return self.swing(self.v_joint)
        }
        // A joint's swing (`v_joint`): the lift off, what is above zero, signed.
        swing: fn(j: vec2f) -> float {
            return sign(j.x) * max(abs(j.x) - 8192.0, 0.0) + sign(j.y) * max(abs(j.y) - 8192.0, 0.0)
        }

        // The section's corner as the tube's geometry gives it; the strip (`DrawFabStrip`) turns
        // its corners to the eye.
        facing_corner: fn(g: vec3f, mid: vec3f, side: vec3f, up: vec3f, cs: float, cu: float) -> vec3f {
            return g
        }
        // How far past each end the bead reaches; the far tier's quad (`DrawFabFar`) draws no
        // caps and reaches over them.
        end_reach: fn(rs: float) -> float {
            return 0.0
        }
        // The normal the bead shows at this corner, `n` the section's; the far tier's quad
        // shows its whole front's (`wall`: the bead's section code, 0 on any but a wall's).
        seen_normal: fn(n: vec3f, mid: vec3f, side: vec3f, up: vec3f, cs: float, cu: float, wall: float) -> vec3f {
            return n
        }
        // Where across the section the corner is (`v_across`): its place right of the middle.
        across_here: fn(g: vec3f, n: vec3f, side: vec3f) -> float {
            return g.y
        }
        // The side of the bead a corner is on for its joints (`vertex`: a corner's inside drops
        // below the swing): its own, across the section.
        joint_side: fn(g: vec3f, mid: vec3f, side: vec3f, up: vec3f) -> float {
            return g.y
        }
        // A wall's pixel reads its whole front (`shade`): the far tier's do, the bead a pixel or
        // less across; the others read theirs where two pixels take it.
        whole_front: fn() -> bool {
            return false
        }

        // The eye's direction across the bead from its middle `mid`, in its section's right
        // (`side`) and up: from the eye in perspective (w rises along the view's axis), down
        // that axis in an orthographic view.
        eye_across: fn(mid: vec3f, side: vec3f, up: vec3f) -> vec2f {
            var mid_p = mid * self.u_space.y
            var side_p = side
            var up_p = up
            if self.u_space.x > 0.5 {
                mid_p = vec3(mid_p.x, mid_p.z, 0.0 - mid_p.y)
                side_p = vec3(side.x, side.z, 0.0 - side.y)
                up_p = vec3(up.x, up.z, 0.0 - up.y)
                let yc = self.u_yaw.x
                let ys = self.u_yaw.y
                mid_p = vec3(yc * mid_p.x + ys * mid_p.z, mid_p.y, yc * mid_p.z - ys * mid_p.x) * self.u_place.w + self.u_place.xyz
                side_p = vec3(yc * side_p.x + ys * side_p.z, side_p.y, yc * side_p.z - ys * side_p.x)
                up_p = vec3(yc * up_p.x + ys * up_p.z, up_p.y, yc * up_p.z - ys * up_p.x)
            }
            let mid_v = (self.draw_pass.camera_view * vec4(mid_p.x, mid_p.y, mid_p.z, 1.0)).xyz
            let persp = clamp((self.draw_pass.camera_projection * vec4(0.0, 0.0, -1.0, 0.0)).w, 0.0, 1.0)
            let ray = mid_v * persp + vec3(0.0, 0.0, persp - 1.0)
            return vec2(
                0.0 - dot(ray, (self.draw_pass.camera_view * vec4(side_p.x, side_p.y, side_p.z, 0.0)).xyz),
                0.0 - dot(ray, (self.draw_pass.camera_view * vec4(up_p.x, up_p.y, up_p.z, 0.0)).xyz)
            )
        }

        // The fragment's point in view space, from its place on the target and the projection:
        // the point the vertex stage put there, without a varying to carry it.
        view_point: fn() -> vec3f {
            let f = self.frag_at
            let p = self.draw_pass.camera_projection
            let c0 = p * vec4(1.0, 0.0, 0.0, 0.0)
            let c1 = p * vec4(0.0, 1.0, 0.0, 0.0)
            let c2 = p * vec4(0.0, 0.0, 1.0, 0.0)
            let c3 = p * vec4(0.0, 0.0, 0.0, 1.0)
            let cw = 1.0 / f.w
            // (perspective: w is the depth; orthographic: z is)
            var z = (f.z * cw - c3.z) / c2.z
            if abs(c2.w) > 0.5 {
                z = (cw - c3.w) / c2.w
            }
            let ndc = vec2(f.x * self.u_px.x - 1.0, 1.0 - f.y * self.u_px.y)
            let xy = (ndc * cw - c2.xy * z - c3.xy) / vec2(c0.x, c1.y)
            return vec3(xy.x, xy.y, z)
        }

        // The parts' light (the model lanes' and the SDF engine's rig: Lambert under a sun and
        // a sky/ground hemisphere that sum to one), so a sliced part reads as bright as the part.
        light: fn(n: vec3f, l: vec3f) -> vec3f {
            let hemi = mix(self.u_sun_ground.xyz, self.u_sun_sky.xyz, clamp(n.z * 0.5 + 0.5, 0.0, 1.0))
            return hemi + self.u_sun_color.xyz * max(dot(n, l), 0.0)
        }

        // A bead's section (a diamond, half width `cs`, half height 1.3 ru) seen along `d` in the
        // plane across the bead: its front runs across the rays (`e`, rising with the stack)
        // between the outline's two ends `lo` and `hi` through the corner nearest the eye `mid`,
        // at `at` (their places across the rays). Its depth along the rays at `s`: from `z` at
        // those points, `k` the rise per unit across the rays on its two facets.
        depth: fn(s: float, z: vec3f, k: vec2f, at: vec3f) -> float {
            if s < at.y {
                return z.x + k.x * (max(s, at.x) - at.x)
            }
            return z.y + k.y * (min(s, at.z) - at.y)
        }

        // The crease where the front passes from this bead to the one stacked on it at `q`, `p`
        // across the rays (p > 0). Where the two fronts overlap, how much nearer the eye the
        // stacked one is (`g`) stays put up to this front's corner nearest the eye, closes at the
        // two facets' slopes' difference from there to the stacked front's corner, and stays put
        // again: the crease is where it passes zero, or the overlap's start if the stacked front
        // hides this one there already, or its end if it never does.
        crease: fn(q: vec2f, z: vec3f, k: vec2f, at: vec3f, e: vec2f, d: vec2f) -> float {
            let p = dot(q, e)
            let s0 = at.x + p
            if s0 >= at.z {
                return at.z
            }
            let g0 = self.depth(s0, z, k, at) - dot(q, d) - z.x
            if g0 >= 0.0 {
                return s0
            }
            let s = max(at.y, s0) - g0 / max(k.y - k.x, 0.00000000000000000001)
            if s <= min(at.y + p, at.z) {
                return s
            }
            return at.z
        }

        // A tap of a wall bead's window at `f` across the rays (past a crease, the next bead's
        // front, one period on): its normal's height and the sun's Lambert on it.
        tap: fn(f: float, top: float, bottom: float, period: float, at: vec3f, ia: float, ib: float, nlo: vec3f, nmid: vec3f, nhi: vec3f, l: vec3f) -> vec2f {
            var s = f
            if s > top {
                s = s - period
            }
            if s < bottom {
                s = s + period
            }
            var n = mix(nmid, nhi, clamp((s - at.y) * ib, 0.0, 1.0))
            if s < at.y {
                n = mix(nlo, nmid, clamp((s - at.x) * ia, 0.0, 1.0))
            }
            let nn = n / max(length(n), 0.00000000000000000001)
            return vec2(nn.z, max(dot(nn, l), 0.0))
        }

        shade: fn() -> vec4f {
            // A sheared joint swings the side forward, as much as the normal leans sideways: a
            // layer's bead runs level, so its forward is its side a quarter turn about the
            // vertical (a vase's rising spiral is within a hair of it).
            let w = self.swing_here()
            let m = self.normal_here()
            // A wall's beads stack in layers, and the crease between two is where the shade
            // jumps from one bead's underside to the next one's top: a hard line, a pixel wide or
            // less, whose darkest pixel goes with where it falls in the pixel, a phase that walks
            // along the wall, so a flat wall beats in bands. The shade is the mean over two
            // pixels across the stack instead, which takes a line that thin alike at any phase.
            // It is read in the plane across the bead, where the rays from the eye run parallel
            // and a place across them is a place on the screen. The bead's section is a diamond,
            // and so are its neighbours' in the wall, where its steps put them: each hides the
            // bead's front past its crease with it, so the front shows between two creases, and
            // past a crease the next bead's goes on as this one's. The two pixels split at the
            // crease in them, each side read at its two Gauss points; a layer two pixels high or
            // less is read over whole, its even mean. Where no wall stands above or below, the
            // stack ends at the section's own outline. `u` is the height over the bead's middle
            // in half layers (the section's normal is linear on its facets, which rise 1.3 half
            // layers).
            let u = m.z * 1.3
            let du = length(vec2(dFdx(u), dFdy(u)))
            let colour = self.bead_colour()
            let section = self.v_color.y - 4.0 * floor((self.v_color.y + 0.5) * 0.25)
            var albedo = max(colour.xyz, vec3(0.0, 0.0, 0.0))
            if colour.w < 1.5 {
                albedo = pow(albedo, vec3(2.2, 2.2, 2.2))
            }
            var sky = vec3(0.0, 0.0, 0.0)
            // A far bead's colour on the display in linear light (`tap_shown`), none (negative)
            // on any other.
            var shown = vec3(0.0 - 1.0, 0.0, 0.0)
            let l = normalize(self.u_sun_dir.xyz)
            if section > 0.125 && (du > 0.000001 || self.whole_front()) {
                // The eye's ray in the plane across the bead, out along this face's side and up:
                // the bead's (`vertex`), mirrored on its left; a tipped bead's own at each pixel,
                // its faces leaning into the tip.
                let tipped = section > 1.5
                var d = self.v_ray
                if self.v_across < 0.0 {
                    d.x = 0.0 - d.x
                }
                if tipped {
                    let vp = self.view_point()
                    let px = dFdx(vp)
                    let py = dFdy(vp)
                    var level = vec4(0.0, 0.0, 1.0, 0.0)
                    if self.u_space.x > 0.5 {
                        level = vec4(0.0, 1.0, 0.0, 0.0)
                    }
                    let up = normalize((self.draw_pass.camera_view * level).xyz)
                    // From the eye in perspective (w rises along the view's axis), down that axis
                    // in an orthographic view.
                    let persp = clamp((self.draw_pass.camera_projection * vec4(0.0, 0.0, -1.0, 0.0)).w, 0.0, 1.0)
                    let r = normalize(vp * persp + vec3(0.0, 0.0, persp - 1.0))
                    let c = cross(px, py)
                    var nf = c / max(length(c), 0.000000000000000000000000000001)
                    if dot(nf, r) > 0.0 {
                        nf = 0.0 - nf
                    }
                    let level_n = nf - up * dot(nf, up)
                    let outward = level_n / max(length(level_n), 0.000001)
                    d = vec2(dot(r, outward), dot(r, up))
                    d = d / max(length(d), 0.000001)
                }
                var e = vec2(d.y, 0.0 - d.x)
                if e.y < 0.0 {
                    e = 0.0 - e
                }
                // (lengths in the section's half heights; a tipped bead's code is 2 up)
                var cs = (section - 0.25) * 32.0
                if tipped {
                    cs = (section - 2.25) * 32.0
                }
                let ru = 1.0
                let cu = 1.3
                var hi = vec2(0.0, cu)
                var mid = vec2(cs, 0.0)
                if d.x > 0.0 {
                    mid = vec2(0.0 - cs, 0.0)
                }
                if cs * abs(e.x) > cu * e.y {
                    hi = vec2(cs, 0.0)
                    if e.x < 0.0 {
                        hi = vec2(0.0 - cs, 0.0)
                    }
                    mid = vec2(0.0, cu)
                    if d.y > 0.0 {
                        mid = vec2(0.0, 0.0 - cu)
                    }
                }
                let lo = 0.0 - hi
                let at = vec3(dot(lo, e), dot(mid, e), dot(hi, e))
                let level_m = max(length(m.xy), 0.00000000000000000001)
                let h = m.xy / level_m
                // Each neighbour hides this bead's front past its crease: the side it lies to
                // across the rays (the one above higher, unless the stack leans over the eye).
                // `vertex` reads them once for the bead, the same on either side of it (its
                // section mirrors there); a tipped bead's here, from its steps.
                var top = at.z
                var bottom = at.x
                var stacked_top = self.v_stack.x < 4096.0
                var stacked_bottom = self.v_stack.y > -4096.0
                if tipped {
                    let z = vec3(dot(lo, d), dot(mid, d), dot(hi, d))
                    let k = vec2((z.y - z.x) / max(at.y - at.x, 0.000001), (z.z - z.y) / max(at.z - at.y, 0.000001))
                    let qa = vec2(self.v_stack.x / level_m, 2.0)
                    let qb = vec2(self.v_stack.y / level_m, -2.0)
                    let above = abs(qa.x) < 1000.0
                    let below = abs(qb.x) < 1000.0
                    let pa = dot(qa, e)
                    let pb = dot(qb, e)
                    if above && pa > 0.0 {
                        top = min(top, self.crease(qa, z, k, at, e, d))
                    }
                    if above && pa < 0.0 {
                        bottom = max(bottom, pa + self.crease(0.0 - qa, z, k, at, e, d))
                    }
                    if below && pb > 0.0 {
                        top = min(top, self.crease(qb, z, k, at, e, d))
                    }
                    if below && pb < 0.0 {
                        bottom = max(bottom, pb + self.crease(0.0 - qb, z, k, at, e, d))
                    }
                    stacked_top = (above && pa > 0.0) || (below && pb > 0.0)
                    stacked_bottom = (above && pa < 0.0) || (below && pb < 0.0)
                } else {
                    if stacked_top {
                        top = min(top, self.v_stack.x)
                    }
                    if stacked_bottom {
                        bottom = max(bottom, self.v_stack.y)
                    }
                }
                // This pixel across the rays, and the rays a pixel takes.
                let here = dot(vec2(cs * (1.0 - abs(u) / 1.3), u * ru), e)
                var rise = vec2(0.0 - cs / 1.3, ru)
                if u < 0.0 {
                    rise = vec2(cs / 1.3, ru)
                }
                let wide = abs(dot(rise, e)) * du
                var a = here - wide
                var b = here + wide
                // Past an end with no bead on it the stack ends: the pixels read only what is there.
                if !stacked_top {
                    b = min(b, top)
                }
                if !stacked_bottom {
                    a = max(a, bottom)
                }
                var split = bottom
                if here > (top + bottom) * 0.5 {
                    split = top
                }
                split = clamp(split, a, b)
                if top - bottom <= 2.0 * wide || self.whole_front() {
                    a = bottom
                    b = top
                    split = clamp(at.y, a, b)
                }
                let span = max(b - a, 0.000001)
                let period = max(top - bottom, 0.000001)
                // The front's normal is linear along each facet (its level share the place across
                // over the half width, its height the place up over 1.3 half heights), and so is
                // a joint's swing of it: the normals at the outline's points, mixed.
                let swing = vec2(0.0 - w, w)
                var nlo = vec3(h * (lo.x / cs), lo.y / 1.3)
                var nmid = vec3(h * (mid.x / cs), mid.y / 1.3)
                var nhi = vec3(h * (hi.x / cs), hi.y / 1.3)
                nlo = nlo + vec3(nlo.yx * swing, 0.0)
                nmid = nmid + vec3(nmid.yx * swing, 0.0)
                nhi = nhi + vec3(nhi.yx * swing, 0.0)
                let ia = 1.0 / max(at.y - at.x, 0.000001)
                let ib = 1.0 / max(at.z - at.y, 0.000001)
                // Each side's two Gauss points, at a sixth of its root-three width either side of
                // its middle.
                let c1 = (a + split) * 0.5
                let g1 = (split - a) * 0.2886751
                let c2 = (split + b) * 0.5
                let g2 = (b - split) * 0.2886751
                let wa = (split - a) / span * 0.5
                let wb = (b - split) / span * 0.5
                if abs(self.v_across) > 1.5 {
                    // A far bead that is no wall: near shades it per fragment, so its pixel is
                    // the curve's mean over its samples, and the curve bends most where a
                    // front's dark part sits, which no mean and spread of its light hold. Each
                    // side's three Gauss points (its middle and root three-fifths of its half
                    // width either side, weighed 8, 5 and 5 ninths of its share) are each lit
                    // and put on the display, then averaged in linear light.
                    let h1 = (split - a) * 0.3872983
                    let h2 = (b - split) * 0.3872983
                    let s1 = self.tap_shown(self.tap(c1, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l), albedo)
                    let s2 = self.tap_shown(self.tap(c1 - h1, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l), albedo)
                    let s3 = self.tap_shown(self.tap(c1 + h1, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l), albedo)
                    let s4 = self.tap_shown(self.tap(c2, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l), albedo)
                    let s5 = self.tap_shown(self.tap(c2 - h2, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l), albedo)
                    let s6 = self.tap_shown(self.tap(c2 + h2, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l), albedo)
                    shown = (s1 * 0.8888889 + (s2 + s3) * 0.5555556) * wa + (s4 * 0.8888889 + (s5 + s6) * 0.5555556) * wb
                } else {
                    let t1 = self.tap(c1 - g1, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l)
                    let t2 = self.tap(c1 + g1, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l)
                    let t3 = self.tap(c2 - g2, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l)
                    let t4 = self.tap(c2 + g2, top, bottom, period, at, ia, ib, nlo, nmid, nhi, l)
                    let taps = (t1 + t2) * wa + (t3 + t4) * wb
                    // `light` over the taps: its hemisphere is linear in the normal's height (that
                    // never leaves -1..1), so it takes the taps' mean height once; the sun's Lambert
                    // stops at the terminator, so each tap takes its own.
                    let height = taps.x
                    let sun = taps.y
                    sky = mix(self.u_sun_ground.xyz, self.u_sun_sky.xyz, clamp(height * 0.5 + 0.5, 0.0, 1.0)) + self.u_sun_color.xyz * sun
                }
            } else {
                sky = self.light(normalize(m + vec3(0.0 - m.y, m.x, 0.0) * w), l)
            }
            // Scene light for the composite's curve, else mapped here. A far bead's mean of
            // display colours in linear light, encoded once: on the HDR lane the composite leaves
            // it as it is (w 2).
            if shown.x >= 0.0 {
                let encoded = pow(shown, vec3(0.4545454, 0.4545454, 0.4545454))
                if self.u_sun_color.w < 0.5 {
                    return vec4(encoded, 2.0)
                }
                return vec4(encoded, 1.0)
            }
            let lit = albedo * sky * self.u_sun_dir.w
            if self.u_sun_color.w < 0.5 {
                return vec4(lit, 1.0)
            }
            return vec4(pow(self.tone(lit), vec3(0.4545454, 0.4545454, 0.4545454)), 1.0)
        }

        // A far bead's tap (`tap`: its normal's height and the sun's Lambert) lit, as `shade`
        // lights a front, and on the display in linear light, where a pixel's samples average:
        // the HDR lane's curve (`display`, encoded) decoded, else this shader's own (`tone`,
        // linear before the encode).
        tap_shown: fn(t: vec2f, albedo: vec3f) -> vec3f {
            let hemi = mix(self.u_sun_ground.xyz, self.u_sun_sky.xyz, clamp(t.x * 0.5 + 0.5, 0.0, 1.0))
            let lit = albedo * (hemi + self.u_sun_color.xyz * t.y) * self.u_sun_dir.w
            if self.u_sun_color.w < 0.5 {
                return pow(self.display(lit), vec3(2.2, 2.2, 2.2))
            }
            return self.tone(lit)
        }

        // The HDR lane's display curve (`FabDisplay`): the far aux takes the composite's; no
        // other draw shades a far bead on that lane.
        display: fn(c: vec3f) -> vec3f {
            return c
        }

        // The filmic curve `shade` maps light with where the composite does not.
        tone: fn(lit: vec3f) -> vec3f {
            return clamp(
                (lit * (lit * 2.51 + vec3(0.03, 0.03, 0.03)))
                    / (lit * (lit * 2.43 + vec3(0.59, 0.59, 0.59)) + vec3(0.14, 0.14, 0.14)),
                vec3(0.0, 0.0, 0.0),
                vec3(1.0, 1.0, 1.0)
            )
        }

        pixel: fn() -> vec4f {
            return self.shade()
        }

        // The bead into the G-buffer, shaded there so no other pass draws it (`DrawFabPathAux`,
        // `DrawFabFarAux`): view distance, the lit colour in place of the normal, the print id. A
        // negative y below -1.5, which no octahedral normal reaches, marks the colour: r, g and b
        // in 12 bits each of their square root (0..16), r and g in y, b in z, plus 4096 where
        // it is on the display already (a far bead's, `shade`), stored times 16 so the code
        // spends its whole range on 0..1. The composite decodes it (`bead_lit`, `bead_shown`).
        aux_pixel: fn() -> vec4f {
            let c = self.shade()
            var v = max(c.xyz, vec3(0.0, 0.0, 0.0))
            var shown = 0.0
            if c.w > 1.5 {
                v = v * 16.0
                shown = 4096.0
            }
            let e = floor(clamp(sqrt(v) * 0.25, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0)) * 4095.0 + vec3(0.5, 0.5, 0.5))
            return vec4(length(self.view_point()), 0.0 - 2.0 - (e.x * 4096.0 + e.y), e.z + shown, 16000008.0 + 8.0 * floor((self.v_color.y + 0.5) * 0.25))
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // The issue frame uses the preview's geometry and projection, blended over the
    // composite so its fade never changes bead depth, picking, or look-ahead.
    mod.draw.DrawFabIssueSpot = mod.std.set_type_default() do #(DrawFabIssueSpot::script_shader(vm)){
        ..mod.draw.DrawFabPath
        alpha_blend: true
        depth_write: false
        u_opacity: uniform(1.0)
        pixel: fn() -> vec4f {
            return vec4(self.bead_colour().xyz * self.u_opacity, self.u_opacity)
        }
    }

    // The headset's beads: their fronts and caps only (`fab_strip_geom`), turned to the eye by
    // `facing_corner`; the rest of the shader is the tube's.
    mod.draw.DrawFabStrip = mod.std.set_type_default() do #(DrawFabStrip::script_shader(vm)){
        ..mod.draw.DrawFabPath
        geom: vertex_buffer(mod.fab_print_geom.FabVertex, mod.fab_print_geom.FabStripGeom)
        // The caps' normal (`cap_out`), and where a pixel is on a cap: the far corner's share
        // (x), non-zero on the caps' triangles (they all hold it), zero on the facets' (none
        // does), and that share times the cap's swing (y), the same at all its corners.
        v_cap: varying(vec2f)
        v_cap_normal: varying(vec3f)
        cap_out: fn(n: vec3f, joint: vec2f) {
            let on = max(self.geom.pos.y - 1.0, 0.0)
            self.v_cap_normal = n
            self.v_cap = vec2(on, on * self.swing(joint))
        }
        normal_here: fn() -> vec3f {
            if abs(self.v_cap.x) > 0.0 {
                return self.v_cap_normal
            }
            return self.v_normal
        }
        swing_here: fn() -> float {
            if abs(self.v_cap.x) > 0.0 {
                return self.v_cap.y / self.v_cap.x
            }
            return self.swing(self.v_joint)
        }
        // The strip (`strip_vertices`): of the section's corners (0 right, 1 up, 2 left, 3
        // down, as the tube's ring), the one between the two facets that face the eye, and
        // this vertex's corner round from it. A facet faces the eye where its outward normal
        // (the right-up one along (cu, cs)) leans to the eye's side of the section: the eye's
        // direction across the bead (`to`, in side and up) decides one of right-up and
        // left-down and one of up-left and down-right, and the two share the corner.
        facing_corner: fn(g: vec3f, mid: vec3f, side: vec3f, up: vec3f, cs: float, cu: float) -> vec3f {
            let to = self.eye_across(mid, side, up)
            let across = cu * to.x
            let rising = cs * to.y
            var near = 3.0
            if across + rising > 0.0 {
                near = 0.0
                if rising > across {
                    near = 1.0
                }
            } else if rising > across {
                near = 2.0
            }
            // its corner: (cos, sin) of a quarter turn's multiple, exactly
            let k = near + g.y + 4.0
            let m = k - 4.0 * floor(k * 0.25 + 0.001)
            let ms = m + 3.0 - 4.0 * floor((m + 3.0) * 0.25 + 0.001)
            return vec3(g.x, 1.0 - min(m, 4.0 - m), 1.0 - min(ms, 4.0 - ms))
        }
    }

    // The beads a pixel or less across (`FarTiers`): one quad each (`fab_quad_geom`),
    // across the eye's rays between its section's two outline corners, where the strip draws
    // the facets between them and the caps. It covers what they cover, past the ends by half a
    // width for the caps, and shows what a pixel of them reads: a wall's whole front, as
    // `shade` reads it for a bead two pixels high or less (`whole_front`), on the side the eye
    // is; any other bead's facets that face the eye, each its corners' mean normal by how wide
    // it shows.
    mod.draw.DrawFabFar = mod.std.set_type_default() do #(DrawFabFar::script_shader(vm)){
        ..mod.draw.DrawFabPath
        backface_culling: false
        geom: vertex_buffer(mod.fab_print_geom.FabVertex, mod.fab_print_geom.FabQuadGeom)
        // The outline's corners across the eye's rays (`e`, rising): the top and the bottom
        // one, or the right and the left one where they reach further (as `shade` takes them).
        // A wall's quads stand in one plane up its stack, so each stops a hair (the tube's seal)
        // past where the next one starts, half a layer from its middle (where it creases with a
        // neighbour in line): two instances share no edge, so the raster covers the seam from one
        // side or the other. Not at its corner past it: that overlap the two would fight over.
        facing_corner: fn(g: vec3f, mid: vec3f, side: vec3f, up: vec3f, cs: float, cu: float) -> vec3f {
            let to = self.eye_across(mid, side, up)
            var e = vec2(0.0 - to.y, to.x)
            if e.y < 0.0 {
                e = 0.0 - e
            }
            if cs * abs(e.x) > cu * e.y {
                return vec3(g.x, sign(e.x) * g.y, 0.0)
            }
            let stacked = self.stacked()
            var reach = 1.0
            if (g.y > 0.0 && stacked.x > 0.5) || (g.y < 0.0 && stacked.y > 0.5) {
                reach = 1.02 / 1.3
            }
            return vec3(g.x, 0.0, g.y * reach)
        }
        // Whether a wall's bead has a neighbour drawn on it (x) and under it (y), from its steps
        // and its layer as `vertex` reads them.
        stacked: fn() -> vec2f {
            let lt = floor(unpack4u8(self.seg_k.y) * 255.0 + 0.5)
            let rk = floor(unpack4u8(self.seg_k.z) * 255.0 + 0.5)
            let layer = lt.x + 256.0 * lt.y - 1.0
            let role_steps = rk.x + 256.0 * (rk.y + 256.0 * rk.z)
            let role = role_steps - 32.0 * floor(role_steps / 32.0 + 0.00001)
            let xmode = floor(rk.w / 8.0 + 0.001)
            let steps = floor(role_steps / 32.0 + 0.00001)
            let above = floor(steps / 256.0 + 0.00001)
            let below = steps - 256.0 * above
            var on = vec2(0.0, 0.0)
            if rk.w - 8.0 * xmode < 0.5 && role > 0.5 && role < 3.5 {
                if above > 0.5 && layer < self.u_layers.w - 0.5 {
                    on.x = 1.0
                }
                if below > 0.5 && layer > self.u_layers.x + 0.5 {
                    on.y = 1.0
                }
            }
            return on
        }
        end_reach: fn(rs: float) -> float {
            return rs
        }
        // A printed bead's (`wall`, its section): its side the eye is on, level (`shade` reads
        // its whole front in that side's plane). A travel or overlay line's: the facets'
        // (outward along (+-cu, +-cs) in the right and up) that face the eye, each by how wide
        // it shows across the rays (its outward normal along the eye's direction) times its
        // corners' sum.
        seen_normal: fn(n: vec3f, mid: vec3f, side: vec3f, up: vec3f, cs: float, cu: float, wall: float) -> vec3f {
            let to = self.eye_across(mid, side, up)
            if wall > 0.0 {
                if to.x < 0.0 {
                    return 0.0 - side
                }
                return side
            }
            let ru = max(cu * to.x + cs * to.y, 0.0)
            let lu = max(cs * to.y - cu * to.x, 0.0)
            let ld = max(0.0 - cu * to.x - cs * to.y, 0.0)
            let rd = max(cu * to.x - cs * to.y, 0.0)
            return side * (ru - lu - ld + rd) + up * (ru + lu - ld - rd)
        }
        across_here: fn(g: vec3f, n: vec3f, side: vec3f) -> float {
            return sign(dot(n, side))
        }
        // Every corner of the quad is on the side the eye sees (`seen_normal`'s): an end's two
        // corners carry one joint, so nothing a pixel reads changes across the quad, whose plate
        // is a pixel or less high and is sampled past its edges.
        joint_side: fn(g: vec3f, mid: vec3f, side: vec3f, up: vec3f) -> float {
            if self.eye_across(mid, side, up).x < 0.0 {
                return -1.0
            }
            return 1.0
        }
        whole_front: fn() -> bool {
            return true
        }
    }

    // The same beads into the G-buffer (`aux_pixel`): the desktop's near tier ...
    mod.draw.DrawFabPathAux = mod.std.set_type_default() do #(DrawFabPathAux::script_shader(vm)){
        ..mod.draw.DrawFabPath
        color_format: @Rgba32F

        pixel: fn() -> vec4f {
            return self.aux_pixel()
        }
    }

    // The HDR lane's display curve in one home: the exposure, the white balance and the shared
    // AgX look. The composite puts the scene's light on the display with it, the far aux its
    // beads' taps (`tap_shown`), both from the same values (`bind_display` in mod.rs).
    mod.draw.FabDisplay = {
        ..mod.draw.ToneCurves,
        // x = exposure, y = 1 when the light is linear scene light (the renderer's HDR lane),
        // z = the tone curve's contrast (1 = the stock look); the composite reads w = 1 as its
        // dither left out (a debug switch)
        u_exposure: uniform(vec4(1.0, 0.0, 1.0, 0.0))
        // the white balance on the scene light before the exposure (1, 1, 1: as it is)
        u_balance: uniform(vec3(1.0, 1.0, 1.0))
        // Scene light to display: exposure and the shared AgX look (HDR lane); the legacy
        // lane's lit image is display-encoded already.
        display: fn(c: vec3f) -> vec3f {
            if self.u_exposure.y < 0.5 {
                return c
            }
            return self.agx_look_fit(c * self.u_balance * self.u_exposure.x, self.u_exposure.z, 1.0)
        }
    }

    // ... and its far one (`FarTiers`): the headset's quads, shaded as they are, into the same
    // G-buffer, a far bead's taps on the display already (`FabDisplay`).
    mod.draw.DrawFabFarAux = mod.std.set_type_default() do #(DrawFabFarAux::script_shader(vm)){
        // first: a spread adds only the names not there yet, so this `display` is the one kept,
        // not the far shader's stand-in
        ..mod.draw.FabDisplay,
        ..mod.draw.DrawFabFar,
        color_format: @Rgba32F

        pixel: fn() -> vec4f {
            return self.aux_pixel()
        }
    }

    // The nozzle heads: their own shader types so their per-frame instances
    // never join the retained beads' draw call.
    mod.draw.DrawFabHead = mod.std.set_type_default() do #(DrawFabHead::script_shader(vm)){
        ..mod.draw.DrawFabPath
        backface_culling: false
        geom: vertex_buffer(mod.fab_print_geom.FabVertex, mod.fab_print_geom.FabHeadGeom)
    }
    mod.draw.DrawFabHeadAux = mod.std.set_type_default() do #(DrawFabHeadAux::script_shader(vm)){
        ..mod.draw.DrawFabPathAux
        backface_culling: false
        geom: vertex_buffer(mod.fab_print_geom.FabVertex, mod.fab_print_geom.FabHeadGeom)
    }
}

macro_rules! path_draw {
    ($name:ident) => {
        #[derive(Script, ScriptHook, Debug)]
        #[repr(C)]
        pub struct $name {
            #[deref]
            pub draw_vars: DrawVars,
            #[live]
            pub seg_a: Vec4f,
            #[live]
            pub seg_b: Vec4f,
            #[live]
            pub seg_k: Vec4f,
        }
    };
}
path_draw!(DrawFabPath);
path_draw!(DrawFabStrip);
path_draw!(DrawFabFar);
path_draw!(DrawFabPathAux);
path_draw!(DrawFabFarAux);
path_draw!(DrawFabIssueSpot);
path_draw!(DrawFabHead);
path_draw!(DrawFabHeadAux);

/// The preview's palette texture (one per viewport, rebuilt per preview).
#[derive(Default)]
pub struct PrintPalette {
    texture: Option<Texture>,
    markers: Option<Texture>,
    /// The parts row (`PrintFrame::parts`) as uploaded, and what it holds.
    parts: Option<Texture>,
    parts_data: Vec<[f32; 4]>,
    /// The overlays' parts row: one empty texel.
    no_parts: Option<Texture>,
    /// Per part (texel 1 + n): its toolpaths are out of date; and the parts it marks.
    faded: Option<(Texture, Arc<[u16]>)>,
    /// One texel, nothing marked: the overlays' and an all-or-nothing frame's.
    unmarked: Option<Texture>,
    /// The part offsets row as uploaded (`PrintPreview::part_offsets`).
    offsets: Option<(Texture, Arc<[[f32; 2]]>)>,
    built: u64,
    drawn: Option<u64>,
}

impl PrintPalette {
    /// This palette's first draw of a preview tells the app.
    fn notify_drawn(&mut self, frame: &PrintFrame) {
        if self.drawn != Some(frame.preview.id) {
            self.drawn = Some(frame.preview.id);
            crate::print::drawn(frame.preview.id);
        }
    }

    fn sync(&mut self, cx: &mut Cx, frame: &PrintFrame) -> &Texture {
        let preview = &frame.preview;
        if self.texture.is_none() || self.built != preview.id {
            self.built = preview.id;
            let data = frame.palette();
            self.texture = Some(Texture::new_with_format(
                cx,
                TextureFormat::VecRGBAf32 { width: palette::WIDTH, height: 1, data: Some(data), updated: TextureUpdated::Full },
            ));
        }
        self.texture.as_ref().unwrap()
    }

    /// The parts row: re-uploaded only when a part's element or selection changes.
    fn sync_parts(&mut self, cx: &mut Cx, parts: &[[f32; 4]]) -> Texture {
        if self.parts.is_none() || self.parts_data != parts {
            self.parts_data = parts.to_vec();
            self.parts = Some(parts_texture(cx, parts));
        }
        self.parts.clone().unwrap()
    }

    /// The overlays' (no parts): an empty row.
    fn no_parts(&mut self, cx: &mut Cx) -> Texture {
        self.no_parts.get_or_insert_with(|| parts_texture(cx, &[])).clone()
    }
    /// The palette and the faded parts' texture for `frame`.
    fn textures(&mut self, cx: &mut Cx, frame: &PrintFrame) -> FrameTextures {
        let palette = self.sync(cx, frame).clone();
        let (faded, faded_width) = match &frame.faded {
            Faded::Parts(parts) => self.faded(cx, parts),
            _ => (self.unmarked(cx), 1),
        };
        let parts = self.sync_parts(cx, &frame.parts);
        let offsets = &frame.preview.part_offsets;
        let offsets_width = offsets.len();
        let offsets = if offsets.is_empty() { self.unmarked(cx) } else { self.offsets(cx, offsets) };
        FrameTextures { palette, faded, faded_width, parts, offsets, offsets_width }
    }

    /// The part offsets row (re-uploaded only when the offsets change).
    fn offsets(&mut self, cx: &mut Cx, offsets: &Arc<[[f32; 2]]>) -> Texture {
        if !self.offsets.as_ref().is_some_and(|(_, of)| Arc::ptr_eq(of, offsets) || **of == **offsets) {
            let data: Vec<f32> = offsets.iter().flat_map(|o| [o[0], o[1], 0.0, 0.0]).collect();
            let texture = Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width: offsets.len(), height: 1, data: Some(data), updated: TextureUpdated::Full });
            self.offsets = Some((texture, offsets.clone()));
        }
        self.offsets.as_ref().unwrap().0.clone()
    }

    /// The texture marking `parts` (built when they change), with its width.
    fn faded(&mut self, cx: &mut Cx, parts: &Arc<[u16]>) -> (Texture, usize) {
        if !self.faded.as_ref().is_some_and(|(_, of)| of == parts) {
            let width = parts.iter().map(|&p| p as usize + 2).max().unwrap_or(1);
            let mut data = vec![0.0f32; width * 4];
            for &p in parts.iter() {
                data[(p as usize + 1) * 4] = 1.0;
            }
            let texture = Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width, height: 1, data: Some(data), updated: TextureUpdated::Full });
            self.faded = Some((texture, parts.clone()));
        }
        let width = parts.iter().map(|&p| p as usize + 2).max().unwrap_or(1);
        (self.faded.as_ref().unwrap().0.clone(), width)
    }

    fn unmarked(&mut self, cx: &mut Cx) -> Texture {
        self.unmarked.get_or_insert_with(|| {
            Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width: 1, height: 1, data: Some(vec![0.0; 4]), updated: TextureUpdated::Full })
        }).clone()
    }
}

fn parts_texture(cx: &mut Cx, parts: &[[f32; 4]]) -> Texture {
    let mut data: Vec<f32> = parts.iter().flatten().copied().collect();
    if data.is_empty() {
        data = vec![0.0; 4];
    }
    let width = data.len() / 4;
    Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width, height: 1, data: Some(data), updated: TextureUpdated::Full })
}

/// A frame's textures: its palette and which parts are faded.
struct FrameTextures {
    palette: Texture,
    faded: Texture,
    faded_width: usize,
    /// The parts row (`PrintFrame::parts`).
    parts: Texture,
    /// The part offsets row and its width (0: none, the beads are placed).
    offsets: Texture,
    offsets_width: usize,
}

/// What of a shown slice is out of date.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Faded {
    #[default]
    None,
    All,
    /// These parts (plate indices).
    Parts(Arc<[u16]>),
}

/// What the viewport draws of the print this frame.
pub struct PrintFrame {
    pub preview: Arc<PrintPreview>,
    /// The beads started by the playhead ([`PrintPreview::cut_at`]).
    pub cut: RunCut,
    /// The travel instances drawn: those started by the playhead while Travel shows, else none.
    pub travel_count: usize,
    pub uniforms: [(LiveId, [f32; 4]); 4],
    /// The heads to draw (none while no print runs at the playhead).
    pub heads: Vec<Head>,
    pub show_markers: bool,
    pub hovered_marker: Option<usize>,
    /// Non-planar tops drawn as the planar terraces they replace.
    pub compare_planar: bool,
    /// The supports' beads are drawn (Show supports); off, the shader leaves them out.
    pub show_supports: bool,
    /// The slice is out of date there: drawn faded.
    pub faded: Faded,
    /// Per plate part: its element + 1 (0: none) and selection state, so a part picked under
    /// the slice is outlined on its beads.
    pub parts: Vec<[f32; 4]>,
    /// The hovered and picked branches' own beads, lit (`print::trees::lit_beads`): drawn over
    /// the beads they light.
    pub lit: Option<RetainedInstances>,
    /// Per part (the beads' part index): it is closed in this frame ([`PrintFrame::closed_parts`]),
    /// so its core's blocks (`BeadStream::Core`) are not drawn, nor its lining's
    /// (`BeadStream::Lining`) where the tiers ([`FarTiers`]) draw them far or, for the headset's
    /// eyes, find them thin.
    /// None: every core is drawn.
    pub closed: Vec<bool>,
}

/// A layer shown on top stands out (its beads lightened, the model's outline drawn at it) only
/// below the print's last layer: at the last one, by the slider or at the end of playback, the
/// view shows the whole print and no layer leads.
pub(super) fn layer_leads(shown: u32, preview: &PrintPreview) -> bool {
    shown < preview.max_layer()
}

/// The model's outline over the slice: the live contours at the slider's top layer, while they
/// last (until the toolpaths come), where they have that layer and it leads ([`layer_leads`]).
pub fn outline_at<'a>(state: &'a AppState, frame: Option<&PrintFrame>) -> Option<(&'a crate::print::LiveContours, u32)> {
    let layer = state.print.play.layer_hi;
    if !layer_leads(layer, &frame?.preview) {
        return None;
    }
    state.print.contours().filter(|l| l.contours.layer(layer).next().is_some()).map(|l| (l, layer))
}

impl PrintFrame {
    /// `u_layers`: the lowest and highest layer shown, 1 + the layer that leads (0: none), and
    /// the highest layer drawn whole at playhead `t`: below the lowest one still printing then
    /// (whatever order the parts print in), at most the highest shown. The shader leaves out a
    /// wall bead's stacked neighbour on any layer above it.
    fn layer_uniform(preview: &PrintPreview, lo: u32, hi: u32, t: f64, lit: f32) -> [f32; 4] {
        let hi = hi.min(preview.max_layer());
        let printing = preview.layers.iter().filter(|l| l.t_end > t).map(|l| l.layer).min();
        let whole = printing.map_or(hi as f32, |l| (l as f32 - 1.0).min(hi as f32));
        [lo as f32, hi as f32, lit, whole]
    }

    /// Which parts are closed in, by `u_layers` (`layers`) and the non-planar comparison: a part
    /// is while the shader draws all of it, the lowest layer shown at or under its first and the
    /// highest drawn whole at or over its last, and none of its beads it hides opens it (the
    /// comparison takes the non-planar tops away). Its core is then inside its walls and skins
    /// from every side. Per frame: one pass over the parts.
    fn closed_parts(preview: &PrintPreview, layers: [f32; 4], compare_planar: bool, closed: &mut Vec<bool>) {
        closed.clear();
        closed.extend(preview.part_layers.iter().map(|l| !compare_planar && layers[0] < l[0] + 0.5 && layers[3] > l[1] - 0.5));
    }

    /// `u_layers`: the lowest and highest layer shown, the highlighted one, the highest drawn whole.
    fn layers(&self) -> [f32; 4] {
        self.uniforms.iter().find(|(id, _)| *id == live_id!(u_layers)).map_or([0.0, 1e9, 0.0, 1e9], |(_, v)| *v)
    }

    /// The lowest and highest layer shown (`u_layers`).
    fn layer_range(&self) -> [f32; 2] {
        let layers = self.layers();
        [layers[0], layers[1]]
    }

    /// Shared by the desktop and XR textures: the job's materials in their trays' colours.
    pub fn palette(&self) -> Vec<f32> {
        self.preview.palette()
    }

    /// Reuse the previous frame's head and part buffers.
    pub fn of(state: &AppState, previous: Option<PrintFrame>) -> Option<PrintFrame> {
        let preview = state.slice_shown()?.clone();
        if !preview.has_beads() {
            return None;
        }
        let (mut heads, mut parts, mut closed) = previous.map_or_else(|| (Vec::new(), Vec::new(), Vec::new()), |f| (f.heads, f.parts, f.closed));
        let play = &state.print.play;
        // Monitor's job: the head is where the printer is, wherever the view looks, and following
        // the layer it prints shows whole so the head moves along what is still to print there.
        let job = state.print.monitor_preview().is_some();
        let live = if job { state.print.follow.live } else { play.time };
        let t = match state.print.following().and(state.print.follow.shown_printing()) {
            Some(j) if j.layer > 0 => play.time.max(j.limit),
            _ => play.time,
        };
        // The job's toolpaths, whatever the plate's slice view is set to.
        let finish = state.print.finish && !job;
        let started = preview.cut_at(t);
        let travel_count = if play.travel && !finish { preview.travel_count_at(t) } else { 0 };
        let cut = preview.timeline.now(t);
        let tip = cut.nozzles[cut.active.min(1)];
        let now = if live == t { cut } else { preview.timeline.now(live) };
        // The layer at the playhead leads (1 + its number), the ones below step back.
        let current = crate::print::timeline::shown_top_layer(&state.print, &preview);
        let lit = if layer_leads(current, &preview) { current as f32 + 1.0 } else { 0.0 };
        // A live slice's tiles show whole, whenever their beads print.
        let t_shown = if preview.chunks.is_empty() { t as f32 } else { 1e9 };
        let layers = Self::layer_uniform(&preview, play.layer_lo, play.layer_hi, t, lit);
        let compare_planar = play.compare_planar && preview.nonplanar.is_some();
        Self::closed_parts(&preview, layers, compare_planar, &mut closed);
        let uniforms = [
            (live_id!(u_now), [t_shown, tip[0], tip[1], tip[2]]),
            (live_id!(u_layers), layers),
            // The printed surface: the outside of what prints, in its filament (`SURFACE_MODE`).
            (live_id!(u_mode), [if finish { SURFACE_MODE } else { play.color.shader_mode() }, preview.speed_range[0], preview.speed_range[1], preview.max_layer() as f32]),
            (live_id!(u_live), [if job { live as f32 } else { 1e9 }, if job { 1.0 } else { 0.0 }, 0.0, 0.0]),
        ];
        // Both heads while the print is running (the idle one where the
        // timeline says it waits).
        heads.clear();
        // A job that is over has no head to show.
        if live > 0.0 && live < preview.total && (!job || state.print.follow.shown_printing().is_some()) {
            heads.extend((0..2).map(|i| Head { tip: now.nozzles[i], nozzle: i, active: i == now.active }));
        }
        // The job's beads answer to the job, not to the plate's parts.
        if job { parts.clear(); } else { part_states(state, &preview.part_objects, &mut parts); }
        Some(PrintFrame {
            preview, cut: started, travel_count, uniforms, heads,
            // An old preview must not expose issue indices from a new report.
            show_markers: !job && !state.print.hide_markers && !state.print.stale,
            hovered_marker: None,
            compare_planar,
            show_supports: play.supports,
            // The parts an edit changed fade, the others are as they print. A slice streaming in
            // marks its own out-of-date layers (drawn as ghosts), its new ones show as they are.
            faded: match &state.print.stale_parts {
                _ if job || !state.print.stale || state.print.streaming => Faded::None,
                Some(p) if p.is_empty() => Faded::None,
                Some(p) => Faded::Parts(p.clone()),
                None => Faded::All,
            },
            parts,
            lit: None,
            closed,
        })
    }
}

/// [`PrintFrame::parts`] of the plate as it is.
fn part_states(state: &AppState, objects: &[String], parts: &mut Vec<[f32; 4]>) {
    use super::elements::{STATE_ACTIVE, STATE_SELECTED, STATE_VISIBLE};
    let selection = &state.scene_state.selection;
    parts.clear();
    parts.extend(objects.iter().map(|object| {
        let Some(p) = state.plate.parts.iter().find(|p| p.object == *object) else { return [0.0; 4] };
        let off = if p.printed { 0.0 } else { 1.0 };
        match p.element {
            Some(e) => {
                let st = if selection.active == Some(e) { STATE_ACTIVE } else if selection.contains(e) { STATE_SELECTED } else { STATE_VISIBLE };
                [e.0 as f32 + 1.0, st, off, 0.0]
            }
            None => [0.0, 0.0, off, 0.0],
        }
    }));
}

/// A Y-up, metre-space placement: `room = R_y(yaw) (scale * point) + origin`.
/// The plate renderer and the forward print pass use the same navigation view.
#[derive(Clone, Copy, Debug)]
pub struct PrintPlacement {
    pub origin: Vec3f,
    pub scale: f32,
    pub yaw: f32,
}

impl PrintPlacement {
    pub const IDENTITY: Self = Self { origin: Vec3f { x: 0.0, y: 0.0, z: 0.0 }, scale: 1.0, yaw: 0.0 };
}

/// The palette and the parts row into the slots the shader's mapping gives them (by name: a
/// slot's index is the mapping's, which need not follow the declaration order).
fn bind_textures(vars: &mut DrawVars, cx: &mut Cx, palette: &Texture, faded: &Texture, parts: &Texture, offsets: &Texture) {
    let Some(shader) = vars.draw_shader_id else { return };
    let slots: Vec<LiveId> = cx.draw_shaders[shader.index].mapping.textures.iter().map(|t| t.id).collect();
    for (i, id) in slots.iter().enumerate() {
        if *id == live_id!(palette) {
            vars.set_texture(i, palette);
        } else if *id == live_id!(faded_parts) {
            vars.set_texture(i, faded);
        } else if *id == live_id!(parts) {
            vars.set_texture(i, parts);
        } else if *id == live_id!(part_offsets) {
            vars.set_texture(i, offsets);
        }
    }
}

/// The parts' light rig onto a bead draw: `light` is in the lit pass's Y-up frame, the shader
/// lights in Fab's Z-up one, (x, y, z) -> (x, -z, y).
pub fn bind_light(vars: &mut DrawVars, cx: &mut Cx, light: &TileLight) {
    let d = light.dir;
    vars.set_uniform(cx, live_id!(u_sun_dir), &[d[0], -d[2], d[1], light.exposure]);
    vars.set_uniform(cx, live_id!(u_sun_color), &[light.color[0], light.color[1], light.color[2], if light.display { 1.0 } else { 0.0 }]);
    vars.set_uniform(cx, live_id!(u_sun_sky), &[light.sky[0], light.sky[1], light.sky[2], 0.0]);
    vars.set_uniform(cx, live_id!(u_sun_ground), &[light.ground[0], light.ground[1], light.ground[2], 0.0]);
}

#[allow(clippy::too_many_arguments)]
fn bind(vars: &mut DrawVars, cx: &mut CxDraw, frame: &PrintFrame, tex: &FrameTextures, lit: bool, head: bool, place: PrintPlacement, light: &TileLight) {
    bind_target(vars, cx);
    bind_textures(vars, cx, &tex.palette, &tex.faded, &tex.parts, &tex.offsets);
    vars.set_uniform(cx, live_id!(u_faded), &[if matches!(frame.faded, Faded::Parts(_)) { 1.0 } else { 0.0 }, tex.faded_width as f32, 0.0, 0.0]);
    vars.set_uniform(cx, live_id!(u_parts), &[frame.parts.len() as f32, 0.0, 0.0, 0.0]);
    vars.set_uniform(cx, live_id!(u_offsets), &[tex.offsets_width as f32, 0.0, 0.0, 0.0]);
    bind_light(vars, cx, light);
    for (id, v) in &frame.uniforms {
        vars.set_uniform(cx, *id, v);
    }
    vars.set_uniform(cx, live_id!(u_space), &[if lit { 1.0 } else { 0.0 }, 0.001, if head { 1.0 } else { 0.0 }, if frame.faded == Faded::All { 1.0 } else { 0.0 }]);
    vars.set_uniform(cx, live_id!(u_place), &[place.origin.x, place.origin.y, place.origin.z, place.scale]);
    vars.set_uniform(cx, live_id!(u_yaw), &[place.yaw.cos(), place.yaw.sin(), 0.0, 0.0]);
    vars.set_uniform(cx, live_id!(u_marker), &[frame.hovered_marker.map(|i| i as f32).unwrap_or(-1.0), if frame.compare_planar { 1.0 } else { 0.0 }, if frame.show_supports { 0.0 } else { 1.0 }, 0.0]);
}

/// The target's size for the shader's `view_point`: the pass being drawn, in its pixels.
fn bind_target(vars: &mut DrawVars, cx: &mut CxDraw) {
    let size = cx.current_pass_size() * cx.current_dpi_factor();
    vars.set_uniform(cx, live_id!(u_px), &[(2.0 / size.x.max(1.0)) as f32, (2.0 / size.y.max(1.0)) as f32, 0.0, 0.0]);
}

/// Model-stage markers use the same retained bead format and pass as the
/// preview. They never enter the document, scene tree, or element BVH.
pub fn draw_markers(cx: &mut Cx3d, markers: &RetainedInstances,
    palette: &mut PrintPalette, hovered: Option<usize>, vars: &mut DrawVars) -> u32 {
    if markers.count() == 0 { return 0; }
    let texture = overlay_palette(cx.cx, palette);
    let parts = palette.no_parts(cx.cx);
    let unmarked = palette.unmarked(cx.cx);
    bind_textures(vars, cx.cx, &texture, &unmarked, &parts, &unmarked);
    vars.set_uniform(cx.cx, live_id!(u_parts), &[0.0, 0.0, 0.0, 0.0]);
    vars.set_uniform(cx.cx, live_id!(u_offsets), &[0.0, 0.0, 0.0, 0.0]);
    vars.set_uniform(cx.cx, live_id!(u_faded), &[0.0, 1.0, 0.0, 0.0]);
    for (id, value) in [
        (live_id!(u_now), [1e9, 0.0, 0.0, 0.0]),
        (live_id!(u_layers), [0.0, 1e9, 0.0, 0.0]),
        (live_id!(u_mode), [0.0, 0.0, 1.0, 1.0]),
        (live_id!(u_space), [0.0, 0.001, 0.0, 0.0]),
        (live_id!(u_place), [0.0, 0.0, 0.0, 1.0]),
        (live_id!(u_yaw), [1.0, 0.0, 0.0, 0.0]),
        (live_id!(u_marker), [hovered.map(|i| i as f32).unwrap_or(-1.0), 0.0, 0.0, 0.0]),
    ] { vars.set_uniform(cx.cx, id, &value); }
    bind_target(vars, cx.cx);
    vars.area = cx.cx.add_retained_instances_count(vars, markers, markers.count());
    1
}

/// Retained bead instances of the overlay kind (issue markers, the live slice's layer contours:
/// kind 3, coloured by its role: 0..2 severity, 3 a contour) in a room placement, layers
/// `lo..=hi` shown, `lit` + 1 highlighted (0: none). The XR view's markers and live slice.
pub fn draw_overlay_placed(cx: &mut Cx3d, instances: &RetainedInstances, palette: &mut PrintPalette, place: PrintPlacement,
    layers: [f32; 3], hovered: Option<usize>, vars: &mut DrawVars) -> u32 {
    if instances.count() == 0 { return 0; }
    let texture = overlay_palette(cx.cx, palette);
    let parts = palette.no_parts(cx.cx);
    let unmarked = palette.unmarked(cx.cx);
    bind_textures(vars, cx.cx, &texture, &unmarked, &parts, &unmarked);
    vars.set_uniform(cx.cx, live_id!(u_parts), &[0.0, 0.0, 0.0, 0.0]);
    vars.set_uniform(cx.cx, live_id!(u_offsets), &[0.0, 0.0, 0.0, 0.0]);
    vars.set_uniform(cx.cx, live_id!(u_faded), &[0.0, 1.0, 0.0, 0.0]);
    for (id, value) in [
        (live_id!(u_now), [1e9, 0.0, 0.0, 0.0]),
        (live_id!(u_layers), [layers[0], layers[1], layers[2], 0.0]),
        (live_id!(u_mode), [0.0, 0.0, 1.0, 1.0]),
        (live_id!(u_space), [1.0, 0.001, 0.0, 0.0]),
        (live_id!(u_place), [place.origin.x, place.origin.y, place.origin.z, place.scale]),
        (live_id!(u_yaw), [place.yaw.cos(), place.yaw.sin(), 0.0, 0.0]),
        (live_id!(u_marker), [hovered.map(|i| i as f32).unwrap_or(-1.0), 0.0, 0.0, 0.0]),
    ] { vars.set_uniform(cx.cx, id, &value); }
    bind_target(vars, cx.cx);
    vars.area = cx.cx.add_retained_instances_count(vars, instances, instances.count());
    1
}

/// The palette of the overlay kind: severities at 54..56, the live contour at 57.
fn overlay_palette(cx: &mut Cx, palette: &mut PrintPalette) -> Texture {
    palette.markers.get_or_insert_with(|| {
        let mut data = vec![0.0; palette::WIDTH * 4];
        for (i, color) in [[1.0, 0.08, 0.08, 1.0], [1.0, 0.65, 0.05, 1.0], [0.15, 0.55, 1.0, 1.0], [0.20, 0.85, 0.95, 1.0]].iter().enumerate() {
            data[(54+i)*4..(55+i)*4].copy_from_slice(color);
        }
        Texture::new_with_format(cx, TextureFormat::VecRGBAf32 {
            width: palette::WIDTH, height: 1, data: Some(data), updated: TextureUpdated::Full,
        })
    }).clone()
}

/// A nozzle head as the playback shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Head {
    /// Its tip, in printer mm.
    pub tip: [f32; 3],
    /// Which nozzle it is (0 left, 1 right): its colour, whatever it does.
    pub nozzle: usize,
    /// It prints now: on the bead, ringed (`HEAD_RING_MM`); the other waits, dimmed.
    pub active: bool,
}

/// The ring round the printing nozzle's tip, on the bead (radius, mm).
pub const HEAD_RING_MM: f32 = 0.625;

// A head's bead: its role 1 while it waits (dimmed), its filament its palette slot.
macro_rules! draw_heads {
    ($d:expr, $cx:expr, $frame:expr) => {
        for h in &$frame.heads {
            let tip = h.tip;
            let s = crate::print::Bead {
                a: tip,
                b: [tip[0], tip[1], tip[2] + HEAD_HEIGHT],
                width: HEAD_WIDTH,
                height: HEAD_WIDTH,
                role: if h.active { 0.0 } else { 1.0 },
                filament: (palette::HEADS + h.nozzle.min(1)) as f32,
                ..Default::default()
            }
            .pack();
            $d.seg_a = vec4(s[0], s[1], s[2], s[3]);
            $d.seg_b = vec4(s[4], s[5], s[6], s[7]);
            $d.seg_k = vec4(s[8], s[9], s[10], s[11]);
            $cx.add_instance(&mut $d.draw_vars);
        }
    };
}

/// The planes of what a camera can see, in the beads' mm (inside: all four terms sum to >= 0).
#[derive(Clone, Copy)]
pub struct BeadView([[f32; 4]; 6]);

impl BeadView {
    pub fn new(view: &Mat4f, projection: &Mat4f) -> BeadView {
        Self::of(view, projection, [vec3f(0.001, 0.0, 0.0), vec3f(0.0, 0.001, 0.0), vec3f(0.0, 0.0, 0.001)], vec3f(0.0, 0.0, 0.0))
    }

    /// One eye of a room placement (the XR pass), as the shader places a lit bead: mm * 0.001,
    /// (x, z, -y), turned by the yaw about y, scaled, moved to the origin.
    pub fn placed(view: &Mat4f, projection: &Mat4f, place: PrintPlacement) -> BeadView {
        let (c, s) = (place.yaw.cos(), place.yaw.sin());
        let turn = |x: f32, y: f32, z: f32| {
            let (x, y, z) = (x, z, -y);
            vec3f(c * x + s * z, y, c * z - s * x) * (0.001 * place.scale)
        };
        Self::of(view, projection, [turn(1.0, 0.0, 0.0), turn(0.0, 1.0, 0.0), turn(0.0, 0.0, 1.0)], place.origin)
    }

    /// The planes from where a bead mm along each axis (`axes`, directions) and the beads'
    /// origin land.
    fn of(view: &Mat4f, projection: &Mat4f, axes: [Vec3f; 3], origin: Vec3f) -> BeadView {
        let clip = |v: Vec3f, w: f32| {
            let c = projection.transform_vec4(view.transform_vec4(vec4(v.x, v.y, v.z, w)));
            [c.x, c.y, c.z, c.w]
        };
        let axes = [clip(axes[0], 0.0), clip(axes[1], 0.0), clip(axes[2], 0.0), clip(origin, 1.0)];
        let plane = |row: usize, sign: f32| [0, 1, 2, 3].map(|k: usize| axes[k][3] + sign * axes[k][row]);
        BeadView([plane(0, 1.0), plane(0, -1.0), plane(1, 1.0), plane(1, -1.0), plane(2, 1.0), plane(2, -1.0)])
    }

    /// The planes for beads in a frame `o` (mm, x and y) off the plate's, as a part's
    /// (`PrintPreview::part_offsets`).
    fn shifted(&self, o: [f32; 2]) -> BeadView {
        BeadView(self.0.map(|p| [p[0], p[1], p[2], p[3] + p[0] * o[0] + p[1] * o[1]]))
    }

    fn sees(&self, b: &[[f32; 3]; 2]) -> bool {
        self.0.iter().all(|p| (0..3).map(|k| p[k] * if p[k] > 0.0 { b[1][k] } else { b[0][k] }).sum::<f32>() + p[3] >= 0.0)
    }
}

/// Per stream ([`BeadStream`]), ranges of its beads (each in order).
pub type StreamRanges = [Vec<std::ops::Range<u32>>; 3];

/// A run of beads left out between two a tier draws, in one stretch of started beads, draws with
/// them where it is this short: drawing its beads costs less than the draw of their own the
/// gap's two ranges take. The beads in it are a closed part's core or out of every view, so they
/// change no pixel; a run cut away by the layer range never joins.
const GAP_BEADS: u32 = 1024;

/// The ranges of the beads started (`started`) worth drawing, near and far, each stream's in its
/// own: the runs any of these cameras (the headset's two eyes, or the window's) and the layer
/// range can show, but the cores of the parts `closed` ([`PrintFrame::closed`]), the short gaps
/// between them drawn ([`GAP_BEADS`]). `tier` says which tier draws each block (by index; 0 near,
/// 1 far) or that none does, told whether it is a closed part's lining.
fn drawn_ranges(blocks: &[PreviewBlock], started: &RunCut, views: &[BeadView], layers: [f32; 2], closed: &[bool], mut tier: impl FnMut(usize, &PreviewBlock, bool) -> Option<usize>) -> [StreamRanges; 2] {
    let mut ranges: [StreamRanges; 2] = Default::default();
    // Per stream, the tier its last beads drawn went to: a gap joins only where none of its beads
    // went to the other one or was cut away since.
    let mut last: [Option<usize>; 3] = [None; 3];
    for (i, b) in blocks.iter().enumerate() {
        let s = b.stream as usize;
        let started = started.0[s];
        if b.first >= started { continue; }
        if (b.layers[0] >= 0.0 && b.layers[1] < layers[0] - 0.5) || b.layers[0] > layers[1] + 0.5 {
            last[s] = None;
            continue;
        }
        let closed_in = b.part.is_some_and(|p| closed.get(p) == Some(&true));
        if (b.stream == BeadStream::Core && closed_in) || !views.iter().any(|v| v.sees(&b.bounds)) { continue; }
        let Some(t) = tier(i, b, b.stream == BeadStream::Lining && closed_in) else { continue };
        let joins = last[s] == Some(t);
        last[s] = Some(t);
        let tier = &mut ranges[t][s];
        let (first, end) = (b.first, b.end.min(started));
        match tier.last_mut() {
            Some(r) if r.end == first || (joins && first - r.end < GAP_BEADS) => r.end = end,
            _ => tier.push(first..end),
        }
    }
    ranges
}

/// A live chunk's ranges worth drawing: its runs' ([`drawn_ranges`], near only) any of `views`
/// can show from its part's place, in one list in order (the streams' ranges joined).
fn chunk_ranges(frame: &PrintFrame, chunk: &LiveChunk, views: &[BeadView]) -> Vec<std::ops::Range<u32>> {
    let o = frame.preview.part_offsets.get(chunk.part).copied().unwrap_or([0.0, 0.0]);
    let views: Vec<BeadView> = views.iter().map(|v| v.shifted(o)).collect();
    let [near, _] = drawn_ranges(&chunk.runs, &RunCut([u32::MAX; 3]), &views, frame.layer_range(), &frame.closed, |_, _, _| Some(0));
    let mut all: Vec<std::ops::Range<u32>> = near.into_iter().flatten().collect();
    all.sort_unstable_by_key(|r| r.start);
    let mut ranges: Vec<std::ops::Range<u32>> = Vec::with_capacity(all.len());
    for r in all {
        match ranges.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => ranges.push(r),
        }
    }
    ranges
}

/// How many pixels across a bead of the preview shows, by where it is: from the eye (the
/// headset's two, between them, or the window's camera) in perspective, the same anywhere in a
/// parallel projection.
#[derive(Clone, Copy, Debug)]
pub enum FarSight {
    /// The eye in the beads' mm, and the projection's focal length in the target's pixels (the
    /// pixels across a thing as long as it is far).
    Eye { at: [f32; 3], focal_px: f32 },
    /// The pixels across a mm.
    Parallel { px_per_mm: f32 },
}

impl FarSight {
    /// The eyes of an XR pass (`Cx::xr_pass_eyes`) in a room placement: undone as
    /// `BeadView::placed` does it (moved to the origin, scaled, turned by the yaw about y,
    /// (x, z, -y) from mm * 0.001).
    pub fn of(eyes: &[(Mat4f, Mat4f); 2], eye_px: DVec2, place: PrintPlacement) -> FarSight {
        let (a, b) = (Self::eye(&eyes[0].0), Self::eye(&eyes[1].0));
        let k = 1.0 / (0.001 * place.scale);
        let q = [((a[0] + b[0]) * 0.5 - place.origin.x) * k, ((a[1] + b[1]) * 0.5 - place.origin.y) * k, ((a[2] + b[2]) * 0.5 - place.origin.z) * k];
        let (c, s) = (place.yaw.cos(), place.yaw.sin());
        let (x, y, z) = (c * q[0] - s * q[2], q[1], s * q[0] + c * q[2]);
        FarSight::Eye { at: [x, -z, y], focal_px: Self::focal_px(&eyes[0].1, eye_px) }
    }

    /// The window's camera (`BeadView::new`: mm * 0.001 as they are) on a target `px` device
    /// pixels: perspective where the projection's w rises along the view's axis (as the bead
    /// shader's `eye_across` tells them apart), parallel where it does not.
    pub fn new(view: &Mat4f, projection: &Mat4f, px: DVec2) -> FarSight {
        let focal_px = Self::focal_px(projection, px);
        if projection.v[11] < -0.5 {
            FarSight::Eye { at: Self::eye(view).map(|v| v * 1000.0), focal_px }
        } else {
            FarSight::Parallel { px_per_mm: focal_px * 0.001 }
        }
    }

    /// Where a view's eye is, in the space it views.
    fn eye(view: &Mat4f) -> [f32; 3] {
        let p = view.invert().transform_vec4(vec4(0.0, 0.0, 0.0, 1.0));
        [p.x / p.w, p.y / p.w, p.z / p.w]
    }

    /// The pixels across a thing one unit long, one unit far (perspective), or anywhere
    /// (parallel): the wider of the projection's two axes on a target `px` pixels.
    fn focal_px(projection: &Mat4f, px: DVec2) -> f32 {
        let p = &projection.v;
        (p[0] * px.x as f32).max(p[5] * px.y as f32) * 0.5
    }

    /// How many pixels across the run's widest bead is at its nearest to the eye.
    fn pixels(&self, b: &PreviewBlock) -> f32 {
        match *self {
            FarSight::Eye { at, focal_px } => {
                let d2: f32 = (0..3).map(|k| (b.bounds[0][k] - at[k]).max(at[k] - b.bounds[1][k]).max(0.0).powi(2)).sum();
                b.width * focal_px / d2.sqrt().max(0.001)
            }
            FarSight::Parallel { px_per_mm } => b.width * px_per_mm,
        }
    }
}

/// Which runs of a preview draw as far beads (`DrawFabFar`, `DrawFabFarAux`), kept from frame
/// to frame: a run goes far where its widest bead is a pixel or less across at its nearest to
/// the eye, and comes back near only past a quarter more, so a head or a camera that holds still
/// or sways flips none. Runs reaching past the highest layer drawn whole (the one printing) stay near.
/// A closed part's lining is drawn in neither tier where its run is far, nor, for the headset's
/// eyes, where it is thin ([`FarTiers::LINING_PX`]), kept from frame to frame the same way.
#[derive(Default)]
pub struct FarTiers {
    /// The preview (`PrintPreview::id`) and its run count the tiers are for.
    preview: Option<(u64, usize)>,
    far: Vec<bool>,
    /// Per run: a closed part's lining too thin to draw near.
    thin: Vec<bool>,
    /// The runs that went far and came back near since `flips` last took them, and the near and
    /// far ranges the last frame drew.
    went_far: usize,
    came_near: usize,
    ranges: [usize; 2],
}

impl FarTiers {
    /// A run goes far at this many pixels across or fewer ...
    const ENTER_PX: f32 = 1.0;
    /// ... and comes back near past this many.
    const LEAVE_PX: f32 = 1.25;
    /// A far range of fewer beads than this draws near: as a draw of its own it costs more than
    /// its beads drawn near do over far (a draw's own cost against a bead's, measured on a Linux
    /// node's GPU).
    const RUN_BEADS: u32 = 430;
    /// A closed part's lining stands behind its outer wall and skins: far, their quads leave no
    /// gap and none of it shows; near, a run of it this many pixels across or fewer shows only
    /// through the gaps between their beads, as what lies deeper does without it, and the
    /// headset's eyes leave it out (on a Linux node's GPU: no pixel of a plate turned to the
    /// background in any view, the colour in such gaps changed; on an M-series GPU 2.5 ms of a
    /// headset frame's eye pass at 35 cm). The window keeps it: there a person inspects the
    /// print, and no frame's budget presses ...
    const LINING_PX: f32 = 1.5;
    /// ... and a run left out is drawn again past this many.
    const LINING_LEAVE_PX: f32 = 1.875;

    /// The near and far ranges of `frame`'s started beads (`PrintFrame::cut`) the eyes `views` see;
    /// `headset`: they are the headset's, which leave a closed part's thin lining out.
    pub fn ranges(&mut self, frame: &PrintFrame, views: &[BeadView], sight: FarSight, headset: bool) -> [StreamRanges; 2] {
        let blocks = &frame.preview.blocks;
        let key = (frame.preview.id, blocks.len());
        if self.preview != Some(key) {
            self.preview = Some(key);
            self.far.clear();
            self.far.resize(blocks.len(), false);
            self.thin.clear();
            self.thin.resize(blocks.len(), false);
        }
        let whole = frame.layers()[3];
        let (mut went_far, mut came_near) = (0, 0);
        let ranges = drawn_ranges(blocks, &frame.cut, views, frame.layer_range(), &frame.closed, |i, b, closed_lining| {
            let was = self.far[i];
            let px = sight.pixels(b);
            let far = b.layers[1] < whole + 0.5 && px <= if was { Self::LEAVE_PX } else { Self::ENTER_PX };
            went_far += (far && !was) as usize;
            came_near += (was && !far) as usize;
            self.far[i] = far;
            let thin = headset && closed_lining && px <= if self.thin[i] { Self::LINING_LEAVE_PX } else { Self::LINING_PX };
            self.thin[i] = thin;
            (!(closed_lining && (far || thin))).then_some(far as usize)
        });
        // Each stream's far ranges too short to draw far draw near.
        let [mut near, mut far] = ranges;
        let [[n0, f0], [n1, f1], [n2, f2]]: [[Vec<std::ops::Range<u32>>; 2]; 3] = std::array::from_fn(|s| near_takes_short([std::mem::take(&mut near[s]), std::mem::take(&mut far[s])], Self::RUN_BEADS));
        let ranges = [[n0, n1, n2], [f0, f1, f2]];
        self.went_far += went_far;
        self.came_near += came_near;
        self.ranges = ranges.each_ref().map(|t| t.iter().map(Vec::len).sum());
        ranges
    }

    /// The runs that went far and came back near since the last call (none: nothing to tell),
    /// and the near and far ranges the last frame drew.
    pub fn flips(&mut self) -> Option<(usize, usize, [usize; 2])> {
        let flips = (std::mem::take(&mut self.went_far), std::mem::take(&mut self.came_near));
        (flips.0 + flips.1 > 0).then_some((flips.0, flips.1, self.ranges))
    }
}

/// The near and far ranges (each in order) with the far ones of fewer than `beads` beads drawn
/// near, joined to the near ones they touch.
fn near_takes_short([near, far]: [Vec<std::ops::Range<u32>>; 2], beads: u32) -> [Vec<std::ops::Range<u32>>; 2] {
    if far.iter().all(|r| r.end - r.start >= beads) {
        return [near, far];
    }
    let (short, far): (Vec<_>, Vec<_>) = far.into_iter().partition(|r| r.end - r.start < beads);
    let mut joined: Vec<std::ops::Range<u32>> = Vec::with_capacity(near.len() + short.len());
    let (mut a, mut b) = (near.into_iter().peekable(), short.into_iter().peekable());
    while let Some(next) = match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if y.start < x.start => b.next(),
        (Some(_), _) => a.next(),
        (None, _) => b.next(),
    } {
        match joined.last_mut() {
            Some(r) if r.end == next.start => r.end = next.end,
            _ => joined.push(next),
        }
    }
    [joined, far]
}

/// The print into the aux G-buffer, shaded, inside that pass's draw list and 3D scene: the
/// desktop draws it nowhere else. Per recording: the timeline prefix's runs the camera
/// (`view`, `projection`) can see, near ones as tubes and far ones as quads (`FarTiers`, the
/// pass's pixels), as `draw_print_placed` draws them, and the live chunks' runs it can see.
/// `exact` (View › Sliced Detail): every run near, as tubes.
pub fn draw_print(
    cx: &mut Cx3d,
    frame: &PrintFrame,
    palette: &mut PrintPalette,
    light: &TileLight,
    (view, projection): (&Mat4f, &Mat4f),
    (path_aux, far_aux, tiers, exact): (&mut DrawFabPathAux, &mut DrawFabFarAux, &mut FarTiers, bool),
    head_aux: &mut DrawFabHeadAux,
) -> u32 {
    let tex = palette.textures(cx.cx, frame);
    let bead_view = BeadView::new(view, projection);
    let px = cx.cx.current_pass_size() * cx.cx.current_dpi_factor();
    let sight = FarSight::new(view, projection, px);
    let ranges = if exact {
        drawn_ranges(&frame.preview.blocks, &frame.cut, &[bead_view], frame.layer_range(), &frame.closed, |_, _, _| Some(0))
    } else {
        tiers.ranges(frame, &[bead_view], sight, false)
    };
    let mut calls = draw_beads(cx, frame, &tex, false, PrintPlacement::IDENTITY, light, Some(&ranges[0]), &mut path_aux.draw_vars);
    if ranges[1].iter().any(|s| !s.is_empty()) {
        calls += draw_beads(cx, frame, &tex, false, PrintPlacement::IDENTITY, light, Some(&ranges[1]), &mut far_aux.draw_vars);
    }
    calls += draw_extras(cx, frame, Some(std::slice::from_ref(&bead_view)), &mut path_aux.draw_vars);
    if !frame.heads.is_empty() {
        bind(&mut head_aux.draw_vars, cx.cx, frame, &tex, false, true, PrintPlacement::IDENTITY, light);
        draw_heads!(head_aux, cx.cx, frame);
        calls += 1;
    }
    palette.notify_drawn(frame);
    calls
}

/// Forward colour + depth, in the plate's room placement: the XR pass. The caller owns the
/// draw list and scene; XR needs only these lit shaders, no aux targets.
/// Per frame: the timeline prefix's runs either eye of the frame being drawn can see
/// (`Cx::xr_pass_eyes`, the eyes the pass is drawn with), near ones as strips and far ones as
/// quads (`FarTiers`), the live chunks' runs they can see and at most two head instances; placement changes only
/// uniforms and the runs drawn, never the bead buffer (both tiers draw from its one copy).
pub fn draw_print_placed(
    cx: &mut Cx3d,
    frame: &PrintFrame,
    palette: &mut PrintPalette,
    place: PrintPlacement,
    light: &TileLight,
    (path, far, tiers): (&mut DrawFabStrip, &mut DrawFabFar, &mut FarTiers),
    head: &mut DrawFabHead,
) -> u32 {
    let tex = palette.textures(cx.cx, frame);
    let eyes = cx.cx.xr_pass_eyes().map(|(eyes, eye_px)| (eyes.map(|(view, projection)| BeadView::placed(&view, &projection, place)), FarSight::of(&eyes, eye_px, place)));
    let ranges = eyes.map(|(views, sight)| tiers.ranges(frame, &views, sight, true));
    let mut calls = draw_beads(cx, frame, &tex, true, place, light, ranges.as_ref().map(|r| &r[0]), &mut path.draw_vars);
    if let Some(far_ranges) = ranges.as_ref().map(|r| &r[1]).filter(|r| r.iter().any(|s| !s.is_empty())) {
        calls += draw_beads(cx, frame, &tex, true, place, light, Some(far_ranges), &mut far.draw_vars);
    }
    calls += draw_extras(cx, frame, eyes.as_ref().map(|(views, _)| &views[..]), &mut path.draw_vars);
    if !frame.heads.is_empty() {
        bind(&mut head.draw_vars, cx.cx, frame, &tex, true, true, place, light);
        draw_heads!(head, cx.cx, frame);
        calls += 1;
    }
    palette.notify_drawn(frame);
    calls
}

/// The timeline's beads up to the playhead, bound to `vars`: those in `ranges`, or all of them.
/// Each stream draws from its own buffer, its ranges in one draw.
#[allow(clippy::too_many_arguments)]
fn draw_beads(cx: &mut Cx3d, frame: &PrintFrame, tex: &FrameTextures, lit: bool, place: PrintPlacement, light: &TileLight, ranges: Option<&StreamRanges>, vars: &mut DrawVars) -> u32 {
    debug_assert_eq!(PREVIEW_SLOTS, 12);
    bind(vars, cx.cx, frame, tex, lit, false, place, light);
    let mut calls = 0;
    for s in BeadStream::ALL {
        let beads = frame.preview.beads.of(s);
        // Past the cut nothing of the stream has started: drawn whole, it draws to its first bead
        // not started.
        let count = (frame.cut.0[s as usize] as usize).min(beads.count());
        let whole = [0..count as u32];
        let ranges = ranges.map_or(&whole[..], |r| &r[s as usize][..]);
        if count == 0 || ranges.is_empty() {
            continue;
        }
        let area = cx.cx.add_retained_instances_count(vars, beads, count);
        vars.area = area;
        if let Area::Instance(a) = area {
            cx.cx.draw_lists[a.draw_list_id].draw_items.set_instance_ranges(a.draw_item_id, ranges);
        }
        calls += 1;
    }
    calls
}

/// The rest of a frame's beads, with `vars` as `draw_beads` bound them: live chunks (their runs
/// any of `views` can show, all of them without), travel, the compared terraces, markers and the
/// lit branches.
fn draw_extras(cx: &mut Cx3d, frame: &PrintFrame, views: Option<&[BeadView]>, vars: &mut DrawVars) -> u32 {
    let mut calls = 0;
    for chunk in &frame.preview.chunks {
        let area = cx.cx.add_retained_instances_count(vars, &chunk.beads, chunk.beads.count());
        vars.area = area;
        if let (Some(views), Area::Instance(a)) = (views, area) {
            // A chunk none of whose beads show keeps its draw (and buffer), drawing nothing.
            let ranges = chunk_ranges(frame, chunk, views);
            let ranges = if ranges.is_empty() { vec![0..0] } else { ranges };
            cx.cx.draw_lists[a.draw_list_id].draw_items.set_instance_ranges(a.draw_item_id, &ranges);
        }
        calls += 1;
    }
    let travel = &frame.preview.travel;
    let travel_count = frame.travel_count.min(travel.count());
    if travel_count > 0 {
        vars.area = cx.cx.add_retained_instances_count(vars, travel, travel_count);
        calls += 1;
    }
    let ghosts = &frame.preview.ghosts;
    if frame.compare_planar && ghosts.count() > 0 {
        vars.area = cx.cx.add_retained_instances_count(vars, ghosts, ghosts.count());
        calls += 1;
    }
    let markers = &frame.preview.markers;
    if frame.show_markers && markers.count() > 0 {
        vars.area = cx.cx.add_retained_instances_count(vars, markers, markers.count());
        calls += 1;
    }
    if let Some(lit) = frame.lit.as_ref().filter(|l| l.count() > 0) {
        vars.area = cx.cx.add_retained_instances_count(vars, lit, lit.count());
        calls += 1;
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::PlatePart;
    use crate::print::{apply, PrintAction};

    #[test]
    fn bead_aux_ids_keep_codes_and_lit_marker_distinct() {
        let bead_code = |w: f32| {
            if w < 16_000_004.0 {
                -1.0
            } else {
                ((w - PRINT_AUX_ID) / 8.0 + 0.5).floor()
            }
        };
        let lit_bead = |w: f32| (w - LIT_BEAD_AUX_ID).abs() < 1.5;
        for k in [0, 1, 2, 5, 1000, 1 << 20] {
            let w = PRINT_AUX_ID + 8.0 * k as f32;
            assert!(w > 16_000_004.0, "print code {k}: id must be a bead");
            assert_eq!(bead_code(w), k as f32, "print code {k}: code must round-trip");
            assert!(!lit_bead(w), "print code {k}: id must not match the lit-bead marker");
        }
        assert!(LIT_BEAD_AUX_ID > 16_000_004.0, "lit-bead marker must be a bead");
        assert_eq!(bead_code(LIT_BEAD_AUX_ID), 0.0, "lit-bead marker must decode to code 0");
        assert!(lit_bead(LIT_BEAD_AUX_ID), "lit-bead marker must match the lit-bead test");
        assert_eq!(bead_code(16_000_008.0), 0.0, "board id must decode to code 0");
        assert!(!lit_bead(16_000_008.0), "board id must not match the lit-bead marker");
        // the composite's tests: `abs(w - SELECTED_BODY_AUX_ID) < 0.5` before the bead code
        let selected = |w: f32| (w - SELECTED_BODY_AUX_ID).abs() < 0.5;
        assert_eq!(bead_code(SELECTED_BODY_AUX_ID), 0.0, "selected body must decode to code 0");
        assert!(!lit_bead(SELECTED_BODY_AUX_ID), "selected body must take its own lit test");
        for w in [LIT_BEAD_AUX_ID, PRINT_AUX_ID, PRINT_AUX_ID + 8.0] {
            assert!(!selected(w), "{w}: only the selected body's id reads as selected");
        }
    }

    #[test]
    fn the_top_layer_is_not_highlighted_and_one_below_it_is() {
        let pv = crate::print::follow::tests::preview_of(4);
        let mut state = AppState::default();
        state.print.set_preview(pv.clone());
        apply(&mut state.print, &PrintAction::Preview(true));
        // The plate as edited: its contours on every layer, drawn while the toolpaths are on
        // their way.
        let bead = RetainedInstances::new(PREVIEW_SLOTS, crate::print::Bead::default().pack().to_vec().into()).unwrap();
        let contours = crate::print::Contours { parts: vec![Arc::from(vec![Some(bead.clone()); 4])], all: bead, top_layer: 3 };
        state.print.live_contours = Some(crate::print::LiveContours { revision: 1, contours });
        // The current layer's bead highlight (1 + the layer, 0: none) and the layer the model's
        // outline marks.
        let lit = |state: &AppState| {
            let frame = PrintFrame::of(state, None).expect("the slice view draws");
            let beads = frame.uniforms.iter().find(|(id, _)| *id == live_id!(u_layers)).expect("u_layers").1[2];
            (beads, outline_at(state, Some(&frame)).map(|(_, layer)| layer))
        };
        // The whole print (the default, the slider at the top): no layer leads.
        assert_eq!(lit(&state), (0.0, None));
        // One layer down by the slider: that layer leads.
        apply(&mut state.print, &PrintAction::SetTopLayer(2));
        assert_eq!(lit(&state), (3.0, Some(2)));
        // Back at the top.
        apply(&mut state.print, &PrintAction::SetTopLayer(3));
        assert_eq!(lit(&state), (0.0, None));
        // Playback below the top lights the layer it prints (the slider stays at the top); in the
        // last one, and at the end, none.
        apply(&mut state.print, &PrintAction::Seek(25.0));
        assert_eq!(lit(&state), (2.0, None));
        apply(&mut state.print, &PrintAction::Seek(45.0));
        assert_eq!(lit(&state), (0.0, None));
        apply(&mut state.print, &PrintAction::Seek(pv.total));
        assert_eq!(lit(&state), (0.0, None));
    }

    /// A wall bead shades its top and underside by the beads stacked on them; one on a layer
    /// that is not drawn (above the slider's top, below its bottom, not yet printed at the
    /// playhead) must not count, or the bead shows a dark crease that turns with the camera.
    /// The shader takes the layers drawn whole from `u_layers` x (lowest) and w (highest).
    /// A part's core is left out only while all of the part is drawn (`PrintFrame::closed`): a
    /// cut from above or below, playback inside the part or the non-planar comparison opens it.
    #[test]
    fn a_bead_stacks_only_on_layers_that_are_drawn() {
        let mut pv = crate::print::follow::tests::preview_of(4);
        // a part on every layer, and one on the lowest two; non-planar tops to compare
        let fresh = Arc::get_mut(&mut pv).expect("a fresh preview");
        fresh.part_layers = vec![[0.0, 3.0], [0.0, 1.0]];
        fresh.nonplanar = Some(crate::print::NpSummary::default());
        let mut state = AppState::default();
        state.print.set_preview(pv.clone());
        apply(&mut state.print, &PrintAction::Preview(true));
        let drawn = |state: &AppState| {
            let frame = PrintFrame::of(state, None).expect("the slice view draws");
            let layers = frame.uniforms.iter().find(|(id, _)| *id == live_id!(u_layers)).expect("u_layers").1;
            (layers[0], layers[3], frame.closed)
        };
        // The whole print: every layer up to the top, both parts closed.
        assert_eq!(drawn(&state), (0.0, 3.0, vec![true, true]));
        // The slider's top at layer 2: nothing stands on it; the tall part is cut open.
        apply(&mut state.print, &PrintAction::SetTopLayer(2));
        assert_eq!(drawn(&state), (0.0, 2.0, vec![false, true]));
        // Its bottom at layer 1: nothing under that; both are cut open.
        apply(&mut state.print, &PrintAction::SetBottomLayer(1));
        assert_eq!(drawn(&state), (1.0, 2.0, vec![false, false]));
        apply(&mut state.print, &PrintAction::SetBottomLayer(0));
        apply(&mut state.print, &PrintAction::SetTopLayer(3));
        // Playback halfway through layer 1: only layer 0 is whole, layer 1's beads have
        // nothing on them yet; both parts print on.
        apply(&mut state.print, &PrintAction::Seek(25.0));
        assert_eq!(drawn(&state), (0.0, 0.0, vec![false, false]));
        // Before anything prints: no layer is whole.
        apply(&mut state.print, &PrintAction::Seek(0.0));
        assert_eq!(drawn(&state), (0.0, -1.0, vec![false, false]));
        apply(&mut state.print, &PrintAction::Seek(pv.total));
        assert_eq!(drawn(&state), (0.0, 3.0, vec![true, true]));
        // Comparing non-planar tops with their terraces takes the tops away.
        apply(&mut state.print, &PrintAction::ComparePlanar(true));
        assert_eq!(drawn(&state).2, vec![false, false]);
    }

    #[test]
    fn beads_find_their_part_by_object_and_an_eye_turned_off_hides_them() {
        let (a, b) = (ElementId::from_index(0), ElementId::from_index(1));
        let mut state = AppState::default();
        // The shown plate has reordered since the slice (printed parts first).
        state.plate.parts = vec![
            PlatePart { object: "a".into(), element: Some(a), printed: true, ..Default::default() },
            PlatePart { object: "b".into(), element: Some(b), printed: false, ..Default::default() },
        ];
        let mut parts = Vec::new();
        part_states(&state, &["b".to_string(), "gone".to_string(), "a".to_string()], &mut parts);
        assert_eq!(parts.len(), 3);
        assert_eq!((parts[0][0], parts[0][2]), (2.0, 1.0));
        assert_eq!(parts[1], [0.0; 4]);
        assert_eq!((parts[2][0], parts[2][2]), (1.0, 0.0));
    }

    #[test]
    fn an_insert_draws_under_the_slice_is_no_ghost_and_cuts_at_the_layer_when_asked() {
        let (printed, excluded, insert) = (ElementId::from_index(0), ElementId::from_index(1), ElementId::from_index(2));
        let mut state = AppState::default();
        state.plate.parts = vec![
            PlatePart { object: "printed".into(), element: Some(printed), printed: true, ..Default::default() },
            PlatePart { object: "excluded".into(), element: Some(excluded), printed: false, ..Default::default() },
            PlatePart { object: "insert".into(), element: Some(insert), printed: false, insert: true, ..Default::default() },
        ];

        assert!(!state.parts_under_slice(), "the plate view has no slice");
        let poses = state.shared_poses();
        for id in [printed, excluded, insert] {
            assert!(state.is_drawn(id) || state.is_ghost_drawn_under(poses, id), "every part is drawn on the plate: {id:?}");
            assert_eq!(state.is_drawn(id), id != excluded, "only the excluded part is not solid: {id:?}");
            assert_eq!(state.is_ghost_under(poses, id), id == excluded, "only the excluded part is a ghost: {id:?}");
            assert_eq!(state.is_ghost_drawn_under(poses, id), id == excluded, "only the excluded part uses the ghost pass: {id:?}");
        }

        state.print.set_preview(crate::print::follow::tests::preview_of(2));
        apply(&mut state.print, &PrintAction::Preview(true));
        state.view.see.on = true;
        assert!(state.parts_under_slice(), "the Sliced view hides the printed parts");
        for layer in 0..2 {
            apply(&mut state.print, &PrintAction::SetTopLayer(layer));
            for id in [printed, excluded, insert] {
                assert_eq!(state.is_drawn(id), id == insert, "only the insert stays solid under layer {layer}: {id:?}");
                assert!(!state.is_ghost_drawn_under(poses, id), "no part uses the ghost pass under layer {layer}: {id:?}");
                assert!(!state.see_through_shows_under(poses, id), "no part is see-through under layer {layer}: {id:?}");
            }
        }

        // Cut Parts at the Layer: off, nothing is cut; on, the slider's layer cuts while it leads.
        apply(&mut state.print, &PrintAction::SetTopLayer(0));
        assert_eq!(super::layer_cut(&state), None, "the parts are drawn whole by default");
        apply(&mut state.print, &PrintAction::CutAtLayer(true));
        let top = crate::tools::transform::mm_to_world([0.0, 0.0, 0.2]).z;
        assert_eq!(super::layer_cut(&state), Some([0.0, 0.0, -1.0, top]), "layer 0 leads: the parts are cut at its top");
        assert!(!state.scene_state.section.caps && super::caps_shown(&state, &[]), "the layer's cut is capped with no section on");
        apply(&mut state.print, &PrintAction::SetTopLayer(1));
        assert_eq!(super::layer_cut(&state), None, "at the last layer the print shows whole, uncut");
        assert!(!super::caps_shown(&state, &[]), "nothing cut, nothing capped");
    }

    /// An item printed into its part is not in the print until the pause for it: the Sliced view
    /// draws it from the layer the print pauses after, and the slider's tag says to place it there.
    #[test]
    fn an_embedded_item_shows_in_the_sliced_view_from_its_pause() {
        let (host, nut, insert) = (ElementId::from_index(0), ElementId::from_index(1), ElementId::from_index(2));
        let mut state = AppState::default();
        state.plate.parts = vec![
            PlatePart { object: "host".into(), name: "Host".into(), element: Some(host), printed: true, ..Default::default() },
            PlatePart { object: "nut".into(), name: "M3 nut".into(), element: Some(nut), printed: false, insert: true, embed: true, insert_into: "host".into(), ..Default::default() },
            PlatePart { object: "post".into(), name: "Post".into(), element: Some(insert), printed: false, insert: true, insert_into: "host".into(), ..Default::default() },
        ];
        let mut pv = crate::print::follow::tests::preview_of(4);
        std::sync::Arc::get_mut(&mut pv).unwrap().pauses = vec![crate::print::PreviewPause { layer: 2, z: 0.6, items: vec!["M3 nut".into()] }];
        state.print.set_preview(pv);
        apply(&mut state.print, &PrintAction::Preview(true));
        assert!(state.parts_under_slice());
        for (layer, placed) in [(0, false), (1, true), (3, true)] {
            apply(&mut state.print, &PrintAction::SetTopLayer(layer));
            assert_eq!(state.is_drawn(nut), placed, "the nut under layer {layer}: in place from the pause after layer 2");
            assert!(state.is_drawn(insert), "a plain insert stays under layer {layer}");
        }
        assert_eq!(crate::print::PreviewPause { layer: 2, z: 0.6, items: vec!["M3 nut".into(), "Board".into()] }.step(), "Pause before layer 3, at 0.60 mm: place M3 nut, Board");
        // Outside the Sliced view the item is drawn as any insert.
        apply(&mut state.print, &PrintAction::Preview(false));
        apply(&mut state.print, &PrintAction::SetTopLayer(0));
        assert!(state.is_drawn(nut));
    }

    /// The window's camera measures a run in its own pixels, as the headset's eyes do
    /// (`FarSight`, which sends a run a pixel or less across to the far tier): in perspective by
    /// how far the run is from the eye, in a parallel projection (View › Orthographic) by the
    /// projection's scale alone, wherever the run is.
    #[test]
    fn the_window_camera_measures_a_bead_in_its_own_pixels() {
        let block = |x: f32| PreviewBlock { first: 0, end: 1, bounds: [[x, -1.0, 0.0], [x + 2.0, 1.0, 0.2]], layers: [0.0, 0.0], width: 0.4, stream: BeadStream::Surface, part: None };
        let px = dvec2(400.0, 300.0);
        // parallel, 75 mm tall on 300 pixels: 4 pixels a mm, a 0.4 mm bead 1.6 across anywhere
        let ortho = Camera { ortho: true, ortho_height: 0.075, ..Camera::default() };
        let sight = FarSight::new(&ortho.view(), &ortho.projection(4.0 / 3.0), px);
        assert!(matches!(sight, FarSight::Parallel { .. }), "{sight:?}");
        for x in [0.0, 100.0] {
            assert!((sight.pixels(&block(x)) - 1.6).abs() < 1e-3, "{sight:?}: {}", sight.pixels(&block(x)));
        }
        // perspective: the eye where the camera is (in mm), a run twice as far half as wide
        let camera = Camera { eye: vec3(0.0, -0.2, 0.0), target: vec3(0.0, 0.0, 0.0), ..Camera::default() };
        let sight = FarSight::new(&camera.view(), &camera.projection(4.0 / 3.0), px);
        let FarSight::Eye { at, .. } = sight else { panic!("{sight:?}") };
        assert!(at[0].abs() < 1e-2 && (at[1] + 200.0).abs() < 1e-2 && at[2].abs() < 1e-2, "{at:?}");
        let ahead = |y: f32| PreviewBlock { bounds: [[-1.0, y, 0.0], [1.0, y + 0.4, 0.2]], ..block(0.0) };
        let (one, two) = (sight.pixels(&ahead(0.0)), sight.pixels(&ahead(200.0)));
        assert!((one / two - 2.0).abs() < 1e-2, "{one} px 200 mm away, {two} px 400 mm away");
    }
}
