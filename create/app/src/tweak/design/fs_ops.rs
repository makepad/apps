use std::{
    fs,
    io::{self, ErrorKind},
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileId {
    pub(super) dev: u64,
    pub(super) ino: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriterLiveness {
    Dead,
    Alive,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TempCheckError {
    InUse,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OsPidStatus {
    Absent,
    ExistsInaccessible,
    ExistsAccessible,
    Unknown,
}

/// Holds the exclusive lock across Unix quarantine. Windows closes its handle before moving.
pub(super) struct StaleGuard {
    #[cfg(unix)]
    _lock: fs::File,
}

pub(super) fn is_plain_file(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        meta.is_file() && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    {
        meta.is_file()
    }
}

/// Publish to an unoccupied name. Only try a hard link when the native move is unsupported.
pub(super) fn move_no_replace(src: &Path, dst: &Path) -> io::Result<()> {
    match rename_noreplace(src, dst) {
        Err(err) if err.kind() == ErrorKind::Unsupported => {
            link_and_unlink(src, dst).map_err(|link_err| {
                // FAT32/exFAT can report EPERM for unsupported hard links.
                if matches!(link_err.kind(), ErrorKind::Unsupported | ErrorKind::PermissionDenied) {
                    io::Error::new(ErrorKind::Unsupported, link_err)
                } else {
                    link_err
                }
            })
        }
        other => other,
    }
}

fn link_and_unlink(src: &Path, dst: &Path) -> io::Result<()> {
    fs::hard_link(src, dst)?;
    match fs::remove_file(src) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(dst);
            Err(err)
        }
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::{c_char, c_int, CString};
    use std::os::unix::ffi::OsStrExt;
    let c_path = |path: &Path| CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "path contains NUL"));
    let src = c_path(src)?;
    let dst = c_path(dst)?;
    #[cfg(target_vendor = "apple")]
    let result = {
        const RENAME_EXCL: u32 = 0x0000_0004;
        extern "C" {
            fn renamex_np(from: *const c_char, to: *const c_char, flags: u32) -> c_int;
        }
        unsafe { renamex_np(src.as_ptr(), dst.as_ptr(), RENAME_EXCL) }
    };
    #[cfg(target_os = "linux")]
    let result = {
        const AT_FDCWD: c_int = -100;
        const RENAME_NOREPLACE: u32 = 1;
        extern "C" {
            fn renameat2(olddirfd: c_int, oldpath: *const c_char,
                newdirfd: c_int, newpath: *const c_char, flags: u32) -> c_int;
        }
        unsafe { renameat2(AT_FDCWD, src.as_ptr(), AT_FDCWD, dst.as_ptr(), RENAME_NOREPLACE) }
    };
    if result == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    const EINVAL: i32 = 22;
    if err.raw_os_error() == Some(EINVAL) {
        Err(io::Error::new(ErrorKind::Unsupported, err))
    } else {
        Err(err)
    }
}

#[cfg(windows)]
fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
        if path.contains(&0) {
            return Err(io::Error::new(ErrorKind::InvalidInput, "path contains NUL"));
        }
        path.push(0);
        Ok(path)
    }
    let src = wide(src)?;
    let dst = wide(dst)?;
    // Omitting MOVEFILE_REPLACE_EXISTING and MOVEFILE_COPY_ALLOWED requires a no-replace move.
    if unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
fn rename_noreplace(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(ErrorKind::Unsupported, "atomic no-replace rename is unsupported on this platform"))
}

pub(super) fn classify_probe(
    expected_start: u64,
    queried_start: Option<u64>,
    os_status: impl FnOnce() -> OsPidStatus,
) -> WriterLiveness {
    match queried_start {
        Some(live) if live == expected_start => WriterLiveness::Alive,
        Some(live) if live > 0 => WriterLiveness::Dead,
        Some(_) => WriterLiveness::Unknown,
        None => match os_status() {
            OsPidStatus::Absent => WriterLiveness::Dead,
            OsPidStatus::ExistsInaccessible => WriterLiveness::Alive,
            OsPidStatus::ExistsAccessible | OsPidStatus::Unknown => WriterLiveness::Unknown,
        },
    }
}

pub(super) fn probe_writer(pid: u32, expected_start: u64) -> WriterLiveness {
    #[cfg(unix)]
    if pid == 0 || pid > i32::MAX as u32 {
        return WriterLiveness::Dead;
    }
    #[cfg(windows)]
    if pid == 0 {
        return WriterLiveness::Dead;
    }
    classify_probe(expected_start, makepad_remote::group::start_time(pid), || os_pid_status(pid))
}

