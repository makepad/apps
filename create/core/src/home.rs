//! One instance owns a home. `local/` holds its lock, device key, AI acknowledgement and
//! discovery endpoint. `local/peers` remembers linked devices and rooms. `local/home` lists
//! the local stores this home expects. Only `space/` is shared: the signed log and
//! content-addressed blobs. `checkpoints/` and `index/` are rebuildable from the shared log.

use crate::{event::RoomId, key::PublicKey};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub struct Home {
    pub dir: PathBuf,
    /// Held for the life of the instance: the OS releases it when the process ends.
    _lock: File,
}

#[derive(Debug)]
pub enum HomeError {
    /// Another Create holds this home.
    InUse(PathBuf),
    /// The home is inside a git work tree, where its key and log could be committed or pushed.
    InsideRepository { home: PathBuf, repository: PathBuf },
    Io(io::Error),
}

impl std::fmt::Display for HomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HomeError::InUse(dir) => write!(f, "another Create is using {}", dir.display()),
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
    /// `MAKEPAD_CREATE_HOME`, else `.makepad/create` in the user's home directory.
    pub fn default_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("MAKEPAD_CREATE_HOME").map(PathBuf::from).filter(|d| d.is_absolute()) {
            return Some(dir);
        }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        Some(PathBuf::from(home).join(".makepad").join("create"))
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
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("local/lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(HomeError::InUse(dir.to_path_buf())),
            Err(std::fs::TryLockError::Error(e)) => return Err(HomeError::Io(e)),
        }
        let home = Home { dir: dir.to_path_buf(), _lock: lock };
        for path in [dir.join("space"), home.rooms_dir(), home.blobs_dir(), home.checkpoints_dir(), home.index_dir()] {
            private_dir(&path)?;
        }
        Ok(home)
    }

    pub fn key_path(&self) -> PathBuf {
        self.dir.join("local/key")
    }
    pub fn peers_path(&self) -> PathBuf {
        self.dir.join("local/peers")
    }
    pub fn record_path(&self) -> PathBuf {
        self.dir.join("local/home")
    }
    pub fn log_path(&self) -> PathBuf {
        self.dir.join("space/log")
    }
    pub fn rooms_dir(&self) -> PathBuf {
        self.dir.join("space/rooms")
    }
    pub fn room_log_path(&self, room: &crate::event::RoomId) -> PathBuf {
        self.rooms_dir().join(format!("{}.log", crate::hex(room)))
    }

    pub fn open_space(&self) -> Option<crate::event::RoomId> {
        let bytes = self.home_record().ok()?;
        bytes.split(|&b| b == b'\n').rev()
            .find_map(|line| line.strip_prefix(b"open space "))
            .and_then(|id| crate::parse_id(std::str::from_utf8(id).ok()?))
    }

    pub fn set_open_space(&self, room: &crate::event::RoomId) -> io::Result<()> {
        if self.open_space().as_ref() == Some(room) { return Ok(()) }
        self.append_home_line(&format!("open space {}", crate::hex(room)))
    }

    /// Local agreement order disambiguates earlier shared copies on reconnect. A room's
    /// signed membership supplies the peer, so the home journal needs only the room id.
    pub fn share_order(&self) -> io::Result<Vec<crate::event::RoomId>> {
        let bytes = self.home_record()?;
        Ok(bytes.split(|&b| b == b'\n').filter_map(|line| {
            crate::parse_id(std::str::from_utf8(line.strip_prefix(b"shared space ")?).ok()?)
        }).collect())
    }

    pub fn remember_share(&self, room: &crate::event::RoomId) -> io::Result<()> {
        if self.share_order()?.last() == Some(room) { return Ok(()) }
        self.append_home_line(&format!("shared space {}", crate::hex(room)))
    }

    pub fn close_final(&self, room: &RoomId, peer: &PublicKey) -> io::Result<()> {
        self.append_home_line(&format!("final exchange closed {} {}", crate::hex(room), crate::hex(peer)))
    }

    pub fn closed_finals(&self) -> io::Result<BTreeSet<(RoomId, PublicKey)>> {
        let record = self.home_record()?;
        record.split(|&b| b == b'\n').filter_map(|line| line.strip_prefix(b"final exchange closed "))
            .map(|fields| {
                let pair = std::str::from_utf8(fields).ok().and_then(|s| s.split_once(' '))
                    .and_then(|(room, peer)| Some((crate::parse_id(room)?, makepad_network::tls::from_hex(peer)?)));
                pair.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid final exchange closure"))
            }).collect()
    }

    pub fn room_checkpoint(&self, room: &crate::event::RoomId) -> PathBuf {
        self.checkpoints_dir().join(format!("fold-{}", crate::hex(room)))
    }

    /// Journal the destination before the rename. The home lock serializes writers, and
    /// an interrupted rename can use the same recorded destination on the next open.
    pub fn set_aside_space(&self, room: Option<&crate::event::RoomId>, now: u64) -> io::Result<Option<PathBuf>> {
        let (old, prefix) = match room {
            Some(id) => (self.room_log_path(id), format!("room.before-policy-{}-", crate::hex(id))),
            None => (self.log_path(), "log.before-rooms-".to_owned()),
        };
        if !occupied(&old)? { return Ok(None) }
        let pending = self.space_notices()?.into_iter().rev().find(|path|
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)));
        let destination = match pending {
            Some(path) if !occupied(&path)? => path,
            _ => {
                let base = format!("{prefix}{}", utc_date(now));
                let mut name = base.clone();
                let mut suffix = 2;
                let space = self.log_path();
                while occupied(&space.with_file_name(&name))? {
                    name = format!("{base}-{suffix}");
                    suffix += 1;
                }
                self.append_home_line(&format!("set aside space {now} {name}"))?;
                space.with_file_name(name)
            }
        };
        std::fs::rename(old, &destination)?;
        Ok(Some(destination))
    }

    fn home_record(&self) -> io::Result<Vec<u8>> {
        match std::fs::read(self.record_path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(b"v1 home\n".to_vec()),
            result => result,
        }
    }

    fn append_home_line(&self, line: &str) -> io::Result<()> {
        let mut record = self.home_record()?;
        if !record.ends_with(b"\n") { record.push(b'\n'); }
        record.extend_from_slice(line.as_bytes());
        record.push(b'\n');
        write_private(&self.record_path(), &record)
    }

    pub fn space_notices(&self) -> io::Result<Vec<PathBuf>> {
        let record = self.home_record()?;
        let mut pending = Vec::new();
        for line in record.split(|&b| b == b'\n').filter_map(|l| std::str::from_utf8(l).ok()) {
            let Some((fields, name)) = line.rsplit_once(' ') else { continue };
            if !(name.starts_with("log.before-rooms-") || name.starts_with("room.before-policy-"))
                || name.ends_with(".log") || Path::new(name).file_name() != Some(name.as_ref()) { continue }
            let path = self.log_path().with_file_name(name);
            if fields.strip_prefix("set aside space ").is_some_and(|s| s.parse::<u64>().is_ok()) {
                pending.push(path);
            } else if fields == "told space" {
                pending.retain(|p| *p != path);
            }
        }
        Ok(pending)
    }

    pub fn dismiss_space_notice(&self, path: &Path) -> io::Result<()> {
        let name = path.file_name().and_then(|n| n.to_str()).ok_or(io::ErrorKind::InvalidInput)?;
        self.append_home_line(&format!("told space {name}"))
    }

    pub fn acks_path(&self) -> PathBuf {
        self.dir.join("local/ai-acked")
    }
    /// Where this instance publishes its endpoint (`makepad_remote::discovery::publish`).
    pub fn endpoint_path(&self) -> PathBuf {
        self.dir.join("local/endpoint")
    }
    pub fn blobs_dir(&self) -> PathBuf {
        self.dir.join("space/blobs")
    }
    pub fn checkpoints_dir(&self) -> PathBuf {
        self.dir.join("checkpoints")
    }
    pub fn index_dir(&self) -> PathBuf {
        self.dir.join("index")
    }
}

