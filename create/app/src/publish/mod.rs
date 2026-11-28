//! Publishing is local to this home. Only the upload worker talks to the configured target.
pub mod config;
pub mod post;
pub mod session;
pub mod sftp;
pub mod store;

use makepad_strict_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// The background publish jobs must finish their private index and history writes before returning.
pub(super) fn write_private_bg(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("new");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_data()?;
    }
    fs::rename(&tmp, path)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn field<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| invalid(format!("expected string field {key}")))
}

fn nonce() -> io::Result<String> {
    let mut bytes = [0; 16];
    makepad_network::tls::os_random(&mut bytes)?;
    Ok(makepad_town_core::hex(&bytes))
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(parent: &Path) -> io::Result<Self> {
        let path = parent.join(format!(".publish-{}", nonce()?));
        let builder = &mut fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(builder, 0o700);
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
