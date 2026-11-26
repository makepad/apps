//! The Splash side of a Motion document: `mod.motion` (the layer prototypes,
//! the animation and time helpers, `in()`, `asset()`, the audio feature
//! readers), evaluation of a document's source in a fresh VM, and reading the
//! result into plain Rust values that remember where they were written.
//!
//! A document is full Splash: `let` bindings, functions, loops that build
//! layers, prototypes of one's own (`let Card = Rect{…}` then `Card{…}`)
//! all work. It evaluates once, in one VM: its 3D scenes (`Scene3D{…}`
//! objects) are objects of that VM too, read by motion3d from it (the scene
//! vocabulary is `mod.scene`, installed beside `mod.motion` with `mod.forms`,
//! `mod.post` and `mod.kernel`; a document names the ones it uses). A Widget's `ui:` and an inline Shader's
//! `pixel: fn() {…}` are functions this VM never calls (the drawing side
//! compiles their tokens).

use crate::features::WordState;
use crate::signals::SignalSource;
use crate::model::{DocSource, InputValue, Loc};
use makepad_script::*;
use std::sync::Arc;

/// The file name documents evaluate under.
pub const DOC_FILE: &str = "motion.splash";

/// The name the document's own text goes by where its code is compiled
/// apart (a post pass, a Shader layer's function): what their errors and
/// edit mode's live shader literal sites name it.
pub const DOC_CODE_FILE: &str = "motion";
const PRELUDE_FILE: &str = "mod.motion";
/// What every document gets in front of its first line: the language core.
/// `mod.std`, Splash's math (`sin`, `mix`, `clamp`, ...) and the vector
/// types (`vec2(1, 2) * 0.5` works). Everything else a document names with
/// a `use` of its own: `use mod.motion.*` (its layers, time and inputs, the
/// forms and the shared Splash stdlib `ease(@name, u)`, `wrap`, `rot2`,
/// `sd_*`, ...), and `use mod.scene`, `use mod.post`, `use mod.kernel`
/// qualified (`scene.Mesh{..}`, `post.Glow{..}`, `kernel.input(..)`).
pub(crate) const MATH_PREFIX: &str = "use mod.std.*; use mod.math.*; use mod.pod.*; ";
/// Instructions one evaluation (or one property function call) may run: a
/// runaway loop is an error, not a hang.
pub(crate) const INSTRUCTION_LIMIT: usize = 20_000_000;

/// The load: the top-level evaluation that builds the document. Bounded by
/// the instructions it runs, never by the clock (makepad's EvalLimits has no
/// time), so a heavy plate loads the same on a busy machine.
pub(crate) const LOAD_LIMITS: EvalLimits = EvalLimits::DOC_LOAD.with_instructions(INSTRUCTION_LIMIT as u64);
/// One property function or live binding, per frame.
pub(crate) const FRAME_LIMITS: EvalLimits = EvalLimits::DOC_FRAME.with_instructions(INSTRUCTION_LIMIT as u64);
/// A whole Draw2D scene drawn per frame: the load's bounds.
pub(crate) const DRAW_LIMITS: EvalLimits = EvalLimits::DOC_LOAD;

/// `mod.motion`'s own helpers in Splash (`prelude.splash`): the ones that
/// make the value components (`crate::layers`) the loader reads by their
/// type. Natives (the audio feature readers) are added in Rust. The
/// animation forms and the pure timing and lyric helpers are `mod.forms`'
/// ([`crate::forms`]); `mod.motion` re-exports them. The lyric film's own
/// kit is the `film` module (`modules/film.splash`).
const MOTION_PRELUDE: &str = include_str!("prelude.splash");

/// The Splash prelude of the document vocabulary, for display (the control
/// API serves it to authors): `mod.forms` (the animation forms) and
/// `mod.motion`'s helpers, then the pure helpers of `mod.forms`. The VM
/// evaluates them apart, into their modules.
pub const PRELUDE: &str = concat!(include_str!("forms.splash"), include_str!("prelude.splash"), include_str!("kit_pure.splash"));

/// The animation components.
pub(crate) const ANIMS: &[&str] = &["Tween", "Keys", "Sequence", "Loop", "Blend", "Blend2"];
/// The time reference components (`Beats` is a length).
pub(crate) const TIMES: &[&str] = &["Beat", "Bar", "Downbeat", "Onset", "Section", "Word", "Line", "Edit", "Beats"];

pub(crate) fn fmt_num(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

/// How the evaluated source maps back to the author's.
#[derive(Clone, Debug)]
pub(crate) struct SourceMap {
    /// Columns inserted at the start of line 1 (the `use` prefix).
    pub prefix_cols: u32,
    /// The file the VM reports the document's places in: [`DOC_FILE`], or
    /// the game's script file an inline document's function is written in
    /// (the VM reports an adopted body at the image's real place).
    pub file: String,
    /// Where an error with no place of its own is put: the document's start
    /// (an inline document's, the function's place in the game's script).
    pub home: Loc,
}

impl SourceMap {
    /// Whether the VM location `file` is in the document.
    pub fn owns(&self, file: &str) -> bool {
        file == self.file
    }

    /// A VM location (0-based line and column) as a 1-based location in
    /// the document.
    pub fn map(&self, line: u32, col: u32) -> Loc {
        let col = if line == 0 { col.saturating_sub(self.prefix_cols) } else { col };
        Loc { line: line + 1, col: col + 1 }
    }
}

/// The source as evaluated: with the `use` prefix.
pub(crate) struct Prepared {
    pub code: String,
    pub map: SourceMap,
}

pub(crate) fn prepare(source: &str) -> Prepared {
    let code = math_after_std(source);
    Prepared { code: format!("{MATH_PREFIX}{code}"), map: SourceMap { prefix_cols: MATH_PREFIX.len() as u32, file: DOC_FILE.to_string(), home: Loc { line: 1, col: 1 } } }
}

/// A document's names from `mod.math` mean the maths wherever it imports
/// `mod.std` (whose `log` prints): `log(x)` is the natural log in every
/// part of a document, as in its shaders and scenes. Each top-level
/// `use mod.std…` statement is followed on its own line by the maths again
/// (lines and the columns before it stay); printing stays `std.log(…)` and
/// `print(…)`.
fn math_after_std(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    for line in code.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        let rest = &line[indent..];
        if let Some(path) = rest.strip_prefix("use mod.std") {
            // The statement ends where its path does (`.*`, `.log`, …).
            let end = indent + "use mod.std".len() + path.bytes().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'*')).count();
            out.push_str(&line[..end]);
            out.push_str("; use mod.math.*");
            out.push_str(&line[end..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The vocabularies a file may glob (`use mod.motion.*`): one per file, the
/// rest are used by name (`use mod.scene`, then `scene.Mesh{..}`).
pub(crate) const VOCABULARIES: &[&str] = &["motion", "scene", "forms", "post", "kernel"];

/// The error for a file that globs two vocabularies, naming both.
pub(crate) fn one_glob(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut globs: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = skip_literal(bytes, i) {
            i = end;
            continue;
        }
        if bytes[i] == b'u' && text[i..].starts_with("use mod.") && (i == 0 || !ident_byte(bytes[i - 1])) {
            let rest = &text[i + "use mod.".len()..];
            let name_len = rest.bytes().take_while(|b| ident_byte(*b)).count();
            if rest[name_len..].starts_with(".*") {
                if let Some(v) = VOCABULARIES.iter().find(|v| **v == &rest[..name_len]) {
                    if !globs.contains(v) {
                        globs.push(v);
                    }
                }
            }
            i += "use mod.".len() + name_len;
            continue;
        }
        i += 1;
    }
    glob_error(&globs)
}

/// The error for a file that globs two vocabularies (the first two).
pub(crate) fn glob_error<S: AsRef<str>>(globs: &[S]) -> Option<String> {
    let known: Vec<&str> = globs.iter().map(AsRef::as_ref).filter(|g| VOCABULARIES.contains(g)).collect();
    if known.len() < 2 {
        return None;
    }
    let (first, second) = (known[0], known[1]);
    Some(format!(
        "globs two vocabularies, `use mod.{first}.*` and `use mod.{second}.*`: glob the file's main one and use the other by name (`use mod.{second}`, then `{second}.<name>`)"
    ))
}

/// Skip a string or comment starting at `index`; the index after it.
pub(crate) fn skip_literal(bytes: &[u8], index: usize) -> Option<usize> {
    // `r"…"`, `r#"…"#`: a raw string (no escapes; quotes inside it when
    // it has `#`s), an `r` that does not end a name.
    if bytes[index] == b'r' && (index == 0 || !ident_byte(bytes[index - 1])) {
        let mut k = index + 1;
        while bytes.get(k) == Some(&b'#') {
            k += 1;
        }
        if bytes.get(k) == Some(&b'"') {
            let hashes = k - index - 1;
            let mut i = k + 1;
            while i < bytes.len() {
                if bytes[i] == b'"' && (1..=hashes).all(|h| bytes.get(i + h) == Some(&b'#')) {
                    return Some(i + 1 + hashes);
                }
                i += 1;
            }
            return Some(bytes.len());
        }
    }
    match (bytes[index], bytes.get(index + 1)) {
        (b'"', _) => {
            let mut i = index + 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            Some((i + 1).min(bytes.len()))
        }
        (b'/', Some(b'/')) => Some(bytes[index..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |end| index + end)),
        (b'/', Some(b'*')) => Some(
            bytes.get(index + 2..).and_then(|rest| rest.windows(2).position(|w| w == b"*/")).map_or(bytes.len(), |at| index + 2 + at + 2),
        ),
        _ => None,
    }
}

pub(crate) fn ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The fields of a Scene3D layer itself (its place and time, the 2D layers
/// and pictures it shows): read by Motion's loader, not by the scene.
pub(crate) const SCENE_KEEP: &[&str] = &["name", "at", "x", "y", "scale", "rotate", "anchor", "opacity", "blend", "span", "blur", "feather", "feather_side", "effects", "visible", "locked", "hard_cut", "size", "textures", "videos", "layers"];
/// The fields of Motion's own (simple) Scene3D; a Scene3D with any other
/// field is a scene motion3d reads.
pub(crate) const SCENE_SIMPLE: &[&str] = &["model", "camera", "lights", "nodes", "materials", "background", "exposure"];

/// What a document's VM keeps for its natives besides the heap: the
/// inputs `in()` was asked for and the document's 2D kernels.
#[derive(Default)]
pub(crate) struct DocHost {
    /// What `in(@name)` gives: the params while the document loads, the
    /// inputs' values once it has loaded.
    /// (An instance's values are swapped in for a frame:
    /// [`crate::MotionDoc::eval_with`].)
    pub inputs: Arc<Vec<(LiveId, InputValue)>>,
    /// While the document is evaluated at load: each `in(@name)` it asked
    /// for and the value it got (`None`: the placeholder the loader checks
    /// where it is used).
    pub asked: Option<Vec<(LiveId, Option<InputValue>)>>,
    #[cfg(feature = "scene3d")]
    pub kernels: crate::kernel2d::Kernels2d,
    pub draw2d: crate::draw2d::Draw2dHost,
    /// The registered types' prototypes (`crate::layers::register`).
    pub types: Vec<(ScriptObject, &'static str)>,
    /// `modulate(…)`'s clock and the document's live lets ([`crate::live`]).
    pub live: crate::live::LiveState,
    /// Explicit host clock for ambient motion, independent of layer time.
    pub live_time: f64,
    /// Set by the native when this frame actually reads that clock.
    pub reads_live: bool,
    /// The language this render is for ([`crate::MotionContext::lang`]).
    pub lang: Option<Arc<str>>,
}

/// The document's host state, read.
pub(crate) fn doc_host_ref<'a>(vm: &'a ScriptVm) -> &'a DocHost {
    vm.host
        .get::<DocHost>()
        .expect("a Motion pass runs in a document's VM")
}

/// The document's host state, from inside one of its natives.
pub(crate) fn doc_host<'a>(vm: &'a mut ScriptVm) -> &'a mut DocHost {
    vm.host
        .get_mut::<DocHost>()
        .expect("a Motion native runs in a document's VM")
}

