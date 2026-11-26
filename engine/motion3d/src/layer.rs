//! The Scene3D layer as a host draws it: a loaded document, its evaluation
//! cache and its GPU renderer, with motion-blur sub-frames evaluated at
//! their own times. This is the
//! hook Motion's `Scene3D` layer calls (see the report for the wiring):
//!
//! ```ignore
//! let mut layer = Scene3dLayer::load(&scene3d_source, opts)?;   // once
//! let tex = layer.draw(cx, &FrameTime { t, fps, edit_offset }, (w, h), &audio); // per frame, in the host's draw
//! // composite `tex` (premultiplied sRGB) into the layer's box when layer.painted(cx);
//! // until then, on each draw: layer.reattach(cx, &audio)
//! ```

use crate::anim::AudioSource;
use crate::doc::{LoadOptions, Scene3dDoc};
use crate::eval::{EvalCache, EvalInput};
use crate::frame::SceneFrame;
use makepad_render_graph::{AdaptiveSampling, Sampling};
use crate::shared::SharedRenderer;
use crate::script::Diagnostic;
use makepad_widgets::*;

/// When to evaluate.
#[derive(Clone, Copy, Debug)]
pub struct FrameTime {
    /// The layer's local time in seconds.
    pub t: f32,
    /// Output frame rate (for the motion-blur shutter).
    pub fps: f32,
    /// The edit time of the layer's t = 0 (audio features are read on the
    /// edit's clock).
    pub edit_offset: f32,
}

/// How long effect physics may step per evaluation in a realtime preview.
pub const PREVIEW_PHYSICS_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);

pub struct Scene3dLayer {
    pub doc: Scene3dDoc,
    pub cache: EvalCache,
    pub renderer: SharedRenderer,
    anchors: Vec<crate::frame::Anchor>,
    /// The frame being recorded: its time, the open shutter (seconds) and
    /// the aspect, for its sub-frames.
    shutter: Option<(FrameTime, f32, f32)>,
    /// The host samples time itself (its own sub-frames) and tone maps:
    /// draw one frame at the time given, in linear light.
    host_sampled: bool,
    /// Realtime (a preview): motion blur is the camera's screen motion
    /// (the graph's VelocityBlur), one frame per paint.
    realtime: bool,
}

impl Scene3dLayer {
    pub fn load(source: &str, opts: LoadOptions) -> Result<Self, Vec<Diagnostic>> {
        Self::load_with_modules(source, opts, &[])
    }

    /// See [`Scene3dDoc::load_with_modules`].
    pub fn load_with_modules(source: &str, opts: LoadOptions, modules: &[makepad_script_imports::LoadedModule]) -> Result<Self, Vec<Diagnostic>> {
        Ok(Self::from_doc(Scene3dDoc::load_with_modules(source, opts, modules)?, EvalCache::default()))
    }

    /// Its document and evaluation cache, its renderer gone (a document
    /// moves to another thread; the renderer stays on this one).
    pub fn into_doc(self) -> (Scene3dDoc, EvalCache) {
        (self.doc, self.cache)
    }

    /// A layer drawing a document loaded elsewhere (a worker: documents and
    /// their caches move between threads, renderers stay on the UI thread),
    /// with what its evaluations built so far.
    pub fn from_doc(doc: Scene3dDoc, cache: EvalCache) -> Self {
        Self { doc, cache, renderer: SharedRenderer::new(), anchors: Vec::new(), shutter: None, host_sampled: false, realtime: false }
    }

    /// How the frame at `center` samples its shutter: adaptive sub-frames
    /// for motion blur, fixed jittered copies for temporal AA, or one frame.
    fn sampling(center: &SceneFrame, fps: f32) -> (Option<Sampling>, f32) {
        if let Some((shutter, max, tol)) = center.motion_blur.filter(|(s, n, _)| *s > 0.0 && *n > 1 && fps > 0.0) {
            let open = shutter / 360.0 / fps;
            return (Some(Sampling::Adaptive(AdaptiveSampling { min: 4, max: max.max(4), tol })), open);
        }
        if center.temporal_aa > 1 {
            return (Some(Sampling::Fixed(center.temporal_aa)), 0.0);
        }
        (None, 0.0)
    }

    /// Record the frame at `time` into the host's draw and return the output
    /// texture (premultiplied sRGB BGRA8, `size` pixels). It is complete once
    /// [`Self::painted`]; until then call [`Self::reattach`] on each draw:
    /// a frame with motion blur or temporal AA records one sub-frame per
    /// paint, each evaluated at its own time over the shutter (motion blur
    /// adds sub-frames until the frame converges, up to its `samples`).
    pub fn draw(&mut self, cx: &mut Cx2d, time: &FrameTime, size: (usize, usize), audio: &dyn AudioSource) -> Option<Texture> {
        // A new frame: whole only after recordings of its own.
        self.renderer.new_frame();
        self.renderer.set_ao_frame_index((time.t as f64 * time.fps as f64).round() as i64 as u64);
        if !self.doc.models.is_empty() {
            crate::models::pump(cx);
        }
        let aspect = size.0 as f32 / size.1.max(1) as f32;
        let mut center = self.doc.eval(&EvalInput { t: time.t, aspect, audio, edit_offset: time.edit_offset }, &mut self.cache);
        let (sampling, open) = if self.host_sampled { (None, 0.0) } else { Self::sampling(&center, time.fps) };
        let sampling = match sampling {
            // A preview blurs along the camera's screen motion instead
            // (the VelocityBlur pass), one frame per paint.
            Some(Sampling::Adaptive(_)) if self.realtime => None,
            other => other,
        };
        if open > 0.0 {
            // Post (flash, shake, fades, every pass parameter) and anchors
            // are read at one point of the shutter, the same for every
            // sub-frame count (makepad_render_graph::mode::FRAME_POINT_U).
            let t = time.t + makepad_render_graph::mode::FRAME_POINT_U * open;
            center = self.doc.eval(&EvalInput { t, aspect, audio, edit_offset: time.edit_offset }, &mut self.cache);
        }
        self.anchors = center.anchors.clone();
        // Locked time sums sub-frames; only a preview blurs by velocity.
        if !self.realtime || self.host_sampled {
            center.post.velocity_blur = None;
        }
        self.shutter = Some((*time, open, aspect));
        let out = self.renderer.draw(cx, center, size, time.t, sampling);
        self.advance(cx, audio);
        out
    }

