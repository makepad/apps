//! `AppRunDoc::load`: evaluate an `AppRun{...}` file and
//! check its header, collecting every problem with its source location.
//!
//! The file is ordinary Splash: `use mod.apprun.*` brings in the `AppRun`
//! prototype and the two coordinate helpers, and the file's value is the
//! `AppRun{...}` object. Loading never calls `script`; the executor
//! evaluates the file again in its own VM and calls it with the app.

use crate::values::{read, split_error, Sv};
use makepad_script::*;

/// The frame rates a capture takes (`cap start` accepts 1..=240).
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=240;
/// Launch flags the executor sets itself; a run's `args` may not repeat them.
const RESERVED_ARGS: &[&str] = &["--remote", "--virtual-clock", "--window", "--seed", "--remote-title-tag"];
/// The only environment a run file may set: `MAKEPAD_APP_*`, variables an
/// app reads for itself. Nothing that changes how a process loads
/// (`DYLD_*`, `LD_*`, `PATH`), where Makepad keeps state (`MAKEPAD_HOME`,
/// always the run's own scratch home) or what the executor controls.
pub const ENV_PREFIX: &str = "MAKEPAD_APP_";
/// A run's recording may not be longer unless its `max_duration` says so.
pub const DEFAULT_MAX_DURATION: f64 = 600.0;
const FIELDS: &[&str] = &["label", "app", "args", "env", "output", "theme", "seed", "audio", "cursor", "max_duration", "script"];

/// Pixel size, scale and frame rate of a run's recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunOutput {
    pub width: u32,
    pub height: u32,
    pub dpi_scale: f64,
    pub fps: u32,
}

impl RunOutput {
    /// The window's logical size in points: the pixel size at the scale.
    pub fn logical(&self) -> (f64, f64) {
        (self.width as f64 / self.dpi_scale, self.height as f64 / self.dpi_scale)
    }

    /// `--window WxH@scale`.
    pub fn window_arg(&self) -> String {
        let (w, h) = self.logical();
        format!("{}x{}@{}", fmt_num(w), fmt_num(h), fmt_num(self.dpi_scale))
    }
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMode {
    None,
    Synthetic,
}

/// A checked run file. `source` is kept verbatim: it is what is published
/// (`run.splash`) and what the executor evaluates.
#[derive(Clone, Debug, PartialEq)]
pub struct AppRunDoc {
    pub source: String,
    pub label: String,
    /// Shared app registry id (`makepad_app_registry::find`).
    pub app: String,
    pub args: Vec<String>,
    /// Extra environment for the app, sorted by name.
    pub env: Vec<(String, String)>,
    pub output: RunOutput,
    /// A UI theme from the catalogue, handed to the app as a file
    /// (`MAKEPAD_WIDGET_STYLE_FILE`); `None` is the plain
    /// theme (also written `"default"` or `"dark"`: the plain theme is dark).
    pub theme: Option<String>,
    pub seed: u64,
    pub audio: bool,
    pub cursor: CursorMode,
    /// Seconds of clock the run may take (`DEFAULT_MAX_DURATION`); longer
    /// fails it rather than recording on.
    pub max_duration: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based, in the run file.
    pub line: u32,
    pub col: u32,
    /// The `AppRun` field it is about, when there is one.
    pub field: Option<String>,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// How the loader looks an app id up. The registry in production; tests
/// substitute their own.
pub enum AppLookup {
    Known,
    /// In the registry, but its sources are not in this checkout.
    Unavailable,
    Unknown,
}

pub fn registry_lookup(id: &str) -> AppLookup {
    match makepad_app_registry::find(id) {
        Some(entry) if makepad_app_registry::is_available(entry) => AppLookup::Known,
        Some(_) => AppLookup::Unavailable,
        None => AppLookup::Unknown,
    }
}

impl AppRunDoc {
    pub fn load(source: &str) -> Result<AppRunDoc, Vec<Diagnostic>> {
        Self::load_with(source, &registry_lookup)
    }

