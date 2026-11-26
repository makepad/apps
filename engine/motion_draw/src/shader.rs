//! Shader layers: an inline `pixel: fn() {…}` in Makepad's shader DSL,
//! compiled at runtime as a subclass of `DrawMotionShaderBase`; render-kit
//! effects by name; and the installed effect pack's effects (bundled
//! presets, KineticText, or library `VjEffect`s; makepad-content-packs)
//! built on the task pool and run on locked time (`set_time`, never
//! integrated). With no pack installed such a preset or effect draws nothing
//! and says what it needs.
//!
//! An inline shader sees `self.pos` (0..1 in its box), `self.time` (the
//! layer's seconds), `self.beat`, `self.size` (its box in pixels), its
//! `params` by name (`self.glow`, each an instance input the draw sets by
//! name), and `self.below` (the frame under it, `input: @below`;
//! `self.below.sample(uv)`), and returns a premultiplied colour;
//! the layer's opacity is applied after.
//!
//! `data: [f, f, …]` on the layer is a flat list of floats the pixel
//! function reads exactly: `self.data_float(i)` is float `i`,
//! `self.data_vec4(i)` floats `4i..4i+3` (`i` within the list; the last
//! texel's unused lanes read zero). For tables too
//! large to write as constants (the shader compiler's cost grows with them):
//! the list is an RGBA32F texture `self.data`, four floats a texel, rows of
//! up to 1024 texels (a power of two, so the row and column of `i` are
//! exact), read at texel centres with `sample_nearest`. It is uploaded once
//! per list (the document's shared `Arc`) and is never part of the shader's
//! text or its cache key: one compile serves any data. Without `data` the
//! layer binds a 1x1 texture of zeros.

use super::{aabb, coeffs, MotionDraw};
use makepad_motion::frame::{Affine, Item, ItemKind};
use makepad_motion::model::{AssetRef, Loc, ShaderProgram};
use makepad_render_graph::pass::PassFn;
use makepad_stage_render_kit::catalog::{by_name as kit_by_name, ParamKind};
use makepad_stage_render_kit::{Anim, Effect as KitEffect, FxContext, Value as KitValue};
use makepad_content_packs::effects::{self as packs, BuiltEffect, EffectView};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[path = "kinetic.rs"]
pub(crate) mod kinetic;

/// Set once any Shader layer asked for a vj_fx preset or library effect
/// (built or not): how a build tool learns whether a film needs the VJ pack.
static VJ_USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a Shader layer in this process has asked for a vj_fx preset or
/// library effect (see `VJ_USED`); a film that never does is built
/// without the VJ effect pack.
pub fn vj_effects_used() -> bool {
    VJ_USED.load(Ordering::Relaxed)
}

