//! Deterministic ZIP packages and in-memory read-back of their files.

use crate::error::CamError;
use crate::profile::OutFile;
use makepad_zip_file::{zip_read_central_directory, ZipMethod, ZipWriter};
use std::io::Cursor;

pub fn package(files: &[OutFile], zip_name: &str) -> Result<OutFile, CamError> {
    // ZipWriter uses a fixed DOS timestamp (1980-01-01) and the default deflate level.
    let mut writer = ZipWriter::new();
    for file in files {
        writer.add(&file.name, &file.bytes, ZipMethod::Deflate)
            .map_err(|e| CamError::Profile(format!("{zip_name}: {}: {e}", file.name)))?;
    }
    let bytes = writer.finish().map_err(|e| CamError::Profile(format!("{zip_name}: {e}")))?;
    Ok(OutFile { name: zip_name.into(), bytes })
}

pub fn unpackage(zip: &[u8]) -> Result<Vec<OutFile>, CamError> {
    let mut cursor = Cursor::new(zip);
    let directory = zip_read_central_directory(&mut cursor)
        .map_err(|e| CamError::Profile(format!("zip directory: {e:?}")))?;
    let mut files = Vec::with_capacity(directory.file_headers.len());
    for header in directory.file_headers {
        let bytes = header.extract(&mut cursor)
            .map_err(|e| CamError::Profile(format!("zip member {}: {e:?}", header.file_name)))?;
        files.push(OutFile { name: header.file_name, bytes });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_roundtrip_is_deterministic() {
        let files = vec![
            OutFile { name: "board-bom.csv".into(), bytes: b"10k,R2,R_0603,RES-10K\n".repeat(32) },
            OutFile { name: "layers/data.bin".into(), bytes: vec![0, 1, 2, 127, 128, 255] },
            OutFile { name: "empty.txt".into(), bytes: Vec::new() },
        ];
        let zip = package(&files, "board.zip").unwrap();
        assert_eq!(zip.name, "board.zip", "requested archive name");
        assert_eq!(unpackage(&zip.bytes).unwrap(), files, "member names, order and bytes survive");
        assert_eq!(package(&files, "board.zip").unwrap().bytes, zip.bytes, "identical input gives identical ZIP bytes");
        assert!(matches!(unpackage(b"invalid zip"), Err(CamError::Profile(_))), "reader errors map to profile errors");
        assert!(matches!(package(&[files[0].clone(), files[0].clone()], "duplicate.zip"), Err(CamError::Profile(_))), "writer errors map to profile errors");
    }
}
