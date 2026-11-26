//! The AI authoring blob: an app's registry entry, its
//! widget paths at the target output (from a hidden launch) and the script
//! API, as one text.

use crate::doc::{AppRunDoc, RunOutput};
use crate::exec::build_app;
use crate::identity::locate;
use crate::plan::frames_for;
use crate::session::{LaunchSpec, Session};
use std::path::Path;

/// The script API, as the host implements it.
pub const API_TABLE: &str = "\
Script API

`script: fn(app) {...}` gets the app. Every call advances the virtual clock in whole
frames and never retries input; the first failure ends the run. A target is a widget
path `@a.b` (it must match exactly one visible widget: exactly, or as a suffix of whole
segments; write a path with a Splash keyword segment as a string, \"dialog.ok\") or
`vec2(x, y)` in window points (allowed, flagged in review). Times are seconds.

| Call | Meaning |
|---|---|
| app.click(target, {button: @left|@right|@middle, count: 1..3, move_time: 0.35}) | move the cursor there over move_time, press and release (a press on the spot of the last one waits out the 0.5 s multi-click window: separate calls are never a double click; ask with count) |
| app.drag(from, to, {time: 0.5, move_time: 0.35}) | move to from, down, move to to over time, up |
| app.scroll(target, dx, dy, {time: 0.3, move_time: 0.35}) | move there, wheel dx/dy spread over time |
| app.type(target, text, {cps: 12, move_time: 0.35}) | click target, then type at cps characters per second (\\n is Return, \\t Tab) |
| app.key(@KeyS, {cmd, shift, alt, ctrl}) | key down, 3 frames, key up (codes: KeyA.., Key0.., ReturnKey, Escape, Tab, Backspace, ArrowLeft..) |
| app.drop(target, \"file\", {move_time}) | move there and drop a file from the run's drop root (a relative path; nothing outside it) |
| app.resize(width, height) | window size in points; only the output's aspect, at most the output size |
| app.hold(seconds) | advance the clock, recording |
| app.wait_settled({max: 5}) | step until nothing is pending (animation, redraw, timer) |
| app.wait_text(target, \"text\", {max: 5}) | step until the widget shows the text; fails at max |
| app.wait_visible(target, {max: 5}) | step until the widget is visible; fails at max |
| app.wait_state(target, {enabled, checked, selected, value, max: 5}) | step until the widget is in every named state (value: exact text; selected: true, false or the item); fails at max |
| app.mark(@name) | name the frame the next step shows (edits cut on marks; each name once) |
| app.cut() / app.resume() | stop / restart recording; the clock runs on |
| app.focus(target, {pad: 0, weight: 1, hold: 1}) | where the viewer should look (reframing) |
| app.focus_rect(rect(x, y, w, h), {pad, weight, hold}) | the same for a region in window points |
| app.wide({hold: 1}) | show the whole window |
";

/// The run file's shape, for the blob.
pub const RUN_SHAPE: &str = "\
use mod.apprun.*

AppRun{
    label: \"...\"
    app: \"<registry id>\"
    args: []                       // the app's own arguments
    output: {width: W height: H dpi_scale: S fps: 60}   // pixels; W/S and H/S whole
    theme: \"default\"               // or a widget style: macos, macos-dark, ios, ...
    seed: 1
    audio: false
    env: {}                        // only MAKEPAD_APP_* variables
    max_duration: 600              // seconds of clock the run may take
    cursor: @synthetic             // @none | @synthetic
    script: fn(app) {
        app.wait_settled()
        ...
    }
}
";

/// Launch `app` hidden at `output` (building it first when `build`), let it
/// settle, and return the registry entry, the widget-path dump and the API
/// as one text. `work_dir` receives the app's log.
pub fn app_dump(app: &str, output: RunOutput, build: bool, work_dir: &Path) -> Result<String, String> {
    dump(app, output, Vec::new(), Vec::new(), None, build, work_dir)
}

/// `app_dump` for a run file: its app at its output, with its `args`,
/// `theme` and `env`, so the dump shows the layout the run will see.
pub fn app_dump_doc(doc: &AppRunDoc, build: bool, work_dir: &Path) -> Result<String, String> {
    dump(&doc.app, doc.output, doc.args.clone(), doc.env.clone(), doc.theme.clone(), build, work_dir)
}

fn dump(app: &str, output: RunOutput, run_args: Vec<String>, env: Vec<(String, String)>, theme: Option<String>, build: bool, work_dir: &Path) -> Result<String, String> {
    let (entry, root, binary) = locate(app)?;
    std::fs::create_dir_all(work_dir).map_err(|e| format!("{}: {e}", work_dir.display()))?;
    let work_dir = work_dir.canonicalize().map_err(|e| e.to_string())?;
    if build {
        build_app(app, &work_dir.join("build.log"))?;
    }
    let mut args: Vec<String> = entry.args.iter().map(|a| a.to_string()).collect();
    args.extend(run_args);
    let spec = LaunchSpec {
        binary,
        cwd: root,
        args,
        env: crate::exec::isolated_env(&env, &work_dir)?,
        output,
        seed: 1,
        audio: false,
        theme,
        log_path: work_dir.join("dump-app.log"),
    };
    let mut session = Session::launch(&spec)?;
    let px = session.window_px()?;
    if px != (output.width, output.height) {
        return Err(format!("refused: the window is {}x{} px, not the {}x{} asked for", px.0, px.1, output.width, output.height));
    }
    // Let the first frames lay out and draw (text is measured when drawn;
    // bounded: an app that animates forever is dumped as it is after 5 s).
    session.step(2)?;
    for _ in 0..frames_for(5.0, output.fps) {
        if session.settled()?.0 {
            break;
        }
        session.step(1)?;
    }
    let rows = session.ui_nodes("")?;
    let dump = rows.iter().filter_map(|row| row.path.as_ref().map(|path| {
        let [x, y, w, h] = row.rect;
        format!("{path} {} {x} {y} {w} {h}", row.ty)
    })).collect::<Vec<_>>().join("\n");
    // The text each widget shows, for wait_text.
    let texts: Vec<(String, String)> = rows
        .into_iter()
        .filter_map(|row| Some((row.path?, row.text?)))
        .filter(|(p, t)| !p.is_empty() && !t.is_empty())
        .collect();
    session.quit()?;
    let (w, h) = output.logical();
    let mut out = String::new();
    out.push_str(&format!("# App `{}`: {}\n\n", entry.id, entry.label));
    out.push_str(&format!(
        "registry: package {}, dir {}, binary {}, default args [{}]\n",
        entry.package,
        entry.dir,
        entry.bin,
        entry.args.join(" ")
    ));
    out.push_str(&format!("output: {}x{} px at dpi_scale {} = a {}x{} point window, {} fps\n\n", output.width, output.height, output.dpi_scale, w, h, output.fps));
    out.push_str("## Semantic paths at this size (`path type x y w h`, window points)\n\n");
    out.push_str(dump.trim_end());
    if !texts.is_empty() {
        out.push_str("\n\n## Visible text (`path: text`)\n\n");
        for (path, text) in texts {
            out.push_str(&format!("{path}: {text:?}\n"));
        }
    }
    out.push_str("\n\n## ");
    out.push_str(API_TABLE);
    out.push_str("\n## Run file\n\n");
    out.push_str(RUN_SHAPE);
    Ok(out)
}
