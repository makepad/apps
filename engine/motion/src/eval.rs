//! `MotionDoc::eval(t)` and `eval_live(t, live)`: the document at one moment,
//! as a pure function of its clocks, inputs and context. Keys are sampled,
//! staggers shift unit and child clocks, voice-bound text takes its words' states, and property
//! functions run in the document's own VM (the one its load evaluated it
//! in; the VM sees only the supplied clocks, never wall time or a random source).

use crate::plain::{PrimitiveFrame, TextProps};
use crate::features::{VoiceFeatures, WordState};
use crate::frame::*;
use crate::model::*;
use crate::value::Evaluated;
use crate::{Ease, LoadedLayers};

/// Character layers (lane CH1).
mod character;
mod rig;
use makepad_script::*;
use std::sync::Arc;

use makepad_motion_plain::sample::{FnOut, PropertyCalls, Sampler, NEEDS_VM};

/// Operations that need the VM of a loaded Motion document.
pub(crate) trait Calls: PropertyCalls {
    fn call_draw2d(&self, index: usize, local: f64, edit: f64) -> Result<(crate::draw2d::Draw2dFrame, Vec<String>), String>;
    fn scene_source(&self, scene: &SceneSource) -> Result<SceneSource, String>;
}

struct NoCalls;

impl PropertyCalls for NoCalls {
    fn call_fn(&self, _: usize, _: f64, _: usize, _: f64) -> Result<FnOut, String> {
        Err(NEEDS_VM.into())
    }

}

impl Calls for NoCalls {
    fn call_draw2d(&self, _: usize, _: f64, _: f64) -> Result<(crate::draw2d::Draw2dFrame, Vec<String>), String> {
        Err(NEEDS_VM.into())
    }

    fn scene_source(&self, _: &SceneSource) -> Result<SceneSource, String> {
        Err(NEEDS_VM.into())
    }
}

impl PropertyCalls for MotionDoc {
    /// Call property function `index` as `fn(local, i, edit)`: the layer's
    /// clock, the unit's index and the host's time.
    fn call_fn(&self, index: usize, t: f64, i: usize, edit: f64) -> Result<FnOut, String> {
        self.with_evaluated(|evaluated| {
            let Some(&function) = self.run.fns.get(index) else {
                return Err("the property function is missing from the document's evaluation".into());
            };
            let (errors, out) = evaluated.with_vm(|vm| {
                vm.capture_errors();
                let (value, _) = vm.eval_bounded(&crate::value::FRAME_LIMITS, |vm| vm.call(function, &[t.into(), (i as f64).into(), edit.into()]));
                let errors = vm.take_error_records();
                (errors, read_out(vm, value))
            });
            if let Some(error) = errors.first() {
                return Err(error.message().to_owned());
            }
            out
        })
    }

}

impl Calls for MotionDoc {
    /// A Draw2D layer's `draw` function at layer time `local`.
    fn call_draw2d(&self, index: usize, local: f64, edit: f64) -> Result<(crate::draw2d::Draw2dFrame, Vec<String>), String> {
        self.with_evaluated(|evaluated| {
            let Some(&function) = self.run.fns.get(index) else {
                return Err("the draw function is missing from the document's evaluation".into());
            };
            let (out, errors) = evaluated.with_vm(|vm| {
                vm.capture_errors();
                let (out, _) = vm.eval_bounded(&crate::value::DRAW_LIMITS, |vm| crate::draw2d::eval(vm, function, local, edit));
                (out, vm.take_error_records())
            });
            let (picture, mut messages) = out;
            for error in &errors {
                messages.push(error.message().to_owned());
            }
            Ok((picture, messages))
        })
    }

    fn scene_source(&self, scene: &SceneSource) -> Result<SceneSource, String> {
        Ok(match scene {
            SceneSource::Template { source, inputs } => {
                // The frame's instance values, followed by the layer's overrides.
                let held = self.vm.with(|e| e.host.host.inputs.clone());
                let inputs = self.inputs.iter().map(|decl| decl.name.clone())
                    .zip(held.iter().map(|(_, value)| value.clone())).chain(inputs.iter().cloned()).collect();
                SceneSource::Template { source: source.clone(), inputs }
            }
            #[cfg(feature = "scene3d")]
            SceneSource::Object(_) => scene.clone(),
        })
    }
}

impl MotionDoc {
    /// A post pass uniform given as `fn(t)` that returns a vector is a
    /// vector uniform: its width is read from one call at t = 0 (every
    /// frame calls it again; a failing call is reported by the frame).
    pub(crate) fn widen_fn_uniforms(&mut self) {
        let mut wide = Vec::new();
        for (pi, p) in self.post.iter().enumerate() {
            for (ui, u) in p.uniforms.iter().enumerate() {
                if let PostUniform::One(Prop::Fn(f)) = u {
                    if let Ok(FnOut::Nums(v)) = self.call_fn(f.index, 0.0, 0, 0.0) {
                        if (2..=4).contains(&v.len()) {
                            wide.push((pi, ui, v.len() as u8, f.clone()));
                        }
                    }
                }
            }
        }
        for (pi, ui, width, f) in wide {
            self.post[pi].uniforms[ui] = PostUniform::Four(Prop::Fn(f));
            if let Some(d) = self.post[pi].decl.uniforms.get_mut(ui) {
                d.width = width;
            }
        }
    }

    /// Run `f` with this document's VM.
    fn with_evaluated<R>(&self, f: impl FnOnce(&mut Evaluated) -> Result<R, String>) -> Result<R, String> {
        self.vm.with(f)
    }

    /// Collect the garbage frames left in this document's VM, once it has
    /// doubled (every frame allocates: without it a playing preview grew by
    /// gigabytes, and the heap's growth stalled a frame now and then).
    fn collect_garbage(&self) {
        self.vm.with(|evaluated| {
            evaluated.with_vm(|vm| {
                if vm.bx.heap_ref().needs_gc() {
                    vm.gc();
                }
            })
        });
    }

    /// The frame at `t` seconds of the document's clock, with the document's
    /// own input values.
    pub fn eval(&self, t: f64) -> MotionFrame {
        self.eval_live(t, t)
    }

    /// The story frame at `t`, with an independent clock for `live()`.
    /// The host supplies both clocks; evaluation never reads wall time.
    pub fn eval_live(&self, t: f64, live: f64) -> MotionFrame {
        let mut own = InstanceInputs { slots: self.own.clone() };
        self.eval_with_live(t, live, &mut own)
    }

    /// Whether the most recent frame read `live()`, including property,
    /// Draw2D and kernel parameter functions in the document's VM.
    pub fn reads_live(&self) -> bool {
        self.vm.with(|e| e.host.host.reads_live)
    }

    /// The frame at `t` of the instance whose inputs are `inputs`: they are
    /// the ones the document's property functions read for the frame (and
    /// are back in `inputs` after it), so any number of instances share this
    /// document and its VM. Nothing is evaluated again. (Scene3D closures run
    /// at draw and read the load values, not these.)
    pub fn eval_with(&self, t: f64, inputs: &mut InstanceInputs) -> MotionFrame {
        self.eval_with_live(t, t, inputs)
    }