/// What the document's VM keeps in its std slot: motion3d's scene state
/// (its kernels, audio, song and host entries; none without scenes).
#[cfg(feature = "scene3d")]
pub(crate) type Std = makepad_stage_motion3d::script::M3Host;
#[cfg(not(feature = "scene3d"))]
pub(crate) type Std = ();

/// A fresh VM with `mod.motion` installed (features readable through
/// `voice()` / `music()`), the document evaluated in it, and the value
/// read back. The loaded document keeps it: its property functions run in
/// it.
pub(crate) struct Evaluated {
    pub host: Box<ScriptVmHost<DocHost, Std>>,
    /// `None` only while `with_vm` runs (the VM owns it then).
    pub bx: Option<Box<ScriptVmBase>>,
    pub errors: Vec<ScriptErrorRecord>,
    /// The lowering's diagnostics ([`crate::lower`]).
    pub diags: Vec<crate::model::Diagnostic>,
    /// The document's value (its `Motion{…}`) as the VM first evaluated it.
    pub root: ScriptValue,
    pub map: SourceMap,
    /// The document and its functions, held for the garbage collector:
    /// what every frame allocates is collected, they are not.
    pub _roots: Vec<ScriptObjectRef>,
    pub _array_roots: Vec<ScriptArrayRef>,
    /// An editor's document: the records and lists its top-level `let`s
    /// bound, by name (rooted above).
    pub tops: Vec<(String, ScriptValue)>,
    /// What its `let`s (and its modules') bound as it loaded.
    pub load_lets: Vec<(ScriptIp, String, crate::schema::Watched)>,
    /// An editor's document: the literal immediates its load ran (what a
    /// patch of one says it cannot reach without a reload).
    pub load_trace: Option<makepad_script::literal::LoadTrace>,
    /// An editor's document: each imported module's code file
    /// (`module:<key>`), the file its body evaluated under (the path a tree
    /// gave it, else the code file) and its text as it loaded: what a
    /// literal edit of a module is diffed against and its sites are in.
    pub module_sources: Vec<(String, String, Arc<str>)>,
    /// What evaluates the document again ([`Evaluated::rerun`]).
    pub source: DocSource,
    pub code: Arc<str>,
    pub parent: Option<ScriptObject>,
    pub signals: Arc<dyn SignalSource>,
    pub offset: f64,
    /// Whether the VM may evaluate the document again (see `evaluate`).
    pub reusable: bool,
    /// How many times the document has been evaluated in this VM.
    pub evaluations: u64,
}

impl Evaluated {
    /// Run `f` with the document's VM.
    /// (The base is moved into the VM and back: no stand-in is built per
    /// call, which cost more than the property function it ran.)
    /// The base goes back when `f` panics too: a caller that catches the
    /// panic still has a document that evaluates.
    pub fn with_vm<R>(&mut self, f: impl FnOnce(&mut ScriptVm) -> R) -> R {
        struct Lent<'a, 'h> {
            slot: &'a mut Option<Box<ScriptVmBase>>,
            vm: Option<ScriptVm<'h>>,
        }
        impl Drop for Lent<'_, '_> {
            fn drop(&mut self) {
                if let Some(vm) = self.vm.take() {
                    *self.slot = Some(vm.bx);
                }
            }
        }
        let bx = self.bx.take().expect("the document's VM is not in use");
        let mut lent = Lent {
            slot: &mut self.bx,
            vm: Some(ScriptVm {
                host: self.host.host_context(),
                bx,
            }),
        };
        f(lent.vm.as_mut().unwrap())
    }

    /// A new frame at `t`: what `memo()` computed for the last one is
    /// forgotten, and the live lets are worked out at `t`.
    pub fn new_frame(&mut self, t: f64, live: f64) {
        self.host.host.live_time = live;
        self.host.host.reads_live = false;
        crate::forms::clear_memo();
        self.with_vm(|vm| crate::live::frame(vm, t));
    }
}

/// A loaded document's VM. A document evaluates on one thread at a time
/// (the one drawing it); a thread that evaluates the same document
/// alongside takes [`crate::MotionDoc::fork`], a VM of its own.
pub(crate) struct DocVm {
    state: std::sync::Mutex<Evaluated>,
    /// Held by one frame's evaluation from the instance's inputs going in to
    /// its going out: another instance's frame waits its turn.
    turn: std::sync::Mutex<()>,
}

