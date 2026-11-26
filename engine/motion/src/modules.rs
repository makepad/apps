//! Splash modules a Motion document imports (the effects library and an
//! AI's own): `use mod.motion.modules.<name>.*` for a built-in module
//! (`engine/motion/modules/<name>.splash`, one effect family each, listed in
//! its INDEX.md) or one a host installed ([`install`]), `let m =
//! import(asset("<id>", "<rev>"))` for a library revision the host
//! resolves (a Motion asset tagged `module`, pinned like any asset). A module is ordinary Splash whose last expression is an
//! object of its exports; it sees what a document sees (math, the shared
//! stdlib, `mod.motion`) and imports modules the same way. Loading is
//! `makepad-script-compute`'s ([`imports`]).

use crate::AssetRef;
pub use makepad_script_imports::{self as imports, LoadedModule, MemoryModules, ModuleInfo, ModuleRef, ModuleResolver, ModuleSource, NoModules, MODULE_TAG};
use std::sync::Arc;

/// Where a document names a built-in (or host) module.
pub const NAMED: &str = "mod.motion.modules.";

/// The library's index: each effect, what it looks like, its module.
pub const INDEX: &str = include_str!("../modules/INDEX.md");

/// The effects library: name, version, source. Each file's first comment
/// line is its one-line description (the index, the guide).
pub const BUILTIN: &[(&str, &str, &str)] = &[
    ("accents", "1", include_str!("../modules/accents.splash")),
    ("camera2d", "1", include_str!("../modules/camera2d.splash")),
    ("color", "1", include_str!("../modules/color.splash")),
    ("film", "1", include_str!("../modules/film.splash")),
    ("kinetic", "1", include_str!("../modules/kinetic.splash")),
    ("numbers", "1", include_str!("../modules/numbers.splash")),
    ("paths", "1", include_str!("../modules/paths.splash")),
    ("plot", "1", include_str!("../modules/plot.splash")),
    ("print", "1", include_str!("../modules/print.splash")),
    ("song", "1", include_str!("../modules/song.splash")),
    ("sparks", "1", include_str!("../modules/sparks.splash")),
    ("spectrum", "1", include_str!("../modules/spectrum.splash")),
    ("strokes", "1", include_str!("../modules/strokes.splash")),
    ("style", "1", include_str!("../modules/style.splash")),
    ("words", "1", include_str!("../modules/words.splash")),
];

pub fn builtin(name: &str) -> Option<ModuleSource> {
    // The Seq3D templates are motion3d's (its own documents build them too).
    #[cfg(feature = "scene3d")]
    if name == "seq3d" {
        return Some(ModuleSource::new(name, "1", makepad_stage_motion3d::seq3d::MODULE));
    }
    BUILTIN.iter().copied().chain(installed()).find(|m| m.0 == name).map(|m| ModuleSource::new(m.0, m.1, m.2))
}

static INSTALLED: std::sync::RwLock<Vec<(&'static str, &'static str, &'static str)>> = std::sync::RwLock::new(Vec::new());

/// A named module a host installs beside the built-ins (a content pack's
/// own, such as the Text3D presets): `mod.motion.modules.<name>` to every
/// document, as a built-in is. A name already built in or installed keeps
/// its first source.
pub fn install(name: &'static str, version: &'static str, source: &'static str) {
    let mut installed = INSTALLED.write().unwrap_or_else(|e| e.into_inner());
    if !BUILTIN.iter().any(|m| m.0 == name) && !installed.iter().any(|m| m.0 == name) {
        installed.push((name, version, source));
    }
}

/// The modules hosts installed, in install order: name, version, source.
pub fn installed() -> Vec<(&'static str, &'static str, &'static str)> {
    INSTALLED.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The host's module resolver in a [`crate::MotionContext`]: library
/// revisions (and any named modules of its own); the built-ins come after.
/// The default is the application's library ([`set_library`]), or none.
#[derive(Clone)]
pub struct Modules(pub Arc<dyn ModuleResolver>);

static LIBRARY: std::sync::RwLock<Option<Arc<dyn ModuleResolver>>> = std::sync::RwLock::new(None);

/// The application's module library (Stage's asset store): what every
/// document loaded without a resolver of its own imports from.
pub fn set_library(library: Arc<dyn ModuleResolver>) {
    *LIBRARY.write().unwrap_or_else(|e| e.into_inner()) = Some(library);
}

impl Default for Modules {
    fn default() -> Self {
        let library = LIBRARY.read().unwrap_or_else(|e| e.into_inner()).clone();
        Self(library.unwrap_or_else(|| Arc::new(NoModules)))
    }
}

/// The same resolver (a frame's scenes resolve their imports through it).
impl PartialEq for Modules {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Modules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Modules")
    }
}

impl Modules {
    /// Library revisions from their texts: `(id, rev, source)`.
    pub fn from_sources<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> Self {
        let mut mem = MemoryModules::default();
        for (id, rev, text) in sources {
            mem.add_asset(id, rev, text);
        }
        Self(Arc::new(mem))
    }
}

struct WithBuiltins<'a>(&'a dyn ModuleResolver);