    /// Record the next sub-frame when one is due.
    fn advance(&mut self, cx: &mut Cx2d, audio: &dyn AudioSource) {
        let Some((time, open, aspect)) = self.shutter else { return };
        if let Some((k, u, tap)) = self.renderer.next_subframe(cx) {
            let f = self.doc.eval(&EvalInput { t: time.t + u * open, aspect, audio, edit_offset: time.edit_offset }, &mut self.cache);
            self.renderer.record_subframe(cx, f, k, tap);
        }
    }

    /// A host that composes this scene into its own linear frame, sampling
    /// the shutter itself (the HDR Motion layer): every draw is one frame
    /// at the given time, output as linear, exposed, premultiplied RGBA16F
    /// (no tone map or display post of the scene's own; its @hdr passes
    /// run).
    pub fn set_host_sampled(&mut self, on: bool) {
        self.host_sampled = on;
        self.renderer.set_linear_output(on);
    }

    /// The frame's time exactly (the host's f64 seconds), so a closure
    /// called at the frame's own time sees it without f32 rounding: a
    /// beat at 36.6 s fires on the frame at 36.6 s.
    pub fn set_exact_time(&mut self, t: Option<f64>) {
        self.doc.set_exact_time(t);
    }

    /// Realtime (a preview): motion blur smears along the camera's screen
    /// motion in one frame (the graph's VelocityBlur pass) instead of
    /// adding sub-frames until the frame converges, and effect
    /// physics steps at most [`PREVIEW_PHYSICS_BUDGET`] per evaluation (a
    /// long forward scrub shows the newest state while it catches up
    /// instead of blocking the frame). The default is locked time.
    pub fn set_realtime(&mut self, on: bool) {
        if on != self.realtime {
            self.doc.set_physics_budget(on.then_some(PREVIEW_PHYSICS_BUDGET));
        }
        self.realtime = on;
        // Graph passes read it as `self.realtime()`.
        self.renderer.set_realtime(on);
    }

    /// Sample the key light's disc for this recording's shadow cascades.
    pub fn set_sun_disc_sample(&mut self, point: Option<[f32; 2]>) {
        self.renderer.set_sun_disc_sample(point);
    }

    /// A named texture for materials with `texture: "name"` (device
    /// screens showing a video or an app run).
    pub fn set_texture(&mut self, name: &str, texture: Texture) {
        self.renderer.set_texture(name, texture);
    }

    /// A named texture's picture size (video frames do not carry one) and
    /// the crop to show this frame, `[x, y, w, h]` in its pixels: a reframe
    /// or focus track following the action. A material's own `crop` wins.
    /// The named nodes of the last drawn frame, projected to the frame
    /// (0..1 from the top left): where 2D HUD boxes and leader lines attach.
    pub fn anchors(&self) -> &[crate::frame::Anchor] {
        &self.anchors
    }

    pub fn set_texture_frame(&mut self, name: &str, size: (u32, u32), crop: Option<[f32; 4]>) {
        self.renderer.set_texture_frame(name, size, crop);
    }

    /// The frame is whole: every pass painted, and in locked time every
    /// model it places is built (a model is MustComplete: an export never
    /// shows a frame without it; the host records the frame again once the
    /// document reloaded, see [`Self::models_changed`]). A realtime preview
    /// shows the scene while its models build.
    pub fn painted(&self, cx: &Cx) -> bool {
        self.renderer.painted(cx) && (self.realtime || !self.doc.waiting_for_models())
    }

    /// A model the document places is still building.
    pub fn waiting_for_models(&self) -> bool {
        self.doc.waiting_for_models()
    }

    /// The scene's environment is still preparing off the UI thread (see
    /// [`SharedRenderer::environment_pending`]): [`Self::painted`] is false
    /// until the draw after it landed.
    pub fn environment_pending(&self) -> bool {
        self.renderer.environment_pending()
    }

    /// A model the document places installed (or failed) since it loaded:
    /// load the document again to show it.
    pub fn models_changed(&self) -> bool {
        self.doc.models_changed()
    }

    /// Keep the frame painting (and record its next sub-frame when the
    /// last one has painted).
    pub fn reattach(&mut self, cx: &mut Cx2d, audio: &dyn AudioSource) {
        if !self.doc.models.is_empty() {
            crate::models::pump(cx);
        }
        if self.renderer.in_progress() {
            self.advance(cx, audio);
        }
        self.renderer.reattach(cx)
    }
}