// SAFETY: everything not `Send` in an `Evaluated` (the VM's `Rc`s and
// `RefCell`s and the natives' closures) is reached
// only through it: no handle to any of it leaves the VM (natives capture
// `Send + Sync` data, the document's signals, inputs and features, or state
// of this VM's own), and the mutex lets one thread at a time in. Moving
// the whole VM from the thread that loaded it to the one that draws it is
// then like moving any owned value.
unsafe impl Send for DocVm {}
unsafe impl Sync for DocVm {}

impl DocVm {
    pub fn new(evaluated: Evaluated) -> Self {
        Self { state: std::sync::Mutex::new(evaluated), turn: std::sync::Mutex::new(()) }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Evaluated) -> R) -> R {
        let mut evaluated = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut evaluated)
    }

    /// The VM for one frame's evaluation (waiting for the frame before it).
    pub fn turn(&self) -> std::sync::MutexGuard<'_, ()> {
        self.turn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One evaluation of a document in its VM, as a [`crate::MotionDoc`] holds
/// it: its root and the property functions its layers index. Documents of
/// one VM (a structural input changed) each have their own.
pub(crate) struct Run {
    pub vm: Arc<DocVm>,
    pub root: ScriptValue,
    /// The document's property functions, by the index the loader gave
    /// them (its `FnProp`s).
    pub fns: Vec<ScriptValue>,
    /// What keeps the root alive, released with the run (the VM's own first
    /// evaluation is held by the VM).
    roots: Vec<ScriptObjectRef>,
    array_roots: Vec<ScriptArrayRef>,
    /// The run whose root this one reads again (`reread`).
    pub keep: Option<Arc<Run>>,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Run({} functions)", self.fns.len())
    }
}

// SAFETY: as `DocVm`: the refs are changed only with the VM's lock.
unsafe impl Send for Run {}
unsafe impl Sync for Run {}

impl Run {
    pub fn new(vm: Arc<DocVm>, root: ScriptValue, fns: Vec<ScriptValue>, roots: Vec<ScriptObjectRef>, array_roots: Vec<ScriptArrayRef>) -> Self {
        Self { vm, root, fns, roots, array_roots, keep: None }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if self.roots.is_empty() && self.array_roots.is_empty() {
            return;
        }
        // The VM's root table is changed with the VM's turn.
        let (roots, array_roots) = (std::mem::take(&mut self.roots), std::mem::take(&mut self.array_roots));
        self.vm.with(|_| drop((roots, array_roots)));
    }
}

/// A scene of this document is read (and its closures run) in its VM, one
/// thread at a time. This is at draw, apart from any instance's frame: the
/// closures read the inputs the VM was loaded with (the first evaluation's),
/// not an instance's. A scene layer is a function of its load values; an
/// input that must move it per instance is read by a 2D property function.
#[cfg(feature = "scene3d")]
impl makepad_stage_motion3d::doc::SceneVmHost for DocVm {
    fn with_vm(&self, f: &mut dyn FnMut(&mut ScriptVm)) {
        self.with(|evaluated| evaluated.with_vm(|vm| f(vm)));
    }
}

impl std::fmt::Debug for DocVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DocVm")
    }
}