    /// Instance inputs and an explicit ambient clock for this frame.
    pub fn eval_with_live(&self, t: f64, live: f64, inputs: &mut InstanceInputs) -> MotionFrame {
        let _turn = self.vm.turn();
        let _lent = LentInputs::new(&self.vm, &mut inputs.slots);
        self.vm.with(|e| e.new_frame(t, live));
        let mut ev = Evaluator::new(&self.plain, self, t);
        let mut data_layers = Vec::new();
        let items = ev.layers_at(t, Some(&mut data_layers));
        let camera = self.camera.as_ref().map(|camera| camera.iter().map(|(name, arg)| (name.clone(), ev.arg(arg, t))).collect());
        let post = (!self.post.is_empty()).then(|| ev.post_at(&self.post, t));
        let motion_blur = self.motion_blur.as_ref().map(|m| {
            let [shutter] = ev.sample(&m.shutter, t, 0);
            (shutter.clamp(0.0, 720.0), m.max, m.tol)
        });
        let mut frame = ev.into_frame(t, items);
        frame.camera = camera;
        frame.post = post;
        frame.motion_blur = motion_blur;
        frame.hdr = self.hdr;
        frame.data_layers = data_layers;
        self.collect_garbage();
        frame
    }
}

/// An instance's input values in the document's VM for the length of a
/// frame (swapped in, swapped back out).
struct LentInputs<'a> {
    vm: &'a crate::value::DocVm,
    slots: &'a mut Arc<Vec<(LiveId, InputValue)>>,
}

impl<'a> LentInputs<'a> {
    fn new(vm: &'a crate::value::DocVm, slots: &'a mut Arc<Vec<(LiveId, InputValue)>>) -> Self {
        vm.with(|e| std::mem::swap(&mut e.host.host.inputs, slots));
        Self { vm, slots }
    }
}

impl Drop for LentInputs<'_> {
    fn drop(&mut self) {
        self.vm.with(|e| std::mem::swap(&mut e.host.host.inputs, self.slots));
    }
}

fn read_out(vm: &ScriptVm, value: ScriptValue) -> Result<FnOut, String> {
    if let Some(n) = value.as_number() {
        return Ok(FnOut::Nums(vec![n]));
    }
    if let Some(b) = value.as_bool() {
        return Ok(FnOut::Nums(vec![if b { 1.0 } else { 0.0 }]));
    }
    if let Some(c) = value.as_color() {
        return Ok(FnOut::Color(crate::load::unpack(c)));
    }
    // A Splash vector (`p - size * 0.5`).
    if let Some(pod) = value.as_pod() {
        let (ty, data) = vm.bx.heap_ref().pod_data(pod);
        use makepad_script::pod::{ScriptPodVec};
        let lanes = match ty.ty().as_vec() {
            Some(ScriptPodVec::Vec2f) => 2,
            Some(ScriptPodVec::Vec3f) => 3,
            Some(ScriptPodVec::Vec4f) => 4,
            None if ty.ty().kind() == makepad_script::pod::ScriptPodTyTag::F32 => 1,
            _ => return Err("the property function returned a vector kind motion does not use".into()),
        };
        return Ok(FnOut::Nums(data.iter().take(lanes).map(|bits| f32::from_bits(*bits) as f64).collect()));
    }
    if let Some(array) = value.as_array() {
        let heap = vm.heap();
        let mut out = Vec::new();
        for index in 0..heap.array_len(array) {
            match heap.array_index(array, index, NoTrap).as_number() {
                Some(n) => out.push(n),
                None => return Err("a property function returned an array with something other than numbers".into()),
            }
        }
        return Ok(FnOut::Nums(out));
    }
    // An emissive colour, `emit(#ff5a1f, 4)`: `{color: [r g b a]}`, its
    // components over 1 kept (as a written `emit()` is).
    if let Some(object) = value.as_object() {
        let heap = vm.heap();
        if let Some(parts) = heap.value(object, id!(color).into(), NoTrap).as_array() {
            let lanes: Vec<f64> = (0..heap.array_len(parts)).filter_map(|i| heap.array_index(parts, i, NoTrap).as_number()).collect();
            if let [r, g, b, a] = lanes[..] {
                return Ok(FnOut::Color([r, g, b, a]));
            }
        }
    }
    if value.is_nil() {
        return Err("the property function returned nil; it returns a number, vec2 or colour".into());
    }
    // tween(…) / keys(…) are animation values, not numbers: inside a
    // function they come back as objects. Say what to write instead.
    if let Some(ty) = crate::obj::type_name(vm, value).filter(|ty| crate::value::ANIMS.contains(ty)) {
        let what = ty.to_lowercase();
        return Err(format!("the property function returned a {what}(…): {what} is a property value, not a number; write it as the property itself (`opacity: keys([2.8, 0], [3.4, 0.5], [10.5, 0.5], [11.5, 0])`), or compute inside the function: `ease(@ease_out_cubic, prog(t, 2.8, 3.4)) * 0.5`"));
    }
    Err(format!("the property function returned {:?}; it returns a number, vec2 or colour (or emit(colour, k))", value.value_type()))
}

struct Evaluator<'a> {
    doc: &'a LoadedLayers,
    calls: &'a dyn Calls,
    errors: Vec<Diagnostic>,
    /// The frame's time on the host's clock (`edit` in property functions).
    edit: f64,
    /// The copy of a `Repeat` being evaluated: property functions see it
    /// as their index `i`.
    instance: usize,
    /// The root layer being evaluated (a Freeze shows the ones before it).
    root: usize,
    /// Inside a Freeze: a freeze in what it shows draws nothing.
    freezing: bool,
    /// Groups' own post passes, as reached (their layer paths).
    group_post: Vec<(String, (std::sync::Arc<Vec<makepad_render_graph::PassDecl>>, Vec<makepad_render_graph::PassValues>))>,
}

pub(crate) fn eval_plain(doc: &LoadedLayers, t: f64) -> MotionFrame {
    let mut ev = Evaluator::new(doc, &NoCalls, t);
    let items = ev.layers_at(t, None);
    ev.into_frame(t, items)
}

impl<'a> Evaluator<'a> {
    fn new(doc: &'a LoadedLayers, calls: &'a dyn Calls, t: f64) -> Self {
        Self { doc, calls, errors: Vec::new(), edit: t + doc.context.offset, instance: 0, root: 0, freezing: false, group_post: Vec::new() }
    }

    fn layers_at(&mut self, t: f64, mut data_layers: Option<&mut Vec<(String, Vec<Item>)>>) -> Vec<Item> {
        let mut items = Vec::new();
        for (root, layer) in self.doc.layers.iter().enumerate() {
            self.root = root;
            if layer.data.is_some() && data_layers.is_none() {
                continue;
            }
            let item = self.layer(layer, t, 0.0, &Affine::IDENTITY, true);
            if layer.data.is_some() {
                if let (Some(data_layers), Some(name)) = (data_layers.as_mut(), &layer.name) {
                    data_layers.push((name.clone(), item.into_iter().collect()));
                }
            } else {
                items.extend(item);
            }
        }
        items
    }

    fn into_frame(self, t: f64, items: Vec<Item>) -> MotionFrame {
        MotionFrame {
            t, items, size: self.doc.size, background: self.doc.background, fps: self.doc.fps,
            camera: None, post: None, motion_blur: None, hdr: false, data_layers: Vec::new(),
            group_post: self.group_post, errors: self.errors,
        }
    }