    pub fn load_with(source: &str, lookup: &dyn Fn(&str) -> AppLookup) -> Result<AppRunDoc, Vec<Diagnostic>> {
        let mut host = ScriptVmHost::new((), ());
        let mut vm = ScriptVm {
            host: host.host_context(),
            bx: Box::new(ScriptVmBase::new()),
        };
        let (value, errors) = eval_run(&mut vm, source, "run.splash");
        let mut diags: Vec<Diagnostic> = errors
            .iter()
            .map(|e| {
                let (line, col, message) = split_error(e);
                Diagnostic { line, col, field: None, message }
            })
            .collect();
        let root = match value {
            Some(root) => root,
            None => {
                diags.push(Diagnostic { line: 1, col: 1, field: None, message: "a run file's value is one `AppRun{...}`".into() });
                return Err(diags);
            }
        };
        let doc = check(source, &read(&vm, root), lookup, &mut diags);
        if diags.is_empty() {
            Ok(doc)
        } else {
            diags.sort_by_key(|d| (d.line, d.col));
            Err(diags)
        }
    }
}

/// Install `mod.apprun` and evaluate the file. Returns the `AppRun` object
/// (None when the file's value is something else) and the VM's errors.
pub(crate) fn eval_run(vm: &mut ScriptVm, source: &str, file: &str) -> (Option<ScriptValue>, Vec<String>) {
    let proto = install_prelude(vm);
    vm.capture_errors();
    let value = vm.with_instruction_limit(1_000_000, |vm| {
        vm.eval(ScriptMod::new("", "", &(file), 0, 0, &(source), &[]))
    });
    let errors = vm.take_errors();
    vm.capture_errors();
    let is_run = value.as_object().is_some_and(|o| vm.bx.heap_ref().proto(o) == proto.into());
    (is_run.then_some(value), errors)
}

/// `mod.apprun`: the `AppRun` prototype, `vec2(x, y)` (a point in window
/// points, `[x, y]`) and `rect(x, y, w, h)` (`[x, y, w, h]`). Ids read as
/// widget paths: `@main.new_note` is the id `main.new_note`.
pub(crate) fn install_prelude(vm: &mut ScriptVm) -> ScriptObject {
    let module = vm.new_module(id!(apprun));
    let proto = vm.bx.heap_mut().new_object();
    vm.bx.heap_mut().set_value_def(module, id!(AppRun).into(), proto.into());
    vm.add_method(module, id_lut!(vec2), script_args!(x = NIL, y = NIL), |vm, args| {
        numbers_array(vm, args, &[id!(x), id!(y)])
    });
    vm.add_method(module, id_lut!(rect), script_args!(x = NIL, y = NIL, w = NIL, h = NIL), |vm, args| {
        numbers_array(vm, args, &[id!(x), id!(y), id!(w), id!(h)])
    });
    install_path_ids(vm);
    proto
}

/// A field of an id extends it: `@a.b` is the path id `a.b`. Only the VM
/// that installs this gets the getter; an id has no fields otherwise.
pub fn install_path_ids(vm: &mut ScriptVm) {
    vm.set_type_getter_native(ScriptValueType::REDUX_ID, makepad_script::native::NativeGetterFn::new(|_vm, value, field| {
        let Some(base) = value.as_id() else { return NIL };
        let path = format!(
            "{}.{}",
            base.as_string(|s| s.map(str::to_string)).unwrap_or_default(),
            field.as_string(|s| s.map(str::to_string)).unwrap_or_default()
        );
        match LiveId::from_str_with_lut(&path) {
            Ok(id) => ScriptValue::from_escaped_id(id),
            Err(_) => NIL,
        }
    }));
}

fn numbers_array(vm: &mut ScriptVm, args: ScriptObject, keys: &[LiveId]) -> ScriptValue {
    let array = vm.bx.heap_mut().new_array();
    for key in keys {
        let v = vm.bx.heap_ref().value(args, (*key).into(), NoTrap);
        vm.bx.heap_mut().array_push(array, v, NoTrap);
    }
    array.into()
}

/// Where `name:` is written in the file, 1-based; (1, 1) when it is not.
pub(crate) fn field_loc(source: &str, name: &str) -> (u32, u32) {
    for (row, line) in source.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        let mut from = 0;
        while let Some(at) = code[from..].find(name) {
            let start = from + at;
            let end = start + name.len();
            let before_ok = start == 0 || !code.as_bytes()[start - 1].is_ascii_alphanumeric() && code.as_bytes()[start - 1] != b'_';
            let rest = code[end..].trim_start();
            if before_ok && rest.starts_with(':') && !rest.starts_with("::") {
                return (row as u32 + 1, start as u32 + 1);
            }
            from = end;
        }
    }
    (1, 1)
}

fn check(source: &str, root: &Sv, lookup: &dyn Fn(&str) -> AppLookup, diags: &mut Vec<Diagnostic>) -> AppRunDoc {
    let mut err = |field: &str, message: String| {
        let (line, col) = field_loc(source, field);
        diags.push(Diagnostic { line, col, field: Some(field.to_string()), message });
    };
    let empty = Vec::new();
    let fields = match root {
        Sv::Obj(fields) => fields,
        _ => &empty,
    };
    for (key, _) in fields {
        if !FIELDS.contains(&key.as_str()) {
            err(key, format!("`{key}` is not an AppRun field (fields: {})", FIELDS.join(", ")));
        }
    }
    let get = |name: &str| root.field(name).unwrap_or(&Sv::Nil);

    let label = match get("label") {
        Sv::Str(s) if !s.trim().is_empty() => s.clone(),
        Sv::Nil => {
            err("label", "an app run has a `label`".into());
            String::new()
        }
        other => {
            err("label", format!("`label` is a non-empty string, not {}", other.kind()));
            String::new()
        }
    };
    let app = match get("app") {
        Sv::Str(id) => {
            match lookup(id) {
                AppLookup::Known => {}
                AppLookup::Unavailable => err("app", format!("app `{id}` is in the registry but its sources are not in this checkout")),
                AppLookup::Unknown => err("app", format!("`{id}` is not a registry app id{}", suggest(id))),
            }
            id.clone()
        }
        other => {
            err("app", format!("`app` is a registry app id string, not {}", other.kind()));
            String::new()
        }
    };
    let mut args = Vec::new();
    match get("args") {
        Sv::Nil => {}
        Sv::Arr(items) => {
            for item in items {
                match item {
                    Sv::Str(s) => {
                        let flag = s.split('=').next().unwrap_or("");
                        if RESERVED_ARGS.contains(&flag) {
                            err("args", format!("`{s}`: the executor sets {flag} itself (from output, seed and the run)"));
                        }
                        args.push(s.clone());
                    }
                    other => err("args", format!("`args` holds strings, not {}", other.kind())),
                }
            }
        }
        other => err("args", format!("`args` is an array of strings, not {}", other.kind())),
    }
    let mut env = Vec::new();
    match get("env") {
        Sv::Nil => {}
        Sv::Obj(pairs) => {
            for (key, value) in pairs {
                let name_ok = key.len() > ENV_PREFIX.len() && key.starts_with(ENV_PREFIX) && key.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
                if !name_ok {
                    err("env", format!("`{key}`: a run may only set {ENV_PREFIX}* variables (A-Z, 0-9, _)"));
                }
                match value {
                    Sv::Str(v) => env.push((key.clone(), v.clone())),
                    Sv::Num(n) => env.push((key.clone(), fmt_num(*n))),
                    other => err("env", format!("`env.{key}` is a string, not {}", other.kind())),
                }
            }
        }
        other => err("env", format!("`env` is an object of strings, not {}", other.kind())),
    }
    let output = check_output(get("output"), &mut err);
    let theme = match get("theme") {
        Sv::Nil => None,
        Sv::Str(name) if name == "default" || name == "dark" => None,
        Sv::Str(name) => {
            // A theme from the catalogue, handed to the app as a
            // file; a dark appearance is a sheet of its own there.
            if makepad_widgets_core::desktop_style::find(name).is_none() {
                let ids: Vec<&str> = makepad_widgets_core::desktop_style::catalogue().into_iter().map(|entry| entry.id).collect();
                err("theme", format!("`{name}` is not a UI theme (\"default\" or one of {})", ids.join(", ")));
            }
            Some(name.clone())
        }
        other => err_value(&mut err, "theme", "a theme name string", other),
    };
    let seed = match get("seed") {
        Sv::Nil => 1,
        Sv::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 => *n as u64,
        other => {
            err("seed", format!("`seed` is a whole number >= 0, not {other:?}"));
            1
        }
    };
    let audio = match get("audio") {
        Sv::Nil => false,
        Sv::Bool(b) => *b,
        other => err_value(&mut err, "audio", "true or false", other).unwrap_or(false),
    };
    let cursor = match get("cursor") {
        Sv::Nil => CursorMode::Synthetic,
        Sv::Id(id) if id == "synthetic" => CursorMode::Synthetic,
        Sv::Id(id) if id == "none" => CursorMode::None,
        other => err_value(&mut err, "cursor", "@none or @synthetic", other).unwrap_or(CursorMode::None),
    };
    let max_duration = match get("max_duration") {
        Sv::Nil => DEFAULT_MAX_DURATION,
        Sv::Num(n) if *n > 0.0 && n.is_finite() => *n,
        other => err_value(&mut err, "max_duration", "a positive number of seconds", other).unwrap_or(DEFAULT_MAX_DURATION),
    };
    match get("script") {
        Sv::Fn => {}
        Sv::Nil => err("script", "an app run has `script: fn(app) {...}`".into()),
        other => err("script", format!("`script` is fn(app) {{...}}, not {}", other.kind())),
    }
    AppRunDoc { source: source.to_string(), label, app, args, env, output, theme, seed, audio, cursor, max_duration }
}

fn err_value<T>(err: &mut impl FnMut(&str, String), field: &str, want: &str, got: &Sv) -> Option<T> {
    err(field, format!("`{field}` is {want}, not {}", got.kind()));
    None
}

impl RunOutput {
    /// The run file's output rules for a size given elsewhere (the CLI's
    /// `--output`).
    pub fn problems(&self) -> Vec<String> {
        let v = Sv::Obj(vec![
            ("dpi_scale".into(), Sv::Num(self.dpi_scale)),
            ("fps".into(), Sv::Num(self.fps as f64)),
            ("height".into(), Sv::Num(self.height as f64)),
            ("width".into(), Sv::Num(self.width as f64)),
        ]);
        let mut out = Vec::new();
        check_output(&v, &mut |_: &str, m: String| out.push(m));
        out
    }
}

fn check_output(value: &Sv, err: &mut impl FnMut(&str, String)) -> RunOutput {
    let mut output = RunOutput { width: 0, height: 0, dpi_scale: 1.0, fps: 60 };
    let Sv::Obj(fields) = value else {
        err("output", "an app run declares `output: {width height dpi_scale fps}`".into());
        return output;
    };
    for (key, _) in fields {
        if !["width", "height", "dpi_scale", "fps"].contains(&key.as_str()) {
            err("output", format!("`output.{key}` is not an output field (width, height, dpi_scale, fps)"));
        }
    }
    let num = |name: &str| value.field(name).and_then(Sv::num);
    let size = |name: &str, err: &mut dyn FnMut(&str, String)| match num(name) {
        Some(v) if v.fract() == 0.0 && (2.0..=16384.0).contains(&v) && (v as u32) % 2 == 0 => v as u32,
        _ => {
            err("output", format!("`output.{name}` is an even whole number of pixels, 2 to 16384 (H.264 4:2:0 needs even sizes)"));
            0
        }
    };
    output.width = size("width", err);
    output.height = size("height", err);
    output.dpi_scale = match num("dpi_scale") {
        None if value.field("dpi_scale").is_none() => 1.0,
        Some(s) if (0.5..=8.0).contains(&s) => s,
        _ => {
            err("output", "`output.dpi_scale` is a number from 0.5 to 8".into());
            1.0
        }
    };
    output.fps = match num("fps") {
        Some(f) if f.fract() == 0.0 && FPS_RANGE.contains(&(f as u32)) => f as u32,
        _ => {
            err("output", format!("`output.fps` is a whole number from {} to {}", FPS_RANGE.start(), FPS_RANGE.end()));
            60
        }
    };
    if output.width > 0 && output.height > 0 {
        let (w, h) = output.logical();
        let integral = |v: f64| (v - v.round()).abs() < 1e-9;
        if !integral(w) || !integral(h) {
            err(
                "output",
                format!(
                    "{}x{} px is not a whole window at dpi_scale {}: {}x{} points (pick a size divisible by the scale)",
                    output.width, output.height, fmt_num(output.dpi_scale), w, h
                ),
            );
        }
    }
    output
}

/// Registry ids near `id`: shared prefix or small edit distance.
fn suggest(id: &str) -> String {
    let mut near: Vec<&str> = makepad_app_registry::apps()
        .iter()
        .map(|e| e.id)
        .filter(|other| edit_distance(id, other) <= 2 || other.starts_with(id) || id.starts_with(other))
        .collect();
    near.truncate(5);
    if near.is_empty() {
        let all: Vec<&str> = makepad_app_registry::apps().iter().map(|e| e.id).collect();
        format!(" (registry ids: {})", all.join(", "))
    } else {
        format!(" (did you mean {}?)", near.join(", "))
    }
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + (ca != *cb) as usize).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"use mod.apprun.*

AppRun{
    label: "Notes: create a note"
    app: "notes"                     // shared app registry id
    args: ["--demo"]
    output: {width: 1080 height: 1920 dpi_scale: 3.0 fps: 60}
    theme: "dark"
    seed: 1
    audio: false
    cursor: @synthetic               // @none | @synthetic
    script: fn(app) {
        app.wait_settled()
        app.hold(0.5)
        app.click(@main.new_note)
        app.type(@editor.body, "Groceries: oat milk, lemons", {cps: 14})
        app.mark(@note_saved)
        app.key(@KeyS, {cmd: true})
        app.wait_text(@toast, "Saved")
        app.hold(1.5)
    }
}
"#;

