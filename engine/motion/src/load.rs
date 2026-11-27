//! `MotionDoc::load`: evaluate a document and read it into the typed model,
//! collecting every problem with the place it was written and a fix.
//!
//! Diagnostics are written for the agents that author these documents as
//! much as for people: each names the property (`layers[2].opacity`), says
//! what it got and what it takes, and where a name is misspelt, suggests
//! the nearest ones.

use crate::plain::{GroupProps, PathProps, RepeatProps, TextProps};
use crate::Ease;
use crate::model::*;
use crate::timing::{parse_time_fields, Length, TimeArg, TimeExpr, TimeValue};
use crate::obj::{self, Look};
use crate::value::{evaluate, fmt_num, SourceMap};
use makepad_script::*;
use std::sync::Arc;

/// Text3D layers (lane T3).
mod document;
mod fields;
/// Other tools' words for our fields, in errors only.
mod foreign;
pub(crate) use foreign::how as field_how;
mod blend;
mod group;
mod rig;
mod crowd;
mod scene3d;
mod widget;
mod media;
mod shader;
mod shapes;
mod text;
mod text3d;
/// Character layers (lane CH1).
mod character;
/// Seq3D layers (lane R3): ready-made 3D sequences lowered by motion3d.
#[cfg(feature = "scene3d")]
mod seq3d;

/// The nearest of `names` to `name`, as ` Did you mean …?`.
pub(crate) fn did_you_mean(name: &str, names: &[&str]) -> String {
    let near: Vec<String> = makepad_script::suggest::nearest(name, names.iter().copied(), 2).into_iter().map(|n| format!("`{n}`")).collect();
    if near.is_empty() {
        String::new()
    } else {
        format!(" Did you mean {}?", near.join(" or "))
    }
}

impl MotionDoc {
    /// Load a document with its inputs' defaults and no host context.
    pub fn load(source: &str) -> Result<MotionDoc, Vec<Diagnostic>> {
        Self::load_with(source, &[], MotionContext::default())
    }

    /// Load with `params` filling declared inputs (an edit's
    /// `Graphic{params: …}`) and the host's timing and audio features.
    /// Errors come back sorted by position; warnings stay in
    /// [`MotionDoc::warnings`].
    pub fn load_with(source: &str, params: &[(LiveId, InputValue)], context: MotionContext) -> Result<MotionDoc, Vec<Diagnostic>> {
        let source = context.modules.0.document_file().map_or_else(|| DocSource::file(source), |file| DocSource::File(file.into(), source.into()));
        Self::load_source(source, params, context)
    }

    /// Load the document a function of a game's script is: its body is
    /// evaluated in the document's VM (`adopt_fn`, no text is read again) and
    /// gives the `Motion{…}`. It reads game values only through its declared
    /// inputs; a name of the game's scope is an error naming the game's file
    /// and line.
    pub fn load_image(image: &ScriptFnImage, params: &[(LiveId, InputValue)], context: MotionContext) -> Result<MotionDoc, Vec<Diagnostic>> {
        Self::load_source(DocSource::Image(Arc::new(image.clone())), params, context)
    }

    pub(crate) fn load_source(source: DocSource, params: &[(LiveId, InputValue)], context: MotionContext) -> Result<MotionDoc, Vec<Diagnostic>> {
        crate::font_spec::font_roots();
        let context = Arc::new(context);
        // `in(@name)` gives the params' values. Where the document asked for
        // an input whose value is not the one it was given (a declared
        // default, a param of another type), it is evaluated again with the
        // inputs' values, so a document computes with them (`in(@size) *
        // 0.5`); one that never asks evaluates once.
        let mut evaluated = evaluate(&source, context.signals.clone(), context.offset, params, &*context.modules.0, context.lang.clone())?;
        let root = evaluated.root;
        let values = input_values(&mut evaluated, root, params);
        let asked = evaluated.host.host.asked.take().unwrap_or_default();
        let mut structural: Vec<LiveId> = asked.iter().map(|(id, _)| *id).collect();
        if !is_exact(&asked, &values) {
            drop(evaluated);
            evaluated = evaluate(&source, context.signals.clone(), context.offset, &values, &*context.modules.0, context.lang.clone())?;
            if let Some(again) = evaluated.host.host.asked.take() {
                structural.extend(again.iter().map(|(id, _)| *id));
            }
        }
        // Property functions read the inputs' values.
        evaluated.host.host.inputs = Arc::new(values);
        let errors = std::mem::take(&mut evaluated.errors);
        let mut diags = doc_diagnostics(&mut evaluated, &errors);
        evaluated.errors = errors;
        diags.extend(std::mem::take(&mut evaluated.diags));
        let (root, map) = (evaluated.root, evaluated.map.clone());
        let vm = Arc::new(crate::value::DocVm::new(evaluated));
        let literals = match &source {
            DocSource::File(_, text) => crate::schema::DocLiterals::register(text, &vm),
            // An inline document is the game's code: no live literals.
            DocSource::Image(_) => None,
        };
        let doc = compile(source, params, context, vm, literals, root, &map, (Vec::new(), Vec::new()), &structural, &mut diags);
        finish(doc, diags)
    }

    /// The model read again from the running document's VM, as loaded and as
    /// patched since (`Cx::patch_literal`): what a layer's static fields
    /// copied from a value at load (a `fill: look.BONE`) now says, without
    /// evaluating anything again. Cheap next to a load; an editor does it
    /// after a patch reached the document.
    pub fn reread(&self) -> Result<MotionDoc, Vec<Diagnostic>> {
        let mut diags = Vec::new();
        let params: Vec<(LiveId, InputValue)> = self.slots();
        let structural: Vec<LiveId> = self.inputs.iter().zip(&self.structural).filter(|(_, s)| **s).map(|(decl, _)| decl.id).collect();
        let map = self.vm.with(|e| e.map.clone());
        // An editor's document reads as one (its live literals' places).
        let read = |diags: &mut Vec<Diagnostic>| compile(self.source.clone(), &params, self.context.clone(), self.vm.clone(), self._literals.clone(), self.run.root, &map, (Vec::new(), Vec::new()), &structural, diags);
        let mut doc = match &self._literals {
            Some(literals) => crate::value::editable_as(literals.file(), || read(&mut diags)),
            None => read(&mut diags),
        };
        if let Some(run) = Arc::get_mut(&mut doc.run) {
            run.keep = Some(self.run.clone());
        }
        finish(doc, diags)
    }

    /// The same document with other input values (and the same context),
    /// loaded again from its source in a VM of its own: what an edit does.
    /// A host that changes inputs of a running document writes them into an
    /// [`InstanceInputs`] ([`MotionDoc::write_input`]) and evaluates with
    /// them ([`MotionDoc::eval_with`]).
    pub fn with_inputs(&self, params: &[(LiveId, InputValue)]) -> Result<MotionDoc, Vec<Diagnostic>> {
        Self::load_source(self.source.clone(), params, (*self.context).clone())
    }

    /// The document evaluated again for the values `inputs` holds, for a
    /// write that [`MotionDoc::write_input`] said needs it (an input read
    /// while the document evaluates, deciding its structure). The new
    /// document shares this one's VM when it can: nothing is parsed again
    /// (an inline document's function is already adopted, a file's body
    /// already read) and the VM keeps its prelude and modules. A document
    /// whose VM holds state of its evaluation (kernels, live lets, an editor's
    /// literals) is loaded in a VM of its own.
    pub fn reevaluate(&self, inputs: &InstanceInputs) -> Result<MotionDoc, Vec<Diagnostic>> {
        let params: &[(LiveId, InputValue)] = &inputs.slots;
        if !self.vm.with(|e| e.reusable) {
            let prior = self.evaluations();
            let doc = Self::load_source(self.source.clone(), params, (*self.context).clone())?;
            doc.vm.with(|e| e.evaluations += prior);
            return Ok(doc);
        }
        let vm = self.vm.clone();
        // No frame runs in the VM while it is evaluated again, and the
        // inputs the VM had are back when that is done: the evaluation's
        // values are for the loader only. (The turn is released before
        // `finish`, which evaluates frames itself.)
        struct Restore<'a>(&'a crate::value::DocVm, Option<Arc<Vec<(LiveId, InputValue)>>>);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                if let Some(was) = self.1.take() {
                    self.0.with(|e| e.host.host.inputs = was);
                }
            }
        }
        let (doc, diags) = {
            let _turn = vm.turn();
            let _restore = Restore(&vm, Some(vm.with(|e| e.host.host.inputs.clone())));
            let (mut ran, structural, map) = vm.with(|e| {
                let mut ran = e.rerun(Arc::new(params.to_vec()));
                let values = input_values(e, ran.root, params);
                let asked = e.host.host.asked.take().unwrap_or_default();
                let mut structural: Vec<LiveId> = asked.iter().map(|(id, _)| *id).collect();
                if !is_exact(&asked, &values) {
                    ran = e.rerun(Arc::new(values.clone()));
                    if let Some(again) = e.host.host.asked.take() {
                        structural.extend(again.iter().map(|(id, _)| *id));
                    }
                }
                (ran, structural, e.map.clone())
            });
            let mut diags = vm.with(|e| doc_diagnostics(e, &ran.errors));
            diags.extend(std::mem::take(&mut ran.diags));
            let doc = compile(self.source.clone(), params, self.context.clone(), vm.clone(), None, ran.root, &map, (std::mem::take(&mut ran.held), std::mem::take(&mut ran.array_held)), &structural, &mut diags);
            (doc, diags)
        };
        finish(doc, diags)
    }
}

/// Whether the values each `in(@…)` asked for are the ones the declared
/// inputs hold (else the document computed with a wrong one).
fn is_exact(asked: &[(LiveId, Option<InputValue>)], values: &[(LiveId, InputValue)]) -> bool {
    asked.iter().all(|(id, given)| {
        let value = values.iter().find(|(n, _)| n == id).map(|(_, v)| v).filter(|v| !matches!(v, InputValue::Empty));
        value == given.as_ref()
    })
}

/// The VM's errors as the document's diagnostics, each where it was written.
fn doc_diagnostics(evaluated: &mut crate::value::Evaluated, errors: &[ScriptErrorRecord]) -> Vec<Diagnostic> {
    let map = evaluated.map.clone();
    let messages: Vec<String> = errors.iter().map(|error| error.message().to_owned()).collect();
    let vocabulary: Vec<Option<String>> = evaluated.with_vm(|vm| {
        // a hint looks in every vocabulary: the lazy ones are made (an error path only)
        if !messages.is_empty() {
            vm.expand_all_lazy();
        }
        messages.iter().map(|message| vocabulary_hint(vm, message)).collect()
    });
    errors.iter().enumerate().map(|(k, error)| {
        // Where the document wrote it (an error in a module or the
        // prelude is the document's as a whole).
        let (loc, message) = match error.loc() {
            Some(l) if map.owns(l.file()) => (map.map(l.line(), l.col()), error.message().to_owned()),
            // In a module: the document as a whole, the message naming the module's place.
            Some(l) => (map.home, format!("{l}: {}", error.message())),
            None => (map.home, error.message().to_owned()),
        };
        // A name of a vocabulary the document did not import: the
        // VM's spelling guesses are noise next to the `use` it needs.
        let message = if vocabulary[k].is_some() { message.split(". Did you mean").next().unwrap_or(&message).to_string() } else { message };
        // Another tool's word for one of the type's fields: the error
        // names ours (the spelling guesses could not find it).
        let message = foreign_property(&message).unwrap_or(message);
        // An inline document is evaluated apart from the game that wrote it.
        let hint = if vocabulary[k].is_some() {
            vocabulary[k].clone()
        } else if map.file != crate::value::DOC_FILE && message.contains("not found in scope") {
            Some("an inline document sees the motion vocabulary and its own names, not the game's: pass the value in as an input (`inputs: {name: …}`, read with `in(@name)`)".to_string())
        } else {
            vm_hint(&message)
        };
        let diag = Diagnostic::error(loc, "", message);
        match hint {
            Some(hint) => diag.hint(hint),
            None => diag,
        }
    }).collect()
}

fn finish(doc: MotionDoc, mut diags: Vec<Diagnostic>) -> Result<MotionDoc, Vec<Diagnostic>> {
    let mut doc = doc;
    // An inline document's problems are in the game's script.
    if let DocSource::Image(image) = &doc.source {
        let file: Arc<str> = image.file().into();
        for diag in &mut diags {
            diag.file = Some(file.clone());
        }
    }
    diags.sort_by_key(|diag| (diag.loc, diag.severity));
    // A problem met many times is said once, where first met: a module's
    // errors all sit at the document's home, so the same few interleave
    // (a function failing per word of a lyric) and are not neighbours.
    let mut seen = std::collections::HashSet::new();
    diags.retain(|diag| seen.insert((diag.loc, diag.severity, diag.file.clone(), diag.path.clone(), diag.message.clone(), diag.hint.clone())));
    if diags.iter().any(Diagnostic::is_error) {
        return Err(diags);
    }
    doc.warnings.extend(diags);
    // Musical and spoken time: resolved against the context now, so eval
    // reads plain seconds.
    let context = doc.context.clone();
    let warnings = crate::resolve::resolve_times(&mut doc, &context);
    doc.warnings.extend(warnings);
    doc.warnings.sort_by_key(|diag| diag.loc);
    // A post uniform's `fn(t)` giving a vector is a vector uniform.
    doc.widen_fn_uniforms();
    // The render kit checks every effect and the camera now.
    let effects = crate::render_effects::check_effects(&doc);
    if !effects.is_empty() {
        return Err(effects);
    }
    Ok(doc)
}

/// A teaching hint for a VM error message.
/// A name the document did not import (`variable Mesh not found in scope`)
/// that one of the vocabulary modules has: which one, and the `use` line that
/// brings it (a document from before modules says so instead of drawing
/// nothing).
fn vocabulary_hint(vm: &makepad_script::ScriptVm, message: &str) -> Option<String> {
    use makepad_script::*;
    let name = message.strip_prefix("variable ")?.split(" not found in scope").next()?;
    if name.is_empty() || message.len() == name.len() + "variable ".len() {
        return None;
    }
    let id = LiveId::from_str(name);
    let modules = vm.modules();
    for vocab in crate::value::VOCABULARIES {
        let Some(module) = vm.bx.heap_ref().value(modules, LiveId::from_str(vocab).into(), NoTrap).as_object() else { continue };
        let v = vm.bx.heap_ref().value(module, id.into(), NoTrap);
        if v.is_nil() || v.is_err() {
            continue;
        }
        return Some(match *vocab {
            "motion" => format!("`{name}` is in mod.motion: a Motion document starts with `use mod.motion.*`"),
            other => format!("`{name}` is in mod.{other}: add `use mod.{other}` and write `{other}.{name}` (or `use mod.{other}.*` when it is the file's main vocabulary)"),
        });
    }
    None
}

/// `property position not defined on type Rect…` as the error that names
/// the field another tool's word means ([`foreign`]).
fn foreign_property(message: &str) -> Option<String> {
    let (word, rest) = message.strip_prefix("property ")?.split_once(" not defined on type ")?;
    let ty = rest.split(|c: char| !c.is_alphanumeric()).next()?;
    foreign::message(word, ty)
}