thread_local! {
    /// Documents loaded on this thread now are an editor's (see
    /// [`editable`]).
    static EDITABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The name the document loading now gives its own code's literal sites
    /// ([`editable_as`]); `None`: [`DOC_CODE_FILE`].
    static CODE_FILE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// [`editable`], the document's literal sites named `file` instead of
/// [`DOC_CODE_FILE`]: one of several documents running at once (an edit's
/// graphics), each patched by its own name (`Cx::patch_literal`).
pub fn editable_as<R>(file: &str, f: impl FnOnce() -> R) -> R {
    let was = CODE_FILE.with(|c| c.replace(Some(file.to_string())));
    let out = editable(f);
    CODE_FILE.with(|c| *c.borrow_mut() = was);
    out
}

/// The name the document loading on this thread gives its code's literal
/// sites.
pub(crate) fn doc_code_file() -> String {
    CODE_FILE.with(|c| c.borrow().clone()).unwrap_or_else(|| DOC_CODE_FILE.to_string())
}

/// Load documents in `f` as an editor's: their number literals stay values
/// of their own, so a dragged value is set in the running document at once
/// ([`crate::MotionDoc::patch_literal`]) instead of after a reload.
/// Whether the document loading on this thread is an editor's.
pub(crate) fn loading_editable() -> bool {
    EDITABLE.with(std::cell::Cell::get)
}

pub fn editable<R>(f: impl FnOnce() -> R) -> R {
    let was = EDITABLE.with(|e| e.replace(true));
    let out = f();
    EDITABLE.with(|e| e.set(was));
    out
}

/// What one evaluation of the document's body gave.
pub(crate) struct Ran {
    pub errors: Vec<ScriptErrorRecord>,
    pub diags: Vec<crate::model::Diagnostic>,
    /// The document's value (its `Motion{…}`).
    pub root: ScriptValue,
    /// The document and what an editor keeps of it, held for the collector.
    pub held: Vec<ScriptObjectRef>,
    pub array_held: Vec<ScriptArrayRef>,
    pub tops: Vec<(String, ScriptValue)>,
    pub load_trace: Option<makepad_script::literal::LoadTrace>,
}

/// A name the VM keeps the prefix of an inline document under.
const IMAGE_PREFIX_FILE: &str = "mod.motion.inline";
/// What an inline document's function sees beyond its own names: what a
/// document's first line brings (the maths, `mod.motion`'s vocabulary).
const IMAGE_PREFIX: &str = "use mod.std.*; use mod.math.*; use mod.pod.*; use mod.motion.*; nil";

/// The document's body, evaluated in `vm`: the file's text, or the inline
/// document's function adopted (no parse of a function already adopted) and
/// called. `first_body`: the first of the bodies it and its modules made
/// (their kernels load). Everything the load does after: the lowerings, the
/// live lets, the freeze.
fn run_document(vm: &mut ScriptVm, source: &DocSource, code: &str, map: &SourceMap, parent: Option<ScriptObject>, signals: &Arc<dyn SignalSource>, offset: f64, first_body: usize, module_errors: Vec<ScriptErrorRecord>) -> Ran {
    // An editor's document keeps its top-level records and lists.
    if vm.keep_numbers() {
        crate::schema::tops_begin(source.text());
    }
    let mut not_a_fn = false;
    let (root, _) = vm.eval_bounded(&LOAD_LIMITS, |vm| match source {
        DocSource::File(file, _) => vm.eval(ScriptMod::new("", "", file, 0, 0, code, &[])),
        DocSource::Image(image) => match parent.and_then(|parent| vm.adopt_fn(image, parent)) {
            Some(f) => vm.call(f.into(), &[]),
            None => {
                not_a_fn = true;
                NIL
            }
        },
    });
    let mut errors = module_errors;
    errors.extend(vm.take_error_records());
    if not_a_fn {
        errors.push(ScriptErrorRecord::plain("an inline Motion document is a function that returns the `Motion{…}`: `|| Motion{…}`".to_string()));
    }
    #[cfg(feature = "scene3d")]
    errors.extend(crate::kernel2d::load(vm, first_body).into_iter().map(ScriptErrorRecord::plain));
    #[cfg(not(feature = "scene3d"))]
    let _ = first_body;
    // `style:` and the text animators build their layers in this VM.
    crate::style::expand(vm, root, &mut errors);
    crate::typography::expand(vm, root, &mut errors);
    let diags = crate::lower::expand(vm, root, map, signals.clone(), offset);
    // Live lets (`modulate(…)`), compiled over the document as it ended
    // (a file's: an inline document has none).
    if matches!(source, DocSource::File(..)) {
        errors.extend(crate::live::compile(vm, code, map.prefix_cols as usize));
    }
    // Frozen from here on: property functions read the document but
    // cannot keep state in it (a frame is a pure function of its time).
    vm.freeze_document(&[root]);
    let load_trace = vm.end_literal_trace();
    let mut held: Vec<ScriptObjectRef> = root.as_object().map(|o| vm.bx.heap_mut().new_object_ref(o)).into_iter().collect();
    vm.set_let_hook(None);
    // (Rooted: the document's module scope is gone after the load.)
    let mut array_held = Vec::new();
    let tops: Vec<(String, ScriptValue)> = crate::schema::tops_end().into_iter().map(|(id, v)| {
        if let Some(o) = v.as_object() {
            held.push(vm.bx.heap_mut().new_object_ref(o));
        } else if let Some(a) = v.as_array() {
            array_held.push(vm.bx.heap_mut().new_array_ref(a));
        }
        (id.to_string(), v)
    }).collect();
    Ran { errors, diags, root, held, array_held, tops, load_trace }
}

/// Evaluate `source`. `in(@name)` gives the value `inputs` has for the
/// name, and a placeholder the loader type-checks where it is used for a
/// name it has none for (or an empty one); each one asked is noted in
/// [`DocHost::asked`].
/// `offset`: the host time of the document's 0 (`here()`).
///
/// A module that does not resolve (an import with no document context, a
/// revision not in the library) stops it before the VM runs: the
/// document's errors are the resolution's, once each.
pub(crate) fn evaluate(source: &DocSource, signals: Arc<dyn SignalSource>, offset: f64, inputs: &[(LiveId, InputValue)], modules: &dyn crate::modules::ModuleResolver, lang: Option<Arc<str>>) -> Result<Evaluated, Vec<crate::model::Diagnostic>> {
    // The scenes a Text3D or Seq3D lowers to run in VMs of their own: give them the text functions.
    #[cfg(feature = "scene3d")]
    makepad_stage_motion3d::script::set_host_natives(crate::type_fns::install);
    let (code, map) = match source {
        DocSource::File(file, text) => {
            let mut prepared = prepare(text);
            prepared.map.file = file.to_string();
            (prepared.code, prepared.map)
        }
        // An inline document is not prefixed: it sits where the game's script
        // wrote it, and errors name that place.
        DocSource::Image(image) => (String::new(), SourceMap { prefix_cols: 0, file: image.file().to_string(), home: Loc { line: image.line() + 1, col: image.col() + 1 } }),
    };
    // The modules it imports (resolved before the VM runs anything): found
    // in a file's text, or in an inline document's tokens.
    let mut module_errors = Vec::new();
    let resolved = match source {
        DocSource::File(_, text) => crate::modules::resolve(text, modules),
        DocSource::Image(image) => crate::modules::resolve_image(image, modules),
    };
    let loaded = resolved.map_err(|errors| errors.into_iter().map(|error| crate::model::Diagnostic::error(map.home, "", error)).collect::<Vec<_>>())?;
    // A file globs one vocabulary: the rest are used by name.
    let own = match source {
        DocSource::File(_, text) => one_glob(text),
        DocSource::Image(image) => glob_error(&crate::modules::image_globs(image)),
    };
    for (what, e) in std::iter::once(("the document", own)).chain(loaded.iter().map(|m| ("a module", one_glob(&m.source.text)))) {
        if let Some(e) = e {
            module_errors.push(ScriptErrorRecord::plain(format!("{what}: {e}")));
        }
    }
    let mut host = Box::new(ScriptVmHost::new(DocHost { inputs: Arc::new(inputs.to_vec()), asked: Some(Vec::new()), lang, ..DocHost::default() }, Std::default()));
    #[cfg(feature = "scene3d")]
    {
        host.std.new_evaluation = Some(|_| crate::forms::clear_memo());
        host.std.song = song_timing(&*signals, offset);
    }
    let bx = Box::new(ScriptVmBase::new());
    let (ran, bx, parent, reusable, module_sources) = {
        let mut vm = ScriptVm {
            host: host.host_context(),
            bx,
        };
        // What its `let`s bind as it loads (its constants, what they
        // compute), for editors.
        crate::schema::watch_begin();
        vm.set_let_hook(Some(crate::schema::let_bound));
        // An editor's document keeps its numbers settable in place, and
        // records which of its literals its load ran.
        vm.set_keep_numbers(EDITABLE.with(std::cell::Cell::get));
        if vm.keep_numbers() {
            vm.begin_literal_trace();
        }
        vm.set_script_domain("motion");
        let module = vm.new_module(id!(motion));
        // The forms 2D and 3D share (the records are typed here), the
        // shared Splash stdlib and the render graph's post kits: modules of
        // their own, which `mod.motion` carries or the document names.
        let forms = vm.new_module(id!(forms));
        crate::layers::register(&mut vm, module, forms);
        vm.capture_errors();
        let forms_errors = crate::forms::install(&mut vm, forms);
        debug_assert!(forms_errors.is_empty(), "mod.forms: {forms_errors:?}");
        crate::bind::install(&mut vm, module, signals.runs().cloned().unwrap_or_default());
        install_signals(&mut vm, module, signals.clone(), offset);
        crate::live::install(&mut vm, module, signals.clone(), offset);
        // `lang()`: the language this render is for ("en": as written).
        vm.add_method(module, id_lut!(lang), script_args!(), |vm, _args| {
            let lang = doc_host(vm).lang.clone();
            let lang = lang.as_deref().unwrap_or("en").to_string();
            vm.new_string_with(|_, s| s.push_str(&lang))
        });
        vm.add_method(module, id_lut!(live), script_args!(), |vm, _args| {
            let host = doc_host(vm);
            host.reads_live = true;
            host.live_time.into()
        });
        crate::draw2d::install(&mut vm, module);
        crate::procedural::install_plotting(&mut vm, module);
        crate::procedural::install_boolean(&mut vm, module);
        crate::align::install(&mut vm, module);
        // An emissive colour: `emit(#ff5a1f, 4)` is the colour at 4x its
        // light (components over 1). In a `color: @hdr` document it glows
        // (the Glow kit's threshold is 1); a display document clamps it.
        vm.add_method(module, id_lut!(emit), script_args!(c = NIL, k = NIL), |vm, args| {
            let c = vm.bx.heap_ref().value(args, id!(c).into(), NoTrap);
            let k = vm.bx.heap_ref().value(args, id!(k).into(), NoTrap).as_number().unwrap_or(1.0).max(0.0);
            let Some(packed) = c.as_color() else { return NIL };
            let [r, g, b, a] = crate::load::unpack(packed);
            let parts = vm.bx.heap_mut().new_array();
            for v in [r * k, g * k, b * k, a] {
                vm.bx.heap_mut().array_push(parts, v.into(), NoTrap);
            }
            let object = vm.bx.heap_mut().new_object();
            vm.bx.heap_mut().set_value_def(object, id_lut!(color).into(), parts.into());
            object.into()
        });
        install_icons(&mut vm, module);
        crate::type_fns::install(&mut vm, module);
        vm.capture_errors();
        vm.eval(ScriptMod::new("", "", &(PRELUDE_FILE), 0, 0, MOTION_PRELUDE, &[]));
        let prelude_errors = vm.take_errors();
        debug_assert!(prelude_errors.is_empty(), "mod.motion prelude: {prelude_errors:?}");
        let shared = if makepad_script_compute::module::register_shared_std(&mut vm) {
            makepad_tween_script::install_ease(&mut vm);
            vm.bx.heap_ref().value(vm.modules(), id!(shared).into(), NoTrap).as_object()
        } else {
            vm.bx.heap_ref().value(vm.modules(), id!(shared).into(), NoTrap).as_object()
        };
        // `use mod.motion.*` brings the forms and the stdlib too.
        crate::forms::reexport(&mut vm, forms, module);
        if let Some(shared) = shared {
            crate::forms::reexport(&mut vm, shared, module);
        }
        // The render graph's post kits, eagerly: the Scene3D vocabulary adopts them below
        // (`install_document` tags their protos as the scene's post kinds) and Rust reads them
        // from the heap (kit templates, the default motion blur), so they cannot wait for a
        // script's first read (they did: P(DOOM)'s Scene3D post lists read "not a post effect").
        let post_errors = install_post(&mut vm);
        debug_assert!(post_errors.is_empty(), "mod.post: {post_errors:?}");
        // Modules: the effects library by name, library revisions through
        // `import(asset(id, rev))`, evaluated before the document.
        let named = vm.bx.heap_mut().new_object();
        vm.bx.heap_mut().set_value_def(module, id_lut!(modules).into(), named.into());
        let module_sources: Vec<(String, String, Arc<str>)> = if vm.keep_numbers() {
            loaded.iter().map(|m| (makepad_script_imports::module_file(&m.reference), makepad_script_imports::file_of(m), m.source.text.clone())).collect()
        } else {
            Vec::new()
        };
        // Kernels: `Kernel{..}` objects of the document and its modules (its
        // 2D draws' and its scenes'), loaded once they evaluated; and the 3D
        // scenes' vocabulary, `mod.scene` and `mod.kernel`, beside the
        // document's own.
        #[cfg(feature = "scene3d")]
        makepad_stage_motion3d::script::install_document(&mut vm, module);
        modules.install(&mut vm, module);
        modules.install(&mut vm, forms);
        let first_body = vm.body_count();
        let core = makepad_script_imports::language_core(&mut vm);
        module_errors.extend(makepad_script_imports::load(&mut vm, module, &core, &loaded, Some(named), INSTRUCTION_LIMIT));
        // (`import` is a form: `mod.forms` has it too.)
        let import = vm.bx.heap_ref().value(module, id_lut!(import).into(), NoTrap);
        vm.bx.heap_mut().set_value_def(forms, id_lut!(import).into(), import);
        // `in(@name)`: a declared input's value.
        vm.add_method(module, id_lut!(in), script_args!(name = NIL, default = NIL), |vm, args| {
            let Some(name) = arg_live_id(vm, args, id!(name)) else { return NIL };
            let default = vm.bx.heap_ref().value(args, id!(default).into(), NoTrap);
            let host = doc_host(vm);
            let given = host.inputs.iter().find(|(n, _)| *n == name).map(|(_, value)| value).filter(|value| !matches!(value, InputValue::Empty)).cloned();
            if let Some(asked) = &mut host.asked {
                asked.push((name, given.clone()));
            }
            match &given {
                Some(value) => input_to_script(vm, value),
                // `in(@name, default)`: the default where nothing is given.
                None if !default.is_nil() && !default.is_err() => default,
                // A placeholder the loader understands (an empty asset
                // input's layer draws nothing).
                None => crate::layers::make(vm, "In", &[(id_lut!(name), ScriptValue::from_id(name))]).into(),
            }
        });
        // An inline document's function reads what a document's first line
        // brings: the scope it is adopted over.
        let parent = match source {
            DocSource::Image(_) => {
                vm.eval(ScriptMod::new("", "", IMAGE_PREFIX_FILE, 0, 0, IMAGE_PREFIX, &[]));
                vm.end_scope_of(IMAGE_PREFIX_FILE)
            }
            DocSource::File(..) => None,
        };
        vm.capture_errors();
        let ran = run_document(&mut vm, source, &code, &map, parent, &signals, offset, first_body, module_errors);
        // A document that kept state outside its heap (kernels, live lets)
        // or that an editor patches in place is evaluated again in a VM of
        // its own, never in this one.
        let reusable = !vm.keep_numbers() && doc_host(&mut vm).live.lets.is_empty() && !has_kernels(&vm);
        (ran, vm.bx, parent, reusable, module_sources)
    };
    let load_lets = crate::schema::watch_end();
    Ok(Evaluated {
        host,
        bx: Some(bx),
        errors: ran.errors,
        diags: ran.diags,
        root: ran.root,
        map,
        _roots: ran.held,
        _array_roots: ran.array_held,
        tops: ran.tops,
        load_lets,
        load_trace: ran.load_trace,
        module_sources,
        source: source.clone(),
        code: code.into(),
        parent,
        signals,
        offset,
        reusable,
        evaluations: 1,
    })
}

/// Whether the document's evaluation registered kernels (their state is the
/// VM's, one document's).
fn has_kernels(vm: &ScriptVm) -> bool {
    #[cfg(feature = "scene3d")]
    {
        makepad_stage_motion3d::script::host(vm).is_some_and(|h| !h.kernels.list.is_empty())
    }
    #[cfg(not(feature = "scene3d"))]
    {
        let _ = vm;
        false
    }
}

impl Evaluated {
    /// The document's body evaluated again in this VM, `inputs` read by
    /// `in(@…)`: nothing is parsed again (the file's body, or the adopted
    /// function, is the one already read) and the VM keeps its prelude and
    /// modules. A new evaluation: the previous one's objects stay valid for
    /// the documents made from it. Only where [`Self::reusable`].
    pub fn rerun(&mut self, inputs: Arc<Vec<(LiveId, InputValue)>>) -> Ran {
        debug_assert!(self.reusable, "this document's VM is evaluated once");
        self.host.host.inputs = inputs;
        self.host.host.asked = Some(Vec::new());
        self.evaluations += 1;
        let (source, code, map, parent, signals, offset) = (self.source.clone(), self.code.clone(), self.map.clone(), self.parent, self.signals.clone(), self.offset);
        self.with_vm(|vm| {
            vm.capture_errors();
            let first_body = vm.body_count();
            run_document(vm, &source, &code, &map, parent, &signals, offset, first_body, Vec::new())
        })
    }
}

/// The post vocabulary: `Pass`, `MotionBlur` and the render graph's kits
/// (Glow, Halation, Aberration, Grain, Vignette, FramePost, Lut, Bloom,
/// Shoulder), each a
/// prototype in `mod.motion` like the layer types.
pub const POST_KITS: &[&str] = &["glow", "halation", "aberration", "grain", "vignette", "frame_post", "lut", "bloom", "shoulder"];
pub const POST_TYPES: &[&str] = &["Pass", "MotionBlur", "Glow", "Halation", "Aberration", "Grain", "Vignette", "FramePost", "Lut", "Bloom", "Shoulder", "DepthOfField", "Outline", "Upsample", "VelocityBlur"];

/// `mod.post` (the render graph's: every post type and kit, tagged with its
/// kind), and its types named for the loader.
fn install_post(vm: &mut ScriptVm) -> Vec<String> {
    let errors = makepad_render_graph::script::install_post(vm);
    if let Some(post) = vm.bx.heap_ref().value(vm.modules(), id!(post).into(), NoTrap).as_object() {
        crate::layers::name_types(vm, post, POST_TYPES);
    }
    errors
}

/// An id whose text reads back (`@name` values and keys the reader names).
pub(crate) fn lut_id(name: &str) -> LiveId {
    LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name))
}