    /// Post passes and their uniform values at `t`.
    fn post_at(&mut self, post: &[crate::model::PostPassSpec], t: f64) -> (std::sync::Arc<Vec<makepad_render_graph::PassDecl>>, Vec<makepad_render_graph::PassValues>) {
        let decls = std::sync::Arc::new(post.iter().map(|p| p.decl.clone()).collect::<Vec<_>>());
        let values = post.iter().map(|p| p.uniforms.iter().map(|u| match u {
            crate::model::PostUniform::One(prop) => {
                let [v] = self.sample(prop, t, 0);
                makepad_render_graph::UniformValue::from([v as f32, 0.0, 0.0, 0.0])
            }
            crate::model::PostUniform::Two(prop) => {
                let [x, y] = self.sample(prop, t, 0);
                [x as f32, y as f32, 0.0, 0.0].into()
            }
            crate::model::PostUniform::Three(prop) => {
                let [x, y, z] = self.sample(prop, t, 0);
                [x as f32, y as f32, z as f32, 0.0].into()
            }
            crate::model::PostUniform::Four(prop) => self.sample(prop, t, 0).map(|v| v as f32).into(),
        }).collect()).collect();
        (decls, values)
    }

    fn properties(&mut self) -> Sampler<'_> {
        Sampler { calls: Some(self.calls), errors: &mut self.errors, instance: self.instance, edit: self.edit }
    }

    fn sample<const N: usize>(&mut self, prop: &Prop<N>, t: f64, i: usize) -> [f64; N] {
        self.properties().sample(prop, t, i)
    }

    fn sample_kind<const N: usize>(&mut self, prop: &Prop<N>, t: f64, i: usize, kind: Option<usize>) -> [f64; N] {
        self.properties().sample_kind(prop, t, i, kind)
    }

    fn arg(&mut self, arg: &EffectArg, t: f64) -> EffectValue {
        match arg {
            EffectArg::Num(p) => EffectValue::Num(self.sample(p, t, 0)[0]),
            EffectArg::Vec2(p) => EffectValue::Vec2(self.sample(p, t, 0)),
            EffectArg::Vec3(p) => EffectValue::Vec3(self.sample(p, t, 0)),
            EffectArg::Color(p) => EffectValue::Color(self.sample(p, t, 0)),
            EffectArg::Bool(b) => EffectValue::Bool(*b),
            EffectArg::Choice(c) => EffectValue::Choice(c.clone()),
            EffectArg::Text(s) => EffectValue::Text(s.clone()),
            EffectArg::Points(p) => EffectValue::Points(p.clone()),
        }
    }

    /// A KineticText's words at host time `host`.
    fn kinetic(&self, source: &KineticSource, host: f64, local: f64) -> KineticFrame {
        match source {
            KineticSource::Text(text) => KineticFrame { text: text.clone(), next: String::new(), progress: 0.0, voice: false, since: local },
            KineticSource::Voice(id) => {
                let voice = self.doc.context.signals.voice(id).cloned().unwrap_or_default();
                let line = voice.line_at(host).or_else(|| voice.lines.iter().rposition(|l| l.0 <= host));
                let words = |index: usize| voice.words.iter().filter(|w| w.line as usize == index).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                match line {
                    Some(index) => {
                        let (start, end) = voice.lines[index];
                        KineticFrame {
                            text: words(index),
                            next: if index + 1 < voice.lines.len() { words(index + 1) } else { String::new() },
                            progress: ((host - start) / (end - start).max(1e-9)).clamp(0.0, 1.0),
                            voice: true,
                            since: host - start,
                        }
                    }
                    None => KineticFrame { text: String::new(), next: if voice.lines.is_empty() { String::new() } else { words(0) }, progress: 0.0, voice: true, since: 0.0 },
                }
            }
        }
    }

    fn opt4(&mut self, prop: &Option<Prop<4>>, t: f64) -> Option<Color> {
        self.properties().opt4(prop, t)
    }

    /// The beat at document time `abs` (the host's grid, else the
    /// document's bpm from 0).
    fn beat_at(&self, abs: f64) -> f64 {
        match &self.doc.context.timing.grid {
            Some(grid) => grid.beat_at(abs + self.doc.context.offset),
            None => abs * self.doc.bpm / 60.0,
        }
    }

    /// Layer `layer` at `t` of its parent's clock, whose start is at `origin`
    /// on the document's clock; `parent` places the parent's children.
    fn layer(&mut self, layer: &Layer, t: f64, origin: f64, parent: &Affine, parent_static: bool) -> Option<Item> {
        let (lt, origin) = makepad_motion_plain::placement::layer_clock(layer.visible, layer.span.as_ref(), t, origin)?;
        let per_unit = matches!(&layer.kind, LayerKind::Text { props: TextProps { by, .. }, source, .. } if *by != TextBy::Whole || matches!(source, TextSource::Voice(_)));
        let makepad_motion_plain::placement::Placement { world, local, opacity, blur, feather } =
            self.properties().place(&layer.transform, &layer.opacity, &layer.blur, &layer.feather, lt, parent, per_unit);
        let is_static = parent_static && !layer.animated;
        let kind = match &layer.kind {
            LayerKind::Rect(props) => ItemKind::Primitive(props.eval(&mut self.properties(), lt)),
            LayerKind::Ellipse(props) => ItemKind::Primitive(props.eval(&mut self.properties(), lt)),
            LayerKind::Path(props) => ItemKind::Primitive(props.eval(&mut self.properties(), lt)),
            LayerKind::Svg { src, size } => ItemKind::Svg { src: src.clone(), size: size.as_ref().map(|s| self.sample(s, lt, 0)) },
            LayerKind::Image { src, size, fit, zoom, focus, .. } => ItemKind::Image {
                mesh: None,
                src: src.clone(),
                size: size.as_ref().map(|s| self.sample(s, lt, 0)),
                fit: *fit,
                zoom: self.sample(zoom, lt, 0)[0].max(1e-3),
                focus: focus.as_ref().map(|f| self.sample(f, lt, 0)),
            },
            LayerKind::Video { src, size, fit, source_in, speed } => ItemKind::Video {
                src: src.clone(),
                size: size.as_ref().map(|s| self.sample(s, lt, 0)),
                fit: *fit,
                source_time: (source_in.value + lt * speed).max(0.0),
            },
            LayerKind::Text { .. } => self.text(layer, lt, origin),
            LayerKind::Bones(rig) => {
                let placed = world.then(&local.affine([0.0, 0.0]));
                self.bones(rig, lt, origin, &placed, is_static)
            }
            LayerKind::Group { layers, mask, props, post, .. } => {
                if !post.0.is_empty() {
                    let at = self.post_at(&post.0, lt);
                    self.group_post.push((layer.path.clone(), at));
                }
                let (size, placed) = props.placement(&mut self.properties(), lt, &world, &local);
                let count = layers.len();
                let mut children = Vec::new();
                for (index, child) in layers.iter().enumerate() {
                    let delay = props.stagger.delay(index, count);
                    if let Some(item) = self.layer(child, lt - delay, origin + delay, &placed, is_static) {
                        children.push(item);
                    }
                }
                let mask = mask.as_ref().and_then(|mask| self.layer(mask, lt, origin, &placed, is_static)).map(Box::new);
                ItemKind::Group { children, mask, mask_mode: props.mask_mode, size }
            }
            LayerKind::Vector { src, size, parts, draw_on, stagger, trace, fill, stroke } => ItemKind::Vector {
                src: src.clone(),
                size: size.as_ref().map_or(self.doc.size, |s| self.sample(s, lt, 0)),
                parts: parts.iter().map(|(id, p)| (id.clone(), PartFrame {
                    opacity: self.sample(&p.opacity, lt, 0)[0].clamp(0.0, 1.0),
                    offset: [self.sample(&p.x, lt, 0)[0], self.sample(&p.y, lt, 0)[0]],
                    scale: self.sample(&p.scale, lt, 0),
                    rotate: self.sample(&p.rotate, lt, 0)[0],
                    fill: self.opt4(&p.fill, lt),
                    stroke: self.opt4(&p.stroke, lt),
                    width: p.width.as_ref().map(|w| self.sample(w, lt, 0)[0].max(0.0)),
                    draw_on: p.draw_on.as_ref().map(|d| self.sample(d, lt, 0)[0].clamp(0.0, 1.0)),
                })).collect(),
                draw_on: draw_on.as_ref().map(|d| self.sample(d, lt, 0)[0].clamp(0.0, 1.0)),
                stagger: *stagger,
                trace: *trace,
                fill: self.opt4(fill, lt),
                stroke: self.opt4(stroke, lt),
            },
            LayerKind::Repeat { layer: template, items, props } => {
                let placed = world.then(&local.affine([0.0, 0.0]));
                let along = props.guide();
                let outer = self.instance;
                let mut children = Vec::with_capacity(props.count);
                for i in 0..props.count {
                    let (copy, o, d) = props.place_copy(i, &placed, along.as_ref());
                    self.instance = outer + i;
                    // A copy of a `source` item is its own compiled layer.
                    let layer = items.get(i).unwrap_or(template);
                    if let Some(mut item) = self.layer(layer, lt - d, origin + d, &copy, is_static) {
                        item.opacity *= o;
                        suffix_paths(&mut item, i);
                        children.push(item);
                    }
                }
                self.instance = outer;
                ItemKind::Group { children, mask: None, mask_mode: Default::default(), size: None }
            }
            LayerKind::Anchored { layer: inner, scene, node } => {
                // The scene layer's path, by its name.
                let scene_path = self.doc.all_layers().iter().find(|l| l.name.as_deref() == Some(scene.as_str())).map(|l| l.path.clone()).unwrap_or_default();
                let placed = world.then(&local.affine([0.0, 0.0]));
                match self.layer(inner, lt, origin, &placed, is_static) {
                    Some(item) => ItemKind::Anchored { inner: Box::new(item), scene: scene_path, node: node.clone() },
                    None => ItemKind::Group { children: Vec::new(), mask: None, mask_mode: MaskMode::Alpha, size: None },
                }
            }
            LayerKind::FlyThrough { layers, target, letter, time, duration, zoom } => {
                let placed = world.then(&local.affine([0.0, 0.0]));
                let children: Vec<Item> = layers.iter().filter_map(|child| self.layer(child, lt, origin, &placed, is_static)).collect();
                // Accelerating in: exponential in the zoom, so the speed of
                // the approach feels constant, then faster.
                let p = ((lt - time.value) / duration).clamp(0.0, 1.0);
                let e = p * p * p;
                let inner = Item {
                    path: format!("{}.fly", layer.path),
                    world: Affine::IDENTITY,
                    local: Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
                    opacity: 1.0,
                    blur: 0.0,
                    feather: 0.0,
                    feather_side: Default::default(),
                    effects: Vec::new(),
                    blend: Blend::Normal,
                    is_static: false,
                    kind: ItemKind::Group { children, mask: None, mask_mode: MaskMode::Alpha, size: None },
                };
                ItemKind::Fly { inner: Box::new(inner), target: target.clone(), letter: *letter, zoom: zoom.powf(e) }
            }
            LayerKind::Freeze { time, size } => {
                let time = match time {
                    FreezeTime::Moment(time) => time.value,
                    FreezeTime::Moving(time) => self.sample(time, lt, 0)[0],
                };
                let frame = world.then(&local.affine(*size));
                let mut children = Vec::new();
                if !self.freezing {
                    // The picture at `time`, covering the box about its centre.
                    let [w, h] = self.doc.size;
                    let scale = (size[0] / w.max(1e-9)).max(size[1] / h.max(1e-9));
                    let placed = frame.then(&Affine::translate((size[0] - w * scale) * 0.5, (size[1] - h * scale) * 0.5)).then(&Affine::scale(scale, scale));
                    let (edit, instance) = (self.edit, self.instance);
                    self.freezing = true;
                    self.edit = time + self.doc.context.offset;
                    if self.doc.context.edit_frames {
                        // The host's picture at `time` (its edit under the
                        // graphic), under the document's own layers.
                        children.push(Item {
                            path: format!("{}.edit", layer.path),
                            world: placed,
                            local: Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
                            opacity: 1.0,
                            blur: 0.0,
                            feather: 0.0,
                            feather_side: Default::default(),
                            effects: Vec::new(),
                            blend: Blend::Normal,
                            is_static: false,
                            kind: ItemKind::Video { src: Media::Backdrop, size: Some([w, h]), fit: Fit::Fill, source_time: self.edit },
                        });
                    }
                    for below in &self.doc.layers[..self.root] {
                        if let Some(mut item) = self.layer(below, time, 0.0, &placed, false) {
                            suffix_paths(&mut item, usize::MAX);
                            children.push(item);
                        }
                    }
                    self.freezing = false;
                    self.edit = edit;
                    self.instance = instance;
                }
                let clip = Item {
                    path: format!("{}.clip", layer.path),
                    world: frame,
                    local: Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
                    opacity: 1.0,
                    blur: 0.0,
                    feather: 0.0,
                    feather_side: Default::default(),
                    effects: Vec::new(),
                    blend: Blend::Normal,
                    is_static: false,
                    kind: ItemKind::Primitive(PrimitiveFrame::Rect { size: *size, radius: 0.0, fill: Some([1.0, 1.0, 1.0, 1.0]), stroke: None, width: 0.0, trim: [0.0, 1.0], cap: Cap::Butt }),
                };
                ItemKind::Group { children, mask: Some(Box::new(clip)), mask_mode: MaskMode::Alpha, size: Some(*size) }
            }
            LayerKind::Character(ch) => {
                let placed = world.then(&local.affine([0.0, 0.0]));
                self.character(&layer.path, layer.loc, ch, lt, origin, &placed)
            }
            LayerKind::Crowd(crowd) => {
                let placed = world.then(&local.affine(crowd.size));
                let feature = |this: &Self, name: &str| -> f64 {
                    let signals = &this.doc.context.signals;
                    let music = signals.default_music().unwrap_or("").to_string();
                    let level = if name == "energy" { signals.energy(&music, this.edit) } else { signals.band(&music, name, this.edit) };
                    level.clamp(0.0, 1.0)
                };
                let (level, still) = match &crowd.react {
                    CrowdReact::Beat => (None, false),
                    CrowdReact::None => (None, true),
                    CrowdReact::Feature(name) => (Some(feature(self, name)), false),
                };
                // A cheer lifts the arms once the feature is well up.
                let cheer = crowd.cheer.as_deref().map_or(0.0, |name| {
                    let v = ((feature(self, name) - 0.55) / 0.3).clamp(0.0, 1.0);
                    v * v * (3.0 - 2.0 * v)
                });
                let color = self.sample(&crowd.color, lt, 0);
                let color2 = self.opt4(&crowd.color2, lt);
                let outline = self.opt4(&crowd.outline, lt);
                let bob = self.sample(&crowd.bob, lt, 0)[0].max(0.0);
                let hop = self.sample(&crowd.hop, lt, 0)[0].max(0.0);
                let nod = self.sample(&crowd.nod, lt, 0)[0].max(0.0);
                let beat = self.beat_at(origin + lt);
                let beats_per_second = self.doc.bpm / 60.0;
                let mut late = |d: f64, i: usize, reach: &mut Vec<_>| {
                    reach.clear();
                    if let Some(member) = &crowd.member {
                        reach.extend(member.reach.iter().map(|(joint, target)| (*joint, self.sample(target, lt, i))));
                    }
                    (self.sample(&crowd.arms, lt - d, 0)[0].clamp(0.0, 1.0), self.sample(&crowd.sign_up, lt - d, 0)[0].clamp(0.0, 1.0), crowd.member.as_ref().and_then(|ch| self.pose_blend(ch, lt, i)))
                };
                let mut unreachable = Vec::new();
                let mut moment = crate::crowd::Moment { crowd, color, color2, outline, bob, hop, nod, level, still, cheer, late: &mut late, unreachable: &mut unreachable, beat, beats_per_second, t: lt, start: crowd.start.value };
                let children = crate::crowd::items(&layer.path, &placed, &mut moment);
                for (i, joint) in unreachable {
                    self.errors.push(Diagnostic::warning(layer.loc, format!("{}.member[{i}].reach.{}", layer.path, joint.name()), "Character reach target is out of reach; using the nearest reachable point"));
                }
                ItemKind::Group { children, mask: None, mask_mode: Default::default(), size: None }
            }
            LayerKind::Shader { program, size, params, speed, below, data, text, font } => ItemKind::Shader {
                program: program.clone(),
                size: size.as_ref().map_or(self.doc.size, |s| self.sample(s, lt, 0)),
                params: params.iter().map(|(name, prop)| (name.clone(), self.sample(prop, lt, 0)[0])).collect(),
                local_time: lt * speed,
                beat: self.beat_at(origin + lt),
                below: *below,
                data: data.clone(),
                text: text.as_ref().map(|source| self.kinetic(source, origin + lt + self.doc.context.offset, lt)),
                audio: {
                    let signals = &self.doc.context.signals;
                    let music = signals.default_music().unwrap_or("").to_string();
                    let at = self.edit;
                    [signals.energy(&music, at), signals.band(&music, "low", at), signals.band(&music, "mid", at), signals.band(&music, "high", at)]
                },
                sound: (self.doc.context.signals.default_music().unwrap_or("").to_string(), self.edit),
                font: font.clone(),
            },
            LayerKind::Scene3dDoc { scene, loc, written, size, textures, videos, layers, .. } => ItemKind::Scene3dDoc {
                written: *written,
                layers: layers.iter().map(|(name, layer)| (name.clone(), self.layer(layer, lt, origin, &Affine::IDENTITY, false).into_iter().collect())).collect(),
                bpm: self.doc.bpm,
                textures: textures.clone(),
                videos: videos.clone(),
                scene: match self.calls.scene_source(scene) {
                    Ok(scene) => scene,
                    Err(message) => {
                        self.errors.push(Diagnostic::error(*loc, layer.path.clone(), message));
                        return None;
                    }
                },
                first_line: loc.line,
                size: size.as_ref().map_or(self.doc.size, |s| self.sample(s, lt, 0)),
                local_time: lt,
                edit_offset: self.edit - lt,
                fps: self.doc.fps,
                signals: SignalsRef(self.doc.context.signals.clone()),
                modules: self.doc.context.modules.clone(),
            },
            LayerKind::Scene3D { model, size, camera, lights, nodes, materials, background, exposure } => {
                let target = self.sample(&camera.target, lt, 0);
                let eye = if camera.orbit {
                    let (yaw, pitch) = (self.sample(&camera.yaw, lt, 0)[0].to_radians(), self.sample(&camera.pitch, lt, 0)[0].to_radians());
                    let distance = self.sample(&camera.distance, lt, 0)[0];
                    [target[0] + distance * pitch.cos() * yaw.sin(), target[1] + distance * pitch.sin(), target[2] + distance * pitch.cos() * yaw.cos()]
                } else {
                    self.sample(&camera.position, lt, 0)
                };
                ItemKind::Scene3D {
                    model: model.clone(),
                    size: size.as_ref().map_or(self.doc.size, |s| self.sample(s, lt, 0)),
                    camera: CameraFrame { eye, target, fov: self.sample(&camera.fov, lt, 0)[0].clamp(1.0, 170.0) },
                    lights: lights.iter().map(|light| LightFrame {
                        point: light.point,
                        direction: self.sample(&light.direction, lt, 0),
                        position: self.sample(&light.position, lt, 0),
                        color: self.sample(&light.color, lt, 0),
                        intensity: self.sample(&light.intensity, lt, 0)[0],
                    }).collect(),
                    nodes: nodes.iter().map(|node| NodeFrame {
                        name: node.name.clone(),
                        translate: self.sample(&node.translate, lt, 0),
                        rotate: self.sample(&node.rotate, lt, 0),
                        scale: self.sample(&node.scale, lt, 0),
                        visible: self.sample(&node.visible, lt, 0)[0] > 0.5,
                    }).collect(),
                    materials: materials.iter().map(|m| MaterialFrame {
                        name: m.name.clone(),
                        base_color: self.opt4(&m.base_color, lt),
                        metallic: m.metallic.as_ref().map(|p| self.sample(p, lt, 0)[0]),
                        roughness: m.roughness.as_ref().map(|p| self.sample(p, lt, 0)[0]),
                        emissive: self.opt4(&m.emissive, lt),
                    }).collect(),
                    background: self.opt4(background, lt),
                    exposure: self.sample(exposure, lt, 0)[0],
                    local_time: lt,
                }
            }
            LayerKind::Widget { ui, size, props, .. } => ItemKind::Widget {
                ui: ui.clone(),
                size: self.sample(size, lt, 0),
                props: props.iter().map(|(name, prop)| (name.clone(), match prop {
                    WidgetProp::Num(p) => WidgetValue::Num(self.sample(p, lt, 0)[0]),
                    WidgetProp::Color(p) => WidgetValue::Color(self.sample(p, lt, 0)),
                    WidgetProp::Text(text) => WidgetValue::Text(text.clone()),
                })).collect(),
            },
            LayerKind::Draw2d { draw, size } => {
                let size = size.as_ref().map_or(self.doc.size, |p| self.sample(p, lt, 0));
                match self.calls.call_draw2d(draw.index, lt, self.edit) {
                    Ok((picture, messages)) => {
                        for message in messages {
                            self.errors.push(Diagnostic::error(draw.loc, layer.path.clone(), message));
                        }
                        ItemKind::Draw2d { picture, size }
                    }
                    Err(message) => {
                        self.errors.push(Diagnostic::error(draw.loc, layer.path.clone(), message));
                        return None;
                    }
                }
            }
        };
        let effects = layer.effects.iter().map(|e| EffectFrame { name: e.name.clone(), params: e.params.iter().map(|(n, a)| (n.clone(), self.arg(a, lt))).collect() }).collect();
        Some(Item { path: layer.path.clone(), world, local, opacity, blur, feather, feather_side: layer.feather_side, effects, blend: layer.blend, is_static, kind })
    }

    fn text(&mut self, layer: &Layer, lt: f64, origin: f64) -> ItemKind {
        let LayerKind::Text { source, props: TextProps { font, weight, italic, size, color, align, valign, by, stagger, line_height, tracking, wrap, fit, reveal }, ranges, voice, plate, caret, path, flicker, only, axes, stroke, .. } = &layer.kind else {
            unreachable!()
        };
        let size = self.sample(size, lt, 0)[0].max(0.0);
        let plain = |this: &mut Self, text: String| {
            let units = this.properties().text_units(&text, *by, stagger, &layer.transform, &layer.opacity, &layer.blur, color, lt);
            (text, units)
        };
        let (text, units) = match source {
            TextSource::Text(text) => plain(self, text.clone()),
            TextSource::Number { value, decimals, grouping, prefix, suffix, clock, format, log, wall, .. } => {
                let clock = match clock {
                    None => 0.0,
                    Some(Clock::Layer) => lt,
                    Some(Clock::Doc) => origin + lt,
                    Some(Clock::Edit) => origin + lt + self.doc.context.offset,
                };
                let n = self.sample(value, lt, 0)[0] + clock;
                // The power of ten when `log`.
                let exponent = if *log { n } else if n.abs() > 0.0 { n.abs().log10() } else { f64::NEG_INFINITY };
                let shown = if *wall && exponent >= 15.0 {
                    // A 1 and its zeros, broken into lines as wide as the frame.
                    let zeros = exponent.floor().min(4000.0) as usize;
                    let digits = format!("1{}", "0".repeat(zeros));
                    let per_line = ((self.doc.size[0] / (size * 4.0 / 3.0 * 0.62)).floor() as usize).max(4);
                    digits.as_bytes().chunks(per_line).map(|c| String::from_utf8_lossy(c).to_string()).collect::<Vec<_>>().join("\n")
                } else if *log {
                    format_escalate_exp(n, *decimals)
                } else {
                    match format {
                        TimeFormat::Plain => format_number(n, *decimals, grouping.as_deref()),
                        other => format_time(n, *other, self.doc.fps, *decimals),
                    }
                };
                plain(self, format!("{prefix}{shown}{suffix}"))
            }
            TextSource::Voice(id) => {
                let features = self.doc.context.signals.voice(id).cloned().unwrap_or_default();
                self.voice_units(layer, &features, *by, voice, stagger, lt, origin, size, color)
            }
        };
        // Neon: each letter's tube sputters; the `fail` letters keep dropping out.
        let mut units = units;
        if let Some((amount, fails)) = flicker {
            let amount = self.sample(amount, lt, 0)[0].clamp(0.0, 1.0);
            let abs = origin + lt;
            for (i, unit) in units.iter_mut().enumerate() {
                let letter = text.get(..unit.range.start).map_or(i, |before| before.chars().count());
                unit.opacity *= crate::crowd::neon(letter, abs, amount, fails.contains(&letter));
            }
        }
        // `only`: the other letters draw nothing.
        if let Some(only) = only {
            for (i, unit) in units.iter_mut().enumerate() {
                let letter = text.get(..unit.range.start).map_or(i, |before| before.chars().count());
                if !only.contains(&letter) {
                    unit.opacity = 0.0;
                }
            }
        }
        let reveal = self.properties().text_reveal(&text, reveal.as_ref(), lt);
        let plate = plate.as_ref().map(|p| PlateFrame {
            color: self.sample(&p.color, lt, 0),
            pad: p.pad,
            radius: p.radius,
            stroke: self.opt4(&p.stroke, lt),
            width: p.width,
            reveal: self.sample(&p.reveal, lt, 0)[0].clamp(0.0, 1.0),
            each: p.each,
            underline: p.underline,
        });
        let path = path.as_ref().map(|(data, offset)| (data.clone(), self.sample(offset, lt, 0)[0]));
        let caret = caret.as_ref().map(|(opacity, color, block)| (self.sample(opacity, lt, 0)[0].clamp(0.0, 1.0), self.sample(color, lt, 0), *block));
        let axes: Vec<_> = axes.iter().map(|(tag, value)| (tag.clone(), axis_step(tag, self.sample(value, lt, 0)[0]))).collect();
        self.text_ranges(ranges, &text, *by, &axes, &mut units, lt);
        let stroke = stroke.as_ref().map(|(color, width)| (self.sample(color, lt, 0), self.sample(width, lt, 0)[0].max(0.0)));
        ItemKind::Primitive(PrimitiveFrame::Text {
            text,
            font: font.clone(),
            weight: *weight,
            axes,
            italic: *italic,
            size,
            align: *align,
            valign: *valign,
            line_height: *line_height,
            tracking: self.sample(tracking, lt, 0)[0],
            wrap: *wrap,
            fit: *fit,
            path,
            by: *by,
            units,
            plate,
            reveal,
            caret,
            outline: stroke,
        })
    }

    fn unit(&mut self, layer: &Layer, range: std::ops::Range<usize>, ut: f64, index: usize, color: &Prop<4>) -> TextUnit {
        self.properties().text_unit(&layer.transform, &layer.opacity, &layer.blur, range, ut, index, color)
    }

    /// Ranges sample once on the layer clock, then blend in written order.
    /// Only units receiving axis targets allocate an axes vector; selection
    /// walks the text in order without a per-unit map or weight buffer.
    fn text_ranges(&mut self, ranges: &[TextRange], text: &str, by: TextBy, axes: &[(String, f64)], units: &mut [TextUnit], lt: f64) {
        let axis_capacity = axes.len() + ranges.iter().map(|r| r.axes.len()).sum::<usize>();
        for range in ranges {
            let from = self.sample(&range.from, lt, 0)[0];
            let to = self.sample(&range.to, lt, 0)[0];
            if to < from {
                self.errors.push(Diagnostic::error(range.loc, format!("{}.to", range.path), "Range `to` must be greater than or equal to `from`"));
                continue;
            }
            let slide = self.sample(&range.slide, lt, 0)[0];
            let soft = self.sample(&range.soft, lt, 0)[0];
            if soft < 0.0 {
                self.errors.push(Diagnostic::error(range.loc, format!("{}.soft", range.path), "Range `soft` is a non-negative fraction (0 gives hard edges)"));
                continue;
            }
            let window = RangeWindow { from: from + slide, to: to + slide, soft, ease: range.ease, invert: range.invert };
            let x = range.x.as_ref().map(|p| self.sample(p, lt, 0)[0]);
            let y = range.y.as_ref().map(|p| self.sample(p, lt, 0)[0]);
            let rotate = range.rotate.as_ref().map(|p| self.sample(p, lt, 0)[0]);
            let blur = range.blur.as_ref().map(|p| self.sample(p, lt, 0)[0]);
            let scale = range.scale.as_ref().map(|p| self.sample(p, lt, 0));
            let opacity = range.opacity.as_ref().map(|p| self.sample(p, lt, 0)[0]);
            let color = range.color.as_ref().map(|p| self.sample(p, lt, 0));
            let count = if range.by == by { units.len() } else { unit_range_iter(text, range.by).count() };
            if x.is_some() || y.is_some() || rotate.is_some() || blur.is_some() || scale.is_some() || opacity.is_some() || color.is_some() {
                range_units(text, by, range.by, count, units, |unit, i, n| {
                    let w = window.weight(i, n);
                    if w == 0.0 {
                        return;
                    }
                    if let Some(x) = x { unit.offset[0] += w * x; }
                    if let Some(y) = y { unit.offset[1] += w * y; }
                    if let Some(rotate) = rotate { unit.rotate += w * rotate; }
                    if let Some(blur) = blur { unit.blur += w * blur; }
                    if let Some(scale) = scale {
                        for k in 0..2 { unit.scale[k] *= 1.0 + w * (scale[k] - 1.0); }
                    }
                    if let Some(opacity) = opacity { unit.opacity *= 1.0 + w * (opacity - 1.0); }
                    if let Some(color) = color {
                        for k in 0..4 { unit.color[k] += w * (color[k] - unit.color[k]); }
                    }
                });
            }
            // Sample each axis once, then walk the same selection. No
            // temporary vector of sampled axis values is needed.
            for axis in &range.axes {
                let target = self.sample(&axis.value, lt, 0)[0];
                range_units(text, by, range.by, count, units, |unit, i, n| {
                    let w = window.weight(i, n);
                    if w == 0.0 {
                        return;
                    }
                    if unit.axes.is_empty() {
                        unit.axes.reserve(axis_capacity);
                        unit.axes.extend(axes.iter().cloned());
                    }
                    match unit.axes.iter_mut().find(|(tag, _)| *tag == axis.tag) {
                        Some((_, value)) => *value += w * (target - *value),
                        None => unit.axes.push((axis.tag.clone(), axis.default + w * (target - axis.default))),
                    }
                });
            }
        }
        // Round only after every range has blended, so list order is exact
        // and drawing still reuses the same stepped font instances.
        if axis_capacity > axes.len() {
            for unit in units {
                for (tag, value) in &mut unit.axes {
                    *value = axis_step(tag, *value);
                }
            }
        }
    }

    /// A voiceover's words as units (§10.3): which show, and their
    /// highlight, pop, stretch and wave, from the words' states at the
    /// document time `origin + lt`.
    #[allow(clippy::too_many_arguments)]
    fn voice_units(&mut self, layer: &Layer, voice: &VoiceFeatures, by: TextBy, style: &VoiceStyle, stagger: &Stagger, lt: f64, origin: f64, size: f64, color: &Prop<4>) -> (String, Vec<TextUnit>) {
        // Word times are on the host's clock.
        let abs = origin + lt + self.doc.context.offset;
        let active_line = voice.line_at(abs).or_else(|| {
            // Between lines: the last line begun.
            voice.lines.iter().rposition(|line| line.0 <= abs)
        });
        let active_word = voice.word_at(abs);
        let mut text = String::new();
        let mut units = Vec::new();
        let words: Vec<(usize, &crate::features::FeatureWord)> = voice.words.iter().enumerate().filter(|(_, word)| match (style.line, style.show) {
            (Some(line), _) => word.line == line,
            (None, VoiceShow::Line) => Some(word.line as usize) == active_line,
            _ => true,
        }).collect();
        let count = words.len();
        for (k, (index, word)) in words.iter().enumerate() {
            let state = voice.word_state(*index, abs);
            let visible = match style.show {
                VoiceShow::All | VoiceShow::Line => true,
                VoiceShow::Spoken => !matches!(state, WordState::Upcoming),
                VoiceShow::Active => active_word == Some(*index),
            };
            if !text.is_empty() {
                text.push(' ');
            }
            let start = text.len();
            text.push_str(&word.text);
            // The unit's own clock: from its word's start, so `y:
            // tween(20, 0, 0, 0.2)` rises as each word is said.
            let delay = stagger.delay(k, count);
            let ut = abs - word.start - delay;
            let ranges: Vec<std::ops::Range<usize>> = if by == TextBy::Letter {
                word.text.char_indices().map(|(at, c)| start + at..start + at + c.len_utf8()).collect()
            } else {
                vec![start..text.len()]
            };
            let letters = ranges.len().max(1);
            for (li, range) in ranges.into_iter().enumerate() {
                let mut unit = self.unit(layer, range, ut, *index, color);
                if !visible {
                    unit.opacity = 0.0;
                }
                // With `sweep`, a letter of the current word counts as said
                // once the fill has reached it.
                let reached = !style.sweep || abs >= word.start + (word.end - word.start).max(0.0) * li as f64 / letters as f64;
                let upcoming = matches!(state, WordState::Upcoming) || (matches!(state, WordState::Active(_)) && !reached);
                let lit = match style.highlight {
                    Some(VoiceShow::Active) => matches!(state, WordState::Active(_)) && reached,
                    Some(VoiceShow::Spoken) => !upcoming,
                    _ => false,
                };
                if lit {
                    unit.color = style.highlight_color;
                } else if upcoming {
                    if let Some(ghost) = style.ghost {
                        unit.color = ghost;
                    }
                }
                if let Some(k) = &style.karaoke {
                    unit.color = karaoke_color(k, word, li, letters, abs, unit.color);
                }
                if style.pop {
                    let e = abs - word.start;
                    if (0.0..0.3).contains(&e) {
                        let bump = 1.0 + 0.3 * (std::f64::consts::PI * e / 0.3).sin();
                        unit.scale = [unit.scale[0] * bump, unit.scale[1] * bump];
                    }
                }
                if style.stretch {
                    if let Some((a, b)) = word.syllables.iter().find(|(a, b)| *a <= abs && abs < *b) {
                        let long = ((b - a - 0.2) / 0.4).clamp(0.0, 0.5);
                        let u = ((abs - a) / (b - a)).clamp(0.0, 1.0);
                        unit.scale[0] *= 1.0 + long * (std::f64::consts::PI * u).sin();
                    }
                }
                if style.wave && matches!(state, WordState::Active(_)) {
                    let pitch = voice.pitch.sample(abs);
                    if pitch > 0.0 {
                        unit.offset[1] -= ((pitch - 150.0) / 150.0).clamp(-1.0, 1.0) * size * 0.25;
                    }
                }
                units.push(unit);
            }
        }
        (text, units)
    }
}

