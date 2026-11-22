//! The resin printer the person linked: a `<name>.resin` file in Fab's printers folder
//! (`~/.makepad/fab/printers`, beside the filament printers' `.conf` files), the first by name.
//! It says where the printer listens, one `key = value` a line (`#` starts a comment):
//!
//! ```text
//! host = the printer's address on the network
//! port = the port its link listens on
//! ```
//!
//! The address lives only in that file: Fab reads it when it needs it and never says it, in the
//! UI, a log or an error.

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

/// The extension of a resin printer's file.
pub const EXTENSION: &str = "resin";

/// A linked resin printer: its file's name and where it listens.
#[derive(Clone, PartialEq, Eq)]
pub struct Linked {
    /// The file's name without `.resin`.
    pub name: String,
    host: String,
    port: u16,
}

impl std::fmt::Debug for Linked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never the address
        write!(f, "Linked({})", self.name)
    }
}

impl Linked {
    /// The printer of `name` as its file's `text` says. The errors name the file, never what it
    /// holds.
    pub fn parse(name: &str, text: &str) -> Result<Linked, String> {
        let file = format!("{name}.{EXTENSION}");
        let (mut host, mut port) = (None, None);
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (key, value) = line.split_once('=').ok_or_else(|| format!("{file}: a line that isn't key = value"))?;
            let value = value.trim();
            let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
            let slot = match key.trim() {
                "host" => &mut host,
                "port" => &mut port,
                other => return Err(format!("{file}: no setting {other:?} (host and port are)")),
            };
            if slot.replace(value).is_some() {
                return Err(format!("{file}: {} is set twice", key.trim()));
            }
        }
        let host = host.filter(|h| !h.is_empty() && !h.chars().any(char::is_whitespace) && !h.contains('/') && !h.contains('@')).ok_or_else(|| format!("{file}: no host, or not one"))?;
        let port = port.and_then(|p| p.parse::<u16>().ok()).filter(|p| *p > 0).ok_or_else(|| format!("{file}: no port, or not one"))?;
        Ok(Linked { name: name.to_string(), host: host.to_string(), port })
    }

    /// The printer's address, looked up now (a name is resolved).
    pub fn addr(&self) -> Result<SocketAddr, String> {
        let file = format!("{}.{EXTENSION}", self.name);
        (self.host.as_str(), self.port).to_socket_addrs().map_err(|e| format!("{file}: its host isn't found: {}", e.kind()))?.next().ok_or_else(|| format!("{file}: its host has no address"))
    }
}

/// Fab's printers folder.
pub fn printers_dir() -> PathBuf {
    makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("printers")
}

/// The first resin printer by name in `dir`: none when there is none (or no folder).
pub fn linked_in(dir: &Path) -> Result<Option<Linked>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(None) };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == EXTENSION).then(|| path.file_stem()?.to_str().map(str::to_string))?
        })
        .filter(|n| super::file_name_ok(n))
        .collect();
    names.sort();
    let Some(name) = names.into_iter().next() else { return Ok(None) };
    named_in(dir, &name).map(Some)
}

/// The resin printer whose file in `dir` is `name`'s.
pub fn named_in(dir: &Path, name: &str) -> Result<Linked, String> {
    if !super::file_name_ok(name) {
        return Err(format!("{name:?} is no printer's file"));
    }
    let text = std::fs::read_to_string(dir.join(format!("{name}.{EXTENSION}"))).map_err(|e| format!("{name}.{EXTENSION}: {}", e.kind()))?;
    Linked::parse(name, &text)
}

/// The linked resin printer in Fab's printers folder.
pub fn linked() -> Result<Option<Linked>, String> {
    linked_in(&printers_dir())
}

/// The resin printer whose file in Fab's printers folder is `name`'s: the one a job went to.
pub fn named(name: &str) -> Result<Linked, String> {
    named_in(&printers_dir(), name)
}
