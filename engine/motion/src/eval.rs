//! `MotionDoc::eval(t)` and `eval_live(t, live)`: the document at one moment,
//! as a pure function of its clocks, inputs and context. Keys are sampled,
//! staggers shift unit and child clocks, voice-bound text takes its words' states, and property
//! functions run in the document's own VM (the one its load evaluated it
//! in; the VM sees only the supplied clocks, never wall time or a random source).

use crate::features::{VoiceFeatures, WordState};
use crate::frame::*;
use crate::model::*;
use crate::value::Evaluated;
use crate::Ease;

/// Character layers (lane CH1).
mod character;
mod rig;
use makepad_script::*;
use std::sync::Arc;

/// The value a property function gave.
enum FnOut {
    Nums(Vec<f64>),
    Color(Color),
}

impl MotionDoc {
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
        let mut ev = Evaluator { doc: self, errors: Vec::new(), edit: t + self.context.offset, instance: 0, root: 0, freezing: false, group_post: Vec::new() };
        let mut items = Vec::new();
        let mut data_layers: Vec<(String, Vec<Item>)> = Vec::new();
        for (root, layer) in self.layers.iter().enumerate() {
            ev.root = root;
            let item = ev.layer(layer, t, 0.0, &Affine::IDENTITY, true);
            match (&layer.data, &layer.name) {
                (Some(_), Some(name)) => data_layers.push((name.clone(), item.into_iter().collect())),
                (Some(_), None) => {}
                _ => items.extend(item),
            }
        }
        let camera = self.camera.as_ref().map(|camera| camera.iter().map(|(name, arg)| (name.clone(), ev.arg(arg, t))).collect());
        let post = (!self.post.is_empty()).then(|| ev.post_at(&self.post, t));
        let motion_blur = self.motion_blur.as_ref().map(|m| {
            let [shutter] = ev.sample(&m.shutter, t, 0);
            (shutter.clamp(0.0, 720.0), m.max, m.tol)
        });
        let errors = ev.errors;
        self.collect_garbage();
        let group_post = std::mem::take(&mut ev.group_post);
        MotionFrame { t, size: self.size, background: self.background, items, camera, fps: self.fps, hdr: self.hdr, post, group_post, motion_blur, data_layers, errors }
    }

    /// Frame times of the document at its own frame rate: `n / fps` for
    /// every frame that starts before `duration`.
    pub fn frame_times(&self) -> Vec<f64> {
        let n = (self.duration * self.fps - 1e-9).ceil().max(1.0) as usize;
        (0..n).map(|k| k as f64 / self.fps).collect()
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
    doc: &'a MotionDoc,
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

impl Evaluator<'_> {
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

    fn sample<const N: usize>(&mut self, prop: &Prop<N>, t: f64, i: usize) -> [f64; N] {
        self.sample_kind(prop, t, i, None)
    }

    /// A blend also checks function results: ordinary field shorthand must
    /// not silently truncate a vector control or hide differing value kinds.
    fn sample_kind<const N: usize>(&mut self, prop: &Prop<N>, t: f64, i: usize, kind: Option<usize>) -> [f64; N] {
        match prop {
            Prop::Const(value) => *value,
            Prop::Track(track) => sample_track(track, t),
            Prop::Blend(blend) => {
                let x = self.sample_kind(&blend.x, t, i, Some(1))[0];
                let y = blend.y.as_ref().map_or(0.0, |y| self.sample_kind(y, t, i, Some(1))[0]);
                let kind = Some(blend.value_kind.unwrap_or(N));
                blend.sample(x, y, |p| self.sample_kind(p, t, i, kind), |a, b, w| std::array::from_fn(|k| a[k] + (b[k] - a[k]) * w))
            }
            Prop::Fn(f) => match self.doc.call_fn(f.index, t, i + self.instance, self.edit) {
                Ok(FnOut::Nums(values)) => {
                    if kind.is_some_and(|kind| kind != values.len()) {
                        self.fn_error(f, format!("mismatched blend value kinds: returned {} components, expected {}", values.len(), kind.unwrap()));
                        return [0.0; N];
                    }
                    let mut out = [0.0; N];
                    if values.len() == 1 {
                        out = [values[0]; N];
                        if N == 4 {
                            out[3] = 1.0;
                        }
                    } else if values.len() >= N {
                        out.copy_from_slice(&values[..N]);
                    } else if N == 4 && values.len() == 3 {
                        out[..3].copy_from_slice(&values);
                        out[3] = 1.0;
                    } else if N == 4 && values.len() == 2 {
                        // A vec2 uniform (a post pass's `fn(t)`).
                        out[..2].copy_from_slice(&values);
                    } else {
                        self.fn_error(f, format!("returned {} numbers where {N} are needed", values.len()));
                    }
                    out
                }
                Ok(FnOut::Color(color)) => {
                    if N == 4 && kind.is_none_or(|kind| kind == 4) {
                        std::array::from_fn(|k| color[k])
                    } else {
                        self.fn_error(f, "returned a colour where a number is needed".into());
                        [0.0; N]
                    }
                }
                Err(message) => {
                    self.fn_error(f, message);
                    [0.0; N]
                }
            },
        }
    }

    fn fn_error(&mut self, f: &FnProp, message: String) {
        let mut diag = Diagnostic::error(f.loc, "", format!("property function: {message}"));
        if message.contains("cannot index into pod type Vec") {
            diag = diag.hint("vectors have fields: write `p.x` and `p.y` (not `p[0]`), and compute with them directly (`p - size * 0.5`)");
        }
        if !self.errors.contains(&diag) {
            self.errors.push(diag);
        }
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

    fn trim(&mut self, start: &Prop<1>, end: &Prop<1>, t: f64) -> [f64; 2] {
        let a = self.sample(start, t, 0)[0].clamp(0.0, 1.0);
        let b = self.sample(end, t, 0)[0].clamp(0.0, 1.0);
        [a.min(b), a.max(b)]
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
        prop.as_ref().map(|prop| self.sample(prop, t, 0))
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
        if !layer.visible {
            return None;
        }
        let (lt, origin) = match &layer.span {
            Some(span) => {
                if !(t >= span.start.value && t < span.start.value + span.length.value) {
                    return None;
                }
                (t - span.start.value, origin + span.start.value)
            }
            None => (t, origin),
        };
        let tr = &layer.transform;
        let per_unit = matches!(&layer.kind, LayerKind::Text { by, source, .. } if *by != TextBy::Whole || matches!(source, TextSource::Voice(_)));
        let at = match &tr.along {
            Some(along) => point_along(&along.path, self.sample(&along.progress, lt, 0)[0]),
            None => self.sample(&tr.at, lt, 0),
        };
        let (x, y) = if per_unit { (0.0, 0.0) } else { (self.sample(&tr.x, lt, 0)[0], self.sample(&tr.y, lt, 0)[0]) };
        let world = parent.then(&Affine::translate(at[0] + x, at[1] + y));
        let local = if per_unit {
            Local { anchor: tr.anchor, scale: [1.0, 1.0], rotate: 0.0 }
        } else {
            let scale = self.sample(&tr.scale, lt, 0);
            Local { anchor: tr.anchor, scale: [scale[0], scale[1]], rotate: self.sample(&tr.rotate, lt, 0)[0] }
        };
        let opacity = if per_unit { 1.0 } else { self.sample(&layer.opacity, lt, 0)[0].clamp(0.0, 1.0) };
        let blur = if per_unit { 0.0 } else { self.sample(&layer.blur, lt, 0)[0].max(0.0) };
        let feather = self.sample(&layer.feather, lt, 0)[0].max(0.0);
        let is_static = parent_static && !layer.animated;
        let kind = match &layer.kind {
            LayerKind::Rect { size, radius, fill, stroke, width, trim_start, trim_end, cap } => ItemKind::Rect {
                cap: *cap,
                size: self.sample(size, lt, 0),
                radius: self.sample(radius, lt, 0)[0].max(0.0),
                fill: self.opt4(fill, lt),
                stroke: self.opt4(stroke, lt),
                width: self.sample(width, lt, 0)[0].max(0.0),
                trim: self.trim(trim_start, trim_end, lt),
            },
            LayerKind::Ellipse { size, fill, stroke, width, trim_start, trim_end, cap } => ItemKind::Ellipse {
                cap: *cap,
                size: self.sample(size, lt, 0),
                fill: self.opt4(fill, lt),
                stroke: self.opt4(stroke, lt),
                width: self.sample(width, lt, 0)[0].max(0.0),
                trim: self.trim(trim_start, trim_end, lt),
            },
            LayerKind::Path { data, fill, stroke, width, trim_start, trim_end, cap, join } => {
                let path = match data {
                    PathShape::Static(path) => path.clone(),
                    PathShape::Morph { template, track, shapes } => {
                        // The keyed index, eased: between two outlines.
                        let x = sample_track(track, lt)[0].clamp(0.0, (shapes.len() - 1) as f64);
                        let i = (x.floor() as usize).min(shapes.len().saturating_sub(2));
                        let u = x - i as f64;
                        let (a, b) = (&shapes[i], &shapes[(i + 1).min(shapes.len() - 1)]);
                        template.interpolate(a, b, u).into()
                    }
                };
                ItemKind::Path {
                    path,
                    fill: self.opt4(fill, lt),
                    stroke: self.opt4(stroke, lt),
                    width: self.sample(width, lt, 0)[0].max(0.0),
                    trim: self.trim(trim_start, trim_end, lt),
                    cap: *cap,
                    join: *join,
                }
            }
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
            LayerKind::Group { layers, mask, mask_mode, stagger, size, post, .. } => {
                if !post.0.is_empty() {
                    let at = self.post_at(&post.0, lt);
                    self.group_post.push((layer.path.clone(), at));
                }
                let size = size.as_ref().map(|s| self.sample(s, lt, 0));
                // Children are placed by the group's full transform.
                let group_box = size.unwrap_or([0.0, 0.0]);
                let placed = world.then(&local.affine(group_box));
                let count = layers.len();
                let mut children = Vec::new();
                for (index, child) in layers.iter().enumerate() {
                    let delay = stagger.delay(index, count);
                    if let Some(item) = self.layer(child, lt - delay, origin + delay, &placed, is_static) {
                        children.push(item);
                    }
                }
                let mask = mask.as_ref().and_then(|mask| self.layer(mask, lt, origin, &placed, is_static)).map(Box::new);
                ItemKind::Group { children, mask, mask_mode: *mask_mode, size }
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
            LayerKind::Repeat { layer: template, items, count, layout, delay, step, jitter, seed } => {
                let placed = world.then(&local.affine([0.0, 0.0]));
                let along = match layout {
                    RepeatLayout::Along { path, .. } => Some(Polyline::of(path)),
                    _ => None,
                };
                let outer = self.instance;
                let mut children = Vec::with_capacity(*count);
                for i in 0..*count {
                    let fi = i as f64;
                    let rnd = |k: u32| -> f64 {
                        let mut h = (*seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (i as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ (k as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
                        h ^= h >> 31;
                        h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
                        h ^= h >> 32;
                        (h as f64 / u64::MAX as f64) * 2.0 - 1.0
                    };
                    let (mut pos, mut turn) = match layout {
                        RepeatLayout::Grid { columns, spacing } => ([(i % columns) as f64 * spacing[0], (i / columns) as f64 * spacing[1]], 0.0),
                        RepeatLayout::Radial { radius, start, sweep, face } => {
                            let full = (sweep.abs() - 360.0).abs() < 1e-9;
                            let a = start + sweep * if full { fi / (*count).max(1) as f64 } else { fi / ((*count).max(2) - 1) as f64 };
                            let r = a.to_radians();
                            ([radius * r.sin(), -radius * r.cos()], if *face { a } else { 0.0 })
                        }
                        RepeatLayout::Along { face, .. } => {
                            let path = along.as_ref().unwrap();
                            let u = if *count > 1 { fi / (*count - 1) as f64 } else { 0.0 };
                            let p = path.point(u);
                            let q = path.point((u + 0.001).min(1.0));
                            let p0 = path.point((u - 0.001).max(0.0));
                            let angle = (q[1] - p0[1]).atan2(q[0] - p0[0]).to_degrees();
                            (p, if *face { angle } else { 0.0 })
                        }
                        RepeatLayout::Line => ([0.0, 0.0], 0.0),
                    };
                    pos[0] += step.x * fi + jitter.x * rnd(1);
                    pos[1] += step.y * fi + jitter.y * rnd(2);
                    turn += step.rotate * fi + jitter.rotate * rnd(3);
                    let s = (1.0 + step.scale * fi + jitter.scale * rnd(4)).max(0.0);
                    let o = (1.0 + step.opacity * fi + jitter.opacity * rnd(5)).clamp(0.0, 1.0);
                    let copy = placed.then(&Affine::translate(pos[0], pos[1])).then(&Affine::rotate(turn)).then(&Affine::scale(s, s));
                    let d = delay * fi;
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
                    kind: ItemKind::Rect { size: *size, radius: 0.0, fill: Some([1.0, 1.0, 1.0, 1.0]), stroke: None, width: 0.0, trim: [0.0, 1.0], cap: Cap::Butt },
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
                scene: match scene {
                    // The document's, then the layer's own (a Text3D's look), which win.
                    SceneSource::Template { source, inputs } => SceneSource::Template {
                        source: source.clone(),
                        inputs: {
                            // The values the frame's instance holds (in the VM for the frame).
                            let held = self.doc.vm.with(|e| e.host.host.inputs.clone());
                            self.doc.inputs.iter().map(|decl| decl.name.clone()).zip(held.iter().map(|(_, value)| value.clone())).chain(inputs.iter().cloned()).collect()
                        },
                    },
                    #[cfg(feature = "scene3d")]
                    SceneSource::Object(_) => scene.clone(),
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
                match self.doc.call_draw2d(draw.index, lt, self.edit) {
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
        let LayerKind::Text { source, font, weight, italic, size, color, align, valign, by, stagger, ranges, line_height, tracking, wrap, fit, voice, plate, reveal, caret, path, flicker, only, axes, stroke, .. } = &layer.kind else {
            unreachable!()
        };
        let size = self.sample(size, lt, 0)[0].max(0.0);
        let plain = |this: &mut Self, text: String| -> (String, Vec<TextUnit>) {
            let count = unit_range_iter(&text, *by).count();
            let mut units = Vec::with_capacity(count);
            units.extend(unit_range_iter(&text, *by).enumerate().map(|(i, range)| {
                if *by == TextBy::Whole {
                    // The item carries the text's own animation.
                    TextUnit { range, offset: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0, opacity: 1.0, color: this.sample(color, lt, 0), blur: 0.0, axes: Vec::new() }
                } else {
                    this.unit(layer, range, lt - stagger.delay(i, count), i, color)
                }
            }));
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
        let chars = text.chars().filter(|c| !c.is_whitespace()).count();
        let reveal = reveal.as_ref().map(|r| {
            let r = self.sample(r, lt, 0)[0].max(0.0);
            // A fraction of the characters, or (above 1) a count; spaces
            // count too, as they appear.
            let all = text.chars().count();
            let n = if r <= 1.0 { (r * all as f64).round() as usize } else { r.floor() as usize };
            let _ = chars;
            n.min(all)
        });
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
        ItemKind::Text {
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
        }
    }

    /// One unit's own animation at its clock `ut`.
    fn unit(&mut self, layer: &Layer, range: std::ops::Range<usize>, ut: f64, index: usize, color: &Prop<4>) -> TextUnit {
        let tr = &layer.transform;
        let x = self.sample(&tr.x, ut, index)[0];
        let y = self.sample(&tr.y, ut, index)[0];
        TextUnit {
            range,
            offset: [x, y],
            scale: self.sample(&tr.scale, ut, index),
            rotate: self.sample(&tr.rotate, ut, index)[0],
            opacity: self.sample(&layer.opacity, ut, index)[0].clamp(0.0, 1.0),
            color: self.sample(color, ut, index),
            blur: self.sample(&layer.blur, ut, index)[0].max(0.0),
            axes: Vec::new(),
        }
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

/// The point `progress` (0..1) of the way along SVG path data, by length.
pub fn point_along(data: &str, progress: f64) -> [f64; 2] {
    Polyline::of(data).point(progress)
}

/// A path flattened once, for many points along it (a `Repeat` along a path).
#[derive(Clone, Debug, PartialEq)]
pub struct Polyline {
    pieces: Vec<(Vec<[f64; 2]>, bool)>,
    total: f64,
}

impl Polyline {
    pub fn of(data: &str) -> Self {
        Self::from_path(&crate::geometry::parse_path(data))
    }

    pub fn from_path(path: &makepad_svg::VectorPath) -> Self {
        let pieces = crate::geometry::flatten(path, 0.1);
        let total = pieces.iter().map(|(p, _)| p.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).sum::<f64>()).sum();
        Self { pieces, total }
    }

    pub fn length(&self) -> f64 {
        self.total
    }

    pub fn point(&self, progress: f64) -> [f64; 2] {
        self.sample(progress).0
    }

    /// Arc-length position and unit tangent, including either endpoint.
    pub fn sample(&self, progress: f64) -> ([f64; 2], [f64; 2]) {
        let target = progress.clamp(0.0, 1.0) * self.total;
        let mut walked = 0.0;
        let mut last = ([0.0, 0.0], [1.0, 0.0]);
        for (points, _) in &self.pieces {
            for w in points.windows(2) {
                let delta = [w[1][0] - w[0][0], w[1][1] - w[0][1]];
                let seg = delta[0].hypot(delta[1]);
                if seg == 0.0 { last.0 = w[1]; continue; }
                let tangent = [delta[0] / seg, delta[1] / seg];
                if walked + seg >= target {
                    let u = (target - walked) / seg;
                    return ([w[0][0] + delta[0] * u, w[0][1] + delta[1] * u], tangent);
                }
                walked += seg;
                last = (w[1], tangent);
            }
        }
        last
    }
}

/// Byte ranges of the units `by` splits `text` into. Letters skip
/// whitespace (a space takes no stagger step); words and lines are the
/// runs between whitespace and newlines.
pub fn unit_ranges(text: &str, by: TextBy) -> Vec<std::ops::Range<usize>> {
    unit_range_iter(text, by).collect()
}

/// The same segmentation without an intermediate allocation, for frame
/// units and coarser range selections. Empty lines keep their old behaviour
/// (skipped); a whitespace-only line is still a line.
fn unit_range_iter(text: &str, by: TextBy) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut chars = text.char_indices();
    let mut line_start = 0;
    let mut whole = false;
    std::iter::from_fn(move || match by {
        TextBy::Whole => {
            if whole { return None; }
            whole = true;
            Some(0..text.len())
        }
        TextBy::Letter | TextBy::Syllable => chars.find(|(_, c)| !c.is_whitespace()).map(|(at, c)| at..at + c.len_utf8()),
        TextBy::Word => {
            // A script written without spaces (Chinese, Japanese) is a word
            // per character.
            let (start, first) = chars.find(|(_, c)| !c.is_whitespace())?;
            if crate::typography::is_cjk(first) {
                return Some(start..start + first.len_utf8());
            }
            let rest = &text[start..];
            let end = rest.char_indices().skip(1).find(|(_, c)| c.is_whitespace() || crate::typography::is_cjk(*c)).map_or(text.len(), |(at, _)| start + at);
            while chars.as_str().len() > text.len() - end {
                chars.next();
            }
            Some(start..end)
        }
        TextBy::Line => loop {
            let start = line_start;
            let end = match chars.find(|(_, c)| *c == '\n') {
                Some((at, _)) => { line_start = at + 1; at }
                None => { line_start = text.len(); text.len() }
            };
            if end > start { return Some(start..end); }
            if line_start == text.len() { return None; }
        },
    })
}

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

/// A key track's value at `t` (seconds on its layer's clock).
pub fn sample_track<const N: usize>(track: &Track<N>, t: f64) -> [f64; N] {
    let keys = &track.keys;
    let Some(first) = keys.first() else { return [0.0; N] };
    let mut t = t;
    if let Some(repeat) = &track.repeat {
        let period = repeat.period.value.max(1e-9);
        let start = first.t.value;
        if t > start {
            let elapsed = t - start;
            let cycle = (elapsed / period).floor();
            let within = repeat.count.map_or(true, |count| cycle < count as f64);
            if within {
                let mut u = elapsed - cycle * period;
                if repeat.mode == LoopMode::PingPong && (cycle as i64) % 2 == 1 {
                    u = period - u;
                }
                t = start + u;
            } else {
                // Finished: rest where the last cycle ended.
                let count = repeat.count.unwrap_or(1) as i64;
                t = if repeat.mode == LoopMode::PingPong && count % 2 == 0 { start } else { start + period };
            }
        }
    }
    let last = &keys[keys.len() - 1];
    let (t0, t1) = (first.t.value, last.t.value);
    // Past the keys: the extrapolation.
    if t < t0 || t > t1 {
        let (mode, end) = if t < t0 { (track.before, 0) } else { (track.after, keys.len() - 1) };
        match mode {
            Extrap::Constant => return keys[end].v,
            Extrap::Cycle if t1 > t0 => {
                let span = t1 - t0;
                t = t0 + (t - t0).rem_euclid(span);
            }
            Extrap::Cycle => return keys[end].v,
            Extrap::Linear => {
                let slope = end_slope(keys, end);
                let dt = t - keys[end].t.value;
                return std::array::from_fn(|i| keys[end].v[i] + slope[i] * dt);
            }
        }
    }
    if !(t > first.t.value) {
        return first.v;
    }
    let next = keys.partition_point(|key| key.t.value <= t);
    if next >= keys.len() {
        return keys[keys.len() - 1].v;
    }
    let (a, b) = (&keys[next - 1], &keys[next]);
    let span = b.t.value - a.t.value;
    if b.bez.is_some_and(|h| h.bezier) {
        let (_, out) = key_handles(keys, next - 1);
        let (inn, _) = key_handles(keys, next);
        return std::array::from_fn(|i| bezier_at([a.t.value, a.v[i]], [a.t.value + out.0, a.v[i] + out.1[i]], [b.t.value + inn.0, b.v[i] + inn.1[i]], [b.t.value, b.v[i]], t));
    }
    let u = b.ease.apply((t - a.t.value) / span.max(1e-12), span);
    std::array::from_fn(|i| a.v[i] + (b.v[i] - a.v[i]) * u)
}

/// The slope a track leaves its end key `k` with (per second, each
/// component): its handle's for a bezier end, else its segment's.
fn end_slope<const N: usize>(keys: &[Key<N>], k: usize) -> [f64; N] {
    if keys.len() < 2 {
        return [0.0; N];
    }
    let (a, b) = if k == 0 { (0, 1) } else { (k - 1, k) };
    let bezier = keys[b].bez.is_some_and(|h| h.bezier);
    if bezier {
        let (inn, out) = key_handles(keys, k);
        let h = if k == 0 { out } else { inn };
        if h.0.abs() > 1e-9 {
            return std::array::from_fn(|i| h.1[i] / h.0);
        }
        return [0.0; N];
    }
    let dt = (keys[b].t.value - keys[a].t.value).max(1e-12);
    std::array::from_fn(|i| (keys[b].v[i] - keys[a].v[i]) / dt)
}

/// Key `k`'s handles (in, out) as offsets (seconds, value per component):
/// written ones as written, the rest from the neighbours by its mode. A
/// handle never reaches past the next key in time.
pub fn key_handles<const N: usize>(keys: &[Key<N>], k: usize) -> ((f64, [f64; N]), (f64, [f64; N])) {
    let key = &keys[k];
    let h = key.bez.unwrap_or_else(|| {
        // A key without a record whose next segment is a bezier: smooth.
        KeyHandles::default()
    });
    let dt_prev = if k > 0 { key.t.value - keys[k - 1].t.value } else { 0.0 };
    let dt_next = if k + 1 < keys.len() { keys[k + 1].t.value - key.t.value } else { 0.0 };
    let dv_prev: [f64; N] = std::array::from_fn(|i| if k > 0 { key.v[i] - keys[k - 1].v[i] } else { 0.0 });
    let dv_next: [f64; N] = std::array::from_fn(|i| if k + 1 < keys.len() { keys[k + 1].v[i] - key.v[i] } else { 0.0 });
    let clamp_t = |dt: f64, limit: f64| if limit > 0.0 { dt.clamp(-limit, limit) } else { 0.0 };
    match h.mode {
        HandleMode::Aligned | HandleMode::Free if N == 1 => {
            let inn = h.r#in.unwrap_or([-dt_prev / 3.0, 0.0]);
            let out = h.out.unwrap_or([dt_next / 3.0, 0.0]);
            let it = clamp_t(inn[0].min(0.0), dt_prev);
            let ot = clamp_t(out[0].max(0.0), dt_next);
            // A handle cut short in time keeps its direction.
            let iv = if inn[0].abs() > 1e-12 { inn[1] * it / inn[0] } else { inn[1] };
            let ov = if out[0].abs() > 1e-12 { out[1] * ot / out[0] } else { out[1] };
            ((it, std::array::from_fn(|_| iv)), (ot, std::array::from_fn(|_| ov)))
        }
        HandleMode::Vector => ((-dt_prev / 3.0, std::array::from_fn(|i| -dv_prev[i] / 3.0)), (dt_next / 3.0, std::array::from_fn(|i| dv_next[i] / 3.0))),
        mode => {
            // A slope through the key: the neighbours' (one-sided at the ends).
            let slope: [f64; N] = std::array::from_fn(|i| {
                let s = match (k > 0, k + 1 < keys.len()) {
                    (true, true) => (dv_prev[i] + dv_next[i]) / (dt_prev + dt_next).max(1e-12),
                    (false, true) => dv_next[i] / dt_next.max(1e-12),
                    (true, false) => dv_prev[i] / dt_prev.max(1e-12),
                    (false, false) => 0.0,
                };
                if mode != HandleMode::Clamped {
                    return s;
                }
                // Clamped: flat at a peak or an end, and never past a neighbour.
                let ends = k == 0 || k + 1 == keys.len();
                if ends || dv_prev[i] * dv_next[i] <= 0.0 {
                    return 0.0;
                }
                let lim_prev = 3.0 * dv_prev[i].abs() / dt_prev.max(1e-12);
                let lim_next = 3.0 * dv_next[i].abs() / dt_next.max(1e-12);
                s.signum() * s.abs().min(lim_prev).min(lim_next)
            });
            ((-dt_prev / 3.0, std::array::from_fn(|i| -slope[i] * dt_prev / 3.0)), (dt_next / 3.0, std::array::from_fn(|i| slope[i] * dt_next / 3.0)))
        }
    }
}

/// A cubic bezier through (time, value) points p0..p3, read at time `t`
/// (time runs forward along it).
pub fn bezier_at(p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], t: f64) -> f64 {
    let x = |u: f64| {
        let m = 1.0 - u;
        m * m * m * p0[0] + 3.0 * m * m * u * p1[0] + 3.0 * m * u * u * p2[0] + u * u * u * p3[0]
    };
    let y = |u: f64| {
        let m = 1.0 - u;
        m * m * m * p0[1] + 3.0 * m * m * u * p1[1] + 3.0 * m * u * u * p2[1] + u * u * u * p3[1]
    };
    // Bisection on x(u) = t (x is monotone: the handles stay within the segment).
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if x(mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    y(0.5 * (lo + hi))
}

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
