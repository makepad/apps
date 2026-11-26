//! Evaluating a document at a time: the scene graph walked with every
//! value resolved, into a [`SceneFrame`]. Pure: the same document, time,
//! size and audio give the same frame, bit for bit.

use crate::anim::{Anim, AudioFeature, AudioSource, EvalCtx, ScriptEval};
use crate::doc::Scene3dDoc;
use crate::frame::*;
use makepad_geom3d::math::*;
use makepad_geom3d::mesh::*;
use crate::scene::*;
use crate::script::AudioNow;
use makepad_render_lines::{Segment, Sprite};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

/// What to evaluate against.
pub struct EvalInput<'a> {
    /// Scene time in seconds (the layer's local clock).
    pub t: f32,
    /// Output width / height.
    pub aspect: f32,
    pub audio: &'a dyn AudioSource,
    /// The edit time of the scene's t = 0 (audio is sampled on the edit's
    /// clock).
    pub edit_offset: f32,
}

/// Vertex-shader deformation of one draw (see the mesh shader's
/// `deform_pos`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Deform {
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub c: [f32; 4],
    pub d: [f32; 4],
}

impl Deform {
    pub const NONE: Deform = Deform { a: [0.0; 4], b: [0.0, 1.0, 0.0, 0.0], c: [0.0; 4], d: [0.0; 4] };
}

/// Per-document caches that do not change results (built on first use).
#[derive(Default)]
pub struct EvalCache {
    /// Rest-pose primitives of models without skin or morphs.
    prims: HashMap<(usize, usize, usize), SharedMesh>,
    /// Per model: the fit/ground matrix from its rest bounds.
    fits: HashMap<usize, Mat4f>,
    /// Ink hulls of static meshes, by geometry key.
    hulls: HashMap<u64, Arc<Mesh>>,
}

pub(crate) struct Walk<'a, 'b> {
    pub(crate) t: f32,
    pub(crate) edit_offset: f32,
    audio: &'a dyn AudioSource,
    pub(crate) script: &'b mut Scene3dDoc,
    frame: SceneFrame,
    named: HashMap<String, [f32; 3]>,
    cache: &'b mut EvalCache,
    /// Deferred instance closures (node def pointer, world, opacity).
    lights_seen: usize,
    /// Effect physics, seeked to `t` (taken out of the document for the
    /// walk).
    phys: Option<&'b mut crate::physics::PhysicsRuntime>,
}

impl<'a, 'b> Walk<'a, 'b> {
    fn cx(&mut self) -> EvalCtx<'_> {
        EvalCtx { audio: self.audio, edit_offset: self.edit_offset, script: Some(self.script as &mut dyn ScriptEval) }
    }
    /// A kernel's per-frame document arrays at `t`.
    pub(crate) fn frame_data(&mut self, list: &[(String, u32)], t: f32) -> Vec<(String, Arc<Vec<f32>>)> {
        let edit = t + self.edit_offset;
        list.iter()
            .map(|(name, index)| {
                let mut v = Vec::new();
                self.script.eval_script_array(*index, t, edit, &mut v);
                (name.clone(), Arc::new(v))
            })
            .collect()
    }
    pub(crate) fn a1(&mut self, a: &Anim<1>, t: f32) -> f32 {
        let mut cx = self.cx();
        a.eval(t, &mut cx)[0]
    }
    pub(crate) fn a3(&mut self, a: &Anim<3>, t: f32) -> [f32; 3] {
        let mut cx = self.cx();
        a.eval(t, &mut cx)
    }
    pub(crate) fn a4(&mut self, a: &Anim<4>, t: f32) -> [f32; 4] {
        let mut cx = self.cx();
        a.eval(t, &mut cx)
    }
}

impl Scene3dDoc {
    /// The frame at `input.t`.
    pub fn eval(&mut self, input: &EvalInput, cache: &mut EvalCache) -> SceneFrame {
        self.begin_evaluation();
        crate::kernels::clear_runs();
        let def = self.def.clone();
        // Closures see the audio at this edit time.
        let te = input.t + input.edit_offset;
        let src = def.music.as_deref();
        let s = |f: AudioFeature| input.audio.sample(src, &f, te);
        self.set_audio_now(AudioNow {
            energy: s(AudioFeature::Energy),
            low: s(AudioFeature::Band("low".into())),
            mid: s(AudioFeature::Band("mid".into())),
            high: s(AudioFeature::Band("high".into())),
            beat: s(AudioFeature::BeatPulse),
            beat_phase: s(AudioFeature::BeatPhase),
            bar_phase: s(AudioFeature::BarPhase),
            beat_index: s(AudioFeature::BeatIndex),
            onset: s(AudioFeature::Onset),
            voice: s(AudioFeature::Voice),
        });
        let frame = SceneFrame {
            t: input.t,
            aspect: input.aspect,
            camera: CameraFrame {
                view: Mat4f::identity(),
                proj: Mat4f::identity(),
                position: [0.0; 3],
                forward: [0.0, 0.0, -1.0],
                near: 0.1,
                far: 100.0,
                fov_y: 35.0,
                orthographic: false,
                height: 5.0,
                aspect: input.aspect,
                sensor: 24.0,
            },
            clear: None,
            env: None,
            ambient: [0.0; 3],
            hemisphere: None,
            lights: Vec::new(),
            fog: None,
            meshes: Vec::new(),
            points: Vec::new(),
            lines: Vec::new(),
            post: PostFrame::default(),
            tone_mapping: def.tone_mapping,
            exposure: 1.0,
            motion_blur: None,
            anchors: Vec::new(),
            errors: Vec::new(),
            bounds: None,
            reflector: None,
            contact: None,
            grade: None,
            temporal_aa: def.post.temporal_aa,
        };
        // Physics steps to this time first (kinematic bodies read the
        // document's keyables, so the document is free for it here).
        let mut phys = self.physics.take();
        if let Some(p) = phys.as_mut() {
            p.seek(input.t, self, input.audio, input.edit_offset, &def.root);
        }
        let phys_errors = phys.as_ref().map(|p| p.errors()).unwrap_or_default();
        let mut w = Walk { t: input.t, edit_offset: input.edit_offset, audio: input.audio, script: self, frame, named: HashMap::new(), cache, lights_seen: 0, phys: phys.as_mut() };
        w.frame.errors.extend(phys_errors.into_iter().map(|e| format!("physics: {e}")));
        let t = w.t;
        w.frame.clear = def.background.as_ref().map(|b| w.a4(b, t));
        w.frame.exposure = w.a1(&def.exposure, t);
        if let Some(env) = &def.environment {
            let intensity = w.a1(&env.intensity, t);
            let rotation = w.a1(&env.rotation, t);
            let background_intensity = w.a1(&env.background_intensity, t);
            w.frame.env = Some(EnvFrame { key: env.key, map: env.map.clone(), intensity, rotation, background: env.background, blur: env.blur, background_intensity });
        }
        if let Some(f) = &def.fog {
            let c = w.a4(&f.color, t);
            let near = w.a1(&f.near, t);
            let far = w.a1(&f.far, t);
            let density = f.density.as_ref().map(|d| w.a1(d, t)).unwrap_or(0.0);
            w.frame.fog = Some(FogFrame { color: [c[0], c[1], c[2]], near, far: far.max(near + 1e-3), density });
        }
        // The camera first for nodes that face it (it is placed again
        // below, once named nodes it may aim at are known).
        w.camera(&def.camera);
        w.node(&def.root, &Mat4f::identity(), 1.0);
        let _ = w.lights_seen;
        w.frame.bounds = scene_bounds(&w.frame.meshes);
        w.camera(&def.camera);
        w.anchors();
        w.post(&def.post);
        w.shadows(&def.root);
        w.finish(&def.post);
        let mut frame = w.frame;
        self.physics = phys;
        // A non-number (a division by zero, a helper called with the wrong
        // arguments) would drop what it touches without a word: say so.
        if let Some(what) = non_finite(&frame) {
            frame.errors.push(format!("the scene computed a non-number (NaN or infinity) for {what} at t = {:.2} s", input.t));
        }
        frame
    }
}

/// The first part of a frame that holds a non-number.
fn non_finite(f: &SceneFrame) -> Option<String> {
    let bad = |v: &[f32]| v.iter().any(|x| !x.is_finite());
    if bad(&f.camera.position) || bad(&f.camera.view.v) || bad(&f.camera.proj.v) {
        return Some("the camera".into());
    }
    if let Some(k) = f.meshes.iter().position(|m| bad(makepad_render::TransformTint::floats(&m.instances))) {
        return Some(format!("mesh {k}'s placement"));
    }
    if let Some(k) = f.lights.iter().position(|l| bad(&l.position) || bad(&l.direction) || bad(&l.color)) {
        return Some(format!("light {k}"));
    }
    if f.points.iter().any(|p| p.points.iter().any(|s| bad(&s.pos) || bad(&s.color) || !s.size.is_finite() || !s.rotation.is_finite())) {
        return Some("points".into());
    }
    let bad_segment = |s: &Segment| bad(&s.a) || bad(&s.b) || bad(&s.color_a) || bad(&s.color_b) || !(s.width_a.is_finite() && s.width_b.is_finite() && s.dist_a.is_finite() && s.dist_b.is_finite());
    if f.lines.iter().any(|l| l.segments.iter().any(bad_segment)) {
        return Some("lines".into());
    }
    None
}

fn scene_bounds(meshes: &[MeshDraw]) -> Option<([f32; 3], [f32; 3])> {
    let mut out: Option<([f32; 3], [f32; 3])> = None;
    for m in meshes.iter().filter(|m| m.cast_shadow || m.receive_shadow) {
        let (a, b) = m.bounds;
        out = Some(match out {
            None => (a, b),
            Some((x, y)) => ([x[0].min(a[0]), x[1].min(a[1]), x[2].min(a[2])], [y[0].max(b[0]), y[1].max(b[1]), y[2].max(b[2])]),
        });
    }
    out
}