#[cfg(unix)]
fn os_pid_status(pid: u32) -> OsPidStatus {
    // A pid_t of zero or a negative value refers to a group of processes.
    if pid == 0 || pid > i32::MAX as u32 {
        return OsPidStatus::Absent;
    }
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    if unsafe { kill(pid as i32, 0) } == 0 {
        return OsPidStatus::ExistsAccessible;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(3) => OsPidStatus::Absent,             // ESRCH
        Some(1) => OsPidStatus::ExistsInaccessible, // EPERM
        _ => OsPidStatus::Unknown,
    }
}

#[cfg(windows)]
fn os_pid_status(pid: u32) -> OsPidStatus {
    use std::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    if pid == 0 {
        return OsPidStatus::Absent;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return match io::Error::last_os_error().raw_os_error() {
            Some(ERROR_INVALID_PARAMETER) => OsPidStatus::Absent,
            Some(ERROR_ACCESS_DENIED) => OsPidStatus::ExistsInaccessible,
            _ => OsPidStatus::Unknown,
        };
    }
    let mut code = 0;
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) };
    unsafe { CloseHandle(handle) };
    if queried == 0 {
        OsPidStatus::Unknown
    } else if code == STILL_ACTIVE {
        OsPidStatus::ExistsAccessible
    } else {
        OsPidStatus::Absent
    }
}

pub(super) fn inspect_regular_unlocked(path: &Path) -> Result<FileId, TempCheckError> {
    let Some((_file, id)) = open_verified_regular(path, None, false)? else {
        return Err(TempCheckError::Ambiguous);
    };
    Ok(id)
}

pub(super) fn verify_and_lock_stale(path: &Path, expected: FileId) -> Result<Option<StaleGuard>, TempCheckError> {
    let Some((file, _id)) = open_verified_regular(path, Some(expected), true)? else {
        return Ok(None);
    };
    #[cfg(not(unix))]
    drop(file);
    Ok(Some(StaleGuard {
        #[cfg(unix)]
        _lock: file,
    }))
}

#[cfg(not(any(unix, windows)))]
fn os_pid_status(_pid: u32) -> OsPidStatus {
    OsPidStatus::Unknown
}

#[cfg(any(unix, windows))]
fn open_verified_regular(
    path: &Path, expected: Option<FileId>, allow_missing: bool,
) -> Result<Option<(fs::File, FileId)>, TempCheckError> {
    let ambiguous = |_| TempCheckError::Ambiguous;
    let missing = |err: &io::Error| allow_missing && err.kind() == ErrorKind::NotFound;
    let mut options = fs::OpenOptions::new();
    options.read(true);

    #[cfg(unix)]
    let path_id = {
        use std::os::unix::fs::OpenOptionsExt;
        let metadata = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) if missing(&err) => return Ok(None),
            Err(_) => return Err(TempCheckError::Ambiguous),
        };
        let id = unix_file_id(&metadata);
        if !is_plain_file(&metadata) || expected.is_some_and(|want| want != id) {
            return Err(TempCheckError::Ambiguous);
        }
        let flags = if cfg!(target_vendor = "apple") {
            0x0100 | 0x0004
        } else if cfg!(all(any(target_os = "linux", target_os = "android"),
            any(target_arch = "arm", target_arch = "aarch64"))) {
            0o100000 | 0o4000
        } else if cfg!(all(any(target_os = "linux", target_os = "android"), any(
            target_arch = "x86", target_arch = "x86_64", target_arch = "riscv32",
            target_arch = "riscv64", target_arch = "loongarch64"))) {
            0o400000 | 0o4000
        } else {
            0
        };
        options.custom_flags(flags); // O_NOFOLLOW | O_NONBLOCK where the flags are known
        id
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(err) if missing(&err) => return Ok(None),
        Err(_) => return Err(TempCheckError::Ambiguous),
    };
    let opened = file.metadata().map_err(ambiguous)?;
    if !is_plain_file(&opened) {
        return Err(TempCheckError::Ambiguous);
    }
    #[cfg(unix)]
    let id = {
        let id = unix_file_id(&opened);
        if id != path_id {
            return Err(TempCheckError::Ambiguous);
        }
        id
    };
    #[cfg(windows)]
    let id = {
        let id = windows_file_id(&file)?;
        if expected.is_some_and(|want| want != id) {
            return Err(TempCheckError::Ambiguous);
        }
        id
    };
    match file.try_lock() {
        Ok(()) => Ok(Some((file, id))),
        Err(fs::TryLockError::WouldBlock) => Err(TempCheckError::InUse),
        Err(fs::TryLockError::Error(_)) => Err(TempCheckError::Ambiguous),
    }
}

