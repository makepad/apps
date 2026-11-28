//! One instance owns a home. `local/` holds its lock, device key, AI acknowledgement and
//! discovery endpoint. `local/peers` remembers linked devices and towns. `local/home` lists
//! the local stores this home expects. Only `town/` is shared: the signed log and
//! content-addressed blobs. `checkpoints/` and `index/` are rebuildable from the shared log.

mod computer_label;

use crate::{event::TownId, fold::FinalClosure, home_writer::{HomeWriter, PrivateFiles}, key::PublicKey};
mod lock;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::num::NonZeroU16;
use std::sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, Ordering}};

const POLICY_SET_ASIDE_PREFIX: &str = "town.before-policy-";

fn strict_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    if s.len() != N * 2 { return None }
    let mut out = [0; N];
    for (byte, pair) in out.iter_mut().zip(s.as_bytes().chunks_exact(2)) {
        let &[hi, lo] = pair else { return None };
        *byte = nibble(hi)? << 4 | nibble(lo)?;
    }
    Some(out)
}

pub fn is_policy_set_aside(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(POLICY_SET_ASIDE_PREFIX))
}

pub struct Home {
    pub dir: PathBuf,
    /// Held for the life of the instance: the OS releases it when the process ends. It is a
    /// process lock, so a child between fork and exec never keeps it.
    _lock: lock::HomeLock,
    files: std::sync::OnceLock<PrivateFiles>,
    port_holder: Mutex<bool>,
    port_available: Condvar,
}

/// The worker drops its socket before releasing this lease.
pub struct LinkPortLease(Arc<Home>);

impl LinkPortLease {
    pub fn port(&self) -> io::Result<Option<u16>> { self.0.link_port() }

    pub fn remember(&self, port: NonZeroU16) -> io::Result<()> {
        self.0.files().replace(&self.0.dir.join("local/link-port"), format!("{port}\n").into_bytes())
    }
}

impl Drop for LinkPortLease {
    fn drop(&mut self) {
        *self.0.port_holder.lock().unwrap() = false;
        self.0.port_available.notify_all();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeSource { Explicit, TestHome, User }

#[derive(Debug)]
pub enum HomeError {
    /// Another Town holds this home.
    InUse(PathBuf),
    /// The home is inside a git work tree, where its key and log could be committed or pushed.
    InsideRepository { home: PathBuf, repository: PathBuf },
    Io(io::Error),
}

impl std::fmt::Display for HomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HomeError::InUse(dir) => write!(f, "another Town is using {}", dir.display()),
            HomeError::InsideRepository { home, repository } => write!(
                f,
                "{} is inside the git work tree {}: the device key and log could be committed or pushed from there",
                home.display(),
                repository.display()
            ),
            HomeError::Io(e) => e.fmt(f),
        }
    }
}

impl From<io::Error> for HomeError {
    fn from(e: io::Error) -> Self {
        HomeError::Io(e)
    }
}

impl Home {
    /// `MAKEPAD_CREATE_HOME`, else `.makepad/create` under the test home or the user's home.
    pub fn default_dir() -> Option<PathBuf> {
        Self::default_dir_source().map(|(dir, _)| dir)
    }