script_mod! {
    use mod.prelude.widgets_internal.*

    // Default BGRA8, alpha_blend true, source-over; generated shaders inherit these settings.
    // Canvas/layer targets are single-sample BGRA8 or RGBA16F, cleared to zero or the premultiplied frame background.
    // motion_user_pixel must return premultiplied RGB and coverage alpha; pixel scales both by layer opacity.
    // Coverage composites over earlier colour; nonzero RGB with alpha 0 adds light.
    // Items combine in submission order; canvases add no depth, but an attached/reused depth buffer tests and writes.
    // Earlier draws in this target enter only through blending, never target reads: below is transparent or a completed
    // prior canvas sampled while drawing into a different target, not the current framebuffer.
    // Disabling alpha_blend replaces colour/alpha; declaring a data format also writes raw.
    // Linear-light output decodes premultiplied RGB for over, straight RGB when blending is off; these passes are not MSAA.
    mod.draw.DrawMotionShaderBase = set_type_default() do #(DrawMotionShader::script_shader(vm)){
        ..mod.draw.DrawAffineQuad
        below: texture_2d(float)
        data: texture_2d(float)

        // Texel `i` of the layer's `data` (floats 4i..4i+3).
        data_vec4: fn(i: float) -> vec4 {
            let s = self.data.size()
            let row = floor(i / s.x)
            return self.data.sample_nearest(vec2((i - row * s.x + 0.5) / s.x, (row + 0.5) / s.y))
        }

        // Float `i` of the layer's `data`.
        data_float: fn(i: float) -> float {
            let t = floor(i / 4.0)
            let v = self.data_vec4(t)
            let c = i - t * 4.0
            if c < 0.5 { return v.x }
            if c < 1.5 { return v.y }
            if c < 2.5 { return v.z }
            return v.w
        }

        motion_user_pixel: fn() {
            return vec4(0.0, 0.0, 0.0, 0.0)
        }

        pixel: fn() {
            return self.motion_user_pixel() * self.opacity
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMotionShader {
    #[deref]
    pub draw_super: DrawAffineQuad,
    #[live]
    pub size: Vec2f,
    #[live]
    pub time: f32,
    #[live]
    pub beat: f32,
    #[live]
    pub opacity: f32,
}

/// What an inline shader's fns are adopted under: the shader prelude and
/// the shared Splash stdlib (knob, rot2, palette, sd_*, …).
const INLINE_SCOPE_FILE: &str = "motion://shader_scope";
const INLINE_PRELUDE: &str = "use mod.std.*\nuse mod.pod.*\nuse mod.math.*\nuse mod.shader.*\nuse mod.draw\nuse mod.shared.*\n";

/// Compile an inline Shader layer's `pixel: fn() {…}` (the document's
/// function, its `helpers` and its params' names) the way `MotionDraw`
/// does: `Err` carries the compiler's message. For a publish check (lane
/// A1) or an editor; needs the widgets' and motion's script modules
/// registered (`makepad_motion_draw::script_mod`).
pub fn compile_shader(cx: &mut Cx, source: &PassFn, helpers: &[(String, PassFn)], params: &[String]) -> Result<(), String> {
    match compile_inline(cx, source, helpers, params, None).1 {
        None => Ok(()),
        Some(error) => Err(error),
    }
}

/// The layer's shader as an object of this VM: a `DrawMotionShaderBase`
/// whose `motion_user_pixel` is the document's `pixel` fn and whose members
/// are its `helpers` (both moved here by their tokens), with one instance
/// input per param. No shader text is written.
pub fn inline_shader_object(vm: &mut ScriptVm, source: &PassFn, helpers: &[(String, PassFn)], names: &[String]) -> Result<ScriptObject, String> {
    if makepad_script_compute::module::register_shared_std(vm) {
        makepad_tween_script::install_ease(vm);
    }
    let scope = match vm.end_scope_of(INLINE_SCOPE_FILE) {
        Some(scope) => scope,
        None => {
            vm.capture_errors();
            // (A body whose last item is a glob `use` reads past it: end on a value.)
            vm.eval(ScriptMod::new("", "", INLINE_SCOPE_FILE, 0, 0, &format!("{INLINE_PRELUDE}nil\n"), &[]));
            let errors = vm.take_errors();
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            vm.end_scope_of(INLINE_SCOPE_FILE).ok_or("the shader scope did not evaluate")?
        }
    };
    vm.capture_errors();
    let pixel = vm.adopt_fn(&source.image, scope);
    let members: Vec<(LiveId, Option<ScriptObject>)> = helpers.iter().map(|(name, f)| (LiveId::from_str(name), vm.adopt_fn(&f.image, scope))).collect();
    let errors = vm.take_errors();
    if !errors.is_empty() {
        let mut out = errors.join("; ");
        for f in std::iter::once(source).chain(helpers.iter().map(|(_, f)| f)) {
            out = out.replace(&adopted_file(f), &f.at.file);
        }
        return Err(out);
    }
    let pixel = pixel.ok_or("the shader did not evaluate")?;
    let draw_mod = vm.bx.heap_ref().value(vm.modules(), id!(draw).into(), NoTrap).as_object().ok_or("no mod.draw")?;
    let base = vm.bx.heap_ref().value(draw_mod, id!(DrawMotionShaderBase).into(), NoTrap);
    let shader = vm.bx.heap_mut().new_with_proto(base);
    vm.bx.heap_mut().set_value_def(shader, id!(motion_user_pixel).into(), pixel.into());
    for (name, f) in members {
        let f = f.ok_or("a helper did not evaluate")?;
        vm.bx.heap_mut().set_value_def(shader, name.into(), f.into());
    }
    for name in names {
        let input = vm.bx.heap_mut().new_with_proto(0.0.into());
        vm.bx.heap_mut().set_shader_io(input, makepad_script::mod_shader::SHADER_IO_DYN_INSTANCE);
        vm.bx.heap_mut().set_value_def(shader, LiveId::from_str(name).into(), input.into());
    }
    Ok(shader)
}

/// The module an adopted fn's body is named by (see `ScriptVm::adopt_fn`).
fn adopted_file(f: &PassFn) -> String {
    format!("adopted://{:016x}", f.image.hash())
}

fn compile_inline(cx: &mut Cx, source: &PassFn, helpers: &[(String, PassFn)], names: &[String], origin: Option<&(String, Loc)>) -> (DrawMotionShader, Option<String>) {
    // Edit mode: the pixel function's rows are the document's own, from
    // `origin` on, so its literals compile as slots an editor patches by
    // where they are written (the helpers stay folded).
    if cx.live_literals_on() {
        let origins = match origin {
            Some((file, at)) => vec![LiteralOrigin::new(0, source.text().matches('\n').count() as u32 + 1, 0, file, at.line.saturating_sub(1), at.col.saturating_sub(1))],
            None => Vec::new(),
        };
        cx.set_literal_source(&adopted_file(source), origins);
    }
    let mut draw = cx.with_vm(|vm| DrawMotionShader::script_new_with_default(vm));
    let result = cx.with_vm(|vm| {
        let shader = inline_shader_object(vm, source, helpers, names)?;
        draw.script_apply(vm, &Apply::Eval, &mut Scope::default(), shader.into());
        Ok::<(), String>(())
    });
    let error = match result {
        Err(error) => Some(error),
        Ok(()) => makepad_widgets::makepad_platform::shader_error::take(),
    };
    (draw, error)
}

struct Compiled {
    draw: DrawMotionShader,
    error: Option<String>,
}

/// The install half of a prepared effect, kept while its build runs.
type Install = Box<dyn FnOnce(&mut Cx, BuiltEffect) -> Box<dyn EffectView>>;

enum Effect {
    // (the browser builds on its own thread: never pending there)
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Building { install: Option<Install>, task: TaskHandle<BuiltEffect> },
    Ready(Box<dyn EffectView>),
    Failed(String),
}

#[derive(Default)]
pub(crate) struct Shaders {
    compiled: HashMap<u64, Compiled>,
    /// KineticText layers on kinetic kits.
    pub(crate) kinetic: kinetic::Kinetics,
    effects: HashMap<String, Effect>,
    /// Sources of library VjEffects by revision.
    sources: HashMap<AssetRef, String>,
    /// Effects rendered this frame.
    rendered: Vec<String>,
    errors: Vec<String>,
    /// Inline shaders' `data` textures by list (its address; the list is
    /// held so the address stays its own), dropped once no document holds
    /// the list; and the 1x1 texture a shader without `data` reads.
    data: HashMap<usize, (Arc<[f32]>, Texture)>,
    no_data: Option<Texture>,
}

/// Texels in a row of a `data` texture at most (a power of two).
const DATA_ROW: usize = 1024;

/// `data` as an RGBA32F texture: four floats a texel, rows of a power of
/// two texels (up to `DATA_ROW`), the last row padded with zeros.
fn data_texture(cx: &mut Cx, data: &[f32]) -> Texture {
    let texels = data.len().div_ceil(4).max(1);
    let width = texels.next_power_of_two().min(DATA_ROW);
    let height = texels.div_ceil(width);
    let mut texture = vec![0.0f32; width * height * 4];
    texture[..data.len()].copy_from_slice(data);
    Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width, height, data: Some(texture), updated: TextureUpdated::Full })
}

