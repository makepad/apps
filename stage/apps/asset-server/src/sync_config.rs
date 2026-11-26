//! The `store-sync` file: whether (and how) an app's store syncs with the
//! fleet (makepad_asset_store::host::sync). Off unless the file exists.
//!
//! ```text
//! # <PREFIX>_CONFIG_DIR/store-sync (Sandbox: ~/.config/makepad-sandbox/store-sync)
//! listen 192.0.2.10:8795      # this machine's LAN address for the sync front (required)
//! peer 192.0.2.11:8795        # every store to pull from (none = serve only)
//! peer 192.0.2.12:8795
//! exclude gen/built/          # optional: alias prefixes never pulled (default gen/built/)
//! ```
//!
//! `<PREFIX>_STORE_SYNC=<file>` points at another file. The front and the
//! pulls use this machine's fleet identity (`~/.makepad/fleet`,
//! tools/fleet-identity.md); sync refuses to start without it.

use makepad_asset_store::SyncConfig;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "store-sync";

/// Alias prefixes no store pulls unless its file says otherwise: per-machine
/// build products every machine derives itself.
pub const DEFAULT_EXCLUDE: &[&str] = &["gen/built/"];

/// Where an app's sync file is: `<PREFIX>_STORE_SYNC`, else
/// `<PREFIX>_CONFIG_DIR/store-sync`, else `~/.config/makepad-<app>/store-sync`.
pub fn sync_file(prefix: &str, app: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(format!("{prefix}_STORE_SYNC")).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(dir) = std::env::var_os(format!("{prefix}_CONFIG_DIR")).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(dir).join(FILE_NAME));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config").join(format!("makepad-{app}")).join(FILE_NAME))
}

/// Settings an older build wrote that sync no longer reads (FLEETID: the fleet identity lives in
/// `~/.makepad/fleet`), with what replaced them.
const RETIRED: &[(&str, &str)] = &[("fleet", "the fleet identity is ~/.makepad/fleet now (tools/fleet-identity.md); delete this line")];

/// The sync config in `text` (read from `path`, for messages), and what in it was skipped: a
/// retired or unknown setting is reported, never a reason to refuse the whole file.
pub fn parse(text: &str, path: &Path) -> Result<(SyncConfig, Vec<String>), String> {
    let mut skipped = Vec::new();
    let mut listen = None;
    let mut peers = Vec::new();
    let mut exclude: Option<Vec<String>> = None;
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once(char::is_whitespace).map(|(k, v)| (k, v.trim())).unwrap_or((line, ""));
        let at = || format!("{}:{}", path.display(), n + 1);
        match key {
            "listen" => listen = Some(value.parse().map_err(|_| format!("{}: listen wants ip:port, got '{value}'", at()))?),
            "peer" => {
                value.parse::<std::net::SocketAddr>().map_err(|_| format!("{}: peer wants ip:port, got '{value}'", at()))?;
                peers.push(value.to_string());
            }
            "exclude" => exclude.get_or_insert_with(Vec::new).push(value.to_string()),
            other => match RETIRED.iter().find(|r| r.0 == other) {
                Some((_, instead)) => skipped.push(format!("{}: '{other}' is a retired setting, ignored: {instead}", at())),
                None => skipped.push(format!("{}: unknown setting '{other}', ignored (known: listen, peer, exclude)", at())),
            },
        }
    }
    let cfg = SyncConfig {
        listen: listen.ok_or_else(|| format!("{}: no listen line", path.display()))?,
        peers,
        exclude: exclude.unwrap_or_else(|| DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect()),
    };
    Ok((cfg, skipped))
}

/// Prints a settings problem once per process, as a `config error:` line (devhub turns the
/// line in a JIT's log into a `config` event on its stream).
fn report_once(what: &str) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.iter().any(|s| s == what) {
        seen.push(what.to_string());
        eprintln!("config error: {what}");
    }
}

/// The app's sync config: `None` when it has no file (sync off).
pub fn load(prefix: &str, app: &str) -> Result<Option<SyncConfig>, String> {
    let Some(path) = sync_file(prefix, app) else { return Ok(None) };
    let r = match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text, &path).map(|(cfg, skipped)| {
            skipped.iter().for_each(|s| report_once(s));
            Some(cfg)
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    };
    // a file that cannot work at all (no listen line, a bad address) is said once too; the
    // caller's retries then fail with the same message, quietly
    if let Err(e) = &r {
        report_once(e);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_file_parses() {
        let (cfg, skipped) = parse(
            "# fleet\nlisten 192.0.2.10:8795\npeer 192.0.2.11:8795 # main\npeer 192.0.2.12:8795\n",
            Path::new("store-sync"),
        )
        .unwrap();
        assert!(skipped.is_empty());
        assert_eq!(cfg.listen, "192.0.2.10:8795".parse().unwrap());
        assert_eq!(cfg.peers, vec!["192.0.2.11:8795", "192.0.2.12:8795"]);
        assert_eq!(cfg.exclude, vec!["gen/built/"]);
        assert!(parse("peer 192.0.2.1:1\n", Path::new("x")).unwrap_err().contains("no listen"));
        assert!(parse("listen nowhere\n", Path::new("x")).is_err());
        // a retired or unknown setting is skipped and said, the rest works
        let (cfg, skipped) = parse("listen 192.0.2.1:1\nfleet /srv/storesync/fleet\nbeacon on\n", Path::new("x")).unwrap();
        assert_eq!(cfg.listen, "192.0.2.1:1".parse().unwrap());
        assert_eq!(skipped.len(), 2);
        assert!(skipped[0].contains("x:2: 'fleet' is a retired setting") && skipped[1].contains("unknown setting 'beacon'"));
    }
}