    pub fn default_dir_source() -> Option<(PathBuf, HomeSource)> {
        let explicit = std::env::var_os("MAKEPAD_CREATE_HOME").map(PathBuf::from);
        let override_set = explicit.is_some();
        if let Some(dir) = explicit.filter(|d| d.is_absolute()) {
            return Some((dir, HomeSource::Explicit));
        }
        let (home, source) = std::env::var_os("MAKEPAD_TEST_HOME").map(PathBuf::from).filter(|d| d.is_absolute())
            .map(|dir| (dir, HomeSource::TestHome)).or_else(|| std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE")).map(|dir| (dir.into(), HomeSource::User)))?;
        // Preserve path fallback for an invalid explicit override, without granting test consent.
        Some((home.join(".makepad").join("create"), if override_set { HomeSource::Explicit } else { source }))
    }

    /// Opens the home with private directories, or says another instance holds it. A home inside a
    /// git work tree is refused.
    pub fn open(dir: &Path) -> Result<Home, HomeError> {
        let absolute = std::path::absolute(dir)?;
        if let Some(repository) = absolute.ancestors().find(|a| a.join(".git").exists()) {
            return Err(HomeError::InsideRepository { home: dir.to_path_buf(), repository: repository.to_path_buf() });
        }
        private_dir(dir)?;
        private_dir(&dir.join("local"))?;
        // The key lives in `local/`: an unencrypted backup disk never holds it. A volume without
        // extended attributes (FAT, some network shares) can't take the mark, and the home
        // still opens.
        let _ = makepad_network::at_rest::exclude_from_backups(&dir.join("local"));
        // `local/lock` is opened by `lock` alone: closing any descriptor of it drops the lock.
        let lock = match lock::HomeLock::acquire(dir) {
            Ok(lock) => lock,
            Err(lock::LockError::InUse) => return Err(HomeError::InUse(dir.to_path_buf())),
            Err(lock::LockError::Io(e)) => return Err(HomeError::Io(e)),
        };
        let home = Home { dir: dir.to_path_buf(), _lock: lock, files: Default::default(), port_holder: Default::default(), port_available: Default::default() };
        for path in [dir.join("town"), home.towns_dir(), home.blobs_dir(), home.checkpoints_dir(), home.index_dir()] {
            private_dir(&path)?;
        }
        Ok(home)
    }

    pub(crate) fn attach_writer(&self, writer: &Arc<HomeWriter>) {
        self.files.set(PrivateFiles::queued(writer)).expect("one writer per open home");
    }

    pub fn files(&self) -> PrivateFiles { self.files.get().cloned().unwrap_or_default() }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn settle_writer(&self) { self.files().settle(); }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn hold_writer(&self) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) { self.files().hold() }

    /// The caller's stop flag is set before wake_port_waiters is called. Checking it under
    /// the lease mutex prevents losing a stop between the check and the wait.
    pub fn lease_link_port(self: &Arc<Self>, stopped: &AtomicBool) -> Option<LinkPortLease> {
        let held = self.port_holder.lock().unwrap();
        let mut held = self.port_available.wait_while(held, |held| *held && !stopped.load(Ordering::Acquire)).unwrap();
        if stopped.load(Ordering::Acquire) { return None }
        *held = true;
        Some(LinkPortLease(self.clone()))
    }

    pub fn wake_port_waiters(&self) {
        let _guard = self.port_holder.lock().unwrap();
        self.port_available.notify_all();
    }

