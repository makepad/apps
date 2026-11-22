//! Version 1 wire format: a 64-byte header and 16-byte segment records.
//! Every field is u32 little-endian except the two u64 hashes. Segment kinds
//! are TEXT=1, RODATA=2, IRAM=3, DATA=4, BSS=5; padding is zero-filled.

use crate::{bytes, u32le, Error, Location};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub magic: u32,
    pub format: u32,
    pub header_size: u32,
    pub image_size: u32,
    pub pack_hash: u64,
    pub abi_hash: u64,
    pub mmu_page: u32,
    pub entry_offset: u32,
    pub entry_count: u32,
    pub gate_count: u32,
    pub gate_table: u32,
    pub segment_offset: u32,
    pub segment_count: u32,
    pub crc: u32,
}

impl Header {
    pub const MAGIC: u32 = 0x454c5349;
    pub const FORMAT: u32 = 1;
    pub const SIZE: u32 = 64;
    /// The S3 flash's erase unit (W25Q128JV, 4 KB sectors). The header and its segment and entry
    /// tables have the image's first sectors to themselves, so a write that puts those sectors last
    /// commits the image by its header: a cut before it leaves no valid header, or the old one.
    pub const SECTOR: u32 = 4096;

    /// Where the image's code begins: the first sector boundary past the header, segment and entry
    /// tables. The bytes between are erased flash (0xFF).
    pub fn body_offset(&self) -> u32 {
        (self.entry_offset + self.entry_count * 4).next_multiple_of(Self::SECTOR)
    }

    pub fn encode(&self) -> [u8; 64] {
        let mut data = [0; 64];
        for (index, value) in [self.magic, self.format, self.header_size, self.image_size,
            self.pack_hash as u32, (self.pack_hash >> 32) as u32,
            self.abi_hash as u32, (self.abi_hash >> 32) as u32,
            self.mmu_page, self.entry_offset, self.entry_count, self.gate_count,
            self.gate_table, self.segment_offset, self.segment_count, self.crc]
            .into_iter().enumerate()
        {
            data[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        data
    }

    /// Decode the header alone; CRC verification requires the complete image.
    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        let at = Location::new("<image>", "<header>", "ISLE");
        let data = bytes(data, 0, Self::SIZE as usize, &at)?;
        let word = |offset| u32le(data, offset, &at);
        let header = Self {
            magic: word(0)?, format: word(4)?, header_size: word(8)?, image_size: word(12)?,
            pack_hash: u64::from(word(16)?) | (u64::from(word(20)?) << 32),
            abi_hash: u64::from(word(24)?) | (u64::from(word(28)?) << 32),
            mmu_page: word(32)?, entry_offset: word(36)?, entry_count: word(40)?,
            gate_count: word(44)?, gate_table: word(48)?, segment_offset: word(52)?,
            segment_count: word(56)?, crc: word(60)?,
        };
        if header.magic != Self::MAGIC || header.format != Self::FORMAT || header.header_size != Self::SIZE {
            return Err(at.error("unsupported ISLE magic, format or header size"));
        }
        if header.image_size < Self::SIZE { return Err(at.error("image size is smaller than header")); }
        for (name, offset, count, stride) in [
            ("entry", header.entry_offset, header.entry_count, 4),
            ("segment", header.segment_offset, header.segment_count, Segment::SIZE),
        ] {
            let end = u64::from(offset) + u64::from(count) * u64::from(stride);
            if offset < Self::SIZE || offset % 4 != 0 || end > u64::from(header.image_size) {
                return Err(at.error(format!("{name} table outside image or misaligned")));
            }
        }
        Ok(header)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SegmentKind { Text = 1, Rodata = 2, Iram = 3, Data = 4, Bss = 5 }

impl SegmentKind {
    pub fn name(self) -> &'static str {
        match self { Self::Text => "TEXT", Self::Rodata => "RODATA", Self::Iram => "IRAM", Self::Data => "DATA", Self::Bss => "BSS" }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub address: u32,
    pub file_offset: u32,
    pub bytes: u32,
}

impl Segment {
    pub const SIZE: u32 = 16;

    pub fn encode(self) -> [u8; 16] {
        let mut data = [0; 16];
        for (index, value) in [self.kind as u32, self.address, self.file_offset, self.bytes].into_iter().enumerate() {
            data[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        data
    }

    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        let at = Location::new("<image>", "<segment>", "ISLE");
        let kind = match u32le(data, 0, &at)? {
            1 => SegmentKind::Text, 2 => SegmentKind::Rodata, 3 => SegmentKind::Iram,
            4 => SegmentKind::Data, 5 => SegmentKind::Bss,
            kind => return Err(at.error(format!("unknown segment kind {kind}"))),
        };
        Ok(Self { kind, address: u32le(data, 4, &at)?, file_offset: u32le(data, 8, &at)?, bytes: u32le(data, 12, &at)? })
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1)); }
    }
    crc ^ u32::MAX
}

pub(crate) struct Fnv(pub u64);

impl Fnv {
    pub fn new() -> Self { Self(0xcbf29ce484222325) }
    pub fn add(&mut self, data: &[u8]) {
        for byte in data { self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x100000001b3); }
    }
}