    fn known(_: &str) -> AppLookup {
        AppLookup::Known
    }

    #[test]
    fn the_theme_names_are_the_catalogue_themes() {
        makepad_widgets_sheets::register();
        let theme = |name: &str| {
            let src = EXAMPLE.replace("theme: \"dark\"", &format!("theme: \"{name}\""));
            AppRunDoc::load_with(&src, &known).map(|doc| doc.theme)
        };
        for entry in makepad_widgets_core::desktop_style::catalogue() {
            assert_eq!(theme(entry.id).ok().flatten().as_deref(), Some(entry.id), "{} is refused", entry.id);
        }
        // The library's and Stage's alike, dark variants included.
        for name in ["macos-dark", "steampunk-brass", "neumorphic-dark"] {
            assert!(theme(name).is_ok(), "{name}");
        }
        for name in ["beos", "omarchy-dark"] {
            assert!(theme(name).is_err(), "{name} is taken");
        }
    }

    #[test]
    fn the_spec_example_loads() {
        let doc = AppRunDoc::load_with(EXAMPLE, &known).unwrap();
        assert_eq!(doc.label, "Notes: create a note");
        assert_eq!(doc.app, "notes");
        assert_eq!(doc.args, vec!["--demo".to_string()]);
        assert_eq!(doc.output, RunOutput { width: 1080, height: 1920, dpi_scale: 3.0, fps: 60 });
        assert_eq!(doc.output.window_arg(), "360x640@3");
        assert_eq!((doc.theme, doc.seed, doc.audio, doc.cursor), (None, 1, false, CursorMode::Synthetic));
        assert_eq!(doc.max_duration, DEFAULT_MAX_DURATION);
    }

