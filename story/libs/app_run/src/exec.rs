//! The executor: build the app, launch it hidden at
//! the run's output, record the script, write `run.mp4` and `run.json`.
//! Validate mode runs the same script at 5 fps after resolving its targets
//! against the launch dump, and publishes nothing.

use crate::doc::{eval_run, AppRunDoc, CursorMode, RunOutput};
use crate::call::Target;
use crate::host::{app_object, Script};
use crate::runner::RunLimits;
use crate::identity::{cache_key, locate, BuildIdentity};
use crate::plan::path_matches;
use crate::session::{LaunchSpec, Session};
use crate::track::num;
use makepad_script::*;
use makepad_strict_json::{self as json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Validate mode's frame rate.
pub const VALIDATE_FPS: u32 = 5;
pub const RUN_JSON_FORMAT: &str = "makepad-app-run/1";

pub struct RunOptions {
    /// Where run.mp4, run.json, app.log and build.log go (created).
    pub out_dir: PathBuf,
    /// Run the registry's build first (off: use the binary as it is).
    pub build: bool,
    /// Validate mode: resolve targets against the launch dump, run at
    /// `VALIDATE_FPS`.
    pub validate: bool,
    /// Grab the last frame as a PNG (the thumbnail a publish needs), also
    /// written as thumbnail.png.
    pub thumbnail: bool,
    /// Progress lines (build, launch, calls).
    pub progress: Box<dyn FnMut(&str) + Send>,
    /// Set from anywhere to stop the run between chunks of frames: it
    /// fails as "cancelled", with the capture stopped and the app quit.
    pub cancel: Arc<AtomicBool>,
    /// The only directory `app.drop` may take files from (canonicalized,
    /// symlinks followed). None: `<out_dir>/files`, created empty, for the
    /// caller to fill with the run's pinned assets.
    pub drop_root: Option<PathBuf>,
}

impl RunOptions {
    pub fn new(out_dir: impl Into<PathBuf>) -> RunOptions {
        RunOptions {
            out_dir: out_dir.into(),
            build: true,
            validate: false,
            thumbnail: false,
            progress: Box::new(|_| {}),
            cancel: Arc::new(AtomicBool::new(false)),
            drop_root: None,
        }
    }
}

pub struct RunResult {
    /// run.json as written.
    pub record: Value,
    pub mp4: PathBuf,
    pub run_json: PathBuf,
    /// The last frame, PNG, when asked for.
    pub thumbnail: Option<Vec<u8>>,
    pub build: BuildIdentity,
    pub output: RunOutput,
    pub frames: u64,
    /// The capture's hash of all raw frames and one per frame.
    pub hash: String,
    pub frame_hashes: Vec<String>,
    /// Failures (the run is not publishable when there are any).
    pub failures: Vec<String>,
    /// Review notes: coordinates used, validate-mode target warnings.
    pub warnings: Vec<String>,
}

impl RunResult {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }

    /// `name → output frame`.
    pub fn marks(&self) -> Vec<(String, u64)> {
        match self.record.get("marks") {
            Some(Value::Obj(m)) => m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).collect(),
            _ => Vec::new(),
        }
    }
}

/// Run the registry build of `app`, output to `log`.
pub fn build_app(app: &str, log: &Path) -> Result<(), String> {
    let entry = makepad_app_registry::find(app).ok_or_else(|| format!("`{app}` is not a registry app id"))?;
    // the launchers' exact cargo command and environment: what this builds, the WM, Code and
    // DECK find fresh (and the reverse)
    let mut command = makepad_app_registry::build_command(entry)?;
    let program = PathBuf::from(command.get_program());
    let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let file = std::fs::File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = file.try_clone().map_err(|e| e.to_string())?;
    let status = command
        .stdout(file)
        .stderr(err)
        .status()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if status.success() {
        return Ok(());
    }
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().filter(|l| l.starts_with("error") || l.contains("-->")).take(20).collect();
    Err(format!("{} {} failed ({status}); see {}\n{}", program.display(), args.join(" "), log.display(), tail.join("\n")))
}

