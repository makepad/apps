//! Grab a web video (YouTube, an X post, any page with a video) through
//! Stage's web grab, from the command line.
//!
//! ```text
//! makepad-stage-web-grab <url> [--in T] [--out T] [--every S] [--width W] [--dir DIR] [--launch SCRATCH]
//! ```
//!
//! It drives the `stage_cut.web_grab_*` tools through Stage's `app` service,
//! or with `--launch SCRATCH` starts its own hidden, muted Stage whose
//! home, browser profile, drafts and asset store
//! all live under SCRATCH, and quits it at the end. Stage picks the method:
//! it records the page's video frame-exactly, or, where the page's video
//! cannot play in Stage's browser (H.264-only sites such as X), takes the
//! media file behind the page (the page's own network) and transcodes it.
//!
//! The clip lands in the library as a `Video` asset with its provenance
//! (address, title, range, capture date). With `--every S` it also writes
//! frames every S seconds plus scene changes and a contact sheet; `--dir`
//! copies the clip (`grab.mp4`), its provenance and transcript, and the
//! frames there. Progress goes to stderr; the last grab status (JSON) to
//! stdout.

use makepad_ai_services::wire::AppLine;
use makepad_remote::Client;
use makepad_strict_json::{self as json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut launched = None;
    let mut code = match run(&args, &mut launched) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("web grab: {error}");
            1
        }
    };
    if let Some(stage) = launched {
        if let Err(error) = quit(stage) {
            eprintln!("web grab: {error}");
            code = 1;
        }
    }
    std::process::exit(code);
}

const USAGE: &str = "usage: makepad-stage-web-grab <url> [--in T] [--out T] [--every S] [--width W] [--dir DIR] [--launch SCRATCH]";

struct Launched {
    child: Child,
}