/// Seconds as a time: `@timecode` 00:00:00:12 (frames at `fps`), `@mmss`
/// 45:10, `@hhmmss` 01:45:10, `@seconds` 3.5 s or 3.5 / 15.0 s (with
/// `decimals` digits, 1 when 0). Shared with DV1's `@counter`.
pub fn format_time(t: f64, format: TimeFormat, fps: f64, decimals: u32) -> String {
    if format == TimeFormat::Escalate {
        return format_escalate(t, decimals);
    }
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let whole = t.floor() as u64;
    let (h, m, s) = (whole / 3600, (whole / 60) % 60, whole % 60);
    match format {
        TimeFormat::Plain => format_number(t, decimals, None),
        TimeFormat::Timecode => {
            let fps = fps.max(1.0);
            let frame = ((t - t.floor()) * fps + 1e-6).floor() as u64;
            format!("{h:02}:{m:02}:{s:02}:{frame:02}")
        }
        TimeFormat::MmSs => format!("{:02}:{s:02}", whole / 60),
        TimeFormat::HhMmSs => format!("{h:02}:{m:02}:{s:02}"),
        TimeFormat::Escalate => format_escalate(t, decimals),
        TimeFormat::Seconds { total } => {
            let d = if decimals == 0 { 1 } else { decimals as usize };
            match total {
                Some(total) => format!("{:.*} / {:.*} s", d, t.min(total), d, total),
                None => format!("{:.*} s", d, t),
            }
        }
    }
}