impl Shaders {
    pub fn begin_frame(&mut self) {
        self.rendered.clear();
        // A list no document or frame holds any more (an edit replaced it).
        self.data.retain(|_, (list, _)| Arc::strong_count(list) > 1);
        if let Some(error) = makepad_widgets::makepad_platform::shader_error::take() {
            self.push_error(format!("shader: {error}"));
        }
    }

    fn push_error(&mut self, error: String) {
        if !self.errors.contains(&error) {
            self.errors.push(error);
        }
    }

    pub fn ready(&self) -> bool {
        !self.effects.values().any(|e| matches!(e, Effect::Building { .. }))
    }

    pub fn errors(&self) -> Vec<String> {
        let mut out = self.errors.clone();
        out.extend(self.kinetic.errors());
        for (path, effect) in &self.effects {
            if let Effect::Failed(error) = effect {
                out.push(format!("{path}: {error}"));
            }
        }
        for compiled in self.compiled.values() {
            if let Some(error) = &compiled.error {
                if !out.contains(error) {
                    out.push(error.clone());
                }
            }
        }
        out
    }

    pub fn units(&self) -> Vec<String> {
        self.rendered.clone()
    }

    pub fn painted(&self, cx: &Cx, units: &[String], recorded: u64) -> bool {
        units.iter().all(|path| match self.effects.get(path) {
            Some(Effect::Ready(view)) => view.live_output_painted(cx, recorded),
            _ => true,
        })
    }

