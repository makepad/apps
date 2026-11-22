use fab_elec_mcu_rt::bundle::{pack, read, BundleFile, Kind};

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 0 { 0 } else { 0xedb8_8320 };
        }
    }
    !crc
}

#[test]
fn firmware_files_pack_and_read_in_place() {
    let mut files = [
        BundleFile {
            name: "thumbnail".into(),
            kind: Kind::Image,
            format: 3,
            a: 11,
            b: 3,
            bytes: (0..66).collect(),
        },
        BundleFile {
            name: "setting_1".into(),
            kind: Kind::Blob,
            format: 0,
            a: 0,
            b: 0,
            bytes: vec![0xa5],
        },
        BundleFile {
            name: "audio".into(),
            kind: Kind::Sound,
            format: 7,
            a: 16_000,
            b: 9,
            bytes: b"123456789".to_vec(),
        },
    ];
    let bytes = pack(&files).expect("three valid files pack");
    files.reverse();
    assert_eq!(pack(&files).expect("reordered files pack"), bytes, "input order preserves every byte");

    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let half = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    assert_eq!(&bytes[..4], b"FBND", "wire magic");
    assert_eq!(half(4), 1, "wire version");
    assert_eq!(half(6), 32, "wire entry size");
    assert_eq!(word(8), 3, "wire count");
    assert_eq!(word(12), 160, "names follow the three index rows");
    assert_eq!(word(16), 192, "first payload follows the padded names");
    assert_eq!(word(20) as usize, bytes.len(), "header total covers the entire packed bundle");
    assert_eq!(bytes.len(), 448, "total includes final image padding");
    assert_eq!(bytes.len() % 64, 0, "total is aligned");
    assert_eq!(word(24), crc32(&bytes[64..192]), "index CRC includes names and padding");
    assert_eq!(word(28), crc32(&bytes[192..]), "data CRC includes all padded files");
    assert_eq!(&bytes[32..64], &[0; 32], "reserved header bytes are zero");
    assert_eq!(&bytes[160..183], b"audiosetting_1thumbnail", "names are packed in byte order");
    assert_eq!(&bytes[183..192], &[0; 9], "name padding is zero");
    assert_eq!(word(64 + 28), 0xcbf4_3926, "sound has the IEEE CRC-32 check value");

    let bundle = read(&bytes).expect("packed bundle validates");
    assert_eq!(bundle.len(), 3, "reader count");
    assert_eq!(bundle.index_crc(), word(24), "reader exposes the stored index CRC");
    assert_eq!(bundle.entries().len(), 3, "iterator covers every file");
    for (i, (entry, file)) in bundle.entries().zip(&files).enumerate() {
        let name = file.name.as_str();
        let row = 64 + 32 * i;
        let start = word(row + 4) as usize;
        let end = start + file.bytes.len();
        let name_at = word(row + 20) as usize;
        assert_eq!(start, [192, 256, 320][i], "{name} data offset in index order");
        assert_eq!(start % 64, 0, "{name} data alignment");
        assert_eq!(word(row + 8) as usize, file.bytes.len(), "{name} wire size");
        assert_eq!(name_at, [160, 165, 174][i], "{name} absolute name offset");
        assert_eq!(half(row + 24) as usize, name.len(), "{name} wire name length");
        assert_eq!(bytes[row], [2, 4, 1][i], "{name} wire kind");
        assert_eq!(bytes[row + 1], file.format, "{name} wire format");
        assert_eq!((word(row + 12), word(row + 16)), (file.a, file.b), "{name} wire metadata");
        assert_eq!(half(row + 2), 0, "{name} wire flags");
        assert_eq!(half(row + 26), 0, "{name} reserved entry field");
        let next = [256, 320, 448][i];
        assert!(bytes[end..next].iter().all(|&b| b == 0), "{name} data padding is zero");
        assert_eq!(entry.name, name, "iteration yields names in byte order");
        assert_eq!((entry.kind, entry.format), (file.kind, file.format), "{name} kind and format");
        assert_eq!((entry.a, entry.b), (file.a, file.b), "{name} metadata");
        assert_eq!(entry.crc, crc32(&file.bytes), "{name} CRC covers only its payload");
        assert_eq!(entry.data, file.bytes.as_slice(), "{name} payload contents");
        assert!(core::ptr::eq(entry.data, &bytes[start..end]), "{name} payload borrows the input");
        let wire_name = &bytes[name_at..name_at + name.len()];
        assert!(core::ptr::eq(entry.name.as_bytes(), wire_name), "{name} borrows the input");
        assert_eq!(bundle.find(name), Some(entry), "{name} lookup returns the iterated entry");
    }
    assert!(bundle.find("missing").is_none(), "absent name has no entry");

    let duplicate = pack(&[files[0].clone(), files[0].clone()]).expect_err("duplicate must fail");
    assert!(
        duplicate.contains("duplicate") && duplicate.contains("audio"),
        "duplicate error identifies the file"
    );
    for name in ["", "_audio", "9audio", "Audio", "au-dio", "aé"] {
        files[0].name = name.into();
        let error = pack(&files).expect_err("invalid identifier must fail");
        assert!(error.contains(&format!("{name:?}")), "invalid identifier {name:?} is named in the error");
    }
    files[0].name = "a".repeat(65);
    let error = pack(&files).expect_err("65-byte identifier must fail");
    assert!(error.contains(&files[0].name), "overlong identifier is named in the error");

    let mut damaged = bytes.clone();
    damaged[192] ^= 1;
    let error = read(&damaged).expect_err("flipped payload must fail");
    assert!(error.contains("data CRC"), "payload corruption reports data CRC");
    damaged.copy_from_slice(&bytes);
    damaged[65] ^= 1;
    let error = read(&damaged).expect_err("flipped index must fail");
    assert!(error.contains("index CRC"), "index corruption reports index CRC");
    let error = read(&bytes[..447]).expect_err("truncation must fail");
    assert!(error.contains("truncated"), "missing final padding is truncation");

    damaged.copy_from_slice(&bytes);
    damaged[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
    let index_crc = crc32(&damaged[64..192]);
    damaged[24..28].copy_from_slice(&index_crc.to_le_bytes());
    let error = read(&damaged).expect_err("invalid offset with a valid CRC must fail");
    assert!(
        error.contains("offset") && error.contains("audio"),
        "out-of-range payload offset identifies the file"
    );

    damaged.copy_from_slice(&bytes);
    damaged.extend_from_slice(&[0xff; 64]);
    let region = read(&damaged).expect("flash region may extend past the bundle");
    assert_eq!(region.find("audio"), bundle.find("audio"), "lookup honors the declared length");

    let empty = pack(&[]).expect("empty set packs");
    assert_eq!(empty.len(), 64, "empty bundle fits in its header");
    let empty = read(&empty).expect("empty bundle validates");
    assert!(empty.is_empty() && empty.entries().next().is_none(), "empty bundle has no files");
    assert_eq!(empty.index_crc(), 0, "IEEE CRC of the empty index");
}