const SCALE_NAMES: [&str; 11] = ["million", "billion", "trillion", "quadrillion", "quintillion", "sextillion", "septillion", "octillion", "nonillion", "decillion", "undecillion"];

/// A number that outgrows its notation (shared with DV1's `@counter`):
/// below a million, grouped ("999,999.5" with `decimals`); up to 10^36,
/// named with three significant digits ("12.4 million", "1 nonillion");
/// exactly 10^100 "googol"; beyond, scientific ("1.0e42", `decimals`
/// digits, 1 when 0); "∞" and "NaN" for those.
pub fn format_escalate(n: f64, decimals: u32) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "∞".into() } else { "-∞".into() };
    }
    if n.abs() < 1e6 {
        return format_number(n, decimals, Some(","));
    }
    let sign = if n < 0.0 { "-" } else { "" };
    format!("{sign}{}", format_escalate_exp(n.abs().log10(), decimals))
}

/// `format_escalate` of 10^`e` (for numbers past f64: `log: true`).
pub fn format_escalate_exp(e: f64, decimals: u32) -> String {
    if e.is_nan() {
        return "NaN".into();
    }
    if e == f64::INFINITY {
        return "∞".into();
    }
    if e < 6.0 {
        return format_number(10f64.powf(e), decimals, Some(","));
    }
    if (e - 100.0).abs() < 1e-9 {
        return "googol".into();
    }
    let whole = e.floor();
    let mantissa = 10f64.powf(e - whole);
    if e < 36.0 {
        let group = ((whole as i64 - 6) / 3) as usize;
        let value = mantissa * 10f64.powi((whole as i64 - 6 - 3 * group as i64) as i32);
        // Three significant digits, no trailing zeros.
        let digits = if value >= 100.0 { 0 } else if value >= 10.0 { 1 } else { 2 };
        let text = format!("{value:.digits$}");
        let text = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text };
        return format!("{text} {}", SCALE_NAMES[group.min(SCALE_NAMES.len() - 1)]);
    }
    let d = if decimals == 0 { 1 } else { decimals as usize };
    format!("{mantissa:.d$}e{}", whole as i64)
}