fn occupied(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
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
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
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
    fn a_home_separates_local_shared_and_rebuildable_files() {
        let dir = std::env::temp_dir().join(format!("makepad-create-home-{}", std::process::id()));
        let home = Home::open(&dir).unwrap();
        assert_eq!(home.key_path(), dir.join("local/key"), "the signing key is private");
        assert_eq!(home.acks_path(), dir.join("local/ai-acked"), "the acknowledgement is local");
        assert_eq!(home.endpoint_path(), dir.join("local/endpoint"), "discovery is local");
        assert_eq!(home.log_path(), dir.join("space/log"), "the pre-room log can still be located");
        assert_eq!(home.rooms_dir(), dir.join("space/rooms"), "rooms have their own directory");
        assert_eq!(home.room_log_path(&[0xab; 32]), home.rooms_dir().join(format!("{}.log", "ab".repeat(32))),
            "the room id is the full lower-case hex filename");
        assert_eq!(home.blobs_dir(), dir.join("space/blobs"), "blobs live beside the log");
        assert_eq!(home.checkpoints_dir(), dir.join("checkpoints"), "checkpoints have their own directory");
        assert_eq!(home.index_dir(), dir.join("index"), "indexes have their own directory");
        for path in [&dir, &dir.join("local"), &dir.join("space"), &home.rooms_dir(), &home.blobs_dir(), &home.checkpoints_dir(), &home.index_dir()] {
            let meta = std::fs::metadata(path).unwrap();
            assert!(meta.is_dir(), "opening the home makes {path:?}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(meta.permissions().mode() & 0o777, 0o700, "{path:?} is private");
            }
        }
        let first = home.log_path().with_file_name("log.before-rooms-2000-02-29");
        std::fs::write(&first, b"keep").unwrap();
        std::fs::write(home.log_path(), b"old").unwrap();
        let aside = home.set_aside_space(None, 951782400).unwrap().unwrap();
        assert_eq!(aside, first.with_file_name("log.before-rooms-2000-02-29-2"), "a UTC leap day and an occupied backup name");
        assert_eq!(std::fs::read(first).unwrap(), b"keep", "the first backup survived the collision");
        home.dismiss_space_notice(&aside).unwrap();
        std::fs::write(home.log_path(), b"next").unwrap();
        home.append_home_line("set aside space 951782400 log.before-rooms-2000-02-29-3").unwrap();
        let resumed = home.set_aside_space(None, 951868800).unwrap().unwrap();
        assert_eq!(home.space_notices().unwrap(), [resumed.clone()], "a journaled rename is resumed exactly once across midnight");
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
}