impl ModuleResolver for WithBuiltins<'_> {
    fn named(&self, name: &str) -> Option<ModuleSource> {
        self.0.named(name).or_else(|| builtin(name))
    }
    fn asset(&self, id: &str, rev: &str) -> Option<ModuleSource> {
        self.0.asset(id, rev)
    }
    fn path(&self, from: &str, path: &str) -> Result<ModuleSource, String> { self.0.path(from, path) }
    fn validate(&self, source: &str, file: &str) -> Result<(), Vec<String>> { self.0.validate(source, file) }
    fn named_hint(&self, name: &str) -> String {
        let installed = installed();
        let names: Vec<&str> = BUILTIN.iter().chain(installed.iter()).map(|m| m.0).collect();
        let near = makepad_script::suggest::nearest(name, names.iter().copied(), 1).first().copied();
        match near {
            Some(n) => format!(" Did you mean `mod.motion.modules.{n}`?"),
            None => format!(" Built-in modules: {}.", names.join(", ")),
        }
    }
}

/// Every module `source` uses and what they import, dependencies first.
pub fn resolve(source: &str, host: &dyn ModuleResolver) -> Result<Vec<LoadedModule>, Vec<String>> {
    host.validate(source, "")?;
    imports::resolve(&imports::scan(source, Some(NAMED)), &WithBuiltins(host), Some(NAMED))
}

/// The texts of an inline document's tokens, in order (comments are not
/// tokens): what its module references and `use` lines are found in.
fn token_texts(image: &makepad_script::ScriptFnImage) -> Vec<&str> {
    image.tokens().map(|(_, text)| text).collect()
}

/// A string token's text without its quotes.
fn unquoted(text: &str) -> Option<&str> {
    text.strip_prefix('"')?.strip_suffix('"')
}

/// Every module an inline document uses, from its tokens: `mod.motion.modules.<name>`
/// and `import(asset("id", "rev"))`, in order of first mention.
pub fn scan_image(image: &makepad_script::ScriptFnImage) -> Vec<ModuleRef> {
    let t = token_texts(image);
    let mut out: Vec<ModuleRef> = Vec::new();
    let mut push = |r: ModuleRef| {
        if !out.contains(&r) {
            out.push(r);
        }
    };
    for i in 0..t.len() {
        if t[i] == "mod" && t.get(i + 1..i + 6) == Some(&["." , "motion", ".", "modules", "."][..]) {
            if let Some(name) = t.get(i + 6).filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
                push(ModuleRef::Named((*name).to_string()));
            }
        }
        if t[i] == "import" && t.get(i + 1) == Some(&"(") && t.get(i + 3) == Some(&")") {
            if let Some(path) = t.get(i + 2).and_then(|s| unquoted(s)) {
                push(ModuleRef::Path { from: String::new(), path: path.into() });
            }
        }
        if t[i] == "import" && t.get(i + 1) == Some(&"(") && t.get(i + 2) == Some(&"asset") && t.get(i + 3) == Some(&"(") && t.get(i + 5) == Some(&",") {
            if let (Some(id), Some(rev)) = (t.get(i + 4).and_then(|s| unquoted(s)), t.get(i + 6).and_then(|s| unquoted(s))) {
                push(ModuleRef::Asset { id: id.to_string(), rev: rev.to_string() });
            }
        }
    }
    out
}