/// The world AABB of a mesh AABB under a matrix.
fn transformed_bounds(m: &Mat4f, (a, b): ([f32; 3], [f32; 3])) -> ([f32; 3], [f32; 3]) {
    let mut mn = [f32::MAX; 3];
    let mut mx = [f32::MIN; 3];
    for k in 0..8 {
        let p = [if k & 1 == 0 { a[0] } else { b[0] }, if k & 2 == 0 { a[1] } else { b[1] }, if k & 4 == 0 { a[2] } else { b[2] }];
        let q = transform_point(m, p);
        for i in 0..3 {
            mn[i] = mn[i].min(q[i]);
            mx[i] = mx[i].max(q[i]);
        }
    }
    (mn, mx)
}

fn union(a: ([f32; 3], [f32; 3]), b: ([f32; 3], [f32; 3])) -> ([f32; 3], [f32; 3]) {
    ([a.0[0].min(b.0[0]), a.0[1].min(b.0[1]), a.0[2].min(b.0[2])], [a.1[0].max(b.1[0]), a.1[1].max(b.1[1]), a.1[2].max(b.1[2])])
}


/// Hue rotation of a linear colour by `turns` (0..1).
fn hue_shift(c: [f32; 4], turns: f32) -> [f32; 4] {
    if turns == 0.0 {
        return c;
    }
    let hsv = vec4(c[0], c[1], c[2], c[3]).to_hsva();
    let o = Vec4f::from_hsva(vec4((hsv.x + turns).rem_euclid(1.0), hsv.y, hsv.z, hsv.w));
    [o.x, o.y, o.z, c[3]]
}

/// Delay ranks for `n` staggered units.
pub(crate) fn ranks(order: Order, n: usize, seed: u64, distance: Option<&[f32]>) -> Vec<f32> {
    match order {
        Order::Forward => (0..n).map(|i| i as f32).collect(),
        Order::Reverse => (0..n).map(|i| (n - 1 - i) as f32).collect(),
        Order::Center => {
            let c = (n as f32 - 1.0) * 0.5;
            (0..n).map(|i| (i as f32 - c).abs()).collect()
        }
        Order::Edges => {
            let c = (n as f32 - 1.0) * 0.5;
            (0..n).map(|i| c - (i as f32 - c).abs()).collect()
        }
        Order::Random => {
            // A permutation, so no two units share a slot.
            let mut idx: Vec<usize> = (0..n).collect();
            idx.sort_by(|a, b| rand01(seed, *a as u64).partial_cmp(&rand01(seed, *b as u64)).unwrap());
            let mut r = vec![0.0; n];
            for (rank, i) in idx.into_iter().enumerate() {
                r[i] = rank as f32;
            }
            r
        }
        Order::Distance => distance.map(|d| d.to_vec()).unwrap_or_else(|| (0..n).map(|i| i as f32).collect()),
        // Instances rank by their own random (see `instances`); elsewhere evenly.
        Order::Phase => (0..n).map(|i| i as f32 / n.max(1) as f32).collect(),
    }
}

impl<'a, 'b> Walk<'a, 'b> {
    fn local_matrix(&mut self, tr: &TransformDef, parent: &Mat4f) -> Mat4f {
        let t = self.t;
        let p = self.a3(&tr.position, t);
        let s = self.a3(&tr.scale, t);
        let pivot = self.a3(&tr.pivot, t);
        let q = match &tr.look_at {
            _ if tr.face_camera => {
                // Face the camera, then turn by `rotation` in that frame.
                let world_pos = transform_point(parent, p);
                let r = self.a3(&tr.rotation, t);
                quat_mul(look_rotation(sub3(self.frame.camera.position, world_pos), [0.0, 1.0, 0.0]), quat_from_euler_deg(r))
            }
            Some(la) => {
                // Aim +z at a world point (parent rotation ignored).
                let world_pos = transform_point(parent, p);
                let target = self.a3(la, t);
                look_rotation(sub3(target, world_pos), [0.0, 1.0, 0.0])
            }
            None => {
                let r = self.a3(&tr.rotation, t);
                quat_from_euler_deg(r)
            }
        };
        let m = compose(p, q, s);
        if pivot == [0.0; 3] {
            m
        } else {
            mul(&m, &translation([-pivot[0], -pivot[1], -pivot[2]]))
        }
    }