    pub fn set_effect_source(&mut self, effect: AssetRef, source: String) {
        self.sources.insert(effect, source);
    }

    pub fn handle_event(&mut self, cx: &mut Cx, _event: &Event) {
        for effect in self.effects.values_mut() {
            let Effect::Building { install, task } = effect else { continue };
            match task.try_take() {
                None => {}
                Some(Err(error)) => *effect = Effect::Failed(format!("geometry build: {error:?}")),
                Some(Ok(built)) => {
                    let Some(install) = install.take() else { continue };
                    *effect = Effect::Ready(install(cx, built));
                }
            }
        }
    }

    /// Start building the effect of `item` (once) in the installed effect
    /// pack. A name no pack has that is a render-kit effect draws in the kit
    /// (see `kit_effect`); with no pack a preset or library effect is a
    /// problem to report, once.
    fn start(&mut self, cx: &mut Cx, item: &Item) {
        if self.effects.contains_key(&item.path) {
            return;
        }
        let ItemKind::Shader { program, .. } = &item.kind else { return };
        let (pack, source) = match program {
            ShaderProgram::Preset(name) => {
                let bare = name.strip_prefix("vjfx/").unwrap_or(name);
                match packs::resolve(bare) {
                    Some((pack, _, source)) => (pack, source.to_string()),
                    // Not a pack's preset: the render kit's effect of that
                    // name draws it (see `kit_effect`).
                    None if kit_by_name(name.strip_prefix("kit/").unwrap_or(bare)).is_some() => return,
                    None if makepad_content_packs::effect_packs().is_empty() => {
                        VJ_USED.store(true, Ordering::Relaxed);
                        self.effects.insert(item.path.clone(), Effect::Failed(packs::missing(&format!("the vj_fx preset `{bare}`"))));
                        return;
                    }
                    None => {
                        self.effects.insert(item.path.clone(), Effect::Failed(format!("`{name}` is neither a bundled vj_fx preset nor a render-kit effect")));
                        return;
                    }
                }
            }
            ShaderProgram::Effect(asset) => {
                let Some(pack) = packs::library() else {
                    VJ_USED.store(true, Ordering::Relaxed);
                    self.effects.insert(item.path.clone(), Effect::Failed(packs::missing(&format!("the library effect {}", asset.id))));
                    return;
                };
                match self.sources.get(asset) {
                    Some(source) => (pack, source.clone()),
                    // Not given yet: the host lists it among the needs.
                    None => return,
                }
            }
            ShaderProgram::Inline { .. } => return,
        };
        VJ_USED.store(true, Ordering::Relaxed);
        let prepared = match pack.prepare(cx, &format!("motion/{}", item.path), &source) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.effects.insert(item.path.clone(), Effect::Failed(error));
                return;
            }
        };
        // In the browser on this thread: a worker there could hold a lock
        // the page's main thread then may not wait on.
        #[cfg(target_arch = "wasm32")]
        {
            let built = (prepared.build)();
            self.effects.insert(item.path.clone(), Effect::Ready((prepared.install)(cx, built)));
        }
        #[cfg(not(target_arch = "wasm32"))]
        match cx.task_pool().submit(Class::Background, prepared.build) {
            Ok(task) => {
                self.effects.insert(item.path.clone(), Effect::Building { install: Some(prepared.install), task });
            }
            Err(error) => {
                self.effects.insert(item.path.clone(), Effect::Failed(format!("geometry build: {error:?}")));
            }
        }
    }

    /// Whether `item`'s effect still needs its library source.
    pub fn needs_source(&self, item: &Item) -> Option<AssetRef> {
        match &item.kind {
            ItemKind::Shader { program: ShaderProgram::Effect(asset), .. } if !self.sources.contains_key(asset) => Some(asset.clone()),
            _ => None,
        }
    }
}