#[cfg(unix)]
fn unix_file_id(meta: &fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    FileId { dev: meta.dev(), ino: meta.ino() }
}

#[cfg(windows)]
fn windows_file_id(file: &fs::File) -> Result<FileId, TempCheckError> {
    use std::{ffi::c_void, mem::MaybeUninit, os::windows::io::AsRawHandle};
    #[repr(C)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: [u32; 2],
        last_access_time: [u32; 2],
        last_write_time: [u32; 2],
        volume_serial: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(handle: *mut c_void, info: *mut ByHandleFileInformation) -> i32;
    }
    let mut info = MaybeUninit::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(TempCheckError::Ambiguous);
    }
    let info = unsafe { info.assume_init() };
    Ok(FileId {
        dev: info.volume_serial.into(),
        ino: (u64::from(info.index_high) << 32) | u64::from(info.index_low),
    })
}

#[cfg(not(any(unix, windows)))]
fn open_verified_regular(
    _path: &Path, _expected: Option<FileId>, _allow_missing: bool,
) -> Result<Option<(fs::File, FileId)>, TempCheckError> {
    Err(TempCheckError::Ambiguous)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_no_replace_and_link_fallback_never_overwrite_existing_or_dangling_symlink() {
        let dir = std::env::temp_dir().join(format!("makepad-town-no-replace-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let src = dir.join("src");
        let dst = dir.join("dst");
        for move_file in [move_no_replace, link_and_unlink] {
            fs::write(&src, "first").unwrap();
            move_file(&src, &dst).unwrap();
            assert!(!src.exists());
            assert_eq!(fs::read_to_string(&dst).unwrap(), "first");

            fs::write(&src, "second").unwrap();
            assert_eq!(move_file(&src, &dst).unwrap_err().kind(), ErrorKind::AlreadyExists);
            assert_eq!(fs::read_to_string(&src).unwrap(), "second");
            assert_eq!(fs::read_to_string(&dst).unwrap(), "first");
            fs::remove_file(&dst).unwrap();

            #[cfg(unix)]
            {
                let missing = dir.join("missing");
                std::os::unix::fs::symlink(&missing, &dst).unwrap();
                assert_eq!(move_file(&src, &dst).unwrap_err().kind(), ErrorKind::AlreadyExists);
                assert_eq!(fs::read_to_string(&src).unwrap(), "second");
                assert_eq!(fs::read_link(&dst).unwrap(), missing);
                assert!(!missing.exists());
                fs::remove_file(&dst).unwrap();
            }
            fs::remove_file(&src).unwrap();
            assert_eq!(move_file(&src, &dst).unwrap_err().kind(), ErrorKind::NotFound);
        }
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn probe_writer_treats_unknown_and_inaccessible_as_not_dead() {
        use OsPidStatus::*;
        use WriterLiveness::{Alive, Dead};
        let expected = 42;
        for (start, result) in [(expected, Alive), (expected ^ 1, Dead), (0, WriterLiveness::Unknown)] {
            assert_eq!(classify_probe(expected, Some(start), || panic!("redundant PID lookup")), result);
        }
        for (status, result) in [(Absent, Dead), (ExistsInaccessible, Alive),
            (ExistsAccessible, WriterLiveness::Unknown), (Unknown, WriterLiveness::Unknown)] {
            assert_eq!(classify_probe(expected, None, || status), result);
        }
        #[cfg(any(unix, windows))]
        {
            assert_eq!(probe_writer(0, 1), Dead);
            assert_eq!(probe_writer(u32::MAX, 1), Dead);
        }
        #[cfg(unix)]
        assert_eq!(probe_writer(i32::MAX as u32 + 1, 1), Dead);
        let pid = std::process::id();
        if let Some(start) = makepad_remote::group::start_time(pid).filter(|&t| t > 0) {
            let wrong_start = if start == 1 { 2 } else { start ^ 1 };
            assert_eq!(probe_writer(pid, start), Alive);
            assert_eq!(probe_writer(pid, wrong_start), Dead);
        }
    }
}
