//! The firmware network: which projects' twins may reach the real servers their benches name, and
//! the tokens those servers take (main 11-19). Kept on this machine and never in a project, since
//! projects are shared and committed:
//!
//! - A bench names each connection's endpoint and its address. The twin plays a fake server for it
//!   unless the person turned the real network on for that project.
//! - Turning it on approves the hosts the project names at that moment, with the time. A host the
//!   project names later stays played until the person turns it on again; off needs no
//!   confirmation.
//! - A token belongs to an endpoint name and to the host it was saved for. Fab sends it only to that
//!   host, so a shared project cannot send a person's token elsewhere by naming its endpoint so,
//!   and only over TLS unless the host is this machine: a project that names a plain `ws://`
//!   address cannot have a token sent in the clear ([`TokenFor::Plaintext`]).
//!
//! `~/.makepad/fab/firmware_net.prefs` (or `$FAB_FIRMWARE_NET`): one block per project, opened by
//! its `project=` line. `~/.makepad/fab/firmware_net.tokens` (or `$FAB_FIRMWARE_TOKENS`): one
//! `name host token` line each, owner-only (0600), checked on the opened file before a byte is read.
//! A token never formats ([`Secret`]) and never reaches a log. A lane's test instance and a unit
//! test read no file and keep none.
//!
//! Cost: a load reads one small file; a token is read when a connection opens, off the UI thread.

use std::path::{Path, PathBuf};

/// One project's choice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectNet {
    pub on: bool,
    /// The hosts approved when it was last turned on.
    pub hosts: Vec<String>,
    /// When it was last turned on (Unix seconds).
    pub accepted: Option<i64>,
    /// When the twin last opened a connection to a real server (Unix seconds).
    pub last_used: Option<i64>,
}

/// Every project's choice on this machine, by the project's file path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirmwareNet {
    pub projects: Vec<(String, ProjectNet)>,
}

impl FirmwareNet {
    pub fn get(&self, project: &str) -> Option<&ProjectNet> {
        self.projects.iter().find(|(p, _)| p == project).map(|(_, net)| net)
    }

    fn entry(&mut self, project: &str) -> &mut ProjectNet {
        let k = match self.projects.iter().position(|(p, _)| p == project) {
            Some(k) => k,
            None => {
                self.projects.push((project.to_string(), ProjectNet::default()));
                self.projects.len() - 1
            }
        };
        &mut self.projects[k].1
    }

    /// Whether the project's twin may reach `host`: the project is on and the host was approved.
    pub fn allowed(&self, project: &str, host: &str) -> bool {
        self.get(project).is_some_and(|net| net.on && net.hosts.iter().any(|h| h.eq_ignore_ascii_case(host)))
    }

    /// The hosts of `named` (what the project's benches name now) not approved for it.
    pub fn unapproved<'a>(&self, project: &str, named: &'a [String]) -> Vec<&'a str> {
        let approved = self.get(project).map_or(&[][..], |net| &net.hosts[..]);
        named.iter().filter(|h| !approved.iter().any(|a| a.eq_ignore_ascii_case(h))).map(String::as_str).collect()
    }

    /// The person turned the real network on for `project` at `now`, approving the hosts it names.
    pub fn turn_on(&mut self, project: &str, named: &[String], now: i64) {
        let net = self.entry(project);
        net.on = true;
        net.hosts = named.to_vec();
        net.hosts.sort();
        net.hosts.dedup();
        net.accepted = Some(now);
    }

    /// Off: the twin plays every endpoint. The approval is kept to show when it was given.
    pub fn turn_off(&mut self, project: &str) {
        if let Some((_, net)) = self.projects.iter_mut().find(|(p, _)| p == project) { net.on = false; }
    }

    pub fn used(&mut self, project: &str, now: i64) { self.entry(project).last_used = Some(now); }

    fn text(&self) -> String {
        let mut out = String::new();
        for (project, net) in &self.projects {
            out.push_str(&format!("project={}\non={}\n", project.replace('\n', " "), if net.on { 1 } else { 0 }));
            if !net.hosts.is_empty() { out.push_str(&format!("hosts={}\n", net.hosts.join(" "))); }
            if let Some(at) = net.accepted { out.push_str(&format!("accepted={at}\n")); }
            if let Some(at) = net.last_used { out.push_str(&format!("used={at}\n")); }
        }
        out
    }

    /// The choice in `text`. A project reads as on only with an approval time beside it.
    fn parse(text: &str) -> FirmwareNet {
        let mut out = FirmwareNet::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            if key == "project" {
                if !value.is_empty() { out.projects.push((value.to_string(), ProjectNet::default())); }
                continue;
            }
            let Some((_, net)) = out.projects.last_mut() else { continue };
            match key {
                "on" => net.on = value == "1",
                "hosts" => net.hosts = value.split_whitespace().map(str::to_string).collect(),
                "accepted" => net.accepted = value.parse().ok(),
                "used" => net.last_used = value.parse().ok(),
                _ => {}
            }
        }
        for (_, net) in &mut out.projects {
            net.on &= net.accepted.is_some();
        }
        out
    }
}

