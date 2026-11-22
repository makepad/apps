//! Read-only firmware file bundles: the files a program ships with (pictures, sounds, fonts,
//! blobs), packed on the host and read in place from their own flash region. Format v1,
//! little-endian, every offset from the bundle's first byte:
//!
//! - header, 64 bytes: magic "FBND", version 1, entry size 32, count, names offset, data offset,
//!   total length (a multiple of 64), CRC-32 of [64, data offset), CRC-32 of [data offset, total);
//!   bytes 32..64 zero;
//! - index: `count` entries of 32 bytes in bytewise name order (kind, format, flags 0, data
//!   offset, size, two kind-specific words, name offset, name length, 0, the file's CRC-32), so a
//!   file's id is its index position and `find` is a binary search;
//! - names back to back, zero-padded to 64; then each file at a 64-byte boundary, zero-padded.
//!
//! The same set of files packs to the same bytes in any order, so Flash to can skip a bundle
//! region whose checksum is unchanged.

use alloc::{format, string::String, vec, vec::Vec};
use core::str;

pub const MAGIC: u32 = 0x444e_4246;
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 64;
pub const ENTRY_SIZE: usize = 32;
pub const ALIGNMENT: usize = 64;
/// An image's format: row-major RGB565 pixels, each a little-endian u16; `a` and `b` are its
/// width and height.
pub const IMAGE_RGB565: u8 = 1;
const MAX_NAME_LEN: usize = 64;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image = 1,
    Sound = 2,
    Font = 3,
    Blob = 4,
}

impl Kind {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Image),
            2 => Some(Self::Sound),
            3 => Some(Self::Font),
            4 => Some(Self::Blob),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BundleFile {
    pub name: String,
    pub kind: Kind,
    pub format: u8,
    pub a: u32,
    pub b: u32,
    pub bytes: Vec<u8>,
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() <= MAX_NAME_LEN
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

// IEEE CRC-32, reflected polynomial 0xEDB88320, as in the firmware image.
static CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < table.len() {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc = (crc >> 8) ^ CRC_TABLE[((crc as u8) ^ byte) as usize];
    }
    !crc
}

fn aligned(end: u64) -> u64 {
    end.next_multiple_of(ALIGNMENT as u64)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Packs files in bytewise name order, including zero padding in both region CRCs.
pub fn pack(files: &[BundleFile]) -> Result<Vec<u8>, String> {
    let mut names_at = HEADER_SIZE as u64;
    let mut names_len = 0;
    let mut payload_len = 0;
    for file in files {
        if !valid_name(&file.name) {
            return Err(format!("invalid bundle file name {:?}", file.name));
        }
        if file.kind == Kind::Blob && (file.a != 0 || file.b != 0) {
            return Err(format!("blob {} requires zero a and b", file.name));
        }
        let size = u32::try_from(file.bytes.len())
            .map_err(|_| format!("size of {} exceeds u32", file.name))?;
        names_at += ENTRY_SIZE as u64;
        names_len += file.name.len() as u64;
        payload_len += aligned(u64::from(size));
        let total = aligned(names_at + names_len) + payload_len;
        if total > u64::from(u32::MAX) {
            return Err(format!("bundle offset exceeds u32 adding {}", file.name));
        }
        if total > isize::MAX as u64 {
            return Err(format!("bundle exceeds addressable memory adding {}", file.name));
        }
    }

    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    for pair in sorted.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(format!("duplicate bundle file name {}", pair[1].name));
        }
    }

    let data_at = aligned(names_at + names_len) as usize;
    let total = data_at + payload_len as usize;
    let mut bytes = vec![0; total];
    put_u32(&mut bytes, 0, MAGIC);
    bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
    bytes[6..8].copy_from_slice(&(ENTRY_SIZE as u16).to_le_bytes());
    put_u32(&mut bytes, 8, files.len() as u32);
    put_u32(&mut bytes, 12, names_at as u32);
    put_u32(&mut bytes, 16, data_at as u32);
    put_u32(&mut bytes, 20, total as u32);

    let mut name_pos = names_at as usize;
    let mut data_pos = data_at;
    for (i, file) in sorted.into_iter().enumerate() {
        let at = HEADER_SIZE + i * ENTRY_SIZE;
        let row = &mut bytes[at..at + ENTRY_SIZE];
        row[0] = file.kind as u8;
        row[1] = file.format;
        put_u32(row, 4, data_pos as u32);
        put_u32(row, 8, file.bytes.len() as u32);
        put_u32(row, 12, file.a);
        put_u32(row, 16, file.b);
        put_u32(row, 20, name_pos as u32);
        row[24..26].copy_from_slice(&(file.name.len() as u16).to_le_bytes());
        put_u32(row, 28, crc32(&file.bytes));

        let name_end = name_pos + file.name.len();
        bytes[name_pos..name_end].copy_from_slice(file.name.as_bytes());
        name_pos = name_end;
        let data_end = data_pos + file.bytes.len();
        bytes[data_pos..data_end].copy_from_slice(&file.bytes);
        data_pos = aligned(data_end as u64) as usize;
    }
    let index_crc = crc32(&bytes[HEADER_SIZE..data_at]);
    let data_crc = crc32(&bytes[data_at..]);
    put_u32(&mut bytes, 24, index_crc);
    put_u32(&mut bytes, 28, data_crc);
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub kind: Kind,
    pub format: u8,
    pub a: u32,
    pub b: u32,
    /// Where `data` starts, from the bundle's first byte.
    pub offset: u32,
    pub data: &'a [u8],
    pub crc: u32,
}

#[derive(Debug)]
pub struct Bundle<'a> {
    bytes: &'a [u8],
    index: &'a [[u8; ENTRY_SIZE]],
}