/// The outputs a run writes into its directory; a new run removes the
/// previous ones first, so a directory never pairs files of two runs.
const OUTPUTS: &[&str] = &["run.mp4", "run.part.mp4", "run.json", "validate.mp4", "validate.part.mp4", "validate.json", "thumbnail.png"];

/// Execute a run file. Errors before the script (bad file, build, launch,
/// a pixel-size mismatch) are `Err`, with a run.json holding the failure
/// when the app was already launched; a script that fails is an `Ok`
/// result with `failures`, its partial recording and run.json kept for
/// review. The recording is written as `*.part.mp4` and renamed when the
/// capture finished.
pub fn execute(source: &str, mut opts: RunOptions) -> Result<RunResult, String> {
    let doc = AppRunDoc::load(source).map_err(|diags| {
        let lines: Vec<String> = diags.iter().map(|d| format!("run.splash:{d}")).collect();
        lines.join("\n")
    })?;
    std::fs::create_dir_all(&opts.out_dir).map_err(|e| format!("{}: {e}", opts.out_dir.display()))?;
    let out_dir = opts.out_dir.canonicalize().map_err(|e| e.to_string())?;
    for name in OUTPUTS {
        let _ = std::fs::remove_file(out_dir.join(name));
    }
    let (entry, root, binary) = locate(&doc.app)?;
    if opts.build {
        (opts.progress)(&format!("building {} ({})", entry.package, entry.bin));
        build_app(&doc.app, &out_dir.join("build.log"))?;
    }
    let build = BuildIdentity::current(entry, &root, &binary, opts.build)?;
    let mut output = doc.output;
    if opts.validate {
        output.fps = VALIDATE_FPS;
    }
    // The registry's own arguments first, then the run's.
    let mut args: Vec<String> = entry.args.iter().map(|a| a.to_string()).collect();
    args.extend(doc.args.iter().cloned());
    let spec = LaunchSpec {
        binary,
        cwd: root.clone(),
        args,
        env: isolated_env(&doc.env, &out_dir)?,
        output,
        seed: doc.seed,
        audio: doc.audio,
        theme: doc.theme.clone(),
        log_path: out_dir.join("app.log"),
    };
    let stem = if opts.validate { "validate" } else { "run" };
    let run_json = out_dir.join(format!("{stem}.json"));
    (opts.progress)(&format!("launching {} {}", spec.binary.display(), spec.argv().join(" ")));
    let session = Session::launch(&spec)?;
    let drop_root = match &opts.drop_root {
        Some(root) => root.clone(),
        None => {
            let files = out_dir.join("files");
            std::fs::create_dir_all(&files).map_err(|e| format!("{}: {e}", files.display()))?;
            files
        }
    };
    let limits = RunLimits { cancel: opts.cancel.clone(), max_frames: (doc.max_duration * output.fps as f64).ceil() as u64 };
    let recorded = record_run(&doc, &mut opts, session, output, limits, drop_root, &out_dir, stem);
    let (mut script, mut warnings) = match recorded {
        Ok(done) => done,
        Err(e) => {
            // The app is gone (its session dropped); leave a run.json that
            // says why, next to at most a `.part` recording.
            let record = json::obj(vec![
                ("format", json::s(RUN_JSON_FORMAT)),
                ("label", json::s(&doc.label)),
                ("app", json::obj(vec![("id", json::s(entry.id)), ("package", json::s(entry.package)), ("bin", json::s(entry.bin))])),
                ("build", build.to_json()),
                ("output", output_json(&output)),
                ("validate", Value::Bool(opts.validate)),
                ("failures", Value::Arr(vec![json::obj(vec![("call", json::s("executor")), ("frame", Value::Int(0)), ("message", json::s(&e))])])),
            ]);
            let _ = std::fs::write(&run_json, record.to_json());
            return Err(e);
        }
    };
    let (hash, frame_hashes, captured, thumbnail) = std::mem::take(&mut script.runner.finished);
    let mp4 = out_dir.join(format!("{stem}.mp4"));
    let part = out_dir.join(format!("{stem}.part.mp4"));
    if part.exists() || captured > 0 {
        std::fs::rename(&part, &mp4).map_err(|e| format!("{}: {e}", part.display()))?;
    }
    if let Some(png) = &thumbnail {
        let path = out_dir.join("thumbnail.png");
        std::fs::write(&path, png).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    warnings.extend(script.runner.warnings.iter().cloned());
    for c in script.runner.track.calls.iter().filter(|c| c.coordinates) {
        warnings.push(format!("{} (frame {}) uses window coordinates, vec2(x, y), instead of a widget path", c.call, script.runner.track.out_frame(c.from)));
    }
    let failures: Vec<String> = script.runner.track.failures.iter().map(|f| format!("{}: {}", f.call, f.message)).collect();
    let written = script.runner.track.frames_written();
    let key = cache_key(&unpublished_revision(source), &build, &output);
    let mut record = json::obj(vec![
        ("format", json::s(RUN_JSON_FORMAT)),
        ("label", json::s(&doc.label)),
        ("app", json::obj(vec![("id", json::s(entry.id)), ("package", json::s(entry.package)), ("bin", json::s(entry.bin))])),
        ("build", build.to_json()),
        ("output", output_json(&output)),
        ("platform", json::obj(vec![("os", json::s(std::env::consts::OS)), ("arch", json::s(std::env::consts::ARCH))])),
        ("seed", Value::Int(doc.seed as i64)),
        ("validate", Value::Bool(opts.validate)),
        ("run_revision", json::s("")),
        ("cache_key", json::s(&key)),
        ("frames", Value::Int(written as i64)),
        ("duration", num(written as f64 / output.fps as f64)),
        ("marks", script.runner.track.marks_json()),
        ("calls", script.runner.track.calls_json()),
        ("segments", script.runner.track.segments_json()),
        ("focus", script.runner.track.focus_json()),
        ("bind", script.runner.track.bind_json()),
        ("failures", script.runner.track.failures_json()),
        ("capture", json::obj(vec![("hash", json::s(&hash)), ("frames", Value::Int(captured as i64))])),
    ]);
    // A validation next to a full run of the same directory is
    // compared with it (marks on the script's schedule).
    let sibling = out_dir.join(if opts.validate { "run.json" } else { "validate.json" });
    if let Some(other) = std::fs::read(&sibling).ok().and_then(|b| json::parse_depth(&b, 32).ok()) {
        for d in compare_marks(&record, &other) {
            warnings.push(format!("marks differ from {}: {d}", sibling.display()));
        }
    }
    if let Value::Obj(fields) = &mut record {
        fields.push(("warnings".into(), Value::Arr(warnings.iter().map(json::s).collect())));
    }
    std::fs::write(&run_json, record.to_json()).map_err(|e| format!("{}: {e}", run_json.display()))?;
    Ok(RunResult { record, mp4, run_json, thumbnail, build, output, frames: written, hash, frame_hashes, failures, warnings })
}

/// The run revision of a script that is not published yet: its text's
/// hash, so two unpublished scripts never share a cache key.
pub fn unpublished_revision(source: &str) -> String {
    format!("source:{}", makepad_core_util::sha256::sha256_hex(source.as_bytes()))
}

/// From launch to quit: px check, validate's target check, cursor,
/// capture, the script, cap/stop, thumbnail. Returns the script (with the
/// capture's result in `finished`) and the warnings so far.
#[allow(clippy::too_many_arguments)]
fn record_run(
    doc: &AppRunDoc,
    opts: &mut RunOptions,
    session: Session,
    output: RunOutput,
    limits: RunLimits,
    drop_root: PathBuf,
    out_dir: &Path,
    stem: &str,
) -> Result<(Script, Vec<String>), String> {
    // Exact pixels or nothing.
    let px = session.window_px()?;
    if px != (output.width, output.height) {
        return Err(format!(
            "refused: the window is {}x{} px, the run declares {}x{} (--window {}); nothing is scaled behind the author's back",
            px.0,
            px.1,
            output.width,
            output.height,
            output.window_arg()
        ));
    }
    let mut warnings = Vec::new();
    if opts.validate {
        let nodes = session.ui_nodes("")?;
        warnings.extend(check_targets(doc, &nodes)?);
    }
    let mut script = Script::new(Some(session), output, limits, drop_root);
    if doc.cursor == CursorMode::Synthetic {
        script.s().cursor_show(script.runner.cursor())?;
    }
    script.s().cap_start(&out_dir.join(format!("{stem}.part.mp4")), doc.audio)?;
    script.runner.snapshot_layout(script.session.as_mut().unwrap())?;
    (opts.progress)("running the script");
    let mut script = run_script(doc, script)?;
    let session = script.session.as_ref().unwrap();
    let stop = session.cap_stop();
    let (hash, frame_hashes, captured) = match &stop {
        Ok(v) => (
            v.get("hash").and_then(Value::as_str).unwrap_or("").to_string(),
            v.get("frame_hashes")
                .and_then(Value::as_arr)
                .map(|h| h.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_json())).collect())
                .unwrap_or_default(),
            v.get("frames").and_then(Value::as_u64).unwrap_or(0),
        ),
        // A run that failed before its first frame has no recording.
        Err(e) if script.runner.track.frames_written() == 0 && e.contains("no frame was captured") => (String::new(), Vec::new(), 0),
        Err(e) => return Err(format!("cap/stop: {e}")),
    };
    let written = script.runner.track.frames_written();
    if captured != written {
        script.runner.fail("cap/stop", format!("the capture wrote {captured} frames, the run stepped {written}"));
    }
    if let Ok(v) = &stop {
        if v.get("missing").and_then(Value::as_u64).unwrap_or(0) > 0 {
            script.runner.fail("cap/stop", format!("the capture is missing frames: {}", v.to_json()));
        }
    }
    let thumbnail = if opts.thumbnail {
        match script.s().grab_png(4096) {
            Ok(png) => Some(png),
            Err(e) => {
                warnings.push(format!("no thumbnail: {e}"));
                None
            }
        }
    } else {
        None
    };
    if let Err(e) = script.session.as_mut().unwrap().quit() {
        warnings.push(e);
    }
    script.runner.finished = (hash, frame_hashes, captured, thumbnail);
    Ok((script, warnings))
}

