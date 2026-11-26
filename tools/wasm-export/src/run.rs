//! The trace host's runs, as child processes: each writes its log and its coverage file into
//! the build folder; up to `jobs` run at once, in one [`Pool`] across phases. Progress is read from the logs: a driver that
//! logs `progress <n>/<total>` (makepad-story-doc-player does, every 10 s) is that far; the others count
//! as started until they end. A cancelled export stops its runs (by their own handles).

use crate::{Driver, Failure, Options, Phase, Progress};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// One run of the trace host.
#[derive(Clone, Debug)]
pub struct Job {
    pub name: String,
    /// The trace host's arguments (`trace ... <package>`).
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub log: PathBuf,
    /// Its coverage file.
    pub out: PathBuf,
}

impl Job {
    /// `trace` of `driver`, writing `<dir>/<name>.json` and `<dir>/<name>.log`. Pass options go
    /// in after `trace` (`args.splice(1..1, ..)`).
    pub fn trace(opts: &Options, d: &Driver, dir: &Path) -> Job {
        let out = dir.join(format!("{}.json", d.name));
        let mut args: Vec<String> = vec!["trace".into(), "--window".into(), d.window.clone(), "--out".into(), out.display().to_string()];
        args.extend(d.environment.args());
        // a run that ends at a console line plays as long as it takes
        let ms = d.ms.or(if d.until.is_some() && d.script.is_none() { Some(1_000_000_000) } else { None });
        if let Some(ms) = ms {
            args.extend(["--ms".into(), ms.to_string()]);
        }
        if let Some(until) = &d.until {
            args.extend(["--until".into(), until.clone()]);
        }
        if let Some(script) = &d.script {
            args.extend(["--script".into(), script.display().to_string()]);
        }
        if d.stress {
            args.push("--stress".into());
        }
        if !d.query.is_empty() {
            args.extend(["--query".into(), d.query.clone()]);
        }
        for (prefix, dir) in &opts.serves {
            args.extend(["--serve".into(), format!("{}={}", prefix, dir.display())]);
        }
        args.push(opts.package.clone());
        args.push("--release".into());
        let mut env = vec![
            ("MAKEPAD_HIDE_WINDOWS".into(), "1".into()),
            ("MAKEPAD_MUTE".into(), "1".into()),
            ("CARGO_INCREMENTAL".into(), "0".into()),
        ];
        env.extend(crate::tools::host_env(&opts.source_root, opts.build()));
        Job { name: d.name.clone(), args, env, log: dir.join(format!("{}.log", d.name)), out }
    }

    fn spawn(&self, opts: &Options) -> Result<Child, String> {
        let log = std::fs::File::create(&self.log).map_err(|e| format!("{}: {e}", self.log.display()))?;
        let err = log.try_clone().map_err(|e| format!("{}: {e}", self.log.display()))?;
        // at a low priority: the person's machine stays usable
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("nice");
            c.arg("-n").arg("15").arg(opts.host());
            c
        };
        #[cfg(not(unix))]
        let mut cmd = Command::new(opts.host());
        cmd.args(&self.args).current_dir(&opts.package_dir).stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(err));
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let stripped: Vec<&str> = opts.js_stripped.iter().map(String::as_str).collect();
        let js = makepad_web_pack::js::one_wasm_js(&opts.source_root, &stripped, &opts.name)?;
        let roots: Vec<_> = makepad_web_pack::js::export_references(&js)?.into_iter().filter(|n| n.starts_with("wasm_")).collect();
        cmd.env("RELAX_WASM_EXPORT_ROOTS", roots.join(","));
        let _ = std::fs::remove_file(&self.out);
        cmd.spawn().map_err(|e| format!("{}: {e}", opts.host().display()))
    }
}

/// The last `progress <n>/<total>` of a log, as a fraction, and the log's last line.
fn read_progress(log: &Path) -> (Option<f64>, String) {
    let Ok(mut f) = std::fs::File::open(log) else { return (None, String::new()) };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(16 * 1024);
    let _ = f.seek(SeekFrom::Start(from));
    let mut tail = Vec::new();
    let _ = f.read_to_end(&mut tail);
    let text = String::from_utf8_lossy(&tail);
    let mut done = None;
    for line in text.lines().rev() {
        if let Some(at) = line.rfind("progress ") {
            let rest = &line[at + "progress ".len()..];
            let (n, total) = rest.split_once('/').unwrap_or((rest, ""));
            let total: String = total.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let (Ok(n), Ok(total)) = (n.trim().parse::<f64>(), total.parse::<f64>()) {
                if total > 0.0 {
                    done = Some((n / total).clamp(0.0, 1.0));
                    break;
                }
            }
        }
    }
    let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    (done, last)
}