/// The vocabularies an inline document globs (`use mod.<name>.*`), from its tokens.
pub(crate) fn image_globs(image: &makepad_script::ScriptFnImage) -> Vec<String> {
    let t = token_texts(image);
    let mut out: Vec<String> = Vec::new();
    for i in 0..t.len() {
        if t[i] == "use" && t.get(i + 1) == Some(&"mod") && t.get(i + 2) == Some(&".") && t.get(i + 4) == Some(&".") && t.get(i + 5) == Some(&"*") {
            let name = t[i + 3].to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Every module an inline document uses and what they import, dependencies first.
pub fn resolve_image(image: &makepad_script::ScriptFnImage, host: &dyn ModuleResolver) -> Result<Vec<LoadedModule>, Vec<String>> {
    host.validate(image.text(), image.file())?;
    imports::resolve(&scan_image(image), &WithBuiltins(host), Some(NAMED))
}

/// The library revisions `source` imports directly (a host fetches them,
/// then what they import: [`imports::scan`] on each).
pub fn asset_imports(source: &str) -> Vec<AssetRef> {
    imports::scan(source, None).into_iter().filter_map(|r| match r {
        ModuleRef::Asset { id, rev } => Some(AssetRef { id, rev }),
        ModuleRef::Named(_) | ModuleRef::Path { .. } => None,
    }).collect()
}

/// A module's one-line doc, exports and imports (the library's record).
pub fn module_info(source: &str) -> ModuleInfo {
    imports::module_info(source, Some(NAMED))
}

/// Whether `source` is a module (not a Motion document): it has no `Motion{`.
pub fn is_module(source: &str) -> bool {
    !source.contains("Motion{")
}

/// Load `source` as a module the way a document would (its imports from
/// `host`, then the library), for a library to publish it: its record, or
/// what is wrong (`module:<name>:line:col: …`).
pub fn check(source: &str, host: Modules) -> Result<ModuleInfo, Vec<String>> {
    struct Probe(Arc<str>, Modules);
    impl ModuleResolver for Probe {
        fn named(&self, name: &str) -> Option<ModuleSource> {
            (name == "module_under_check").then(|| ModuleSource::new(name, "0", self.0.clone())).or_else(|| self.1 .0.named(name))
        }
        fn asset(&self, id: &str, rev: &str) -> Option<ModuleSource> { self.1 .0.asset(id, rev) }
        fn path(&self, from: &str, path: &str) -> Result<ModuleSource, String> { self.1 .0.path(from, path) }
        fn validate(&self, source: &str, file: &str) -> Result<(), Vec<String>> { self.1 .0.validate(source, file) }
        fn install(&self, vm: &mut makepad_script::ScriptVm, module: makepad_script::ScriptObject) { self.1 .0.install(vm, module); }
        fn document_file(&self) -> Option<&str> { self.1 .0.document_file() }
    }
    let probe = "use mod.motion.*\nuse mod.motion.modules.module_under_check.*\nMotion{duration: 1.0 layers: [Rect{size: vec2(1, 1) fill: #ffffff}]}";
    let context = crate::MotionContext { modules: Modules(Arc::new(Probe(source.into(), host))), ..Default::default() };
    match crate::MotionDoc::load_with(probe, &[], context) {
        Ok(_) => Ok(module_info(source)),
        Err(diags) => Err(diags.iter().filter(|d| d.is_error()).map(|d| d.message.clone()).collect()),
    }
}

/// One of Motion's own Splash modules (`name`, `text`; its `use`s are
/// built-in modules) loaded into a document's VM: its exports.
pub(crate) fn load_internal(vm: &mut makepad_script::ScriptVm, name: &str, text: &str) -> Result<makepad_script::ScriptObject, Vec<makepad_script::ScriptErrorRecord>> {
    use makepad_script::*;
    let mut loaded = resolve(text, &NoModules).map_err(|e| e.into_iter().map(ScriptErrorRecord::plain).collect::<Vec<_>>())?;
    loaded.push(LoadedModule { reference: ModuleRef::Named(name.into()), source: ModuleSource::new(name, "1", text) });
    let motion = vm.bx.heap_ref().value(vm.modules(), id!(motion).into(), NoTrap).as_object().ok_or_else(|| vec![ScriptErrorRecord::plain("no mod.motion".to_string())])?;
    // Into the document's `mod.motion.modules` (so `use mod.motion.modules.<dep>.*` finds a dependency).
    let named = match vm.bx.heap_ref().value(motion, id_lut!(modules).into(), NoTrap).as_object() {
        Some(named) => named,
        None => {
            let named = vm.bx.heap_mut().new_object();
            vm.bx.heap_mut().set_value_def(motion, id_lut!(modules).into(), named.into());
            named
        }
    };
    let core = imports::language_core(vm);
    let errors = imports::load(vm, motion, &core, &loaded, Some(named), 50_000_000);
    if !errors.is_empty() {
        return Err(errors);
    }
    vm.bx.heap_ref().value(named, LiveId::from_str(name).into(), NoTrap).as_object().ok_or_else(|| vec![ScriptErrorRecord::plain(format!("module {name} has no exports"))])
}

/// `message` at where `obj` was made in the document (the form the VM's own
/// errors take, so the loader places it).
pub(crate) fn error_at(vm: &makepad_script::ScriptVm, obj: makepad_script::ScriptObject, message: &str) -> makepad_script::ScriptErrorRecord {
    let loc = vm.bx.code_ref().ip_to_loc(vm.bx.heap_ref().made_at(obj));
    let text = match &loc {
        Some(loc) => format!("{}:{}:{}: {message}", loc.file(), loc.line(), loc.col()),
        None => message.to_string(),
    };
    makepad_script::ScriptErrorRecord::new(loc.as_ref(), message, &text)
}
