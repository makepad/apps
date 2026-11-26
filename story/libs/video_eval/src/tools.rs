//! Child processes: the generator and `makepad-edit-tools`. Each runs with
//! a deadline, its output captured to files beside the case, and is killed
//! when the deadline passes.

use makepad_micro_serde::{DeJson, JsonValue};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Outcome {
    /// `None` when killed at the deadline or it could not report a code.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub seconds: f64,
    pub timed_out: bool,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// A one-line reason for a failed run: its last stderr line.
    pub fn reason(&self) -> String {
        if self.timed_out {
            return format!("timed out after {:.0} s", self.seconds);
        }
        // A `--json` command says why in its report.
        let report = JsonValue::deserialize_json_lenient(self.stdout.trim()).ok();
        if let Some(error) = report.as_ref().and_then(|report| report.get("error")).and_then(|error| error.as_str()) {
            return format!("exit {:?}: {error}", self.code);
        }
        let last = self.stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        format!("exit {:?}: {}", self.code, last.trim())
    }
}

/// Run `argv` in `cwd`, capturing output; kill it after `timeout`. When
/// `log` is given, stdout and stderr are also written to `<log>.out` and
/// `<log>.err`.
pub fn run(argv: &[String], cwd: &Path, timeout: Duration, log: Option<&Path>) -> Result<Outcome, String> {
    let (exe, args) = argv.split_first().ok_or("empty command")?;
    let started = Instant::now();
    let mut child = Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{exe}: {e}"))?;
    let drain = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                timed_out = true;
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{exe}: {e}")),
        }
    };
    let outcome = Outcome {
        code: if timed_out { None } else { status.and_then(|s| s.code()) },
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        seconds: started.elapsed().as_secs_f64(),
        timed_out,
    };
    if let Some(log) = log {
        let _ = std::fs::write(log.with_extension("out"), &outcome.stdout);
        let _ = std::fs::write(log.with_extension("err"), &outcome.stderr);
    }
    Ok(outcome)
}

/// `makepad-edit-tools`, invoked as a CLI (its `--json` output is the
/// stable interface for agents; linking it would tie this harness to the
/// renderer's build).
pub struct CutTools {
    pub exe: PathBuf,
    pub timeout: Duration,
}

impl CutTools {
    fn call(&self, args: &[&str], cwd: &Path, log: &Path) -> Result<Outcome, String> {
        let mut argv = vec![self.exe.display().to_string()];
        argv.extend(args.iter().map(|a| a.to_string()));
        run(&argv, cwd, self.timeout, Some(log))
    }

    /// `inspect FILE --json`. The report is on stdout; a non-zero exit
    /// with a report means "has errors", which the report counts.
    pub fn inspect(&self, file: &Path, dir: &Path) -> Result<(Outcome, Option<String>), String> {
        let o = self.call(&["inspect", &file.display().to_string(), "--json"], dir, &dir.join("inspect.log"))?;
        let json = o.stdout.trim_start().starts_with('{').then(|| o.stdout.clone());
        Ok((o, json))
    }

    /// `strip FILE --count N --json`: the report (with the frames'
    /// exposure) is the outcome's stdout.
    pub fn strip(&self, file: &Path, count: u32, png: &Path, dir: &Path) -> Result<Outcome, String> {
        self.call(
            &["strip", &file.display().to_string(), "--count", &count.to_string(), "--out", &png.display().to_string(), "--json"],
            dir,
            &dir.join("strip.log"),
        )
    }

    /// A strip of `count` frames between `from` and `to`, `columns` per row.
    #[allow(clippy::too_many_arguments)]
    pub fn strip_range(&self, file: &Path, from: f64, to: f64, count: u32, columns: u32, png: &Path, dir: &Path) -> Result<Outcome, String> {
        self.call(
            &[
                "strip",
                &file.display().to_string(),
                "--from",
                &format!("{from:.3}"),
                "--to",
                &format!("{to:.3}"),
                "--count",
                &count.to_string(),
                "--columns",
                &columns.to_string(),
                "--out",
                &png.display().to_string(),
            ],
            dir,
            &dir.join("strip-range.log"),
        )
    }

