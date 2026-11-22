//! A resin printer's link: an exported layer stack ([`crate::export`]) goes to the printer over
//! a network connection, and the printer is asked to start the job, pause, resume or stop it,
//! and how far it is.
//!
//! This is Fab's own generic protocol. A real printer's protocol would be another link over the
//! same exported stack. Until one is named, the protocol is spoken by [`fake::FakePrinter`], a
//! printer the tests start on the loopback address.
//!
//! Where the printer listens is the person's own file ([`linked`]); a print runs on a thread of
//! its own ([`printing`]).
//!
//! The protocol, version 1, over one TCP connection:
//! - Every request is one line of text ending in a newline. Every answer is one line too.
//! - An answer is `OK`, the request's own answer (`READY`, `STATUS`), or `ERR` followed by why
//!   the printer refused.
//! - `HELLO 1`: the client speaks version 1. The printer answers `READY <its name>`.
//! - `SEND <file> <bytes>`, then exactly that many bytes: one file of the stack. A file name is
//!   letters, digits, `.`, `-` and `_`, starting with a letter or digit. A file sent again
//!   replaces the earlier one. Answer: `OK`.
//! - `CLEAR`: the printer drops every file it holds (refused while it prints). Answer: `OK`.
//! - `PRINT`: the printer starts the stack it holds. It first checks that the manifest is a layer
//!   stack's, that the images are its screen's size, and that every layer the manifest names is
//!   there. Answer: `OK`.
//! - `STATUS`: answer `STATUS <state> <layer> <layers>`. The state is `idle`, `printing`,
//!   `paused`, `done` or `stopped`. `layer` is how many layers have cured.
//! - `PAUSE`, `RESUME`, `STOP`: the job holds, goes on or ends. Answer: `OK`.
//! - `BYE`: the printer closes the connection.

pub mod fake;
pub mod linked;
pub mod printing;

use crate::export::MANIFEST;
use makepad_strict_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// The protocol's version.
pub const VERSION: u32 = 1;

/// The largest file either end takes (bytes).
pub const MAX_FILE: usize = 1 << 30;

/// What the printer is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Printing,
    Paused,
    Done,
    Stopped,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Printing => "printing",
            State::Paused => "paused",
            State::Done => "done",
            State::Stopped => "stopped",
        }
    }

    pub fn of(name: &str) -> Option<State> {
        [State::Idle, State::Printing, State::Paused, State::Done, State::Stopped].into_iter().find(|s| s.name() == name)
    }
}

/// How far the printer's job is: `layer` of `layers` cured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub layer: u32,
    pub layers: u32,
}