fn vm_hint(message: &str) -> Option<String> {
    // `property fil not defined on type Rect. …` (or the foreign word's
    // `Rect has no `position`; …`): the type's fields.
    if let Some(rest) = message.split_once(" not defined on type ").map(|(_, rest)| rest).or_else(|| message.split_once(" has no `").map(|(ty, _)| ty)) {
        let kind = rest.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
        let fields = if kind == "Motion" { crate::layers::document_fields() } else { crate::layers::fields(kind) };
        if !fields.is_empty() {
            return Some(format!("{kind} fields: {}", fields.join(", ")));
        }
    }
    if message.contains("got id") {
        return Some("an @id is not a function: write `spring(170, 26)`, not `@spring(170, 26)`".into());
    }
    if message.contains("not found in scope") {
        return Some("layer types, helpers (tween, keys, sequence, loop, stagger, spring, in, asset, beat, bar, …) and your own `let` names are in scope; check the spelling".into());
    }
    if message.contains("ForExpr") || message.contains("Expected expression after") {
        return Some("`loop` is a Splash keyword: repeat an animation with `repeat(tween(…), period, @restart | @pingpong)`".into());
    }
    if message.contains("instruction limit") || message.contains("instructions") {
        return Some("the document ran too long while evaluating: look for a loop that never ends".into());
    }
    None
}

/// Give the Text at `text` the path data of the Path named `name` (or, in
/// the Chart or Group named `name`, its series line `line <series>`, the
/// first one without a series), moved from that layer's place to the
/// Text's own coordinates (the `at`, `x`, `y` of both and their parents).
fn bind_text_path(layers: &mut [Layer], text: &str, name: &str, series: Option<&str>) -> Result<(), String> {
    fn offset(layer: &Layer) -> [f64; 2] {
        let c1 = |p: &Prop<1>| match p { Prop::Const([v]) => *v, _ => 0.0 };
        let at = match &layer.transform.at { Prop::Const(v) => *v, _ => [0.0, 0.0] };
        [at[0] + c1(&layer.transform.x), at[1] + c1(&layer.transform.y)]
    }
    fn children(layer: &Layer) -> &[Layer] {
        match &layer.kind {
            LayerKind::Group { layers, .. } | LayerKind::FlyThrough { layers, .. } => layers,
            LayerKind::Bones(rig) => &rig.layers,
            _ => &[],
        }
    }
    // The path's geometry and its world offset.
    fn find_path(layers: &[Layer], name: &str, series: Option<&str>, base: [f64; 2], inside: bool) -> Option<(Arc<makepad_svg::VectorPath>, [f64; 2])> {
        for layer in layers {
            let o = offset(layer);
            let here = [base[0] + o[0], base[1] + o[1]];
            let named = layer.name.as_deref() == Some(name);
            if let LayerKind::Path(PathProps { data: PathShape::Static(data), .. }) = &layer.kind {
                let is_series = layer.name.as_deref().is_some_and(|n| match series {
                    Some(s) => n == format!("line {s}"),
                    None => n.starts_with("line "),
                });
                if (named && !inside) || (inside && is_series) {
                    return Some((data.clone(), here));
                }
            }
            if named && matches!(layer.kind, LayerKind::Group { .. }) {
                if let Some(found) = find_path(children(layer), name, series, here, true) {
                    return Some(found);
                }
            }
            if let Some(found) = find_path(children(layer), name, series, here, inside) {
                return Some(found);
            }
        }
        None
    }
    fn text_offset(layers: &[Layer], path: &str, base: [f64; 2]) -> Option<[f64; 2]> {
        for layer in layers {
            let o = offset(layer);
            let here = [base[0] + o[0], base[1] + o[1]];
            if layer.path == path {
                return Some(here);
            }
            if path.starts_with(&layer.path) {
                if let Some(found) = text_offset(children(layer), path, here) {
                    return Some(found);
                }
            }
        }
        None
    }
    fn set(layers: &mut [Layer], path: &str, data: Arc<makepad_svg::VectorPath>) -> bool {
        for layer in layers {
            if layer.path == path {
                if let LayerKind::Text { path: Some((d, _)), .. } = &mut layer.kind {
                    *d = data;
                    return true;
                }
            }
            let children = match &mut layer.kind {
                LayerKind::Group { layers, .. } | LayerKind::FlyThrough { layers, .. } => Some(layers),
                LayerKind::Bones(rig) => Some(&mut rig.layers),
                _ => None,
            };
            if children.is_some_and(|layers| set(layers, path, data.clone())) { return true; }
        }
        false
    }
    let (data, world) = find_path(layers, name, series, [0.0, 0.0], false).ok_or_else(|| match series {
        Some(s) => format!("no series \"{s}\" line in a layer named \"{name}\""),
        None => format!("no Path (or Chart with a line) named \"{name}\""),
    })?;
    let own = text_offset(layers, text, [0.0, 0.0]).unwrap_or([0.0, 0.0]);
    let moved = crate::geometry::transformed(&data, &crate::frame::Affine::translate(world[0] - own[0], world[1] - own[1]));
    set(layers, text, moved.into());
    Ok(())
}

/// The inputs a document declares, with `params` over their defaults.
fn input_values(evaluated: &mut crate::value::Evaluated, root: ScriptValue, params: &[(LiveId, InputValue)]) -> Vec<(LiveId, InputValue)> {
    let map = evaluated.map.clone();
    evaluated.with_vm(|vm| {
        let Some(r) = root.as_object().filter(|o| !vm.bx.heap_ref().is_fn(*o) && obj::items(vm, root).is_none()) else { return Vec::new() };
        let mut diags = Vec::new();
        let mut c = Reader::new(vm, &map, &mut diags, None, None);
        let at = c.at(r, String::new(), Loc { line: 1, col: 1 });
        let inputs = crate::layers::Motion::script_from_value(c.vm, r.into()).inputs.look(&c);
        c.declare_inputs(&at, inputs.as_ref(), params);
        c.inputs.iter().map(|decl| decl.id).zip(c.values.iter().cloned()).collect()
    })
}

struct Reader<'a, 'v> {
    /// The document's VM, its objects read where they are.
    vm: &'a mut ScriptVm<'v>,
    map: &'a SourceMap,
    /// The document's VM as a whole, which its 3D scenes are read from
    /// (none where only inputs are declared).
    #[cfg_attr(not(feature = "scene3d"), allow(dead_code))]
    doc_vm: Option<Arc<crate::value::DocVm>>,
    diags: &'a mut Vec<Diagnostic>,
    inputs: Vec<InputDecl>,
    values: Vec<InputValue>,
    duration: f64,
    /// The document's width, for `@safe` widths.
    width: f64,
    /// The document's height (a Chart centres itself by default).
    height: f64,
    /// The document's tempo (a Hud shows it, a Words sequence steps on it).
    bpm: f64,
    /// The document's own `bpm` as written (a Seq3D falls back on it).
    doc_bpm: Option<f64>,
    has_functions: bool,
    /// Texts riding another layer's path (`path: layer("name", "series")`):
    /// (the Text's path, the name, the series, where it was written).
    pending_paths: Vec<(String, String, Option<String>, Loc)>,
    /// The host context (voices, timing) for layers lowered at load
    /// (Text3D's karaoke word times); None where inputs are only declared.
    context: Option<Arc<MotionContext>>,
    /// The document's property functions, by the index a [`FnProp`] holds.
    fns: Vec<ScriptValue>,
    /// The inputs the loader read to decide structure (a Repeat's `source`).
    read_inputs: Vec<LiveId>,
}

/// Where a value sits, for messages: the object that wrote it and the
/// property path.
#[derive(Clone)]
struct At {
    obj: ScriptObject,
    path: String,
    loc: Loc,
}

impl At {
    fn field(&self, name: &str) -> String {
        if self.path.is_empty() { name.to_string() } else { format!("{}.{name}", self.path) }
    }

    /// Note a typed read of field `name` for [`recording`]: what the
    /// loader falls back on when it is not written (as Splash), and the
    /// range it keeps it in.
    fn note(&self, name: &str, default: impl FnOnce() -> Option<String>, range: Option<[f64; 2]>) {
        READS.with(|reads| {
            if let Some(reads) = reads.borrow_mut().as_mut() {
                reads.push(FieldRead { path: self.field(name), default: default(), range });
            }
        });
    }
}

/// A typed field read the loader made, as [`recording`] notes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FieldRead {
    /// The field's place: `layers[2].radius`, `duration`.
    pub path: String,
    /// What it reads as when not written, as Splash (`0`, `vec2(1, 1)`,
    /// `#ffffff`, `@butt`, `true`).
    pub default: Option<String>,
    /// The range the loader keeps a number in.
    pub range: Option<[f64; 2]>,
}

thread_local! {
    static READS: std::cell::RefCell<Option<Vec<FieldRead>>> = const { std::cell::RefCell::new(None) };
}

/// Run `load` (a document load on this thread), noting every typed field
/// read it makes: the field registry's defaults and ranges are the
/// loader's own (`crate::field_registry`).
pub(crate) fn recording<R>(load: impl FnOnce() -> R) -> (R, Vec<FieldRead>) {
    READS.with(|reads| *reads.borrow_mut() = Some(Vec::new()));
    let out = load();
    (out, READS.with(|reads| reads.borrow_mut().take().unwrap_or_default()))
}

/// A default of `N` numbers as Splash: a number, `vec2(..)`, `vec3(..)`,
/// or (four) a colour.
fn splash_default<const N: usize>(value: [f64; N]) -> String {
    match value.as_slice() {
        [x] => fmt_num(*x),
        [x, y] => format!("vec2({}, {})", fmt_num(*x), fmt_num(*y)),
        [x, y, z] => format!("vec3({}, {}, {})", fmt_num(*x), fmt_num(*y), fmt_num(*z)),
        [r, g, b, a] => {
            let packed = pack([*r, *g, *b, *a]);
            if packed & 0xff == 0xff { format!("#{:06x}", packed >> 8) } else { format!("#{packed:08x}") }
        }
        many => format!("[{}]", many.iter().map(|v| fmt_num(*v)).collect::<Vec<_>>().join(", ")),
    }
}

/// The document `root` is, read into the model. `roots`: what keeps `root` alive
/// where the VM does not (a second evaluation in the VM). `structural`:
/// the inputs the evaluation read.
fn compile(source: DocSource, params: &[(LiveId, InputValue)], context: Arc<MotionContext>, vm: Arc<crate::value::DocVm>, literals: Option<Arc<crate::schema::DocLiterals>>, root: ScriptValue, map: &SourceMap, roots: (Vec<ScriptObjectRef>, Vec<ScriptArrayRef>), structural: &[LiveId], diags: &mut Vec<Diagnostic>) -> MotionDoc {
    let mut doc = MotionDoc {
        source,
        label: String::new(),
        brief: String::new(),
        tags: Vec::new(),
        camera: None,
        plain: crate::LoadedLayers {
            size: [1920.0, 1080.0],
            duration: 1.0,
            fps: 60.0,
            bpm: 120.0,
            background: None,
            stops: Vec::new(),
            inputs: Vec::new(),
            input_values: Vec::new(),
            layers: Vec::new(),
            context,
        },
        hdr: false,
        post: Vec::new(),
        motion_blur: None,
        structural: Vec::new(),
        own: Arc::new(Vec::new()),
        warnings: Vec::new(),
        has_functions: false,
        _literals: literals,
        run: Arc::new(crate::value::Run::new(vm.clone(), root, Vec::new(), Vec::new(), Vec::new())),
        vm: vm.clone(),
    };
    let doc_vm = vm.clone();
    let fns = vm.with(|e| {
        e.with_vm(|vm| {
            let context = doc.context.clone();
            let mut c = Reader::new(vm, map, diags, Some(context), Some(doc_vm));
            c.document(root, &mut doc, params);
            (c.fns, c.read_inputs)
        })
    });
    let (fns, read) = fns;
    doc.own = Arc::new(doc.slots());
    doc.structural = doc.inputs.iter().map(|decl| structural.contains(&decl.id) || read.contains(&decl.id)).collect();
    doc.run = Arc::new(crate::value::Run::new(vm, root, fns, roots.0, roots.1));
    doc
}

/// Values that name an input or a library asset, before type checking.
enum Slot<'v> {
    Value(&'v Look),
    Input(String),
}

impl<'a, 'v> Reader<'a, 'v> {
    fn new(vm: &'a mut ScriptVm<'v>, map: &'a SourceMap, diags: &'a mut Vec<Diagnostic>, context: Option<Arc<MotionContext>>, doc_vm: Option<Arc<crate::value::DocVm>>) -> Self {
        Reader { vm, map, doc_vm, diags, inputs: Vec::new(), values: Vec::new(), duration: 0.0, width: 1920.0, height: 1080.0, bpm: 120.0, doc_bpm: None, has_functions: false, pending_paths: Vec::new(), context, fns: Vec::new(), read_inputs: Vec::new() }
    }

    /// Read the document's root into `doc`.
    fn document(&mut self, root: ScriptValue, doc: &mut MotionDoc, params: &[(LiveId, InputValue)]) {
        let root = match self.look(root) {
            Look::Obj(o, Some("Motion")) => o,
            other => {
                if !self.diags.iter().any(Diagnostic::is_error) {
                    self.diags.push(Diagnostic::error(Loc { line: 1, col: 1 }, "", format!("a Motion document's value is one `Motion{{...}}`, and this one is {}", other.kind()))
                        .hint("end the file with `Motion{ size: vec2(1920, 1080) duration: 6.0 layers: [ ... ] }`"));
                }
                return;
            }
        };
        let at = self.at(root, String::new(), Loc { line: 1, col: 1 });
        crate::layers::Motion::script_from_value(self.vm, root.into()).read(self, &at, doc, params);
    }

    // ---- reading the document's objects ----------------------------------

    /// `o`'s field `name` (through its prototypes), looked at; `None` where it has none.
    fn get(&self, o: ScriptObject, name: &str) -> Option<Look> {
        obj::opt(self.vm, o, name).map(|v| obj::look(self.vm, v))
    }

    fn look(&self, v: ScriptValue) -> Look {
        obj::look(self.vm, v)
    }

    /// `o`'s fields, its own first, then each prototype's it does not override.
    fn fields(&self, o: ScriptObject) -> Vec<(String, Look)> {
        obj::fields(self.vm, o).into_iter().map(|(k, v, _)| (k, obj::look(self.vm, v))).collect()
    }

    /// Where `o` was made in the document.
    fn obj_loc(&self, o: ScriptObject) -> Option<Loc> {
        obj::loc(self.vm, self.map, o)
    }

    /// An `At` for object `o` at `path`: where it was made, else `parent`.
    fn at(&self, o: ScriptObject, path: String, parent: Loc) -> At {
        At { obj: o, path, loc: self.obj_loc(o).unwrap_or(parent) }
    }

