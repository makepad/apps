use super::{u32_at, Fault, Rule};
use crate::format;
use std::collections::HashSet;
use std::ops::Range;

const END: u32 = 0x06054b50;
const CENTRAL: u32 = 0x02014b50;
const LOCAL: u32 = 0x04034b50;
const ZIP64_FIELD: u16 = 1;
const CRC_POLYNOMIAL: u32 = 0xedb88320;
const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = i as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ if value & 1 == 1 { CRC_POLYNOMIAL } else { 0 };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
};

#[derive(Debug)]
pub(super) struct Place {
    pub name: String,
    data: Range<usize>,
    size: u32,
    crc: u32,
    hashes: Vec<Range<usize>>,
}
impl Place {
    pub fn inflate(&self, file: &[u8]) -> Result<Vec<u8>, Fault> {
        let bytes = makepad_fast_inflate::inflate::decompress_to_vec(&file[self.data.clone()])
            .map_err(|e| Fault::new(Rule::Deflated, format!("{}: inflate failed: {e:?}", self.name)))?;
        if bytes.len() as u64 != self.size as u64 {
            return Err(Fault::new(Rule::Deflated, format!("{}: inflated {} bytes, expected {}", self.name, bytes.len(), self.size)));
        }
        Ok(bytes)
    }
    pub fn verify(&self, file: &[u8]) -> Result<(), Fault> {
        let bytes = self.inflate(file)?;
        let crc = crc32(&bytes);
        if crc != self.crc {
            return Err(Fault::new(Rule::Crc, format!("{}: CRC {crc:08x}, expected {:08x}", self.name, self.crc)));
        }
        if self.hashes.len() != 1 {
            return Err(Fault::new(Rule::Hash, format!("{}: {} hash fields, expected one", self.name, self.hashes.len())));
        }
        let field = &file[self.hashes[0].clone()];
        if field.len() != format::HASH_FIELD_LEN as usize || u16_at(field, 0) != format::HASH_FIELD_TAG || u16_at(field, 2) != 32 {
            return Err(Fault::new(Rule::Hash, format!("{}: malformed hash field, expected tag and 32-byte digest", self.name)));
        }
        let digest = makepad_core_util::sha256::sha256(&bytes);
        if field[4..] != digest {
            return Err(Fault::new(Rule::Hash, format!("{}: SHA-256 differs from central hash", self.name)));
        }
        Ok(())
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes { crc = (crc >> 8) ^ CRC_TABLE[((crc ^ *byte as u32) & 255) as usize]; }
    !crc
}
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn fault(detail: impl Into<String>) -> Fault { Fault::new(Rule::Zip, detail) }
fn span(start: usize, len: usize, end: usize) -> Result<Range<usize>, Fault> {
    let stop = start.checked_add(len).filter(|n| *n <= end)
        .ok_or_else(|| fault(format!("range at {start} with length {len} exceeds boundary {end}")))?;
    Ok(start..stop)
}
fn extras(file: &[u8], range: Range<usize>) -> Result<Vec<Range<usize>>, Fault> {
    let mut hashes = Vec::new();
    let mut at = range.start;
    while at < range.end {
        span(at, 4, range.end)?;
        let id = u16_at(file, at);
        let len = u16_at(file, at + 2) as usize;
        let data = span(at + 4, len, range.end)?;
        if id == ZIP64_FIELD { return Err(fault("ZIP64 extra field is unsupported")); }
        if id == format::HASH_FIELD_ID { hashes.push(data.clone()); }
        at = data.end;
    }
    Ok(hashes)
}

pub(super) fn parse(file: &[u8]) -> Result<Vec<Place>, Fault> {
    if file.len() < 22 { return Err(fault("file is shorter than a ZIP end record")); }
    let end = (file.len().saturating_sub(22 + u16::MAX as usize)..=file.len() - 22).rev()
        .find(|at| u32_at(file, *at) == END && *at + 22 + u16_at(file, *at + 20) as usize == file.len())
        .ok_or_else(|| fault("missing ZIP end record with matching comment length"))?;
    let count = u16_at(file, end + 10);
    if u16_at(file, end + 4) != 0 || u16_at(file, end + 6) != 0 || u16_at(file, end + 8) != count {
        return Err(fault("nonzero disk number or unequal directory entry counts"));
    }
    let size = u32_at(file, end + 12);
    let offset = u32_at(file, end + 16);
    if size == u32::MAX || offset == u32::MAX {
        return Err(fault("ZIP64 directory values are unsupported"));
    }
    let central = span(offset as usize, size as usize, end)?;
    if central.end != end { return Err(fault("central directory does not end at the end record")); }
    let mut at = central.start;
    let mut entries = Vec::with_capacity(count as usize);
    let mut names = HashSet::new();
    for _ in 0..count {
        span(at, 46, central.end)?;
        if u32_at(file, at) != CENTRAL { return Err(fault(format!("invalid central header at {at}"))); }
        let flags = u16_at(file, at + 8);
        let method = u16_at(file, at + 10);
        let crc = u32_at(file, at + 16);
        let compressed = u32_at(file, at + 20);
        let size = u32_at(file, at + 24);
        let local = u32_at(file, at + 42);
        if flags & 1 != 0 || u16_at(file, at + 34) != 0 {
            return Err(fault(format!("central header at {at}: encrypted entry or nonzero disk")));
        }
        if [compressed, size, local].contains(&u32::MAX) { return Err(fault("ZIP64 entry values are unsupported")); }
        let name_range = span(at + 46, u16_at(file, at + 28) as usize, central.end)?;
        let name = std::str::from_utf8(&file[name_range.clone()]).map_err(|e| fault(format!("entry name is not UTF-8: {e}")))?.to_owned();
        if !names.insert(name.clone()) { return Err(fault(format!("duplicate entry {name}"))); }
        let extra = span(name_range.end, u16_at(file, at + 30) as usize, central.end)?;
        let hashes = extras(file, extra.clone()).map_err(|e| e.at(&name))?;
        let comment = span(extra.end, u16_at(file, at + 32) as usize, central.end)?;
        if method != 8 { return Err(Fault::new(Rule::Deflated, format!("{name}: method {method}, expected 8"))); }
        let local = local as usize;
        span(local, 30, central.start).map_err(|e| e.at(&name))?;
        if u32_at(file, local) != LOCAL { return Err(fault(format!("{name}: invalid local header"))); }
        if u16_at(file, local + 6) != flags || u16_at(file, local + 8) != method {
            return Err(fault(format!("{name}: local flags or method differ from central header")));
        }
        for (delta, expected) in [(14, crc), (18, compressed), (22, size)] {
            let found = u32_at(file, local + delta);
            if found != expected && !(flags & 8 != 0 && found == 0) {
                return Err(fault(format!("{name}: local field at {delta} is {found}, expected {expected}")));
            }
        }
        let local_name = span(local + 30, u16_at(file, local + 26) as usize, central.start)?;
        if file[local_name.clone()] != file[name_range] { return Err(fault(format!("{name}: local name differs"))); }
        let local_extra = span(local_name.end, u16_at(file, local + 28) as usize, central.start)?;
        extras(file, local_extra.clone()).map_err(|e| e.at(&name))?;
        let data = span(local_extra.end, compressed as usize, central.start).map_err(|e| e.at(&name))?;
        entries.push(Place { name, data, size, crc, hashes });
        at = comment.end;
    }
    if at != central.end { return Err(fault(format!("directory entries end at {at}, expected {}", central.end))); }
    Ok(entries)
}