pub(crate) fn input_to_script(vm: &mut ScriptVm, value: &InputValue) -> ScriptValue {
    match value {
        InputValue::Text(text) | InputValue::Markup(text) => vm.new_string_with(|_, s| s.push_str(text)),
        InputValue::Number(n) => (*n).into(),
        InputValue::Bool(b) => (*b).into(),
        InputValue::Color(c) => ScriptValue::from_color(crate::load::pack(*c)),
        InputValue::Vec2(v) => {
            // A Splash vec2, so documents can compute with it.
            let ty = vm.pod_builtins().pod_vec2f;
            let pod = vm.bx.heap_mut().new_pod(ty);
            let data = vm.bx.heap_mut().pod_data_mut(pod);
            data[0] = (v[0] as f32).to_bits();
            data[1] = (v[1] as f32).to_bits();
            pod.into()
        }
        InputValue::Asset(asset) => {
            let id = vm.new_string_with(|_, s| s.push_str(&asset.id));
            let rev = vm.new_string_with(|_, s| s.push_str(&asset.rev));
            crate::layers::make(vm, "Asset", &[(id_lut!(id), id), (id_lut!(rev), rev)]).into()
        }
        InputValue::Empty => NIL,
        InputValue::List(items) => {
            let array = vm.bx.heap_mut().new_array();
            for item in items {
                let value = input_to_script(vm, item);
                vm.bx.heap_mut().array_push(array, value, NoTrap);
            }
            array.into()
        }
        InputValue::Record(fields) => {
            let object = vm.bx.heap_mut().new_object();
            for (name, value) in fields {
                let value = input_to_script(vm, value);
                set(vm, object, lut_id(name), value);
            }
            object.into()
        }
    }
}