    fn node(&mut self, n: &NodeDef, parent: &Mat4f, parent_opacity: f32) {
        let t = self.t;
        if let Some((start, len)) = n.span {
            if t < start || t >= start + len {
                return;
            }
        }
        if self.a1(&n.visible, t) < 0.5 {
            return;
        }
        let opacity = parent_opacity * self.a1(&n.opacity, t).clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return;
        }
        let local = self.local_matrix(&n.transform, parent);
        let world = mul(parent, &local);
        if let Some(name) = &n.name {
            self.named.insert(name.clone(), mat_translation(&world));
        }
        match &n.kind {
            NodeKind::Group => {}
            NodeKind::Mesh { geometry, material, deform } => {
                let (mesh, key, dynamic) = match geometry {
                    GeometrySrc::Static(m) => (m.mesh.clone(), m.key, false),
                    GeometrySrc::Tube { tube, start, end } => {
                        let (s, e) = (self.a1(start, t).clamp(0.0, 1.0), self.a1(end, t).clamp(0.0, 1.0));
                        let key = hash64(n.path.bytes().fold(0x7b0e_u64, |h, b| h.wrapping_mul(0x100_0000_01b3) ^ b as u64));
                        (Arc::new(makepad_geom3d::tube_trimmed(tube, s.min(e), s.max(e))), key, true)
                    }
                    GeometrySrc::Engine(p) => {
                        let key = hash64(n.path.bytes().fold(0x7b1e_u64, |h, b| h.wrapping_mul(0x100_0000_01b3) ^ b as u64));
                        (Arc::new(crate::engines::build(p, t)), key, true)
                    }
                };
                let mat = self.material(material, opacity);
                let deform = self.deform(deform, &mesh);
                self.push_mesh(n, mesh, key, dynamic, mat, deform, &[(world, [1.0; 4], 1.0, 0.0)]);
            }
            NodeKind::Model(m) => self.model(n, m, &world, opacity),
            NodeKind::Text3D(td) => crate::text3d_node::eval(self, n, td, &world, opacity),
            NodeKind::Instances(id) => self.instances(n, id, &world, opacity),
            NodeKind::Points(pd) => self.points(n, pd, &world, opacity),
            NodeKind::Line(ld) => self.line(ld, &world, opacity),
            NodeKind::Light(l) => self.light(l, &world),
        }
        for c in &n.children {
            self.node(c, &world, opacity);
        }
    }

    pub(crate) fn deform(&mut self, d: &DeformDef, mesh: &Mesh) -> Deform {
        let t = self.t;
        let mut a = [0.0; 4];
        let mut b = [0.0; 4];
        let mut c = [0.0; 4];
        let mut e = [0.0; 4];
        if let Some((amount, freq, speed)) = &d.displace {
            a[0] = self.a1(amount, t);
            a[1] = self.a1(freq, t);
            a[2] = self.a1(speed, t) * t;
        }
        if let Some(tw) = &d.twist {
            a[3] = self.a1(tw, t);
        }
        if let Some(wr) = &d.wrap {
            b[3] = self.a1(wr, t).max(0.0);
        }
        if let Some(bd) = &d.bend {
            b[0] = self.a1(bd, t);
        }
        let bounds = mesh.bounds();
        let h = bounds.map(|(mn, mx)| mx[1] - mn[1]).unwrap_or(1.0).max(1e-4);
        b[1] = h;
        if let Some(x) = &d.explode {
            b[2] = self.a1(x, t);
        }
        if let Some((amount, freq, speed)) = &d.ripple {
            c = [self.a1(amount, t), self.a1(freq, t), self.a1(speed, t) * t, 0.0];
        }
        if let Some((p, radius)) = &d.curl {
            let (x0, x1) = bounds.map(|(mn, mx)| (mn[0], mx[0])).unwrap_or((-0.5, 0.5));
            e = [self.a1(p, t).clamp(0.0, 1.0), *radius, x1, (x1 - x0).max(1e-4)];
        }
        Deform { a, b, c, d: e }
    }

    pub(crate) fn material(&mut self, m: &MaterialDef, opacity: f32) -> MaterialFrame {
        let t = self.t;
        let emissive = self.a4(&m.emissive, t);
        let ei = self.a1(&m.emissive_intensity, t);
        let rim = self.a4(&m.rim_color, t);
        let att = self.a4(&m.attenuation_color, t);
        let sheen_color = self.a4(&m.sheen_color, t);
        let mut custom = [0.0; 8];
        for (i, (_, a)) in m.uniforms.iter().enumerate().take(8) {
            custom[i] = self.a1(a, t);
        }
        MaterialFrame {
            model: m.model,
            color: self.a4(&m.color, t),
            opacity: (self.a1(&m.opacity, t) * opacity).clamp(0.0, 1.0),
            metalness: self.a1(&m.metalness, t).clamp(0.0, 1.0),
            roughness: self.a1(&m.roughness, t).clamp(0.02, 1.0),
            emissive: [emissive[0] * ei, emissive[1] * ei, emissive[2] * ei],
            clearcoat: self.a1(&m.clearcoat, t).clamp(0.0, 1.0),
            clearcoat_roughness: self.a1(&m.clearcoat_roughness, t).clamp(0.02, 1.0),
            transmission: self.a1(&m.transmission, t).clamp(0.0, 1.0),
            ior: self.a1(&m.ior, t).clamp(1.0, 3.0),
            thickness: self.a1(&m.thickness, t).max(0.0),
            attenuation: [att[0], att[1], att[2]],
            sheen: self.a1(&m.sheen, t).clamp(0.0, 1.0),
            sheen_color: [sheen_color[0], sheen_color[1], sheen_color[2]],
            iridescence: self.a1(&m.iridescence, t).clamp(0.0, 1.0),
            specular: self.a1(&m.specular_intensity, t).max(0.0),
            env_intensity: self.a1(&m.env_intensity, t).max(0.0),
            rim: [rim[0], rim[1], rim[2]],
            rim_power: self.a1(&m.rim_power, t).max(0.0),
            steps: m.steps,
            matcap: m.matcap,
            maps: m.maps.clone(),
            normal_scale: m.normal_scale,
            occlusion_strength: m.occlusion_strength,
            alpha_cutoff: m.alpha_cutoff,
            vertex_colors: m.vertex_colors,
            flat_shading: m.flat_shading,
            wireframe: m.wireframe,
            side: m.side,
            blending: m.blending,
            transparent: m.transparent,
            depth_write: m.depth_write,
            shader: m.shader.clone(),
            custom,
            flakes: [self.a1(&m.flakes, t).max(0.0), m.flake_scale],
            brushed: [self.a1(&m.brushed, t).max(0.0), m.brush_scale],
            reflectivity: self.a1(&m.reflectivity, t).clamp(0.0, 1.0),
            reflection_blur: self.a1(&m.reflection_blur, t).clamp(0.0, 1.0),
            refraction_env: m.refraction_env,
            fade: m.fade,
            external: m.external.clone(),
            fit: m.fit,
            crop: m.crop.as_ref().map(|c| self.a4(c, t)),
            aspect: m.aspect,
            map_space: m.map_space,
            ink: self.a1(&m.ink, t).max(0.0),
            ink_color: self.a4(&m.ink_color, t),
            wire_fill: m.wire_fill.as_ref().map(|c| self.a4(c, t)),
            wire_width: m.wire_width,
            shadow_catcher: m.shadow_catcher,
        }
        .with_look(self.script.def.look)
    }

    /// Push a mesh with (world, colour multiplier, opacity, glow) instances:
    /// a copy's glow is light in its own colour (TransformTint::glow).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_mesh(&mut self, n: &NodeDef, mesh: Arc<Mesh>, key: u64, dynamic: bool, mut material: MaterialFrame, deform: Deform, inst: &[(Mat4f, [f32; 4], f32, f32)]) {
        if inst.is_empty() || mesh.is_empty() {
            return;
        }
        let local_bounds = mesh.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
        // A fitted picture needs the surface's aspect: a flat mesh's x/y extent.
        if material.aspect <= 0.0 && material.fit != crate::scene::MapFit::Fill {
            let (w, h) = (local_bounds.1[0] - local_bounds.0[0], local_bounds.1[1] - local_bounds.0[1]);
            material.aspect = if h > 1e-6 && w > 1e-6 { w / h } else { 1.0 };
        }
        // Displacement reaches past the mesh's own bounds.
        let pad = deform.a[0].abs() + deform.b[2].abs() + deform.c[0].abs() + deform.d[1] * 2.0;
        let lb = ([local_bounds.0[0] - pad, local_bounds.0[1] - pad, local_bounds.0[2] - pad], [local_bounds.1[0] + pad, local_bounds.1[1] + pad, local_bounds.1[2] + pad]);
        let mut instances = Vec::with_capacity(inst.len());
        let mut bounds: Option<([f32; 3], [f32; 3])> = None;
        for (m, c, o, glow) in inst {
            if *o <= 0.0 {
                continue;
            }
            instances.push(makepad_render::TransformTint { transform: *m, tint: vec4(c[0], c[1], c[2], c[3] * o), glow: *glow });
            let b = transformed_bounds(m, lb);
            bounds = Some(match bounds {
                None => b,
                Some(x) => union(x, b),
            });
        }
        let Some(bounds) = bounds else { return };
        let ext = sub3(bounds.1, bounds.0);
        let ground = n.receive_shadow && ext[1] <= 1e-3 * ext[0].max(ext[2]).max(1e-3) && ext[0].max(ext[2]) > 0.0;
        self.frame.meshes.push(MeshDraw {
            node: n.id,
            ground,
            geo_key: key,
            mesh,
            dynamic,
            material,
            instances,
            cast_shadow: n.cast_shadow,
            receive_shadow: n.receive_shadow,
            outline: n.outline,
            deform: deform.a,
            deform2: deform.b,
            deform3: deform.c,
            deform4: deform.d,
            ink: 0.0,
            bounds,
        });
        // An ink outline: the inverted hull (reversed winding, smoothed
        // normals) drawn in the ink colour and pushed out in the vertex
        // shader; its front faces are culled, so only the rim shows.
        let last = self.frame.meshes.last().unwrap();
        if last.material.ink > 0.0 && !last.material.is_blended() && !last.material.wireframe {
            let hull_key = last.geo_key ^ 0x1_4b5_0bad_c0de;
            let hull = match self.cache.hulls.get(&last.geo_key) {
                Some(h) if !last.dynamic => h.clone(),
                _ => {
                    let h = Arc::new(makepad_geom3d::mesh::ink_hull(&last.mesh));
                    if !last.dynamic {
                        self.cache.hulls.insert(last.geo_key, h.clone());
                    }
                    h
                }
            };
            let mut m = last.material.clone();
            let (width, color) = (m.ink, m.ink_color);
            m.model = crate::scene::ShadingModel::Basic;
            m.color = [color[0], color[1], color[2], 1.0];
            m.emissive = [0.0; 3];
            m.maps = Default::default();
            m.external = None;
            m.shader = None;
            m.fit = crate::scene::MapFit::Fill;
            m.side = crate::scene::FaceSide::Front;
            m.ink = 0.0;
            m.rim_power = 0.0;
            m.reflectivity = 0.0;
            m.vertex_colors = false;
            let mut hull_draw = last.clone();
            hull_draw.geo_key = hull_key;
            hull_draw.mesh = hull;
            hull_draw.material = m;
            hull_draw.cast_shadow = false;
            hull_draw.receive_shadow = false;
            hull_draw.outline = false;
            hull_draw.ground = false;
            hull_draw.ink = width;
            self.frame.meshes.push(hull_draw);
        }
    }

    fn model(&mut self, n: &NodeDef, m: &ModelDef, world: &Mat4f, opacity: f32) {
        let t = self.t;
        let model = m.model.clone();
        let model_id = Arc::as_ptr(&model) as usize;
        let fit = *self.cache.fits.entry(model_id ^ (m.fit.to_bits() as usize) ^ ((m.ground as usize) << 1)).or_insert_with(|| {
            let rest = model.rest_pose();
            match model.bounds(&rest) {
                Some((mn, mx)) if m.fit > 0.0 || m.ground => {
                    let size = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2]).max(1e-6);
                    let k = if m.fit > 0.0 { m.fit / size } else { 1.0 };
                    let cx = (mn[0] + mx[0]) * 0.5;
                    let cz = (mn[2] + mx[2]) * 0.5;
                    let cy = if m.ground { mn[1] } else { (mn[1] + mx[1]) * 0.5 };
                    mul(&scaling([k, k, k]), &translation([-cx, -cy, -cz]))
                }
                _ => Mat4f::identity(),
            }
        });
        let base = mul(world, &fit);
        let mut pose = model.rest_pose();
        for c in &m.clips {
            let weight = self.a1(&c.weight, t).clamp(0.0, 1.0);
            if weight <= 0.0 {
                continue;
            }
            let dur = model.clips[c.clip].duration.max(1e-6);
            let mut ct = match &c.time {
                Some(a) => self.a1(a, t),
                None => (t - c.offset) * c.speed,
            };
            if c.looping {
                ct = ct.rem_euclid(dur);
            }
            model.apply_clip(&mut pose, c.clip, ct, weight);
        }
        let mut hidden = Vec::new();
        for o in &m.nodes {
            if let Some(p) = &o.position {
                pose.t[o.node] = self.a3(p, t);
            }
            if let Some(r) = &o.rotation {
                let e = self.a3(r, t);
                pose.r[o.node] = quat_from_euler_deg(e);
            }
            if let Some(s) = &o.scale {
                pose.s[o.node] = self.a3(s, t);
            }
            if let Some(v) = &o.visible {
                if self.a1(v, t) < 0.5 {
                    hidden.push(o.node);
                }
            }
        }
        for (node, target, a) in &m.morphs {
            let v = self.a1(a, t);
            if let Some(w) = pose.weights.get_mut(*node).and_then(|w| w.get_mut(*target)) {
                *w = v;
            }
        }
        let worlds = model.world_matrices(&pose);
        // A hidden node hides its subtree.
        let is_hidden = |mut i: usize| loop {
            if hidden.contains(&i) {
                return true;
            }
            match model.nodes[i].parent {
                Some(p) => i = p,
                None => return false,
            }
        };
        let animated_morphs = !m.morphs.is_empty();
        for (node, mesh_index) in model.scene_nodes() {
            if is_hidden(node) {
                continue;
            }
            let gm = &model.meshes[mesh_index];
            let skinned = model.nodes[node].skin.is_some();
            for (pi, prim) in gm.primitives.iter().enumerate() {
                let needs_pose = skinned || (!prim.targets.is_empty() && (animated_morphs || !m.clips.is_empty() || pose.weights.get(node).map(|w| w.iter().any(|x| *x != 0.0)).unwrap_or(false)));
                let (mesh, key, dynamic, inst_world) = if needs_pose {
                    let (posed, space) = model.posed_primitive(node, pi, &pose, &worlds);
                    let key = hash64((model_id as u64) ^ ((node as u64) << 20) ^ ((pi as u64) << 40) ^ hash64(n.path.len() as u64));
                    let w = match space {
                        crate::glb::MeshSpace::World => base,
                        crate::glb::MeshSpace::NodeLocal => mul(&base, &worlds[node]),
                    };
                    (Arc::new(posed), key, true, w)
                } else {
                    let explode = m.deform.explode.is_some();
                    let shared = self.cache.prims.entry((model_id ^ (explode as usize) << (usize::BITS - 4), node, pi)).or_insert_with(|| SharedMesh::new(if explode { crate::doc::explodable(&prim.mesh) } else { prim.mesh.clone() }));
                    (shared.mesh.clone(), shared.key, false, mul(&base, &worlds[node]))
                };
                let mut mat_def = prim.material.and_then(|i| m.base_materials.get(i)).cloned().unwrap_or_default();
                for o in &m.materials {
                    if o.material.is_none() || o.material == prim.material {
                        if o.set.iter().any(|s| s == "*") {
                            mat_def = o.def.clone();
                        } else {
                            apply_override(&mut mat_def, &o.def, &o.set);
                        }
                    }
                }
                if !prim.mesh.colors.is_empty() {
                    mat_def.vertex_colors = true;
                }
                let mat = self.material(&mat_def, opacity);
                let dm = self.deform(&m.deform, &mesh);
                self.push_mesh(n, mesh, key, dynamic, mat, dm, &[(inst_world, [1.0; 4], 1.0, 0.0)]);
            }
        }
    }

    /// Instances from a physics group: one per live body at its pose.
    fn body_instances(&mut self, n: &NodeDef, id: &InstancesDef, group: &str, world: &Mat4f, opacity: f32) {
        let t = self.t;
        let mut recs = Vec::new();
        if let Some(p) = self.phys.as_deref_mut() {
            p.bodies(group, t, &mut recs);
        }
        let mat = self.material(&id.material, opacity);
        let deform = self.deform(&id.deform, &id.geometry.mesh);
        let each_scale = id.each.scale.as_ref().map(|a| self.a3(a, t)).unwrap_or([1.0; 3]);
        let inst: Vec<_> = recs
            .iter()
            .map(|b| {
                let s = [b.scale[0] * each_scale[0], b.scale[1] * each_scale[1], b.scale[2] * each_scale[2]];
                let m = mul(world, &compose(b.pos, b.rot, s));
                (m, b.color, 1.0, 0.0)
            })
            .collect();
        self.push_mesh(n, id.geometry.mesh.clone(), id.geometry.key, false, mat, deform, &inst);
    }

    fn instances(&mut self, n: &NodeDef, id: &InstancesDef, world: &Mat4f, opacity: f32) {
        if let Some(g) = &id.bodies {
            return self.body_instances(n, id, g, world, opacity);
        }
        let t = self.t;
        let count = id.bases.len();
        let dist: Option<Vec<f32>> = if id.order == Order::Distance {
            Some(id.bases.iter().map(|b| len3(b.position)).collect())
        } else {
            None
        };
        let rank = if id.order == Order::Phase { id.bases.iter().map(|b| b.rand[0]).collect() } else { ranks(id.order, count, 17, dist.as_deref()) };
        let mat = self.material(&id.material, opacity);
        let deform = self.deform(&id.deform, &id.geometry.mesh);
        let mut inst = Vec::with_capacity(count);
        let each = &id.each;
        let base_color = mat.color;
        for (i, b) in id.bases.iter().enumerate() {
            let lt = t - rank[i] * id.stagger;
            let mut pos = b.position;
            let mut rot = [0.0; 3];
            let mut scale = [1.0; 3];
            let mut color = [1.0; 4];
            let mut o = 1.0;
            if let Some(a) = &each.position {
                pos = add3(pos, self.a3(a, lt));
            }
            if let Some(a) = &each.rotation {
                rot = self.a3(a, lt);
            }
            if let Some(a) = &each.scale {
                scale = self.a3(a, lt);
            }
            if let Some(a) = &each.opacity {
                o = self.a1(a, lt).clamp(0.0, 1.0);
            }
            if let Some(a) = &each.color {
                let c = self.a4(a, lt);
                color = [c[0] / base_color[0].max(1e-4), c[1] / base_color[1].max(1e-4), c[2] / base_color[2].max(1e-4), c[3]];
            }
            // A glow in the instance's own colour, as strong as the
            // emissive's brightest channel.
            let glow = each.emissive.as_ref().map(|a| { let e = self.a4(a, lt); e[0].max(e[1]).max(e[2]) }).unwrap_or(0.0);
            if let Some(hf) = &id.heights {
                // The box grows from its base: taller by `h`, its centre up by h/2.
                let amount = self.a1(&hf.amount, t);
                let h = height_at(hf, amount, b.position[0], b.position[2], t);
                let g = id.geometry.mesh.bounds().map(|(mn, mx)| mx[1] - mn[1]).unwrap_or(1.0).max(1e-3);
                let h = h.max(-g * 0.9);
                scale[1] *= (g + h) / g;
                pos[1] += h * 0.5;
            }
            if let Some(f) = id.each_fn {
                let out = self.script.eval_each(f, i, count, lt, lt + self.edit_offset);
                if let Some(p) = out.position {
                    pos = p;
                }
                if let Some(r) = out.rotation {
                    rot = r;
                }
                if let Some(s) = out.scale {
                    scale = s;
                }
                if let Some(c) = out.color {
                    color = [c[0] / base_color[0].max(1e-4), c[1] / base_color[1].max(1e-4), c[2] / base_color[2].max(1e-4), c[3]];
                }
            }
            let r = b.rand;
            if let Some((target, progress)) = &id.morph {
                let k = self.a1(progress, lt).clamp(0.0, 1.0);
                let to = target[i % target.len()].position;
                let lift = (k * std::f32::consts::PI).sin() * 0.5 * (0.5 + r[2]);
                pos = [pos[0] + (to[0] - b.position[0]) * k, pos[1] + (to[1] - b.position[1]) * k + lift, pos[2] + (to[2] - b.position[2]) * k];
            }
            if let Some((amount, radius)) = &id.scatter {
                let a = self.a1(amount, lt);
                if a != 0.0 {
                    let dir = normalize_or_up([r[0] * 2.0 - 1.0, r[1] * 2.0 - 1.0 + 0.3, r[2] * 2.0 - 1.0]);
                    pos = add3(pos, scale3(dir, a * radius * (0.4 + 0.6 * r[3])));
                    rot = add3(rot, [a * 360.0 * (r[1] - 0.5), a * 540.0 * (r[2] - 0.5), a * 360.0 * (r[3] - 0.5)]);
                }
            }
            if id.rotation_jitter != 0.0 {
                rot = [rot[0] + (r[1] - 0.5) * 2.0 * id.rotation_jitter, rot[1] + (r[2] - 0.5) * 2.0 * id.rotation_jitter, rot[2] + (r[3] - 0.5) * 2.0 * id.rotation_jitter];
            }
            if id.scale_jitter != 0.0 {
                let k = 1.0 + (r[0] - 0.5) * 2.0 * id.scale_jitter;
                scale = scale3(scale, k.max(0.0));
            }
            if id.color_jitter != 0.0 {
                let shifted = hue_shift([base_color[0] * color[0], base_color[1] * color[1], base_color[2] * color[2], 1.0], (r[1] - 0.5) * id.color_jitter);
                color = [shifted[0] / base_color[0].max(1e-4), shifted[1] / base_color[1].max(1e-4), shifted[2] / base_color[2].max(1e-4), color[3]];
            }
            let q = quat_from_euler_deg(rot);
            let q = if id.align { quat_mul(look_rotation(b.normal, if b.normal[1].abs() > 0.99 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] }), q) } else { q };
            let m = mul(world, &compose(pos, q, scale));
            inst.push((m, color, o, glow));
        }
        self.push_mesh(n, id.geometry.mesh.clone(), id.geometry.key, false, mat, deform, &inst);
    }

    fn points(&mut self, n: &NodeDef, pd: &PointsDef, world: &Mat4f, opacity: f32) {
        let t = self.t;
        let size = self.a1(&pd.size, t);
        let color = self.a4(&pd.color, t);
        let op = self.a1(&pd.opacity, t) * opacity;
        let mut pts = Vec::new();
        let mut mesh_inst = Vec::new();
        match &pd.source {
            PointSource::Layout(bases) => {
                pts.reserve(bases.len());
                for (i, b) in bases.iter().enumerate() {
                    let lt = t - i as f32 * pd.stagger;
                    let mut p = b.position;
                    if let Some(a) = &pd.each.position {
                        p = add3(p, self.a3(a, lt));
                    }
                    let s = pd.each.scale.as_ref().map(|a| self.a3(a, lt)[0]).unwrap_or(1.0) * size;
                    let c = pd.each.color.as_ref().map(|a| self.a4(a, lt)).unwrap_or(color);
                    let o = pd.each.opacity.as_ref().map(|a| self.a1(a, lt)).unwrap_or(1.0) * op;
                    let w = transform_point(world, p);
                    if pd.mesh.is_some() {
                        mesh_inst.push((mul(world, &compose(p, QUAT_ID, [s, s, s])), [1.0; 4], o, 0.0));
                    } else {
                        pts.push(Sprite { pos: w, size: s, color: [c[0], c[1], c[2], c[3] * o], rotation: b.rand[0] * 6.2831, birth: 0.0 });
                    }
                }
            }
            PointSource::Particles { system, emitter_points, path } => {
                let mut particles = system.eval(t, emitter_points.as_deref());
                if let Some((curve, duration)) = path {
                    let arc = curve.arc();
                    for p in &mut particles {
                        let born = t - p.age;
                        let u = (born / duration.max(1e-3)).rem_euclid(1.0);
                        p.position = add3(p.position, arc.point_at(u));
                    }
                }
                pts.reserve(particles.len());
                for p in particles {
                    let w = transform_point(world, p.position);
                    let s = p.size * size;
                    let c = [p.color[0] * color[0], p.color[1] * color[1], p.color[2] * color[2], p.color[3] * color[3] * op];
                    if pd.mesh.is_some() {
                        let q = quat_axis_angle([0.3, 1.0, 0.2], p.rotation);
                        mesh_inst.push((mul(world, &compose(p.position, q, [s, s, s])), [p.color[0], p.color[1], p.color[2], 1.0], p.color[3] * op, 0.0));
                    } else {
                        pts.push(Sprite { pos: w, size: s, color: c, rotation: p.rotation, birth: 0.0 });
                    }
                }
            }
            PointSource::Bodies(group) => {
                let mut recs = Vec::new();
                if let Some(p) = self.phys.as_deref_mut() {
                    p.bodies(group, t, &mut recs);
                }
                for b in &recs {
                    let w = transform_point(world, b.pos);
                    let c = [b.color[0] * color[0], b.color[1] * color[1], b.color[2] * color[2], b.color[3] * color[3] * op];
                    pts.push(Sprite { pos: w, size, color: c, rotation: (b.seed >> 8) as f32 / 16_777_216.0 * 6.2831, birth: 0.0 });
                }
            }
            PointSource::Sim(name) => {
                if let Some((data, layout)) = self.phys.as_deref().and_then(|p| p.sim_state(name, t)) {
                    let field = |n: &str, ty| layout.fields.iter().find(|f| f.name == n && f.ty == ty).map(|f| f.offset as usize);
                    use makepad_script_compute::kernel::FieldTy;
                    let (pf, sf, cf) = (field("pos", FieldTy::Vec3), field("size", FieldTy::F32), field("color", FieldTy::Vec4));
                    let stride = (layout.stride as usize).max(1);
                    if let Some(pf) = pf {
                        for rec in data.chunks_exact(stride) {
                            let s = sf.map(|k| rec[k]).unwrap_or(1.0) * size;
                            let c = cf.map(|k| [rec[k], rec[k + 1], rec[k + 2], rec[k + 3]]).unwrap_or([1.0; 4]);
                            if s <= 0.0 || c[3] <= 0.0 {
                                continue;
                            }
                            let w = transform_point(world, [rec[pf], rec[pf + 1], rec[pf + 2]]);
                            pts.push(Sprite { pos: w, size: s, color: [c[0] * color[0], c[1] * color[1], c[2] * color[2], c[3] * color[3] * op], rotation: 0.0, birth: 0.0 });
                        }
                    }
                }
            }
            PointSource::Kernel(run) => {
                let params: Vec<(String, f32)> = run.params.iter().map(|(n, a)| (n.clone(), self.a1(a, t))).collect();
                let physics: Vec<(String, Arc<Vec<f32>>, usize)> = run
                    .physics
                    .iter()
                    .map(|(name, what)| {
                        let (data, n) = self.phys.as_deref().map(|p| p.input(what, t)).unwrap_or_else(|| (Arc::new(vec![0.0; 16]), 0));
                        (name.clone(), data, n)
                    })
                    .collect();
                let frame_data = self.frame_data(&run.frame_data, t);
            match run.run(t, &params, &physics, &frame_data, run.active.as_ref().map(|a| self.a1(a, t).max(0.0) as usize)) {
                    Ok(data) => {
                        let w = run.outputs[run.buffer].width;
                        pts.reserve(data.len() / w.max(1));
                        for rec in data.chunks_exact(w.max(1)) {
                            let (p, s, c) = match w {
                                8 => ([rec[0], rec[1], rec[2]], rec[3], [rec[4], rec[5], rec[6], rec[7]]),
                                4 => ([rec[0], rec[1], rec[2]], rec[3], [1.0; 4]),
                                3 => ([rec[0], rec[1], rec[2]], 1.0, [1.0; 4]),
                                _ => continue,
                            };
                            if s <= 0.0 || c[3] <= 0.0 {
                                continue;
                            }
                            let wp = transform_point(world, p);
                            let c = [c[0] * color[0], c[1] * color[1], c[2] * color[2], c[3] * color[3] * op];
                            if pd.mesh.is_some() {
                                mesh_inst.push((mul(world, &compose(p, QUAT_ID, [s * size; 3])), [c[0], c[1], c[2], 1.0], c[3], 0.0));
                            } else {
                                pts.push(Sprite { pos: wp, size: s * size, color: c, rotation: 0.0, birth: 0.0 });
                            }
                        }
                    }
                    Err(e) => self.frame.errors.push(e),
                }
            }
        }
        if let Some((mesh, mat_def)) = &pd.mesh {
            let mat = self.material(mat_def, 1.0);
            self.push_mesh(n, mesh.mesh.clone(), mesh.key, false, mat, Deform::NONE, &mesh_inst);
        } else if !pts.is_empty() {
            // Size in world units → the renderer scales by the world matrix's scale.
            let sx = len3([world.v[0], world.v[1], world.v[2]]);
            if (sx - 1.0).abs() > 1e-4 {
                for p in &mut pts {
                    p.size *= sx;
                }
            }
            self.frame.points.push(PointsDraw { points: pts, shape: pd.shape, blending: pd.blending, size_attenuation: pd.size_attenuation });
        }
    }

    fn line(&mut self, ld: &LineDef, world: &Mat4f, opacity: f32) {
        let t = self.t;
        let (s, e) = (self.a1(&ld.trim_start, t).clamp(0.0, 1.0), self.a1(&ld.trim_end, t).clamp(0.0, 1.0));
        let (s, e) = (s.min(e), s.max(e));
        if e - s <= 1e-5 {
            return;
        }
        let width = self.a1(&ld.width, t);
        let k_int = self.a1(&ld.intensity, t).max(0.0);
        let glow = |c: [f32; 4]| [c[0] * k_int, c[1] * k_int, c[2] * k_int, c[3]];
        let c0 = glow(self.a4(&ld.color, t));
        let c1 = ld.color_end.as_ref().map(|c| glow(self.a4(c, t))).unwrap_or(c0);
        let op = self.a1(&ld.opacity, t) * opacity;
        let tail = self.a1(&ld.tail, t).clamp(0.0, 1.0);
        let dash_offset = self.a1(&ld.dash_offset, t);
        let mut segs = Vec::new();
        // The comet fade: alpha over the drawn window [s, e], full at the head.
        let comet = |u: f32| if tail > 0.0 { (((u - s) / (e - s).max(1e-6)) / tail).clamp(0.0, 1.0) } else { 1.0 };
        let push_seg = |segs: &mut Vec<Segment>, a: [f32; 3], b: [f32; 3], ca: [f32; 4], cb: [f32; 4], da: f32, db: f32| {
            segs.push(Segment {
                a,
                b,
                width_a: width,
                width_b: width,
                color_a: [ca[0], ca[1], ca[2], ca[3] * op],
                color_b: [cb[0], cb[1], cb[2], cb[3] * op],
                dist_a: da,
                dist_b: db,
                birth_a: f32::MIN,
                birth_b: f32::MIN,
            });
        };
        if !matches!(&ld.curve, makepad_geom3d::Curve3::Polyline(p) if p.len() < 2) {
            let arc = ld.curve.arc();
            let n = ((ld.segments as f32 * (e - s)).ceil() as usize).max(1);
            segs.reserve(n);
            let mut prev = transform_point(world, arc.point_at(s));
            let mut prev_d = s * arc.length();
            let mut prev_c = lerp4(c0, c1, s);
            prev_c[3] *= comet(s);
            for k in 1..=n {
                let u = s + (e - s) * k as f32 / n as f32;
                let p = transform_point(world, arc.point_at(u));
                let mut c = lerp4(c0, c1, u);
                c[3] *= comet(u);
                let d = u * arc.length();
                push_seg(&mut segs, prev, p, prev_c, c, prev_d, d);
                prev = p;
                prev_d = d;
                prev_c = c;
            }
        }
        // Polylines: each trimmed by the same window over its own length.
        for pl in &ld.polylines {
            let pts: Vec<[f32; 3]> = pl.iter().map(|p| transform_point(world, *p)).collect();
            let lens: Vec<f32> = pts.windows(2).map(|w| len3(sub3(w[1], w[0]))).collect();
            let total: f32 = lens.iter().sum::<f32>().max(1e-6);
            let (a, b) = (s * total, e * total);
            let mut run = 0.0;
            for (i, w) in pts.windows(2).enumerate() {
                let (d0, d1) = (run, run + lens[i]);
                run = d1;
                if d1 <= a || d0 >= b || lens[i] <= 0.0 {
                    continue;
                }
                let f0 = ((a - d0) / lens[i]).clamp(0.0, 1.0);
                let f1 = ((b - d0) / lens[i]).clamp(0.0, 1.0);
                let lerp = |f: f32| add3(w[0], scale3(sub3(w[1], w[0]), f));
                let (u0, u1) = ((d0 + f0 * lens[i]) / total, (d0 + f1 * lens[i]) / total);
                let (mut ca, mut cb) = (lerp4(c0, c1, u0), lerp4(c0, c1, u1));
                ca[3] *= comet(u0);
                cb[3] *= comet(u1);
                push_seg(&mut segs, lerp(f0), lerp(f1), ca, cb, u0 * total, u1 * total);
            }
        }
        // A kernel's segments (a line batch): each its own ends, colour and
        // width, in this line's colour, opacity and width.
        if let Some(run) = &ld.source {
            let params: Vec<(String, f32)> = run.params.iter().map(|(n, a)| (n.clone(), self.a1(a, t))).collect();
            let physics: Vec<(String, Arc<Vec<f32>>, usize)> = run
                .physics
                .iter()
                .map(|(name, what)| {
                    let (data, n) = self.phys.as_deref().map(|p| p.input(what, t)).unwrap_or_else(|| (Arc::new(vec![0.0; 16]), 0));
                    (name.clone(), data, n)
                })
                .collect();
            let frame_data = self.frame_data(&run.frame_data, t);
            match run.run(t, &params, &physics, &frame_data, run.active.as_ref().map(|a| self.a1(a, t).max(0.0) as usize)) {
                Ok(data) => {
                    let w = run.outputs[run.buffer].width;
                    if w == 11 {
                        let gather = |records: &[f32], out: &mut Vec<Segment>| {
                            out.reserve(records.len() / 11);
                            for rec in records.chunks_exact(11) {
                                let c = [rec[6] * c0[0], rec[7] * c0[1], rec[8] * c0[2], rec[9] * c0[3] * op];
                                if c[3] <= 0.0 || rec[10] <= 0.0 {
                                    continue;
                                }
                                let (a, b) = (transform_point(world, [rec[0], rec[1], rec[2]]), transform_point(world, [rec[3], rec[4], rec[5]]));
                                out.push(Segment::new(a, b, width * rec[10], c));
                            }
                        };
                        // A large batch (a smear of tens of thousands of
                        // segments) is gathered in pieces on the kernel
                        // workers, joined in order: the same segments.
                        const PIECE: usize = 8192;
                        let records = data.len() / 11;
                        if records >= 2 * PIECE {
                            let pieces = records.div_ceil(PIECE);
                            let outs: Vec<std::sync::Mutex<Vec<Segment>>> = (0..pieces).map(|_| std::sync::Mutex::new(Vec::new())).collect();
                            use makepad_script_compute_sched::sched::Executor;
                            makepad_script_compute_sched::sched::ThreadExecutor::shared().fan_out(pieces, &|k| {
                                let end = ((k + 1) * PIECE).min(records);
                                gather(&data[k * PIECE * 11..end * 11], &mut outs[k].lock().unwrap());
                            });
                            segs.reserve(outs.iter().map(|o| o.lock().unwrap().len()).sum());
                            for out in outs {
                                segs.extend_from_slice(&out.into_inner().unwrap());
                            }
                        } else {
                            gather(&data, &mut segs);
                        }
                    } else {
                        self.frame.errors.push(format!("Line3D source `{}`: records are not Segment (a, b, color, width)", run.name));
                    }
                }
                Err(e) => self.frame.errors.push(e),
            }
        }
        if segs.is_empty() {
            return;
        }
        let fade = ld.fade.unwrap_or((0.0, 0.0));
        self.frame.lines.push(LineDraw { segments: segs, world_width: ld.world_width, dash: ld.dash, gap: ld.gap, dash_offset, blending: ld.blending, fade });
    }

    fn light(&mut self, l: &LightDef, world: &Mat4f) {
        let t = self.t;
        self.lights_seen += 1;
        let c = self.a4(&l.color, t);
        let i = self.a1(&l.intensity, t).max(0.0);
        let color = [c[0] * i, c[1] * i, c[2] * i];
        let pos = mat_translation(world);
        match l.kind {
            LightKind::Ambient => {
                self.frame.ambient = add3(self.frame.ambient, color);
            }
            LightKind::Hemisphere => {
                let g = self.a4(&l.ground, t);
                self.frame.hemisphere = Some((color, [g[0] * i, g[1] * i, g[2] * i]));
            }
            LightKind::Directional | LightKind::Point | LightKind::Spot => {
                let direction = match &l.direction {
                    Some(d) => normalize_or_up(transform_dir(world, self.a3(d, t))),
                    None => {
                        let target = self.a3(&l.target, t);
                        let d = sub3(target, pos);
                        if dot3(d, d) < 1e-12 { [0.0, -1.0, 0.0] } else { normalize_or_up(d) }
                    }
                };
                let angle = self.a1(&l.angle, t).clamp(0.5, 89.0);
                let penumbra = self.a1(&l.penumbra, t).clamp(0.0, 1.0);
                let cos_outer = (angle * DEG).cos();
                let cos_inner = (angle * (1.0 - penumbra) * DEG).cos();
                let range = self.a1(&l.distance, t).max(0.0);
                let kind = match l.kind {
                    LightKind::Directional => LightType::Directional,
                    LightKind::Point => LightType::Point,
                    _ => LightType::Spot,
                };
                // The shadow's matrices are made once the scene bounds are known.
                let shadow = l.shadow.as_ref().map(|s| {
                    let softness = self.a1(&s.softness, t).max(0.0);
                    ShadowFrame { size: s.size, bias: s.bias, normal_bias: s.normal_bias, softness, view_proj: Mat4f::identity(), view: Mat4f::identity(), range: s.extent, texel_world: 0.0 }
                });
                self.frame.lights.push(LightFrame { kind, color, position: pos, direction, range, decay: l.decay, cos_outer, cos_inner, shadow });
            }
        }
    }

    fn camera(&mut self, c: &CameraDef) {
        let t = self.t;
        let mut target = match &c.aim {
            CameraAim::Target(a) => self.a3(a, t),
            CameraAim::Node(name) => self.named.get(name).copied().unwrap_or([0.0; 3]),
        };
        let mut position = self.a3(&c.position, t);
        if let Some((yaw, pitch, dist)) = &c.orbit {
            let (y, p, d) = (self.a1(yaw, t) * DEG, self.a1(pitch, t).clamp(-89.0, 89.0) * DEG, self.a1(dist, t).max(1e-3));
            position = [target[0] + d * p.cos() * y.sin(), target[1] + d * p.sin(), target[2] + d * p.cos() * y.cos()];
        }
        if let Some((curve, along, aim_along)) = &c.path {
            let u = self.a1(along, t).clamp(0.0, 1.0);
            let arc = curve.arc();
            position = arc.point_at(u);
            if *aim_along {
                target = add3(position, arc.tangent_at(u));
            }
        }
        // Named moves, each from where the previous one left the camera.
        let mut fov_scale = 1.0f32;
        let mut ortho_scale = 1.0f32;
        let mut roll_add = 0.0f32;
        let mut fov_override = None;
        if !c.shots.is_empty() {
            let s = fly_shots(&c.shots, t);
            position = s.position;
            target = s.target;
            roll_add += s.roll;
            if s.fov > 0.0 {
                fov_override = Some(s.fov);
            }
        }
        for mv in &c.moves {
            let span = (mv.end - mv.start).max(1e-4);
            let p = mv.ease.apply(((t - mv.start) / span).clamp(0.0, 1.0) as f64, span as f64) as f32;
            let offset = sub3(position, target);
            let dist = len3(offset).max(1e-4);
            let fwd = scale3(offset, -1.0 / dist);
            let right = normalize_or_up(cross3(fwd, [0.0, 1.0, 0.0]));
            let a = mv.amount * p;
            match mv.kind {
                MoveKind::PushIn if c.orthographic => ortho_scale *= (1.0 - a).max(0.05),
                MoveKind::PullOut if c.orthographic => ortho_scale *= 1.0 + a,
                MoveKind::PushIn => position = add3(position, scale3(fwd, dist * a)),
                MoveKind::PullOut => position = sub3(position, scale3(fwd, dist * a)),
                MoveKind::TruckLeft | MoveKind::TruckRight => {
                    let s = if mv.kind == MoveKind::TruckLeft { -1.0 } else { 1.0 };
                    let d = scale3(right, s * dist * a);
                    position = add3(position, d);
                    target = add3(target, d);
                }
                MoveKind::OrbitLeft | MoveKind::OrbitRight => {
                    let s = if mv.kind == MoveKind::OrbitLeft { -1.0 } else { 1.0 };
                    let q = quat_axis_angle([0.0, 1.0, 0.0], s * a * DEG);
                    position = add3(target, transform_dir(&quat_to_mat4(q), offset));
                }
                MoveKind::CraneUp | MoveKind::CraneDown => {
                    let s = if mv.kind == MoveKind::CraneUp { 1.0 } else { -1.0 };
                    position[1] += s * dist * a;
                }
                MoveKind::Rise => {
                    position[1] += dist * a;
                    target[1] += dist * a;
                }
                MoveKind::DollyZoom => {
                    // Push in while widening: the subject keeps its size.
                    let k = (1.0 - a).max(0.05);
                    position = add3(target, scale3(offset, k));
                    fov_scale *= 1.0 / k;
                }
                MoveKind::Roll => roll_add += a,
                MoveKind::Dive => {
                    // Toward overhead and into the target, widening like a whip.
                    let dir = normalize_or_up(add3(scale3(offset, (1.0 - a.min(1.0) * 0.8) / dist), [0.0, a.min(1.0) * 0.8, 0.0]));
                    position = add3(target, scale3(dir, dist * (1.0 - a * 0.985).max(0.004)));
                    fov_scale *= 1.0 + 0.35 * a.min(1.0);
                }
            }
        }
        if let Some((amount, freq, seed)) = &c.shake {
            let a = self.a1(amount, t);
            if a != 0.0 {
                let f = *freq;
                let off = [noise1(*seed, t * f) * a, noise1(seed + 1, t * f) * a, noise1(seed + 2, t * f) * a * 0.5];
                position = add3(position, off);
                target = add3(target, scale3(off, 0.5));
            }
        }
        let up = normalize_or_up(self.a3(&c.up, t));
        let mut view = Mat4f::look_at(vec3(position[0], position[1], position[2]), vec3(target[0], target[1], target[2]), vec3(up[0], up[1], up[2]));
        let roll = self.a1(&c.roll, t) + roll_add;
        if roll != 0.0 {
            view = mul(&quat_to_mat4(quat_axis_angle([0.0, 0.0, 1.0], roll * DEG)), &view);
        }
        let zoom = self.a1(&c.zoom, t).max(1e-3);
        let base_fov = match &c.focal_length {
            // A lens: the vertical field of view of `focal_length` mm on the sensor.
            Some(f) => 2.0 * (c.sensor * 0.5 / self.a1(f, t).max(1.0)).atan() / DEG,
            None => fov_override.unwrap_or_else(|| self.a1(&c.fov, t)),
        };
        let fov = (base_fov.clamp(1.0, 170.0) * DEG * 0.5).tan() * fov_scale.min(20.0) / zoom;
        let fov_y = 2.0 * fov.atan() / DEG;
        let aspect = self.frame.aspect.max(1e-4);
        // Orthographic cameras push and pull by framing, not distance.
        let height = self.a1(&c.height, t) / zoom * ortho_scale;
        let proj = if c.orthographic {
            ortho_gl(height * aspect * 0.5, height * 0.5, c.near, c.far)
        } else {
            Mat4f::perspective(fov_y, aspect, c.near, c.far)
        };
        self.frame.camera = CameraFrame {
            view,
            proj,
            position,
            forward: normalize_or_up(sub3(target, position)),
            near: c.near,
            far: c.far,
            fov_y,
            orthographic: c.orthographic,
            height,
            aspect,
            sensor: c.sensor,
        };
    }

    /// Reflector plane, contact shadows and the colour grade.
    fn finish(&mut self, p: &PostDef) {
        let t = self.t;
        self.frame.reflector = self.frame.meshes.iter().find(|m| m.material.reflectivity > 0.0 && m.ground).map(|m| m.bounds.0[1]);
        if let Some(g) = &p.grade {
            let v4 = |w: &mut Self, a: &Anim<4>| { let v = w.a4(a, t); [v[0], v[1], v[2]] };
            self.frame.grade = Some(GradeFrame {
                contrast: self.a1(&g.contrast, t),
                saturation: self.a1(&g.saturation, t),
                temperature: self.a1(&g.temperature, t),
                tint: self.a1(&g.tint, t),
                lift: v4(self, &g.lift),
                gamma: v4(self, &g.gamma),
                gain: v4(self, &g.gain),
            });
        }
        if let Some(c) = &p.contact {
            let mut casters: Option<([f32; 3], [f32; 3])> = None;
            for m in self.frame.meshes.iter().filter(|m| m.cast_shadow && !m.ground && !m.material.is_blended()) {
                casters = Some(match casters {
                    None => m.bounds,
                    Some(x) => union(x, m.bounds),
                });
            }
            let ground_y = self.frame.meshes.iter().filter(|m| m.ground).map(|m| m.bounds.0[1]).fold(f32::MAX, f32::min);
            if let (Some((mn, mx)), true) = (casters, ground_y < f32::MAX) {
                let blur = self.a1(&c.blur, t).max(0.0);
                let extent = ((mx[0] - mn[0]).max(mx[2] - mn[2]) * 0.5 + blur * 2.0 + 0.05).max(0.05);
                let (cxp, czp) = ((mn[0] + mx[0]) * 0.5, (mn[2] + mx[2]) * 0.5);
                // From just under the ground, looking up.
                let eye = [cxp, ground_y - 0.02, czp];
                let range = c.far + 0.04;
                let view = Mat4f::look_at(vec3(eye[0], eye[1], eye[2]), vec3(eye[0], eye[1] + 1.0, eye[2]), vec3(0.0, 0.0, -1.0));
                let proj = ortho_gl(extent, extent, 0.01, range);
                self.frame.contact = Some(ContactFrame {
                    opacity: self.a1(&c.opacity, t).clamp(0.0, 1.0),
                    blur: blur / (2.0 * extent),
                    far: c.far / range,
                    view,
                    view_proj: mul(&proj, &view),
                    size: c.size,
                });
            }
        }
    }

    fn post(&mut self, p: &PostDef) {
        let t = self.t;
        let mut post = PostFrame { antialias: p.antialias, ..PostFrame::default() };
        if let Some((s, r, th)) = &p.bloom {
            post.bloom = Some((self.a1(s, t).max(0.0), self.a1(r, t).clamp(0.0, 1.0), self.a1(th, t).max(0.0)));
        }
        if let Some((r, i)) = &p.ssao {
            post.ssao = Some((self.a1(r, t).max(1e-3), self.a1(i, t).max(0.0)));
        }
        if let Some((focus, node, aperture, max_blur)) = &p.dof {
            let f = match node.as_ref().and_then(|n| self.named.get(n).copied()) {
                Some(pos) => len3(sub3(pos, self.frame.camera.position)),
                None => self.a1(focus, t),
            };
            let mut ap = self.a1(aperture, t).max(0.0);
            if let Some(n) = &p.f_stop {
                // Thin lens: the blur disc at infinity is f² / (N (S − f))
                // on the sensor, in pixels of a 1080-line frame.
                let fl = self.frame.camera.sensor * 0.5 / (self.frame.camera.fov_y * DEG * 0.5).tan();
                let s_mm = f.max(1e-3) * 1000.0;
                let coc_mm = fl * fl / (self.a1(n, t).max(0.5) * (s_mm - fl).max(1.0));
                ap = coc_mm / self.frame.camera.sensor * 1080.0;
            }
            post.dof = Some((f.max(1e-3), ap, *max_blur));
        }
        if let Some((c, th)) = &p.outline {
            post.outline = Some((self.a4(c, t), self.a1(th, t).max(0.0)));
        }
        if let Some((a, s)) = &p.vignette {
            post.vignette = Some((self.a1(a, t), *s));
        }
        if let Some(g) = &p.grain {
            post.grain = Some(self.a1(g, t));
        }
        if let Some(sh) = &p.shafts {
            let pos = match &sh.node {
                Some(n) => self.named.get(n).copied().unwrap_or([0.0; 3]),
                None => self.a3(&sh.position, t),
            };
            let vp = mul(&self.frame.camera.proj, &self.frame.camera.view);
            let v = vp.transform_vec4(vec4(pos[0], pos[1], pos[2], 1.0));
            let (strength, c) = (self.a1(&sh.strength, t), self.a4(&sh.color, t));
            let front = v.w > 1e-4;
            let uv = if front { [v.x / v.w * 0.5 + 0.5, v.y / v.w * 0.5 + 0.5] } else { [0.5, 0.5] };
            post.shafts = Some((uv, if front { strength } else { 0.0 }, [c[0], c[1], c[2]], sh.decay, sh.threshold));
        }
        if !p.passes.is_empty() {
            let mut u = [0.0f32; 16];
            for (k, pass) in p.passes.iter().take(4).enumerate() {
                for (j, a) in pass.uniforms.iter().take(4).enumerate() {
                    u[k * 4 + j] = self.a1(a, t);
                }
            }
            let codes: Vec<makepad_render_graph::pass::PassFn> = p.passes.iter().take(4).map(|x| x.code.clone()).collect();
            let key = makepad_geom3d::math::hash64(codes.iter().fold(0x9057u64, |h, c| h.wrapping_mul(0x100_0000_01b3) ^ c.image.hash()));
            post.passes = Some((key, codes, u, t));
        }
        if let Some(z) = &p.zoom_blur {
            post.zoom_blur = Some(self.a1(z, t).clamp(0.0, 0.95));
        }
        if let Some((shutter, samples, tol)) = &p.motion_blur {
            let shutter = self.a1(shutter, t).clamp(0.0, 720.0);
            self.frame.motion_blur = Some((shutter, *samples, *tol));
            if let Some(decl) = &p.velocity_blur {
                post.velocity_blur = Some((decl.clone(), shutter / 360.0));
            }
        }
        if !p.graph.is_empty() {
            let values = p
                .graph
                .iter()
                .map(|g| {
                    g.uniforms
                        .iter()
                        .map(|u| match u {
                            GraphUniform::One(a) => [self.a1(a, t), 0.0, 0.0, 0.0].into(),
                            GraphUniform::Many(a) => self.a4(a, t).into(),
                        })
                        .collect()
                })
                .collect();
            post.graph = Some((p.graph_decls.clone(), values));
        }
        if let Some((a, h, cell, c)) = &p.paper {
            post.paper = Some((self.a1(a, t).clamp(0.0, 1.0), self.a1(h, t).clamp(0.0, 1.0), *cell, self.a4(c, t)));
        }
        self.frame.post = post;
    }

    /// Shadow matrices, fitted to the casters' bounds.
    /// Shadow matrices. A directional light's map covers the shadow
    /// CASTERS tightly (flat meshes — floors, walls — are receivers only,
    /// so a big floor does not spread the map thin), and its depth range
    /// reaches every receiver.
    fn shadows(&mut self, _root: &NodeDef) {
        let Some(all) = self.frame.bounds else { return };
        let is_flat = |(a, b): ([f32; 3], [f32; 3])| {
            let d = sub3(b, a);
            let big = d[0].max(d[1]).max(d[2]);
            d[0].min(d[1]).min(d[2]) < big * 0.02
        };
        let mut casters: Option<([f32; 3], [f32; 3])> = None;
        for m in self.frame.meshes.iter().filter(|m| m.cast_shadow && !is_flat(m.bounds)) {
            casters = Some(match casters {
                None => m.bounds,
                Some(c) => union(c, m.bounds),
            });
        }
        let (mn, mx) = casters.unwrap_or(all);
        let center = scale3(add3(mn, mx), 0.5);
        let radius = (len3(sub3(mx, mn)) * 0.5).max(1e-3);
        let corners: Vec<[f32; 3]> = (0..8).map(|k| [if k & 1 == 0 { all.0[0] } else { all.1[0] }, if k & 2 == 0 { all.0[1] } else { all.1[1] }, if k & 4 == 0 { all.0[2] } else { all.1[2] }]).collect();
        for l in &mut self.frame.lights {
            let Some(s) = &mut l.shadow else { continue };
            let up = if l.direction[1].abs() > 0.99 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            match l.kind {
                LightType::Directional => {
                    let extent = if s.range > 0.0 { s.range } else { radius * 1.05 };
                    let eye = sub3(center, scale3(l.direction, extent * 2.0));
                    // Far enough for every receiver in the scene.
                    let far = corners.iter().map(|c| dot3(sub3(*c, eye), l.direction)).fold(extent * 4.0, f32::max) + 0.01;
                    let view = Mat4f::look_at(vec3(eye[0], eye[1], eye[2]), vec3(center[0], center[1], center[2]), vec3(up[0], up[1], up[2]));
                    let proj = ortho_gl(extent, extent, 0.01, far);
                    s.view = view;
                    s.view_proj = mul(&proj, &view);
                    s.range = far;
                    s.texel_world = 2.0 * extent / s.size as f32;
                    // Depth bias: `bias` is in world units here, as a
                    // fraction of the map's depth range for the shader.
                    s.bias = (s.bias.max(0.0) * 4.0 + s.texel_world * 1.5) / far;
                }
                LightType::Spot => {
                    let dist_to_center = len3(sub3(center, l.position));
                    let far_all = corners.iter().map(|c| len3(sub3(*c, l.position))).fold(0.0f32, f32::max);
                    let far = if l.range > 0.0 { l.range } else { far_all.max(dist_to_center + radius) };
                    let fov = (l.cos_outer.clamp(-1.0, 1.0).acos() / DEG * 2.0 + 4.0).min(170.0);
                    let tgt = add3(l.position, l.direction);
                    let view = Mat4f::look_at(vec3(l.position[0], l.position[1], l.position[2]), vec3(tgt[0], tgt[1], tgt[2]), vec3(up[0], up[1], up[2]));
                    let proj = Mat4f::perspective(fov, 1.0, 0.05, far.max(0.1));
                    s.view = view;
                    s.view_proj = mul(&proj, &view);
                    s.range = far.max(0.1);
                    s.texel_world = 2.0 * dist_to_center * (fov * 0.5 * DEG).tan() / s.size as f32;
                    s.bias = (s.bias.max(0.0) * 4.0 + s.texel_world * 1.5) / s.range;
                }
                LightType::Point => {
                    // A cube atlas (3 x 2 faces); depth is distance / range.
                    s.size = s.size.min(1024);
                    let far_all = corners.iter().map(|c| len3(sub3(*c, l.position))).fold(0.1f32, f32::max);
                    let range = if l.range > 0.0 { l.range } else { far_all };
                    s.range = range;
                    s.view = translation([-l.position[0], -l.position[1], -l.position[2]]);
                    s.view_proj = s.view;
                    s.texel_world = 2.0 * range / s.size as f32 * 0.25;
                    s.bias = (s.bias.max(0.0) * 4.0 + s.texel_world * 1.5) / range;
                }
            }
        }
    }
}