fn file(var: &str, name: &str) -> Option<PathBuf> {
    if cfg!(test) || cfg!(target_arch = "wasm32") {
        return None;
    }
    if let Some(p) = std::env::var_os(var) {
        return Some(PathBuf::from(p));
    }
    if std::env::var_os("MAKEPAD_TEST_LANE").is_some() {
        return None;
    }
    Some(makepad_fab_settings::home::user_home().join(".makepad").join("fab").join(name))
}

/// This machine's choices.
pub fn load() -> FirmwareNet {
    file("FAB_FIRMWARE_NET", "firmware_net.prefs").and_then(|p| std::fs::read_to_string(p).ok()).map_or_else(FirmwareNet::default, |t| FirmwareNet::parse(&t))
}

/// Keep the choices for the next start. A failed write is logged once per run; the choice holds
/// for this run.
pub fn save(net: &FirmwareNet) {
    let Some(p) = file("FAB_FIRMWARE_NET", "firmware_net.prefs") else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, net.text()) {
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            makepad_widgets_core::log!("fab: could not keep the firmware network choices in {}: {e}", p.display());
        }
    }
}

/// Credential bytes: never formatted, wiped when dropped.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Secret { Secret(bytes) }
    pub fn bytes(&self) -> &[u8] { &self.0 }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("***") }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// A saved token's name and host: what the settings show, never the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenEntry {
    pub name: String,
    pub host: String,
}

/// The token for an endpoint.
#[derive(Debug)]
pub enum TokenFor {
    /// None saved under its name: the connection goes without one.
    None,
    Token(Secret),
    /// The token saved under its name was given for this other host: it is not sent.
    OtherHost(String),
    /// A token is saved for it, but the connection is not encrypted and the host is not this
    /// machine: it is not sent.
    Plaintext,
}

/// An endpoint name: letters, digits, `-` and `_`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_host(host: &str) -> bool {
    !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b':' || b == b'[' || b == b']')
}

/// Each `name host token` line of a tokens file, as byte ranges into it (the token's bytes are
/// never copied here). Lines that are blank or `#` comments are skipped; a malformed line is an
/// error naming its number only.
fn lines(text: &[u8]) -> Result<Vec<(&str, &str, std::ops::Range<usize>)>, String> {
    let mut out = Vec::new();
    for (number, line) in text.split(|b| *b == b'\n').enumerate() {
        let trimmed = line.trim_ascii();
        if trimmed.is_empty() || trimmed[0] == b'#' { continue; }
        let mut fields = trimmed.splitn(3, |b| *b == b' ');
        let name = fields.next().and_then(|f| std::str::from_utf8(f).ok()).filter(|n| valid_name(n));
        let host = fields.next().and_then(|f| std::str::from_utf8(f).ok()).filter(|h| valid_host(h));
        let token = fields.next().map(<[u8]>::trim_ascii).filter(|t| !t.is_empty());
        let (Some(name), Some(host), Some(token)) = (name, host, token) else {
            return Err(format!("line {} of the firmware network tokens is not `name host token`", number + 1));
        };
        let offset = token.as_ptr() as usize - text.as_ptr() as usize;
        out.push((name, host, offset..offset + token.len()));
    }
    Ok(out)
}

