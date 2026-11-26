//! Runs the packaged page in Node. The embedded harness is shipped with the
//! exporter binary, so installation does not require a checkout or npm.
use crate::{Failure, Options, Phase, Progress};
use std::process::{Command, Stdio};
use std::time::Duration;

const FILES: &[(&str, &str)] = &[
    ("loader-audit.js", include_str!("../nodegate/loader-audit.js")),
    ("dom.js", include_str!("../nodegate/dom.js")),
    ("input.js", include_str!("../nodegate/input.js")),
    ("gl.js", include_str!("../nodegate/gl.js")),
    ("audio.js", include_str!("../nodegate/audio.js")),
    ("frames.js", include_str!("../nodegate/frames.js")),
];

pub fn run(opts: &Options, progress: &mut dyn Progress) -> Result<String, Failure> {
    let fail = |e: String| Failure::new(Phase::Nodegate, e);
    match Command::new("node").arg("--version").output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && opts.allow_missing_node => {
            let note = "SKIPPED: Node is absent (--allow-missing-node); page correctness unverified";
            progress.note(Phase::Nodegate, note);
            return Ok(note.into());
        }
        Err(e) => return Err(fail(format!("Node is required for the final page gate: {e}; --allow-missing-node only permits an absent Node"))),
        Ok(out) if !out.status.success() => return Err(fail("node --version failed".into())),
        Ok(_) => {}
    }
    let dir = opts.build().join("nodegate");
    std::fs::create_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
    for (name, text) in FILES {
        std::fs::write(dir.join(name), text).map_err(|e| fail(e.to_string()))?;
    }
    let mut failures = Vec::new();
    let mut summaries = Vec::new();
    let count = opts.drivers.len() + opts.replays.len();
    for (i, (driver, pass)) in opts.drivers.iter().map(|d| (d, "training")).chain(opts.replays.iter().map(|d| (d, "gate"))).enumerate() {
        if progress.cancelled() { return Err(fail("cancelled".into())); }
        progress.phase(Phase::Nodegate, i as f64 / count as f64, &driver.name);
        let output = dir.join(format!("{}.json", driver.name));
        if output.exists() { std::fs::remove_file(&output).map_err(|e| fail(e.to_string()))?; }
        let log = dir.join(format!("{}.log", driver.name));
        let file = std::fs::File::create(&log).map_err(|e| fail(e.to_string()))?;
        let mut command = Command::new("node");
        command.arg(dir.join("loader-audit.js")).arg(&opts.out_dir)
            .arg(opts.build().join(pass).join(format!("{}.json", driver.name))).arg(&output);
        if let Some(script) = &driver.script { command.arg(opts.package_dir.join(script)); }
        let mut child = command.stdin(Stdio::null()).stdout(file.try_clone().map_err(|e| fail(e.to_string()))?).stderr(file)
            .spawn().map_err(|e| fail(e.to_string()))?;
        let status = loop {
            if progress.cancelled() {
                let _ = child.kill(); let _ = child.wait();
                return Err(fail("cancelled".into()));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => { let _ = child.kill(); let _ = child.wait(); return Err(fail(e.to_string())); }
            }
        };
        let summary = std::fs::read_to_string(&log).unwrap_or_default();
        progress.note(Phase::Nodegate, &format!("{}: {}", driver.name, summary.trim()));
        let passed = std::fs::read(&output).ok()
            .and_then(|b| makepad_strict_json::parse_depth(&b, 64).ok())
            .is_some_and(|j| j.get("status").and_then(makepad_strict_json::Value::as_str) == Some("PASS"));
        if !status.success() || !passed { failures.push(format!("{}: {}; missing or failed result: {}", driver.name, summary.trim(), output.display())); }
        summaries.push(format!("{}: {}", driver.name, summary.trim()));
    }
    std::fs::write(dir.join("summary.txt"), summaries.join("\n")).map_err(|e| fail(e.to_string()))?;
    if !failures.is_empty() {
        return Err(fail(failures.join("\n")).details(&dir.join("summary.txt")));
    }
    progress.phase(Phase::Nodegate, 1.0, "all packaged frames equal");
    Ok(format!("PASS: {count} drivers; all frames equal"))
}