/// `n` with `decimals` digits after the point and `grouping` between
/// thousands.
pub fn format_number(n: f64, decimals: u32, grouping: Option<&str>) -> String {
    let n = if n.is_finite() { n } else { 0.0 };
    let text = format!("{:.*}", decimals as usize, n);
    let Some(sep) = grouping else { return text };
    let (sign, rest) = if let Some(rest) = text.strip_prefix('-') { ("-", rest) } else { ("", text.as_str()) };
    let (int, frac) = match rest.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (rest, None),
    };
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push_str(sep);
        }
        grouped.push(c);
    }
    match frac {
        Some(frac) => format!("{sign}{grouped}.{frac}"),
        None => format!("{sign}{grouped}"),
    }
}

pub use makepad_motion_plain::geometry::{point_along, Polyline};

pub use makepad_motion_plain::unit_ranges;
use makepad_motion_plain::unit_range_iter;

/// Each unit and its index/count in a range's selection. A coarser range
/// advances one cursor through the containing words/lines (linear work).
fn range_units(text: &str, text_by: TextBy, by: TextBy, n: usize, units: &mut [TextUnit], mut apply: impl FnMut(&mut TextUnit, usize, usize)) {
    if by == text_by {
        for (i, unit) in units.iter_mut().enumerate() { apply(unit, i, n); }
        return;
    }
    let mut selected = unit_range_iter(text, by).enumerate();
    let mut current = selected.next();
    for unit in units {
        while current.as_ref().is_some_and(|(_, span)| span.end <= unit.range.start) {
            current = selected.next();
        }
        if let Some((i, span)) = &current {
            if span.contains(&unit.range.start) { apply(unit, *i, n); }
        }
    }
}