fn lerp4(a: [f32; 4], b: [f32; 4], f: f32) -> [f32; 4] {
    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * f)
}

/// Orthographic projection, symmetric, with depth mapped to 0..1 (near..
/// far): Metal clips clip-space z below 0, so a GL-style -1..1 mapping would
/// lose the nearer half of the range there; 0..1 is inside every backend's
/// clip volume.
pub fn ortho_gl(half_w: f32, half_h: f32, near: f32, far: f32) -> Mat4f {
    let mut m = Mat4f::identity();
    m.v[0] = 1.0 / half_w.max(1e-6);
    m.v[5] = 1.0 / half_h.max(1e-6);
    m.v[10] = -1.0 / (far - near);
    m.v[14] = -near / (far - near);
    m
}

fn apply_override(base: &mut MaterialDef, o: &MaterialDef, set: &[String]) {
    for f in set {
        match f.as_str() {
            "preset" => {
                // A preset rewrites the whole look (then the fields written
                // after it, handled by their own names, override it).
                let (name, maps, uniforms, shader) = (base.name.clone(), base.maps.clone(), base.uniforms.clone(), base.shader.clone());
                *base = MaterialDef { name, maps, uniforms, shader, ..o.clone() };
            }
            "flakes" => base.flakes = o.flakes.clone(),
            "brushed" => base.brushed = o.brushed.clone(),
            "reflectivity" => base.reflectivity = o.reflectivity.clone(),
            "reflection_blur" => base.reflection_blur = o.reflection_blur.clone(),
            "color" | "base_color" => base.color = o.color.clone(),
            "opacity" => base.opacity = o.opacity.clone(),
            "metalness" | "metallic" => base.metalness = o.metalness.clone(),
            "roughness" => base.roughness = o.roughness.clone(),
            "emissive" => base.emissive = o.emissive.clone(),
            "emissive_intensity" => base.emissive_intensity = o.emissive_intensity.clone(),
            "clearcoat" => base.clearcoat = o.clearcoat.clone(),
            "clearcoat_roughness" => base.clearcoat_roughness = o.clearcoat_roughness.clone(),
            "transmission" => base.transmission = o.transmission.clone(),
            "ior" => base.ior = o.ior.clone(),
            "thickness" => base.thickness = o.thickness.clone(),
            "attenuation_color" => base.attenuation_color = o.attenuation_color.clone(),
            "sheen" => base.sheen = o.sheen.clone(),
            "sheen_color" => base.sheen_color = o.sheen_color.clone(),
            "iridescence" => base.iridescence = o.iridescence.clone(),
            "specular_intensity" => base.specular_intensity = o.specular_intensity.clone(),
            "env_intensity" | "env_map_intensity" => base.env_intensity = o.env_intensity.clone(),
            "rim_color" => base.rim_color = o.rim_color.clone(),
            "rim_power" => base.rim_power = o.rim_power.clone(),
            _ => {}
        }
    }
    // Flags and the model are copied as the override wrote them.
    base.wireframe |= o.wireframe;
    base.flat_shading |= o.flat_shading;
    if o.model != ShadingModel::Standard {
        base.model = o.model;
    }
}

