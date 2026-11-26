//! App runs against `examples/counter` (registry id `counter`), hidden, on
//! the virtual clock. One test function: the lane laws allow one app
//! instance at a time, and cargo runs a binary's tests in parallel.

use makepad_story_app_run::*;
use std::path::PathBuf;

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("makepad-app-run-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run_file(script: &str) -> String {
    format!(
        "use mod.apprun.*\n\nAppRun{{\n    label: \"Counter\"\n    app: \"counter\"\n    output: {{width: 1080 height: 1920 dpi_scale: 3 fps: 60}}\n    seed: 1\n    cursor: @synthetic\n    script: fn(app) {{\n{script}\n    }}\n}}\n"
    )
}

const THREE_CLICKS: &str = "        app.click(@increment_button)
        app.click(@body.increment_button)
        app.click(@increment_button)
        app.wait_text(@counter_label, \"Count: 3\")
        app.mark(@clicked)
        app.hold(1.0)";

fn execute_in(source: &str, dir: &str, build: bool, validate: bool) -> RunResult {
    let mut opts = RunOptions::new(work_dir(dir));
    opts.build = build;
    opts.validate = validate;
    execute(source, opts).unwrap_or_else(|e| panic!("{dir}: {e}"))
}

#[test]
fn counter_runs_are_exact_deterministic_and_fail_on_unknown_targets() {
    // 9:16 at 1080x1920@3: three clicks by path, a mark, a one-second hold.
    let source = run_file(THREE_CLICKS);
    let first = execute_in(&source, "first", true, false);
    assert!(first.ok(), "{:?}", first.failures);
    // Each click: 21 frames of pointer travel (0.35 s), a 3-frame press,
    // one frame after release. The second and third press on the same spot
    // wait 6 more frames, so presses are 31 frames apart (past the
    // platform's 0.5 s multi-click window: three clicks, not a triple
    // click). The label already reads 3: no wait. 60 held.
    assert_eq!(first.frames, 25 + 2 * (21 + 6 + 3 + 1) + 60);
    assert_eq!(first.marks(), vec![("clicked".to_string(), 87)]);
    assert_eq!(first.record.get("capture").and_then(|c| c.get("frames")).and_then(|f| f.as_u64()), Some(first.frames));
    // The window was checked to be exactly the output in pixels (a
    // mismatch refuses the run); the recording is that size.
    assert_eq!(first.output, RunOutput { width: 1080, height: 1920, dpi_scale: 3.0, fps: 60 });
    let focus = first.record.get("focus").unwrap();
    let events = focus.get("events").and_then(|e| e.as_arr()).unwrap();
    assert!(events.iter().filter(|e| e.get("source").and_then(|s| s.as_str()) == Some("click")).count() == 3);
    assert!(!focus.get("frames").and_then(|f| f.as_arr()).unwrap().is_empty());
    let calls = first.record.get("calls").and_then(|c| c.as_arr()).unwrap();
    assert_eq!(calls.len(), 6);
    assert!(first.mp4.is_file() && first.run_json.is_file());
    assert_eq!(first.frame_hashes.len() as u64, first.frames);
    // What bound overlays read: three presses, the pointer path, the
    // button's rect over time.
    let bind = first.record.get("bind").unwrap();
    let presses = bind.get("pointer").and_then(|p| p.get("presses")).and_then(|p| p.as_arr()).unwrap();
    assert_eq!(presses.len(), 3);
    assert_eq!(presses[0].get("frames").unwrap().to_json(), "[21,24]");
    assert!(bind.get("pointer").and_then(|p| p.get("path")).and_then(|p| p.as_arr()).unwrap().len() > 20);
    assert!(bind.get("widgets").and_then(|w| w.get("body.increment_button#0")).is_some());

    // The same script, build and output: the same frames.
    let second = execute_in(&source, "second", false, false);
    assert!(second.ok(), "{:?}", second.failures);
    assert_eq!(second.frame_hashes, first.frame_hashes);
    assert_eq!(second.hash, first.hash);
    // Same binary. (The cache key also hashes the trees' uncommitted
    // changes, which other work in this checkout moves between runs.)
    assert_eq!(second.build.binary_sha256, first.build.binary_sha256);
    assert!(first.build.built && !second.build.built);

    // Validate mode: 5 fps, the targets resolved against the launch dump,
    // the marks where the full run has them.
    let check = execute_in(&source, "validate", false, true);
    assert!(check.ok(), "{:?}", check.failures);
    assert_eq!(check.output.fps, VALIDATE_FPS);
    assert!(check.mp4.ends_with("validate.mp4"));
    assert!(compare_marks(&first.record, &check.record).is_empty(), "{:?}", compare_marks(&first.record, &check.record));

    // A target that matches nothing fails the run, names candidates, and
    // nothing after it runs.
    let failing = execute_in(&run_file("        app.click(@increment)\n        app.hold(1.0)"), "failing", false, false);
    assert!(!failing.ok());
    assert!(failing.failures[0].contains("@increment matches no visible widget"), "{:?}", failing.failures);
    assert!(failing.failures[0].contains("@body.increment_button"), "{:?}", failing.failures);
    assert_eq!(failing.frames, 0);
    let recorded = failing.record.get("failures").and_then(|f| f.as_arr()).unwrap();
    assert_eq!(recorded.len(), 1);

    // Validate mode warns that the path is not in the launch dump (it
    // could still appear during the run), then its 5 fps run fails the
    // same way.
    let failing = execute_in(&run_file("        app.click(@increment)"), "failing-validate", false, true);
    assert!(!failing.ok());
    assert!(failing.warnings.iter().any(|w| w.contains("@increment: not in the launch dump")), "{:?}", failing.warnings);

    // cut() / resume(): the second click happens while cut. Both holds are
    // longer than the press ripple, so the frames before the cut all show
    // "Count: 1" and the frames after it "Count: 2", and nothing of the
    // cut section (the pointer travel, the press) is in the file.
    let cut = execute_in(
        &run_file(
            "        app.click(@increment_button)
        app.hold(0.6)
        app.mark(@before)
        app.cut()
        app.click(@increment_button)
        app.hold(0.6)
        app.resume()
        app.mark(@after)
        app.hold(0.3)
        app.drop(@increment_button, \"x.txt\")",
        ),
        "cut",
        false,
        false,
    );
    assert!(!cut.ok(), "the drop of a file missing from the drop root must fail");
    assert!(cut.failures[0].contains("drop x.txt"), "{:?}", cut.failures);
    let clicked = 25 + 36;
    let cut_len = 21 + 3 + 1 + 36;
    assert_eq!(cut.frames, clicked + 18);
    assert_eq!(cut.record.get("capture").and_then(|c| c.get("frames")).and_then(|f| f.as_u64()), Some(cut.frames));
    assert_eq!(cut.frame_hashes.len() as u64, cut.frames);
    assert_eq!(cut.marks(), vec![("before".to_string(), clicked), ("after".to_string(), clicked)]);
    let (at, h) = (clicked as usize, &cut.frame_hashes);
    assert!(h[at - 10..at].iter().all(|x| *x == h[at - 1]), "the frames before the cut settled");
    assert!(h[at..].iter().all(|x| *x == h[at]), "the frames after the resume are one state");
    assert_ne!(h[at - 1], h[at], "the count changed inside the cut");
    let segments = cut.record.get("segments").unwrap().to_json();
    assert_eq!(segments, format!("[[0,{clicked}],[{},{}]]", clicked + cut_len, clicked + cut_len + 18));
    // Focus frames stay inside the file and the cut click has no frames.
    let focus = cut.record.get("focus").unwrap();
    for run in focus.get("frames").and_then(|f| f.as_arr()).unwrap() {
        assert!(run.get("to").and_then(|t| t.as_u64()).unwrap() <= cut.frames);
    }
    let clicks: Vec<String> = focus
        .get("events")
        .and_then(|e| e.as_arr())
        .unwrap()
        .iter()
        .filter(|e| e.get("source").and_then(|s| s.as_str()) == Some("click"))
        .map(|e| e.get("frames").unwrap().to_json())
        .collect();
    assert_eq!(clicks, vec!["[0,55]".to_string()], "the click inside the cut is not in the track");

    // A file in the drop root drops (its hash recorded); a resize keeps
    // the aspect and centres the smaller window in the recording.
    let dir = work_dir("drop");
    std::fs::create_dir_all(dir.join("files")).unwrap();
    std::fs::write(dir.join("files/x.txt"), b"hello").unwrap();
    let mut opts = RunOptions::new(&dir);
    opts.build = false;
    let dropped = execute(
        &run_file(
            "        app.drop(@increment_button, \"x.txt\")
        app.resize(180, 320)
        app.click(@increment_button)",
        ),
        opts,
    )
    .unwrap();
    assert!(dropped.ok(), "{:?}", dropped.failures);
    let calls = dropped.record.get("calls").unwrap().to_json();
    assert!(calls.contains("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"), "{calls}");
    // After the resize the window is 540x960 px centred in the 1080x1920
    // recording, at (270, 480): the click's rect lies inside it.
    let events = dropped.record.get("focus").unwrap().get("events").and_then(|e| e.as_arr()).unwrap();
    let click = events.iter().find(|e| e.get("source").and_then(|s| s.as_str()) == Some("click")).unwrap();
    let rect: Vec<f64> = click.get("rect").and_then(|r| r.as_arr()).unwrap().iter().map(|v| v.to_json().parse().unwrap()).collect();
    assert!(rect[0] >= 270.0 && rect[1] >= 480.0 && rect[0] + rect[2] <= 810.0, "{rect:?}");
    let mut opts = RunOptions::new(work_dir("drop-outside"));
    opts.build = false;
    let outside = execute(&run_file("        app.drop(@increment_button, \"/etc/hosts\")"), opts).unwrap();
    assert!(outside.failures[0].contains("only files under the run's drop root"), "{:?}", outside.failures);

    // A run longer than its max_duration, and a cancelled run, stop
    // cleanly: the app quits, the partial recording is kept for review.
    let long = execute_in(&run_file("        app.hold(2)").replace("seed: 1", "seed: 1\n    max_duration: 1"), "long", false, false);
    assert!(long.failures[0].contains("longer than its max_duration (1 s)"), "{:?}", long.failures);
    assert!(long.frames <= 60);
    let mut opts = RunOptions::new(work_dir("cancel"));
    opts.build = false;
    let cancel = opts.cancel.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let cancelled = execute(&run_file("        app.hold(300)"), opts).unwrap();
    stopper.join().unwrap();
    assert!(cancelled.failures[0].contains("cancelled"), "{:?}", cancelled.failures);
}