/// Whether `name` is a file name the protocol carries.
pub fn file_name_ok(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// A connection to a resin printer.
pub struct ResinLink {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    printer: String,
}

impl ResinLink {
    /// Connects to the printer at `addr` and says hello. Connecting, and each answer after,
    /// waits at most `wait`.
    pub fn connect(addr: SocketAddr, wait: Duration) -> Result<ResinLink, String> {
        let stream = TcpStream::connect_timeout(&addr, wait).map_err(|e| format!("connecting to the printer: {e}"))?;
        let set = |r: std::io::Result<()>| r.map_err(|e| format!("setting up the connection: {e}"));
        set(stream.set_read_timeout(Some(wait)))?;
        set(stream.set_write_timeout(Some(wait)))?;
        set(stream.set_nodelay(true))?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| format!("setting up the connection: {e}"))?);
        let mut link = ResinLink { reader, writer: stream, printer: String::new() };
        let ready = link.ask(&format!("HELLO {VERSION}"))?;
        link.printer = ready.strip_prefix("READY ").ok_or_else(|| format!("the printer answered {ready:?} to hello"))?.to_string();
        Ok(link)
    }

    /// The printer's name, as it said hello.
    pub fn printer(&self) -> &str {
        &self.printer
    }

    /// Sends one request (and `payload` after its line), and returns the answer, or why the
    /// printer refused.
    fn request(&mut self, line: &str, payload: &[u8]) -> Result<String, String> {
        let verb = line.split(' ').next().unwrap_or(line);
        let sent = self.writer.write_all(format!("{line}\n").as_bytes()).and_then(|_| self.writer.write_all(payload)).and_then(|_| self.writer.flush());
        sent.map_err(|e| format!("sending {verb} to the printer: {e}"))?;
        let mut answer = String::new();
        match self.reader.read_line(&mut answer) {
            Ok(0) => return Err(format!("the printer closed the connection after {verb}")),
            Ok(_) => {}
            Err(e) => return Err(format!("the printer didn't answer {verb}: {e}")),
        }
        let answer = answer.trim_end_matches(['\n', '\r']).to_string();
        match answer.strip_prefix("ERR ") {
            Some(why) => Err(format!("the printer refused {verb}: {why}")),
            None => Ok(answer),
        }
    }

    fn ask(&mut self, line: &str) -> Result<String, String> {
        self.request(line, &[])
    }

    /// A request whose answer is `OK`.
    fn ok(&mut self, line: &str, payload: &[u8]) -> Result<(), String> {
        match self.request(line, payload)? {
            a if a == "OK" => Ok(()),
            a => Err(format!("the printer answered {a:?} to {}", line.split(' ').next().unwrap_or(line))),
        }
    }

    /// Sends the file `name` (letters, digits, `.`, `-` and `_`).
    pub fn send_file(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if !file_name_ok(name) {
            return Err(format!("{name:?} is no file name the printer takes"));
        }
        if bytes.len() > MAX_FILE {
            return Err(format!("{name} is {} bytes, more than the printer takes", bytes.len()));
        }
        self.ok(&format!("SEND {name} {}", bytes.len()), bytes)
    }

    /// Sends the stack exported in `dir`: the printer's old files go, then every layer the
    /// manifest names, then the manifest. `sent(k, n)` hears each layer as it goes, and stops the
    /// sending when it says false (the manifest doesn't go then). Returns the number of layers.
    pub fn send_stack(&mut self, dir: &Path, sent: &mut dyn FnMut(u32, u32) -> bool) -> Result<u32, String> {
        let manifest = std::fs::read(dir.join(MANIFEST)).map_err(|e| format!("reading {}: {e}", dir.join(MANIFEST).display()))?;
        let layers = layer_files(&manifest)?;
        self.ok("CLEAR", &[])?;
        let n = layers.len() as u32;
        for (k, file) in layers.iter().enumerate() {
            let bytes = std::fs::read(dir.join(file)).map_err(|e| format!("reading {file}: {e}"))?;
            self.send_file(file, &bytes)?;
            if !sent(k as u32 + 1, n) {
                return Err(format!("stopped after {} of {n} layers", k + 1));
            }
        }
        self.send_file(MANIFEST, &manifest)?;
        Ok(n)
    }

    /// Starts the stack the printer holds.
    pub fn print(&mut self) -> Result<(), String> {
        self.ok("PRINT", &[])
    }

    pub fn status(&mut self) -> Result<Status, String> {
        let a = self.ask("STATUS")?;
        let parsed = a.strip_prefix("STATUS ").and_then(|rest| {
            let mut w = rest.split(' ');
            let s = Status { state: State::of(w.next()?)?, layer: w.next()?.parse().ok()?, layers: w.next()?.parse().ok()? };
            w.next().is_none().then_some(s)
        });
        parsed.ok_or_else(|| format!("the printer answered {a:?} to STATUS"))
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.ok("PAUSE", &[])
    }

    pub fn resume(&mut self) -> Result<(), String> {
        self.ok("RESUME", &[])
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.ok("STOP", &[])
    }
}

impl Drop for ResinLink {
    fn drop(&mut self) {
        // a goodbye the printer may not hear: the connection closes either way
        let _ = self.writer.write_all(b"BYE\n");
    }
}

/// The layer files a manifest names, in order.
pub fn layer_files(manifest: &[u8]) -> Result<Vec<String>, String> {
    let m = makepad_strict_json::parse(manifest).map_err(|e| format!("the manifest is no JSON: {e}"))?;
    let layers = m.get("layers").and_then(Value::as_arr).ok_or("the manifest names no layers")?;
    layers.iter().map(|l| l.as_str().filter(|f| file_name_ok(f)).map(str::to_string).ok_or_else(|| format!("the manifest names a layer {l:?}"))).collect()
}

#[cfg(test)]
mod tests;