    /// A property function: `f` kept for evaluation (its index), and where it is.
    fn fn_prop(&mut self, f: ScriptObject, at: &At, field: &str) -> FnProp {
        self.has_functions = true;
        let v: ScriptValue = f.into();
        let index = match self.fns.iter().position(|g| *g == v) {
            Some(i) => i,
            None => {
                self.fns.push(v);
                self.fns.len() - 1
            }
        };
        FnProp { index, loc: self.obj_loc(f).unwrap_or_else(|| self.loc_of(at, field)) }
    }

    fn loc_of(&self, at: &At, field: &str) -> Loc {
        obj::field_loc(self.vm, self.map, at.obj, field).unwrap_or(at.loc)
    }

    /// A `voice(@id)` object's id.
    fn voice_id(&self, v: &Look) -> Option<String> {
        match v {
            Look::Obj(o, Some("Voice")) => match self.get(*o, "id") {
                Some(Look::Id(id) | Look::Str(id)) => Some(id),
                _ => None,
            },
            _ => None,
        }
    }

    /// An `asset(id, rev)` object's reference.
    fn asset_ref(&self, o: ScriptObject) -> Option<AssetRef> {
        match (self.get(o, "id"), self.get(o, "rev")) {
            (Some(Look::Str(id)), Some(Look::Str(rev))) if !id.is_empty() && !rev.is_empty() => Some(AssetRef { id, rev }),
            _ => None,
        }
    }

    /// An HDR `rgba(…)` object's colour (`{color: [r, g, b, a]}`).
    fn rgba_obj(&self, o: ScriptObject) -> Option<Color> {
        match self.get(o, "color") {
            Some(Look::Arr(parts)) if parts.len() == 4 => Some([parts[0].num()?, parts[1].num()?, parts[2].num()?, parts[3].num()?]),
            _ => None,
        }
    }

    fn color_of(&self, value: &Look) -> Option<Color> {
        match value {
            Look::Color(c) => Some(unpack(*c)),
            Look::Obj(o, _) => self.rgba_obj(*o),
            Look::Str(text) => parse_hex(text),
            _ => None,
        }
    }

    /// `post: [...]`: passes and kits (a list in the list gives its items in
    /// its place, as `film_post({..})` returns them), each read the one way
    /// every host reads them (render_graph::script::read_post_entry): a
    /// kit's template builds its passes, whose names get the kit's place.
    pub(crate) fn post_passes(&mut self, at: &At, value: ScriptValue) -> Vec<crate::model::PostPassSpec> {
        if value.is_nil() {
            return Vec::new();
        }
        fn flatten(vm: &ScriptVm, v: ScriptValue, out: &mut Vec<ScriptValue>) {
            for item in obj::items(vm, v).unwrap_or_default() {
                match item.as_array() {
                    Some(_) => flatten(vm, item, out),
                    None => out.push(item),
                }
            }
        }
        if value.as_array().is_none() {
            let kind = self.look(value).kind();
            self.err(at, "post", format!("`post` is a list of passes and kits, not {kind}"), Some("`post: [post.Glow{strength: 0.8} post.Grain{}]` (with `use mod.post`)"));
            return Vec::new();
        }
        let mut items = Vec::new();
        flatten(self.vm, value, &mut items);
        let module = self.vm.bx.heap_ref().value(self.vm.modules(), id!(post).into(), NoTrap);
        let Some(module) = module.as_object() else { return Vec::new() };
        let mut out = Vec::new();
        let mut kits = 0;
        for (index, item) in items.into_iter().enumerate() {
            // `on: false`: bypassed, kept in the list as written (an
            // editor's switch), not drawn.
            if item.as_object().is_some_and(|o| matches!(self.get(o, "on"), Some(Look::Bool(false)))) {
                continue;
            }
            let label = format!("post[{index}]");
            let kind = makepad_render_graph::script::post_kind(self.vm, item).filter(|k| k != "motion_blur");
            let (Some(kind), Some(obj)) = (kind, item.as_object()) else {
                let kind = self.look(item).kind();
                self.err(at, "post", format!("`{label}` is not a pass or a kit ({kind})"), Some("post.Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { ... }}, or a kit: post.Glow post.Bloom post.Halation post.Shoulder post.Aberration post.Grain post.Vignette post.FramePost post.Lut (with `use mod.post`)"));
                continue;
            };
            let pat = self.at(obj, label.clone(), at.loc);
            let prefix = format!("k{kits}_");
            if kind != "pass" {
                kits += 1;
            }
            let reads = match makepad_render_graph::script::read_post_entry(self.vm, module, &kind, item, &label, &prefix) {
                Ok(makepad_render_graph::script::PostEntry::Passes(reads)) => reads,
                Ok(makepad_render_graph::script::PostEntry::Host(_)) => continue,
                Err(e) => {
                    // The reader names the entry (`post[0] Pass: ...`); the
                    // diagnostic is placed at the field it names, if any.
                    let message = e.strip_prefix(&label).unwrap_or(&e).trim_start_matches(" Pass").trim_start_matches(": ").to_string();
                    let field = makepad_render_graph::script::PASS_FIELDS.iter().find(|f| message.contains(&format!("`{f}`")) || message.contains(&format!("`{f}:"))).copied();
                    match field {
                        Some(field) => self.err(&pat, field, message, None),
                        None => self.err(at, &label, message, None),
                    }
                    continue;
                }
            };
            for read in reads {
                let mut decl = read.decl;
                if decl.history {
                    self.err(&pat, "history", "a Motion pass keeps no history: every frame is a function of its time".into(), None);
                    decl.history = false;
                }
                for f in decl.fns_mut() {
                    f.at = self.code_at(&f.at);
                }
                // An editor's document: its literals are live in edit mode.
                decl.live_literals = crate::value::loading_editable();
                let mut uniforms = Vec::new();
                for (name, value) in read.uniforms {
                    let value = self.look(value);
                    let uat = self.at(obj, format!("{label}.uniforms"), pat.loc);
                    let colour = matches!(value, Look::Color(_) | Look::Str(_)) || matches!(&value, Look::Obj(o, _) if self.get(*o, "color").is_some()) || self.anim_is_color(&value);
                    let width = match &value { Look::Arr(values) => Some(values.len()), _ => self.anim_kind(&value) };
                    if colour {
                        let prop = self.prop_value::<4>(&uat, &name, &value).unwrap_or(Prop::Const([0.0; 4]));
                        let prop = match prop {
                            // Colours are linear light in passes.
                            Prop::Const(c) => Prop::Const(makepad_tween::color_srgb_to_linear(c)),
                            other => other,
                        };
                        decl.uniforms.push(makepad_render_graph::UniformDecl { name: name.clone(), width: 4 });
                        uniforms.push(crate::model::PostUniform::Four(prop));
                    } else if width == Some(2) {
                        let prop = self.prop_value::<2>(&uat, &name, &value).unwrap_or(Prop::Const([0.0; 2]));
                        decl.uniforms.push(makepad_render_graph::UniformDecl { name: name.clone(), width: 2 });
                        uniforms.push(crate::model::PostUniform::Two(prop));
                    } else if width == Some(3) {
                        let prop = self.prop_value::<3>(&uat, &name, &value).unwrap_or(Prop::Const([0.0; 3]));
                        decl.uniforms.push(makepad_render_graph::UniformDecl { name: name.clone(), width: 3 });
                        uniforms.push(crate::model::PostUniform::Three(prop));
                    } else {
                        let prop = self.prop_value::<1>(&uat, &name, &value).unwrap_or(Prop::Const([0.0]));
                        decl.uniforms.push(makepad_render_graph::UniformDecl { name: name.clone(), width: 1 });
                        uniforms.push(crate::model::PostUniform::One(prop));
                    }
                }
                if let Err(e) = decl.validate() {
                    self.err(&pat, "pixel", e, None);
                    continue;
                }
                out.push(crate::model::PostPassSpec { decl, uniforms });
            }
        }
        out
    }

    /// Where a pass's function is written, as the render graph's origins
    /// hold it: the document's own lines (file `motion`), or a module's.
    fn code_at(&self, at: &makepad_render_graph::pass::CodeAt) -> makepad_render_graph::pass::CodeAt {
        if self.map.owns(&at.file) {
            let loc = self.map.map(at.line, at.col);
            makepad_render_graph::pass::CodeAt { file: crate::value::doc_code_file(), line: loc.line as u32, col: loc.col as u32 }
        } else {
            makepad_render_graph::pass::CodeAt { file: at.file.clone(), line: at.line + 1, col: at.col + 1 }
        }
    }

    /// `motion_blur: MotionBlur{shutter samples tol}`.
    fn motion_blur(&mut self, at: &At, value: &Look) -> Option<crate::model::MotionBlurSpec> {
        let obj = match value {
            Look::Nil => return None,
            Look::Obj(o, Some("MotionBlur")) => *o,
            other => {
                self.err(at, "motion_blur", format!("`motion_blur` is `MotionBlur{{shutter: 180}}`, not {}", other.kind()), None);
                return None;
            }
        };
        let mat = self.at(obj, "motion_blur".into(), at.loc);
        let shutter = self.prop::<1>(&mat, "shutter", [180.0]);
        let max = match self.get(obj, "samples") {
            Some(Look::Id(id)) if id == "auto" => 324,
            Some(v) => v.num().map(|n| n.clamp(1.0, 324.0) as u32).unwrap_or(324),
            None => 324,
        };
        let tol = self.get(obj, "tol").and_then(|v| v.num()).unwrap_or(3.0).clamp(0.25, 64.0) as f32;
        Some(crate::model::MotionBlurSpec { shutter, max, tol })
    }

    /// Where the function `f` is written (its `fn`), by the file name its
    /// compiled copies give it: the document (`DOC_CODE_FILE`) or a module
    /// (`module:<key>`). For edit mode's live literals.
    fn fn_site(&self, f: ScriptObject) -> Option<(String, Loc)> {
        let at = makepad_render_graph::script::fn_code(self.vm, f.into())?.at;
        if self.map.owns(&at.file) {
            Some((crate::value::doc_code_file(), self.map.map(at.line, at.col)))
        } else if at.file.starts_with("module:") {
            Some((at.file.clone(), Loc { line: at.line + 1, col: at.col + 1 }))
        } else {
            None
        }
    }

    /// The function `f` (moved to the drawing side's VM by its tokens) and
    /// where it is written, when that is the document itself.
    fn fn_code(&self, f: ScriptObject) -> Option<(Option<Loc>, makepad_render_graph::pass::PassFn)> {
        let code = makepad_render_graph::script::fn_code(self.vm, f.into())?;
        let loc = self.map.owns(&code.at.file).then(|| self.map.map(code.at.line, code.at.col));
        Some((loc, code))
    }


    fn err(&mut self, at: &At, field: &str, message: String, hint: Option<&str>) {
        let loc = self.loc_of(at, field);
        let diag = Diagnostic::error(loc, at.field(field), message);
        self.diags.push(match hint {
            Some(hint) => diag.hint(hint),
            None => diag,
        });
    }

    fn warn(&mut self, at: &At, field: &str, message: String, hint: Option<&str>) {
        let loc = self.loc_of(at, field);
        let diag = Diagnostic::warning(loc, at.field(field), message);
        self.diags.push(match hint {
            Some(hint) => diag.hint(hint),
            None => diag,
        });
    }

    /// An open layer type's fields ([`crate::layers::OPEN`]) are checked
    /// here, where its layer is read: the VM allows any.
    fn check_open(&mut self, at: &At, ty: &str) {
        let fields = crate::layers::fields(ty);
        let allowed: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.unknown_fields(at, &allowed, ty);
    }

    fn unknown_fields(&mut self, at: &At, allowed: &[&str], what: &str) {
        let fields: Vec<String> = self.fields(at.obj).into_iter().map(|(key, _)| key).collect();
        for key in fields {
            if key.starts_with("__") || allowed.contains(&key.as_str()) {
                continue;
            }
            let hint = format!("{what} fields: {}", allowed.join(", "));
            let message = foreign::message(&key, what).unwrap_or_else(|| format!("`{key}` is not a {what} field.{}", did_you_mean(&key, allowed)));
            self.err(at, &key, message, Some(&hint));
        }
    }

    // ---- inputs ------------------------------------------------------

    fn declare_inputs(&mut self, at: &At, value: Option<&Look>, params: &[(LiveId, InputValue)]) {
        let fields = match value {
            None | Some(Look::Nil) => Vec::new(),
            Some(Look::Obj(obj, _)) => self.fields(*obj),
            Some(other) => {
                self.err(at, "inputs", format!("`inputs` is an object of name: default, not {}", other.kind()), Some("`inputs: {headline: \"Hello\" accent: #ff5a36 logo: input(@image)}`"));
                Vec::new()
            }
        };
        let inputs_obj = match value {
            Some(Look::Obj(obj, _)) => Some(*obj),
            _ => None,
        };
        for (name, default) in fields {
            let inputs_at = self.at(inputs_obj.unwrap(), "inputs".into(), at.loc);
            let loc = self.loc_of(&inputs_at, &name);
            let (ty, default, doc) = match &default {
                Look::Obj(obj, Some("Input")) => {
                    let obj = *obj;
                    let ty = match self.get(obj, "ty") {
                        Some(Look::Id(ty)) => InputType::from_id(&ty),
                        _ => None,
                    };
                    let Some(ty) = ty else {
                        self.err(&inputs_at, &name, format!("`input(…)` takes a type: @text, @number, @color, @vec2, @bool, @image, @svg, @video, @model or @list"), None);
                        continue;
                    };
                    let doc = match self.get(obj, "doc") {
                        Some(Look::Str(doc)) => doc,
                        _ => String::new(),
                    };
                    let default = match self.get(obj, "default") {
                        None | Some(Look::Nil) => match ty {
                            InputType::Text => InputValue::Text(String::new()),
                            InputType::Number => InputValue::Number(0.0),
                            InputType::Color => InputValue::Color([1.0, 1.0, 1.0, 1.0]),
                            InputType::Vec2 => InputValue::Vec2([0.0, 0.0]),
                            InputType::Bool => InputValue::Bool(false),
                            InputType::List => InputValue::List(Vec::new()),
                            _ => InputValue::Empty,
                        },
                        Some(value) => match self.input_value(&value) {
                            Some(value) if fits(ty, &value) => value,
                            _ => {
                                self.err(&inputs_at, &name, format!("input `{name}` is @{} but its default is {}", ty.id(), value.kind()), None);
                                continue;
                            }
                        },
                    };
                    (ty, default, doc)
                }
                value => match self.input_value(value) {
                    Some(v) => (type_of(&v), v, String::new()),
                    None => {
                        self.err(&inputs_at, &name, format!("input `{name}`'s default is {}; inputs hold text, numbers, colours, vec2s, bools, assets or lists", value.kind()),
                            Some("declare a typed input without a default as `logo: input(@image)`"));
                        continue;
                    }
                },
            };
            self.inputs.push(InputDecl { id: crate::value::lut_id(&name), name, ty, default: default.clone(), doc, loc });
            self.values.push(default);
        }
        for (id, value) in params {
            let name = crate::obj::id_text(*id);
            match self.inputs.iter().position(|input| input.id == *id) {
                Some(index) => {
                    let ty = self.inputs[index].ty;
                    let value = coerce_param(ty, value.clone());
                    if fits(ty, &value) {
                        self.values[index] = value;
                    } else {
                        let decl = self.inputs[index].loc;
                        self.diags.push(Diagnostic::error(decl, format!("inputs.{name}"), format!("the host gives input `{name}` {} but it is @{}", value.describe(), ty.id())));
                    }
                }
                None => {
                    let names: Vec<&str> = self.inputs.iter().map(|input| input.name.as_str()).collect();
                    self.diags.push(Diagnostic::warning(at.loc, format!("inputs.{name}"),
                        format!("the host gives `{name}`, which the document does not declare (inputs: {}).{}", if names.is_empty() { "none".to_string() } else { names.join(", ") }, did_you_mean(&name, &names))));
                }
            }
        }
    }

