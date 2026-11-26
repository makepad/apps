//! One hidden app instance on the virtual clock:
//! launched with the run's pixel size, scale and seed, driven through the
//! remote commands, always shut down when the session is dropped.

use crate::doc::RunOutput;
use crate::driver::{Driver, Mods, Mouse, Row};
use crate::remote::{num, parse_startup, Remote};
use makepad_strict_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How to start the app.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    /// The app's own arguments (the run's `args`).
    pub args: Vec<String>,
    /// The run's `env`.
    pub env: Vec<(String, String)>,
    pub output: RunOutput,
    pub seed: u64,
    pub audio: bool,
    pub theme: Option<String>,
    /// The child's stdout and stderr go here too.
    pub log_path: PathBuf,
}

impl LaunchSpec {
    /// The command line after the binary: ours first, then the app's.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            "--remote".to_string(),
            "--virtual-clock".to_string(),
            format!("--window={}", self.output.window_arg()),
            format!("--seed={}", self.seed),
            // No "[remote]" in the window title the frames show.
            "--remote-title-tag=off".to_string(),
        ];
        argv.extend(self.args.iter().cloned());
        argv
    }
}

pub struct Session {
    child: Child,
    /// Kills the app if this process dies without unwinding (see
    /// `spawn_watchdog`); stopped on a normal quit.
    watchdog: Option<Child>,
    pub remote: Remote,
    log: Arc<Mutex<String>>,
    pub output: RunOutput,
    closed: bool,
}