/// The run's environment plus `MAKEPAD_HOME` (Makepad's per-user state:
/// storage, settings) in a fresh `home/` of the run's own directory: a run
/// starts from the same state every time and never reads or writes the
/// user's. A run file cannot name another (`doc::ENV_PREFIX`).
pub(crate) fn isolated_env(env: &[(String, String)], dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut env: Vec<(String, String)> = env.iter().filter(|(k, _)| k != "MAKEPAD_HOME").cloned().collect();
    // `dir` is the canonical run directory (never empty): the scratch home
    // is always a child of it.
    let home = dir.join("home");
    if home.parent() != Some(dir) || !dir.is_absolute() {
        return Err(format!("refusing a scratch home outside the run directory: {}", home.display()));
    }
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    env.push(("MAKEPAD_HOME".into(), home.to_string_lossy().into_owned()));
    Ok(env)
}

pub(crate) fn output_json(o: &RunOutput) -> Value {
    json::obj(vec![
        ("width", Value::Int(o.width as i64)),
        ("height", Value::Int(o.height as i64)),
        ("dpi_scale", num(o.dpi_scale)),
        ("fps", Value::Int(o.fps as i64)),
    ])
}

/// Evaluate the run file in a VM whose host is `script` and call its
/// script with the app. Returns the script with what its runner recorded.
fn run_script(doc: &AppRunDoc, script: Script) -> Result<Script, String> {
    let mut host = ScriptVmHost::new(script, ());
    {
        let mut vm = ScriptVm {
            host: host.host_context(),
            bx: Box::new(ScriptVmBase::new()),
        };
        let (value, errors) = eval_run(&mut vm, &doc.source, "run.splash");
        let root = match value {
            Some(root) if errors.is_empty() => root,
            _ => return Err(format!("run.splash does not evaluate: {}", errors.join("\n"))),
        };
        let script = vm.bx.heap_ref().value(root.as_object().unwrap(), id!(script).into(), NoTrap);
        let app = app_object(&mut vm);
        vm.capture_errors();
        // The script's own instructions only; the natives' waiting is not
        // counted. A script that loops forever still ends.
        vm.with_instruction_limit(50_000_000, |vm| vm.call(script, &[app]));
        let errors = vm.take_errors();
        if !errors.is_empty() {
            let script = vm.host.get_mut::<Script>().unwrap();
            script.runner.fail("script", errors.join("\n"));
        }
    }
    Ok(host.host)
}