    pub fn link_port(&self) -> io::Result<Option<u16>> {
        let bytes = match self.files().read(&self.dir.join("local/link-port")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other?,
        };
        let text = std::str::from_utf8(&bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let value = text.trim_end_matches('\n');
        value.parse::<u16>().ok().filter(|p| *p != 0 && value.bytes().all(|b| b.is_ascii_digit()))
            .map(Some).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Town link port"))
    }

    pub fn key_path(&self) -> PathBuf {
        self.dir.join("local/key")
    }
    pub fn peers_path(&self) -> PathBuf {
        self.dir.join("local/peers")
    }
    /// The microphone and speaker last chosen for calls on this computer. Never synced.
    pub fn call_devices_path(&self) -> PathBuf {
        self.dir.join("local/call-devices")
    }
    pub fn record_path(&self) -> PathBuf {
        self.dir.join("local/home")
    }
    pub fn log_path(&self) -> PathBuf {
        self.dir.join("town/log")
    }
    pub fn towns_dir(&self) -> PathBuf {
        self.dir.join("town/towns")
    }
    pub fn town_log_path(&self, town: &crate::event::TownId) -> PathBuf {
        self.towns_dir().join(format!("{}.log", crate::hex(town)))
    }

    pub fn open_town(&self) -> Option<crate::event::TownId> {
        let bytes = self.home_record().ok()?;
        bytes.split(|&b| b == b'\n').rev()
            .find_map(|line| line.strip_prefix(b"open town "))
            .and_then(|id| crate::parse_id(std::str::from_utf8(id).ok()?))
    }

    pub fn set_open_town(&self, town: &crate::event::TownId) -> io::Result<()> {
        if self.open_town().as_ref() == Some(town) { return Ok(()) }
        self.append_home_line(&format!("open town {}", crate::hex(town)))
    }

    /// Local agreement order disambiguates earlier shared copies on reconnect. A town's
    /// signed membership supplies the peer, so the home journal needs only the town id.
    pub fn share_order(&self) -> io::Result<Vec<crate::event::TownId>> {
        let bytes = self.home_record()?;
        Ok(bytes.split(|&b| b == b'\n').filter_map(|line| {
            crate::parse_id(std::str::from_utf8(line.strip_prefix(b"shared town ")?).ok()?)
        }).collect())
    }

    pub fn remember_share(&self, town: &crate::event::TownId) -> io::Result<()> {
        if self.share_order()?.last() == Some(town) { return Ok(()) }
        self.append_home_line(&format!("shared town {}", crate::hex(town)))
    }

    pub fn close_final(&self, town: &TownId, peer: &PublicKey, closure: FinalClosure) -> io::Result<()> {
        let line = match closure.normalized() {
            Some(FinalClosure::Removal(id)) => format!(
                "final exchange closed {} {} {}", crate::hex(town), crate::hex(peer), crate::hex(&id)),
            Some(FinalClosure::Fork([lo, hi])) => format!(
                "final exchange closed {} {} {} {}", crate::hex(town), crate::hex(peer), crate::hex(&lo), crate::hex(&hi)),
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "fork closure requires two distinct events")),
        };
        self.append_home_line(&line)
    }

    pub fn closed_finals(&self) -> io::Result<BTreeMap<(TownId, PublicKey), BTreeSet<FinalClosure>>> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid final exchange closure");
        let record = self.home_record()?;
        let mut out: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for line in record.split(|&b| b == b'\n') {
            if line == b"final exchange closed" { return Err(invalid()) }
            let Some(fields) = line.strip_prefix(b"final exchange closed ") else { continue };
            let text = std::str::from_utf8(fields).map_err(|_| invalid())?;
            let parts: Vec<_> = text.split(' ').collect();
            let (town, peer, closure) = match parts.as_slice() {
                [town, peer] if strict_hex::<32>(town).is_some() && strict_hex::<65>(peer).is_some() => {
                    continue; // An id-less legacy closure allows a fresh exchange.
                }
                [town, peer, id] => (town, peer, FinalClosure::Removal(strict_hex(id).ok_or_else(invalid)?)),
                [town, peer, a, b] => {
                    let closure = FinalClosure::fork(strict_hex(a).ok_or_else(invalid)?, strict_hex(b).ok_or_else(invalid)?);
                    (town, peer, closure.ok_or_else(invalid)?)
                }
                _ => return Err(invalid()),
            };
            let pair = (strict_hex(town).ok_or_else(invalid)?, strict_hex(peer).ok_or_else(invalid)?);
            out.entry(pair).or_default().insert(closure);
        }
        Ok(out)
    }

    pub fn town_checkpoint(&self, town: &crate::event::TownId) -> PathBuf {
        self.checkpoints_dir().join(format!("fold-{}", crate::hex(town)))
    }

    /// Journal the destination before the rename. The home lock serializes writers, and
    /// an interrupted rename can use the same recorded destination on the next open.
    pub fn set_aside_town(&self, town: Option<&crate::event::TownId>, now: u64) -> io::Result<Option<PathBuf>> {
        let (old, prefix) = match town {
            Some(id) => (self.town_log_path(id), format!("{POLICY_SET_ASIDE_PREFIX}{}-", crate::hex(id))),
            None => (self.log_path(), "log.before-towns-".to_owned()),
        };
        if !self.files().exists(&old)? { return Ok(None) }
        let pending = self.town_notices()?.into_iter().rev().find(|path|
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)));
        let destination = match pending {
            Some(path) if !self.files().exists(&path)? => path,
            _ => {
                let base = format!("{prefix}{}", utc_date(now));
                let mut name = base.clone();
                let mut suffix = 2;
                let town = self.log_path();
                while self.files().exists(&town.with_file_name(&name))? {
                    name = format!("{base}-{suffix}");
                    suffix += 1;
                }
                self.append_home_line(&format!("set aside town {now} {name}"))?;
                town.with_file_name(name)
            }
        };
        self.files().rename(&old, &destination)?;
        Ok(Some(destination))
    }

    fn home_record(&self) -> io::Result<Vec<u8>> {
        match self.files().read(&self.record_path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(b"v1 home\n".to_vec()),
            result => result,
        }
    }

    fn append_home_line(&self, line: &str) -> io::Result<()> {
        let mut record = self.home_record()?;
        if !record.ends_with(b"\n") { record.push(b'\n'); }
        record.extend_from_slice(line.as_bytes());
        record.push(b'\n');
        self.files().replace(&self.record_path(), record)
    }

    pub fn town_notices(&self) -> io::Result<Vec<PathBuf>> {
        let record = self.home_record()?;
        let mut pending = Vec::new();
        for line in record.split(|&b| b == b'\n').filter_map(|l| std::str::from_utf8(l).ok()) {
            let Some((fields, name)) = line.rsplit_once(' ') else { continue };
            if !(name.starts_with("log.before-towns-") || is_policy_set_aside(Path::new(name)))
                || name.ends_with(".log") || Path::new(name).file_name() != Some(name.as_ref()) { continue }
            let path = self.log_path().with_file_name(name);
            if fields.strip_prefix("set aside town ").is_some_and(|s| s.parse::<u64>().is_ok()) {
                pending.push(path);
            } else if fields == "told town" {
                pending.retain(|p| *p != path);
            }
        }
        Ok(pending)
    }

    pub fn dismiss_town_notice(&self, path: &Path) -> io::Result<()> {
        let name = path.file_name().and_then(|n| n.to_str()).ok_or(io::ErrorKind::InvalidInput)?;
        self.append_home_line(&format!("told town {name}"))
    }

    pub fn agents_path(&self, town: &crate::event::TownId) -> PathBuf {
        self.dir.join("local").join(format!("agents-{}", crate::hex(town)))
    }

    pub fn acks_path(&self) -> PathBuf {
        self.dir.join("local/ai-acked")
    }
    /// Where this instance publishes its endpoint (`makepad_remote::discovery::publish`).
    pub fn endpoint_path(&self) -> PathBuf {
        self.dir.join("local/endpoint")
    }
    pub fn recent_emoji_path(&self) -> PathBuf { self.dir.join("recent-emoji") }
    pub fn layout_path(&self) -> PathBuf {
        self.dir.join("local/layout")
    }
    pub fn blobs_dir(&self) -> PathBuf {
        self.dir.join("town/blobs")
    }
    pub fn checkpoints_dir(&self) -> PathBuf {
        self.dir.join("checkpoints")
    }
    pub fn index_dir(&self) -> PathBuf {
        self.dir.join("index")
    }
}