    fn input_value(&self, value: &Look) -> Option<InputValue> {
        Some(match value {
            Look::Arr(items) if !(items.len() == 2 && items.iter().all(|v| v.num().is_some())) => {
                InputValue::List(items.iter().map(|item| self.item_value(item)).collect::<Option<_>>()?)
            }
            Look::Obj(obj, ty) if !matches!(ty, Some("Asset" | "MissingMedia")) && self.get(*obj, "color").is_none() => return None,
            other => self.item_value(other)?,
        })
    }

    /// One value of an input, or of a list input's items: a record
    /// (`{name: "Ada" score: 12}`) only as an item.
    fn item_value(&self, value: &Look) -> Option<InputValue> {
        Some(match value {
            Look::Str(text) => InputValue::Text(text.clone()),
            Look::Num(n) if n.is_finite() => InputValue::Number(*n),
            Look::Bool(b) => InputValue::Bool(*b),
            Look::Color(c) => InputValue::Color(unpack(*c)),
            Look::Arr(items) if items.len() == 2 && items.iter().all(|v| v.num().is_some()) => InputValue::Vec2([items[0].num()?, items[1].num()?]),
            Look::Arr(items) => InputValue::List(items.iter().map(|item| self.item_value(item)).collect::<Option<_>>()?),
            Look::Obj(_, Some("MissingMedia")) => InputValue::Empty,
            Look::Obj(obj, Some("Asset")) => InputValue::Asset(self.asset_ref(*obj)?),
            Look::Obj(obj, _) if self.get(*obj, "color").is_some() => InputValue::Color(self.rgba_obj(*obj)?),
            Look::Obj(obj, None) => InputValue::Record(self.fields(*obj).into_iter().map(|(k, v)| Some((k, self.item_value(&v)?))).collect::<Option<_>>()?),
            _ => return None,
        })
    }