fn call(target: &str, name: &str, arguments: Value) -> Result<Value, String> {
    let client = Client::connect(target).map_err(|error| error.to_string())?;
    let mut result = None;
    let mut error = None;
    let failed = makepad_remote::app::run(&client, &["call".into(), name.into(), arguments.to_json()], &mut |line| {
        match AppLine::parse(line) {
            Some(AppLine::Result(reply)) => result = Some(reply),
            Some(AppLine::Progress { note, .. }) => eprintln!("web grab: {note}"),
            Some(_) => {},
            None => error = Some(line.to_string()),
        }
        true
    }).map_err(|error| error.to_string())?;
    if failed || error.is_some() {
        return Err(error.unwrap_or_else(|| "Stage refused the call".into()));
    }
    let result = result.ok_or("Stage ended the call without a result")?;
    if !result.outcome.is_ok() {
        return Err(result.text);
    }
    json::parse(result.text.as_bytes()).map_err(|error| format!("Stage's result is not JSON: {error}"))
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn number(value: &Value, key: &str) -> Option<f64> {
    match value.get(key)? {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

fn run(args: &[String], launched: &mut Option<Launched>) -> Result<(), String> {
    let mut url = None;
    let (mut from, mut to, mut every, mut width, mut dir, mut scratch) = (None, None, None, None, None, None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().cloned().ok_or(format!("{arg} needs a value\n{USAGE}"));
        match arg.as_str() {
            "--in" => from = Some(value()?),
            "--out" => to = Some(value()?),
            "--every" => every = Some(value()?.parse::<f64>().map_err(|_| "--every takes seconds")?),
            "--width" => width = Some(value()?.parse::<u32>().map_err(|_| "--width takes pixels")?),
            "--dir" => dir = Some(PathBuf::from(value()?)),
            "--launch" => scratch = Some(PathBuf::from(value()?)),
            "-h" | "--help" => return Err(USAGE.into()),
            other if url.is_none() && !other.starts_with("--") => url = Some(other.to_string()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    let url = url.ok_or(USAGE)?;
    if let Some(scratch) = &scratch {
        launch(scratch, launched)?;
    }
    let target = launched.as_ref().map(|stage| stage.child.id().to_string()).unwrap_or_else(|| "makepad-stage".into());
    // Start.
    let mut start = vec![("url", json::s(url.as_str()))];
    if let Some(from) = &from {
        start.push(("in", json::s(from.as_str())));
    }
    if let Some(to) = &to {
        start.push(("out", json::s(to.as_str())));
    }
    // A Stage just started is still installing its browser graph: ask again.
    let arguments = json::obj(start);
    let deadline = Instant::now() + Duration::from_secs(60);
    let started = loop {
        match call(&target, "stage_cut.web_grab_start", arguments.clone()) {
            Ok(started) => break started,
            Err(error) if Instant::now() < deadline && (error.contains("installing") || error.contains("Starting") || error.contains("Loading") || error.contains("not running")) => {
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(error) => return Err(error),
        }
    };
    let job = started.get("job").and_then(Value::as_i64).ok_or("Stage did not answer with a job")?;
    eprintln!("web grab: job {job} for {url}");
    // Follow it to the end.
    let mut last_line = String::new();
    let status = loop {
        std::thread::sleep(Duration::from_secs(2));
        let status = call(&target, "stage_cut.web_grab_status", json::obj(vec![("job", Value::Int(job))]))?;
        let state = text(&status, "state");
        let line = format!(
            "{state} {:.1}/{} s {}",
            number(&status, "recorded_s").unwrap_or(0.0),
            number(&status, "target_s").map_or("?".to_string(), |t| format!("{t:.1}")),
            text(&status, "note")
        );
        if line != last_line {
            eprintln!("web grab: {line}");
            last_line = line;
        }
        match state.as_str() {
            "done" => break status,
            "failed" | "cancelled" => return Err(format!("{state}: {}", text(&status, "note"))),
            _ => {}
        }
    };
    let asset = status.get("asset").cloned().unwrap_or(Value::Null);
    eprintln!("web grab: in the library as {} {}", text(&asset, "asset_id"), text(&asset, "revision"));
    let grab_dir = PathBuf::from(text(&status, "dir"));
    // Frames.
    let mut frames_dir = None;
    if let Some(every) = every {
        let mut arguments = vec![("job", Value::Int(job)), ("every_s", Value::F64(every)), ("summary", Value::Bool(true))];
        if let Some(width) = width {
            arguments.push(("width", Value::Int(width as i64)));
        }
        let frames = call(&target, "stage_cut.web_grab_frames", json::obj(arguments))?;
        let count = frames.get("frame_count").and_then(Value::as_i64).unwrap_or(0);
        let sheet = text(&frames, "sheet");
        eprintln!("web grab: {count} frames, contact sheet {sheet}");
        frames_dir = Path::new(&sheet).parent().map(Path::to_path_buf);
    }
    // Copies.
    if let Some(dir) = &dir {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        for name in ["grab.mp4", "provenance.txt", "transcript.txt"] {
            let from = grab_dir.join(name);
            if from.exists() {
                std::fs::copy(&from, dir.join(name)).map_err(|error| format!("{}: {error}", from.display()))?;
            }
        }
        if let Some(frames) = &frames_dir {
            let target = dir.join("frames");
            std::fs::create_dir_all(&target).map_err(|error| format!("{}: {error}", target.display()))?;
            for entry in std::fs::read_dir(frames).map_err(|error| error.to_string())?.flatten() {
                let _ = std::fs::copy(entry.path(), target.join(entry.file_name()));
            }
        }
        eprintln!("web grab: copied to {}", dir.display());
    }
    println!("{}", status.to_json());
    Ok(())
}

/// Start a hidden, muted Stage with everything under `scratch` and wait
/// until its app service answers. Keep the child for cleanup if startup fails.
fn launch(scratch: &Path, launched: &mut Option<Launched>) -> Result<(), String> {
    let scratch = std::fs::canonicalize(scratch).or_else(|_| {
        std::fs::create_dir_all(scratch).and_then(|_| std::fs::canonicalize(scratch))
    }).map_err(|error| format!("{}: {error}", scratch.display()))?;
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let stage = exe.with_file_name(format!("makepad-stage{}", std::env::consts::EXE_SUFFIX));
    if !stage.exists() {
        return Err(format!("{} is not built (cargo build --release -p makepad-stage)", stage.display()));
    }
    let home = scratch.join("home");
    let log = scratch.join("stage.log");
    let log_file = std::fs::File::create(&log).map_err(|error| format!("{}: {error}", log.display()))?;
    let child = Command::new(&stage)
        .arg("--remote")
        .env("MAKEPAD_HOME", &home)
        .env("MAKEPAD_CEF_PROFILE_DIR", scratch.join("cef"))
        .env("STAGE_ASSET_ROOT", scratch.join("store"))
        .env("STAGE_EDIT_DRAFTS", scratch.join("drafts"))
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .env("MAKEPAD_MUTE", "1")
        .stdout(Stdio::from(log_file.try_clone().map_err(|error| error.to_string())?))
        .stderr(Stdio::from(log_file))
        .spawn()
        .map_err(|error| format!("starting {}: {error}", stage.display()))?;
    let pid = child.id().to_string();
    *launched = Some(Launched { child });
    let stage = launched.as_mut().unwrap();
    eprintln!("web grab: started a hidden Stage (pid {pid}), log {}", log.display());
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if call(&pid, "stage_page.stage_status", Value::Obj(Vec::new())).is_ok() {
            return Ok(());
        }
        if let Ok(Some(status)) = stage.child.try_wait() {
            return Err(format!("the hidden Stage exited ({status}); see {}", log.display()));
        }
        if Instant::now() > deadline {
            return Err(format!("the hidden Stage's app service did not come up; see {}", log.display()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Quit the launched Stage gracefully (`quit`), then make sure it is gone.
fn quit(mut stage: Launched) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut requested = false;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = stage.child.try_wait() {
            eprintln!("web grab: the hidden Stage quit");
            return Ok(());
        }
        if !requested {
            if let Ok(client) = Client::connect(&stage.child.id().to_string()) {
                requested = matches!(makepad_remote::app::run(&client, &["quit".to_string()], &mut |_| true), Ok(false));
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("the hidden Stage (pid {}) did not quit in time", stage.child.id()))
}