    #[test]
    fn a_run_cannot_set_loader_path_or_home_variables() {
        for name in ["DYLD_INSERT_LIBRARIES", "LD_PRELOAD", "PATH", "HOME", "MAKEPAD_HOME", "MAKEPAD_APP_"] {
            let bad = EXAMPLE.replace("args: [\"--demo\"]", &format!("env: {{{name}: \"/tmp/x\"}}"));
            let diags = AppRunDoc::load_with(&bad, &known).unwrap_err();
            assert!(diags.iter().any(|d| d.field.as_deref() == Some("env")), "{name}: {diags:#?}");
        }
        let ok = EXAMPLE.replace("args: [\"--demo\"]", "env: {MAKEPAD_APP_DEMO: \"1\"}");
        assert_eq!(AppRunDoc::load_with(&ok, &known).unwrap().env, vec![("MAKEPAD_APP_DEMO".to_string(), "1".to_string())]);
        assert!(RunOutput { width: 1081, height: 1920, dpi_scale: 3.0, fps: 60 }.problems().len() >= 1);
        assert!(RunOutput { width: 1080, height: 1920, dpi_scale: 3.0, fps: 60 }.problems().is_empty());
    }

    #[test]
    fn the_registry_is_the_default_lookup() {
        let doc = AppRunDoc::load(&EXAMPLE.replace("\"notes\"", "\"counter\"")).unwrap();
        assert_eq!(doc.app, "counter");
        let diags = AppRunDoc::load(&EXAMPLE.replace("\"notes\"", "\"countr\"")).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!((diags[0].line, diags[0].field.as_deref()), (5, Some("app")));
        assert!(diags[0].message.contains("did you mean counter"), "{}", diags[0].message);
    }