    pub fn frame(&self, file: &Path, t: f64, width: u32, png: &Path, dir: &Path) -> Result<Outcome, String> {
        self.call(
            &[
                "frame",
                &file.display().to_string(),
                "--at",
                &format!("{t:.3}"),
                "--width",
                &width.to_string(),
                "--out",
                &png.display().to_string(),
            ],
            dir,
            &dir.join(format!("frame-{t:.2}.log")),
        )
    }

    pub fn onion(&self, file: &Path, from: f64, to: f64, steps: u32, png: &Path, dir: &Path) -> Result<Outcome, String> {
        self.call(
            &[
                "onion",
                &file.display().to_string(),
                "--from",
                &format!("{from:.3}"),
                "--to",
                &format!("{to:.3}"),
                "--steps",
                &steps.to_string(),
                "--out",
                &png.display().to_string(),
            ],
            dir,
            &dir.join("onion.log"),
        )
    }

    /// `render FILE --draft --out MP4`; the outcome's `seconds` is the
    /// render time.
    pub fn render_draft(&self, file: &Path, mp4: &Path, dir: &Path) -> Result<Outcome, String> {
        self.call(&["render", &file.display().to_string(), "--draft", "--out", &mp4.display().to_string()], dir, &dir.join("render.log"))
    }
}

/// The nearest ancestor of `from` holding both Makepad checkout markers.
pub fn repo_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| dir.join("makepad.splash").is_file() && dir.join("platform/Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// A binary built in the checkout's release target.
pub fn release_bin(root: &Path, name: &str) -> PathBuf {
    root.join("target/release").join(name)
}

/// An isolated copy of an asset library for one evaluation run, so
/// generations never land in the person's library and the run's hidden
/// Stage never hosts the person's store.
///
/// The store is cloned copy-on-write (`cp -c`, APFS clonefile: no data is
/// copied until a side writes), its SQLite databases with their WAL and
/// shared-memory files together and then checked, and the library's
/// `index.json`/`fleet_prefs.json` beside it. The store's identity and
/// hosting files (`server-id`, `listen`, `server.lock`, `admin-token`) are
/// left out: the copy mints its own identity, so it is never mistaken for
/// the person's store on the LAN.
pub fn clone_library(src: &Path, dst: &Path) -> Result<(), String> {
    let store = src.join("store");
    if !store.join("catalog.sqlite3").is_file() {
        return Err(format!("{} has no store/catalog.sqlite3", src.display()));
    }
    let dst_store = dst.join("store");
    std::fs::create_dir_all(&dst_store).map_err(|e| format!("{}: {e}", dst_store.display()))?;
    let cp = |from: &Path, to: &Path| -> Result<(), String> {
        let o = Command::new("cp").arg("-c").arg("-R").arg(from).arg(to).output().map_err(|e| format!("cp: {e}"))?;
        if o.status.success() {
            Ok(())
        } else {
            Err(format!("cp -c {}: {}", from.display(), String::from_utf8_lossy(&o.stderr).trim()))
        }
    };
    for dir in ["cas", "chat"] {
        if store.join(dir).exists() {
            cp(&store.join(dir), &dst_store.join(dir))?;
        }
    }
    for db in ["catalog.sqlite3", "transport.sqlite3"] {
        if !store.join(db).is_file() {
            continue;
        }
        // The live store may write between the three copies; a torn copy
        // fails the check, and one more attempt usually lands clean.
        let mut ok = false;
        for _ in 0..3 {
            for ext in ["", "-wal", "-shm"] {
                let f = store.join(format!("{db}{ext}"));
                let t = dst_store.join(format!("{db}{ext}"));
                let _ = std::fs::remove_file(&t);
                if f.is_file() {
                    cp(&f, &t)?;
                }
            }
            let check = Command::new("sqlite3").arg(dst_store.join(db)).arg("PRAGMA quick_check;").output();
            if check.is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "ok") {
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(format!("{db}: the copy does not pass quick_check"));
        }
    }
    for f in ["index.json", "fleet_prefs.json"] {
        if src.join(f).is_file() {
            cp(&src.join(f), &dst.join(f))?;
        }
    }
    Ok(())
}
