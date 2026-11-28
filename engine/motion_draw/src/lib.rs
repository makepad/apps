//! `MotionDraw`: a [`MotionFrame`] drawn on the GPU into an offscreen
//! target at a given pixel size, with Makepad's vector tessellator
//! (`DrawVector`), SVG renderer (`DrawSvg`), text renderer (`DrawText`),
//! runtime-compiled shaders, vj_fx effects, the widget tree and the PBR
//! renderer. The EDIT renderer and preview (edit_render's `graphic.rs`) and
//! the agent tools (`makepad-edit-tools`) draw documents through it.
//!
//! A frame is a chain of passes recorded in order and linked so they
//! execute in that order (each pass's parent is the next, the last is the
//! child of whatever pass the host is drawing): first everything that needs
//! a texture of its own (isolated groups and their masks, rotated text,
//! widgets, 3D, SVGs under rotation), then the canvas, split wherever a
//! layer reads what is under it (a blend mode other than normal, a shader
//! with `input: @below`, a vj_fx effect). Shapes, text and images draw
//! straight into the canvas.
//!
//! Static layers at the bottom of the frame (no animated value) are drawn
//! once into a kept texture and reused while they stay the same.
//!
//! Host resources: images, SVG markup and video frames are handed in with
//! [`MotionDraw::set_image`], [`MotionDraw::set_svg`] and
//! [`MotionDraw::set_video_frame`] (decode off the UI thread with
//! [`decode_image`]); [`MotionDraw::missing`] lists what a frame needs that
//! is not there yet. vj_fx effects build on the task pool: call
//! [`MotionDraw::handle_event`] on every event and draw once
//! [`MotionDraw::ready`].

use makepad_motion::plain::PrimitiveFrame;
pub mod coverage;
mod draw2d;
/// The frame tool (`makepad-motion`, and hosts' pixel tests), on full Scene3D.
#[cfg(feature = "scene3d")]
pub mod frame_tool;
mod scene;
pub mod paint;
#[cfg(feature = "scene3d")]
mod scene3d;
#[cfg(not(feature = "scene3d"))]
#[path = "scene3d_off.rs"]
mod scene3d;
mod vector;
pub(crate) mod shader;
mod text;
mod widget;
pub use widget::widget_ui;

use makepad_motion::frame::*;
use makepad_motion::{geometry, plain, LoadedPlainFrame, PlainFrame};
use makepad_motion::model::{Blend, Cap, Fit, Join, Media, ShaderProgram};
use makepad_motion::render_effects::{camera_spec, kit_effect};
use makepad_stage_render_kit::{Camera as KitCamera, Painted as KitPainted, RenderKit};
use makepad_widgets::makepad_draw::image_cache::{decode_image_from_data, ImageBuffer};
use makepad_widgets::makepad_draw::shader::draw_vector::FeatherSide as DrawFeatherSide;
use makepad_widgets::*;
use std::collections::HashMap;

pub use shader::{compile_shader, inline_shader_object, vj_effects_used, DrawMotionShader};
pub use text::face_style;

/// The drawing side's own shaders and text faces (see [`script_mod`]).
mod draws {
    use super::*;

    script_mod! {
        use mod.prelude.widgets_internal.*
        use mod.shared.*

        // One textured-triangle draw for images and compositing. A quad is
        // two instances of the same unit triangle; meshes reuse its sampling,
        // opacity, mask and HDR conversion. t0/t1/t2 carry pixel xy and UV zw.
        set_type_default() do #(DrawMotionQuad::script_shader(vm)){
            ..mod.draw.DrawQuad
            tex: texture_2d(float)
            mask: texture_2d(float)

            vertex: fn() {
                let p = (1.0 - self.geom.pos.x - self.geom.pos.y) * self.t0 + self.geom.pos.x * self.t1 + self.geom.pos.y * self.t2
                self.pos = p.zw
                self.world = self.draw_list.view_transform * vec4(p.x, p.y, self.draw_depth + self.draw_call.zbias, 1.0)
                self.vertex_pos = self.draw_pass.camera_projection * (self.draw_pass.camera_view * self.world)
            }

            pixel: fn() {
                let uv = self.pos
                if uv.x < self.q_clip.x || uv.y < self.q_clip.y || uv.x > self.q_clip.z || uv.y > self.q_clip.w {
                    return vec4(0.0, 0.0, 0.0, 0.0)
                }
                var c = self.tex.sample(clamp(uv, vec2(0.0, 0.0), vec2(1.0, 1.0))) * self.opacity
                if self.decode > 1.5 {
                    // A linear (HDR) frame encoded sRGB for a display-referred
                    // effect, the range above 1 kept (the curve extended).
                    let a = max(c.w, 0.00001)
                    let e = linear_to_srgb(max(c.xyz / a, vec3(0.0, 0.0, 0.0)))
                    c = vec4(e * c.w, c.w)
                } else if self.decode > 0.5 {
                    // An sRGB picture into a linear (HDR) frame.
                    let a = max(c.w, 0.00001)
                    let l = srgb_to_linear(clamp(c.xyz / a, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0)))
                    c = vec4(l * c.w, c.w)
                }
                if self.mask_mode > 0.5 {
                    let mc = self.mask.sample(clamp(uv, vec2(0.0, 0.0), vec2(1.0, 1.0)))
                    // 1 alpha, 2 inverse alpha, 3 luma, 4 inverse luma (the mask
                    // is premultiplied, so its luma already carries its alpha).
                    let mut m = mc.w
                    if self.mask_mode > 2.5 {
                        m = dot(mc.xyz, vec3(0.2126, 0.7152, 0.0722))
                    }
                    if self.mask_mode > 3.5 || (self.mask_mode > 1.5 && self.mask_mode < 2.5) {
                        return c * (1.0 - m)
                    }
                    return c * m
                }
                return c
            }
        }

        // A filled and/or stroked rectangle (rounded), ellipse or arc, drawn
        // from its distance field on one instanced quad: no tessellation, no
        // mesh upload, and a thousand shapes batch into one draw call. `q_*`
        // place the box as for DrawMotionQuad; `s_fill`/`s_stroke` are
        // premultiplied. `s_px` is pixels per box unit (0: read from the
        // screen-space derivatives, for a host under its own view transform).
        // The distance fields and the edge are core Sdf2d's.
        // The shapes of `paint` (Motion frames, Sandbox's HUD and screens).
        set_type_default() do #(DrawMotionShape::script_shader(vm)){
            ..mod.draw.DrawAffineQuad,
            ..mod.glyph_coverage.FeatherCover

            vertex: fn() {
                // The box grown by half the stroke, three sigma and AA.
                let mut edge = 1.5 + 3.0 * self.s_sigma
                if self.s_px > 0.0 {
                    edge = edge / self.s_px
                }
                let e = abs(self.s_width) * 0.5 + edge
                let mut p = vec2(0.0 - e, 0.0 - e) + self.geom.pos * (self.affine_size + vec2(2.0 * e, 2.0 * e))
                let mut w = self.place(p)
                // An unturned box keeps to the turtle's clip, as a DrawQuad
                // does (a host's clipped panel, a square radar).
                if abs(self.affine_m.y) + abs(self.affine_m.z) < 0.000001 && abs(self.affine_m.x) > 0.000001 && abs(self.affine_m.w) > 0.000001 {
                    let wc = clamp(w, self.draw_clip.xy, self.draw_clip.zw)
                    p = p + (wc - w) / vec2(self.affine_m.x, self.affine_m.w)
                    w = wc
                }
                self.pos = p
                self.world = self.draw_list.view_transform * vec4(w.x, w.y, self.draw_depth + self.draw_call.zbias, 1.0)
                self.vertex_pos = self.draw_pass.camera_projection * (self.draw_pass.camera_view * self.world)
            }

            pixel: fn() {
                // The shape's distance on Sdf2d, its edge centred on the
                // pixel (viewport_px): a pixel straddling the edge is half
                // covered, a box on whole pixels fills exactly its pixels.
                let half = self.affine_size * 0.5
                let mut sdf = Sdf2d.viewport_px(self.pos, self.s_px)
                if self.s_kind > 1.5 && self.s_kind < 2.5 {
                    // An arc of the ring inside the short side: from s_param.x
                    // over s_param.y * s_param.z radians, s_param.w thick,
                    // flat caps.
                    if self.s_param.z <= 0.001 {
                        return vec4(0.0, 0.0, 0.0, 0.0)
                    }
                    let r = min(half.x, half.y) - self.s_param.w * 0.5 - 1.0
                    sdf.arc_flat_caps(half.x, half.y, r, self.s_param.x, self.s_param.x + self.s_param.y * self.s_param.z, self.s_param.w)
                    return self.s_fill * self.feather_cover(-sdf.shape * sdf.aa, self.s_sigma, self.s_side) * self.opacity
                }
                if self.s_kind > 2.5 {
                    // A box cut on the chamfer: s_param is the cut of each
                    // corner (top-left, top-right, bottom-right, bottom-left).
                    sdf.chamfer_box(0.0, 0.0, self.affine_size.x, self.affine_size.y, self.s_param.x, self.s_param.y, self.s_param.z, self.s_param.w)
                } else if self.s_kind > 0.5 {
                    sdf.ellipse(half.x, half.y, half.x, half.y)
                } else {
                    // Sdf2d's box radius is half the visual one.
                    sdf.box(0.0, 0.0, self.affine_size.x, self.affine_size.y, self.s_radius * 0.5)
                }
                let d = sdf.shape
                let fill = self.s_fill * self.feather_cover(-d * sdf.aa, self.s_sigma, self.s_side)
                // A positive width strokes centred on the edge, a negative
                // one inside it (a plate's rim): the stroke's band over the
                // fill, premultiplied.
                let hw = abs(self.s_width) * 0.5
                let mut ds = d
                if self.s_width < 0.0 {
                    ds = d + hw
                }
                let stroke_d = abs(ds) - hw + 1e+20 * step(abs(self.s_width), 0.0001)
                let stroke = self.s_stroke * self.feather_cover(-stroke_d * sdf.aa, self.s_sigma, self.s_side)
                return (stroke + fill * (1.0 - stroke.a)) * self.opacity
            }
        }

        // A layer over what is below it, both premultiplied and full-target,
        // in the W3C separable blend modes, at `amount` of the layer.
        // Vector shapes in an emissive colour (components over 1, in an HDR
        // frame): the vertex colour is 8-bit, so the light multiple is a
        // uniform of their own session.
        set_type_default() do #(DrawMotionVectorEmit::script_shader(vm)){
            ..mod.draw.DrawVector
            gain: uniform(1.0)
            get_color: fn() {
                let c = self.eval_gradient()
                return vec4(c.xyz * self.gain, c.w)
            }
        }

        set_type_default() do #(DrawMotionBlend::script_shader(vm)){
            ..mod.draw.DrawQuad
            below: texture_2d(float)
            layer: texture_2d(float)

            pixel: fn() {
                let b = self.below.sample(self.pos)
                if self.mode > 4.5 {
                    // A dissolve: `amount` of the layer, the rest of what is
                    // below (a transition's plain mix).
                    return mix(b, self.layer.sample(self.pos), self.amount)
                }
                let l = self.layer.sample(self.pos) * self.amount
                let alpha = l.w + b.w * (1.0 - l.w)
                if self.mode > 3.5 {
                    return vec4(l.xyz * b.xyz + l.xyz * (1.0 - b.w) + b.xyz * (1.0 - l.w), alpha)
                }
                if self.mode > 2.5 {
                    return vec4(l.xyz + b.xyz - l.xyz * b.xyz, alpha)
                }
                if self.mode > 1.5 {
                    // Additive: light adds up (clipped at 1 only in a
                    // display frame).
                    if self.hdr > 0.5 {
                        return vec4(l.xyz + b.xyz, alpha)
                    }
                    return vec4(min(l.xyz + b.xyz, vec3(1.0, 1.0, 1.0)), alpha)
                }
                return l + b * (1.0 - l.w)
            }
        }

        mod.widgets.MotionDraws = set_type_default() do #(MotionDraws::script_component(vm)){
            text +: {color: #fff}
        }
    }
}

/// Register the drawing side: the render kit's shaders, motion3d's (with
/// `scene3d`), the quads and text faces frames draw with, and the inline
/// shader base. After `makepad_widgets::script_mod`, and
/// `makepad_content_packs::script_mod` when the host installed packs whose
/// presets documents run.
pub fn script_mod(vm: &mut ScriptVm) {
    for step in SCRIPT_MOD_STEPS {
        step(vm);
    }
}

/// [`script_mod`] in order, a call each: a host that registers it while frames run (a game's
/// fresh isolate on a restart) spreads them over frames.
pub const SCRIPT_MOD_STEPS: &[fn(&mut ScriptVm)] = &[
    // Before the text styles: they name makepad_motion's fonts.
    |vm| makepad_motion::register_resources(vm),
    |vm| makepad_stage_render_kit::script_mod(vm),
    |vm| {
        #[cfg(feature = "scene3d")]
        makepad_stage_motion3d::shared::script_mod(vm);
        makepad_render_graph::script_mod_passes(vm);
    },
    |vm| makepad_render_lines::script_mod(vm),
    |vm| {
        // The shared Splash stdlib (`mod.shared`: sRGB, hashes) the draws use.
        if makepad_script_compute::module::register_shared_std(vm) {
            makepad_tween_script::install_ease(vm);
        }
    },
    |vm| {
        draws::script_mod(vm);
    },
    |vm| {
        draw2d::script_mod(vm);
    },
    |vm| {
        shader::script_mod(vm);
    },
];