impl<'a> Bundle<'a> {
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn index_crc(&self) -> u32 {
        u32_at(self.bytes, 24)
    }

    // read() has checked the ranges, UTF-8, and kind of every row.
    fn name(&self, row: &[u8; ENTRY_SIZE]) -> &'a str {
        let start = u32_at(row, 20) as usize;
        let end = start + usize::from(u16_at(row, 24));
        str::from_utf8(&self.bytes[start..end]).expect("validated bundle name")
    }

    fn entry(&self, row: &[u8; ENTRY_SIZE]) -> Entry<'a> {
        let start = u32_at(row, 4) as usize;
        let end = start + u32_at(row, 8) as usize;
        Entry {
            name: self.name(row),
            kind: Kind::from_byte(row[0]).expect("validated bundle kind"),
            format: row[1],
            a: u32_at(row, 12),
            b: u32_at(row, 16),
            offset: start as u32,
            data: &self.bytes[start..end],
            crc: u32_at(row, 28),
        }
    }

    /// Iterates in bytewise name order, borrowing names and data from the bundle.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = Entry<'a>> + '_ {
        self.index.iter().map(|row| self.entry(row))
    }

    pub fn find(&self, name: &str) -> Option<Entry<'a>> {
        let i = self.index.binary_search_by(|row| self.name(row).cmp(name)).ok()?;
        Some(self.entry(&self.index[i]))
    }
}

/// Validates one bundle at the start of a flash region. Bytes after its declared
/// length belong to the surrounding region.
pub fn read(bytes: &[u8]) -> Result<Bundle<'_>, String> {
    if bytes.len() < HEADER_SIZE {
        return Err("truncated bundle header".into());
    }
    if u32_at(bytes, 0) != MAGIC {
        return Err("invalid bundle magic".into());
    }
    if u16_at(bytes, 4) != VERSION {
        return Err("unsupported bundle version".into());
    }
    if usize::from(u16_at(bytes, 6)) != ENTRY_SIZE {
        return Err("invalid bundle entry size".into());
    }
    if bytes[32..HEADER_SIZE].iter().any(|&b| b != 0) {
        return Err("nonzero reserved header bytes".into());
    }
    let count = u64::from(u32_at(bytes, 8));
    let names_at = u64::from(u32_at(bytes, 12));
    let data_at = u64::from(u32_at(bytes, 16));
    let total = u64::from(u32_at(bytes, 20));
    if names_at != HEADER_SIZE as u64 + count * ENTRY_SIZE as u64 {
        return Err("names offset does not match index length".into());
    }
    if data_at < names_at || data_at > total || data_at != aligned(data_at) {
        return Err("data offset out of range or unaligned".into());
    }
    if total > bytes.len() as u64 {
        return Err("truncated bundle data".into());
    }
    if total != aligned(total) {
        return Err("unaligned bundle length".into());
    }

    let bytes = &bytes[..total as usize];
    if crc32(&bytes[HEADER_SIZE..data_at as usize]) != u32_at(bytes, 24) {
        return Err("bundle index CRC mismatch".into());
    }
    if crc32(&bytes[data_at as usize..]) != u32_at(bytes, 28) {
        return Err("bundle data CRC mismatch".into());
    }
    let (index, _) = bytes[HEADER_SIZE..names_at as usize].as_chunks::<ENTRY_SIZE>();
    let mut name_pos = names_at;
    let mut data_pos = data_at;
    let mut previous = None;
    for (i, row) in index.iter().enumerate() {
        let name_len = usize::from(u16_at(row, 24));
        if !(1..=MAX_NAME_LEN).contains(&name_len) {
            return Err(format!("entry {i} has invalid name length {name_len}"));
        }
        let name_end = name_pos + name_len as u64;
        if u64::from(u32_at(row, 20)) != name_pos || name_end > data_at {
            return Err(format!("entry {i} has invalid name offset or length"));
        }
        let name = str::from_utf8(&bytes[name_pos as usize..name_end as usize])
            .map_err(|_| format!("entry {i} name is invalid UTF-8"))?;
        if !valid_name(name) {
            return Err(format!("entry {i} has invalid file name {name:?}"));
        }
        if let Some(prev) = previous {
            if prev == name {
                return Err(format!("duplicate bundle file name {name}"));
            }
            if prev > name {
                return Err(format!("bundle names out of order at {name}"));
            }
        }
        previous = Some(name);
        name_pos = name_end;

        let kind = Kind::from_byte(row[0])
            .ok_or_else(|| format!("unknown file kind {} for {name}", row[0]))?;
        if kind == Kind::Blob && (u32_at(row, 12) != 0 || u32_at(row, 16) != 0) {
            return Err(format!("blob {name} requires zero a and b"));
        }
        if u16_at(row, 2) != 0 || u16_at(row, 26) != 0 {
            return Err(format!("nonzero flags or reserved field for {name}"));
        }
        let data_end = data_pos + u64::from(u32_at(row, 8));
        if u64::from(u32_at(row, 4)) != data_pos || data_end > total {
            return Err(format!("invalid file data offset or size for {name}"));
        }
        if crc32(&bytes[data_pos as usize..data_end as usize]) != u32_at(row, 28) {
            return Err(format!("file CRC mismatch for {name}"));
        }
        data_pos = aligned(data_end);
        if bytes[data_end as usize..data_pos as usize].iter().any(|&b| b != 0) {
            return Err(format!("nonzero data padding after {name}"));
        }
    }
    let names_padding = &bytes[name_pos as usize..data_at as usize];
    if aligned(name_pos) != data_at || names_padding.iter().any(|&b| b != 0) {
        return Err("invalid names padding".into());
    }
    if data_pos != total {
        return Err("bundle length does not match end of file data".into());
    }
    Ok(Bundle { bytes, index })
}