fn token_in(text: &[u8], name: &str, host: &str, encrypted: bool) -> Result<TokenFor, String> {
    let lines = lines(text)?;
    let Some((_, saved_host, range)) = lines.iter().rev().find(|(n, _, _)| *n == name) else { return Ok(TokenFor::None) };
    if !saved_host.eq_ignore_ascii_case(host) {
        return Ok(TokenFor::OtherHost(saved_host.to_string()));
    }
    if !encrypted && !fab_elec_bench::net::loopback(host) {
        return Ok(TokenFor::Plaintext);
    }
    Ok(TokenFor::Token(Secret::new(text[range.clone()].to_vec())))
}

/// The whole file, only if it is a regular file only its owner can read or write.
#[cfg(all(unix, not(target_arch = "wasm32")))]
fn read_private(path: &Path) -> Result<Option<Secret>, String> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot open the firmware network tokens".into()),
    };
    let metadata = file.metadata().map_err(|_| "cannot check the firmware network tokens' permissions")?;
    if !metadata.is_file() { return Err("the firmware network tokens are not a regular file".into()); }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("the firmware network tokens must be private (mode 0600)".into());
    }
    let mut buffer = Secret::new(Vec::new());
    file.read_to_end(&mut buffer.0).map_err(|_| "cannot read the firmware network tokens")?;
    Ok(Some(buffer))
}

#[cfg(not(all(unix, not(target_arch = "wasm32"))))]
fn read_private(_path: &Path) -> Result<Option<Secret>, String> {
    Err("private token files cannot be checked on this platform".into())
}

/// The token saved for endpoint `name`, given only if it was saved for `host`, and only for an
/// `encrypted` connection unless `host` is this machine.
pub fn token(name: &str, host: &str, encrypted: bool) -> Result<TokenFor, String> {
    token_at(file("FAB_FIRMWARE_TOKENS", "firmware_net.tokens").as_deref(), name, host, encrypted)
}

fn token_at(path: Option<&Path>, name: &str, host: &str, encrypted: bool) -> Result<TokenFor, String> {
    let Some(path) = path else { return Ok(TokenFor::None) };
    match read_private(path)? {
        Some(text) => token_in(text.bytes(), name, host, encrypted),
        None => Ok(TokenFor::None),
    }
}

/// The saved tokens' names and hosts.
pub fn tokens() -> Result<Vec<TokenEntry>, String> {
    tokens_at(file("FAB_FIRMWARE_TOKENS", "firmware_net.tokens").as_deref())
}

fn tokens_at(path: Option<&Path>) -> Result<Vec<TokenEntry>, String> {
    let Some(path) = path else { return Ok(Vec::new()) };
    let Some(text) = read_private(path)? else { return Ok(Vec::new()) };
    let mut out: Vec<TokenEntry> = Vec::new();
    for (name, host, _) in lines(text.bytes())? {
        out.retain(|e| e.name != name);
        out.push(TokenEntry { name: name.into(), host: host.into() });
    }
    Ok(out)
}