/// Validate mode's target check: run the script dry (arguments checked,
/// nothing sent) and resolve every path it names against the launch dump.
/// A path that matches nothing or several widgets at launch is a warning:
/// the call may run once the layout has changed; the run itself decides.
pub fn check_targets(doc: &AppRunDoc, rows: &[crate::driver::Row]) -> Result<Vec<String>, String> {
    let dry = run_script(doc, Script::new(None, doc.output, RunLimits::default(), PathBuf::new()))?;
    if let Some(f) = dry.runner.track.failures.first() {
        return Err(format!("{}: {}", f.call, f.message));
    }
    let mut warnings = Vec::new();
    let mut seen = Vec::new();
    for (call, target) in &dry.runner.targets {
        let Target::Path(path) = target else { continue };
        if seen.contains(path) {
            continue;
        }
        seen.push(path.clone());
        let hits: Vec<&str> = rows.iter().filter_map(|r| r.path.as_deref()).filter(|p| path_matches(path, p)).collect();
        match hits.len() {
            1 => {}
            0 => warnings.push(format!("{call} @{path}: not in the launch dump (it has to appear during the run)")),
            _ => warnings.push(format!("{call} @{path} matches {} widgets at launch: {} (it has to be one when the call runs)", hits.len(), hits.join(", "))),
        }
    }
    Ok(warnings)
}

