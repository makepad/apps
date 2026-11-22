//! A single wall-clock queue. Slow answers make later entries late; no input or grab is dropped.

use crate::document::{time, Action, Tour};
use makepad_remote::{app, link, Client};
use makepad_strict_json::{self as json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

struct Command(Vec<String>);

impl Command {
    fn new(words: &str) -> Self { Self(words.split_whitespace().map(str::to_owned).collect()) }
    fn arg(mut self, key: &str, value: impl std::fmt::Display) -> Self { self.0.push(format!("{key}={value}")); self }
    fn word(mut self, word: &str) -> Self { self.0.push(word.to_owned()); self }
    fn display(&self) -> String {
        self.0.iter().map(|word| {
            if word.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./-=:?".contains(&b)) { word.clone() }
            else { format!("'{}'", word.replace('\'', "'\\''")) }
        }).collect::<Vec<_>>().join(" ")
    }
}

fn find(query: &str) -> Command { Command::new("ui find").arg("q", query).arg("json", 1) }
fn drag(query: &str, dx: impl std::fmt::Display, dy: impl std::fmt::Display, ms: u64, seq: &str) -> Command {
    Command::new("ui act").arg("q", query).word("drag").arg("dx", dx).arg("dy", dy).arg("ms", ms).arg("if_user_seq", seq).arg("json", 1)
}
fn path_text(path: &Path) -> Result<&str, String> { path.to_str().ok_or_else(|| format!("path is not UTF-8: {}", path.display())) }
fn cursor(tour: &Tour) -> Command { Command::new("cursor").arg("show", 1).arg("style", &tour.cursor) }
fn capture(tour: &Tour, out: &Path) -> Result<Option<Command>, String> {
    tour.record.as_ref().map(|record| Ok(Command::new("cap start").arg("path", path_text(&out.join(record))?).arg("overwrite", 1))).transpose()
}

fn commands(action: &Action, at: Duration, out: &Path, seq: &str) -> Result<Vec<Command>, String> {
    Ok(match action {
        Action::Settle(_) => vec![Command::new("settled")],
        Action::Press(query) => vec![Command::new("ui act").arg("q", query).word("press").arg("if_user_seq", seq).arg("json", 1)],
        Action::Drag { node, dx, dy, ms } => vec![drag(node, dx, dy, *ms, seq)],
        Action::DragTo { from, to, ms } => vec![find(from), find(to), drag(from, "?", "?", *ms, seq)],
        Action::Key(code) => vec![Command::new("key").arg("k", "press").arg("c", code).arg("if_user_seq", seq)],
        Action::Grab => vec![Command::new("grab").arg("out", path_text(&out.join(format!("tour-{}.png", time(at))))?)],
    })
}

pub fn plan(tour: &Tour, out: &Path) -> Result<(), String> {
    println!("0.0 activity");
    println!("0.0 {}", cursor(tour).display());
    if let Some(command) = capture(tour, out)? { println!("0.0 {}", command.display()); }
    for event in &tour.events {
        for command in commands(&event.action, event.at, out, "<user_seq>")? { println!("{} {}", time(event.at), command.display()); }
    }
    let end = tour.events.last().map_or(Duration::ZERO, |e| e.at);
    println!("{} activity", time(end));
    if tour.record.is_some() { println!("{} cap stop", time(end)); }
    Ok(())
}

#[derive(Debug)]
pub enum Error { Message(String), Timeout }

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Message(message) => f.write_str(message), Self::Timeout => f.write_str("no answer within the timeout") }
    }
}
impl From<String> for Error { fn from(message: String) -> Self { Self::Message(message) } }
impl From<link::Error> for Error {
    fn from(error: link::Error) -> Self { match error { link::Error::Timeout => Self::Timeout, error => Self::Message(error.to_string()) } }
}

struct Reply { lines: Vec<String>, values: Vec<Value> }
struct Remote { client: Client, seq: Option<u64> }

impl Remote {
    fn ask(&self, command: &Command) -> Result<Reply, Error> {
        let mut reply = Reply { lines: Vec::new(), values: Vec::new() };
        // run_end forwards ordinary commands through request_line/Client::cmd/Lines, while
        // grab uses the frames service and writes the PNG locally. Keep argv values intact:
        // run_line's plain-text path splits on whitespace, even inside quoted queries/paths.
        let result = app::run_end(&self.client, &command.0, &mut |line| { reply.lines.push(line.to_owned()); true });
        for line in &reply.lines {
            let value = json::parse(line.as_bytes()).map_err(|e| Error::Message(format!("{}: expected a JSON answer ({e}): {line}", command.display())))?;
            if let Some(error) = value.get("err") {
                let reason = match error.as_str() {
                    Some(reason) if reason.starts_with("user_intervened") || reason.starts_with("user_interacting") => "user intervened; stopping the tour".to_owned(),
                    _ => format!("command failed: {}", command.display()),
                };
                return Err(Error::Message(format!("{reason}\n{line}")));
            }
            reply.values.push(value);
        }
        let (failed, end) = result?;
        if failed { return Err(Error::Message(format!("command failed: {}", command.display()))); }
        if let (Some(expected), Some(end)) = (self.seq, end) {
            let end = json::parse(end.as_bytes()).map_err(|e| Error::Message(format!("invalid command end: {e}")))?;
            if end.get("user_seq").and_then(Value::as_u64).is_some_and(|seq| seq != expected) { return Err(Error::Message("user intervened; stopping the tour".into())); }
        }
        Ok(reply)
    }