impl MotionDraw {
    /// A Shader layer naming a render-kit effect instead of a vj_fx preset.
    fn kit_effect(&mut self, cx: &mut Cx2d, item: &Item, input: &Texture, px: (u32, u32)) -> Option<Texture> {
        let ItemKind::Shader { program: ShaderProgram::Preset(name), params, local_time, .. } = &item.kind else { return None };
        let bare = name.strip_prefix("kit/").or_else(|| name.strip_prefix("vjfx/")).unwrap_or(name);
        if name.starts_with("vjfx/") && packs::resolve(bare).is_some() {
            return None;
        }
        let info = kit_by_name(bare)?;
        let mut values: Vec<(&str, Anim)> = Vec::new();
        let numbers = info.params.iter().filter_map(|p| match p.kind {
            ParamKind::Num { min, max, .. } => Some((p.name, min, max)),
            _ => None,
        });
        let dials: Vec<f64> = (1..=4).map(|k| params.iter().find(|(n, _)| *n == format!("p{k}")).map_or(-1.0, |(_, v)| *v)).collect();
        for ((param, min, max), dial) in numbers.zip(dials) {
            if dial >= 0.0 {
                values.push((param, Anim::Const(KitValue::Num(min + (max - min) * dial.clamp(0.0, 1.0)))));
            }
        }
        let effect = match KitEffect::build(bare, values) {
            Ok((effect, _)) => effect,
            Err(diagnostics) => {
                let error = format!("{}: `{name}`: {:?}", item.path, diagnostics.errors);
                if !self.shaders.errors.contains(&error) {
                    self.shaders.errors.push(error);
                }
                return None;
            }
        };
        self.note_kind(&format!("kit {bare}"));
        let layers = HashMap::new();
        let applied = self.kit.apply(cx, &[effect.at(*local_time)], input, px, &FxContext::new(*local_time, 30.0, &layers));
        if let (Some(first), Some(last)) = (applied.first, applied.last) {
            self.spans.push((first, last));
        }
        Some(applied.texture)
    }

    /// A library VjEffect's source, for Shader layers that name one.
    pub fn set_effect_source(&mut self, effect: AssetRef, source: String) {
        self.shaders.set_effect_source(effect, source);
    }

