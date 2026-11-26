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

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn field<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| invalid(format!("expected string field {key}")))
}

fn nonce() -> io::Result<String> {
    let mut bytes = [0; 16];
    makepad_network::tls::os_random(&mut bytes)?;
    Ok(makepad_work_core::hex(&bytes))
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