/// How two runs of one script differ in their marks: the determinism
/// check validate mode makes against a full run. A wait lasts as
/// many frames as the app needs to settle or show something, so at 5 fps it
/// is longer than at 60, and so is the spacing that keeps two presses on
/// one spot from becoming a double click; marks are therefore compared on
/// the script's own schedule, their time minus the frames waited before
/// them (calls' `waited`), within a frame of each run. Also reported: marks only one run has, and a different order.
pub fn compare_marks(a: &Value, b: &Value) -> Vec<String> {
    let (ma, mb) = (scheduled_marks(a), scheduled_marks(b));
    let (fa, fb) = (fps_of(a), fps_of(b));
    let tolerance = 1.0 / fa + 1.0 / fb + 1e-9;
    let mut out = Vec::new();
    for (name, t) in &ma {
        match mb.iter().find(|(n, _)| n == name) {
            Some((_, other)) if (t - other).abs() <= tolerance => {}
            Some((_, other)) => out.push(format!("mark {name}: {t:.3} s vs {other:.3} s on the script's schedule (waits excluded)")),
            None => out.push(format!("mark {name}: only in the first run")),
        }
    }
    for (name, _) in mb.iter().filter(|(n, _)| !ma.iter().any(|(m, _)| m == n)) {
        out.push(format!("mark {name}: only in the second run"));
    }
    let order = |m: &[(String, f64)], other: &[(String, f64)]| -> Vec<String> {
        m.iter().filter(|(n, _)| other.iter().any(|(o, _)| o == n)).map(|(n, _)| n.clone()).collect()
    };
    if order(&ma, &mb) != order(&mb, &ma) {
        out.push("the marks come in a different order".into());
    }
    out
}

fn fps_of(v: &Value) -> f64 {
    v.get("output").and_then(|o| o.get("fps")).and_then(Value::as_u64).unwrap_or(60) as f64
}