    #[test]
    fn every_header_problem_is_reported_where_it_is_written() {
        let bad = EXAMPLE
            .replace("dpi_scale: 3.0 fps: 60", "dpi_scale: 7.0 fps: 500")
            .replace("args: [\"--demo\"]", "args: [\"--seed=4\"]\n    env: {MAKEPAD_MUTE: \"0\" my_var: \"1\"}")
            .replace("cursor: @synthetic", "cursor: @hand\n    colour: 1")
            .replace("theme: \"dark\"", "theme: \"solarized\"");
        let diags = AppRunDoc::load_with(&bad, &known).unwrap_err();
        let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        let has = |s: &str| text.iter().any(|t| t.contains(s));
        assert!(has("the executor sets --seed itself"), "{text:#?}");
        assert!(has("`MAKEPAD_MUTE`: a run may only set MAKEPAD_APP_*"), "{text:#?}");
        assert!(has("`my_var`: a run may only set"), "{text:#?}");
        assert!(has("`output.fps` is a whole number from 1 to 240"), "{text:#?}");
        assert!(has("not a whole window at dpi_scale 7"), "{text:#?}");
        assert!(has("`cursor` is @none or @synthetic"), "{text:#?}");
        assert!(has("`colour` is not an AppRun field"), "{text:#?}");
        assert!(has("`solarized` is not a UI theme"), "{text:#?}");
        // Sorted by position, each at its field.
        assert!(diags.windows(2).all(|w| (w[0].line, w[0].col) <= (w[1].line, w[1].col)));
        let fps = diags.iter().find(|d| d.message.contains("output.fps")).unwrap();
        assert_eq!(fps.line, 8);
    }