    /// Compile (once) and draw an inline shader over its box.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn inline_shader(&mut self, cx: &mut Cx2d, item: &Item, m: &Affine, size: [f64; 2], px: [f64; 2], below: &Texture, opacity: f64) {
        let ItemKind::Shader { program: ShaderProgram::Inline { source, helpers, origin, .. }, params, local_time, beat, data, .. } = &item.kind else { return };
        let names: Vec<String> = params.iter().map(|(name, _)| name.clone()).collect();
        let mut key = LiveId(source.image.hash()).bytes_append(names.join(",").as_bytes());
        // Edit mode compiles a shader for where its text is written (its
        // literals' sites): moved, it is another compile.
        let origin = if cx.live_literals_on() { origin.clone() } else { None };
        if let Some((file, at)) = &origin {
            key = key.bytes_append(file.as_bytes()).bytes_append(&at.line.to_le_bytes()).bytes_append(&at.col.to_le_bytes());
        }
        for (name, f) in helpers.iter() {
            key = key.bytes_append(name.as_bytes()).bytes_append(&f.image.hash().to_le_bytes());
        }
        let key = key.0;
        if !self.shaders.compiled.contains_key(&key) {
            let (draw, error) = compile_inline(cx, source, helpers, &names, origin.as_ref());
            let error = error.map(|e| format!("{}: pixel: {e}", item.path));
            self.shaders.compiled.insert(key, Compiled { draw, error });
        }
        self.note_kind(&format!("inline {key:016x}"));
        let compiled = self.shaders.compiled.get_mut(&key).unwrap();
        if compiled.error.is_some() {
            return;
        }
        // The layer's `data` (uploaded once per list), or the empty one.
        let data = match data {
            Some(list) => &self.shaders.data.entry(Arc::as_ptr(&list.0) as *const f32 as usize).or_insert_with(|| (list.0.clone(), data_texture(cx.cx, &list.0))).1,
            None => self.shaders.no_data.get_or_insert_with(|| data_texture(cx.cx, &[])),
        };
        let compiled = self.shaders.compiled.get_mut(&key).unwrap();
        let d = &mut compiled.draw;
        d.draw_vars.set_texture(0, below);
        d.draw_vars.set_texture(1, data);
        d.set_affine(coeffs(m), dvec2(size[0], size[1]));
        d.size = vec2(px[0] as f32, px[1] as f32);
        d.time = *local_time as f32;
        d.beat = *beat as f32;
        d.opacity = opacity as f32;
        for (name, value) in params {
            d.draw_vars.set_dyn_instance(cx, LiveId::from_str(name), &[*value as f32]);
        }
        let b = aabb(m, size);
        d.draw_abs(cx, Rect { pos: dvec2(b[0], b[1]), size: dvec2((b[2] - b[0]).max(1.0), (b[3] - b[1]).max(1.0)) });
    }

    /// Run `item`'s vj_fx effect at its time over `input`, into a target
    /// of `px`: the effect's picture, or `None` while it builds. A name
    /// that is not a vj_fx preset but a render-kit effect (`grain`,
    /// `vignette`, … or `kit/<name>`) runs in the kit, p1..p4 setting its
    /// first four number parameters across their ranges.
    pub(super) fn effect(&mut self, cx: &mut Cx2d, item: &Item, input: Texture, px: (u32, u32)) -> Option<Texture> {
        // A KineticText draws on its kinetic kit.
        if let Some(out) = self.kinetic_effect(cx, item, &input, px) {
            return out;
        }
        if let Some(texture) = self.kit_effect(cx, item, &input, px) {
            return Some(texture);
        }
        self.shaders.start(cx, item);
        self.vj_effect(cx, item, input, px)
    }

    /// `input` (linear) encoded sRGB into an f16 target of `px`.
    fn encode_srgb(&mut self, cx: &mut Cx2d, input: &Texture, px: (u32, u32)) -> Texture {
        let index = self.target(cx, px, "motion_effect_input");
        self.pool[index].begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        self.draws.quad.decode = 2.0;
        self.draws.quad.quad(cx, input, None, &Affine::IDENTITY, [px.0 as f64, px.1 as f64], [0.0, 0.0, 1.0, 1.0], 1.0);
        self.draws.quad.decode = 0.0;
        self.pool[index].end(cx);
        self.record(self.pool[index].pass.draw_pass_id());
        self.pool[index].texture.clone()
    }

    /// A pack effect's picture (see [`Self::effect`]).
    fn vj_effect(&mut self, cx: &mut Cx2d, item: &Item, input: Texture, px: (u32, u32)) -> Option<Texture> {
        let ItemKind::Shader { params, local_time, beat, audio, below, .. } = &item.kind else { return None };
        // The song's picture at this frame's time, when the host gave one.
        let sound = self.audio_frame(cx.cx, item);
        let Some(Effect::Ready(_)) = self.shaders.effects.get(&item.path) else { return None };
        // In an HDR frame the effect reads its input sRGB-encoded (f16, the
        // range above 1 kept) and gives linear light back.
        let hdr = self.hdr;
        let input = if hdr && *below { self.encode_srgb(cx, &input, px) } else { input };
        let Some(Effect::Ready(view)) = self.shaders.effects.get_mut(&item.path) else { return None };
        view.set_linear_output(hdr);
        // Only an effect of what is under it gets content: a bound input
        // makes a generator dim itself to show the content (vj_fx's content
        // backdrop), and a generator's content is nothing.
        view.set_input_texture(0, below.then_some(input));
        view.set_input_texture(1, None);
        set_layer_dials(view.as_mut(), params);
        // The music's levels at this moment (zero without music).
        view.set_signals(audio.map(|level| level.clamp(0.0, 1.0) as f32));
        if sound.is_some() {
            view.set_audio(sound);
        }
        view.set_speed_scale(1.0);
        view.set_time(cx, *local_time, *beat, 1.0 / 60.0);
        self.note_kind(&format!("effect {}", item.path));
        let index = self.target(cx, px, "motion_effect");
        let target = &mut self.pool[index];
        target.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        let pixels = dvec2(px.0 as f64, px.1 as f64);
        let Some(Effect::Ready(view)) = self.shaders.effects.get_mut(&item.path) else { return None };
        let output = view.render_live(cx, pixels);
        if let Some(texture) = &output {
            let full = [px.0 as f64, px.1 as f64];
            self.draws.quad.quad(cx, texture, None, &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], 1.0);
        }
        let target = &mut self.pool[index];
        target.end(cx);
        let id = target.pass.draw_pass_id();
        let first = match self.shaders.effects.get(&item.path) {
            Some(Effect::Ready(view)) => view.live_passes().map_or(id, |(first, _)| first),
            _ => id,
        };
        self.spans.push((first, id));
        self.passes.push(id);
        self.shaders.rendered.push(item.path.clone());
        Some(self.pool[index].texture.clone())
    }
}