/// `m` as DrawAffineQuad's coefficients [a, b, c, d, e, f].
pub(crate) fn coeffs(m: &Affine) -> [f64; 6] {
    [m.a, m.b, m.c, m.d, m.e, m.f]
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMotionQuad {
    // Non-instance storage precedes the draw base.
    #[rust]
    triangle: Option<Geometry>,
    #[deref]
    pub draw_super: DrawQuad,
    #[live] pub t0: Vec4f,
    #[live] pub t1: Vec4f,
    #[live] pub t2: Vec4f,
    #[live] pub q_clip: Vec4f,
    #[live]
    pub opacity: f32,
    #[live]
    pub mask_mode: f32,
    /// 1: the texture is an sRGB picture drawn into a linear frame; 2: a
    /// linear frame encoded sRGB (for a display-referred effect).
    #[live]
    pub decode: f32,
}

impl DrawMotionQuad {
    fn begin_image(&mut self, cx: &mut Cx2d, texture: &Texture, mask: Option<(&Texture, makepad_motion::model::MaskMode)>, opacity: f64) {
        if self.triangle.is_none() {
            let geometry = Geometry::new(cx.cx.cx);
            geometry.update(cx.cx.cx, vec![0, 1, 2], vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
            self.triangle = Some(geometry);
        }
        self.draw_vars.geometry_id = self.triangle.as_ref().map(Geometry::geometry_id);
        self.draw_vars.set_texture(0, texture);
        match mask {
            Some((mask, mode)) => {
                use makepad_motion::model::MaskMode;
                self.draw_vars.set_texture(1, mask);
                self.mask_mode = match mode {
                    MaskMode::Alpha => 1.0,
                    MaskMode::Inverse => 2.0,
                    MaskMode::Luma => 3.0,
                    MaskMode::LumaInverse => 4.0,
                };
            }
            None => {
                self.draw_vars.set_texture(1, texture);
                self.mask_mode = 0.0;
            }
        }
        self.opacity = opacity as f32;
        self.begin_many_instances(cx);
    }

    fn triangle(&mut self, cx: &mut Cx2d, vertices: [[f64; 4]; 3]) {
        let v = |p: [f64; 4]| vec4(p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32);
        self.t0 = v(vertices[0]);
        self.t1 = v(vertices[1]);
        self.t2 = v(vertices[2]);
        let min = [vertices.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min), vertices.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)];
        let max = [vertices.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max), vertices.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max)];
        self.draw_abs(cx, Rect { pos: dvec2(min[0], min[1]), size: dvec2(max[0] - min[0], max[1] - min[1]) });
    }

    /// Draw a quad as two textured triangles, using the mesh sampling path.
    fn quad(&mut self, cx: &mut Cx2d, texture: &Texture, mask: Option<(&Texture, makepad_motion::model::MaskMode)>, m: &Affine, size: [f64; 2], uv: [f64; 4], opacity: f64) {
        self.q_clip = vec4(-1e20, -1e20, 1e20, 1e20);
        self.begin_image(cx, texture, mask, opacity);
        let vertex = |u: f64, v: f64| {
            let p = m.apply([u * size[0], v * size[1]]);
            [p[0], p[1], uv[0] + u * uv[2], uv[1] + v * uv[3]]
        };
        let corners = [vertex(0.0, 0.0), vertex(1.0, 0.0), vertex(1.0, 1.0), vertex(0.0, 1.0)];
        self.triangle(cx, [corners[0], corners[1], corners[2]]);
        self.triangle(cx, [corners[0], corners[2], corners[3]]);
        self.end_many_instances(cx);
    }

    /// One instanced triangle list. The draw-list buffer is reused by Cx2d;
    /// there is no per-frame geometry allocation or per-triangle draw call.
    fn mesh(&mut self, cx: &mut Cx2d, texture: &Texture, m: &Affine, mesh: &ImageMesh, uv: [f64; 4], clip: Option<[f64; 4]>, opacity: f64) {
        let clip = clip.unwrap_or([-1e20, -1e20, 1e20, 1e20]);
        self.q_clip = vec4(clip[0] as f32, clip[1] as f32, clip[2] as f32, clip[3] as f32);
        self.begin_image(cx, texture, None, opacity);
        let vertex = |i: u32| {
            let i = i as usize;
            let p = m.apply(mesh.positions[i]);
            [p[0], p[1], uv[0] + mesh.uvs[i][0] * uv[2], uv[1] + mesh.uvs[i][1] * uv[3]]
        };
        for tri in mesh.indices.chunks_exact(3) {
            self.triangle(cx, [vertex(tri[0]), vertex(tri[1]), vertex(tri[2])]);
        }
        self.end_many_instances(cx);
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMotionShape {
    #[deref]
    pub draw_super: DrawAffineQuad,
    #[live]
    pub s_fill: Vec4f,
    #[live]
    pub s_stroke: Vec4f,
    #[live]
    pub s_width: f32,
    #[live]
    pub s_radius: f32,
    #[live]
    pub s_px: f32,
    /// Gaussian sigma in target pixels and the coverage side (-1/0/1).
    #[live]
    pub s_sigma: f32,
    #[live]
    pub s_side: f32,
    #[live]
    pub s_kind: f32,
    /// An arc (`s_kind` 2): from, sweep, fraction of the sweep lit,
    /// thickness. A chamfered box (`s_kind` 3): each corner's cut.
    #[live]
    pub s_param: Vec4f,
    #[live]
    pub opacity: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMotionVectorEmit {
    #[deref]
    pub draw_super: DrawVector,
}

/// `DrawMotionBlend::mode`: a layer over what is below it.
pub mod blend_mode {
    pub const NORMAL: f32 = 1.0;
    pub const ADD: f32 = 2.0;
    pub const SCREEN: f32 = 3.0;
    pub const MULTIPLY: f32 = 4.0;
    /// `amount` of the layer, the rest of what is below.
    pub const DISSOLVE: f32 = 5.0;
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMotionBlend {
    #[deref]
    pub draw_super: DrawQuad,
    /// One of [`blend_mode`].
    #[live]
    pub mode: f32,
    #[live]
    pub amount: f32,
    /// 1 in an HDR frame: light over 1 adds up instead of clipping.
    #[live]
    pub hdr: f32,
}

/// The drawing resources: shaders and the text draw (its faces are made
/// from `makepad_doc_fonts` as documents name them, see `text::face`).
#[derive(Script, ScriptHook)]
pub struct MotionDraws {
    #[live]
    vector: DrawVector,
    #[live]
    vector_emit: DrawMotionVectorEmit,
    #[live]
    svg: DrawSvg,
    #[live]
    quad: DrawMotionQuad,
    #[live]
    shape: DrawMotionShape,
    #[live]
    blend: DrawMotionBlend,
    #[live]
    text: DrawText,
}

/// Axis-aligned bounds `[x0, y0, x1, y1]` of a box of `size` placed by `m`.
pub fn aabb(m: &Affine, size: [f64; 2]) -> [f64; 4] {
    let corners = [m.apply([0.0, 0.0]), m.apply([size[0], 0.0]), m.apply([0.0, size[1]]), m.apply([size[0], size[1]])];
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for c in corners {
        b = [b[0].min(c[0]), b[1].min(c[1]), b[2].max(c[0]), b[3].max(c[1])];
    }
    b
}

/// Where a picture of `natural` size goes in a box of `size` by `fit`:
/// the quad's offset and size inside the box, and the picture's uv
/// rectangle (x, y, w, h in 0..1).
pub fn fit_quad(size: [f64; 2], natural: [f64; 2], fit: Fit) -> ([f64; 2], [f64; 2], [f64; 4]) {
    let (bw, bh) = (size[0].max(1e-9), size[1].max(1e-9));
    let (nw, nh) = (natural[0].max(1e-9), natural[1].max(1e-9));
    match fit {
        Fit::Stretch => ([0.0, 0.0], size, [0.0, 0.0, 1.0, 1.0]),
        Fit::Fit => {
            let s = (bw / nw).min(bh / nh);
            let (w, h) = (nw * s, nh * s);
            ([(bw - w) * 0.5, (bh - h) * 0.5], [w, h], [0.0, 0.0, 1.0, 1.0])
        }
        Fit::Fill => {
            let s = (bw / nw).max(bh / nh);
            let (uw, uh) = (bw / (nw * s), bh / (nh * s));
            ([0.0, 0.0], size, [(1.0 - uw) * 0.5, (1.0 - uh) * 0.5, uw, uh])
        }
        Fit::None => {
            let (w, h) = (nw.min(bw), nh.min(bh));
            ([(bw - w) * 0.5, (bh - h) * 0.5], [w, h], [(1.0 - w / nw) * 0.5, (1.0 - h / nh) * 0.5, w / nw, h / nh])
        }
    }
}

/// The same document-to-target map used by `MotionDraw::draw_fit`.
/// A host can compose it with a raster scale without changing authored units.
pub fn fit_transform(size: [f64; 2], natural: [f64; 2], fit: Fit) -> Affine {
    let (offset, fitted, _) = fit_quad(size, natural, fit);
    if fit == Fit::Fill {
        let scale = (size[0] / natural[0].max(1e-9)).max(size[1] / natural[1].max(1e-9));
        Affine::translate((size[0] - natural[0] * scale) * 0.5, (size[1] - natural[1] * scale) * 0.5).then(&Affine::scale(scale, scale))
    } else {
        Affine::translate(offset[0], offset[1]).then(&Affine::scale(fitted[0] / natural[0].max(1e-9), fitted[1] / natural[1].max(1e-9)))
    }
}

/// Decode a PNG or JPEG (on a worker) into what [`MotionDraw::set_image`]
/// takes: tight BGRA pixels, width, height.
pub fn decode_image(bytes: &[u8]) -> Result<(Vec<u32>, u32, u32), String> {
    let image: ImageBuffer = decode_image_from_data(bytes).map_err(|e| format!("{e:?}"))?;
    Ok((image.data, image.width as u32, image.height as u32))
}

/// Recordings a pooled target stays allocated without a use.
const TARGET_IDLE_FRAMES: u32 = 30;

/// A target's format: linear half-float in an HDR frame, else 8-bit.
pub(crate) fn target_format(hdr: bool) -> RenderTargetFormat {
    if hdr { RenderTargetFormat::Rgba16f } else { RenderTargetFormat::Bgra8 }
}

/// A recorded frame: its target and its first and last pass, for a host
/// that orders passes itself (the edit compositor).
#[derive(Clone)]
pub struct Drawn {
    pub texture: Texture,
    pub first: DrawPassId,
    pub last: DrawPassId,
    /// Whether everything the frame shows was ready when it was recorded
    /// (every glyph rasterised, every host resource and effect there).
    /// A frame that is not exact is recorded again once it painted: text a
    /// face draws for the first time only has its glyphs the frame after.
    pub exact: bool,
}

/// What the frame stall watch observes between checks.
pub struct LoadProgress {
    /// While work the frame needs is running, the stall clock stays at zero.
    pub working: bool,
    /// A change in this value restarts the stall clock. Repainting the same state leaves it alone.
    pub mark: u64,
}

pub use scene3d::{SceneDoc, SceneLoad, SceneLoaded};

/// The warm draw's blur (pixels): over the kit's blur per level, so it
/// runs every program a large blur does.
const WARM_BLUR_SIGMA: f64 = 24.0;

/// A host resource a frame needs and does not have.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Need {
    Image(Media),
    Svg(Media),
    Model(Media),
    /// The frame of the Video layer at `path` (and its source time).
    VideoFrame(String, Media),
    /// The source of a library VjEffect a Shader layer runs.
    Effect(makepad_motion::model::AssetRef),
    /// The host's picture at host time `.1` for the Freeze item at `.0`
    /// (only with `MotionContext::edit_frames`); given by
    /// [`MotionDraw::set_video_frame`] with that path.
    EditFrame(String, u64),
}

// The standalone editor/compiler entry points have no MotionDraw argument.
// They must also refuse document code throughout a plain recording on this thread.
thread_local! {
    static PLAIN_RECORDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A VM-free picture from the peer reader or the full loader.
pub enum PlainPicture<'a> {
    Data(&'a PlainFrame),
    Loaded(&'a LoadedPlainFrame),
}

impl<'a> From<&'a PlainFrame> for PlainPicture<'a> {
    fn from(frame: &'a PlainFrame) -> Self { Self::Data(frame) }
}

impl<'a> From<&'a LoadedPlainFrame> for PlainPicture<'a> {
    fn from(frame: &'a LoadedPlainFrame) -> Self { Self::Loaded(frame) }
}

// Text and vector painting borrow the sampled values from either item.
trait PaintItem {
    fn primitive(&self) -> Option<&PrimitiveFrame>;
    fn path(&self) -> &str;
    fn world(&self) -> &Affine;
    fn local(&self) -> &Local;
    fn feather(&self) -> f64;
    fn feather_side(&self) -> makepad_motion::FeatherSide;
}

// A group's children and mask belong to its own tree.
trait CanvasItem: PaintItem + Clone + std::fmt::Debug {
    fn rich(&self) -> Option<&Item>;
    fn group(&self) -> Option<(&[Self], Option<&Self>, makepad_motion::MaskMode)>;
    fn is_static(&self) -> bool;
    fn opacity(&self) -> f64;
    fn blur(&self) -> f64;
    fn blend(&self) -> Blend;
    fn effects(&self) -> &[EffectFrame];
    fn without_blend(&self) -> Self;
    fn before_effects(&self) -> Self;
}

macro_rules! canvas_item {
    ($item:ty, $kind:ident, $this:ident, $rich:expr) => {
        impl PaintItem for $item {
            fn primitive(&self) -> Option<&PrimitiveFrame> {
                match &self.kind { $kind::Primitive(shape) => Some(shape), _ => None }
            }
            fn path(&self) -> &str { &self.path }
            fn world(&self) -> &Affine { &self.world }
            fn local(&self) -> &Local { &self.local }
            fn feather(&self) -> f64 { self.feather }
            fn feather_side(&self) -> makepad_motion::FeatherSide { self.feather_side }
        }

        impl CanvasItem for $item {
            fn rich(&$this) -> Option<&Item> { $rich }
            fn group(&self) -> Option<(&[Self], Option<&Self>, makepad_motion::MaskMode)> {
                match &self.kind {
                    $kind::Group { children, mask, mask_mode, .. } => Some((children, mask.as_deref(), *mask_mode)),
                    _ => None,
                }
            }
            fn is_static(&self) -> bool { self.is_static }
            fn opacity(&self) -> f64 { self.opacity }
            fn blur(&self) -> f64 { self.blur }
            fn blend(&self) -> Blend { self.blend }
            fn effects(&self) -> &[EffectFrame] { &self.effects }
            fn without_blend(&self) -> Self {
                let mut layer = self.clone();
                layer.blend = Blend::Normal;
                layer
            }
            fn before_effects(&self) -> Self {
                let mut layer = self.without_blend();
                layer.blur = 0.0;
                layer.opacity = 1.0;
                layer.effects.clear();
                layer
            }
        }
    };
}

use plain::ItemKind as PlainItemKind;
canvas_item!(Item, ItemKind, self, Some(self));
canvas_item!(plain::Item, PlainItemKind, self, None);

struct PlainRecording<'a> {
    draw: &'a mut MotionDraw,
    previous_draw: bool,
    previous_thread: bool,
}

impl Drop for PlainRecording<'_> {
    fn drop(&mut self) {
        self.draw.plain = self.previous_draw;
        PLAIN_RECORDING.set(self.previous_thread);
    }
}

pub struct MotionDraw {
    plain: bool,
    /// Refusals already logged, by document path, for the life of this draw.
    plain_errors: std::collections::BTreeMap<String, String>,
    /// Meshes this recording asked for past what can be made (memory, one
    /// GPU buffer), each said once: "too much geometry: N vertices in
    /// layer X". The shapes before that point draw; a recording without
    /// them clears it.
    geometry_errors: Vec<String>,
    /// The layer being drawn (whose shapes a vector session holds).
    layer_path: String,
    /// The layer the open vector session's shapes are of.
    vector_layer: String,
    /// Pick ids (`Cx::set_pick_ids`): what each id names, the item's path
    /// and the Draw2D op (`None`: the item as a whole); ids are this
    /// list's indices plus one, kept from frame to frame.
    pick_names: Vec<(String, Option<usize>)>,
    pick_ids: HashMap<(String, Option<usize>), u32>,
    /// The item being drawn, its id.
    pick_path: String,
    draws: MotionDraws,
    pool: RenderTargetPool,
    spans: Vec<(DrawPassId, DrawPassId)>,
    passes: Vec<DrawPassId>,
    /// Document units to target pixels for the frame being drawn.
    base: Affine,
    pixels: (u32, u32),
    transparent: Texture,
    images: HashMap<Media, (Texture, [f64; 2])>,
    svgs: HashMap<Media, (DrawSvg, [f64; 2])>,
    videos: HashMap<String, (Texture, [f64; 2])>,
    shaders: shader::Shaders,
    widgets: widget::Widgets,
    scenes: scene::Scenes,
    /// The built-in `Draw2D` layers' line renderer and draw lists.
    draw2d: draw2d::Draw2dDraws,
    /// Full three.js-level scenes (motion3d).
    scenes3d: scene3d::Scenes3d,
    /// The canvases 2D layers are drawn into for 3D scenes (`layers:`).
    #[cfg_attr(not(feature = "scene3d"), allow(dead_code))]
    layer_targets: HashMap<String, RenderTarget>,
    /// SVGs as editable parts.
    vectors: vector::Vectors,
    /// Whether a DrawVector session is open: consecutive shapes share one
    /// mesh and one draw call (a thousand Rects cost one upload, not a
    /// thousand), flushed before anything else draws.
    vector_open: bool,
    /// Textures prepared for this frame by item path (and text unit).
    prepared: HashMap<(String, usize), Texture>,
    /// The kept picture of the frame's static bottom layers.
    static_cache: Option<((u64, bool), RenderTarget)>,
    recorded: Option<(u64, Vec<DrawPassId>, Vec<String>)>,
    last: Option<DrawPassId>,
    output: Option<Texture>,
    exact: bool,
    /// Why the last recording was not exact (for [`Self::debug_state`]).
    inexact: String,
    /// Host resources the last recording still needed.
    missing_at_record: Vec<Need>,
    /// Glyphs drawn so far, by face and pixel size (see `text::note_glyphs`).
    glyphs_seen: std::collections::HashSet<(u64, char)>,
    fresh_glyphs: bool,
    /// What last set `fresh_glyphs` (for [`Self::debug_state`]).
    fresh_why: &'static str,
    /// Draw shaders used so far. A shader's first draws can be left out
    /// while its pipeline builds, without the pass showing it: a recording
    /// that uses one for the first time is not exact.
    kinds_seen: std::collections::HashSet<u64>,
    /// R2's render kit: blur and `effects: [...]`.
    kit: RenderKit,
    /// What an `@backdrop` Image magnifies: the frame under the layer (or
    /// under the group that holds it), else what the host set.
    backdrop: Option<Texture>,
    host_backdrop: Option<Texture>,
    /// The frame being drawn: its time, rate and comp camera.
    frame_t: f64,
    fps: f64,
    camera: Option<KitCamera>,
    kit_errors: Vec<String>,
    /// A warm draw (unseen, ahead of play, [`MotionDraw::set_warm`]): the
    /// render-kit effects the document declares, drawn once at their
    /// defaults; and whether this recording drew its warm programs yet.
    warm: Option<Vec<String>>,
    warm_drawn: bool,
    /// The frame being drawn is linear half-float light (`color: @hdr`).
    hdr: bool,
    /// Locked time (an export): whole-frame motion blur samples the
    /// shutter; realtime (a preview) draws one frame.
    locked: bool,
    /// The document's frames at any time (sub-frames), from the host.
    frames: Option<std::sync::Arc<dyn Fn(f64) -> MotionFrame + Send + Sync>>,
    /// The HDR frame's post: the document's passes, the accumulator, the
    /// tone map into an 8-bit target.
    graph: makepad_render_graph::GraphRunner,
    accum: makepad_render_graph::Accumulator,
    /// The frame being accumulated (its time, size and placement), its
    /// open shutter and its frame-point frame (post reads it).
    accumulating: Option<(u64, f64, MotionFrame)>,
    /// The last frame the shutter finished (its key, average and frame-point
    /// frame): recorded again (the host's frame is not whole yet for
    /// another of its parts), it posts its kept average again instead of
    /// gathering the shutter a second time.
    finished: Option<(u64, Texture, MotionFrame)>,
    /// Pictures the host gives this frame (an edit's picture under a
    /// Freeze) are not final yet: what is drawn with them is not exact, and
    /// no sub-frame showing them is summed ([`MotionDraw::set_inputs_pending`]).
    inputs_pending: Option<String>,
    last_drawn: Option<Drawn>,
    tonemap: Option<Box<makepad_render_graph::DrawToneMap>>,
    tone_target: Option<RenderTarget>,
    post_errors: Vec<String>,
    /// This frame's groups' own post passes (`Group{post: [...]}`), by the
    /// group's path, and a graph runner each (kept while the group is).
    group_posts: HashMap<String, (std::sync::Arc<Vec<makepad_render_graph::PassDecl>>, Vec<makepad_render_graph::PassValues>)>,
    group_graphs: HashMap<String, makepad_render_graph::GraphRunner>,
    /// Data layers' textures by name, and whether the canvas being drawn
    /// takes colours as they are (a data layer: no colour management).
    data_targets: HashMap<String, RenderTarget>,
    raw_colors: bool,
}

/// Compile every inline Shader layer of `doc` ([`compile_shader`]): the
/// errors, located at each layer's `pixel:` field. A publish check.
pub fn check_shaders(cx: &mut Cx, doc: &makepad_motion::model::MotionDoc) -> Vec<makepad_motion::model::Diagnostic> {
    let mut out = Vec::new();
    for layer in doc.all_layers() {
        let makepad_motion::model::LayerKind::Shader { program: ShaderProgram::Inline { source, helpers, loc, .. }, params, .. } = &layer.kind else { continue };
        let names: Vec<String> = params.iter().map(|(name, _)| name.clone()).collect();
        if let Err(error) = compile_shader(cx, source, helpers, &names) {
            out.push(makepad_motion::model::Diagnostic {
                severity: makepad_motion::model::Severity::Error,
                file: doc.script_file(),
                loc: *loc,
                path: format!("{}.pixel", layer.path),
                message: error,
                hint: Some("the pixel function is Makepad shader code: `self.pos` (0..1 in the box), `self.time`, `self.beat`, `self.size`, `self.<param>`, `sample2d(self.below, uv)` with `input: @below`, `self.data_float(i)` / `self.data_vec4(i)` with `data: [...]`; return a vec4".into()),
            });
        }
    }
    out
}

/// `prepared` keys: a whole item's blurred picture.
const BLURRED: usize = usize::MAX;

impl MotionDraw {
    /// Gives back every GPU target this draw holds (scenes, pooled canvases, layer and data
    /// targets, the kept static picture, the post graph's), for a host that stopped drawing it:
    /// the next recording allocates what it needs again. Documents, images and compiled
    /// shaders stay.
    pub fn release_gpu(&mut self, cx: &mut Cx) {
        self.scenes3d.release_all(cx);
        self.pool.release_all(cx);
        for target in self.layer_targets.values_mut().chain(self.data_targets.values_mut()).chain(self.tone_target.iter_mut()) {
            target.release(cx);
        }
        if let Some((key, target)) = &mut self.static_cache {
            target.release(cx);
            key.0 = 0;
        }
        self.graph = Default::default();
        self.accum = Default::default();
        self.finished = None;
        self.group_graphs.clear();
        self.recorded = None;
        self.last_drawn = None;
        self.output = None;
    }

    pub fn new(cx: &mut Cx) -> Self {
        let draws = cx.with_vm(|vm| MotionDraws::script_new_with_default(vm));
        let transparent = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
            width: 2, height: 2, data: Some(vec![0; 4]), updated: TextureUpdated::Full,
        });
        Self {
            plain: false, plain_errors: Default::default(),
            geometry_errors: Vec::new(), layer_path: String::new(), vector_layer: String::new(),
            pick_names: Vec::new(), pick_ids: HashMap::new(), pick_path: String::new(),
            draws, pool: RenderTargetPool::new(TARGET_IDLE_FRAMES), spans: Vec::new(), passes: Vec::new(), base: Affine::IDENTITY, pixels: (1, 1), transparent,
            images: HashMap::new(), svgs: HashMap::new(), videos: HashMap::new(),
            shaders: shader::Shaders::default(), widgets: widget::Widgets::default(), scenes: scene::Scenes::default(),
            draw2d: Default::default(), scenes3d: Default::default(), layer_targets: HashMap::new(), vectors: Default::default(), vector_open: false,
            prepared: HashMap::new(), static_cache: None, recorded: None, last: None, output: None, exact: false,
            glyphs_seen: Default::default(), fresh_glyphs: false, fresh_why: "", kinds_seen: Default::default(), inexact: String::new(), missing_at_record: Vec::new(),
            kit: RenderKit::new(cx), backdrop: None, host_backdrop: None,
            frame_t: 0.0, fps: 60.0, camera: None, kit_errors: Vec::new(), warm: None, warm_drawn: false,
            hdr: false, locked: false, frames: None, graph: Default::default(), accum: Default::default(), accumulating: None, finished: None, inputs_pending: None, last_drawn: None,
            tonemap: None, tone_target: None, post_errors: Vec::new(), data_targets: HashMap::new(), raw_colors: false,
            group_posts: HashMap::new(), group_graphs: HashMap::new(),
        }
    }

    /// The Scene3D documents `frame` shows to load ahead of their first
    /// draw (CPU only: no GPU resources), so that draw, in this or any later
    /// `MotionDraw`, starts at once instead of evaluating the scene and
    /// lowering its kernels (hundreds of ms for a large scene on a
    /// browser's UI thread): each a load to run on a worker
    /// ([`SceneLoad::run`]) and give back with [`Self::adopt_scene`]. A
    /// scene loading, loaded or drawing is not asked for again. A scene
    /// that leaves a draw keeps its document the same way.
    pub fn scene_loads(&self, frame: &MotionFrame) -> Vec<SceneLoad> {
        if self.plain { return Vec::new(); }
        scene3d::scene_loads(self, frame)
    }

    /// A scene a worker loaded ([`Self::scene_loads`]), kept for its first
    /// draw; the shared CPU work that draw would do, for the host to run
    /// before drawing it, on a worker where it has one. There is none now
    /// (None): the environment's prefilter was that work, and the renderer
    /// prepares the environment itself, off the UI thread, on the first
    /// draw.
    pub fn adopt_scene(loaded: SceneLoaded) -> Option<Box<dyn FnOnce() + Send>> {
        scene3d::adopt_loaded(loaded)
    }

    /// Scene3D documents kept between draws ([`Self::scene_loads`],
    /// or left by a draw that went) hold their static meshes' uploaded
    /// geometry, so their next draw uploads nothing; those whose scene
    /// starts (on the edit's clock) where `keep` says no give it back.
    pub fn keep_scene_geometry(keep: impl Fn(f64) -> bool) {
        scene3d::keep_geometry(&keep);
    }

    /// Kept Scene3D documents whose scene starts (on the edit's clock)
    /// where `keep` says no go entirely; they load again ahead of their
    /// next draw ([`Self::scene_loads`]). Their documents are returned, for
    /// the host to drop off the UI thread.
    pub fn keep_scenes(keep: impl Fn(f64) -> bool) -> Vec<SceneDoc> {
        scene3d::keep_scenes(&keep)
    }

    /// An image a frame's Image layers show (BGRA, tight rows).
    pub fn set_image(&mut self, cx: &mut Cx, media: Media, pixels: Vec<u32>, width: u32, height: u32) {
        let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
            width: width as usize, height: height as usize, data: Some(pixels), updated: TextureUpdated::Full,
        });
        self.images.insert(media, (texture, [width as f64, height as f64]));
    }

    /// An image already on the GPU.
    pub fn set_image_texture(&mut self, media: Media, texture: Texture, width: u32, height: u32) {
        self.images.insert(media, (texture, [width as f64, height as f64]));
    }

    /// The markup of an Svg layer's asset.
    pub fn set_svg(&mut self, cx: &mut Cx, media: Media, markup: &str) {
        let mut svg = cx.with_vm(|vm| DrawSvg::script_new_with_default(vm));
        svg.load_from_str(markup);
        let size = svg.svg_size().map_or([0.0, 0.0], |s| [s.x, s.y]);
        self.vectors.markups.insert(media.clone(), markup.into());
        self.svgs.insert(media, (svg, size));
    }

    /// The current frame of the Video layer at `path` (a reader's picture
    /// at the item's `source_time`), BGRA.
    pub fn set_video_frame(&mut self, path: &str, texture: Texture, width: u32, height: u32) {
        self.videos.insert(path.to_string(), (texture, [width as f64, height as f64]));
    }

    /// What `@backdrop` Images show under everything the document draws
    /// (an edit's picture under a graphic). Without it the backdrop is the
    /// document's own lower layers.
    /// The pictures the host gives for the next draw (its video frames, an
    /// edit's picture under a Freeze) are not final yet (`Some(why)`): a
    /// shutter gathering in locked time is still being drawn by the host.
    /// The draw is then not exact, and a motion-blurred frame sums no
    /// sub-frame until they are.
    pub fn set_inputs_pending(&mut self, why: Option<String>) {
        self.inputs_pending = why;
    }

    pub fn set_backdrop(&mut self, texture: Option<Texture>) {
        self.host_backdrop = texture;
    }

    /// A Scene3D's model file (GLB bytes).
    /// A library file a full three.js-level Scene3D names (`asset(id,
    /// rev)`: a GLB, an HDR, an image, a font). Give them before the scene
    /// first draws ([`makepad_motion::MotionDoc::dependencies`] lists them).
    /// Given as an `Arc`, the bytes are shared with the caller (a host
    /// handing one file to several draws keeps one copy).
    pub fn set_file(&mut self, asset: makepad_motion::model::AssetRef, bytes: impl Into<std::sync::Arc<Vec<u8>>>) {
        let bytes: std::sync::Arc<Vec<u8>> = bytes.into();
        self.scenes3d.files.insert(format!("{}@{}", asset.id, asset.rev), bytes.clone());
        self.scenes3d.files.insert(asset.id, bytes);
    }

    pub fn set_model(&mut self, cx: &mut Cx, media: Media, glb: &[u8]) -> Result<(), String> {
        self.scenes.set_model(cx, media, glb)
    }

    /// What `frame` needs from the host that has not been given.
    pub fn missing(&self, frame: &MotionFrame) -> Vec<Need> {
        self.missing_in_mode(frame, self.plain)
    }

    fn missing_in_mode(&self, frame: &MotionFrame, plain: bool) -> Vec<Need> {
        let mut out = Vec::new();
        for (_, item) in frame.walk() {
            let need = match &item.kind {
                ItemKind::Image { src: src @ Media::Asset(_), .. } if !self.images.contains_key(src) => Need::Image(src.clone()),
                ItemKind::Svg { src: src @ Media::Asset(_), .. } if !self.svgs.contains_key(src) => Need::Svg(src.clone()),
                ItemKind::Vector { src: src @ Media::Asset(_), .. } if !self.vectors.markups.contains_key(src) => Need::Svg(src.clone()),
                ItemKind::Scene3D { model: src @ Media::Asset(_), .. } if !plain && !self.scenes.has(src) => Need::Model(src.clone()),
                ItemKind::Video { src: src @ Media::Asset(_), .. } if !self.videos.contains_key(&item.path) => Need::VideoFrame(item.path.clone(), src.clone()),
                ItemKind::Video { src: Media::Backdrop, source_time, .. } if !self.videos.contains_key(&item.path) => Need::EditFrame(item.path.clone(), source_time.to_bits()),
                ItemKind::Scene3dDoc { textures, videos, .. } if !plain => {
                    for (_, media) in textures {
                        if matches!(media, Media::Asset(_)) && !self.images.contains_key(media) && !out.contains(&Need::Image(media.clone())) {
                            out.push(Need::Image(media.clone()));
                        }
                    }
                    for (name, media) in videos {
                        let key = scene3d::video_key(&item.path, name);
                        if matches!(media, Media::Asset(_)) && !self.videos.contains_key(&key) {
                            out.push(Need::VideoFrame(key, media.clone()));
                        }
                    }
                    continue;
                }
                ItemKind::Shader { .. } if !plain => match self.shaders.needs_source(item) {
                    Some(asset) => Need::Effect(asset),
                    None => continue,
                },
                _ => continue,
            };
            if !out.contains(&need) {
                out.push(need);
            }
        }
        out
    }

    /// Effects still building, models still loading their textures.
    pub fn ready(&self) -> bool {
        self.shaders.ready() && self.scenes.ready()
    }

    /// Problems found while drawing (shader compile errors, effect builds,
    /// widget evaluation), each once, with the layer path.
    pub fn errors(&self) -> Vec<String> {
        let mut out = self.draw_errors();
        out.extend(self.plain_errors.values().cloned());
        out
    }

    fn draw_errors(&self) -> Vec<String> {
        let mut out = self.shaders.errors();
        out.extend(self.kit_errors.iter().cloned());
        out.extend(self.widgets.errors());
        out.extend(self.scenes.errors());
        out.extend(self.scenes3d.errors());
        out.extend(self.post_errors.iter().cloned());
        out.extend(self.geometry_errors.iter().cloned());
        out
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.shaders.handle_event(cx, event);
        self.widgets.handle_event(cx, event);
        self.scenes.handle_event(cx, event);
        self.scenes3d.handle_event(cx, event);
    }

    /// An item's box in document units, `[x0, y0, x1, y1]` (axis-aligned
    /// around its transformed shape): text measured with the fonts it
    /// draws with, paths by their outline (plus half the stroke), groups
    /// as the union of their children. For an editor's selection boxes
    /// (lane M5). `None`: nothing to measure (empty text or group).
    /// The pixel-space zoom of a FlyThrough: `zoom` times about the letter
    /// (identity when the letter is not found or not zoomed).
    fn fly_camera(&mut self, cx: &mut Cx2d, inner: &Item, target: &str, letter: usize, zoom: f64) -> Affine {
        if zoom <= 1.0 + 1e-9 {
            return Affine::IDENTITY;
        }
        fn find<'a>(item: &'a Item, target: &str) -> Option<&'a Item> {
            if item.path == target && matches!(item.kind, ItemKind::Primitive(PrimitiveFrame::Text { .. })) {
                return Some(item);
            }
            match &item.kind {
                ItemKind::Group { children, .. } => children.iter().find_map(|c| find(c, target)),
                ItemKind::Fly { inner, .. } => find(inner, target),
                _ => None,
            }
        }
        let Some(text) = find(inner, target).cloned() else { return Affine::IDENTITY };
        let Some(p) = text::letter_point(self, cx, &text, letter) else { return Affine::IDENTITY };
        Affine::translate(p[0], p[1]).then(&Affine::scale(zoom, zoom)).then(&Affine::translate(-p[0], -p[1]))
    }

    pub fn item_bounds(&mut self, cx: &mut Cx2d, item: &Item) -> Option<[f64; 4]> {
        match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Text { .. }) => {
                let layout = text::layout(self, cx, item)?;
                if layout.size[0] <= 0.0 && layout.size[1] <= 0.0 {
                    return None;
                }
                let m = item.world.then(&Affine::translate(layout.shift[0], layout.shift[1])).then(&item.local.affine(layout.size));
                Some(aabb(&m, layout.size))
            }
            ItemKind::Primitive(PrimitiveFrame::Path { path, stroke, width, .. }) => {
                let size = geometry::bounds(path).map_or([0.0, 0.0], |b| [b[2].max(0.0), b[3].max(0.0)]);
                let m = item.transform(size);
                let b = geometry::bounds(&geometry::transformed(path, &m))?;
                let pad = if stroke.is_some() { width * 0.5 * m.mean_scale() } else { 0.0 };
                Some([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad])
            }
            ItemKind::Image { mesh: Some(mesh), size, .. } => {
                let m = item.transform(size.unwrap_or([0.0, 0.0]));
                let mut bounds: Option<[f64; 4]> = None;
                for p in &mesh.positions {
                    let p = m.apply(*p);
                    bounds = Some(bounds.map_or([p[0], p[1], p[0], p[1]], |b| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]));
                }
                bounds
            }
            ItemKind::Group { children, size: None, .. } => {
                let mut out: Option<[f64; 4]> = None;
                for child in children {
                    if let Some(b) = self.item_bounds(cx, child) {
                        out = Some(out.map_or(b, |o| [o[0].min(b[0]), o[1].min(b[1]), o[2].max(b[2]), o[3].max(b[3])]));
                    }
                }
                out
            }
            _ => {
                let size = item.box_size()?;
                Some(aabb(&item.transform(size), size))
            }
        }
    }

    /// The target the last recorded frame lands in.
    pub fn output(&self) -> Option<&Texture> {
        self.output.as_ref()
    }

    /// A pooled target of `size` for one use in this recording: a free one
    /// of that size and format if there is one (no new texture), else a
    /// released one or one idle for two recordings, resized, else a new one. The same layers ask for the same
    /// sizes every frame, so a steady scene allocates nothing; an item that
    /// appears or goes does not shift every other item's target.
    fn target(&mut self, cx: &mut Cx, size: (u32, u32), name: &'static str) -> usize {
        let index = self.pool.take(cx, size, target_format(self.hdr), name);
        // A mip chain is asked for per use (a Scene3D layer's picture).
        self.pool[index].texture.set_render_mips(cx, false);
        index
    }

    /// Note a draw shader (or a per-layer program) this recording uses.
    fn note_kind(&mut self, kind: &str) {
        if self.kinds_seen.insert(LiveId::from_str(kind).0) {
            self.fresh_glyphs = true;
            self.fresh_why = "a draw kind's first use";
        }
    }

    fn record(&mut self, id: DrawPassId) {
        self.spans.push((id, id));
        self.passes.push(id);
    }

    /// The pixel map of an item's own box of `size`.
    fn pixel_map(&self, item: &dyn PaintItem, size: [f64; 2]) -> Affine {
        self.base.then(&item.world().then(&item.local().affine(size)))
    }

    /// The pixel scale an item drawn into its own target (a 3D scene, a
    /// widget, a vector, an effect, a rotated SVG) renders at: its on-screen
    /// scale rounded up to a step of 2^(1/8) (about 9 %). An animated zoom
    /// then changes the target's size a few times per doubling, not every
    /// frame: a size change re-creates the target's textures, and
    /// re-creating them every frame outran their release (the GPU frees a
    /// texture only after the frames that used it).
    fn layer_scale(&self, _size: [f64; 2], scale: f64) -> f64 {
        // (A scale a rounding error above a step stays on it.)
        if scale > 0.0 && scale.is_finite() { ((scale.log2() * 8.0 - 1e-3).ceil() / 8.0).exp2() } else { 1.0 }
    }

    /// Record `frame` at `pixels` (the document stretched to them; give
    /// the document's aspect) into passes under the pass being drawn.
    /// Returns the target and the first and last pass of the chain.
    pub fn draw(&mut self, cx: &mut Cx2d, frame: &MotionFrame, pixels: (u32, u32)) -> Drawn {
        let base = Affine::scale(pixels.0.max(1) as f64 / frame.size[0].max(1e-9), pixels.1.max(1) as f64 / frame.size[1].max(1e-9));
        self.draw_placed(cx, frame, pixels, base)
    }

    /// Record `frame` into a target of `pixels`, the document placed in it
    /// by `fit` (@fit letterboxes over transparent, @fill crops to cover,
    /// @none is 1:1 centred, @stretch fills). What a host with its own
    /// output size (an edit) draws a graphic with.
    pub fn draw_fit(&mut self, cx: &mut Cx2d, frame: &MotionFrame, pixels: (u32, u32), fit: Fit) -> Drawn {
        let base = fit_transform([pixels.0 as f64, pixels.1 as f64], frame.size, fit);
        self.draw_placed(cx, frame, pixels, base)
    }

    /// Draw a frame read as data at `base`. A widget, shader or 3D scene
    /// draws as a neutral tile. Refusals are reported once per item path.
    pub fn draw_plain<'a>(&mut self, cx: &mut Cx2d, frame: impl Into<PlainPicture<'a>>, pixels: (u32, u32), base: Affine) -> Drawn {
        let previous_draw = std::mem::replace(&mut self.plain, true);
        let guard = PlainRecording { draw: self, previous_draw, previous_thread: PLAIN_RECORDING.replace(true) };
        match frame.into() {
            PlainPicture::Loaded(frame) => guard.draw.draw_placed(cx, frame.frame(), pixels, base),
            PlainPicture::Data(frame) => {
                let frame = frame.frame();
                let draw = &mut *guard.draw;
                draw.begin_ldr();
                let (texture, exact, asked) = draw.record_items(cx, &frame.items, frame.t, frame.fps, frame.size,
                    frame.background, None, pixels, base);
                draw.finish_record(cx, texture, exact, asked)
            }
        }
    }

    /// Draw a VM-free picture with the placement used by [`Self::draw_fit`].
    pub fn draw_plain_fit<'a>(&mut self, cx: &mut Cx2d, frame: impl Into<PlainPicture<'a>>, pixels: (u32, u32), fit: Fit) -> Drawn {
        let frame = frame.into();
        let size = match &frame {
            PlainPicture::Loaded(frame) => frame.frame().size,
            PlainPicture::Data(frame) => frame.frame().size,
        };
        self.draw_plain(cx, frame, pixels, fit_transform([pixels.0 as f64, pixels.1 as f64], size, fit))
    }

    fn plain_refusal(&mut self, path: &str, what: &str, with_post: bool) {
        if !self.plain_errors.contains_key(path) {
            let suffix = if with_post { " with its post pass" } else { "" };
            let message = format!("motion: {path} is not drawn{suffix}: {what} runs code, and this picture was read as data");
            makepad_widgets::log!("{message}");
            self.plain_errors.insert(path.to_owned(), message);
        }
    }

    /// Preparation sinks leave the tile to the item's turn in its canvas,
    /// where it has the correct transform, mask and stacking order.
    fn refuse_plain_item(&mut self, item: &Item) -> bool {
        if !self.plain {
            return false;
        }
        let what = match item.kind {
            ItemKind::Widget { .. } => "a widget",
            ItemKind::Shader { .. } => "a shader",
            ItemKind::Scene3D { .. } | ItemKind::Scene3dDoc { .. } => "a 3D scene",
            _ => return false,
        };
        self.plain_refusal(&item.path, what, false);
        true
    }

    fn plain_tile(&mut self, cx: &mut Cx2d, item: &Item) {
        let size = item.box_size().unwrap_or_default();
        let m = self.pixel_map(item, size);
        // Measure perpendicular to each edge in target pixels. Even a
        // stretched or sheared placement then has a one-pixel inside rim.
        let area = (m.a * m.d - m.b * m.c).abs();
        if area == 0.0 { return; }
        let x = area / m.c.hypot(m.d);
        let y = area / m.a.hypot(m.b);
        let placement = m.then(&Affine::scale(1.0 / x, 1.0 / y));
        self.flush_vector(cx);
        self.note_kind("vector");
        paint::sdf(cx, &mut self.draws.shape, &placement, [size[0] * x, size[1] * y], paint::Shape::Rect { radius: 0.0 },
            vec4(0.5, 0.5, 0.5, 1.0), vec4(0.25, 0.25, 0.25, 1.0), -1.0, 1.0,
            0.0, DrawFeatherSide::Both, 0.25 * item.opacity.clamp(0.0, 1.0));
    }

    /// Render into `pixels` with an explicit map from authored document units.
    pub fn draw_placed(&mut self, cx: &mut Cx2d, frame: &MotionFrame, pixels: (u32, u32), base: Affine) -> Drawn {
        if !self.plain && frame.hdr {
            return self.draw_hdr(cx, frame, pixels, base);
        }
        self.begin_ldr();
        let (texture, exact, asked) = self.record_frame(cx, frame, pixels, base);
        self.finish_record(cx, texture, exact, asked)
    }

    fn begin_ldr(&mut self) {
        self.hdr = false;
        self.accumulating = None;
        self.spans.clear();
        self.passes.clear();
    }

    /// Record `frame`'s layers into its canvas (linear half float when the
    /// frame is HDR). Returns the canvas, whether everything it shows was
    /// ready, and whether a redraw was asked before.
    fn record_frame(&mut self, cx: &mut Cx2d, frame: &MotionFrame, pixels: (u32, u32), base: Affine) -> (Texture, bool, bool) {
        self.record_items(cx, &frame.items, frame.t, frame.fps, frame.size, frame.background, Some(frame), pixels, base)
    }

    fn record_items(&mut self, cx: &mut Cx2d, items: &[impl CanvasItem], t: f64, fps: f64, size: [f64; 2],
                    background: Option<makepad_motion::Color>, loaded: Option<&MotionFrame>, pixels: (u32, u32), base: Affine) -> (Texture, bool, bool) {
        // A draw that is missing something asks for another redraw (text
        // whose glyphs are not rasterised yet does): watch for it.
        let asked = std::mem::replace(&mut cx.new_draw_event.redraw_all, false);
        let missing = loaded.map_or_else(Vec::new, |f| self.missing(f));
        let exact = missing.is_empty() && (self.plain || self.ready()) && self.inputs_pending.is_none();
        self.inexact = if !missing.is_empty() { format!("missing {missing:?}") } else if let Some(why) = &self.inputs_pending { why.clone() } else if !exact { format!("not ready (shaders {}, scenes {})", self.shaders.ready(), self.scenes.ready()) } else { String::new() };
        self.missing_at_record = missing;
        self.fresh_glyphs = false;
        // Targets this recording does not use go free; long-idle ones give
        // their texture back.
        self.pool.next_frame(cx);
        self.draw2d.begin_frame();
        self.prepared.clear();
        self.group_posts.clear();
        if let Some(frame) = loaded {
            if self.plain {
                for (path, _) in &frame.group_post {
                    self.plain_refusal(path, "a post pass", true);
                }
                for (present, path, what) in [
                    (frame.post.is_some(), "post", "a post pass"),
                    (frame.camera.is_some(), "camera", "a camera"),
                    (frame.motion_blur.is_some(), "motion_blur", "motion blur"),
                ] {
                    if present {
                        self.plain_refusal(path, what, false);
                    }
                }
            } else {
                self.group_posts = frame.group_post.iter().cloned().collect();
            }
        }
        let posted: std::collections::HashSet<String> = self.group_posts.keys().cloned().collect();
        self.group_graphs.retain(|path, _| posted.contains(path));
        self.scenes3d.begin_frame(cx);
        self.shaders.begin_frame();
        self.kit.begin_frame(cx);
        self.geometry_errors.clear();
        self.kit.set_hdr(self.hdr);
        self.warm_drawn = false;
        self.backdrop = None;
        self.frame_t = t;
        self.fps = fps;
        self.camera = loaded.and_then(|f| f.camera.as_ref()).filter(|_| !self.plain).and_then(|camera| camera_spec(camera).ok()).map(|spec| spec.at(t, [self.pixels.0 as f64, self.pixels.1 as f64]));
        self.pixels = (pixels.0.max(1), pixels.1.max(1));
        self.base = base;
        let clear = background.map_or(vec4(0.0, 0.0, 0.0, 0.0), |c| {
            let c = self.lin(c);
            vec4((c[0] * c[3]) as f32, (c[1] * c[3]) as f32, (c[2] * c[3]) as f32, c[3] as f32)
        });
        // The static bottom layers: kept, or drawn once more into the kept target.
        let prefix = items.iter().take_while(|i| i.is_static() && !matches!(i.rich().map(|r| &r.kind), Some(ItemKind::Shader { below: true, .. }))).count();
        let mut base: Option<Texture> = None;
        if prefix > 0 {
            let key = makepad_motion::frame::static_prefix_key(size, background, &items[..prefix]) ^ ((self.pixels.0 as u64) << 32 | self.pixels.1 as u64) ^ (self.hdr as u64) << 63
                ^ LiveId::from_str(&format!("{:?}", self.base)).0;
            let key = (key, self.plain);
            let fresh = self.static_cache.as_ref().is_some_and(|(k, _)| *k == key);
            if !fresh {
                let mut kept = match self.static_cache.take() {
                    Some((_, target)) => target,
                    None => RenderTarget::new(cx, "motion_static"),
                };
                kept.ensure_kept(cx, self.pixels, target_format(self.hdr));
                let texture = self.canvas_into(cx, &items[..prefix], clear, None, Some(&mut kept));
                self.static_cache = Some((key, kept));
                base = Some(texture);
            } else {
                base = self.static_cache.as_ref().map(|(_, t)| t.texture.clone());
            }
        }
        let rest = &items[prefix..];
        let texture = if prefix > 0 && rest.is_empty() {
            // Everything is static: still copy, so the output is this frame's own.
            self.canvas(cx, &[] as &[Item], vec4(0.0, 0.0, 0.0, 0.0), base)
        } else if prefix > 0 {
            self.canvas(cx, rest, vec4(0.0, 0.0, 0.0, 0.0), base)
        } else {
            self.canvas(cx, rest, clear, None)
        };
        // A sub-frame uses this answer before the shutter adds its texture.
        if let Some(path) = self.scenes3d.waiting_document() {
            self.inexact = format!("Scene3D {path} is loading");
            return (texture, false, asked);
        }
        (texture, exact, asked)
    }

    /// Give the document's frame at any time (the host's `doc.eval`): a
    /// `color: @hdr` document with `motion_blur` samples its shutter from it.
    pub fn set_frames(&mut self, frames: Option<std::sync::Arc<dyn Fn(f64) -> MotionFrame + Send + Sync>>) {
        self.frames = frames;
    }

    /// Locked time (an export or a render): whole-frame motion blur adds
    /// sub-frames until the frame converges, and Scene3D layers sample
    /// their own shutters fully. Realtime (the default, a preview) draws
    /// one frame.
    pub fn set_locked_time(&mut self, on: bool) {
        self.locked = on;
    }

    /// A colour for the frame being drawn: sRGB-encoded in a display frame,
    /// linear light in an HDR one (a component over 1, `emit()`, keeps its
    /// multiple).
    pub(crate) fn lin(&self, c: [f64; 4]) -> [f64; 4] {
        if !self.hdr || self.raw_colors {
            return c;
        }
        makepad_tween::color_srgb_to_linear(c)
    }

    /// A `color: @hdr` frame: its layers in linear half float, summed over
    /// the shutter when it has `motion_blur` (in locked time), then post
    /// once: the document's @hdr passes, the tone map into 8-bit, the
    /// @display and @final passes. One sub-frame is recorded per call; the
    /// frame is not exact until the last, so the host records it again
    /// once it painted.
    fn draw_hdr(&mut self, cx: &mut Cx2d, frame: &MotionFrame, pixels: (u32, u32), base: Affine) -> Drawn {
        self.hdr = true;
        let blur = frame.motion_blur.filter(|(shutter, _, _)| *shutter > 0.0 && frame.fps > 0.0);
        let frames = self.frames.clone();
        let (Some((shutter, max, tol)), Some(frames), true) = (blur, frames, self.locked) else {
            self.accumulating = None;
            self.spans.clear();
            self.passes.clear();
            let (texture, exact, asked) = self.record_frame(cx, frame, pixels, base);
            let out = self.post(cx, frame, texture, pixels);
            let drawn = self.finish_record(cx, out, exact, asked);
            self.last_drawn = Some(drawn.clone());
            return drawn;
        };
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            frame.t.to_bits().hash(&mut h);
            pixels.hash(&mut h);
            format!("{base:?}").hash(&mut h);
            h.finish()
        };
        let open = shutter / 360.0 / frame.fps;
        // The frame the shutter finished, recorded again: the same picture,
        // posted again from its kept average (another part of the host's
        // frame, a picture this one shows, still gathering its own).
        if self.inputs_pending.is_none() && !self.accum.in_progress() {
            if let Some((_, avg, center)) = self.finished.as_ref().filter(|(k, _, _)| *k == key).map(|(k, a, c)| (*k, a.clone(), c.clone())) {
                self.spans.clear();
                self.passes.clear();
                let asked = std::mem::replace(&mut cx.new_draw_event.redraw_all, false);
                self.inexact.clear();
                let out = self.post(cx, &center, avg, pixels);
                self.fresh_glyphs = false;
                let drawn = self.finish_record(cx, out, true, asked);
                self.last_drawn = Some(drawn.clone());
                return drawn;
            }
        }
        self.finished = None;
        if self.accumulating.as_ref().is_none_or(|(k, _, _)| *k != key) || !self.accum.in_progress() {
            let sampling = makepad_render_graph::Sampling::Adaptive(makepad_render_graph::AdaptiveSampling { min: 4, max: max.max(4), tol });
            self.accum.exposure = 1.0;
            self.accum.block_px = (2.0 * pixels.1 as f64 / 720.0).round().max(1.0) as u32;
            self.accum.begin(sampling);
            // Post (and every pass parameter) reads the frame point of the
            // shutter.
            let center = frames(frame.t + makepad_render_graph::mode::FRAME_POINT_U as f64 * open);
            self.accumulating = Some((key, open, center));
        }
        match self.accum.step(cx.cx) {
            makepad_render_graph::Step::Render { u, .. } => {
                self.spans.clear();
                self.passes.clear();
                let sub = frames(frame.t + u as f64 * open);
                let (texture, exact, asked) = self.record_frame(cx, &sub, pixels, base);
                // A sub-frame drawn before all it shows was ready is not
                // summed: the same sub-frame is drawn again.
                let exact = exact && !self.fresh_glyphs && !cx.draw_shaders_pending();
                if !exact && self.inexact.is_empty() {
                    self.inexact = format!("sub-frame: fresh glyphs {} ({}), shaders pending {}", self.fresh_glyphs, self.fresh_why, cx.draw_shaders_pending());
                }
                if exact {
                    if self.accum.add(cx, &texture, pixels, None).is_some() {
                        // One span per pass (as for the post graph): a
                        // reattach re-links the spans, so the accumulator's
                        // own order survives it.
                        for id in self.accum.recorded_ids() {
                            self.spans.push((id, id));
                            self.passes.push(id);
                        }
                    }
                }
                let out = self.last_drawn.as_ref().map_or(texture.clone(), |d| d.texture.clone());
                let drawn = self.finish_record(cx, out, false, asked);
                self.exact = false;
                Drawn { exact: false, ..drawn }
            }
            makepad_render_graph::Step::Finish { .. } => {
                self.spans.clear();
                self.passes.clear();
                let asked = std::mem::replace(&mut cx.new_draw_event.redraw_all, false);
                let Some((avg, id)) = self.accum.finish(cx, pixels, None) else {
                    return self.last_drawn.clone().unwrap_or_else(|| Drawn { texture: self.transparent.clone(), first: self.accum.pass_ids()[0], last: self.accum.pass_ids()[0], exact: false });
                };
                self.spans.push((id, id));
                self.passes.push(id);
                let center = self.accumulating.as_ref().map(|(_, _, c)| c.clone()).unwrap_or_else(|| frame.clone());
                self.finished = Some((key, avg.clone(), center.clone()));
                let out = self.post(cx, &center, avg, pixels);
                self.fresh_glyphs = false;
                let drawn = self.finish_record(cx, out, true, asked);
                self.last_drawn = Some(drawn.clone());
                drawn
            }
            // A measurement is being read back: nothing new to record.
            makepad_render_graph::Step::Wait | makepad_render_graph::Step::Idle => {
                self.exact = false;
                self.inexact = "the shutter's accumulator waits".into();
                let fallback = self.accum.pass_ids()[0];
                let drawn = self.last_drawn.clone().unwrap_or(Drawn { texture: self.transparent.clone(), first: fallback, last: fallback, exact: false });
                Drawn { exact: false, ..drawn }
            }
        }
    }

    /// Post for an HDR frame over its linear `color`: the document's @hdr
    /// passes, the tone map into an 8-bit target, @display and @final
    /// passes. Records into the frame's spans; returns the picture.
    fn post(&mut self, cx: &mut Cx2d, frame: &MotionFrame, color: Texture, pixels: (u32, u32)) -> Texture {
        if self.plain {
            if frame.post.is_some() {
                self.plain_refusal("post", "a post pass", false);
            }
            return color;
        }
        // Post resamples what was drawn: it passes their ids on.
        cx.set_pick_id(0);
        let (decls, values): (&[makepad_render_graph::PassDecl], Vec<makepad_render_graph::PassValues>) = match &frame.post {
            Some((d, v)) => (d.as_slice(), v.clone()),
            None => (&[], Vec::new()),
        };
        self.graph.set_passes(decls);
        // Data layers: each root layer with `data:` drawn into its own
        // linear texture (its colours as written), read by name.
        let mut named: Vec<(LiveId, Texture)> = Vec::new();
        if !decls.is_empty() {
            for (name, items) in &frame.data_layers {
                let mut target = self.data_targets.remove(name).unwrap_or_else(|| RenderTarget::new(cx.cx, "motion_data"));
                target.ensure(cx.cx, pixels, target_format(true));
                self.raw_colors = true;
                let texture = self.canvas_into(cx, items, vec4(0.0, 0.0, 0.0, 0.0), None, Some(&mut target));
                self.raw_colors = false;
                named.push((LiveId::from_str(name), texture));
                self.data_targets.insert(name.clone(), target);
            }
        }
        let host_named: Vec<LiveId> = named.iter().map(|(n, _)| *n).collect();
        // A preview's passes may take fewer samples (`self.realtime()`).
        self.graph.set_realtime(!self.locked);
        let graph_ok = !decls.is_empty() && self.graph.prepare(cx.cx, pixels, makepad_render_graph::Attachments::default(), &host_named, self.locked, 1 << 32);
        for e in self.graph.errors.drain(..) {
            if !self.post_errors.contains(&e) {
                self.post_errors.push(e);
            }
        }
        let fu = makepad_render_graph::FrameUniforms { time: frame.t as f32, frame: (frame.t * frame.fps).round() as f32, ss_tap: -1.0, seed: 0.0, exposure: 1.0, camera: [-1.0, -0.2, 0.0, 1.0] };
        let mut color = color;
        let stage = |md: &mut Self, cx: &mut Cx2d, stage: makepad_render_graph::Stage, color: &Texture| -> Option<Texture> {
            let inputs = makepad_render_graph::StageInputs { color, depth: None, normal: None, velocity: None, glow: None, id: None, named: &named };
            let (out, _) = md.graph.run_stage(cx, stage, &inputs, &values, fu, None)?;
            // One span per pass: a reattach (a Scene3D adding its samples)
            // re-links the spans, so the stage's own order is kept too.
            for id in md.graph.pass_ids(stage) {
                md.spans.push((id, id));
                md.passes.push(id);
            }
            Some(out)
        };
        if graph_ok {
            if let Some(out) = stage(self, cx, makepad_render_graph::Stage::Hdr, &color) {
                color = out;
            }
        }
        // The tone map: linear light to display, premultiplied 8-bit.
        if self.tonemap.is_none() {
            self.tonemap = cx.cx.try_with_vm(|vm| Box::new(makepad_render_graph::DrawToneMap::script_new_with_default(vm)));
        }
        let mut target = self.tone_target.take().unwrap_or_else(|| RenderTarget::new(cx.cx, "motion_tonemap"));
        target.ensure(cx.cx, pixels, target_format(false));
        target.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        if let Some(tm) = &mut self.tonemap {
            tm.set(makepad_render_graph::ToneCurve::Shoulder, 1.0, 1, (pixels.0 as usize, pixels.1 as usize), None);
            tm.draw_vars.set_texture(0, &color);
            tm.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(pixels.0 as f64, pixels.1 as f64) });
        }
        target.end(cx);
        let id = target.pass.draw_pass_id();
        self.record(id);
        let mut out = target.texture.clone();
        self.tone_target = Some(target);
        if graph_ok {
            for s in [makepad_render_graph::Stage::Display, makepad_render_graph::Stage::Final] {
                if let Some(o) = stage(self, cx, s, &out) {
                    out = o;
                }
            }
        }
        out
    }

    /// A group's own post (its path, or the path a Repeat copy came from).
    fn group_post_of(&self, path: &str) -> Option<String> {
        if self.group_posts.contains_key(path) {
            return Some(path.to_string());
        }
        let base = path.split('#').next().unwrap_or(path);
        self.group_posts.contains_key(base).then(|| base.to_string())
    }

    /// A group's picture through its own post passes (every stage in turn,
    /// with no tone map: the group is still part of the frame, whose own
    /// post comes after).
    fn group_post(&mut self, cx: &mut Cx2d, path: &str, color: Texture) -> Texture {
        let Some(key) = self.group_post_of(path) else { return color };
        if self.plain {
            self.plain_refusal(&key, "a post pass", true);
            return color;
        }
        let Some((decls, values)) = self.group_posts.get(&key).cloned() else { return color };
        let pixels = self.pixels;
        let mut graph = self.group_graphs.remove(&key).unwrap_or_default();
        graph.set_passes(&decls);
        graph.set_realtime(!self.locked);
        let ok = graph.prepare(cx.cx, pixels, makepad_render_graph::Attachments::default(), &[], self.locked, 1 << 32);
        for e in graph.errors.drain(..) {
            let e = format!("{key} post: {e}");
            if !self.post_errors.contains(&e) {
                self.post_errors.push(e);
            }
        }
        let mut out = color;
        if ok {
            let fu = makepad_render_graph::FrameUniforms { time: self.frame_t as f32, frame: (self.frame_t * self.fps).round() as f32, ss_tap: -1.0, seed: 0.0, exposure: 1.0, camera: [-1.0, -0.2, 0.0, 1.0] };
            for stage in [makepad_render_graph::Stage::Hdr, makepad_render_graph::Stage::Display, makepad_render_graph::Stage::Final] {
                let inputs = makepad_render_graph::StageInputs { color: &out, depth: None, normal: None, velocity: None, glow: None, id: None, named: &[] };
                if let Some((o, _)) = graph.run_stage(cx, stage, &inputs, &values, fu, None) {
                    for id in graph.pass_ids(stage) {
                        self.record(id);
                    }
                    out = o;
                }
            }
        }
        self.group_graphs.insert(key, graph);
        out
    }

    /// Link the recorded passes into one order and keep the frame's
    /// bookkeeping; `texture` is what the frame ends in.
    fn finish_record(&mut self, cx: &mut Cx2d, texture: Texture, exact: bool, asked: bool) -> Drawn {
        // One total order.
        for pair in self.spans.windows(2) {
            let ((_, last), (first, _)) = (pair[0], pair[1]);
            cx.set_pass_parent(last, first);
        }
        let first = self.spans.first().map(|s| s.0).expect("a frame records at least its canvas");
        let last = self.spans.last().map(|s| s.1).expect("a frame records at least its canvas");
        self.recorded = Some((cx.repaint_id(), self.passes.clone(), self.shaders.units()));
        self.last = Some(last);
        self.output = Some(texture.clone());
        // A shader compiled for this recording may leave its first draws out.
        let exact = exact && !self.fresh_glyphs && !cx.new_draw_event.redraw_all && !cx.draw_shaders_pending();
        // Glyphs still rasterising draw as nothing (or a stand-in): the
        // frame waits for them like for a compiling shader.
        let exact = exact && !cx.pending_async_loads().iter().any(|(kind, n)| *kind == "glyph" && *n > 0);
        // Locked time: a scene waiting for a model it places is recorded
        // again until the model is in it (MustComplete).
        let exact = exact && !(self.locked && self.scenes3d.waiting_for_models());
        if !exact && self.inexact.is_empty() {
            self.inexact = format!("fresh glyphs {}, redraw asked {}, shaders pending {}, glyph loads {}, waiting for models {}", self.fresh_glyphs, cx.new_draw_event.redraw_all, cx.draw_shaders_pending(), cx.pending_async_loads().iter().any(|(kind, n)| *kind == "glyph" && *n > 0), self.locked && self.scenes3d.waiting_for_models());
        }
        let _ = &self.host_backdrop;
        cx.new_draw_event.redraw_all |= asked;
        if !exact {
            // What was kept may be incomplete too: draw it again next time.
            if let Some((key, _)) = &mut self.static_cache {
                key.0 = 0;
            }
        }
        self.exact = exact;
        Drawn { texture, first, last, exact }
    }

    /// A locked frame's shutter steps so far while it gathers its
    /// sub-frames (one per record, or a measurement): moving, recording
    /// again is progress.
    pub fn shutter_progress(&self) -> Option<u32> {
        self.accum.progress()
    }

    /// Whether the last recorded frame had everything it shows ready (see
    /// [`Drawn::exact`]), and none of its render-kit draws was dropped.
    pub fn exact(&self, cx: &Cx) -> bool {
        self.exact && self.kit.painted(cx) != KitPainted::Partial && !self.scenes3d.models_changed()
    }

    /// Whether the last recorded frame is whole: every pass painted after
    /// it was recorded, no shader still compiling, every effect painted.
    pub fn painted(&self, cx: &Cx) -> bool {
        let Some((recorded, passes, units)) = &self.recorded else { return false };
        if cx.draw_shaders_pending() {
            return false;
        }
        passes.iter().all(|&id| !cx.passes[id].paint_dirty && cx.passes[id].painted_serial > *recorded)
            && self.shaders.painted(cx, units, *recorded)
            // A kit draw dropped for an unqueued shader will not come: the
            // frame is done painting, and not exact (record it again).
            && self.kit.painted(cx) != KitPainted::Painting
            && self.scenes3d.painted(cx)
    }

    /// A Scene3D the last recorded frame drew has its environment still
    /// preparing: the renderer builds it off the UI thread, so that frame
    /// was recorded without it (the stock lane, the analytic sky), and the
    /// first draw after it lands adopts it, binding its lane texture and
    /// building its item material. [`Self::painted`] waits for it; a host
    /// that draws ahead of showing (the player's load warm draws) draws
    /// again until this is false.
    pub fn environment_pending(&self) -> bool {
        self.scenes3d.environment_pending()
    }

    /// The work and progress of the current frame, for a host watching for a stall.
    pub fn load_progress(&self, cx: &Cx) -> LoadProgress {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let effects_ready = self.shaders.ready();
        let mut mark = DefaultHasher::new();
        // Check which needs from the recording are still outstanding. This
        // tracks waiting_for without evaluating or walking the document again.
        for need in &self.missing_at_record {
            let supplied = match need {
                Need::Image(media) => self.images.contains_key(media),
                Need::Svg(media) => self.svgs.contains_key(media),
                Need::Model(media) => self.scenes.has(media),
                Need::VideoFrame(path, _) | Need::EditFrame(path, _) => self.videos.contains_key(path),
                Need::Effect(asset) => self.shaders.has_source(asset),
            };
            if !supplied {
                need.hash(&mut mark);
            }
        }
        effects_ready.hash(&mut mark);
        self.scenes.ready().hash(&mut mark);
        self.inexact.hash(&mut mark);
        self.shutter_progress().hash(&mut mark);
        self.scenes3d.hash_progress(&mut mark);
        LoadProgress {
            working: self.scenes3d.loading() || self.environment_pending() || !effects_ready || cx.draw_shaders_pending(),
            mark: mark.finish(),
        }
    }

    /// Draw each Scene3D layer's coverage id map with its frames from the
    /// next record on: an inspect grab's ask (an extra depth and id pass
    /// per scene), never an export's.
    pub fn set_scene_coverage(&mut self, on: bool) {
        self.scenes3d.set_coverage(on);
    }

    /// The coverage maps of the Scene3D layers the last recorded frame
    /// drew, while coverage is on: read each back once the frame has
    /// painted and measure it with [`coverage::FrameCover::measure`].
    pub fn scene_coverage(&self) -> Vec<coverage::SceneCoverage> {
        self.scenes3d.coverage_maps()
    }

    /// Keep the recorded frame attached while it has not painted: every
    /// pass of it, in recording order (the platform drops a pass the host's
    /// list no longer attaches).
    pub fn reattach(&self, cx: &mut Cx2d) {
        if self.recorded.is_none() {
            return;
        }
        self.scenes3d.reattach(cx);
        let Some((_, passes, _)) = &self.recorded else { return };
        // Every target this frame drew into, the pictures of a Scene3D's
        // 2D `layers:` too: a target left out goes stale when the host
        // records again and is never painted (the frame waits forever).
        let all = self.pool.iter().chain(self.static_cache.iter().map(|(_, target)| target)).chain(self.tone_target.iter()).chain(self.data_targets.values()).chain(self.layer_targets.values());
        let by_id: HashMap<DrawPassId, &RenderTarget> = all.map(|t| (t.pass.draw_pass_id(), t)).collect();
        for id in passes {
            if let Some(target) = by_id.get(id) {
                cx.make_child_pass(&target.pass);
            } else if let Some(pass) = self.graph.draw_pass(*id).or_else(|| self.accum.draw_pass(*id)) {
                cx.make_child_pass(pass);
            }
        }
        self.kit.reattach(cx);
        // Attaching resets parents: the order again.
        for pair in self.spans.windows(2) {
            let ((_, last), (first, _)) = (pair[0], pair[1]);
            cx.set_pass_parent(last, first);
        }
    }

    /// Why the last recorded frame has not painted, for a log (P1's paint
    /// reports): the passes not painted since, shaders, effects, the render
    /// kit and full Scene3D samples.
    pub fn debug_state(&self, cx: &Cx) -> String {
        let Some((recorded, passes, units)) = &self.recorded else { return "nothing recorded".into() };
        let mut out = format!("recorded at {recorded}; shaders pending {}; effects painted {}; kit {:?}", cx.draw_shaders_pending(), self.shaders.painted(cx, units, *recorded), self.kit.painted(cx) as u8);
        if !self.exact && !self.inexact.is_empty() {
            out.push_str(&format!("; inexact: {}", self.inexact));
        }
        if self.scenes3d.models_changed() {
            out.push_str("; models changed");
        }
        for &id in passes {
            let pass = &cx.passes[id];
            if pass.paint_dirty || pass.painted_serial <= *recorded {
                out.push_str(&format!("; pass {:?} `{}` dirty {} painted {}", id, pass.debug_name, pass.paint_dirty, pass.painted_serial));
            }
        }
        out.push_str(&self.scenes3d.debug_state(cx));
        out
    }

    /// A gaussian blur of `sigma` target pixels over `texture` (`size`
    /// pixels), through the render kit, its passes in this frame's order.
    fn blurred(&mut self, cx: &mut Cx2d, texture: &Texture, size: (u32, u32), sigma: f64) -> Texture {
        self.note_kind("kit blur");
        let before = self.kit.passes().len();
        let out = self.kit.blur(cx, texture, size, sigma, 0, 0.0, 0);
        let recorded = &self.kit.passes()[before..];
        if let (Some(first), Some(last)) = (recorded.first(), recorded.last()) {
            self.spans.push((*first, *last));
        }
        out
    }

    /// `item`'s render-kit effects over its (full-target) picture. Each
    /// effect is built from its values at this frame; a name or value the
    /// kit refuses is reported (`errors`) and skipped.
    fn effected(&mut self, cx: &mut Cx2d, item: &impl CanvasItem, texture: Texture) -> Texture {
        if item.effects().is_empty() {
            return texture;
        }
        let mut stack = Vec::new();
        for effect in item.effects() {
            match kit_effect(effect) {
                Ok(built) => stack.push(built.at(self.frame_t)),
                Err(error) => {
                    let error = format!("{}: effect `{}`: {error}", item.path(), effect.name);
                    if !self.kit_errors.contains(&error) {
                        self.kit_errors.push(error);
                    }
                }
            }
        }
        if stack.is_empty() {
            return texture;
        }
        self.note_kind("kit effects");
        for effect in item.effects() {
            self.note_kind(&format!("kit {}", effect.name));
        }
        let layers = HashMap::new();
        let camera = self.camera.clone();
        let mut ctx = makepad_stage_render_kit::FxContext::new(self.frame_t, self.fps, &layers);
        ctx.camera = camera.as_ref();
        let applied = self.kit.apply(cx, &stack, &texture, self.pixels, &ctx);
        if let (Some(first), Some(last)) = (applied.first, applied.last) {
            self.spans.push((first, last));
        }
        applied.texture
    }

    /// The target pixels one document unit spans.
    fn unit_px(&self) -> f64 {
        self.base.mean_scale()
    }

    /// Draw `items` into a fresh full-frame canvas (after preparing what
    /// they need), starting from `base` when given. Returns its texture.
    fn canvas(&mut self, cx: &mut Cx2d, items: &[impl CanvasItem], clear: Vec4f, base: Option<Texture>) -> Texture {
        self.canvas_into(cx, items, clear, base, None)
    }

    fn canvas_into(&mut self, cx: &mut Cx2d, items: &[impl CanvasItem], clear: Vec4f, base: Option<Texture>, kept: Option<&mut RenderTarget>) -> Texture {
        for item in items {
            // A backdrop needs the canvas under it: prepared when it is drawn.
            if !item.rich().is_some_and(shows_backdrop) {
                self.prepare(cx, item);
            }
        }
        let mut kept = kept;
        let mut current = self.open_canvas(cx, clear, base, kept.as_deref_mut());
        for item in items {
            if let Some(texture) = self.below_item(cx, item, &mut current, &mut kept) {
                current = texture;
            } else {
                self.draw_item(cx, item, current.index());
            }
        }
        let warm = !self.plain && self.warm.is_some() && !std::mem::replace(&mut self.warm_drawn, true);
        if warm && self.hdr {
            self.warm_vector_emit(cx);
        }
        let texture = self.close_canvas(cx, current, kept);
        if warm {
            self.warm_kit(cx, &texture);
        }
        texture
    }

    /// The emissive vector session (a colour over 1 in an HDR frame, which
    /// the frame may show only later): one pixel of nothing through it, so
    /// its program is built in this unseen draw and not first used in play.
    fn warm_vector_emit(&mut self, cx: &mut Cx2d) {
        self.flush_vector(cx);
        let ev = &mut self.draws.vector_emit;
        ev.begin();
        ev.draw_vars.set_uniform(cx.cx, live_id!(gain), &[1.0]);
        ev.clear();
        ev.rect(0.0, 0.0, 1.0, 1.0);
        ev.set_color(0.0, 0.0, 0.0, 0.0);
        ev.fill();
        ev.end(cx);
    }

    /// The render kit's programs the document may pick by value later (a
    /// blur or an effect that is off now): the kit's blur, and each effect
    /// its layers declare at its defaults, over `texture`; the results are
    /// dropped. Their passes are recorded in the frame, so they draw.
    fn warm_kit(&mut self, cx: &mut Cx2d, texture: &Texture) {
        let before = self.kit.passes().len();
        // Wide enough to halve the picture on the way (a layer's large
        // blur: the kit's copy program as well as its gaussian).
        let _ = self.kit.blur(cx, texture, self.pixels, WARM_BLUR_SIGMA, 0, 0.0, 0);
        let names = self.warm.clone().unwrap_or_default();
        let layers = HashMap::new();
        let ctx = makepad_stage_render_kit::FxContext::new(self.frame_t, self.fps, &layers);
        for name in names {
            let Ok(effect) = kit_effect(&EffectFrame { name, params: Vec::new() }) else { continue };
            let _ = self.kit.apply(cx, &[effect.at(self.frame_t)], texture, self.pixels, &ctx);
        }
        let recorded = &self.kit.passes()[before..];
        if let (Some(first), Some(last)) = (recorded.first().copied(), recorded.last().copied()) {
            self.spans.push((first, last));
        }
    }

    /// The next draws are warm draws (unseen, ahead of play) of `doc`, or
    /// shown draws (None). A warm draw also draws, once, each program the
    /// document may pick by its values later in play, whatever the values
    /// now: the kit's blur and every render-kit effect its layers declare
    /// (at their defaults: an effect off now draws nothing), and in an HDR
    /// frame the emissive vector session. Where a program's first draw is
    /// its compile (Direct3D), that is then behind the loader.
    pub fn set_warm(&mut self, doc: Option<&makepad_motion::MotionDoc>) {
        self.warm = doc.map(|doc| {
            fn walk(layers: &[makepad_motion::Layer], out: &mut Vec<String>) {
                for layer in layers {
                    for effect in &layer.effects {
                        if !out.contains(&effect.name) {
                            out.push(effect.name.clone());
                        }
                    }
                    match &layer.kind {
                        makepad_motion::LayerKind::Group { layers, mask, .. } => {
                            walk(layers, out);
                            walk(mask.as_deref().map(std::slice::from_ref).unwrap_or(&[]), out);
                        }
                        makepad_motion::LayerKind::Repeat { layer, items, .. } => {
                            walk(std::slice::from_ref(layer.as_ref()), out);
                            walk(items, out);
                        }
                        makepad_motion::LayerKind::Bones(rig) => walk(&rig.layers, out),
                        makepad_motion::LayerKind::FlyThrough { layers, .. } => walk(layers, out),
                        makepad_motion::LayerKind::Anchored { layer, .. } => walk(std::slice::from_ref(layer.as_ref()), out),
                        makepad_motion::LayerKind::Scene3dDoc { layers, .. } => {
                            for (_, layer) in layers {
                                walk(std::slice::from_ref(layer), out);
                            }
                        }
                        _ => {}
                    }
                }
            }
            let mut out = Vec::new();
            walk(&doc.layers, &mut out);
            out
        });
    }

    /// Ahead of a first draw that must not pay for them (a HUD part warmed
    /// while its level loads): make the faces `frame`'s text draws in and lay
    /// that text out. Faces and layouts are kept for the whole app, so the
    /// first draw finds them. Needs no draw pass. Returns the texts laid out.
    pub fn warm_text(&mut self, cx: &mut Cx, frame: &MotionFrame) -> usize {
        text::warm(self, cx, &frame.items)
    }

    fn open_canvas(&mut self, cx: &mut Cx2d, clear: Vec4f, base: Option<Texture>, kept: Option<&mut RenderTarget>) -> Canvas {
        let canvas = match kept {
            Some(target) => {
                target.begin(cx, clear);
                Canvas::Kept
            }
            None => {
                let index = self.target(cx, self.pixels, "motion_canvas");
                self.pool[index].begin(cx, clear);
                Canvas::Pool(index)
            }
        };
        if let Some(base) = base {
            let full = [self.pixels.0 as f64, self.pixels.1 as f64];
            self.note_kind("quad");
            self.draws.quad.quad(cx, &base, None, &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], 1.0);
        }
        canvas
    }

    fn close_canvas(&mut self, cx: &mut Cx2d, canvas: Canvas, kept: Option<&mut RenderTarget>) -> Texture {
        self.flush_vector(cx);
        match canvas {
            Canvas::Kept => {
                let target = kept.expect("a kept canvas closes with its target");
                target.end(cx);
                let id = target.pass.draw_pass_id();
                self.record(id);
                target.texture.clone()
            }
            Canvas::Pool(index) => {
                self.pool[index].end(cx);
                let id = self.pool[index].pass.draw_pass_id();
                self.record(id);
                self.pool[index].texture.clone()
            }
        }
    }

    /// Items that read the canvas under them: end the canvas, run them,
    /// and open a new canvas holding the result. `None` for every other item.
    fn below_item(&mut self, cx: &mut Cx2d, item: &impl CanvasItem, current: &mut Canvas, kept: &mut Option<&mut RenderTarget>) -> Option<Canvas> {
        let full = [self.pixels.0 as f64, self.pixels.1 as f64];
        if item.rich().is_some_and(|r| self.refuse_plain_item(r)) {
            // The caller draws its tile directly over the open canvas.
            return None;
        }
        let reads_below = match item.rich().map(|r| &r.kind) {
            Some(ItemKind::Shader { below: true, .. }) => true,
            _ => item.blend() != Blend::Normal || item.rich().is_some_and(shows_backdrop),
        };
        if !reads_below || item.opacity() <= 0.0 {
            return None;
        }
        let under = self.close_canvas(cx, std::mem::replace(current, Canvas::Pool(usize::MAX)), kept.as_deref_mut());
        // The item alone over transparent, or the effect of what is under it.
        let below_effect = item.rich().filter(|r| matches!(r.kind, ItemKind::Shader { program: ShaderProgram::Preset(_) | ShaderProgram::Effect(_), below: true, .. }));
        let layer = match below_effect {
            // An effect of what is under it. (One that makes its own
            // picture was rendered in `prepare`; blended, it is composited
            // below like any layer: rendering it again here would record
            // the view twice in one frame, the first copy sampling the
            // second's output, and never finish painting.)
            Some(effect) => {
                self.effect(cx, effect, under.clone(), self.pixels)
            }
            _ => {
                // What shows the frame under it is prepared now that it exists.
                let outer = self.backdrop.clone();
                if item.rich().is_some_and(shows_backdrop) {
                    self.backdrop = Some(under.clone());
                    self.prepare(cx, item);
                }
                // Inline shaders, blended layers, backdrops: drawn alone.
                let index = self.target(cx, self.pixels, "motion_layer");
                self.pool[index].begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
                let alone = item.without_blend();
                self.draw_item_below(cx, &alone, Some(&under));
                self.flush_vector(cx);
                self.pool[index].end(cx);
                let id = self.pool[index].pass.draw_pass_id();
                self.record(id);
                self.backdrop = outer;
                Some(self.pool[index].texture.clone())
            }
        };
        let index = self.target(cx, self.pixels, "motion_canvas");
        self.pool[index].begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        self.note_kind("blend");
        self.note_kind(&format!("below {}", item.path()));
        match layer {
            Some(layer) => {
                let (mode, amount) = match (below_effect, item.blend()) {
                    // An effect replaces what is under it by its opacity.
                    (Some(_), _) => (blend_mode::NORMAL, item.opacity()),
                    (_, Blend::Add) => (blend_mode::ADD, 1.0),
                    (_, Blend::Screen) => (blend_mode::SCREEN, 1.0),
                    (_, Blend::Multiply) => (blend_mode::MULTIPLY, 1.0),
                    (_, Blend::Normal) => (blend_mode::NORMAL, 1.0),
                };
                if below_effect.is_some() {
                    // below, then the effect's picture at the layer's opacity.
                    self.draws.quad.quad(cx, &under, None, &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], 1.0);
                    self.draws.quad.quad(cx, &layer, None, &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], amount);
                } else {
                    let blend = &mut self.draws.blend;
                    blend.draw_vars.set_texture(0, &under);
                    blend.draw_vars.set_texture(1, &layer);
                    blend.mode = mode;
                    blend.amount = amount as f32;
                    blend.hdr = if self.hdr { 1.0 } else { 0.0 };
                    blend.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(full[0], full[1]) });
                }
            }
            None => self.draws.quad.quad(cx, &under, None, &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], 1.0),
        }
        Some(Canvas::Pool(index))
    }

    /// Render everything `item` needs a texture of its own for, before any
    /// canvas that shows it is opened.
    fn prepare(&mut self, cx: &mut Cx2d, item: &impl CanvasItem) {
        if !item.path().is_empty() {
            self.layer_path = item.path().to_owned();
        }
        let saved = self.pick_enter(cx, item.path());
        self.prepare_item(cx, item);
        self.pick_leave(cx, saved);
    }

    fn prepare_item(&mut self, cx: &mut Cx2d, item: &impl CanvasItem) {
        if item.opacity() <= 0.0 || item.rich().is_some_and(|r| self.refuse_plain_item(r)) {
            return;
        }
        if let Some(item) = item.rich() {
            if let ItemKind::Fly { inner, target, letter, zoom } = &item.kind {
                let saved = self.base;
                self.base = self.fly_camera(cx, inner, target, *letter, *zoom).then(&saved);
                self.prepare(cx, inner.as_ref());
                self.base = saved;
                return;
            }
            if let ItemKind::Anchored { inner, scene, node } = &item.kind {
                if self.plain { return; } // A plain frame has no scene to project this anchor.
                if let Some((p, true)) = self.scenes3d.anchor(scene, node) {
                    let saved = self.base;
                    self.base = saved.then(&Affine::translate(p[0], p[1]));
                    self.prepare(cx, inner.as_ref());
                    self.base = saved;
                }
                return;
            }
            // SVG markup written in the document loads on first use.
            if let ItemKind::Svg { src: src @ Media::Inline(markup), .. } = &item.kind {
                if !self.svgs.contains_key(src) {
                    let markup = markup.to_string();
                    self.set_svg(cx, src.clone(), &markup);
                }
            }
        }
        // Post blur applies to the item's picture, after edge coverage.
        let sigma = item.blur() * self.unit_px();
        let effected = !item.effects().is_empty();
        if (effected || sigma > 0.3) && item.group().is_none() && !(matches!(item.primitive(), Some(PrimitiveFrame::Text { .. })) && !effected && item.blur() <= 0.0) {
            let alone = item.before_effects();
            let texture = self.canvas(cx, std::slice::from_ref(&alone), vec4(0.0, 0.0, 0.0, 0.0), None);
            let texture = if sigma > 0.3 { self.blurred(cx, &texture, self.pixels, sigma) } else { texture };
            let texture = self.effected(cx, item, texture);
            self.prepared.insert((item.path().to_owned(), BLURRED), texture);
            return;
        }
        if let Some((children, mask, _)) = item.group() {
            if group_isolated(item) || self.group_post_of(item.path()).is_some() {
                let texture = self.canvas(cx, children, vec4(0.0, 0.0, 0.0, 0.0), None);
                let texture = self.group_post(cx, item.path(), texture);
                let texture = if sigma > 0.3 { self.blurred(cx, &texture, self.pixels, sigma) } else { texture };
                let texture = self.effected(cx, item, texture);
                self.prepared.insert((item.path().to_owned(), 0), texture);
                if let Some(mask) = mask {
                    let texture = self.canvas(cx, std::slice::from_ref(mask), vec4(0.0, 0.0, 0.0, 0.0), None);
                    self.prepared.insert((item.path().to_owned(), 1), texture);
                }
            } else {
                for child in children {
                    self.prepare(cx, child);
                }
            }
        } else if matches!(item.primitive(), Some(PrimitiveFrame::Text { .. })) {
            text::prepare(self, cx, item);
        } else if let Some(rich) = item.rich() {
            self.prepare_rich(cx, rich);
        }
    }

    fn prepare_rich(&mut self, cx: &mut Cx2d, item: &Item) {
        match &item.kind {
            ItemKind::Svg { src, size } => {
                let natural = self.svgs.get(src).map(|s| s.1);
                let Some(natural) = natural else { return };
                let size = size.unwrap_or(natural);
                let m = self.pixel_map(item, size);
                if !(m.is_axis_aligned() && m.a > 0.0 && m.d > 0.0) {
                    // Rasterised in its own box, then placed.
                    let k = self.layer_scale(size, m.mean_scale());
                    let pad = item.feather * 1.5;
                    let px = (((size[0] + 2.0 * pad) * k).ceil().max(1.0) as u32, ((size[1] + 2.0 * pad) * k).ceil().max(1.0) as u32);
                    let index = self.target(cx, px, "motion_svg");
                    self.pool[index].begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
                    if let Some((svg, _)) = self.svgs.get_mut(src) {
                        svg.opacity = 1.0;
                        svg.set_feather((item.feather * 0.5 * k) as f32, feather_side(item.feather_side));
                        svg.draw_abs(cx, Rect { pos: dvec2(pad * k, pad * k), size: dvec2(px.0 as f64 - 2.0 * pad * k, px.1 as f64 - 2.0 * pad * k) });
                        svg.clear_feather();
                    }
                    self.pool[index].end(cx);
                    let id = self.pool[index].pass.draw_pass_id();
                    self.record(id);
                    self.prepared.insert((item.path.clone(), 0), self.pool[index].texture.clone());
                }
            }
            ItemKind::Widget { .. } => {
                let size = item.box_size().unwrap_or([1.0, 1.0]);
                let m = self.pixel_map(item, size);
                if let Some(texture) = widget::render(self, cx, item, self.layer_scale(size, m.mean_scale())) {
                    self.prepared.insert((item.path.clone(), 0), texture);
                }
            }
            ItemKind::Scene3D { .. } => {
                let size = item.box_size().unwrap_or([1.0, 1.0]);
                let m = self.pixel_map(item, size);
                if let Some(texture) = scene::render(self, cx, item, self.layer_scale(size, m.mean_scale())) {
                    self.prepared.insert((item.path.clone(), 0), texture);
                }
            }
            ItemKind::Vector { size, .. } => {
                let m = self.pixel_map(item, *size);
                if let Some(texture) = vector::render(self, cx, item, self.layer_scale(*size, m.mean_scale())) {
                    self.prepared.insert((item.path.clone(), 0), texture);
                }
            }
            ItemKind::Scene3dDoc { size, .. } => {
                let m = self.pixel_map(item, *size);
                if let Some(texture) = scene3d::render(self, cx, item, self.layer_scale(*size, m.mean_scale())) {
                    self.prepared.insert((item.path.clone(), 0), texture);
                }
            }
            ItemKind::Draw2d { picture, size } => {
                let m = self.pixel_map(item, *size);
                let k = self.layer_scale(*size, m.mean_scale());
                let px = ((size[0] * k).ceil().max(1.0) as u32, (size[1] * k).ceil().max(1.0) as u32);
                self.note_kind("ext Draw2D");
                if let Some(texture) = draw2d::prepare(self, cx, &picture.list, *size, px) {
                    self.prepared.insert((item.path.clone(), 0), texture);
                }
            }
            ItemKind::Shader { program: ShaderProgram::Preset(_) | ShaderProgram::Effect(_), below: false, size, .. } => {
                // A vj_fx effect that makes its own picture, at its box's pixels.
                let m = self.pixel_map(item, *size);
                let k = self.layer_scale(*size, m.mean_scale());
                let px = ((size[0] * k).ceil().max(1.0) as u32, (size[1] * k).ceil().max(1.0) as u32);
                let transparent = self.transparent.clone();
                if let Some(texture) = self.effect(cx, item, transparent, px) {
                    self.prepared.insert((item.path.clone(), 0), texture);
                }
            }
            _ => {}
        }
    }

    fn draw_item(&mut self, cx: &mut Cx2d, item: &impl CanvasItem, _canvas: usize) {
        self.draw_item_below(cx, item, None)
    }

    /// Why a frame would not show what its document means: missing host
    /// resources, effects still building. Empty when it can be drawn exactly.
    pub fn waiting_for(&self, frame: &MotionFrame) -> Vec<String> {
        let mut out: Vec<String> = self.missing(frame).iter().map(|need| format!("{need:?}")).collect();
        if !self.shaders.ready() {
            out.push("vj_fx effects building".into());
        }
        if !self.scenes.ready() {
            out.push("model textures loading".into());
        }
        out
    }

    /// Draw `item` into the open canvas pass. `below` is the canvas under
    /// it for a shader that reads it.
    fn draw_item_below(&mut self, cx: &mut Cx2d, item: &impl CanvasItem, below: Option<&Texture>) {
        if !item.path().is_empty() {
            self.layer_path = item.path().to_owned();
        }
        let saved = self.pick_enter(cx, item.path());
        self.draw_item_below_inner(cx, item, below);
        self.pick_leave(cx, saved);
    }

    // ---- pick ids --------------------------------------------------------

    /// The pick id of `path`'s op `op` (or of the item as a whole), made
    /// on first use.
    fn pick_id_of(&mut self, path: &str, op: Option<usize>) -> u32 {
        let key = (path.to_string(), op);
        if let Some(id) = self.pick_ids.get(&key) {
            return *id;
        }
        self.pick_names.push(key.clone());
        let id = self.pick_names.len() as u32;
        self.pick_ids.insert(key, id);
        id
    }

    /// Draws from now on are `path`'s (while pick ids are on). Returns
    /// what to go back to.
    fn pick_enter(&mut self, cx: &mut Cx2d, path: &str) -> Option<(String, u32)> {
        if !cx.pick_ids_on() {
            return None;
        }
        let saved = (std::mem::replace(&mut self.pick_path, path.to_string()), cx.pick_id());
        let id = self.pick_id_of(path, None);
        self.set_pick_id(cx, id);
        Some(saved)
    }

    fn pick_leave(&mut self, cx: &mut Cx2d, saved: Option<(String, u32)>) {
        if let Some((path, id)) = saved {
            self.pick_path = path;
            self.set_pick_id(cx, id);
        }
    }

    /// Draws from now on pick as `id`. The open vector session stays one draw call: its mesh
    /// notes where each id starts, and a pick repaint draws each of those runs with its own id.
    fn set_pick_id(&mut self, cx: &mut Cx2d, id: u32) {
        cx.set_pick_id(id);
        self.draws.vector.set_pick_id(cx.pick_id());
    }

    /// Draws from now on are op `op` of the Draw2D item being drawn (`None`:
    /// the item as a whole).
    pub(crate) fn pick_op(&mut self, cx: &mut Cx2d, op: Option<usize>) {
        if !cx.pick_ids_on() {
            return;
        }
        let path = self.pick_path.clone();
        let id = self.pick_id_of(&path, op);
        self.set_pick_id(cx, id);
    }

    /// What pick id `id` names: the item's path and its Draw2D op.
    pub fn pick_name(&self, id: u32) -> Option<&(String, Option<usize>)> {
        self.pick_names.get((id as usize).checked_sub(1)?)
    }

    fn draw_item_below_inner(&mut self, cx: &mut Cx2d, item: &impl CanvasItem, below: Option<&Texture>) {
        let opacity = item.opacity().clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return;
        }
        if let Some(item) = item.rich() {
            if self.refuse_plain_item(item) {
                self.plain_tile(cx, item);
                return;
            }
            if let ItemKind::Fly { inner, target, letter, zoom } = &item.kind {
                let saved = self.base;
                self.base = self.fly_camera(cx, inner, target, *letter, *zoom).then(&saved);
                self.draw_item_below(cx, inner.as_ref(), below);
                self.base = saved;
                return;
            }
            if let ItemKind::Anchored { inner, scene, node } = &item.kind {
                // Hidden until its scene has drawn, and while behind the camera.
                if self.plain { return; } // A plain frame has no scene to project this anchor.
                if let Some((p, true)) = self.scenes3d.anchor(scene, node) {
                    let saved = self.base;
                    self.base = saved.then(&Affine::translate(p[0], p[1]));
                    self.draw_item_below(cx, inner.as_ref(), below);
                    self.base = saved;
                }
                return;
            }
        }
        let batched = item.group().is_some() || matches!(item.primitive(), Some(PrimitiveFrame::Rect { .. } | PrimitiveFrame::Ellipse { .. } | PrimitiveFrame::Path { .. }));
        if !batched || self.prepared.contains_key(&(item.path().to_owned(), BLURRED)) || group_isolated(item) || self.group_post_of(item.path()).is_some() {
            self.flush_vector(cx);
        }
        if let Some(texture) = self.prepared.get(&(item.path().to_owned(), BLURRED)).cloned() {
            let full = [self.pixels.0 as f64, self.pixels.1 as f64];
            self.note_kind("quad");
            self.draws.quad.quad(cx, &texture, None, &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], opacity);
            return;
        }
        let kind = match item.primitive() {
            Some(PrimitiveFrame::Text { .. }) => "text",
            Some(_) => "vector",
            None => match item.rich().map(|r| &r.kind) {
                Some(ItemKind::Svg { .. }) => "svg",
                Some(ItemKind::Shader { .. }) => "shader",
                _ => "quad",
            },
        };
        self.note_kind(kind);
        self.note_kind("quad");
        if let Some(primitive) = item.primitive() {
            match primitive {
                PrimitiveFrame::Rect { size, radius, fill, stroke, width, trim, .. } if *trim == [0.0, 1.0] => {
                    self.sdf_shape(cx, item, false, *size, *radius, *fill, *stroke, *width, opacity);
                }
                PrimitiveFrame::Ellipse { size, fill, stroke, width, trim, .. } if *trim == [0.0, 1.0] => {
                    self.sdf_shape(cx, item, true, *size, 0.0, *fill, *stroke, *width, opacity);
                }
                PrimitiveFrame::Rect { size, radius, fill, stroke, width, trim, cap, .. } => {
                    let path = geometry::rect_path(*size, *radius);
                    self.shape(cx, item, &path, *size, *fill, *stroke, *width, *trim, *cap, Join::Miter, opacity);
                }
                PrimitiveFrame::Ellipse { size, fill, stroke, width, trim, cap, .. } => {
                    let path = geometry::ellipse_path(*size);
                    self.shape(cx, item, &path, *size, *fill, *stroke, *width, *trim, *cap, Join::Round, opacity);
                }
                PrimitiveFrame::Path { path, fill, stroke, width, trim, cap, join } => {
                    let size = geometry::bounds(path).map_or([0.0, 0.0], |b| [b[2].max(0.0), b[3].max(0.0)]);
                    if fill.is_some() {
                        self.vector(cx, item, path, size, *fill, None, 0.0, *cap, *join, opacity);
                    }
                    if stroke.is_some() && trim[1] > trim[0] {
                        let tolerance = 0.25 / self.pixel_map(item, size).mean_scale().max(1e-6);
                        let trimmed = (trim[0] > 0.0 || trim[1] < 1.0).then(|| geometry::trim(path, trim[0], trim[1], tolerance));
                        self.vector(cx, item, trimmed.as_ref().unwrap_or(path), size, None, *stroke, *width, *cap, *join, opacity);
                    }
                }
                PrimitiveFrame::Text { .. } => text::draw(self, cx, item, opacity),
            }
        } else if let Some((children, mask, mask_mode)) = item.group() {
            if let Some(texture) = self.prepared.get(&(item.path().to_owned(), 0)).cloned() {
                let full = [self.pixels.0 as f64, self.pixels.1 as f64];
                let mask = mask.and(self.prepared.get(&(item.path().to_owned(), 1)).cloned());
                self.draws.quad.quad(cx, &texture, mask.as_ref().map(|m| (m, mask_mode)), &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], opacity);
            } else {
                for child in children {
                    self.draw_item_below(cx, child, below);
                }
            }
        } else if let Some(rich) = item.rich() {
            self.draw_rich(cx, rich, below, opacity);
        }
    }

    fn draw_rich(&mut self, cx: &mut Cx2d, item: &Item, below: Option<&Texture>, opacity: f64) {
        match &item.kind {
            ItemKind::Svg { src, size } => {
                let Some(natural) = self.svgs.get(src).map(|s| s.1) else { return };
                let size = size.unwrap_or(natural);
                let m = self.pixel_map(item, size);
                if let Some(texture) = self.prepared.get(&(item.path.clone(), 0)).cloned() {
                    let pad = item.feather * 1.5;
                    let placed = m.then(&Affine::translate(-pad, -pad));
                    self.draws.quad.quad(cx, &texture, None, &placed, [size[0] + 2.0 * pad, size[1] + 2.0 * pad], [0.0, 0.0, 1.0, 1.0], opacity);
                } else if let Some((svg, _)) = self.svgs.get_mut(src) {
                    let b = aabb(&m, size);
                    svg.opacity = opacity as f32;
                    svg.set_feather((item.feather * 0.5 * m.mean_scale()) as f32, feather_side(item.feather_side));
                    svg.draw_abs(cx, Rect { pos: dvec2(b[0], b[1]), size: dvec2(b[2] - b[0], b[3] - b[1]) });
                    svg.clear_feather();
                }
            }
            ItemKind::Image { src: Media::Backdrop, size, zoom, focus, mesh, .. } => {
                // The picture under the layer, magnified about `focus`.
                let Some(under) = self.backdrop.clone().or_else(|| below.cloned()).or_else(|| self.host_backdrop.clone()) else { return };
                let size = size.unwrap_or([1.0, 1.0]);
                let m = self.pixel_map(item, size);
                let centre = item.transform(size).apply([size[0] * 0.5, size[1] * 0.5]);
                let focus = focus.unwrap_or(centre);
                let half = [size[0] * 0.5 / zoom, size[1] * 0.5 / zoom];
                let p0 = self.base.apply([focus[0] - half[0], focus[1] - half[1]]);
                let p1 = self.base.apply([focus[0] + half[0], focus[1] + half[1]]);
                let (w, h) = (self.pixels.0 as f64, self.pixels.1 as f64);
                let uv = [p0[0] / w, p0[1] / h, (p1[0] - p0[0]) / w, (p1[1] - p0[1]) / h];
                if let Some(mesh) = mesh {
                    self.draws.quad.mesh(cx, &under, &m, mesh, uv, None, opacity);
                } else {
                    self.draws.quad.quad(cx, &under, None, &m, size, uv, opacity);
                }
            }
            ItemKind::Image { src, size, fit, .. } => {
                let Some((texture, natural)) = self.images.get(src).cloned() else { return };
                self.picture(cx, item, &texture, natural, *size, *fit, opacity);
            }
            ItemKind::Video { size, fit, .. } => {
                let Some((texture, natural)) = self.videos.get(&item.path).cloned() else { return };
                self.picture(cx, item, &texture, natural, *size, *fit, opacity);
            }
            ItemKind::Shader { program: ShaderProgram::Inline { .. }, size, .. } => {
                let m = self.pixel_map(item, *size);
                let below = below.cloned().unwrap_or_else(|| self.transparent.clone());
                let px = [size[0] * m.mean_scale(), size[1] * m.mean_scale()];
                self.inline_shader(cx, item, &m, *size, px, &below, opacity);
            }
            // Handled before (the camera moves, then `inner` draws).
            ItemKind::Primitive(_) | ItemKind::Group { .. } | ItemKind::Fly { .. } | ItemKind::Anchored { .. } => {}
            ItemKind::Vector { size, .. } => {
                if let Some(texture) = self.prepared.get(&(item.path.clone(), 0)).cloned() {
                    let pad = item.feather * 1.5;
                    let m = self.pixel_map(item, *size).then(&Affine::translate(-pad, -pad));
                    self.draws.quad.quad(cx, &texture, None, &m, [size[0] + 2.0 * pad, size[1] + 2.0 * pad], [0.0, 0.0, 1.0, 1.0], opacity);
                }
            }
            ItemKind::Shader { size, .. } | ItemKind::Widget { size, .. } | ItemKind::Scene3D { size, .. } | ItemKind::Draw2d { size, .. } | ItemKind::Scene3dDoc { size, .. } => {
                if let Some(texture) = self.prepared.get(&(item.path.clone(), 0)).cloned() {
                    let m = self.pixel_map(item, *size);
                    self.draws.quad.quad(cx, &texture, None, &m, *size, [0.0, 0.0, 1.0, 1.0], opacity);
                }
            }
        }
    }

    /// A Rect or Ellipse: its fill, and its stroke trimmed to `trim` of
    /// its length.
    #[allow(clippy::too_many_arguments)]
    /// A whole (untrimmed) Rect or Ellipse from its distance field: one
    /// instance, nothing tessellated or uploaded.
    #[allow(clippy::too_many_arguments)]
    fn sdf_shape(&mut self, cx: &mut Cx2d, item: &dyn PaintItem, ellipse: bool, size: [f64; 2], radius: f64, fill: Option<[f64; 4]>, stroke: Option<[f64; 4]>, width: f64, opacity: f64) {
        self.flush_vector(cx);
        let m = self.pixel_map(item, size);
        let fill = fill.map(|c| self.lin(c));
        let stroke = stroke.map(|c| self.lin(c));
        let pm = |c: Option<[f64; 4]>| c.map_or(vec4(0.0, 0.0, 0.0, 0.0), |c| vec4((c[0] * c[3]) as f32, (c[1] * c[3]) as f32, (c[2] * c[3]) as f32, c[3] as f32));
        let shape = if ellipse { paint::Shape::Ellipse } else { paint::Shape::Rect { radius } };
        let width = if stroke.is_some() { width } else { 0.0 };
        let scale = m.mean_scale().max(1e-6);
        paint::sdf(cx, &mut self.draws.shape, &m, size, shape, pm(fill), pm(stroke), width, scale, item.feather() * 0.5 * scale, feather_side(item.feather_side()), opacity);
    }

    fn shape(&mut self, cx: &mut Cx2d, item: &dyn PaintItem, path: &geometry::VectorPath, size: [f64; 2], fill: Option<[f64; 4]>, stroke: Option<[f64; 4]>, width: f64, trim: [f64; 2], cap: Cap, join: Join, opacity: f64) {
        if trim == [0.0, 1.0] {
            self.vector(cx, item, path, size, fill, stroke, width, cap, join, opacity);
            return;
        }
        if fill.is_some() {
            self.vector(cx, item, path, size, fill, None, 0.0, cap, join, opacity);
        }
        if stroke.is_some() && trim[1] > trim[0] {
            let tolerance = 0.25 / self.pixel_map(item, size).mean_scale().max(1e-6);
            let trimmed = geometry::trim(path, trim[0], trim[1], tolerance);
            self.vector(cx, item, &trimmed, size, None, stroke, width, cap, join, opacity);
        }
    }

    /// An image or video frame in its box.
    #[allow(clippy::too_many_arguments)]
    fn picture(&mut self, cx: &mut Cx2d, item: &Item, texture: &Texture, natural: [f64; 2], size: Option<[f64; 2]>, fit: Fit, opacity: f64) {
        let size = size.unwrap_or(natural);
        let (offset, quad, uv) = fit_quad(size, natural, fit);
        if quad[0] <= 0.0 || quad[1] <= 0.0 { return; }
        let m = self.pixel_map(item, size).then(&Affine::translate(offset[0], offset[1]));
        // Pictures are sRGB: decoded into an HDR frame.
        self.draws.quad.decode = if self.hdr { 1.0 } else { 0.0 };
        if let ItemKind::Image { mesh: Some(mesh), .. } = &item.kind {
            let fitted = [uv[0] - offset[0] / quad[0] * uv[2], uv[1] - offset[1] / quad[1] * uv[3], size[0] / quad[0] * uv[2], size[1] / quad[1] * uv[3]];
            let clip = [uv[0], uv[1], uv[0] + uv[2], uv[1] + uv[3]];
            let m = self.pixel_map(item, size);
            self.draws.quad.mesh(cx, texture, &m, mesh, fitted, Some(clip), opacity);
        } else {
            self.draws.quad.quad(cx, texture, None, &m, quad, uv, opacity);
        }
        self.draws.quad.decode = 0.0;
    }

    /// A filled and/or stroked outline in the item's own coordinates.
    #[allow(clippy::too_many_arguments)]
    fn vector(&mut self, _cx: &mut Cx2d, item: &dyn PaintItem, path: &geometry::VectorPath, size: [f64; 2], fill: Option<[f64; 4]>, stroke: Option<[f64; 4]>, width: f64, cap: Cap, join: Join, opacity: f64) {
        let m = self.pixel_map(item, size);
        let path = geometry::without_tiny_segments(&geometry::transformed(path, &m), 0.05);
        let fill = fill.map(|c| self.lin(c));
        let stroke = stroke.map(|c| self.lin(c));
        // An emissive colour (over 1): its own session, the multiple as the
        // session's gain.
        let over = |c: Option<[f64; 4]>| c.map_or(1.0, |c| c[0].max(c[1]).max(c[2]));
        let k = over(fill).max(over(stroke));
        if self.hdr && k > 1.0 {
            self.flush_vector(_cx);
            let scale = |c: Option<[f64; 4]>| c.map(|c| [c[0] / k, c[1] / k, c[2] / k, c[3]]);
            let (fill, stroke) = (scale(fill), scale(stroke));
            let ev = &mut self.draws.vector_emit;
            ev.begin();
            ev.cur_tolerance = 0.2;
            if item.feather() > 0.0 {
                ev.set_feather((item.feather() * 0.5 * m.mean_scale()) as f32, feather_side(item.feather_side()));
            }
            ev.draw_vars.set_uniform(_cx.cx, live_id!(gain), &[k as f32]);
            if let Some(c) = fill {
                ev.path = path.clone();
                ev.set_color(c[0] as f32, c[1] as f32, c[2] as f32, (c[3] * opacity) as f32);
                ev.fill();
            }
            if let Some(c) = stroke {
                if width > 0.0 {
                    ev.path = path;
                    ev.set_color(c[0] as f32, c[1] as f32, c[2] as f32, (c[3] * opacity) as f32);
                    use makepad_svg::path::{LineCap, LineJoin};
                    let cap = match cap {
                        Cap::Round => LineCap::Round,
                        Cap::Butt => LineCap::Butt,
                        Cap::Square => LineCap::Square,
                    };
                    let join = match join {
                        Join::Round => LineJoin::Round,
                        Join::Miter => LineJoin::Miter,
                        Join::Bevel => LineJoin::Bevel,
                    };
                    ev.stroke_opts((width * m.mean_scale()) as f32, cap, join, 4.0, 1.0);
                }
            }
            ev.clear_feather();
            ev.end(_cx);
            let full = self.draws.vector_emit.geometry_full();
            self.geometry_full(full);
            return;
        }
        let dv = &mut self.draws.vector;
        if !self.vector_open {
            dv.begin();
            dv.set_pick_id(_cx.pick_id());
            dv.cur_tolerance = 0.2;
            self.vector_open = true;
            self.vector_layer = self.layer_path.clone();
        }
        if item.feather() > 0.0 {
            dv.set_feather((item.feather() * 0.5 * m.mean_scale()) as f32, feather_side(item.feather_side()));
        }
        if let Some(c) = fill {
            dv.path = path.clone();
            dv.set_color(c[0] as f32, c[1] as f32, c[2] as f32, (c[3] * opacity) as f32);
            dv.fill();
        }
        if let Some(c) = stroke {
            if width > 0.0 {
                dv.path = path;
                dv.set_color(c[0] as f32, c[1] as f32, c[2] as f32, (c[3] * opacity) as f32);
                use makepad_svg::path::{LineCap, LineJoin};
                let cap = match cap {
                    Cap::Round => LineCap::Round,
                    Cap::Butt => LineCap::Butt,
                    Cap::Square => LineCap::Square,
                };
                let join = match join {
                    Join::Round => LineJoin::Round,
                    Join::Miter => LineJoin::Miter,
                    Join::Bevel => LineJoin::Bevel,
                };
                dv.stroke_opts((width * m.mean_scale()) as f32, cap, join, 4.0, 1.0);
            }
        }
        dv.clear_feather();
    }

    /// End the open shape session: its mesh drawn here, in order.
    pub(crate) fn flush_vector(&mut self, cx: &mut Cx2d) {
        if self.vector_open {
            self.vector_open = false;
            self.draws.vector.end(cx);
            let full = self.draws.vector.geometry_full();
            let layer = std::mem::replace(&mut self.layer_path, self.vector_layer.clone());
            self.geometry_full(full);
            self.layer_path = layer;
        }
    }

    /// A vector session that asked for more mesh than can be made: said
    /// (the shapes before that point drew).
    pub(crate) fn geometry_full(&mut self, full: Option<makepad_widgets::makepad_draw::vector::GeometryFull>) {
        let Some(full) = full else { return };
        let error = format!("too much geometry: {} vertices in layer {} (past what memory or one GPU buffer holds; the shapes before that point are drawn)", full.vertices, if self.layer_path.is_empty() { "(the document)" } else { &self.layer_path });
        if !self.geometry_errors.contains(&error) {
            self.geometry_errors.push(error);
        }
    }
}