/// The last lines of a log, for a failure.
fn tail(log: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Runs `jobs`, `opts.jobs` at a time; every one must end with status 0 and write its file.
pub fn run_all(opts: &Options, jobs: &[Job], phase: Phase, progress: &mut dyn Progress) -> Result<(), Failure> {
    let mut pool = Pool::new(opts);
    let ids = pool.queue(jobs.to_vec(), phase);
    pool.wait(&ids, phase, progress)
}

enum Slot {
    Queued,
    Running(Child),
    Done,
}

/// The runs of several phases, `opts.jobs` at a time. A phase queues its runs and waits for
/// them; runs queued for a later phase that need nothing more (the gate's replays, once the
/// pack and the embed exist) start in the slots the waited ones leave free. The waited runs
/// start first. Every run must end with status 0 and write its file; the first that does not
/// stops them all and fails the export in its own phase. Dropped, the pool stops its runs.
pub struct Pool<'a> {
    opts: &'a Options,
    jobs: Vec<(Job, Phase)>,
    slots: Vec<Slot>,
    fraction: Vec<f64>,
}

impl<'a> Pool<'a> {
    pub fn new(opts: &'a Options) -> Pool<'a> {
        Pool { opts, jobs: Vec::new(), slots: Vec::new(), fraction: Vec::new() }
    }

    /// Queues `jobs` of `phase`; their ids for [`Pool::wait`].
    pub fn queue(&mut self, jobs: Vec<Job>, phase: Phase) -> Vec<usize> {
        let first = self.jobs.len();
        for job in jobs {
            self.jobs.push((job, phase));
            self.slots.push(Slot::Queued);
            self.fraction.push(0.0);
        }
        (first..self.jobs.len()).collect()
    }

    fn running(&self) -> usize {
        self.slots.iter().filter(|slot| matches!(slot, Slot::Running(_))).count()
    }

    fn stop(&mut self) {
        for slot in &mut self.slots {
            if let Slot::Running(child) = slot {
                let _ = child.kill();
                let _ = child.wait();
                *slot = Slot::Done;
            }
        }
    }

    fn fail(&mut self, failure: Failure) -> Failure {
        self.stop();
        failure
    }

    /// Runs the pool until the runs `ids` have ended, reporting them as `phase`.
    pub fn wait(&mut self, ids: &[usize], phase: Phase, progress: &mut dyn Progress) -> Result<(), Failure> {
        let width = self.opts.jobs.max(1);
        loop {
            if ids.iter().all(|&i| matches!(self.slots[i], Slot::Done)) {
                break;
            }
            while self.running() < width {
                // the waited runs first, then the ones queued ahead
                let Some(i) = ids.iter().copied().find(|&i| matches!(self.slots[i], Slot::Queued))
                    .or_else(|| (0..self.slots.len()).find(|&i| matches!(self.slots[i], Slot::Queued))) else { break };
                match self.jobs[i].0.spawn(self.opts) {
                    Ok(child) => self.slots[i] = Slot::Running(child),
                    Err(e) => {
                        let (job, job_phase) = &self.jobs[i];
                        let failure = Failure::new(*job_phase, format!("{}: could not start the trace host: {e}", job.name));
                        return Err(self.fail(failure));
                    }
                }
            }
            if progress.cancelled() {
                return Err(self.fail(Failure::new(phase, "cancelled")));
            }
            for i in 0..self.slots.len() {
                let Slot::Running(child) = &mut self.slots[i] else { continue };
                let (job, job_phase) = &self.jobs[i];
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() || !job.out.is_file() {
                            let failure = Failure::new(*job_phase, format!("run `{}` failed ({status}):\n{}", job.name, tail(&job.log, 12))).details(&job.log);
                            self.slots[i] = Slot::Done;
                            return Err(self.fail(failure));
                        }
                        self.slots[i] = Slot::Done;
                        self.fraction[i] = 1.0;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let failure = Failure::new(*job_phase, format!("run `{}`: {e}", job.name));
                        return Err(self.fail(failure));
                    }
                }
            }
            if ids.iter().all(|&i| matches!(self.slots[i], Slot::Done)) {
                break;
            }
            let mut notes = Vec::new();
            for i in 0..self.slots.len() {
                if !matches!(self.slots[i], Slot::Running(_)) {
                    continue;
                }
                let (done, last) = read_progress(&self.jobs[i].0.log);
                if let Some(done) = done {
                    self.fraction[i] = done;
                }
                let shown: String = last.chars().take(80).collect();
                // a run of a later phase, filling a free slot, says so
                let ahead = if self.jobs[i].1 == phase { String::new() } else { format!("{} ", self.jobs[i].1.name()) };
                notes.push(format!("{ahead}{} {:.0}%: {}", self.jobs[i].0.name, self.fraction[i] * 100.0, shown));
            }
            let total = ids.iter().map(|&i| self.fraction[i]).sum::<f64>() / ids.len().max(1) as f64;
            progress.phase(phase, total, &notes.join(" | "));
            std::thread::sleep(Duration::from_millis(500));
        }
        progress.phase(phase, 1.0, &format!("{} runs done", ids.len()));
        Ok(())
    }
}