/// A SHADER LAYER'S DIALS ON A VJ_FX EFFECT, as the live card drives them
/// (`VjFxView::set_card_dials`): `p1`..`p3` are the document's first three
/// dials in its list order, whatever slots they bind, and `p4` is p3, the
/// host's lever. Below 0 (or unset) leaves a dial to the document. So a
/// Motion layer, an edit and the VJ card mean the same dial by P1..P3, as
/// a KineticText's `p1`.. are its kit's dials in order.
fn set_layer_dials(view: &mut dyn EffectView, params: &[(String, f64)]) {
    let mut dials = [None; 4];
    for (name, value) in params {
        if let Some(k) = name.strip_prefix('p').and_then(|n| n.parse::<usize>().ok()).filter(|k| (1..=4).contains(k)) {
            dials[k - 1] = (*value >= 0.0).then_some(*value as f32);
        }
    }
    view.set_card_dials(&dials[..3], dials[3]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view that keeps the dials its host set.
    #[derive(Default)]
    struct Dials {
        card: Vec<Option<f32>>,
        lever: Option<f32>,
    }

    impl EffectView for Dials {
        fn set_linear_output(&mut self, _: bool) {}
        fn wants_deck_inputs(&self) -> bool {
            false
        }
        fn set_input_texture(&mut self, _: usize, _: Option<Texture>) {}
        fn set_card_dials(&mut self, card: &[Option<f32>], lever: Option<f32>) {
            (self.card, self.lever) = (card.to_vec(), lever);
        }
        fn set_signals(&mut self, _: [f32; 4]) {}
        fn set_audio(&mut self, _: Option<makepad_audio_reactive::AudioFrame>) {}
        fn set_speed_scale(&mut self, _: f32) {}
        fn set_bpm(&mut self, _: f64) {}
        fn set_time(&mut self, _: &mut Cx, _: f64, _: f64, _: f64) {}
        fn render_live(&mut self, _: &mut Cx2d, _: DVec2) -> Option<Texture> {
            None
        }
        fn live_passes(&self) -> Option<(DrawPassId, DrawPassId)> {
            None
        }
        fn live_output_painted(&self, _: &Cx, _: u64) -> bool {
            true
        }
        fn live_output_exact(&self) -> bool {
            true
        }
        fn user_override(&self) -> Vec<Option<f32>> {
            Vec::new()
        }
    }

    /// A MOTION LAYER DRIVES THE CARD'S DIALS: `p1`..`p3` are the card's
    /// three dials and `p4` the lever, below 0 leaves a dial to the
    /// document, and other parameters (`speed`) are not dials. (Which of
    /// a document's dials the card's reach is the effect pack's.)
    #[test]
    fn a_shader_layer_drives_the_card_dials_and_p4_the_lever() {
        let mut view = Dials::default();
        let params: Vec<(String, f64)> = [("p3", 0.4), ("p1", 0.9), ("p2", -1.0), ("p4", 0.7), ("speed", 2.0)].iter().map(|(n, v)| (n.to_string(), *v)).collect();
        set_layer_dials(&mut view, &params);
        assert_eq!((view.card.as_slice(), view.lever), (&[Some(0.9), None, Some(0.4)][..], Some(0.7)), "p1, p2 left, p3; p4 the lever");
        set_layer_dials(&mut view, &[("p2".to_string(), 0.5)]);
        assert_eq!((view.card.as_slice(), view.lever), (&[None, Some(0.5), None][..], None), "an unset dial and an unset p4 leave theirs to the document");
    }
}