// Howard Hinnant's civil_from_days, as in platform's script clock. Day zero is 1970-01-01.
fn civil_date(seconds: u64) -> (i64, i64, i64) {
    let day = (seconds / 86_400) as i64;
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let day_of_era = z.rem_euclid(146097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 { march_month + 3 } else { march_month - 9 };
    let year = era * 400 + year_of_era + i64::from(month <= 2);
    (year, month, day)
}

fn utc_date(seconds: u64) -> String {
    let (year, month, day) = civil_date(seconds);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn day_month(seconds: u64) -> String {
    let (_, month, day) = civil_date(seconds);
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{day} {}", months[month as usize - 1])
}

fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `bytes` to `path` readable by the user only, atomically (beside, then renamed).
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("new");
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_data()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_finals_round_trips_scoped_closures_skips_legacy_lines_and_rejects_malformed() {
        let dir = crate::log::tests::temp_dir("scoped-closures-home");
        let home = Home::open(&dir).unwrap();
        let (t1, p1, t2, p2) = ([1; 32], [2; 65], [3; 32], [4; 65]);
        let (r1, r2, lo, hi) = ([5; 32], [6; 32], [0xab; 32], [0xcd; 32]);
        for id in [r1, r2, r1] { home.close_final(&t1, &p1, FinalClosure::Removal(id)).unwrap(); }
        let removals = BTreeSet::from([FinalClosure::Removal(r1), FinalClosure::Removal(r2)]);
        assert_eq!(home.closed_finals().unwrap(), BTreeMap::from([((t1, p1), removals.clone())]),
            "distinct removals round-trip and a duplicate line counts once");
        home.close_final(&t2, &p2, FinalClosure::Fork([hi, lo])).unwrap();
        let prefix = "final exchange closed";
        let pair = format!("{} {}", crate::hex(&t2), crate::hex(&p2));
        let (low, high) = (crate::hex(&lo), crate::hex(&hi));
        let journal = std::fs::read_to_string(home.record_path()).unwrap();
        assert!(journal.lines().any(|line| line == format!("{prefix} {pair} {low} {high}")),
            "writing reversed siblings puts the lower id first on disk");
        assert_eq!(home.close_final(&t2, &p2, FinalClosure::Fork([hi, hi])).unwrap_err().kind(),
            io::ErrorKind::InvalidInput, "a caller cannot close on two identical siblings");
        home.append_home_line(&format!("{prefix} {} {}", crate::hex(&t1), crate::hex(&p2))).unwrap();
        assert_eq!(home.closed_finals().unwrap(), BTreeMap::from([
            ((t1, p1), removals), ((t2, p2), BTreeSet::from([FinalClosure::Fork([lo, hi])])),
        ]), "a valid legacy line is dropped and all scoped closures survive");

        let bad_lines = [
            prefix.to_owned(), format!("{prefix} {low}"), format!("{prefix} {low} xx"),
            format!("{prefix} {pair} {}", low.to_uppercase()), format!("{prefix} {pair} {}", low.replacen('a', ":", 1)),
            format!("{prefix} {pair} ab"), format!("{prefix} {pair} {low} {low}"),
            format!("{prefix} {pair} {low} {high} {low}"), format!("{prefix} {pair} {low} "),
            format!("{prefix} {pair}  {low}"), format!("{prefix} {pair} {}", "é".repeat(32)),
        ];
        for line in bad_lines.iter().map(String::as_bytes).chain([b"final exchange closed \xff".as_slice()]) {
            std::fs::write(home.record_path(), [journal.as_bytes(), line, b"\n"].concat()).unwrap();
            assert_eq!(home.closed_finals().unwrap_err().kind(), io::ErrorKind::InvalidData,
                "a malformed closure fails the whole journal read: {:?}", String::from_utf8_lossy(line));
        }
        drop(home);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_blocked_journal_preserves_peer_backups_and_precedes_the_archive_move() {
        use crate::{key::DeviceKey, peers::{How, Peers}};
        let key = DeviceKey::generate().unwrap().public();
        let dir = std::env::temp_dir().join(format!("town-journal-{}", crate::hex(&key[1..9])));
        let home = Arc::new(Home::open(&dir).unwrap());
        let writer = HomeWriter::start(home.clone(), || {}).unwrap();
        let blocked = home.record_path().with_extension("new");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(home.log_path(), b"earlier town").unwrap();
        let mut peers = Peers::open(&home).unwrap();
        peers.linked(key, How::Words, 3, "Ben", 1).unwrap();
        let old_peers = home.files().read(&home.peers_path()).unwrap();
        Peers::forget(&home, 2).unwrap();
        let archive = home.set_aside_town(None, 3).unwrap().unwrap();
        assert_eq!(home.town_notices().unwrap(), [archive.clone()], "the queued journal names the archive");
        assert!(Peers::open(&home).unwrap().devices().is_empty(), "the reset is visible before it reaches disk");
        loop {
            let generation = writer.wake_gen();
            if writer.tick(false, |_| {}).is_some() { break }
            writer.wait_wake(generation);
        }
        assert!(home.log_path().is_file() && !archive.exists(), "the failed journal blocks the rename");
        assert_eq!(home.files().read_capped(&archive, 7).unwrap(), b"earlier", "the pending archive reads its source with the requested cap");
        let backup = home.peers_path().with_file_name("peers.unreadable-2");
        assert_eq!(home.files().read(&backup).unwrap(), old_peers, "forget preserves the last queued peer snapshot");
        std::fs::remove_dir(blocked).unwrap();
        writer.tick(true, |_| {});
        home.settle_writer();
        assert_eq!(std::fs::read(archive).unwrap(), b"earlier town", "the archived log survives the move");
        assert_eq!(std::fs::read(backup).unwrap(), old_peers, "the peer backup reaches disk intact");
        let journal = std::fs::read_to_string(home.record_path()).unwrap();
        assert!(journal.starts_with("v1 home\npeers\nforgot peers 2 peers.unreadable-2\nset aside town 3 "), "the journal preserves operation order");
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(home.peers_path()).unwrap().permissions().mode() & 0o777, 0o600, "the replaced peer store stays private");
        }
        drop((peers, writer, home));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_home_separates_local_shared_and_rebuildable_files() {
        let dir = std::env::temp_dir().join(format!("makepad-create-home-{}", std::process::id()));
        // This is the only core test that reads or sets these overrides. Restore before asserting.
        let overrides = ["MAKEPAD_CREATE_HOME", "MAKEPAD_TEST_HOME"].map(|key| (key, std::env::var_os(key)));
        std::env::remove_var("MAKEPAD_CREATE_HOME");
        std::env::set_var("MAKEPAD_TEST_HOME", &dir);
        let test_home = Home::default_dir();
        let explicit = dir.join("explicit");
        std::env::set_var("MAKEPAD_CREATE_HOME", &explicit);
        let create_home = Home::default_dir();
        for (key, old) in overrides {
            if let Some(value) = old { std::env::set_var(key, value); } else { std::env::remove_var(key); }
        }
        assert_eq!(test_home, Some(dir.join(".makepad").join("create")), "the test session owns Town's default home");
        assert_eq!(create_home, Some(explicit), "an explicit Town home takes precedence over the test home");
        let home = Home::open(&dir).unwrap();
        assert_eq!(home.key_path(), dir.join("local/key"), "the signing key is private");
        assert_eq!(home.acks_path(), dir.join("local/ai-acked"), "the acknowledgement is local");
        assert_eq!(home.endpoint_path(), dir.join("local/endpoint"), "discovery is local");
        assert_eq!(home.log_path(), dir.join("town/log"), "the pre-town log can still be located");
        assert_eq!(home.towns_dir(), dir.join("town/towns"), "towns have their own directory");
        assert_eq!(home.town_log_path(&[0xab; 32]), home.towns_dir().join(format!("{}.log", "ab".repeat(32))),
            "the town id is the full lower-case hex filename");
        assert_eq!(home.blobs_dir(), dir.join("town/blobs"), "blobs live beside the log");
        assert_eq!(home.checkpoints_dir(), dir.join("checkpoints"), "checkpoints have their own directory");
        assert_eq!(home.index_dir(), dir.join("index"), "indexes have their own directory");
        for path in [&dir, &dir.join("local"), &dir.join("town"), &home.towns_dir(), &home.blobs_dir(), &home.checkpoints_dir(), &home.index_dir()] {
            let meta = std::fs::metadata(path).unwrap();
            assert!(meta.is_dir(), "opening the home makes {path:?}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(meta.permissions().mode() & 0o777, 0o700, "{path:?} is private");
            }
        }
        let label_path = dir.join("local/computer-label");
        assert!(!label_path.exists(), "opening an existing or new home does not need its computer label");
        let label = home.computer_label().unwrap();
        assert!(!label.trim().is_empty() && !label.contains('.'), "the first use selects a short local label");
        assert_eq!(std::fs::read_to_string(label_path).unwrap(), label, "the first use saves the setting locally");
        let first = home.log_path().with_file_name("log.before-towns-2000-02-29");
        std::fs::write(&first, b"keep").unwrap();
        std::fs::write(home.log_path(), b"old").unwrap();
        let aside = home.set_aside_town(None, 951782400).unwrap().unwrap();
        assert_eq!(aside, first.with_file_name("log.before-towns-2000-02-29-2"), "a UTC leap day and an occupied backup name");
        assert_eq!(std::fs::read(first).unwrap(), b"keep", "the first backup survived the collision");
        home.dismiss_town_notice(&aside).unwrap();
        std::fs::write(home.log_path(), b"next").unwrap();
        home.append_home_line("set aside town 951782400 log.before-towns-2000-02-29-3").unwrap();
        let resumed = home.set_aside_town(None, 951868800).unwrap().unwrap();
        assert_eq!(home.town_notices().unwrap(), [resumed.clone()], "a journaled rename is resumed exactly once across midnight");
        assert_eq!(std::fs::read(resumed).unwrap(), b"next", "the interrupted move kept its original destination");
        drop(home);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_device_key_lives_in_the_home_and_is_protected() {
        use crate::key::DeviceKey;
        let root = std::env::temp_dir().join(format!("makepad-create-key-{}", std::process::id()));
        let dir = root.join("home");
        let home = Home::open(&dir).unwrap();
        let path = home.key_path();
        assert_eq!(path, dir.join("local/key"), "the key's path comes from the home");
        let (key, level) = DeviceKey::load_or_create(&path).unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "the key file is private");
        }
        if cfg!(windows) {
            assert_eq!(level, makepad_network::at_rest::Protection::OsBound, "Windows reports OS binding");
            assert_ne!(&on_disk[..65], &key.public()[..], "the file is not the raw key");
        } else {
            assert_eq!(level, makepad_network::at_rest::Protection::FileOnly, "no OS protection is claimed");
        }
        let (again, _) = DeviceKey::load_or_create(&path).unwrap();
        assert_eq!(again.public(), key.public(), "the key loads back");
        #[cfg(target_vendor = "apple")]
        {
            extern "C" {
                fn getxattr(path: *const std::ffi::c_char, name: *const std::ffi::c_char, value: *mut std::ffi::c_void, size: usize, position: u32, options: i32) -> isize;
            }
            let local = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(dir.join("local").as_os_str())).unwrap();
            let name = std::ffi::CString::new("com.apple.metadata:com_apple_backup_excludeItem").unwrap();
            let size = unsafe { getxattr(local.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
            assert!(size > 0, "local/ is excluded from backups");
        }
        drop(home);

        // A torn key file is an error and is left as it was, never replaced by a new key.
        std::fs::write(&path, [1u8; 10]).unwrap();
        assert!(DeviceKey::load_or_create(&path).is_err(), "a torn key file is an error");
        assert_eq!(std::fs::read(&path).unwrap(), [1u8; 10], "the torn file is left unchanged");

        // A home inside a git work tree is refused.
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        match Home::open(&root.join("repo/deep/home")) {
            Err(HomeError::InsideRepository { .. }) => {}
            other => panic!("a home inside a work tree is refused, got {:?}", other.map(|_| ())),
        }
        assert!(!root.join("repo/deep").exists(), "a refused home makes no directories");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A child between fork and exec holds a copy of every descriptor of its parent. The home
    /// lock must not be one the child keeps alive: the home reopens while the child is parked.
    #[cfg(unix)]
    #[test]
    fn a_child_parked_before_exec_does_not_hold_the_home() {
        use std::io::{Read, Write};
        use std::os::unix::io::{AsRawFd, FromRawFd};
        use std::os::unix::process::CommandExt;
        let dir = std::env::temp_dir().join(format!("makepad-create-forked-{}", std::process::id()));
        let home = Home::open(&dir).unwrap();
        let (mut forked_read, forked_write) = std::io::pipe().unwrap();
        let (go_read, mut go_write) = std::io::pipe().unwrap();
        let (forked_fd, go_fd) = (forked_write.as_raw_fd(), go_read.as_raw_fd());
        let spawner = std::thread::spawn(move || {
            let mut command = std::process::Command::new("/usr/bin/true");
            // Runs in the child between fork and exec: one plain write and one plain read, no
            // allocation. The files only borrow the pipe ends and are never closed here.
            unsafe {
                command.pre_exec(move || {
                    let forked = std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(forked_fd));
                    let go = std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(go_fd));
                    (&*forked).write(&[1])?;
                    (&*go).read(&mut [0u8; 1])?;
                    Ok(())
                });
            }
            command.status()
        });
        let mut byte = [0u8; 1];
        forked_read.read_exact(&mut byte).unwrap();
        drop(home);
        let reopened = Home::open(&dir);
        go_write.write_all(&[1]).unwrap();
        let status = spawner.join().unwrap();
        let reopened_ok = reopened.is_ok();
        drop(reopened);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(status.unwrap().success(), "the parked child ran to its exec");
        assert!(reopened_ok, "a home reopens while a forked child that holds copies of its descriptors is parked before exec");
    }

    #[test]
    fn a_home_is_held_once_per_process_through_any_path() {
        let root = std::env::temp_dir().join(format!("makepad-create-twice-{}", std::process::id()));
        let dir = root.join("home");
        let first = Home::open(&dir).unwrap();
        assert!(matches!(Home::open(&dir), Err(HomeError::InUse(_))), "a second open of the same home is InUse");
        #[cfg(unix)]
        {
            let link = root.join("link");
            std::os::unix::fs::symlink(&dir, &link).unwrap();
            assert!(matches!(Home::open(&link), Err(HomeError::InUse(_))), "a symlinked path is the same home");
            drop(first);
            drop(Home::open(&link).expect("the home opens again through the link once dropped"));
        }
        #[cfg(not(unix))]
        drop(first);
        drop(Home::open(&dir).expect("the home opens again once dropped"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The child tries the same lock between fork and exec and must find it taken, which shows
    /// the `fcntl` declaration really locks.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android"))]
    #[test]
    fn another_process_finds_the_home_lock_taken() {
        use std::os::unix::process::CommandExt;
        extern "C" {
            fn open(path: *const std::ffi::c_char, flags: i32, ...) -> i32;
        }
        let dir = std::env::temp_dir().join(format!("makepad-create-excl-{}", std::process::id()));
        let home = Home::open(&dir).unwrap();
        let path = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(dir.join("local/lock").as_os_str())).unwrap();
        let mut command = std::process::Command::new("/usr/bin/true");
        unsafe {
            command.pre_exec(move || {
                let fd = open(path.as_ptr(), 2);
                if fd < 0 { return Err(io::Error::last_os_error()) }
                let mut request = lock::sys::Flock { l_start: 0, l_len: 0, l_pid: 0, l_type: lock::sys::F_WRLCK, l_whence: lock::sys::SEEK_SET };
                if lock::sys::fcntl(fd, lock::sys::F_SETLK, &mut request as *mut lock::sys::Flock) == 0 {
                    // Taking the lock means the parent's is not one: report it as a distinct error.
                    return Err(io::Error::new(io::ErrorKind::Other, "the child locked the home"));
                }
                Err(io::Error::last_os_error())
            });
        }
        let result = command.status();
        drop(home);
        std::fs::remove_dir_all(&dir).unwrap();
        let error = result.expect_err("the child does not get to exec");
        assert!(matches!(error.raw_os_error(), Some(11) | Some(13) | Some(35)),
            "the child sees the lock as taken (EAGAIN or EACCES), got {error:?}");
    }
}