    /// `in(@name)` in a value: the input's name.
    fn slot<'l>(&self, value: &'l Look) -> Slot<'l> {
        if let Look::Obj(o, Some("In")) = value {
            if let Some(Look::Id(name) | Look::Str(name)) = self.get(*o, "name") {
                return Slot::Input(name);
            }
        }
        Slot::Value(value)
    }

    /// The current value of input `name`, or a diagnostic at `field`.
    fn input(&mut self, at: &At, field: &str, name: &str) -> Option<InputValue> {
        match self.inputs.iter().position(|input| input.name == name) {
            Some(index) => {
                let id = self.inputs[index].id;
                if !self.read_inputs.contains(&id) {
                    self.read_inputs.push(id);
                }
                Some(self.values[index].clone())
            }
            None => {
                let names: Vec<String> = self.inputs.iter().map(|input| input.name.clone()).collect();
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let hint = format!("declare it: `inputs: {{{name}: …}}`");
                self.err(at, field, format!("`in(@{name})` names no input (declared: {}).{}", if names.is_empty() { "none".into() } else { names.join(", ") }, did_you_mean(name, &refs)), Some(&hint));
                None
            }
        }
    }

    // ---- scalar reads ------------------------------------------------

    fn num_const(&mut self, at: &At, field: &str, value: &Look) -> Option<f64> {
        match self.slot(value) {
            Slot::Input(name) => match self.input(at, field, &name)? {
                InputValue::Number(n) => Some(n),
                other => {
                    self.err(at, field, format!("`{field}` is a number, but input `{name}` is {}", other.describe()), None);
                    None
                }
            },
            Slot::Value(Look::Num(n)) if n.is_finite() => Some(*n),
            Slot::Value(Look::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
            Slot::Value(other) => {
                let hint = if other.anim().is_some() { Some("this property is not animatable; give it a number") } else { None };
                self.err(at, field, format!("`{field}` is a number, not {}", other.kind()), hint);
                None
            }
        }
    }

    fn vec2_const(&mut self, at: &At, field: &str, value: &Look) -> Option<[f64; 2]> {
        match self.slot(value) {
            Slot::Input(name) => match self.input(at, field, &name)? {
                InputValue::Vec2(v) => Some(v),
                InputValue::Number(n) => Some([n, n]),
                other => {
                    self.err(at, field, format!("`{field}` is a vec2, but input `{name}` is {}", other.describe()), None);
                    None
                }
            },
            Slot::Value(value) => match vec_n::<2>(value) {
                Some(v) => Some(v),
                None => {
                    self.err(at, field, format!("`{field}` is `vec2(x, y)`, not {}", value.kind()), None);
                    None
                }
            },
        }
    }

    fn color_const(&mut self, at: &At, field: &str, value: &Look) -> Option<Color> {
        match self.slot(value) {
            Slot::Input(name) => match self.input(at, field, &name)? {
                InputValue::Color(c) => Some(c),
                other => {
                    self.err(at, field, format!("`{field}` is a colour, but input `{name}` is {}", other.describe()), None);
                    None
                }
            },
            Slot::Value(value) => match self.color_of(value) {
                Some(c) => Some(c),
                None => {
                    self.err(at, field, format!("`{field}` is a colour (`#rrggbb`, `#rrggbbaa` or `rgba(r, g, b, a)` in 0..1), not {}", value.kind()), None);
                    None
                }
            },
        }
    }

    fn string(&mut self, at: &At, field: &str, value: &Look) -> Option<String> {
        match self.slot(value) {
            Slot::Input(name) => match self.input(at, field, &name)? {
                InputValue::Text(text) => Some(text),
                InputValue::Number(n) => Some(fmt_num(n)),
                other => {
                    self.err(at, field, format!("`{field}` is text, but input `{name}` is {}", other.describe()), None);
                    None
                }
            },
            Slot::Value(Look::Str(text)) => Some(text.clone()),
            Slot::Value(Look::Num(n)) => Some(fmt_num(*n)),
            Slot::Value(other) => {
                self.err(at, field, format!("`{field}` is a string, not {}", other.kind()), None);
                None
            }
        }
    }

    fn id_enum_of<T>(&mut self, at: &At, field: &str, value: Option<Look>, parse: impl Fn(&str) -> Option<T>, names: &[&str], default: T) -> T {
        match value {
            None | Some(Look::Nil) => default,
            Some(Look::Id(id)) | Some(Look::Str(id)) => match parse(&id) {
                Some(value) => value,
                None => {
                    let list = names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(", ");
                    self.err(at, field, format!("`{field}: @{id}` is not one of {list}.{}", did_you_mean(&id, names)), None);
                    default
                }
            },
            Some(other) => {
                let list = names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(", ");
                self.err(at, field, format!("`{field}` is one of {list}, not {}", other.kind()), None);
                default
            }
        }
    }

    // ---- time --------------------------------------------------------

    /// A time reference's fields as `parse_time_fields` reads them: its
    /// kind (`beat`, `bar`, …) under `time`.
    fn time_fields<'s>(&'s self, value: &Look) -> impl Fn(&str) -> TimeArg + use<'s, 'a, 'v> {
        let kind = value.time_ref().map(str::to_lowercase);
        let obj = value.obj();
        move |name: &str| match (name, &kind, obj) {
            ("time", Some(kind), _) => TimeArg::Id(kind.clone()),
            ("time", None, _) => TimeArg::Nil,
            (_, _, Some(obj)) => self.get(obj, name).map_or(TimeArg::Nil, |v| Self::time_arg(&v)),
            _ => TimeArg::Nil,
        }
    }

    fn time_arg(value: &Look) -> TimeArg {
        match value {
            Look::Num(n) => TimeArg::Num(*n),
            Look::Id(id) => TimeArg::Id(id.clone()),
            Look::Str(text) => TimeArg::Text(text.clone()),
            Look::Nil => TimeArg::Nil,
            _ => TimeArg::Other,
        }
    }

    fn time(&mut self, at: &At, field: &str, value: &Look) -> Option<Time> {
        if let Slot::Input(name) = self.slot(value) {
            return match self.input(at, field, &name)? {
                InputValue::Number(n) => Some(Time::seconds(n)),
                other => {
                    self.err(at, field, format!("`{field}` is a time, but input `{name}` is {}", other.describe()), None);
                    None
                }
            };
        }
        match value {
            Look::Num(n) if n.is_finite() => Some(Time::seconds(*n)),
            Look::Obj(..) => {
                let parsed = parse_time_fields(&self.time_fields(value));
                match parsed {
                    Some(Ok(TimeValue::Point(expr))) => Some(Time { expr, value: 0.0 }),
                    Some(Ok(TimeValue::Length(_))) => {
                        self.err(at, field, format!("`{field}` is a moment; `beats(x)` is a length"), Some("for a moment on the grid write `beat(n)`; `beats(x)` is for staggers, loop periods and span lengths"));
                        None
                    }
                    Some(Err(message)) => {
                        self.err(at, field, format!("`{field}`: {message}"), None);
                        None
                    }
                    None => {
                        self.err(at, field, format!("`{field}` is a time: seconds or a reference like beat(4), not {}", value.kind()), None);
                        None
                    }
                }
            }
            other => {
                self.err(at, field, format!("`{field}` is a time: seconds, or beat(n) / bar(n, beat) / downbeat(n) / onset(n) / section(@name) / word(@voice, i) / line(@voice, i), not {}", other.kind()), None);
                None
            }
        }
    }

    fn duration_of(&mut self, at: &At, field: &str, value: &Look) -> Option<Dur> {
        match value {
            Look::Num(n) if n.is_finite() && *n >= 0.0 => Some(Dur::seconds(*n)),
            Look::Obj(..) if value.time_ref().is_some() => {
                let parsed = parse_time_fields(&self.time_fields(value));
                match parsed {
                    Some(Ok(TimeValue::Length(length))) => Some(Dur { expr: length, value: match length { Length::Seconds(s) => s, Length::Beats(b) => b * 0.5 } }),
                    Some(Ok(TimeValue::Point(_))) => {
                        self.err(at, field, format!("`{field}` is a length (seconds or `beats(x)`), not a moment"), Some("`beats(1/8)` is an eighth of a beat; `beat(n)` is a moment"));
                        None
                    }
                    Some(Err(message)) => {
                        self.err(at, field, format!("`{field}`: {message}"), None);
                        None
                    }
                    None => None,
                }
            }
            Look::Obj(..) if matches!(self.slot(value), Slot::Input(_)) => {
                let Slot::Input(name) = self.slot(value) else { return None };
                match self.input(at, field, &name)? {
                    InputValue::Number(n) if n >= 0.0 => Some(Dur::seconds(n)),
                    other => {
                        self.err(at, field, format!("`{field}` is a length in seconds, but input `{name}` is {}", other.describe()), None);
                        None
                    }
                }
            }
            other => {
                self.err(at, field, format!("`{field}` is a length: seconds from 0 or `beats(x)`, not {}", other.kind()), None);
                None
            }
        }
    }

    /// A key's record (`{ease: @bezier handle: @free in: [-0.3, 5] out:
    /// [0.25, -4]}`): the segment arriving at it, and its handles.
    fn key_handles<const N: usize>(&mut self, at: &At, field: &str, rec: ScriptObject, index: usize) -> (Ease, Option<KeyHandles>) {
        let handle = |s: &mut Self, side: &str| -> Option<[f64; 2]> {
            match s.get(rec, side) {
                None => None,
                Some(Look::Arr(xs)) if xs.len() == 2 => match (xs[0].num(), xs[1].num()) {
                    (Some(dt), Some(dv)) => Some([dt, dv]),
                    _ => None,
                },
                Some(other) => {
                    s.err(at, field, format!("key {index}'s `{side}` handle is [seconds, value], not {}", other.kind()), Some("`{in: [-0.3, 5] out: [0.25, -4]}`"));
                    None
                }
            }
        };
        let (r#in, out) = (handle(self, "in"), handle(self, "out"));
        let mode = match self.get(rec, "handle") {
            None if r#in.is_some() || out.is_some() => HandleMode::Free,
            None => HandleMode::Clamped,
            Some(Look::Id(id)) => HandleMode::from_name(&id).unwrap_or_else(|| {
                self.err(at, field, format!("key {index}'s handle `@{id}` is not one of @auto @clamped @aligned @free @vector"), None);
                HandleMode::Clamped
            }),
            Some(other) => {
                self.err(at, field, format!("key {index}'s `handle` is a name (@auto …), not {}", other.kind()), None);
                HandleMode::Clamped
            }
        };
        if N > 1 && (r#in.is_some() || out.is_some()) {
            self.err(at, field, format!("key {index}: written handles are for one number; a vector's keys take @auto, @clamped or @vector"), None);
        }
        match self.get(rec, "ease") {
            None => (Ease::Linear, Some(KeyHandles { bezier: true, mode, r#in, out })),
            Some(Look::Id(id)) if id == "bezier" => (Ease::Linear, Some(KeyHandles { bezier: true, mode, r#in, out })),
            // Another ease into it; its handles still shape the next segment.
            Some(ease) => (self.ease(at, field, &ease), Some(KeyHandles { bezier: false, mode, r#in, out })),
        }
    }

    fn ease(&mut self, at: &At, field: &str, value: &Look) -> Ease {
        match value {
            Look::Nil => Ease::Linear,
            Look::Id(id) | Look::Str(id) => match Ease::from_id(id) {
                Some(ease) => ease,
                None => {
                    let near = Ease::suggest(id);
                    let hint = if near.is_empty() {
                        "eases: @linear, @hold, @ease_in, @ease_out, @ease_in_out, @ease_{in,out,in_out}_{quad,cubic,quart,quint,sine,expo,circ,back,elastic,bounce}, spring(k, d), bezier(x1, y1, x2, y2); rate shapers @there_and_back, @there_and_back_with_pause, @running_start, @wiggle, @squish, @not_quite_there, @lingering, @exponential_decay, @double_smooth (over linear with defaults; over another ease or with parameters the call form wiggle(@ease_out, 3))".to_string()
                    } else {
                        format!("did you mean {}?", near.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" or "))
                    };
                    self.err(at, field, format!("`@{id}` is not an ease"), Some(&hint));
                    Ease::Linear
                }
            },
            Look::Obj(obj, Some("Bezier")) => {
                let p = |k: &str| self.get(*obj, k).and_then(|v| v.num());
                match (p("x1"), p("y1"), p("x2"), p("y2")) {
                    (Some(x1), Some(y1), Some(x2), Some(y2)) => {
                        if !(0.0..=1.0).contains(&x1) || !(0.0..=1.0).contains(&x2) {
                            self.warn(at, field, format!("`bezier({}, {}, {}, {})`: the x values are clamped to 0..1 (time runs forward)", fmt_num(x1), fmt_num(y1), fmt_num(x2), fmt_num(y2)), None);
                        }
                        Ease::Bezier { x1, y1, x2, y2 }
                    }
                    _ => {
                        self.err(at, field, "`bezier(x1, y1, x2, y2)` takes four numbers, as CSS cubic-bezier".into(), Some("`bezier(0.7, 0, 0.3, 1)`"));
                        Ease::Linear
                    }
                }
            }
            Look::Obj(obj, Some("Spring")) => {
                let k = self.get(*obj, "k").and_then(|v| v.num()).unwrap_or(170.0);
                let d = self.get(*obj, "d").and_then(|v| v.num()).unwrap_or(26.0);
                if !(k > 0.0 && d >= 0.0) {
                    self.err(at, field, format!("`spring({}, {})` needs a positive stiffness and a damping from 0", fmt_num(k), fmt_num(d)), Some("`spring(170, 26)` settles fast; `spring(100, 10)` wobbles"));
                }
                Ease::Spring { k, d }
            }
            Look::Obj(obj, Some("Rate")) => {
                let shape = match self.get(*obj, "shape") {
                    Some(Look::Id(id)) => makepad_tween::named::Shape::from_id(&id),
                    _ => None,
                };
                let Some(shape) = shape else {
                    self.err(at, field, "a rate shaper names its shape".into(), None);
                    return Ease::Linear;
                };
                let of = self.get(*obj, "of").unwrap_or(Look::Nil);
                let Some(base) = self.ease(at, field, &of).as_base() else {
                    self.err(at, field, format!("`{}` reshapes a plain ease (an @id, `spring(k, d)` or `bezier(..)`), not `@hold` or another shaper", shape.id()), None);
                    return Ease::Linear;
                };
                let (da, db) = shape.defaults();
                let a = self.get(*obj, "a").and_then(|v| v.num()).unwrap_or(da);
                let b = self.get(*obj, "b").and_then(|v| v.num()).unwrap_or(db);
                if shape == makepad_tween::named::Shape::Squish && !(a < b) {
                    self.err(at, field, format!("`squish` needs from < to, not {} and {}", fmt_num(a), fmt_num(b)), None);
                }
                Ease::Shaped(makepad_tween::named::Shaped { shape, base, a, b })
            }
            other => {
                self.err(at, field, format!("an ease is an @id like @ease_out, `spring(k, d)`, `bezier(x1, y1, x2, y2)` or a rate shaper like `wiggle(@ease_out, 3)`, not {}", other.kind()), None);
                Ease::Linear
            }
        }
    }

    // ---- animated properties -----------------------------------------

    /// A constant for property `field` of `N` components.
    fn const_n<const N: usize>(&mut self, at: &At, field: &str, value: &Look) -> Option<[f64; N]> {
        match N {
            1 => self.num_const(at, field, value).map(|n| [n; N]),
            2 => match (self.slot(value), value) {
                (Slot::Value(Look::Num(n)), _) => Some([*n; N]),
                _ => self.vec2_const(at, field, value).map(|v| std::array::from_fn(|i| v[i])),
            },
            3 => match self.slot(value) {
                Slot::Value(value) => match vec_n::<3>(value) {
                    Some(v) => Some(std::array::from_fn(|i| v[i])),
                    None => {
                        self.err(at, field, format!("`{field}` is `vec3(x, y, z)`, not {}", value.kind()), None);
                        None
                    }
                },
                Slot::Input(name) => {
                    self.err(at, field, format!("`{field}` is a vec3; inputs cannot hold one (input `{name}`)"), None);
                    None
                }
            },
            4 => self.color_const(at, field, value).map(|c| std::array::from_fn(|i| c[i])),
            _ => None,
        }
    }

    fn prop<const N: usize>(&mut self, at: &At, field: &str, default: [f64; N]) -> Prop<N> {
        match self.get(at.obj, field) {
            None | Some(Look::Nil) => Prop::Const(default),
            Some(value) => self.prop_value(at, field, &value).unwrap_or(Prop::Const(default)),
        }
    }

    /// [`Self::prop`] of a value already read.
    fn prop_of<const N: usize>(&mut self, at: &At, field: &str, value: Option<Look>, default: [f64; N]) -> Prop<N> {
        at.note(field, || Some(splash_default(default)), None);
        match value {
            None | Some(Look::Nil) => Prop::Const(default),
            Some(value) => self.prop_value(at, field, &value).unwrap_or(Prop::Const(default)),
        }
    }

    fn opt_prop<const N: usize>(&mut self, at: &At, field: &str) -> Option<Prop<N>> {
        match self.get(at.obj, field) {
            None | Some(Look::Nil) => None,
            Some(value) => self.prop_value(at, field, &value),
        }
    }

    fn prop_value<const N: usize>(&mut self, at: &At, field: &str, value: &Look) -> Option<Prop<N>> {
        match value {
            Look::Fn(f) => Some(Prop::Fn(self.fn_prop(*f, at, field))),
            Look::Obj(_, Some("Blend" | "Blend2")) => {
                let form = self.blend_form(at, field, value, |c, at, field, value| c.prop_value::<N>(at, field, value))?;
                Some(Prop::Blend(Box::new(form)))
            }
            Look::Obj(..) if value.anim().is_some() => self.track::<N>(at, field, value).map(Prop::Track),
            _ => self.const_n::<N>(at, field, value).map(Prop::Const),
        }
    }

    /// `tween`, `keys`, `sequence` and `loop`, lowered to a key track.
    fn track<const N: usize>(&mut self, at: &At, field: &str, value: &Look) -> Option<Track<N>> {
        let obj = value.obj()?;
        let kind = value.anim().unwrap_or("").to_string();
        let (keys, repeat, before, after) = match kind.as_str() {
            "Tween" => {
                let get = |name: &str| self.get(obj, name).unwrap_or(Look::Nil);
                let (from_v, to_v, start, end, ease_v) = (get("from"), get("to"), get("start"), get("end"), get("ease"));
                let from = self.const_n::<N>(at, field, &from_v)?;
                let to = self.const_n::<N>(at, field, &to_v)?;
                if matches!(start, Look::Nil) || matches!(end, Look::Nil) {
                    self.err(at, field, format!("`tween(from, to, start, end, ease)` needs its start and end times"), Some("`tween(0, 1, 0.0, 0.5, @ease_out)` goes from 0 at 0 s to 1 at 0.5 s"));
                    return None;
                }
                let start = self.time(at, field, &start)?;
                let end = self.time(at, field, &end)?;
                if let (TimeExpr::Seconds(a), TimeExpr::Seconds(b)) = (&start.expr, &end.expr) {
                    if b <= a {
                        self.err(at, field, format!("`tween` ends at {} s, not after its start at {} s", fmt_num(*b), fmt_num(*a)), Some("the arguments are (from, to, start, end, ease): times come third and fourth"));
                        return None;
                    }
                }
                let ease = self.ease(at, field, &ease_v);
                let keys = vec![Key { t: start, v: from, ease: Ease::Linear, bez: None }, Key { t: end, v: to, ease, bez: None }];
                (keys, None, Extrap::Constant, Extrap::Constant)
            }
            "Keys" => {
                let Some(Look::Arr(mut points)) = self.get(obj, "points") else { return None };
                // A record after the keys: what the track does past them.
                let (mut before, mut after) = (Extrap::Constant, Extrap::Constant);
                if let Some(Look::Obj(rec, _)) = points.last().cloned() {
                    points.pop();
                    for (side, slot) in [("before", &mut before), ("after", &mut after)] {
                        match self.get(rec, side) {
                            None => {}
                            Some(Look::Id(id)) if id == "constant" => *slot = Extrap::Constant,
                            Some(Look::Id(id)) if id == "linear" => *slot = Extrap::Linear,
                            Some(Look::Id(id)) if id == "cycle" => *slot = Extrap::Cycle,
                            Some(other) => self.err(at, field, format!("`{side}` is @constant, @linear or @cycle, not {}", other.kind()), Some("`keys([0, 0], [1, 1], {before: @constant after: @cycle})`")),
                        }
                    }
                }
                if points.is_empty() {
                    self.err(at, field, format!("`keys()` has no keys"), Some("`keys([0, 0], [0.5, 1, @ease_out])`: each key is [time, value] or [time, value, @ease]"));
                    return None;
                }
                let mut keys = Vec::new();
                for (index, point) in points.iter().enumerate() {
                    let Look::Arr(parts) = point else {
                        self.err(at, field, format!("key {index} is {}; each key is `[time, value]` or `[time, value, @ease]`", point.kind()), None);
                        return None;
                    };
                    if parts.len() < 2 || parts.len() > 3 {
                        self.err(at, field, format!("key {index} has {} parts; a key is `[time, value]` or `[time, value, @ease]`", parts.len()), None);
                        return None;
                    }
                    let t = self.time(at, field, &parts[0])?;
                    let v = self.const_n::<N>(at, field, &parts[1])?;
                    let (ease, bez) = match parts.get(2) {
                        None => (Ease::Linear, None),
                        Some(Look::Id(id)) if id == "bezier" => (Ease::Linear, Some(KeyHandles { bezier: true, ..Default::default() })),
                        Some(Look::Obj(rec, None)) => self.key_handles::<N>(at, field, *rec, index),
                        Some(ease) => (self.ease(at, field, ease), None),
                    };
                    keys.push(Key { t, v, ease, bez });
                }
                (keys, None, before, after)
            }
            "Sequence" => {
                let Some(Look::Arr(steps)) = self.get(obj, "steps") else {
                    self.err(at, field, format!("`sequence(steps, start)` takes an array of steps"), Some("`sequence([[0, 0], [1, 0.4, @ease_out], [1, 1.0], [0, 0.3]])`: each step is [value, seconds after the previous, @ease]"));
                    return None;
                };
                let start = self.get(obj, "start").unwrap_or(Look::Num(0.0));
                let start = self.time(at, field, &start)?;
                let mut keys = Vec::new();
                let mut elapsed = 0.0;
                for (index, step) in steps.iter().enumerate() {
                    let Look::Arr(parts) = step else {
                        self.err(at, field, format!("sequence step {index} is {}; a step is `[value, seconds, @ease]`", step.kind()), None);
                        return None;
                    };
                    if parts.is_empty() || parts.len() > 3 {
                        self.err(at, field, format!("sequence step {index} has {} parts; a step is `[value, seconds, @ease]`", parts.len()), None);
                        return None;
                    }
                    let v = self.const_n::<N>(at, field, &parts[0])?;
                    let dur = match parts.get(1) {
                        None => 0.0,
                        Some(value) => match value.num() {
                            Some(d) if d >= 0.0 => d,
                            _ => {
                                self.err(at, field, format!("sequence step {index}'s length is seconds from 0, not {}", value.kind()), None);
                                return None;
                            }
                        },
                    };
                    if index > 0 && dur <= 0.0 {
                        self.err(at, field, format!("sequence step {index} lasts 0 s; every step after the first takes time"), None);
                        return None;
                    }
                    elapsed += dur;
                    let ease = parts.get(2).map_or(Ease::Linear, |ease| self.ease(at, field, ease));
                    // A sequence starting at a musical reference keeps that
                    // reference and each step's offset until time resolution.
                    let t = match offset_expr(&start.expr, elapsed) {
                        TimeExpr::Seconds(s) => Time::seconds(s),
                        expr => Time { expr, value: 0.0 },
                    };
                    keys.push(Key { t, v, ease, bez: None });
                }
                if keys.is_empty() {
                    self.err(at, field, format!("`sequence()` has no steps"), None);
                    return None;
                }
                (keys, None, Extrap::Constant, Extrap::Constant)
            }
            "Loop" => {
                let inner = self.get(obj, "inner").unwrap_or(Look::Nil);
                let track = self.track::<N>(at, field, &inner).or_else(|| {
                    self.err(at, field, format!("`repeat(anim, …)` repeats a tween, keys or sequence, not {}", inner.kind()), None);
                    None
                })?;
                let mode = match self.get(obj, "mode") {
                    Some(Look::Id(id)) if id == "restart" => LoopMode::Restart,
                    Some(Look::Id(id)) if id == "pingpong" || id == "ping_pong" => LoopMode::PingPong,
                    Some(other) => {
                        self.err(at, field, format!("`repeat` mode is @restart or @pingpong, not {}", other.kind()), None);
                        LoopMode::Restart
                    }
                    None => LoopMode::Restart,
                };
                let period = match self.get(obj, "period") {
                    None | Some(Look::Nil) => None,
                    Some(value) => Some(self.duration_of(at, field, &value)?),
                };
                let count = match self.get(obj, "count") {
                    None | Some(Look::Nil) => None,
                    Some(value) => match value.num() {
                        Some(n) if n >= 1.0 && n.fract() == 0.0 => Some(n as u32),
                        _ => {
                            self.err(at, field, format!("`repeat` count is a whole number from 1, not {}", value.kind()), None);
                            None
                        }
                    },
                };
                // Without a period the loop is the keys' own length.
                let period = period.unwrap_or(Dur { expr: Length::Seconds(-1.0), value: -1.0 });
                (track.keys, Some(Repeat { period, mode, count }), track.before, track.after)
            }
            other => {
                self.err(at, field, format!("`{other}` is not an animation; use tween, keys, sequence or repeat"), None);
                return None;
            }
        };
        // Plain seconds can be checked now.
        if let Some(pair) = keys.windows(2).find(|pair| match (&pair[0].t.expr, &pair[1].t.expr) {
            (TimeExpr::Seconds(a), TimeExpr::Seconds(b)) => b <= a,
            _ => false,
        }) {
            let (a, b) = match (&pair[0].t.expr, &pair[1].t.expr) {
                (TimeExpr::Seconds(a), TimeExpr::Seconds(b)) => (*a, *b),
                _ => unreachable!(),
            };
            self.err(at, field, format!("key times must increase: {} s comes after {} s", fmt_num(b), fmt_num(a)), Some("sort the keys by time; two keys at one time make a jump only with a tiny gap (0.001)"));
            return None;
        }
        let last = keys.iter().filter_map(|key| match key.t.expr { TimeExpr::Seconds(s) => Some(s), _ => None }).fold(f64::MIN, f64::max);
        if self.duration > 0.0 && last > self.duration + 1e-9 && at.path.matches('.').count() < 1 && !self.fields(at.obj).iter().any(|(k, _)| k == "span") {
            self.warn(at, field, format!("a key at {} s is after the document's end at {} s; it is never reached", fmt_num(last), fmt_num(self.duration)), None);
        }
        // The resolver supplies local seconds for references and computes
        // implicit periods from the resolved endpoints.
        if keys.iter().any(|key| key.t.expr.is_musical())
            || repeat.as_ref().is_some_and(|r| matches!(r.period.expr, Length::Beats(_)) || r.period.value <= 0.0)
        {
            return Some(Track { keys, repeat, before, after });
        }
        Track::new(keys, repeat, before, after)
            .map_err(|message| self.err(at, field, message, None)).ok()
    }

    /// `svg: keys([t, "M…"], …)` or `svg: tween("M…", "M…", start, end,
    /// @ease)`: outlines that share their commands, morphing.
    fn morph(&mut self, at: &At, value: &Look) -> Option<PathShape> {
        let obj = value.obj()?;
        // `repeat(keys(…), period, mode, count)` of outlines.
        if value.anim() == Some("Loop") {
            let inner = self.get(obj, "inner").unwrap_or(Look::Nil);
            let PathShape::Morph { template, mut track, shapes } = self.morph(at, &inner)? else { return None };
            let mode = match self.get(obj, "mode") {
                Some(Look::Id(id)) if id == "pingpong" || id == "ping_pong" => LoopMode::PingPong,
                _ => LoopMode::Restart,
            };
            let period = match self.get(obj, "period") {
                None | Some(Look::Nil) => Dur { expr: Length::Seconds(-1.0), value: -1.0 },
                Some(value) => self.duration_of(at, "svg", &value)?,
            };
            let count = self.get(obj, "count").and_then(|v| v.num()).filter(|n| *n >= 1.0).map(|n| n as u32);
            track.repeat = Some(Repeat { period, mode, count });
            return Some(PathShape::Morph { template, track, shapes });
        }
        let mut keyed: Vec<(Look, String, Look)> = Vec::new();
        match value.anim() {
            Some("Tween") => {
                let get = |name: &str| self.get(obj, name).unwrap_or(Look::Nil);
                let (from, start, to, end, ease) = (get("from"), get("start"), get("to"), get("end"), get("ease"));
                for (text, time, ease) in [(from, start, Look::Nil), (to, end, ease)] {
                    let Look::Str(text) = text else {
                        self.err(at, "svg", "a morphing `svg` tweens between two path data strings".into(), None);
                        return None;
                    };
                    keyed.push((time, text, ease));
                }
            }
            Some("Keys") => {
                let Some(Look::Arr(points)) = self.get(obj, "points") else { return None };
                for point in &points {
                    match point {
                        Look::Arr(parts) if parts.len() >= 2 => match &parts[1] {
                            Look::Str(text) => keyed.push((parts[0].clone(), text.clone(), parts.get(2).cloned().unwrap_or(Look::Nil))),
                            other => {
                                self.err(at, "svg", format!("a morph key is `[t, \"M…\"]`, not a key of {}", other.kind()), None);
                                return None;
                            }
                        },
                        _ => {
                            self.err(at, "svg", "a morph key is `[t, \"M…\", @ease]`".into(), None);
                            return None;
                        }
                    }
                }
            }
            _ => {
                self.err(at, "svg", "`svg` morphs with `keys([t, \"M…\"], …)` or `tween(\"M…\", \"M…\", start, end, @ease)`".into(), None);
                return None;
            }
        }
        if keyed.is_empty() {
            self.err(at, "svg", "a morph needs at least one `[t, \"M…\"]` key".into(), None);
            return None;
        }
        let parse = |text: &str| {
            check_path_data(text)?;
            crate::geometry::PathTemplate::parse(text)
        };
        let mut compiled = Vec::with_capacity(keyed.len());
        for (index, (_, text, _)) in keyed.iter().enumerate() {
            match parse(text) {
                Ok(shape) => compiled.push(shape),
                Err(message) => {
                    self.err(at, "svg", format!("morph key {index}: {message}"), None);
                    return None;
                }
            }
        }
        // Outlines with other commands (any two shapes): resampled at load
        // into matching commands. Eval only sees the compiled numeric slots.
        let differ = compiled.windows(2).any(|w| w[0].0 != w[1].0);
        if differ {
            let texts: Vec<&str> = keyed.iter().map(|(_, text, _)| text.as_str()).collect();
            let matched = crate::geometry::morphable(&texts);
            for (index, text) in matched.iter().enumerate() {
                match parse(text) {
                    Ok(shape) => compiled[index] = shape,
                    Err(message) => {
                        self.err(at, "svg", format!("morph key {index}: {message}"), None);
                        return None;
                    }
                }
            }
        }
        let mut template = None;
        let mut shapes = Vec::new();
        let mut keys = Vec::new();
        for (index, ((time, _, ease), (commands, numbers))) in keyed.iter().zip(compiled).enumerate() {
            match &template {
                None => template = Some(commands),
                Some(first) if *first != commands => {
                    self.err(at, "svg", format!("morph key {index} has other commands than key 0: every outline of a morph needs the same commands in the same order (M, L, C, … with as many numbers)"),
                        Some("draw every key with the same sequence (convert lines to `C` curves where one key has curves)"));
                    return None;
                }
                Some(_) => {}
            }
            shapes.push(numbers);
            let t = self.time(at, "svg", time)?;
            let ease = self.ease(at, "svg", ease);
            keys.push(Key { t, v: [index as f64], ease: if index == 0 { Ease::Linear } else { ease }, bez: None });
        }
        Some(PathShape::Morph { template: template.unwrap_or_default().into(), track: Track { keys, repeat: None, before: Extrap::Constant, after: Extrap::Constant }, shapes: shapes.into() })
    }

    /// `Effect{name: @glow radius: 24 …}` of a layer's `effects`.
    fn effect_spec(&mut self, parent: &At, index: usize, value: &Look) -> Option<EffectSpec> {
        let obj = match value {
            Look::Obj(obj, ty) if *ty == Some("Effect") || self.get(*obj, "name").is_some() => *obj,
            other => {
                self.err(parent, "effects", format!("effect {index} is {}; write `Effect{{name: @glow …}}`", other.kind()), None);
                return None;
            }
        };
        let at = self.at(obj, format!("{}[{index}]", parent.field("effects")), parent.loc);
        let name = match self.get(obj, "name") {
            Some(Look::Id(name)) | Some(Look::Str(name)) => name.trim_start_matches("kit/").to_string(),
            _ => {
                self.err(&at, "name", "an Effect names its kind: `Effect{name: @glow …}` (see R2's catalogue: glow, bloom, drop_shadow, gaussian_blur, motion_blur, …)".into(), None);
                return None;
            }
        };
        // `on: false`: bypassed (kept as written, not drawn).
        if matches!(self.get(obj, "on"), Some(Look::Bool(false))) {
            return None;
        }
        let mut params = Vec::new();
        for (key, value) in self.fields(obj).into_iter().filter(|(k, _)| k != "name" && k != "on" && !k.starts_with("__")) {
            if let Some(arg) = self.effect_arg(&at, &key, &value) {
                params.push((key, arg));
            }
        }
        Some(EffectSpec { name, params, loc: at.loc })
    }

    /// A render-kit parameter's value: numbers, vectors and colours keyable.
    fn effect_arg(&mut self, at: &At, field: &str, value: &Look) -> Option<EffectArg> {
        Some(match value {
            Look::Bool(b) => EffectArg::Bool(*b),
            Look::Id(id) => EffectArg::Choice(id.clone()),
            Look::Str(text) => EffectArg::Text(text.clone()),
            Look::Color(_) => EffectArg::Color(self.prop_value::<4>(at, field, value)?),
            Look::Fn(..) => EffectArg::Num(self.prop_value::<1>(at, field, value)?),
            Look::Arr(items) if items.len() == 2 && items.iter().all(|v| v.num().is_some()) => EffectArg::Vec2(self.prop_value::<2>(at, field, value)?),
            Look::Arr(items) if items.len() == 3 && items.iter().all(|v| v.num().is_some()) => EffectArg::Vec3(self.prop_value::<3>(at, field, value)?),
            Look::Arr(items) if items.iter().all(|v| vec_n::<2>(v).is_some()) => EffectArg::Points(items.iter().filter_map(vec_n::<2>).collect()),
            Look::Obj(..) if value.anim().is_some() => match self.anim_kind(value) {
                Some(4) => EffectArg::Color(self.prop_value::<4>(at, field, value)?),
                Some(3) => EffectArg::Vec3(self.prop_value::<3>(at, field, value)?),
                Some(2) => EffectArg::Vec2(self.prop_value::<2>(at, field, value)?),
                _ => EffectArg::Num(self.prop_value::<1>(at, field, value)?),
            },
            _ => EffectArg::Num(self.prop_value::<1>(at, field, value)?),
        })
    }

    /// A representative value for fields whose kind is inferred.
    fn anim_sample(&self, value: &Look) -> Option<Look> {
        let obj = value.obj()?;
        match value.anim()? {
            "Tween" => self.get(obj, "from"),
            "Keys" => match self.get(obj, "points") {
                Some(Look::Arr(points)) => points.first().and_then(|p| match p {
                    Look::Arr(parts) => parts.get(1).cloned(),
                    _ => None,
                }),
                _ => None,
            },
            "Sequence" => match self.get(obj, "steps") {
                Some(Look::Arr(steps)) => steps.first().and_then(|s| match s {
                    Look::Arr(parts) => parts.first().cloned(),
                    _ => None,
                }),
                _ => None,
            },
            "Loop" => self.anim_sample(&self.get(obj, "inner")?),
            "Blend" | "Blend2" => self.blend_first(value),
            _ => None,
        }
    }

    /// How many components an animation's values have (a colour has 4).
    fn anim_kind(&self, value: &Look) -> Option<usize> {
        let sample = self.anim_sample(value)?;
        if sample.anim().is_some() { return self.anim_kind(&sample); }
        Some(match sample {
            Look::Color(_) => 4,
            Look::Str(ref text) if parse_hex(text).is_some() => 4,
            Look::Arr(items) => items.len(),
            Look::Obj(obj, _) if self.get(obj, "color").is_some() => 4,
            _ => 1,
        })
    }

    /// Whether an animation's values are colours.
    fn anim_is_color(&self, value: &Look) -> bool {
        self.anim_kind(value) == Some(4)
    }

    fn stagger_of(&mut self, at: &At, field: &str, value: Option<Look>) -> Stagger {
        let Some(value) = value else { return Stagger::none() };
        match &value {
            Look::Nil => Stagger::none(),
            Look::Obj(obj, Some("Stagger")) => {
                let obj = *obj;
                let step = self.get(obj, "step").unwrap_or(Look::Num(0.0));
                let step = self.duration_of(at, field, &step).unwrap_or(Dur::seconds(0.0));
                let seed = self.get(obj, "seed").and_then(|v| v.num()).unwrap_or(1.0) as u32;
                let order = match self.get(obj, "order") {
                    Some(Look::Id(id)) => match id.as_str() {
                        "forward" => StaggerOrder::Forward,
                        "reverse" => StaggerOrder::Reverse,
                        "center" | "centre" => StaggerOrder::Center,
                        "edges" => StaggerOrder::Edges,
                        "random" => StaggerOrder::Random(seed),
                        other => {
                            self.err(at, field, format!("stagger order `@{other}` is not @forward, @reverse, @center, @edges or @random"), None);
                            StaggerOrder::Forward
                        }
                    },
                    _ => StaggerOrder::Forward,
                };
                Stagger { step, order }
            }
            other => Stagger { step: self.duration_of(at, field, other).unwrap_or(Dur::seconds(0.0)), order: StaggerOrder::Forward },
        }
    }

    fn media(&mut self, at: &At, field: &str, value: &Look, allow_markup: bool) -> Option<Media> {
        match self.slot(value) {
            Slot::Input(name) => match self.input(at, field, &name)? {
                InputValue::Asset(asset) => Some(Media::Asset(asset)),
                InputValue::Markup(text) if allow_markup => Some(Media::Inline(text.into())),
                InputValue::Text(text) if allow_markup && text.trim_start().starts_with('<') => Some(Media::Inline(text.into())),
                InputValue::Empty => Some(Media::Empty),
                other => {
                    self.err(at, field, format!("`{field}` is an asset, but input `{name}` is {}", other.describe()), None);
                    None
                }
            },
            Slot::Value(Look::Obj(obj, Some("MissingMedia"))) => {
                let path = self.get(*obj, "path").and_then(|value| match value { Look::Str(path) => Some(path), _ => None }).unwrap_or_default();
                self.warn(at, field, format!("media not present: {path} (plays silent)"), None);
                Some(Media::Empty)
            },
            Slot::Value(Look::Obj(obj, Some("Asset"))) => match self.asset_ref(*obj) {
                Some(asset) => Some(Media::Asset(asset)),
                None => {
                    self.err(at, field, format!("`asset(id, rev)` takes the asset id (`ast_…`) and revision (`arev_…`) as strings"), None);
                    None
                }
            },
            Slot::Value(Look::Str(text)) if allow_markup && text.trim_start().starts_with('<') => Some(Media::Inline(text.as_str().into())),
            Slot::Value(other) => {
                let what = if allow_markup { "`asset(id, rev)`, `in(@name)` or SVG markup" } else { "`asset(id, rev)` or `in(@name)`" };
                self.err(at, field, format!("`{field}` is {what}, not {}", other.kind()), None);
                None
            }
        }
    }

    /// `wrap:` / `fit:` — a width in document units, or `@safe`: the
    /// document's width inside the side margins ([`crate::SAFE_WIDTH`]).
    fn width_of(&mut self, at: &At, field: &str, value: Option<Look>) -> Option<f64> {
        match value {
            None | Some(Look::Nil) => None,
            Some(Look::Id(id)) if id == "safe" => Some(self.width * crate::SAFE_WIDTH),
            Some(value) => match value.num() {
                Some(w) if w > 0.0 => Some(w),
                _ => {
                    self.err(at, field, format!("`{field}` is a width in document units or `@safe`, not {}", value.kind()),
                        Some("`fit: @safe` keeps the text inside the frame's side margins; `fit: 900` inside 900 units"));
                    None
                }
            },
        }
    }

    /// `{name: asset(…) | in(@input)}`: pictures by the name materials use.
    #[cfg(feature = "scene3d")]
    fn named_media(&mut self, at: &At, field: &str, value: Option<Look>) -> Vec<(String, Media)> {
        match value {
            None => Vec::new(),
            Some(Look::Obj(map, _)) => {
                let mut out = Vec::new();
                for (name, value) in self.fields(map).into_iter().filter(|(k, _)| !k.starts_with("__")) {
                    if let Some(m) = self.media(at, field, &value, false) {
                        out.push((name, m));
                    }
                }
                out
            }
            Some(other) => {
                self.err(at, field, format!("`{field}` maps names to assets (`{{screen: asset(id, rev)}}`), not {}", other.kind()), None);
                Vec::new()
            }
        }
    }

    /// A layer's `size` when written (a negative constant is an error).
    fn opt_size_of(&mut self, at: &At, field: &str, value: Option<Look>) -> Option<Prop<2>> {
        let prop = match value {
            None | Some(Look::Nil) => None,
            Some(value) => self.prop_value::<2>(at, field, &value),
        };
        if let Some(Prop::Const(size)) = &prop {
            if size[0] < 0.0 || size[1] < 0.0 {
                self.err(at, "size", format!("`size` {} x {} is negative", fmt_num(size[0]), fmt_num(size[1])), None);
            }
        }
        prop
    }

    // ---- layers ------------------------------------------------------

    fn layers(&mut self, at: &At, field: &str, items: &[Look]) -> Vec<Layer> {
        let mut out = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let path = format!("{}[{index}]", at.field(field));
            if let Some(layer) = self.layer(at, field, &path, item) {
                out.push(layer);
            }
        }
        out
    }

    fn layer(&mut self, parent: &At, field: &str, path: &str, value: &Look) -> Option<Layer> {
        self.layer_with(parent, field, path, value, None)
    }

    /// [`Self::layer`] where a layer without its own `blur` takes `blur_default`
    /// (a Group's `mask_feather` for its mask).
    fn layer_with(&mut self, parent: &At, field: &str, path: &str, value: &Look, blur_default: Option<Look>) -> Option<Layer> {
        let obj = match value {
            Look::Obj(obj, Some(p)) if *p != "Motion" => *obj,
            Look::Nil => return None,
            other => {
                let hint = format!("layer types: {}", crate::layers::layer_types().collect::<Vec<_>>().join(", "));
                let loc = match other {
                    Look::Obj(obj, _) => self.obj_loc(*obj).unwrap_or(parent.loc),
                    _ => self.loc_of(parent, field),
                };
                self.diags.push(Diagnostic::error(loc, path.to_string(), format!("{path} is {}, not a layer", other.kind())).hint(hint));
                return None;
            }
        };
        self.layer_of(parent, field, path, obj, false, blur_default)
    }

    /// One layer from its object; `without_at` reads it as if it had no `at`
    /// (the layer an `at: anchor(…)` wraps), `blur_default` stands in for a
    /// missing `blur`.
    fn layer_of(&mut self, parent: &At, field: &str, path: &str, obj: ScriptObject, without_at: bool, blur_default: Option<Look>) -> Option<Layer> {
        let kind = obj::type_name(self.vm, obj.into()).unwrap_or_default().to_string();
        let at = self.at(obj, path.to_string(), parent.loc);
        let common = crate::layers::Common::script_from_value(self.vm, obj.into());
        let at_value = if without_at { None } else { common.at.look(self) };
        // `at: anchor("scene", "node")`: the layer follows a 3D node.
        if let Some(Look::Obj(a, Some("Anchor"))) = at_value {
            let scene = match self.get(a, "scene") { Some(Look::Str(s)) => s, _ => String::new() };
            let node = match self.get(a, "node") { Some(Look::Str(s)) => s, _ => String::new() };
            if scene.is_empty() || node.is_empty() {
                self.err(&at, "at", "`anchor(\"scene\", \"node\")` names a Scene3D layer and one of its named nodes".into(), None);
                return None;
            }
            let mut layer = self.layer_of(parent, field, path, obj, true, blur_default)?;
            let (name, span) = (layer.name.clone(), layer.span.clone());
            let own_path = layer.path.clone();
            layer.path = format!("{own_path}.anchored");
            let mut outer = layer.clone();
            outer.name = name;
            outer.span = span;
            outer.path = own_path;
            outer.transform = Transform::default();
            outer.opacity = Prop::Const([1.0]);
            outer.blur = Prop::Const([0.0]);
            outer.feather = Prop::Const([0.0]);
            outer.feather_side = FeatherSide::Both;
            outer.effects = Vec::new();
            outer.animated = true;
            layer.span = None;
            outer.kind = LayerKind::Anchored { layer: Box::new(layer), scene, node };
            return Some(outer);
        }
        let name = common.name.text(self, &at, "name");
        let span = match common.span.look(self) {
            None => None,
            Some(Look::Arr(parts)) if parts.len() == 2 => {
                let start = self.time(&at, "span", &parts[0]);
                let length = self.duration_of(&at, "span", &parts[1]);
                match (start, length) {
                    (Some(start), Some(length)) => {
                        if length.value <= 0.0 && matches!(length.expr, Length::Seconds(_)) {
                            self.err(&at, "span", "`span: [start, length]` has a positive length".into(), None);
                        }
                        Some(Span { start, length })
                    }
                    _ => None,
                }
            }
            Some(other) => {
                self.err(&at, "span", format!("`span` is `[start, length]` in seconds (start may be beat(n) etc., length beats(x)), not {}", other.kind()), None);
                None
            }
        };
        let anchor = common.anchor.konst(self, &at, "anchor", [0.5, 0.5]);
        // `at: along(path, progress)`: the origin rides a path.
        let along = match &at_value {
            Some(Look::Obj(o, Some("Along"))) => {
                let o = *o;
                let path = match self.get(o, "path").unwrap_or(Look::Nil) {
                    Look::Str(data) => match check_path_data(&data) {
                        Ok(()) => Some(data),
                        Err(message) => {
                            self.err(&at, "at", format!("`along(…)`'s path: {message}"), None);
                            None
                        }
                    },
                    other => match points_of(&other) {
                        Some(points) if points.len() >= 2 => Some(points_path(&points, false, false)),
                        _ => {
                            self.err(&at, "at", format!("`along(path, progress)` takes SVG path data or at least two `vec2` points, not {}", other.kind()), None);
                            None
                        }
                    },
                };
                let progress = match self.get(o, "progress") {
                    Some(p) => self.prop_value::<1>(&at, "at", &p).unwrap_or(Prop::Const([0.0])),
                    None => Prop::Const([0.0]),
                };
                path.map(|path| Along { path: path.into(), progress })
            }
            _ => None,
        };
        let transform = Transform {
            at: if along.is_some() { Prop::Const([0.0, 0.0]) } else { self.prop_of::<2>(&at, "at", at_value.clone(), [0.0, 0.0]) },
            along,
            x: common.x.prop(self, &at, "x", 0.0),
            y: common.y.prop(self, &at, "y", 0.0),
            scale: common.scale.prop(self, &at, "scale", [1.0, 1.0]),
            rotate: common.rotate.prop(self, &at, "rotate", 0.0),
            anchor,
        };
        let opacity = common.opacity.prop(self, &at, "opacity", 1.0);
        let blur_value = common.blur.look(self).or(blur_default);
        let blur = self.prop_of::<1>(&at, "blur", blur_value, [0.0]);
        let feather = common.feather.prop(self, &at, "feather", 0.0);
        let feather_side = common.feather_side.pick(self, &at, "feather_side", FeatherSide::Both);
        let negative = match &feather {
            Prop::Const([value]) => *value < 0.0,
            Prop::Track(track) => track.keys.iter().any(|key| key.v[0] < 0.0),
            Prop::Fn(_) | Prop::Blend(_) => false,
        };
        if negative {
            self.err(&at, "feather", "`feather` expects a non-negative softness in document units (including every key value)".into(), None);
        }
        let effects = match common.effects.look(self) {
            None => Vec::new(),
            Some(Look::Arr(items)) => items.iter().enumerate().filter_map(|(index, item)| self.effect_spec(&at, index, item)).collect(),
            Some(other) => {
                self.err(&at, "effects", format!("`effects` is a list of `Effect{{name: @glow …}}`, not {}", other.kind()), Some("`effects: [Effect{name: @glow radius: 24}, Effect{name: @drop_shadow}]`"));
                Vec::new()
            }
        };
        at.note("opacity", || None, Some([0.0, 1.0]));
        if let Prop::Const([o]) = opacity {
            if !(0.0..=1.0).contains(&o) {
                self.warn(&at, "opacity", format!("`opacity` {} is outside 0..1 and is clamped", fmt_num(o)), None);
            }
        }
        let blend = common.blend.pick(self, &at, "blend", Blend::Normal);
        let kind_name = kind.clone();
        let kind = match kind.as_str() {
            "Rect" => crate::layers::Rect::script_from_value(self.vm, obj.into()).read(self, &at, path),
            "Ellipse" => crate::layers::Ellipse::script_from_value(self.vm, obj.into()).read(self, &at, path),
            "Path" => crate::layers::Path::script_from_value(self.vm, obj.into()).read(self, &at, path),
            "Svg" => crate::layers::Svg::script_from_value(self.vm, obj.into()).read(self, &at),
            "Text" => crate::layers::Text::script_from_value(self.vm, obj.into()).read(self, &at),
            "Image" => crate::layers::Image::script_from_value(self.vm, obj.into()).read(self, &at),
            "Video" => crate::layers::Video::script_from_value(self.vm, obj.into()).read(self, &at),
            "Bones" => crate::layers::Bones::script_from_value(self.vm, obj.into()).read(self, &at),
            "Group" => crate::layers::Group::script_from_value(self.vm, obj.into()).read(self, &at, path),
            "Shader" => crate::layers::Shader::script_from_value(self.vm, obj.into()).read(self, &at),
            "KineticText" => crate::layers::KineticText::script_from_value(self.vm, obj.into()).read(self, &at),
            "Text3D" => {
                self.check_open(&at, "Text3D");
                crate::layers::Text3D::script_from_value(self.vm, obj.into()).read(self, &at)
            }
            #[cfg(feature = "scene3d")]
            "Seq3D" => {
                self.check_open(&at, "Seq3D");
                crate::layers::Seq3D::script_from_value(self.vm, obj.into()).read(self, &at)
            }
            #[cfg(not(feature = "scene3d"))]
            "Seq3D" => {
                self.err(&at, "", "Seq3D needs makepad-motion's `scene3d` feature (motion3d lowers it)".to_string(), None);
                LayerKind::Group { layers: Vec::new(), mask: None, props: GroupProps { mask_mode: MaskMode::Alpha, stagger: crate::model::Stagger::none(), size: None }, post: Default::default(), glyphs: false }
            }
            "Vector" => crate::layers::Vector::script_from_value(self.vm, obj.into()).read(self, &at),
            "Repeat" => match crate::layers::Repeat::script_from_value(self.vm, obj.into()).read(self, &at, path) {
                Some(kind) => kind,
                None => return None,
            },
            "Crowd" => crate::layers::Crowd::script_from_value(self.vm, obj.into()).read(self, &at),
            "FlyThrough" => crate::layers::FlyThrough::script_from_value(self.vm, obj.into()).read(self, &at),
            "Character" => crate::layers::Character::script_from_value(self.vm, obj.into()).read(self, &at),
            "Freeze" => crate::layers::Freeze::script_from_value(self.vm, obj.into()).read(self, &at),
            "Scene3D" => crate::layers::Scene3D::script_from_value(self.vm, obj.into()).read(self, &at, path),
            "Widget" => crate::layers::Widget::script_from_value(self.vm, obj.into()).read(self, &at),
            "Draw2D" => match crate::layers::Draw2D::script_from_value(self.vm, obj.into()).read(self, &at) {
                Some(kind) => kind,
                None => return None,
            },
            other => {
                self.err(&at, "", format!("`{other}` is not a layer type"), Some(&format!("layer types: {}", crate::layers::layer_types().collect::<Vec<_>>().join(", "))));
                return None;
            }
        };
        // A KineticText's `fit`: its picture drawn smaller about its centre,
        // so whatever the preset puts in its frame lands inside the width.
        let mut transform = transform;
        if kind_name == "KineticText" && !common.scale.is_set() {
            if let Some(width) = crate::layers::KineticText::script_from_value(self.vm, obj.into()).fit.width(self, &at, "fit") {
                let box_w = match &kind {
                    LayerKind::Shader { size: Some(Prop::Const(size)), .. } => size[0],
                    _ => self.width,
                };
                let f = (width / box_w.max(1.0)).min(1.0);
                transform.scale = Prop::Const([f, f]);
            }
        }
        let visible = common.visible.flag(self, &at, "visible", true);
        let locked = common.locked.flag(self, &at, "locked", false);
        let hard_cut = common.hard_cut.flag(self, &at, "hard_cut", false);
        let data = match common.data_layer.look(self) {
            None | Some(Look::Bool(false)) => None,
            Some(Look::Bool(true)) => Some(DataLayer { mips: false, aniso: 1 }),
            Some(Look::Obj(o, _)) => Some(DataLayer {
                mips: matches!(self.get(o, "mips"), Some(Look::Bool(true))),
                aniso: self.get(o, "aniso").and_then(|v| v.num()).map_or(1, |n| n.clamp(1.0, 16.0) as u32),
            }),
            Some(other) => {
                self.err(&at, "data_layer", format!("`data_layer` is true or {{mips: true aniso: 8}}, not {}", other.kind()), None);
                None
            }
        };
        if let Some(d) = data {
            if name.is_none() {
                self.err(&at, "data_layer", "a data layer needs a `name` (post passes read it by name)".into(), Some("`Group{name: \"source\" data_layer: true layers: [...]}`, then `Pass{reads: [\"source\"] ...}`"));
            }
            if !path.starts_with("layers[") || path.matches('[').count() != 1 {
                self.err(&at, "data_layer", "a data layer is a root layer of the document".into(), None);
            }
            if d.mips || d.aniso > 1 {
                self.warn(&at, "data_layer", "`mips` and `aniso` are not built yet: the texture is sampled at one level".into(), None);
            }
        }
        let mut layer = Layer { name, path: path.to_string(), loc: at.loc, span, transform, opacity, blur, feather, feather_side, effects, blend, kind, animated: false, visible, locked, hard_cut, data };
        layer.animated = animated(&layer);
        Some(layer)
    }
}

/// Points of `[vec2, …]` (each a Splash vector or `[x, y]`).
fn points_of(value: &Look) -> Option<Vec<[f64; 2]>> {
    let Look::Arr(items) = value else { return None };
    items.iter().map(|item| vec_n::<2>(item)).collect()
}

pub use makepad_motion_plain::geometry::points_path;

/// Uniform names every inline shader has.
pub const RESERVED_UNIFORMS: &[&str] = &["time", "beat", "size", "below", "data", "opacity"];

fn offset_expr(start: &TimeExpr, by: f64) -> TimeExpr {
    match start {
        TimeExpr::Seconds(s) => TimeExpr::Seconds(s + by),
        TimeExpr::At { at, offset } => TimeExpr::At { at: at.clone(), offset: offset + by },
    }
}

/// Whether anything about a layer changes over its span.
pub(crate) fn animated(layer: &Layer) -> bool {
    let t = &layer.transform;
    let mut any = t.is_animated() || layer.opacity.is_animated() || layer.blur.is_animated() || layer.feather.is_animated()
        || layer.effects.iter().any(|e| e.params.iter().any(|(_, a)| a.is_animated()) || matches!(e.name.as_str(), "motion_blur" | "echo" | "noise" | "grain" | "particles" | "turbulence" | "slices"));
    let opt4 = |p: &Option<Prop<4>>| p.as_ref().is_some_and(Prop::is_animated);
    any |= match &layer.kind {
        LayerKind::Rect(props) => props.is_animated(),
        LayerKind::Ellipse(props) => props.is_animated(),
        LayerKind::Path(props) => props.is_animated(),
        LayerKind::Svg { size, .. } => size.as_ref().is_some_and(Prop::is_animated),
        // A backdrop shows whatever is under it.
        LayerKind::Image { src, size, zoom, focus, .. } => *src == Media::Backdrop || size.as_ref().is_some_and(Prop::is_animated) || zoom.is_animated() || focus.as_ref().is_some_and(Prop::is_animated),
        LayerKind::Text { source, props: TextProps { size, color, reveal, tracking, .. }, plate, caret, axes, ranges, path, flicker, stroke, .. } => {
            path.as_ref().is_some_and(|(_, offset)| offset.is_animated()) ||
            matches!(source, TextSource::Voice(_)) || matches!(source, TextSource::Number { value, clock, .. } if value.is_animated() || clock.is_some())
                || size.is_animated() || color.is_animated() || axes.iter().any(|(_, a)| a.is_animated())
                || ranges.iter().any(TextRange::is_animated)
                || plate.as_ref().is_some_and(|p| p.color.is_animated() || p.reveal.is_animated() || p.stroke.as_ref().is_some_and(Prop::is_animated))
                || reveal.as_ref().is_some_and(Prop::is_animated) || caret.as_ref().is_some_and(|(o, c, _)| o.is_animated() || c.is_animated())
                || flicker.is_some() || tracking.is_animated()
                || stroke.as_ref().is_some_and(|(c, w)| c.is_animated() || w.is_animated())
        }
        // It bobs on the beat.
        LayerKind::Crowd(_) => true,
        LayerKind::Character(_) => true,
        LayerKind::Bones(rig) => rig.is_animated(),
        // Still, unless what it freezes moves: kept animated (its picture
        // is the layers under it at a fixed time, drawn as they are drawn).
        LayerKind::Freeze { .. } => true,
        LayerKind::FlyThrough { .. } => true,
        LayerKind::Anchored { .. } => true,
        // A video's frame changes with time.
        LayerKind::Video { .. } => true,
        LayerKind::Group { layers, mask, props: GroupProps { size, .. }, .. } => layers.iter().any(|l| l.animated) || mask.as_ref().is_some_and(|m| m.animated) || size.as_ref().is_some_and(Prop::is_animated),
        // Shaders run on time (and effects on the frame below).
        LayerKind::Shader { .. } => true,
        LayerKind::Scene3D { camera, lights, nodes, materials, background, exposure, .. } => {
            camera.position.is_animated() || camera.target.is_animated() || camera.yaw.is_animated() || camera.pitch.is_animated()
                || camera.distance.is_animated() || camera.fov.is_animated()
                || lights.iter().any(|l| l.direction.is_animated() || l.position.is_animated() || l.color.is_animated() || l.intensity.is_animated())
                || nodes.iter().any(|n| n.translate.is_animated() || n.rotate.is_animated() || n.scale.is_animated() || n.visible.is_animated())
                || materials.iter().any(|m| opt4(&m.base_color) || opt4(&m.emissive) || m.metallic.as_ref().is_some_and(Prop::is_animated) || m.roughness.as_ref().is_some_and(Prop::is_animated))
                || opt4(background) || exposure.is_animated()
        }
        LayerKind::Widget { size, props, .. } => size.is_animated() || props.iter().any(|(_, p)| match p {
            WidgetProp::Num(p) => p.is_animated(),
            WidgetProp::Color(p) => p.is_animated(),
            WidgetProp::Text(_) => false,
        }),
        LayerKind::Draw2d { .. } | LayerKind::Scene3dDoc { .. } => true,
        LayerKind::Vector { size, parts, draw_on, fill, stroke, .. } => size.as_ref().is_some_and(Prop::is_animated) || draw_on.as_ref().is_some_and(Prop::is_animated) || opt4(fill) || opt4(stroke)
            || parts.iter().any(|(_, p)| p.opacity.is_animated() || p.x.is_animated() || p.y.is_animated() || p.scale.is_animated() || p.rotate.is_animated()
                || opt4(&p.fill) || opt4(&p.stroke) || p.width.as_ref().is_some_and(Prop::is_animated) || p.draw_on.as_ref().is_some_and(Prop::is_animated)),
        LayerKind::Repeat { layer, items, props: RepeatProps { delay, .. }, .. } => std::iter::once(layer.as_ref()).chain(items).any(|layer| layer.animated || (*delay != 0.0 && layer.span.is_some())),
    };
    any
}


pub(crate) fn fits(ty: InputType, value: &InputValue) -> bool {
    matches!((ty, value),
        (InputType::List, InputValue::List(_))
        | (InputType::Text, InputValue::Text(_))
        | (InputType::Number, InputValue::Number(_))
        | (InputType::Color, InputValue::Color(_))
        | (InputType::Vec2, InputValue::Vec2(_))
        | (InputType::Bool, InputValue::Bool(_))
        | (InputType::Image | InputType::Video | InputType::Model, InputValue::Asset(_) | InputValue::Empty)
        | (InputType::Svg, InputValue::Asset(_) | InputValue::Markup(_) | InputValue::Empty))
}

/// A host value in the form an input of `ty` takes (a number for a text
/// input becomes its text, SVG markup given as text becomes markup).
pub(crate) fn coerce_param(ty: InputType, value: InputValue) -> InputValue {
    match (ty, value) {
        (InputType::Color, InputValue::Text(text)) if parse_hex(&text).is_some() => InputValue::Color(parse_hex(&text).unwrap()),
        (InputType::Text, InputValue::Number(n)) => InputValue::Text(fmt_num(n)),
        (InputType::Svg, InputValue::Text(text)) if text.trim_start().starts_with('<') => InputValue::Markup(text),
        (InputType::Vec2, InputValue::Number(n)) => InputValue::Vec2([n, n]),
        // Two numbers read as a vec2 until the input says it is a list.
        (InputType::List, InputValue::Vec2([x, y])) => InputValue::List(vec![InputValue::Number(x), InputValue::Number(y)]),
        (_, value) => value,
    }
}

fn type_of(value: &InputValue) -> InputType {
    match value {
        InputValue::Text(_) => InputType::Text,
        InputValue::Number(_) => InputType::Number,
        InputValue::Color(_) => InputType::Color,
        InputValue::Vec2(_) => InputType::Vec2,
        InputValue::Bool(_) => InputType::Bool,
        InputValue::Asset(_) | InputValue::Empty => InputType::Image,
        InputValue::Markup(_) => InputType::Svg,
        // A record is only ever an item of a list.
        InputValue::List(_) | InputValue::Record(_) => InputType::List,
    }
}

pub(crate) use makepad_motion_plain::unpack;

pub(crate) fn pack(c: Color) -> u32 {
    let b = |v: f64| ((v.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
    b(c[0]) << 24 | b(c[1]) << 16 | b(c[2]) << 8 | b(c[3])
}

pub(crate) fn parse_hex(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#')?;
    makepad_script::colorhex::hex_bytes_to_u32(hex.as_bytes()).ok().map(unpack)
}

fn vec_n<const N: usize>(value: &Look) -> Option<[f64; N]> {
    let Look::Arr(items) = value else { return None };
    if items.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = item.num()?;
    }
    Some(out)
}

pub use makepad_motion_plain::geometry::check_path_data;

#[cfg(test)]
mod tests {
    use crate::plain::{PrimitiveFrame, RectProps};
    use super::*;
    use crate::{ItemKind, MotionFrame};

    #[test]
    fn a_document_computes_with_its_inputs_given_or_defaulted() {
        let source = "use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 inputs: {w: input(@number, 40)}\n  layers: [Rect{size: vec2(in(@w) * 0.5, 10)}]}";
        let size = |doc: &MotionDoc| match &doc.layers[0].kind {
            LayerKind::Rect(RectProps { size: Prop::Const(s), .. }) => s[0],
            other => panic!("{other:?}"),
        };
        let defaulted = MotionDoc::load(source).unwrap();
        assert_eq!(size(&defaulted), 20.0);
        let given = MotionDoc::load_with(source, &[(LiveId::from_str("w"), InputValue::Number(60.0))], MotionContext::default()).unwrap();
        assert_eq!(size(&given), 30.0);
        assert_eq!(size(&given.with_inputs(&[]).unwrap()), 20.0);
    }

    /// How many script bodies (parsed texts) the document's VM holds.
    fn bodies(doc: &MotionDoc) -> usize {
        doc.vm.with(|e| e.with_vm(|vm| vm.body_count()))
    }

    #[test]
    fn a_structural_input_evaluates_again_in_the_kept_vm_without_parsing() {
        let body = "Motion{size: vec2(100, 100) duration: 1 inputs: {w: input(@number, 40)}\n  layers: [Rect{size: vec2(in(@w) * 0.5, 10)}]}";
        let image = {
            let host = Box::leak(Box::new(ScriptVmHost::new(0i32, ())));
            let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
            let f = vm.eval(ScriptMod::new("", "", "game.splash", 0, 0, &format!("|| {body}"), &[]));
            vm.fn_image(f.as_object().unwrap()).unwrap()
        };
        let size = |doc: &MotionDoc| match &doc.layers[0].kind {
            LayerKind::Rect(RectProps { size: Prop::Const(s), .. }) => s[0],
            other => panic!("{other:?}"),
        };
        let w = LiveId::from_str("w");
        for doc in [MotionDoc::load(&format!("use mod.motion.*; {body}")).unwrap(), MotionDoc::load_image(&image, &[], MotionContext::default()).unwrap()] {
            assert_eq!(size(&doc), 20.0);
            let i = doc.input_index(w).unwrap();
            let mut mine = doc.new_inputs();
            let bodies_before = bodies(&doc);
            let evaluations = doc.evaluations();
            // (Each value differs from the declared default the first run used: it runs twice.)
            for n in 1..=10 {
                assert_eq!(doc.write_input(&mut mine, i, InputValue::Number(n as f64 * 10.0)), Ok(InputWrite::Reevaluate));
                let again = doc.reevaluate(&mine).unwrap();
                assert_eq!(size(&again), n as f64 * 5.0);
            }
            assert_eq!(bodies(&doc), bodies_before, "no body was added: nothing was parsed");
            assert!(doc.evaluations() > evaluations + 9);
            // A host that only has the inputs gets the same.
            assert_eq!(size(&doc.with_inputs(&[(w, InputValue::Number(80.0))]).unwrap()), 40.0);
        }
    }

    #[test]
    fn a_repeat_source_input_is_structural() {
        let doc = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 inputs: {rows: input(@list, ["a", "b"]) n: 1.0}
            layers: [Repeat{source: input(@rows) layer: Text{text: item() size: 10}} Rect{size: vec2(4, 4) x: fn(t) { in(@n) }}]}"#).unwrap();
        let rows = doc.input_index(LiveId::from_str("rows")).unwrap();
        let n = doc.input_index(LiveId::from_str("n")).unwrap();
        let mut mine = doc.new_inputs();
        assert_eq!(doc.write_input(&mut mine, n, InputValue::Number(2.0)), Ok(InputWrite::Applied));
        assert_eq!(doc.write_input(&mut mine, rows, InputValue::List(vec![InputValue::Text("c".into())])), Ok(InputWrite::Reevaluate));
    }

    #[test]
    fn documents_of_one_vm_survive_collection_and_each_other() {
        let body = "use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 inputs: {w: input(@number, 40) k: 1.0}\n  layers: [Rect{size: vec2(in(@w) * 0.5, 10) x: fn(t) { in(@k) * 10.0 }}]}";
        let size = |frame: &MotionFrame| match &frame.items[0].kind {
            ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) => size[0],
            other => panic!("{other:?}"),
        };
        let gc = |doc: &MotionDoc| doc.vm.with(|e| e.with_vm(|vm| vm.gc()));
        let first = MotionDoc::load(body).unwrap();
        let w = first.input_index(LiveId::from_str("w")).unwrap();
        let mut mine = first.new_inputs();
        first.write_input(&mut mine, w, InputValue::Number(80.0)).unwrap();
        let second = first.reevaluate(&mine).unwrap();
        for frame in 0..20 {
            gc(&first);
            assert_eq!(size(&first.eval(0.0)), 20.0);
            gc(&second);
            assert_eq!(size(&second.eval_with(0.0, &mut mine)), 40.0);
            if frame == 10 {
                drop(first.clone());
            }
        }
        let third = second.reevaluate(&mine).unwrap();
        drop(second);
        gc(&third);
        assert_eq!(size(&third.eval_with(0.0, &mut mine)), 40.0);
        // The VM's inputs are the load's after a re-evaluation, not the instance's.
        assert_eq!(third.vm.with(|e| e.host.host.inputs.iter().find(|(id, _)| *id == LiveId::from_str("w")).map(|(_, v)| v.clone())), Some(InputValue::Number(40.0)));
    }

    #[test]
    fn hex_colours_and_path_data_are_checked() {
        assert_eq!(parse_hex("#fff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse_hex("#ff000080").map(|c| (c[0], (c[3] * 255.0).round())), Some((1.0, 128.0)));
        assert!(parse_hex("red").is_none());
        assert_eq!(unpack(pack([1.0, 0.0, 0.5, 1.0])).map(|v| (v * 255.0).round()), [255.0, 0.0, 128.0, 255.0]);
        assert!(check_path_data("M0 0 L400 0").is_ok());
        assert!(check_path_data("L0 0").unwrap_err().contains("starts with a move"));
        assert!(check_path_data("M0 0 X4").unwrap_err().contains("`X`"));
    }
}

/// The library files a motion3d scene names: every `Asset` (`asset(id,
/// rev)`) reachable from the scene object, each once (any id: a local media
/// table names files its own way, `asset("lib-8962", "1")`). The layer's own
/// fields (its place and pictures) are the layer's, not the scene's.
#[cfg(feature = "scene3d")]
pub(crate) fn scene_assets(vm: &ScriptVm, scene: ScriptObject) -> Vec<AssetRef> {
    fn walk(vm: &ScriptVm, v: ScriptValue, seen: &mut std::collections::HashSet<ScriptObject>, out: &mut Vec<AssetRef>) {
        if let Some(items) = v.as_array().and_then(|_| obj::items(vm, v)) {
            for item in items {
                walk(vm, item, seen, out);
            }
            return;
        }
        let Some(o) = v.as_object() else { return };
        if vm.bx.heap_ref().is_fn(o) || !seen.insert(o) {
            return;
        }
        if obj::is(vm, v, "Asset") {
            let text = |name: &str| obj::string(vm, obj::get(vm, o, name));
            if let (Some(id), Some(rev)) = (text("id"), text("rev")) {
                let asset = AssetRef { id, rev };
                if !id_is_blank(&asset) && !out.contains(&asset) {
                    out.push(asset);
                }
            }
            return;
        }
        for (name, value, _) in obj::fields(vm, o) {
            if !name.starts_with("__") {
                walk(vm, value, seen, out);
            }
        }
        if let Some(items) = obj::items(vm, v) {
            for item in items {
                walk(vm, item, seen, out);
            }
        }
        // A plain object written as a list keeps its items in its vector part.
        for index in 0..vm.bx.heap_ref().vec_len(o) {
            let entry = vm.bx.heap_ref().vec_key_value(o, index, NoTrap);
            if entry.key.is_nil() {
                walk(vm, entry.value, seen, out);
            }
        }
    }
    fn id_is_blank(asset: &AssetRef) -> bool {
        asset.id.is_empty() || asset.rev.is_empty() || asset.id.contains(char::is_whitespace)
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    seen.insert(scene);
    for (name, value, _) in obj::fields(vm, scene) {
        if !name.starts_with("__") && !crate::value::SCENE_KEEP.contains(&name.as_str()) {
            walk(vm, value, &mut seen, &mut out);
        }
    }
    out
}