fn arg_num(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> f64 {
    vm.bx.heap_ref().value(args, key.into(), NoTrap).as_number().unwrap_or(0.0)
}

fn arg_id(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> Option<String> {
    let value = vm.bx.heap_ref().value(args, key.into(), NoTrap);
    if let Some(id) = value.as_id() {
        return Some(crate::obj::id_text(id));
    }
    value.as_number().map(|n| fmt_num(n))
}

/// The identifier an argument names (`in(@title)`).
fn arg_live_id(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> Option<LiveId> {
    let value = vm.bx.heap_ref().value(args, key.into(), NoTrap);
    if let Some(id) = value.as_id() {
        return Some(id);
    }
    value.as_number().map(|n| lut_id(&fmt_num(n)))
}

/// `voice(@id)` and `music(@id)`: the host's signals. A source the host
/// does not know reads as silence (no words, zero levels, a 120 bpm grid),
/// so a document still evaluates without its audio.
fn install_signals(vm: &mut ScriptVm, module: ScriptObject, signals: Arc<dyn SignalSource>, offset: f64) {
    // `here()`: the second of the song (the host's clock) at which this
    // document starts. The times `voice()` and `music()` give (words, lines,
    // beats, onsets) are on the document's own clock, the one its `t` is on
    // (song second - here()), so `t >= L[i].start` holds wherever the plate
    // is placed; their methods of time take the host's (`edit`).
    vm.add_method(module, id_lut!(here), script_args!(), move |_, _| offset.into());
    let s = signals.clone();
    vm.add_method(module, id_lut!(voice), script_args!(id = NIL), move |vm, args| {
        // While a 3D scene's closure runs, the audio's levels at that moment.
        #[cfg(feature = "scene3d")]
        if let Some(live) = makepad_stage_motion3d::script::live_audio(vm) {
            return live;
        }
        let Some(id) = arg_id(vm, args, id!(id)) else { return NIL };
        voice_object(vm, &s, &id, offset)
    });
    let s = signals;
    vm.add_method(module, id_lut!(music), script_args!(id = NIL), move |vm, args| {
        #[cfg(feature = "scene3d")]
        if let Some(live) = makepad_stage_motion3d::script::live_audio(vm) {
            return live;
        }
        let Some(id) = arg_id(vm, args, id!(id)) else { return NIL };
        music_object(vm, &s, &id, offset)
    });
}

/// A string or id argument as text.
fn arg_text(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> Option<String> {
    let value = vm.bx.heap_ref().value(args, key.into(), NoTrap);
    if let Some(s) = value.as_string() {
        return Some(vm.bx.heap_ref().string(s).to_string());
    }
    arg_id(vm, args, key)
}

/// `icon("stage/play")`: a bundled icon's SVG markup (for `Svg`/`Vector`
/// layers); `icon_path("play", 100)`: every shape of it as one path data
/// string in a 100-unit box (for `Path` layers and morphs, which morph
/// between any two outlines); `svg_path("<svg…>", 100)` the same for
/// markup. An unknown icon raises an error naming the closest ones.
fn install_icons(vm: &mut ScriptVm, module: ScriptObject) {
    fn unknown(vm: &mut ScriptVm, name: &str) -> ScriptValue {
        let near = crate::icons::similar_icons(name, 5).join(", ");
        script_err_not_found!(vm.trap(), "no bundled icon \"{}\" (closest: {}; `makepad-motion icons` lists them)", name, near)
    }
    vm.add_method(module, id_lut!(icon), script_args!(name = NIL), |vm, args| {
        let Some(name) = arg_text(vm, args, id!(name)) else { return NIL };
        match crate::icons::icon(&name) {
            Some(svg) => vm.new_string_with(|_, s| s.push_str(svg)),
            None => unknown(vm, &name),
        }
    });
    vm.add_method(module, id_lut!(icon_path), script_args!(name = NIL, size = 100.0), |vm, args| {
        let Some(name) = arg_text(vm, args, id!(name)) else { return NIL };
        let size = arg_num(vm, args, id!(size));
        match crate::icons::icon(&name) {
            Some(svg) => {
                let data = crate::icons::svg_path_data(svg, if size > 0.0 { size } else { 100.0 });
                vm.new_string_with(|_, s| s.push_str(&data))
            }
            None => unknown(vm, &name),
        }
    });
    vm.add_method(module, id_lut!(svg_path), script_args!(svg = NIL, size = 100.0), |vm, args| {
        let Some(markup) = arg_text(vm, args, id!(svg)) else { return NIL };
        let size = arg_num(vm, args, id!(size));
        let data = crate::icons::svg_path_data(&markup, if size > 0.0 { size } else { 100.0 });
        vm.new_string_with(|_, s| s.push_str(&data))
    });
}

fn num_array(vm: &mut ScriptVm, values: impl Iterator<Item = f64>) -> ScriptValue {
    let array = vm.bx.heap_mut().new_array();
    for value in values {
        vm.bx.heap_mut().array_push(array, value.into(), NoTrap);
    }
    array.into()
}

fn set(vm: &mut ScriptVm, object: ScriptObject, key: LiveId, value: ScriptValue) {
    vm.bx.heap_mut().set_value_def(object, key.into(), value);
}

/// A method of a signal object taking `t`.
/// A method of time over the host's signals: it takes the host's (edit)
/// time, a property function's `edit` (`music(@m).beat_phase(edit)` stays
/// on the song's grid wherever the clip is placed).
fn method_t(vm: &mut ScriptVm, object: ScriptObject, name: LiveId, signals: &Arc<dyn SignalSource>, id: &str, f: fn(&dyn SignalSource, &str, f64) -> f64) {
    let (s, id) = (signals.clone(), id.to_string());
    vm.add_method(object, name, script_args!(t = NIL), move |vm, args| f(&*s, &id, arg_num(vm, args, id!(t))).into());
}

/// A voice's times moved `dt` seconds (the host's clock to a document's).
fn voice_shifted(mut voice: crate::features::VoiceFeatures, dt: f64) -> crate::features::VoiceFeatures {
    if dt == 0.0 {
        return voice;
    }
    for word in &mut voice.words {
        word.start += dt;
        word.end += dt;
        for syllable in &mut word.syllables {
            syllable.0 += dt;
            syllable.1 += dt;
        }
    }
    for line in voice.lines.iter_mut().chain(voice.pauses.iter_mut()) {
        line.0 += dt;
        line.1 += dt;
    }
    voice.amplitude.start += dt;
    voice.pitch.start += dt;
    for phone in &mut voice.phones {
        phone.start += dt;
        phone.end += dt;
    }
    voice
}

fn voice_object(vm: &mut ScriptVm, signals: &Arc<dyn SignalSource>, id: &str, offset: f64) -> ScriptValue {
    let object = crate::layers::make(vm, "Voice", &[(id_lut!(id), ScriptValue::from_id(lut_id(id)))]);
    // The data (words, lines, pauses, …) on the document's clock, the one
    // its `t` is on; the methods (`word_at(edit)`, `progress(i, edit)`, …)
    // on the host's, as every method of time.
    let host_voice = signals.voice(id).cloned().unwrap_or_default();
    let voice = voice_shifted(host_voice.clone(), -offset);
    let words = vm.bx.heap_mut().new_array();
    for word in &voice.words {
        let w = vm.bx.heap_mut().new_object();
        let text = vm.new_string_with(|_, s| s.push_str(&word.text));
        set(vm, w, id_lut!(text), text);
        set(vm, w, id_lut!(start), word.start.into());
        set(vm, w, id_lut!(end), word.end.into());
        set(vm, w, id_lut!(emphasis), word.emphasis.into());
        set(vm, w, id_lut!(line), (word.line as f64).into());
        // `syllables: [{start end}…]`, the analysis' spans; a word it did
        // not split is one syllable.
        let syllables = vm.bx.heap_mut().new_array();
        let spans = if word.syllables.is_empty() { vec![(word.start, word.end)] } else { word.syllables.clone() };
        for (start, end) in spans {
            let syllable = vm.bx.heap_mut().new_object();
            set(vm, syllable, id_lut!(start), start.into());
            set(vm, syllable, id_lut!(end), end.into());
            vm.bx.heap_mut().array_push(syllables, syllable.into(), NoTrap);
        }
        set(vm, w, id_lut!(syllables), syllables.into());
        vm.bx.heap_mut().array_push(words, w.into(), NoTrap);
    }
    set(vm, object, id_lut!(words), words.into());
    let lines = num_array(vm, voice.lines.iter().map(|line| line.0));
    set(vm, object, id_lut!(lines), lines);
    let pauses = num_array(vm, voice.pauses.iter().map(|pause| pause.0));
    set(vm, object, id_lut!(pauses), pauses);
    method_t(vm, object, id_lut!(amplitude), signals, id, |s, id, t| s.amplitude(id, t));
    method_t(vm, object, id_lut!(pitch), signals, id, |s, id, t| s.pitch(id, t));
    let voice = Arc::new(host_voice);
    let v = voice.clone();
    vm.add_method(object, id_lut!(word_at), script_args!(t = NIL), move |vm, args| {
        v.word_at(arg_num(vm, args, id!(t))).map_or(NIL, |i| (i as f64).into())
    });
    let v = voice.clone();
    vm.add_method(object, id_lut!(line_at), script_args!(t = NIL), move |vm, args| {
        v.line_at(arg_num(vm, args, id!(t))).map_or(NIL, |i| (i as f64).into())
    });
    let v = voice.clone();
    vm.add_method(object, id_lut!(in_pause), script_args!(t = NIL), move |vm, args| v.in_pause(arg_num(vm, args, id!(t))).into());
    // A word's progress at t: -1 upcoming, 0..1 while spoken, 2 done.
    let v = voice;
    vm.add_method(object, id_lut!(progress), script_args!(i = NIL, t = NIL), move |vm, args| {
        let i = arg_num(vm, args, id!(i)).max(0.0) as usize;
        match v.word_state(i, arg_num(vm, args, id!(t))) {
            WordState::Upcoming => (-1.0).into(),
            WordState::Active(p) => p.into(),
            WordState::Done => 2.0.into(),
        }
    });
    object.into()
}

fn music_object(vm: &mut ScriptVm, signals: &Arc<dyn SignalSource>, id: &str, offset: f64) -> ScriptValue {
    let object = crate::layers::make(vm, "Music", &[(id_lut!(id), ScriptValue::from_id(lut_id(id)))]);
    set(vm, object, id_lut!(bpm), signals.bpm(id).into());
    let music = signals.music(id).cloned().unwrap_or_default();
    let beats = num_array(vm, music.beats.iter().map(|b| b - offset));
    set(vm, object, id_lut!(beats), beats);
    let onsets = num_array(vm, music.onsets.iter().map(|onset| onset.t - offset));
    set(vm, object, id_lut!(onsets), onsets);
    method_t(vm, object, id_lut!(energy), signals, id, |s, id, t| s.energy(id, t));
    method_t(vm, object, id_lut!(beat_index), signals, id, |s, id, t| s.beat_index(id, t));
    method_t(vm, object, id_lut!(beat_at), signals, id, |s, id, t| s.beat_index(id, t));
    method_t(vm, object, id_lut!(beat_phase), signals, id, |s, id, t| s.beat_phase(id, t));
    method_t(vm, object, id_lut!(bar_phase), signals, id, |s, id, t| s.bar_phase(id, t));
    let (s, music_id) = (signals.clone(), id.to_string());
    vm.add_method(object, id_lut!(band), script_args!(band = NIL, t = NIL), move |vm, args| {
        let name = arg_id(vm, args, id!(band)).unwrap_or_default();
        s.band(&music_id, &name, arg_num(vm, args, id!(t))).into()
    });
    // The sound around a moment as lists: `spectrum(t, 32)` levels 0..1
    // low to high; `wave(t, 128, 0.05)` signed samples -1..1 across the
    // window centred on `t`.
    let (s, music_id) = (signals.clone(), id.to_string());
    vm.add_method(object, id_lut!(spectrum), script_args!(t = NIL, bins = NIL), move |vm, args| {
        let t = arg_num(vm, args, id!(t));
        let bins = vm.bx.heap_ref().value(args, id!(bins).into(), NoTrap).as_number().unwrap_or(32.0).clamp(1.0, 1024.0) as usize;
        let levels = s.spectrum(&music_id, t, bins);
        num_array(vm, levels.into_iter())
    });
    let (s, music_id) = (signals.clone(), id.to_string());
    vm.add_method(object, id_lut!(wave), script_args!(t = NIL, n = NIL, window = NIL), move |vm, args| {
        let t = arg_num(vm, args, id!(t));
        let n = vm.bx.heap_ref().value(args, id!(n).into(), NoTrap).as_number().unwrap_or(128.0).clamp(1.0, 8192.0) as usize;
        let window = vm.bx.heap_ref().value(args, id!(window).into(), NoTrap).as_number().filter(|w| *w > 0.0).unwrap_or(0.05);
        let samples = s.wave(&music_id, t, n, window);
        num_array(vm, samples.into_iter())
    });
    let (s, music_id) = (signals.clone(), id.to_string());
    vm.add_method(object, id_lut!(hit), script_args!(t = NIL, window = NIL), move |vm, args| {
        let t = arg_num(vm, args, id!(t));
        let window = vm.bx.heap_ref().value(args, id!(window).into(), NoTrap).as_number().unwrap_or(0.25);
        s.hit(&music_id, t, window).into()
    });
    object.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::obj::{self, Look};

    /// A document of `source`, evaluated.
    fn evaluate(source: &str, signals: Arc<dyn SignalSource>, offset: f64, inputs: &[(LiveId, InputValue)], modules: &dyn crate::modules::ModuleResolver) -> Evaluated {
        super::evaluate(&DocSource::file(source), signals, offset, inputs, modules, None).unwrap_or_else(|diags| panic!("its modules do not resolve: {diags:?}"))
    }

    /// The document's field `name`, looked at.
    fn field(e: &mut Evaluated, name: &str) -> Option<Look> {
        let root = e.root.as_object()?;
        e.with_vm(|vm| obj::look_field(vm, root, name))
    }

    /// memo(f, t) answers only in the VM that computed it: B's function has
    /// A's handle, and A's kept object lies beyond B's heap.
    #[test]
    fn memo_answers_only_in_the_vm_that_computed_it() {
        let features = Arc::new(crate::features::AudioFeatures::default());
        let mut a = evaluate("use mod.motion.*; let size_raw = fn(t) { {w: 2.0} }\nvar junk = []\nfor i in 0..5000 { junk.push({i: i}) }\nlet W = memo(size_raw, 1.0).w\nMotion{duration: W layers: []}",
            features.clone(), 0.0, &[], &crate::modules::NoModules);
        let mut b = evaluate("use mod.motion.*; let size_raw = fn(t) { {w: 3.0} }\nlet W = memo(size_raw, 1.0).w\nMotion{duration: W layers: []}", features, 0.0, &[], &crate::modules::NoModules);
        assert_eq!((field(&mut a, "duration").and_then(|d| d.num()), field(&mut b, "duration").and_then(|d| d.num())), (Some(2.0), Some(3.0)));
    }

    #[test]
    fn a_document_evaluates_with_prototypes_locations_and_errors() {
        let source = "use mod.motion.*; let Card = Rect{fill: #ff0000 radius: 4}\nMotion{\n  layers: [Card{size: vec2(10, 20)}\n    Text{text: in(@title) y: tween(0, 1, 0, beat(2))}]\n  bad: nope\n}";
        let mut evaluated = evaluate(source, Arc::new(crate::features::AudioFeatures::default()), 0.0, &[], &crate::modules::NoModules);
        let (root, map) = (evaluated.root, evaluated.map.clone());
        evaluated.with_vm(|vm| {
            let r = root.as_object().expect("an object");
            assert_eq!(obj::type_name(vm, root), Some("Motion"));
            assert_eq!(obj::loc(vm, &map, r), Some(Loc { line: 2, col: 7 }));
            let Some(Look::Arr(layers)) = obj::look_field(vm, r, "layers") else { panic!() };
            let Look::Obj(card, Some("Rect")) = layers[0] else { panic!("{:?}", layers[0]) };
            assert!(matches!(obj::look_field(vm, card, "fill"), Some(Look::Color(0xff0000ff))));
            assert_eq!(obj::field_loc(vm, &map, card, "fill"), Some(Loc { line: 1, col: 35 }));
            let Some(Look::Arr(size)) = obj::look_field(vm, card, "size") else { panic!() };
            assert_eq!((size[0].num(), size[1].num()), (Some(10.0), Some(20.0)));
            let Look::Obj(text, _) = layers[1] else { panic!() };
            let title = obj::look_field(vm, text, "text").unwrap();
            assert!(title.is("In"));
            assert!(matches!(obj::look_field(vm, title.obj().unwrap(), "name"), Some(Look::Id(n)) if n == "title"));
            let y = obj::look_field(vm, text, "y").unwrap();
            assert_eq!(y.anim(), Some("Tween"));
            assert_eq!(obj::look_field(vm, y.obj().unwrap(), "end").unwrap().time_ref(), Some("Beat"));
        });
        // `bad: nope`: an unknown name, and a field Motion does not have
        // (the component says so, with the nearest fields).
        assert_eq!(evaluated.errors.len(), 2, "{:?}", evaluated.errors);
        let at = |e: &ScriptErrorRecord| e.loc.as_ref().map(|l| evaluated.map.map(l.line, l.col).line);
        assert_eq!(at(&evaluated.errors[0]), Some(5), "{:?}", evaluated.errors[0]);
        assert!(evaluated.errors[0].message.contains("nope"), "{:?}", evaluated.errors[0]);
        assert_eq!(at(&evaluated.errors[1]), Some(5), "{:?}", evaluated.errors[1]);
        assert!(evaluated.errors[1].message.contains("property bad not defined"), "{:?}", evaluated.errors[1]);
    }

    /// `music(@m).spectrum(t, n)` and `.wave(t, n, window)` read the track's
    /// sound at the host's time: a 1 kHz tone peaks in the band holding
    /// 1 kHz at the bin count asked, and the wave has its cycles.
    #[test]
    fn a_document_reads_the_spectrum_and_the_wave_as_lists() {
        let rate = 48_000.0;
        let tone: Vec<f32> = (0..96_000).map(|i| (0.5 * (std::f64::consts::TAU * 1000.0 * i as f64 / rate).sin()) as f32).collect();
        let mut features = crate::features::AudioFeatures::default();
        features.music.push(("m".into(), crate::features::MusicFeatures { signal: Some(crate::features::AudioSignal::new(0.0, rate, tone.into())), ..Default::default() }));
        let source = "use mod.motion.*\nlet m = music(@m)\nlet s = m.spectrum(1.0, 24)\nvar peak = 0\nfor i in 0..s.len() { if s[i] > s[peak] { peak = i } }\nlet w = m.wave(1.0, 200, 0.01)\nvar flips = 0\nfor i in 1..w.len() { if (w[i] < 0) != (w[i - 1] < 0) { flips = flips + 1 } }\n{bins: s.len() peak: peak level: s[peak] slots: w.len() flips: flips quiet: m.spectrum(5.0, 8)[3] plain: m.spectrum(1.0).len()}";
        let mut e = evaluate(source, Arc::new(features), 0.0, &[], &crate::modules::NoModules);
        assert!(e.errors.is_empty(), "{:?}", e.errors);
        let num = |e: &mut Evaluated, name: &str| field(e, name).and_then(|v| v.num()).unwrap_or(f64::NAN);
        assert_eq!(num(&mut e, "bins"), 24.0);
        // Band i of 24 spans 30 Hz × (16000/30)^(i/24 … (i+1)/24): 1 kHz is in band 13.
        let band = ((1000.0f64 / crate::features::SPECTRUM_LOW_HZ).ln() / (crate::features::SPECTRUM_HIGH_HZ / crate::features::SPECTRUM_LOW_HZ).ln() * 24.0).floor();
        assert_eq!(num(&mut e, "peak"), band);
        assert!(num(&mut e, "level") > 0.85, "{}", num(&mut e, "level"));
        // 10 ms of 1 kHz: ten cycles, twenty sign changes.
        assert_eq!(num(&mut e, "slots"), 200.0);
        assert!((19.0..=21.0).contains(&num(&mut e, "flips")), "{}", num(&mut e, "flips"));
        assert_eq!(num(&mut e, "quiet"), 0.0, "past the sound's end: silence");
        assert_eq!(num(&mut e, "plain"), 32.0, "32 bands by default");
    }

    #[test]
    fn features_read_as_silence_when_absent_and_as_data_when_given() {
        let source = "use mod.motion.*\nlet v = voice(@vo)\nlet m = music(@m)\n{a: v.amplitude(1.2) b: m.energy(0.0) c: v.words.len() d: m.band(@high, 0.0) e: v.progress(1, 1.7)}";
        let mut silent = evaluate(source, Arc::new(crate::features::AudioFeatures::default()), 0.0, &[], &crate::modules::NoModules);
        assert!(silent.errors.is_empty(), "{:?}", silent.errors);
        assert_eq!(field(&mut silent, "a").and_then(|v| v.num()), Some(0.0));
        assert_eq!(field(&mut silent, "c").and_then(|v| v.num()), Some(0.0));
        let mut given = evaluate(source, Arc::new(crate::features::AudioFeatures::synthetic("say it on the beat", 1.0, 2.0, 120.0, 8.0)), 0.0, &[], &crate::modules::NoModules);
        assert!(field(&mut given, "a").and_then(|v| v.num()).unwrap() > 0.1);
        assert_eq!(field(&mut given, "c").and_then(|v| v.num()), Some(5.0));
        assert_eq!(field(&mut given, "d").and_then(|v| v.num()), Some(0.125), "the high band's pulse is half a beat late");
        assert!((field(&mut given, "e").and_then(|v| v.num()).unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn voice_words_carry_their_syllables() {
        // "hello" is two syllables over 1.0..1.4 s; "sky" is left unsplit.
        let mut features = crate::features::AudioFeatures::synthetic("hello sky", 1.0, 2.0, 120.0, 4.0);
        features.voices[0].1.words[1].syllables.clear();
        let source = "use mod.motion.*\nlet w = voice(@vo).words\n{n: w[0].syllables.len() s: w[0].syllables[1].start e: w[0].syllables[1].end m: w[1].syllables.len() us: w[1].syllables[0].start ue: w[1].syllables[0].end}";
        let mut evaluated = evaluate(source, Arc::new(features), 0.0, &[], &crate::modules::NoModules);
        assert!(evaluated.errors.is_empty(), "{:?}", evaluated.errors);
        let mut n = |k: &str| field(&mut evaluated, k).and_then(|v| v.num()).unwrap_or_else(|| panic!("{k}"));
        assert_eq!(n("n"), 2.0);
        let (s, e) = (n("s"), n("e"));
        assert!((s - 1.2).abs() < 1e-9 && (e - 1.4).abs() < 1e-9, "{s} {e}");
        assert_eq!(n("m"), 1.0, "a word without spans is one syllable");
        let (us, ue) = (n("us"), n("ue"));
        assert!((us - 1.5).abs() < 1e-9 && (ue - 1.9).abs() < 1e-9, "{us} {ue}");
    }
}

unsafe impl makepad_script::ScriptHostCapability for DocHost {
    const ID: makepad_script::LiveId =
        makepad_script::LiveId::from_str("makepad.motion.doc_host.v1");
}

/// The document's sung words and beat grid on its own clock (host time minus
/// where it starts), for the scenes' `word_end(..)`, `words(..)` and
/// `beat_near(..)`.
#[cfg(feature = "scene3d")]
pub(crate) fn song_timing(signals: &dyn SignalSource, offset: f64) -> Option<Arc<makepad_stage_motion3d::script::SongTiming>> {
    use makepad_stage_motion3d::script::{SongTiming, SongWord};
    let voice = signals.default_voice().and_then(|v| signals.voice(v));
    let grid = signals.timing(signals.default_music()).grid;
    if voice.is_none() && grid.is_none() {
        return None;
    }
    let local = |t: f64| (t - offset) as f32;
    // The beat grid the document follows (the music's beats, or the edit's
    // steady tempo), as beat times around the document: 2 minutes either side.
    let (beats, beats_per_bar, downbeat) = match &grid {
        Some(g) => {
            let first = g.beat_at(offset - 120.0).floor() as i64;
            let last = g.beat_at(offset + 120.0).ceil() as i64;
            let per = g.beats_per_bar.max(1) as i64;
            let beats: Vec<f32> = (first..=last).map(|b| local(g.time_of_beat(b as f64))).collect();
            // The index of the first downbeat in this list.
            let down = ((g.downbeat as i64 - first) % per + per) % per;
            (beats, g.beats_per_bar.max(1), down as u32)
        }
        None => (Vec::new(), 4, 0),
    };
    Some(Arc::new(SongTiming {
        words: voice.map(|v| v.words.iter().map(|w| SongWord { text: w.text.clone(), start: local(w.start), end: local(w.end), line: w.line }).collect()).unwrap_or_default(),
        beats,
        beats_per_bar,
        downbeat,
    }))
}
