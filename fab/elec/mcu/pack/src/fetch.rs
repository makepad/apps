use crate::{
    io_error, pack_path, remove_file_if_present, verify_file, with_suffix, CancelToken, Error,
    HashingWriter, Manifest, PackFile, StagingDir,
};
use makepad_network::blocking_http::{self, request_to_writer_no_redirect, Limits, Request};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub bytes_done: u64,
    pub total_bytes: u64,
    /// Repository and relative path, separated by a slash.
    pub file: String,
}

fn check_cancel(cancel: &CancelToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn download(
    file: &PackFile,
    path: &Path,
    url: String,
    cancel: &CancelToken,
    progress: impl FnMut(u64),
) -> Result<(), Error> {
    let limits = Limits {
        max_body_bytes: usize::try_from(file.size).map_err(|_| {
            Error::InvalidManifest("file size exceeds this platform's download limit")
        })?,
        ..Limits::default()
    };
    let req = Request::get(url)
        .limits(limits)
        .cancel_token(cancel.clone());
    let part = with_suffix(path, ".part");
    remove_file_if_present(&part)?;
    let output = File::create_new(&part).map_err(|e| io_error(&part, e))?;
    let receive = || {
        let mut writer = HashingWriter::new(output, progress);
        let response = request_to_writer_no_redirect(req, &mut writer).map_err(|source| {
            if source == blocking_http::Error::Cancelled {
                Error::Cancelled
            } else {
                Error::Http {
                    file: path.into(),
                    source,
                }
            }
        })?;
        check_cancel(cancel)?;
        if response.status != 200 {
            return Err(Error::Status {
                file: path.into(),
                status: response.status,
            });
        }
        writer.flush().map_err(|e| io_error(&part, e))?;
        writer.verify(file, path)?;
        fs::rename(&part, path).map_err(|e| io_error(path, e))
    };
    let result = receive();
    if result.is_err() {
        remove_file_if_present(&part)?;
    }
    result
}

/// Fetch into a staging directory owned by this process. A retry reuses
/// verified files and restarts an interrupted file from byte zero.
pub fn fetch(
    manifest: &Manifest,
    url_base_override: Option<&str>,
    staging: &Path,
    cancel: &CancelToken,
    mut progress: impl FnMut(Progress),
) -> Result<(), Error> {
    check_cancel(cancel)?;
    manifest.validate()?;
    fs::create_dir_all(staging).map_err(|e| io_error(staging, e))?;
    let _worker = StagingDir::claim(staging)?;
    let total_bytes = manifest.total_bytes();
    let mut done = 0;
    for file in &manifest.files {
        check_cancel(cancel)?;
        let path = pack_path(staging, file, true)?;
        let name = format!("{}/{}", file.repository, file.path);
        let mut report = |bytes| {
            progress(Progress {
                bytes_done: done + bytes,
                total_bytes,
                file: name.clone(),
            })
        };
        match verify_file(file, &path) {
            Ok(()) => report(file.size),
            Err(Error::Missing(_) | Error::Size { .. } | Error::Hash(_) | Error::WrongType(_)) => {
                // Discard a damaged cache entry before requesting its replacement.
                remove_file_if_present(&path)?;
                let base = match url_base_override {
                    Some(base) => format!("{}/{}", base.trim_end_matches('/'), file.repository),
                    None => manifest
                        .repositories
                        .iter()
                        .find(|repo| repo.name == file.repository)
                        .expect("validated repository")
                        .url_base
                        .trim_end_matches('/')
                        .to_owned(),
                };
                report(0);
                download(file, &path, format!("{base}/{}", file.path), cancel, report)?;
            }
            Err(e) => return Err(e),
        }
        check_cancel(cancel)?;
        done += file.size;
    }
    Ok(())
}