    #[test]
    fn an_odd_size_and_a_missing_script_are_refused() {
        let bad = EXAMPLE.replace("width: 1080", "width: 1081");
        let bad = bad[..bad.find("    script:").unwrap()].to_string() + "}\n";
        let diags = AppRunDoc::load_with(&bad, &known).unwrap_err();
        let text: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();
        assert!(text.iter().any(|t| t.contains("`output.width` is an even whole number")), "{text:#?}");
        assert!(text.iter().any(|t| t.contains("`script: fn(app)")), "{text:#?}");
    }

    #[test]
    fn a_file_that_is_not_an_app_run_says_so() {
        let diags = AppRunDoc::load_with("use mod.apprun.*\n{label: \"x\"}\n", &known).unwrap_err();
        assert!(diags.iter().any(|d| d.message.contains("one `AppRun{...}`")), "{diags:#?}");
        let diags = AppRunDoc::load_with("use mod.apprun.*\nAppRun{label: nope}\n", &known).unwrap_err();
        assert!(diags.iter().any(|d| d.line == 2 && d.field.as_deref() == Some("label")), "{diags:#?}");
    }

    #[test]
    fn a_syntax_error_in_the_script_is_a_load_error() {
        let bad = EXAMPLE.replace("app.hold(0.5)", "app.hold(0.5))");
        let diags = AppRunDoc::load_with(&bad, &known).unwrap_err();
        assert!(diags.iter().any(|d| d.line == 14), "{diags:#?}");
        let bad = EXAMPLE.replace("app.wait_text(@toast,", "app.wait_text(@,");
        let diags = AppRunDoc::load_with(&bad, &known).unwrap_err();
        assert!(diags.iter().any(|d| d.line == 19), "{diags:#?}");
    }

    #[test]
    fn a_keyword_is_a_fine_segment_of_a_widget_path() {
        let fine = EXAMPLE.replace("app.wait_text(@toast,", "app.wait_text(@toast.ok,");
        assert!(AppRunDoc::load_with(&fine, &known).is_ok());
    }

    #[test]
    fn widget_paths_and_points_evaluate() {
        let mut host = ScriptVmHost::new((), ());
        let mut vm = ScriptVm {
            host: host.host_context(),
            bx: Box::new(ScriptVmBase::new()),
        };
        let (value, errors) = eval_run(&mut vm, "use mod.apprun.*\nAppRun{a: @main.new_note.x b: vec2(1, 2.5) c: rect(1, 2, 3, 4) d: @KeyS}", "t");
        assert!(errors.is_empty(), "{errors:?}");
        let v = read(&vm, value.unwrap());
        assert_eq!(v.field("a"), Some(&Sv::Id("main.new_note.x".into())));
        assert_eq!(v.field("b"), Some(&Sv::Arr(vec![Sv::Num(1.0), Sv::Num(2.5)])));
        assert_eq!(v.field("c").map(|c| matches!(c, Sv::Arr(a) if a.len() == 4)), Some(true));
        assert_eq!(v.field("d"), Some(&Sv::Id("KeyS".into())));
    }
}