/// A heightfield's lift at (x, z) and time t, in world units.
pub(crate) fn height_at(hf: &crate::scene::HeightfieldDef, amount: f32, x: f32, z: f32, t: f32) -> f32 {
    use crate::scene::HeightKind;
    let tau = std::f32::consts::TAU;
    let k = tau / hf.cell[0].max(1e-3);
    let wave = || 0.5 + 0.5 * (x * k + t * hf.speed * tau).sin() * (z * k * 0.8 - t * hf.speed * 1.3 * tau).cos();
    let blocks = || {
        // Which block this is, and the pattern of the current and last beat.
        let (bx, bz) = ((x / hf.cell[0]).floor() as i64, (z / hf.cell[1]).floor() as i64);
        let beat = (t / hf.every).floor();
        let u = ((t / hf.every) - beat).clamp(0.0, 1.0);
        let target = |b: f32| {
            let h = crate::layout::instance_rand(hf.seed ^ (b as i64 as u64).wrapping_mul(0x9E37_79B9), ((bx.wrapping_mul(73_856_093)) ^ (bz.wrapping_mul(19_349_663))) as u64);
            if h[0] > 0.62 { 0.35 + 0.65 * h[1] } else if h[0] < 0.18 { -0.45 * h[1] } else { 0.0 }
        };
        // A snappy settle into the new pattern at the start of each beat.
        let e = 1.0 - (1.0 - (u * 4.0).min(1.0)).powi(3);
        target(beat - 1.0) + (target(beat) - target(beat - 1.0)) * e
    };
    amount
        * match hf.kind {
            HeightKind::Wave => wave(),
            HeightKind::Ripple => 0.5 + 0.5 * ((x * x + z * z).sqrt() * k - t * hf.speed * tau).sin(),
            HeightKind::Blocks => blocks(),
            HeightKind::BlocksWave => blocks() + 0.25 * wave(),
            HeightKind::Noise => makepad_geom3d::math::fbm3(hf.seed, [x / hf.cell[0], z / hf.cell[0], t * hf.speed], 3, 0.5) * 0.5 + 0.5,
        }
}

