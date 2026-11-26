//! The hidden app never outlives the executor: the CLI is killed with
//! SIGTERM, then with SIGKILL, in the middle of a long hold, and the app it
//! launched must be gone within 15 s each time. One test function (one app
//! instance at a time). It builds the app first, as an app run does, so it
//! needs no other test's build output.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RUN: &str = "use mod.apprun.*

AppRun{
    label: \"Counter: a long hold\"
    app: \"counter\"
    output: {width: 720 height: 1280 dpi_scale: 2 fps: 30}
    script: fn(app) {
        app.hold(120)
    }
}
";

fn alive(pid: u32) -> bool {
    Command::new("kill").arg("-0").arg(pid.to_string()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// The app's stderr startup line is included in the run's combined app.log.
fn app_pid(log: &Path, deadline: Instant) -> u32 {
    loop {
        if let Ok(text) = std::fs::read_to_string(log) {
            if let Some((pid, _, _)) = text.lines().find_map(makepad_remote::parse_startup_line) {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "the app never started");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn kill_mid_run(signal: &str, dir: PathBuf) {
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let run = dir.join("hold.splash");
    std::fs::write(&run, RUN).unwrap();
    let mut cli = Command::new(env!("CARGO_BIN_EXE_makepad-app-run"))
        .arg(&run)
        .arg("--out")
        .arg(dir.join("out"))
        .arg("--no-build")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = app_pid(&dir.join("out/app.log"), Instant::now() + Duration::from_secs(90));
    // Let it get into the hold.
    std::thread::sleep(Duration::from_secs(2));
    assert!(alive(pid), "the app should be running mid-hold");
    Command::new("kill").arg(format!("-{signal}")).arg(cli.id().to_string()).status().unwrap();
    let _ = cli.wait();
    let end = Instant::now() + Duration::from_secs(15);
    while alive(pid) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    let left = alive(pid);
    if left {
        // Do not leave it running for the next test.
        let _ = Command::new("kill").arg("-KILL").arg(pid.to_string()).status();
    }
    assert!(!left, "app pid {pid} outlived the executor killed with SIG{signal}");
}

#[test]
fn the_app_dies_with_the_executor() {
    let base = std::env::temp_dir().join(format!("makepad-app-run-signal-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    // The registry's build, as a run without --no-build makes it: the kills
    // below run the CLI with --no-build, so no build runs in their timed window.
    makepad_story_app_run::exec::build_app("counter", &base.join("build.log")).unwrap_or_else(|e| panic!("building the counter app: {e}"));
    kill_mid_run("TERM", base.join("term"));
    kill_mid_run("KILL", base.join("kill"));
}
