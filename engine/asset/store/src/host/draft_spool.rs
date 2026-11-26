//! Anonymous scratch files keep live drafts out of the durable catalog and CAS.

use crate::error::io_err;
use crate::ServerResult;
use std::fs::{self, File, OpenOptions};
use std::path::Path;

#[derive(Debug)]
pub struct DraftFile {
    pub file: File,
    pub len: u64,
    pub token: String,
}

impl DraftFile {
    /// Recover names left between creation and unlink by an interrupted process.
    pub fn recover(root: &Path) -> ServerResult<()> {
        let dir = root.join("previews");
        fs::create_dir_all(&dir).map_err(io_err("create preview spool"))?;
        for entry in fs::read_dir(&dir).map_err(io_err("read preview spool"))? {
            let entry = entry.map_err(io_err("read preview spool entry"))?;
            fs::remove_file(entry.path()).map_err(io_err("remove preview orphan"))?;
        }
        Ok(())
    }

    /// The open handle alone owns the bytes, so crashes need no catalog cleanup.
    pub fn create(root: &Path) -> ServerResult<File> {
        let path = root.join("previews").join(super::util::to_hex(&super::util::rand32()?));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
            const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x1 | 0x2 | 0x4;
            options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE).share_mode(FILE_SHARE_READ_WRITE_DELETE);
        }
        let file = options.open(&path).map_err(io_err("create preview file"))?;
        #[cfg(unix)]
        fs::remove_file(&path).map_err(io_err("unlink preview file"))?;
        Ok(file)
    }
}