/// The camera between shots: a Catmull-Rom flight through every shot's
/// position and target, eased between arrivals, holding at each for `hold`.
pub(crate) fn fly_shots(shots: &[crate::scene::ShotDef], t: f32) -> crate::scene::ShotDef {
    let n = shots.len();
    let first = &shots[0];
    if n == 1 || t <= first.at + first.hold {
        return first.clone();
    }
    let last = &shots[n - 1];
    if t >= last.at {
        return last.clone();
    }
    let i = (0..n - 1).find(|&i| t < shots[i + 1].at).unwrap_or(n - 2);
    let (a, b) = (&shots[i], &shots[i + 1]);
    let leave = a.at + a.hold;
    if t <= leave {
        return a.clone();
    }
    let u = ((t - leave) / (b.at - leave).max(1e-4)).clamp(0.0, 1.0);
    let u = u * u * (3.0 - 2.0 * u);
    let p = |k: isize| &shots[(i as isize + k).clamp(0, n as isize - 1) as usize];
    let cr = |f: &dyn Fn(&crate::scene::ShotDef) -> [f32; 3]| {
        let (p0, p1, p2, p3) = (f(p(-1)), f(p(0)), f(p(1)), f(p(2)));
        let mut o = [0.0; 3];
        for k in 0..3 {
            let (a0, a1, a2, a3) = (p0[k], p1[k], p2[k], p3[k]);
            o[k] = 0.5 * ((2.0 * a1) + (-a0 + a2) * u + (2.0 * a0 - 5.0 * a1 + 4.0 * a2 - a3) * u * u + (-a0 + 3.0 * a1 - 3.0 * a2 + a3) * u * u * u);
        }
        o
    };
    let fov = if a.fov > 0.0 && b.fov > 0.0 { a.fov + (b.fov - a.fov) * u } else if b.fov > 0.0 { b.fov } else { a.fov };
    crate::scene::ShotDef { at: t, hold: 0.0, position: cr(&|s| s.position), target: cr(&|s| s.target), roll: a.roll + (b.roll - a.roll) * u, fov }
}

impl Walk<'_, '_> {
    /// Named nodes projected to the frame (for 2D HUD boxes and leader
    /// lines): x, y in 0..1 from the top left, depth, in front of the camera.
    fn anchors(&mut self) {
        let vp = mul(&self.frame.camera.proj, &self.frame.camera.view);
        let mut out: Vec<crate::frame::Anchor> = self
            .named
            .iter()
            .map(|(name, p)| {
                let v = vp.transform_vec4(vec4(p[0], p[1], p[2], 1.0));
                let front = v.w > 1e-4;
                let (x, y) = if front { (v.x / v.w, v.y / v.w) } else { (0.0, 0.0) };
                crate::frame::Anchor { name: name.clone(), world: *p, screen: [x * 0.5 + 0.5, 0.5 - y * 0.5], depth: v.w, in_front: front }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        self.frame.anchors = out;
    }
}