struct RangeWindow {
    from: f64,
    to: f64,
    soft: f64,
    ease: Ease,
    invert: bool,
}

impl RangeWindow {
    fn weight(&self, i: usize, n: usize) -> f64 {
        let p = (i as f64 + 0.5) / n as f64;
        let w = if self.soft == 0.0 {
            if self.from <= p && p <= self.to { 1.0 } else { 0.0 }
        } else {
            let ramp = |u: f64| {
                if u <= 0.0 { 0.0 } else if u >= 1.0 { 1.0 } else { self.ease.apply(u, 1.0).clamp(0.0, 1.0) }
            };
            ramp((p - self.from + self.soft) / self.soft) * ramp((self.to + self.soft - p) / self.soft)
        };
        if self.invert { 1.0 - w } else { w }
    }
}

pub use makepad_motion_plain::{key_handles, sample_track};

/// A `Repeat` copy's item paths made its own (`…layer#3`), so everything
/// keyed by path (prepared pictures, caches) tells the copies apart.
fn suffix_paths(item: &mut Item, index: usize) {
    item.path = format!("{}#{index}", item.path);
    if let ItemKind::Group { children, mask, .. } = &mut item.kind {
        for child in children {
            suffix_paths(child, index);
        }
        if let Some(mask) = mask {
            suffix_paths(mask, index);
        }
    }
}