    fn activity(&self) -> Result<u64, Error> {
        let reply = self.ask(&Command::new("activity"))?;
        let seq = reply.values.iter().find_map(|v| v.get("user_seq").and_then(Value::as_u64))
            .ok_or_else(|| Error::Message("activity: expected a JSON user_seq integer".into()))?;
        if self.seq.is_some_and(|expected| expected != seq) { return Err(Error::Message("user intervened; stopping the tour".into())); }
        Ok(seq)
    }

    fn settle(&self, limit: Duration) -> Result<(), Error> {
        let started = Instant::now();
        loop {
            let remaining = limit.saturating_sub(started.elapsed());
            if remaining.is_zero() { break; }
            let timeout = self.client.timeout();
            self.client.set_timeout(Some(remaining));
            let reply = self.ask(&Command::new("settled"));
            self.client.set_timeout(timeout);
            let reply = match reply { Err(Error::Timeout) => break, reply => reply? };
            let settled = reply.values.iter().find_map(|v| v.get("settled").and_then(Value::as_bool))
                .ok_or_else(|| Error::Message("settled: expected a JSON settled boolean".into()))?;
            if settled { return Ok(()); }
            std::thread::sleep(Duration::from_millis(50).min(limit.saturating_sub(started.elapsed())));
        }
        eprintln!("warning: app did not settle within {} seconds; continuing", time(limit));
        Ok(())
    }
}

struct Target { window: String, x: f64, y: f64 }

fn centre(reply: &Reply, query: &str) -> Result<Target, Error> {
    let fail = |why| Error::Message(format!("ui find {query:?}: {why}"));
    let mut found = None;
    for answer in &reply.values {
        let trees = answer.get("trees").and_then(Value::as_arr).ok_or_else(|| fail("expected JSON trees"))?;
        for tree in trees {
            let nodes = tree.get("nodes").and_then(Value::as_arr).ok_or_else(|| fail("expected JSON nodes"))?;
            for node in nodes {
                if found.is_some() { return Err(fail("expected exactly one node; query is ambiguous")); }
                let bounds = node.get("bounds").and_then(Value::as_arr).ok_or_else(|| fail("expected bounds [x,y,w,h]"))?;
                let values: Option<Vec<f64>> = bounds.iter().map(|v| match v { Value::Int(n) => Some(*n as f64), Value::F64(n) if n.is_finite() => Some(*n), _ => None }).collect();
                let values = values.ok_or_else(|| fail("expected finite bounds [x,y,w,h]"))?;
                let [x, y, w, h] = values.as_slice() else { return Err(fail("expected bounds [x,y,w,h]")); };
                if *w <= 0.0 || *h <= 0.0 { return Err(fail("expected nonempty visible bounds")); }
                let states = node.get("states").and_then(Value::as_str).unwrap_or_default();
                if states.split(|c: char| c == ',' || c.is_whitespace()).any(|s| matches!(s, "hidden" | "offscreen" | "disabled")) { return Err(fail("expected a visible enabled node")); }
                let window = tree.get("window").and_then(Value::as_str).ok_or_else(|| fail("expected a window id"))?.to_owned();
                found = Some(Target { window, x: x + w * 0.5, y: y + h * 0.5 });
            }
        }
    }
    found.ok_or_else(|| fail("expected exactly one node; query matched nothing"))
}

fn timeline(remote: &Remote, tour: &Tour, out: &Path, seq: u64) -> Result<(), Error> {
    // Setup and cap start have completed: this is zero for both beats and grabs.
    let started = Instant::now();
    for (index, event) in tour.events.iter().enumerate() {
        std::thread::sleep(event.at.saturating_sub(started.elapsed()));
        let actual = started.elapsed();
        eprintln!("event {} line {} planned={} actual={:.6} late={:.6}", index + 1, event.line, time(event.at), actual.as_secs_f64(), actual.saturating_sub(event.at).as_secs_f64());
        match &event.action {
            Action::Settle(limit) => remote.settle(*limit)?,
            Action::DragTo { from, to, ms } => {
                let from_node = centre(&remote.ask(&find(from))?, from)?;
                let to_node = centre(&remote.ask(&find(to))?, to)?;
                if from_node.window != to_node.window { return Err(Error::Message("drag from/to requires nodes in the same window".into())); }
                let (dx, dy) = (to_node.x - from_node.x, to_node.y - from_node.y);
                if !dx.is_finite() || !dy.is_finite() { return Err(Error::Message("drag from/to requires finite centre offsets".into())); }
                let command = drag(from, dx, dy, *ms, &seq.to_string());
                eprintln!("  {}", command.display());
                remote.ask(&command)?;
            }
            action => {
                for command in commands(action, event.at, out, &seq.to_string())? {
                    eprintln!("  {}", command.display());
                    remote.ask(&command)?;
                }
            }
        }
    }
    remote.activity()?;
    Ok(())
}

pub fn play(tour: &Tour, out: &Path, spec: &str) -> Result<(), Error> {
    let mut remote = Remote { client: Client::connect(spec)?, seq: None };
    let seq = remote.activity()?;
    remote.seq = Some(seq);
    let recording = capture(tour, out)?;
    let mut capture_attempted = false;
    let result = (|| {
        remote.ask(&cursor(tour))?;
        if let Some(command) = &recording {
            // Even a lost start acknowledgment may leave the encoder running.
            capture_attempted = true;
            remote.ask(command)?;
        }
        timeline(&remote, tour, out, seq)
    })();
    // Cleanup is unconditional after attempting capture, and has no stale input guard.
    remote.seq = None;
    let stop = if capture_attempted {
        remote.ask(&Command::new("cap stop")).map(|reply| { for line in reply.lines { println!("{line}"); } })
    } else { Ok(()) };
    match (result, stop) {
        (Err(error), Err(stop)) => Err(Error::Message(format!("{error}\ncap stop also failed: {stop}"))),
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
}