/// Each mark's time in seconds minus the frames the run spent waiting
/// before it (each call's `waited`: `wait_*` calls, multi-click spacing),
/// in frame order.
fn scheduled_marks(v: &Value) -> Vec<(String, f64)> {
    let fps = fps_of(v);
    let waits: Vec<(u64, u64)> = v
        .get("calls")
        .and_then(Value::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(|c| {
            let f = c.get("frames")?.as_arr()?;
            Some((f.get(1)?.as_u64()?, c.get("waited").and_then(Value::as_u64).unwrap_or(0)))
        })
        .collect();
    let mut marks: Vec<(String, f64)> = match v.get("marks") {
        Some(Value::Obj(m)) => m
            .iter()
            .filter_map(|(name, f)| {
                let frame = f.as_u64()?;
                let waited: u64 = waits.iter().filter(|w| w.0 <= frame).map(|w| w.1).sum();
                Some((name.clone(), frame.saturating_sub(waited) as f64 / fps))
            })
            .collect(),
        _ => Vec::new(),
    };
    marks.sort_by(|a, b| a.1.total_cmp(&b.1));
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_compare_on_the_schedule_without_the_waits() {
        let run = |fps: i64, wait: (i64, i64), marks: Vec<(&str, i64)>| {
            let call = json::obj(vec![
                ("call", json::s("wait_settled")),
                ("frames", Value::Arr(vec![Value::Int(wait.0), Value::Int(wait.1)])),
                ("waited", Value::Int(wait.1 - wait.0)),
            ]);
            json::obj(vec![
                ("output", json::obj(vec![("fps", Value::Int(fps))])),
                ("calls", Value::Arr(vec![call])),
                ("marks", Value::Obj(marks.into_iter().map(|(k, v)| (k.to_string(), Value::Int(v))).collect())),
            ])
        };
        // 60 fps: settled in 14 frames, marks at 2 s and 5 s of script. At
        // 5 fps the wait took 3 frames.
        let full = run(60, (0, 14), vec![("saved", 134), ("end", 314)]);
        assert!(compare_marks(&full, &run(5, (0, 3), vec![("saved", 13), ("end", 28)])).is_empty());
        let off = compare_marks(&full, &run(5, (0, 3), vec![("saved", 16), ("late", 1)]));
        assert_eq!(off.len(), 3, "{off:?}");
        let swapped = compare_marks(&full, &run(5, (0, 3), vec![("saved", 28), ("end", 13)]));
        assert!(swapped.iter().any(|d| d.contains("different order")), "{swapped:?}");
    }

    #[test]
    fn a_dry_run_checks_arguments_and_resolves_targets_against_the_dump() {
        let source = |body: &str| format!("use mod.apprun.*\nAppRun{{label: \"t\" app: \"counter\" output: {{width: 1080 height: 1920 dpi_scale: 3 fps: 60}} script: fn(app) {{ {body} }}}}\n");
        let rows: Vec<_> = ["main_window", "body.increment_button", "body.a.label", "body.b.label"].iter()
            .map(|path| crate::driver::Row::new(Some(path.to_string()), "", "", [0.0, 0.0, 1.0, 1.0])).collect();
        let doc = AppRunDoc::load(&source("app.click(@increment_button) app.wait_visible(@dialog.confirm) app.focus(\"dialog.ok\") app.hold(1)")).unwrap();
        let warnings = check_targets(&doc, &rows).unwrap();
        // A path segment that is a Splash keyword (`.ok`) is written as a string.
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("@dialog.confirm") && warnings[1].contains("@dialog.ok"));
        let doc = AppRunDoc::load(&source("app.click(@label)")).unwrap();
        assert!(check_targets(&doc, &rows).unwrap()[0].contains("matches 2 widgets"));
        let doc = AppRunDoc::load(&source("app.click(@increment_button, {cout: 2})")).unwrap();
        assert!(check_targets(&doc, &rows).unwrap_err().contains("unknown option `cout`"));
        let doc = AppRunDoc::load(&source("app.type(@increment_button, 5)")).unwrap();
        assert!(check_targets(&doc, &rows).unwrap_err().contains("text is a string"));
        let doc = AppRunDoc::load(&source("app.resize(100, 100)")).unwrap();
        assert!(check_targets(&doc, &rows).unwrap_err().contains("keep its aspect"));
        let doc = AppRunDoc::load(&source("app.clack(@increment_button)")).unwrap();
        assert!(check_targets(&doc, &rows).is_err());
    }
}