/// The parent environment a hidden app gets: what a GUI process needs to
/// find its user, fonts and temp dir. Everything else is dropped, so
/// neither the host's nor a run file's `DYLD_*`, `LD_*`, locale or tool
/// variables reach the app.
const PASSED_ENV: &[&str] = &["PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "SYSTEMROOT", "WINDIR", "APPDATA", "LOCALAPPDATA", "USERPROFILE", "XDG_RUNTIME_DIR", "DISPLAY", "WAYLAND_DISPLAY"];
/// Pinned so formatted text in the frames does not follow the host.
const PINNED_ENV: &[(&str, &str)] = &[("TZ", "UTC"), ("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.UTF-8")];
/// The child's log kept in memory for error messages (the file has all).
const LOG_TAIL_BYTES: usize = 64 * 1024;

/// A process that ends the app when this one is gone: on Unix a `sh` loop
/// that polls both pids and sends SIGTERM (the platform finishes an open
/// capture on it), then SIGKILL, once the executor has died without
/// quitting the app — killed by a signal, SIGKILL included, or crashed.
/// The pids are numbers formatted into the script, nothing else.
#[cfg(unix)]
fn spawn_watchdog(app: u32) -> Option<Child> {
    let parent = std::process::id();
    let script = format!(
        "while kill -0 {parent} 2>/dev/null && kill -0 {app} 2>/dev/null; do sleep 0.3; done; \
         if kill -0 {app} 2>/dev/null; then kill -TERM {app}; n=0; \
         while kill -0 {app} 2>/dev/null && [ $n -lt 50 ]; do sleep 0.1; n=$((n+1)); done; \
         kill -0 {app} 2>/dev/null && kill -KILL {app}; fi"
    );
    Command::new("/bin/sh").arg("-c").arg(script).env_clear().stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()
}

#[cfg(not(unix))]
fn spawn_watchdog(_app: u32) -> Option<Child> {
    None
}

/// The sheet `theme` names, as `StyleSheet` JSON in the run's scratch home
/// (`MAKEPAD_HOME`): the app installs it whether or not it registered the
/// sheet itself (the extra style sheets are registered in this runner).
fn write_sheet(spec: &LaunchSpec, theme: &str) -> Result<PathBuf, String> {
    let home = spec.env.iter().find(|(k, _)| k == "MAKEPAD_HOME").map(|(_, v)| PathBuf::from(v)).ok_or("no scratch home for the theme")?;
    let sheet = makepad_widgets_core::desktop_style::StyleSheet::named(theme).ok_or_else(|| format!("`{theme}` is not a UI theme"))?;
    let path = home.join("widget-style.json");
    std::fs::write(&path, sheet.to_json()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// How long an ordinary command may take before the child counts as hung.
const SHORT: Duration = Duration::from_secs(20);

/// A command as the words and `k=v` arguments of its argv.
fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}
pub use crate::driver::NO_PATHS;

impl Session {
    /// Start the app hidden and wait until its first window is up.
    pub fn launch(spec: &LaunchSpec) -> Result<Session, String> {
        if !spec.binary.is_file() {
            return Err(format!("no binary at {}", spec.binary.display()));
        }
        let mut command = Command::new(&spec.binary);
        command.current_dir(&spec.cwd).args(spec.argv()).env_clear();
        for key in PASSED_ENV {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .envs(PINNED_ENV.iter().copied())
            .env("MAKEPAD_HIDE_WINDOWS", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !spec.audio {
            command.env("MAKEPAD_MUTE", "1");
        }
        if let Some(theme) = &spec.theme {
            command.env("MAKEPAD_WIDGET_STYLE_FILE", write_sheet(spec, theme)?);
        }
        command.envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        if let Some(dir) = spec.log_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = Arc::new(Mutex::new(std::fs::File::create(&spec.log_path).map_err(|e| format!("{}: {e}", spec.log_path.display()))?));
        let mut child = command.spawn().map_err(|e| format!("start {}: {e}", spec.binary.display()))?;
        let log = Arc::new(Mutex::new(String::new()));
        let stdout = child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>);
        let stderr = child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>);
        let pid = child.id();
        let (started, socket) = std::sync::mpsc::channel();
        for (pipe, mut started) in [(stdout, None), (stderr, Some(started))] {
            let Some(pipe) = pipe else { continue };
            let (log, file) = (log.clone(), file.clone());
            // One reader per pipe for the child's lifetime; it ends at EOF.
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = writeln!(file.lock().unwrap(), "{line}");
                    if let Some(tx) = &started {
                        if let Some(path) = parse_startup(&line, pid) {
                            let _ = tx.send(path);
                            started = None;
                        }
                    }
                    let mut log = log.lock().unwrap();
                    log.push_str(&line);
                    log.push('\n');
                    if log.len() > 2 * LOG_TAIL_BYTES {
                        let mut cut = log.len() - LOG_TAIL_BYTES;
                        while !log.is_char_boundary(cut) {
                            cut += 1;
                        }
                        log.drain(..cut);
                    }
                }
            });
        }
        let watchdog = spawn_watchdog(child.id());
        let mut session = Session { child, watchdog, remote: Remote::new(), log, output: spec.output, closed: false };
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Ok(path) = socket.try_recv() {
                session.remote.connect(&path)?;
                break;
            }
            if !session.alive() {
                return Err(format!("{} exited before its remote came up:\n{}", spec.binary.display(), session.log_tail(20)));
            }
            if Instant::now() >= deadline {
                return Err("the app's remote did not come up within 90 s".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        loop {
            if let Ok(status) = session.remote.json(&words(&["status"]), false, SHORT) {
                if status.get("w").and_then(Value::as_arr).is_some_and(|w| !w.is_empty()) {
                    break;
                }
            }
            if !session.alive() {
                return Err(format!("the app exited before its window opened:\n{}", session.log_tail(20)));
            }
            if Instant::now() >= deadline {
                return Err("the app's window did not open within 90 s".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let activity = session.remote.json(&words(&["activity"]), false, SHORT)?;
        session.remote.user_seq = Some(activity.get("user_seq").and_then(Value::as_u64).ok_or("activity has no user_seq")?);
        Ok(session)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn log_tail(&self, lines: usize) -> String {
        let log = self.log.lock().unwrap();
        let all: Vec<&str> = log.lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }

    fn get(&self, argv: &[String]) -> Result<Value, String> {
        self.remote.json(argv, false, SHORT)
    }

    fn input(&self, argv: &[String]) -> Result<Value, String> {
        self.remote.json(argv, true, SHORT)
    }

    /// The first window's pixel size, from `/s`.
    /// The main window's (id 0, else the first listed) pixel size, from
    /// `/s`; a fractional size is an error, never rounded.
    pub fn window_px(&self) -> Result<(u32, u32), String> {
        let status = self.get(&words(&["status"]))?;
        let windows = status.get("w").and_then(Value::as_arr).ok_or("status lists no window")?;
        let w = windows.iter().find(|w| w.get("i").and_then(Value::as_u64) == Some(0)).or(windows.first()).ok_or("status lists no window")?;
        let px = w.get("px").and_then(Value::as_arr).ok_or("status has no px")?;
        match (px.first().and_then(num), px.get(1).and_then(num)) {
            (Some(x), Some(y)) if x.fract() == 0.0 && y.fract() == 0.0 && x >= 0.0 && y >= 0.0 => Ok((x as u32, y as u32)),
            (Some(x), Some(y)) => Err(format!("status reports a fractional window of {x}x{y} px")),
            _ => Err("status px is not two numbers".into()),
        }
    }

    /// `/step?frames=k`: answers after the k-th frame is presented (and
    /// captured). Returns the app's frame counter.
    pub fn step(&self, frames: u64) -> Result<u64, String> {
        if frames == 0 {
            return Ok(0);
        }
        // `wait_loads=1`: each frame waits for in-flight image, glyph and
        // resource loads (up to 30 s each), so asynchronous work lands on
        // the same frame at any machine load.
        let v = self.remote.json(&words(&["step", &format!("frames={frames}"), &format!("fps={}", self.output.fps), "wait_loads=1"]), false, Duration::from_secs(60 + 31 * frames))?;
        Ok(v.get("frame").and_then(Value::as_u64).unwrap_or(0))
    }

    pub fn settled(&self) -> Result<(bool, Vec<String>), String> {
        let v = self.get(&words(&["settled"]))?;
        let reasons: Vec<String> = v.get("reasons").and_then(Value::as_arr).map(|r| r.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        // In-flight asynchronous loads (reason `loads`) are not settled.
        let settled = v.get("settled").and_then(Value::as_bool) == Some(true) && !reasons.iter().any(|r| r == "loads");
        Ok((settled, reasons))
    }

    /// Visible semantic nodes in the app's main window, in draw order.
    pub fn ui_nodes(&self, query: &str) -> Result<Vec<Row>, String> {
        rows(&self.get(&words(&["ui", "find", "w=0", "json=1", &format!("q={query} state=visible")]))?)
    }

    /// Show the synthetic cursor at `at` (window points), in the app's own
    /// cursor style (a hand over buttons, a text cursor over text).
    pub fn cursor_show(&self, at: (f64, f64)) -> Result<(), String> {
        self.get(&words(&["cursor", "show=1", "style=app", &format!("x={:.3}", at.0), &format!("y={:.3}", at.1)])).map(|_| ())
    }

    /// `/cap/start`: the directory must exist; a file already there is
    /// replaced (the run owns its output directory).
    pub fn cap_start(&self, path: &Path, audio: bool) -> Result<(), String> {
        let mut argv = words(&["cap", "start", &format!("path={}", path.to_string_lossy()), &format!("fps={}", self.output.fps), "overwrite=1"]);
        if audio {
            argv.push("audio=1".into());
        }
        self.get(&argv).map(|_| ())
    }

    /// `/cap/stop?hashes=1`: waits for the file to be finalized.
    pub fn cap_stop(&self) -> Result<Value, String> {
        self.remote.json(&words(&["cap", "stop", "hashes=1"]), false, Duration::from_secs(660))
    }

    /// `/cap/pause`, `/cap/resume` (cut and resume a recording on one
    /// clock; see the lane report's platform request).
    pub fn cap_pause(&self, pause: bool) -> Result<(), String> {
        let argv = words(&["cap", if pause { "pause" } else { "resume" }]);
        self.get(&argv).map(|_| ()).map_err(|e| format!("cut()/resume() need the remote's cap pause and cap resume, which this app's platform does not answer ({e})"))
    }

    /// `/g`: the next presented frame as PNG bytes, downscaled so no side is
    /// over `max_side`.
    pub fn grab_png(&self, max_side: u32) -> Result<Vec<u8>, String> {
        let (w, h) = self.window_px()?;
        let scale = (max_side as f64 / w.max(h) as f64).min(1.0);
        let v = self.remote.json(&words(&["grab", &format!("scale={scale}")]), false, Duration::from_secs(30))?;
        let path = v.get("png").and_then(Value::as_str).ok_or("grab returned no png path")?;
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("grab did not write a PNG".into());
        }
        Ok(bytes)
    }

    /// Graceful shutdown: `/quit`, then wait for the process; the exact
    /// owned pid is killed only if it does not exit within 10 s.
    pub fn quit(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if !self.alive() {
            return Ok(());
        }
        let asked = self.remote.get(&words(&["quit"]), self.remote.user_seq.is_some(), Duration::from_secs(5));
        let end = Instant::now() + Duration::from_secs(10);
        while self.alive() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.alive() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return Err(format!("pid {} did not exit after quit ({asked:?}); killed", self.child.id()));
        }
        let _ = self.child.wait();
        Ok(())
    }

    fn stop_watchdog(&mut self) {
        if let Some(mut watchdog) = self.watchdog.take() {
            let _ = watchdog.kill();
            let _ = watchdog.wait();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.quit();
        self.stop_watchdog();
    }
}

impl Driver for Session {
    fn snapshot(&mut self) -> Result<Vec<Row>, String> {
        self.ui_nodes("")
    }

    fn find(&mut self, path: &str) -> Result<Vec<Row>, String> {
        self.ui_nodes(&format!("path={}", makepad_a11y::query::quote(path)))
    }

    fn mouse(&mut self, kind: Mouse, x: f64, y: f64) -> Result<(), String> {
        let (name, tail) = match kind {
            Mouse::Move => ("move", Vec::new()),
            Mouse::Down(b) => ("down", vec![format!("b={b}")]),
            Mouse::Up(b) => ("up", vec![format!("b={b}")]),
            Mouse::Scroll(dx, dy) => ("scroll", vec![format!("dx={dx:.4}"), format!("dy={dy:.4}")]),
        };
        let mut argv = words(&["mouse", &format!("k={name}"), &format!("x={x:.3}"), &format!("y={y:.3}")]);
        argv.extend(tail);
        self.input(&argv).map(|_| ())
    }

    fn key(&mut self, down: bool, code: &str, mods: Mods) -> Result<(), String> {
        let kind = if down { "down" } else { "up" };
        let mut argv = words(&["key", &format!("k={kind}"), &format!("c={code}")]);
        argv.extend(mods.remote_params().map(|m| format!("{m}=1")));
        self.input(&argv).map(|_| ())
    }

    fn text(&mut self, text: &str) -> Result<(), String> {
        self.input(&words(&["text", &format!("t={text}")])).map(|_| ())
    }

    fn drop_file(&mut self, path: &Path, x: f64, y: f64) -> Result<(), String> {
        self.input(&words(&["drop", &format!("path={}", path.to_string_lossy()), &format!("x={x:.3}"), &format!("y={y:.3}")])).map(|_| ())
    }

    fn resize(&mut self, w: f64, h: f64) -> Result<(), String> {
        self.remote.json(&words(&["resize", &format!("w={w}"), &format!("h={h}")]), true, SHORT).map(|_| ())
    }

    fn settled(&mut self) -> Result<(bool, Vec<String>), String> {
        Session::settled(self)
    }

    fn advance_frames(&mut self, k: u64) -> Result<(), String> {
        self.step(k).map(|_| ())
    }

    fn capture_pause(&mut self, pause: bool) -> Result<(), String> {
        self.cap_pause(pause)
    }
}

fn rows(v: &Value) -> Result<Vec<Row>, String> {
    let trees = v.get("trees").and_then(Value::as_arr).ok_or("ui reply has no trees")?;
    let mut rows = Vec::new();
    for tree in trees {
        let hex = tree.get("wire").and_then(Value::as_str).ok_or("ui tree has no wire payload")?;
        if !hex.is_ascii() || hex.len() % 2 != 0 {
            return Err("invalid semantic wire hex".into());
        }
        let bytes = (0..hex.len()).step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| "invalid semantic wire hex".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let update = makepad_a11y::wire::decode_update(&bytes).map_err(|e| e.to_string())?;
        rows.extend(update.nodes.iter().map(Row::from_node));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_nodes_decode_and_the_launch_line_is_ours_then_the_apps() {
        use makepad_a11y::{Node, NodeId, Rect, Role, TreeUpdate};
        let update = TreeUpdate { nodes: vec![
            Node { id: NodeId(1), role: Role::Window, path: "main_window".into(), ..Default::default() },
            Node { id: NodeId(2), role: Role::Button, path: "body.increment_button".into(), bounds: Rect { x: 284.0, y: 25.0, w: 76.0, h: 22.0 }, ..Default::default() },
        ], ..Default::default() };
        let mut bytes = Vec::new();
        makepad_a11y::wire::encode_update(&update, &mut bytes);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let reply = makepad_strict_json::parse_depth(format!("{{\"trees\":[{{\"wire\":\"{hex}\"}}]}}").as_bytes(), 32).unwrap();
        let rows = rows(&reply).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].path.as_deref(), Some("body.increment_button"));
        assert_eq!(rows[1].rect, [284.0, 25.0, 76.0, 22.0]);
        let spec = LaunchSpec {
            binary: "x".into(),
            cwd: ".".into(),
            args: vec!["--demo".into()],
            env: vec![],
            output: RunOutput { width: 1080, height: 1920, dpi_scale: 3.0, fps: 60 },
            seed: 7,
            audio: false,
            theme: None,
            log_path: "l".into(),
        };
        assert_eq!(spec.argv(), vec!["--remote", "--virtual-clock", "--window=360x640@3", "--seed=7", "--remote-title-tag=off", "--demo"]);
    }
}
