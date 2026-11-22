//! The radio island reader, audit and linker. Nothing here executes archive code.
//!
//! The census of ESP-IDF v6.1.1's S3 libraries supplies the relocation rules:
//! R_XTENSA_32 computes S + A + the word in place (including section offsets
//! stored in literal/data words). R2b writes every R_XTENSA_SLOT0_OP field from
//! resolved S + A; the in-place field is only cross-checked. Some call/branch
//! fields hold zero offsets left for the linker. R_XTENSA_ASM_EXPAND
//! marks an L32R followed by CALLXn using the loaded register; the literal's
//! R_XTENSA_32 must identify the same callee. Literal words precede instructions
//! inside text sections; .xt.prop ranges distinguish them from instructions.
//! Uncovered zero fill shorter than four bytes before a 4-aligned range is
//! alignment padding; uncovered trailing bytes still fail. Imports are entered
//! only by CALL8/CALLX8.
//!
//! The audit retains section identities and symbolic expressions; the linker
//! assigns addresses after it passes. Validation covers all members;
//! reachability and imports describe only roots and the sections they reach.
//! COMMON allocations are listed
//! separately and included in kept Bss totals as synthetic sections.
//!
//! Policy choices: names must be UTF-8 without control characters; extended ELF
//! section indices are refused. Equal weak definitions select the first input;
//! all named global/weak references use the selected definition, while local and
//! section-symbol references retain their own object. Unknown roots fail.
//! First import references follow archive/member/section/relocation input order.
//! COMMON alignment zero is normalized to one; COMMON declarations are subject
//! to the same duplicate-strong-definition rule as other definitions.
//! Arithmetic wraps as ELF32 words. Unplaced values retain a base identity and
//! offset, including imports whose map supplies a ROM address. Cross-section
//! SLOT0_OPs are informational; branches to imports have a separate usage tag.
//! Without a map, ABSENT imports are informational. The exact permitted flag
//! values, including RELA's optional SHF_INFO_LINK, are listed in `class`.

pub mod ar;
pub mod audit;
pub mod class;
pub mod elf;
pub mod import_map;
pub mod image;
pub mod glue;
pub mod xt;
mod link;
mod patch;
mod report;
mod resolve;

use std::fmt;

pub use audit::{audit, Archive, Audit};
pub use import_map::ImportMap;
pub use glue::{parse_glue, Glue, GlueRegion, GlueRelocation};
pub use image::{crc32, Header, Segment, SegmentKind};
pub use link::{abi_hash, link, symbolize, Island, IslandBinding, IslandLayout, Region};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    pub archive: String,
    pub member: String,
    pub section: String,
    pub offset: Option<u32>,
}

impl Location {
    pub fn new(archive: &str, member: &str, section: &str) -> Self {
        Self {
            archive: archive.into(), member: member.into(), section: section.into(), offset: None,
        }
    }

    pub fn at(&self, offset: u32) -> Self {
        let mut location = self.clone();
        location.offset = Some(offset);
        location
    }

    pub fn error(&self, message: impl Into<String>) -> Error {
        Error { location: self.clone(), message: message.into() }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({}):{}", self.archive, self.member, self.section)?;
        if let Some(offset) = self.offset { write!(f, "+0x{offset:x}")?; }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub location: Location,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

impl std::error::Error for Error {}

pub(crate) fn bytes<'a>(data: &'a [u8], offset: usize, size: usize, at: &Location) -> Result<&'a [u8], Error> {
    let end = offset.checked_add(size).ok_or_else(|| at.error("byte range overflow"))?;
    data.get(offset..end).ok_or_else(|| at.error(format!(
        "truncated byte range 0x{offset:x}..0x{end:x} (length 0x{:x})", data.len()
    )))
}

pub(crate) fn u16le(data: &[u8], offset: usize, at: &Location) -> Result<u16, Error> {
    let b = bytes(data, offset, 2, at)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn u32le(data: &[u8], offset: usize, at: &Location) -> Result<u32, Error> {
    let b = bytes(data, offset, 4, at)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