/// Saves `token` for endpoint `name` on `host` (replacing any token of that name), or with None
/// removes it. The file is written whole to a new owner-only file and moved over the old one.
pub fn set_token(name: &str, host: &str, token: Option<Secret>) -> Result<(), String> {
    let path = file("FAB_FIRMWARE_TOKENS", "firmware_net.tokens").ok_or("tokens are not kept in a test instance")?;
    set_token_at(&path, name, host, token)
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
fn set_token_at(path: &Path, name: &str, host: &str, token: Option<Secret>) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_name(name) { return Err("an endpoint name is letters, digits, - and _".into()); }
    if !valid_host(host) { return Err("not a host name or address".into()); }
    if let Some(token) = &token {
        if token.bytes().is_empty() || token.bytes().iter().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
            return Err("a token is one word of printable characters".into());
        }
    }
    let old = read_private(path)?;
    let mut text = Secret::new(Vec::new());
    if let Some(old) = &old {
        for (n, h, range) in lines(old.bytes())? {
            if n == name { continue; }
            text.0.extend_from_slice(format!("{n} {h} ").as_bytes());
            text.0.extend_from_slice(&old.bytes()[range]);
            text.0.push(b'\n');
        }
    }
    if let Some(token) = &token {
        text.0.extend_from_slice(format!("{name} {host} ").as_bytes());
        text.0.extend_from_slice(token.bytes());
        text.0.push(b'\n');
    }
    let dir = path.parent().ok_or("the tokens file has no folder")?;
    std::fs::create_dir_all(dir).map_err(|_| "cannot make the folder for the firmware network tokens")?;
    let staged = dir.join(format!(".firmware_net.tokens.{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&staged)
        .map_err(|_| "cannot write the firmware network tokens")?;
    file.write_all(text.bytes()).and_then(|()| file.sync_all()).map_err(|_| "cannot write the firmware network tokens")?;
    drop(file);
    std::fs::rename(&staged, path).map_err(|_| "cannot replace the firmware network tokens".into())
}

#[cfg(not(all(unix, not(target_arch = "wasm32"))))]
fn set_token_at(_path: &Path, _name: &str, _host: &str, _token: Option<Secret>) -> Result<(), String> {
    Err("private token files cannot be checked on this platform".into())
}

/// The licences page's entry: what the twin may reach, for which projects, and how tokens are kept.
pub fn resource(net: &FirmwareNet, tokens: &Result<Vec<TokenEntry>, String>) -> crate::model_sources::Resource {
    use crate::model_sources::date;
    let mut lines = vec![
        "Fab's twin plays a fake server for each connection a bench names. It reaches a real server only for a project you turned the real network on for, and only the hosts that project named when you did.".to_string(),
        "Every connection is logged (the endpoint, its address and how it ended), never a token or a message.".to_string(),
    ];
    let on: Vec<_> = net.projects.iter().filter(|(_, p)| p.on).collect();
    if on.is_empty() {
        lines.push("Off for every project.".into());
    }
    for (project, p) in on {
        let name = Path::new(project).file_name().and_then(|n| n.to_str()).unwrap_or(project);
        let since = p.accepted.map_or(String::new(), |at| format!(" since {}", date(at)));
        let used = p.last_used.map_or_else(|| " Not used yet.".to_string(), |at| format!(" Last used {}.", date(at)));
        lines.push(format!("On for {name}{since}: {}.{used}", if p.hosts.is_empty() { "no hosts".to_string() } else { p.hosts.join(", ") }));
    }
    lines.push(match tokens {
        Err(problem) => format!("Tokens: none is sent, because {problem}."),
        Ok(tokens) if tokens.is_empty() => "Tokens: none saved. A token is kept in your Fab settings on this computer, never in a project, and sent only to the host it was saved for.".into(),
        Ok(tokens) => format!("Tokens, kept in your Fab settings on this computer and sent only to the host each was saved for: {}.",
            tokens.iter().map(|t| format!("{} for {}", t.name, t.host)).collect::<Vec<_>>().join(", ")),
    });
    crate::model_sources::Resource { title: "Firmware network".into(), lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_follow_the_named_hosts_and_the_kept_file() {
        let project = "/projects/radio.fab";
        let named = vec!["chat.example.org".to_string(), "api.example.org".to_string()];
        let mut net = FirmwareNet::default();
        assert!(!net.allowed(project, "chat.example.org"), "off until the person turns it on");
        net.turn_on(project, &named, 1_795_000_000);
        assert!(net.allowed(project, "chat.example.org") && net.allowed(project, "API.example.org"), "on: the hosts named then");
        let later = vec!["chat.example.org".to_string(), "elsewhere.example.net".to_string()];
        assert!(!net.allowed(project, "elsewhere.example.net"), "a host named later is not approved");
        assert_eq!(net.unapproved(project, &later), ["elsewhere.example.net"], "and is shown as such");
        assert!(!net.allowed("/projects/other.fab", "chat.example.org"), "another project is not on");
        net.used(project, 1_795_000_100);
        net.turn_off(project);
        assert!(!net.allowed(project, "chat.example.org"), "off plays every endpoint");
        assert_eq!(net.get(project).and_then(|p| p.accepted), Some(1_795_000_000), "the approval's time is kept");
        net.turn_on(project, &later, 1_795_000_200);
        assert_eq!(FirmwareNet::parse(&net.text()), net, "the kept file reads back as written");
        assert!(!FirmwareNet::parse("project=/p.fab\non=1\nhosts=a.example\n").allowed("/p.fab", "a.example"),
            "a file saying on without an approval time reads as off");
        assert_eq!(load(), FirmwareNet::default(), "a unit test reads no machine file");

        let page = resource(&net, &Ok(vec![TokenEntry { name: "chat".into(), host: "chat.example.org".into() }])).lines.join("\n");
        assert!(page.contains("On for radio.fab since 2026-11-18: chat.example.org, elsewhere.example.net. Last used 2026-11-18."), "{page}");
        assert!(page.contains("chat for chat.example.org") && page.contains("never a token"), "{page}");
        assert!(resource(&FirmwareNet::default(), &Ok(Vec::new())).lines.join("\n").contains("Off for every project"));
        assert!(resource(&net, &Err("the tokens file must be private (mode 0600)".into())).lines.join("\n").contains("none is sent, because the tokens file must be private"));
    }

    #[test]
    fn tokens_go_only_to_their_host_encrypted_and_only_from_a_private_file() {
        let text = b"# saved by Fab\nchat chat.example.org s3cr3t-ab\napi api.example.org k3y\ndev localhost d3v\n";
        assert!(matches!(token_in(text, "chat", "chat.example.org", true), Ok(TokenFor::Token(t)) if t.bytes() == b"s3cr3t-ab"), "the token for its host");
        assert!(matches!(token_in(text, "chat", "evil.example.net", true), Ok(TokenFor::OtherHost(h)) if h == "chat.example.org"), "never to another host");
        assert!(matches!(token_in(text, "chat", "chat.example.org", false), Ok(TokenFor::Plaintext)), "never in plain text to another machine");
        assert!(matches!(token_in(text, "dev", "localhost", false), Ok(TokenFor::Token(t)) if t.bytes() == b"d3v"), "in plain text only to this machine");
        assert!(matches!(token_in(text, "nope", "x.example", false), Ok(TokenFor::None)), "an endpoint without one");
        let bad = token_in(b"chat chat.example.org\n", "chat", "chat.example.org", true).unwrap_err();
        assert!(bad.contains("line 1") && !bad.contains("chat.example"), "a bad line is named by number only: {bad}");
        let secret = Secret::new(b"s3cr3t-ab".to_vec());
        assert_eq!(format!("{secret:?}"), "***", "a token never formats");

        // The file: written owner-only, read only while it stays so.
        let dir = std::env::temp_dir().join(format!("fab-firmware-net-{}", std::process::id()));
        let path = dir.join("firmware_net.tokens");
        set_token_at(&path, "chat", "chat.example.org", Some(Secret::new(b"one".to_vec()))).unwrap();
        set_token_at(&path, "api", "api.example.org", Some(Secret::new(b"two".to_vec()))).unwrap();
        set_token_at(&path, "chat", "chat.example.org", Some(Secret::new(b"three".to_vec()))).unwrap();
        assert!(matches!(token_at(Some(&path), "chat", "chat.example.org", true), Ok(TokenFor::Token(t)) if t.bytes() == b"three"), "the newest replaces");
        assert_eq!(tokens_at(Some(&path)).unwrap().iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["api", "chat"], "names and hosts listed");
        set_token_at(&path, "api", "api.example.org", None).unwrap();
        assert!(matches!(token_at(Some(&path), "api", "api.example.org", true), Ok(TokenFor::None)), "removed");
        assert!(set_token_at(&path, "chat", "chat.example.org", Some(Secret::new(b"two words".to_vec()))).is_err(), "one word only");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "owner-only");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(token_at(Some(&path), "chat", "chat.example.org", true).unwrap_err().contains("must be private"), "a readable file gives nothing");
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(token("chat", "chat.example.org", true), Ok(TokenFor::None)), "a unit test reads no machine file");
    }
}