impl Drop for Pool<'_> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A trace host subcommand to the end; its stderr (and stdout) when it fails.
fn host(opts: &Options, args: &[String], phase: Phase) -> Result<(std::process::ExitStatus, String), Failure> {
    let out = Command::new(opts.host())
        .args(args)
        .envs(crate::tools::host_env(&opts.source_root, opts.build()))
        .current_dir(&opts.package_dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Failure::new(phase, format!("{}: {e}", opts.host().display())))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok((out.status, text))
}

/// The coverage files as one (the union of the runs).
pub fn merge(opts: &Options, out: &Path, inputs: &[PathBuf], phase: Phase) -> Result<(), Failure> {
    if let [one] = inputs {
        return std::fs::copy(one, out).map(|_| ()).map_err(|e| Failure::new(phase, format!("{}: {e}", out.display())));
    }
    let mut args = vec!["merge".to_string(), out.display().to_string()];
    args.extend(inputs.iter().map(|p| p.display().to_string()));
    let (status, text) = host(opts, &args, phase)?;
    if !status.success() {
        return Err(Failure::new(phase, format!("merging the runs' coverage failed: {text}")));
    }
    Ok(())
}

/// The replay gate: what `b` ran, fetched, drew or looked up that `a` did not. The report goes
/// to `report`; Ok is its summary line, any stub hit is a failure.
pub fn gate(opts: &Options, a: &Path, b: &Path, report: &Path) -> Result<String, Failure> {
    let mut args = vec!["gate".into(), a.display().to_string(), b.display().to_string()];
    if !opts.block_shaking {
        args.extend(["--functions".into(), opts.build().join("shipped-functions.txt").display().to_string()]);
    }
    let (status, text) = host(opts, &args, Phase::Gate)?;
    let _ = std::fs::write(report, &text);
    let summary = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    match status.code() {
        Some(0) => Ok(if summary.is_empty() { "0 stub hits".into() } else { summary }),
        Some(1) => Err(Failure::new(Phase::Gate, format!("the held-out replays hit code or data the shipped page does not have:\n{}", head(&text, 24))).details(report)),
        _ => Err(Failure::new(Phase::Gate, format!("the gate did not run: {}", head(&text, 8))).details(report)),
    }
}

fn head(text: &str, lines: usize) -> String {
    text.lines().take(lines).collect::<Vec<_>>().join("\n")
}

/// The modules `<prefix><pid>.wasm` an earlier export left in `dir`.
pub fn remove_modules(dir: &Path, prefix: &str) {
    for path in modules(dir, prefix) {
        let _ = std::fs::remove_file(path);
    }
}

fn modules(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(".wasm")))
        .collect();
    out.sort();
    out
}

/// The module the capture run wrote (`RELAX_WASM_EXPORT=<prefix>%p.wasm`).
pub fn captured_module(dir: &Path, prefix: &str, log: &Path) -> Result<Vec<u8>, Failure> {
    let found = modules(dir, prefix);
    let path = match found.as_slice() {
        [one] => one.clone(),
        [] => return Err(Failure::new(Phase::Capture, format!("the capture run wrote no module ({}):\n{}", dir.join(format!("{prefix}<pid>.wasm")).display(), tail(log, 12))).details(log)),
        more => return Err(Failure::new(Phase::Capture, format!("{} modules in {} (one capture run expected)", more.len(), dir.display())).details(log)),
    };
    let bytes = std::fs::read(&path).map_err(|e| Failure::new(Phase::Capture, format!("{}: {e}", path.display())))?;
    if !bytes.starts_with(b"\0asm") {
        return Err(Failure::new(Phase::Capture, format!("{}: not a wasm module", path.display())));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_the_last_progress_line() {
        let dir = std::env::temp_dir().join(format!("wasm-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("a.log");
        std::fs::write(&log, "x\n[page] makepad-story-doc-player: 10 frames so far; waiting for -; progress 10/40\n[I] makepad-story-doc-player: 20 frames so far; waiting for -; progress 20/40\nlast line\n").unwrap();
        let (done, last) = read_progress(&log);
        assert_eq!(done, Some(0.5));
        assert_eq!(last, "last line");
        std::fs::write(&log, "progress ?/x\n").unwrap();
        assert_eq!(read_progress(&log).0, None);
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_dir(&dir);
    }
}