fn feather_side(side: makepad_motion::FeatherSide) -> DrawFeatherSide {
    match side {
        makepad_motion::FeatherSide::Both => DrawFeatherSide::Both,
        makepad_motion::FeatherSide::Inner => DrawFeatherSide::Inner,
        makepad_motion::FeatherSide::Outer => DrawFeatherSide::Outer,
    }
}

/// Where the open canvas is.
enum Canvas {
    Pool(usize),
    Kept,
}

impl Canvas {
    fn index(&self) -> usize {
        match self {
            Canvas::Pool(index) => *index,
            Canvas::Kept => usize::MAX,
        }
    }
}

/// Whether a group composes as one picture (its opacity, mask or blend
/// apply to the whole) rather than drawing its children in place.
fn group_isolated(item: &impl CanvasItem) -> bool {
    item.group().is_some_and(|(_, mask, _)| mask.is_some() || item.opacity() < 1.0 || item.blend() != Blend::Normal || item.blur() > 0.0 || !item.effects().is_empty())
}

/// Whether an item shows the picture under it (`@backdrop`), itself or in
/// its children.
fn shows_backdrop(item: &Item) -> bool {
    match &item.kind {
        ItemKind::Image { src: Media::Backdrop, .. } => true,
        ItemKind::Group { children, .. } => children.iter().any(shows_backdrop),
        ItemKind::Fly { inner, .. } | ItemKind::Anchored { inner, .. } => shows_backdrop(inner),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_place_pictures_in_their_box() {
        assert_eq!(fit_quad([100.0, 100.0], [200.0, 100.0], Fit::Fit), ([0.0, 25.0], [100.0, 50.0], [0.0, 0.0, 1.0, 1.0]));
        assert_eq!(fit_quad([100.0, 100.0], [200.0, 100.0], Fit::Fill), ([0.0, 0.0], [100.0, 100.0], [0.25, 0.0, 0.5, 1.0]));
        assert_eq!(fit_quad([100.0, 100.0], [50.0, 200.0], Fit::None), ([25.0, 0.0], [50.0, 100.0], [0.0, 0.25, 1.0, 0.5]));
        assert_eq!(aabb(&Affine::rotate(90.0), [10.0, 20.0]).map(|v| (v * 1e6).round() / 1e6), [-20.0, 0.0, 0.0, 10.0]);
    }

    /// What Motion measures (`makepad_motion::text_width`, the typography
    /// stack its fit, wrap and outline functions use) is what the text draw
    /// lays out, on every bundled family: advances and line breaks.
    #[test]
    fn motion_measures_text_the_way_the_text_draw_lays_it_out() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.with_vm(makepad_widgets::script_mod);
        cx.with_vm(script_mod);
        let mut md = MotionDraw::new(&mut cx);
        let mut worst = (0.0f64, String::new());
        for family in makepad_doc_fonts::bundled() {
            for (text, weight) in [("Hamburgefonstiv 0123", 400.0), ("Wave AV To fi", 400.0), ("Hamburgefonstiv", 700.0)] {
                assert!(text::set_face(&mut md, &mut cx, family, weight, false, &[], 30.0), "{family} loads");
                let laid = md.draws.text.layout(&mut cx, 0.0, 0.0, None, false, Align { x: 0.0, y: 0.0 }, text);
                let drawn = laid.rows.iter().map(|row| row.width_in_lpxs as f64).fold(0.0, f64::max);
                let measured = makepad_motion::text_width(text, family, 30.0 * 4.0 / 3.0, weight, false, &[]).unwrap();
                let off = (drawn - measured).abs() / drawn.max(1.0);
                eprintln!("{family:22} {weight:4} {text:22} drawn {drawn:8.2} measured {measured:8.2} off {:.3}%", off * 100.0);
                if off > worst.0 {
                    worst = (off, format!("{family} {weight} {text}"));
                }
            }
        }
        assert!(worst.0 < 0.01, "motion's measure and the text draw's layout differ by {:.2}% ({})", worst.0 * 100.0, worst.1);
        // Line breaks: the typography wrap over Motion's measure breaks the
        // lines where the text draw's own wrapping does.
        struct Doc<'a>(&'a str);
        impl makepad_typography::fit::Measure for Doc<'_> {
            fn width(&self, text: &str) -> f32 {
                makepad_motion::text_width(text, self.0, 40.0, 400.0, false, &[]).unwrap() as f32
            }
            fn line_height(&self) -> f32 {
                40.0
            }
        }
        for family in makepad_doc_fonts::bundled() {
            let text = "the quick brown fox jumps over the lazy dog and keeps running far away";
            assert!(text::set_face(&mut md, &mut cx, family, 400.0, false, &[], 30.0));
            let laid = md.draws.text.layout(&mut cx, 0.0, 0.0, Some(300.0), true, Align { x: 0.0, y: 0.0 }, text);
            let drawn: Vec<String> = laid.rows.iter().map(|row| row.text.trim_end().to_string()).collect();
            let wrapped = makepad_typography::fit::wrap(&Doc(family), text, 300.0);
            assert_eq!(drawn, wrapped, "{family}: the lines break differently");
        }
    }
}