/// An axis value as the font is read at it: whole units (`wght` in steps of
/// 5), so an animated axis reuses the font instances of values it passed
/// instead of making a new one every frame (a step is under the eye's
/// threshold at any size).
pub(crate) fn axis_step(tag: &str, value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    if tag == "wght" {
        (value / 5.0).round() * 5.0
    } else {
        value.round()
    }
}

/// A letter's (or, with one letter, a word's) karaoke colour at `abs`:
/// unsung (warming toward sung over the lead-in), sung, then cooled, from
/// `makepad_typography::karaoke` over the word's own time (or its
/// syllables'), never before the word starts.
fn karaoke_color(k: &Karaoke, word: &crate::features::FeatureWord, letter: usize, letters: usize, abs: f64, own: Color) -> Color {
    use makepad_typography::karaoke::{states, Direction, KaraokeBy, KaraokeStyle, State, SungWord};
    let n = letters.max(1);
    // Syllables spread over the letters evenly (the analysis gives times,
    // not letter spans).
    let syllables = if k.syllables && word.syllables.len() > 1 {
        let m = word.syllables.len();
        word.syllables.iter().enumerate().map(|(i, (a, b))| (i * n / m..((i + 1) * n / m).max(i * n / m + 1).min(n), *a as f32, *b as f32)).collect()
    } else {
        Vec::new()
    };
    let style = KaraokeStyle {
        by: if n == 1 { KaraokeBy::Word } else if syllables.is_empty() { KaraokeBy::Char } else { KaraokeBy::Syllable },
        anticipate: k.anticipate as f32,
        cool: k.cool as f32,
        dir: if k.rtl { Direction::Rtl } else { Direction::Ltr },
    };
    let w = SungWord { chars: 0..n, start: word.start as f32, end: word.end as f32, syllables };
    let s = states(&[w], n, abs as f32, &style)[letter.min(n - 1)];
    let unsung = k.unsung.unwrap_or(own);
    let mix = |a: Color, b: Color, t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t];
    match s.state {
        State::Unsung => mix(unsung, k.sung, 0.35 * s.anticipation as f64),
        State::Sung => mix(k.sung, k.cooled, if k.cool > 0.0 { (s.age as f64 / k.cool).clamp(0.0, 1.0) } else { 1.0 }),
        State::Cooled => k.cooled,
    }
}
