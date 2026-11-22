//! ELF32 little-endian ET_REL reader. All byte ranges are checked before access.

use crate::{bytes, class::{classify, Class}, u16le, u32le, Error, Location};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub kind: u32,
    pub flags: u32,
    pub size: u32,
    /// Empty for NOBITS; size still includes its eventual zero-filled storage.
    pub data: Vec<u8>,
    pub link: u32,
    pub info: u32,
    pub align: u32,
    pub entsize: u32,
    pub class: Class,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolSection { Undef, Abs, Common, Section(usize) }

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub size: u32,
    pub bind: u8,
    pub kind: u8,
    pub visibility: u8,
    pub shndx: SymbolSection,
}

#[derive(Clone, Debug)]
pub struct Rela {
    pub offset: u32,
    pub kind: u8,
    pub symbol: usize,
    pub symtab: usize,
    pub addend: i32,
    pub source_section: usize,
}

#[derive(Clone, Debug)]
pub struct Object {
    pub archive: String,
    pub member: String,
    pub sections: Vec<Section>,
    /// Symbol indices are relative to their symbol table, including its null symbol.
    pub symbols: BTreeMap<usize, Vec<Symbol>>,
    /// Keyed by relocation target section, preserving source order within it.
    pub relocations: BTreeMap<usize, Vec<Rela>>,
}

impl Object {
    pub fn location(&self, section: usize) -> Location {
        let name = self.sections.get(section).map(|s| if s.name.is_empty() { "<null>" } else { s.name.as_str() }).unwrap_or("<bad-section>");
        Location::new(&self.archive, &self.member, name)
    }

    pub fn symbol(&self, relocation: &Rela) -> Result<&Symbol, Error> {
        self.symbols.get(&relocation.symtab).and_then(|table| table.get(relocation.symbol))
            .ok_or_else(|| self.location(relocation.source_section).at(relocation.offset)
                .error(format!("invalid symbol index {} in table {}", relocation.symbol, relocation.symtab)))
    }
}

struct Header {
    name: u32,
    kind: u32,
    flags: u32,
    offset: u32,
    size: u32,
    link: u32,
    info: u32,
    align: u32,
    entsize: u32,
}

fn string(table: &[u8], offset: u32, at: &Location, what: &str) -> Result<String, Error> {
    let tail = table.get(offset as usize..).ok_or_else(|| at.error(format!("{what} offset {offset:#x} outside string table")))?;
    let end = tail.iter().position(|b| *b == 0).ok_or_else(|| at.error(format!("unterminated {what}")))?;
    let s = std::str::from_utf8(&tail[..end]).map_err(|_| at.error(format!("non-UTF-8 {what}")))?;
    if s.chars().any(|c| c.is_control()) { return Err(at.error(format!("control character in {what}"))); }
    Ok(s.into())
}

fn table_shape(section: &Section, size: u32, at: &Location) -> Result<(), Error> {
    if section.entsize != size || section.size % size != 0 {
        return Err(at.error(format!("entry size {} / byte size {}; expected {size}-byte entries", section.entsize, section.size)));
    }
    Ok(())
}

fn expected(at: &Location, field: &str, value: u32, want: u32) -> Result<(), Error> {
    if value != want { return Err(at.error(format!("{field} = {value:#x}; expected {want:#x}"))); }
    Ok(())
}

pub fn parse(archive: &str, member: &str, data: &[u8]) -> Result<Object, Error> {
    let at = Location::new(archive, member, "<elf-header>");
    let h = bytes(data, 0, 52, &at)?;
    if &h[..4] != b"\x7fELF" { return Err(at.error("invalid ELF magic")); }
    expected(&at, "EI_CLASS", h[4] as u32, 1)?;
    expected(&at, "EI_DATA", h[5] as u32, 1)?;
    expected(&at, "EI_VERSION", h[6] as u32, 1)?;
    expected(&at, "e_type", u16le(h, 16, &at)? as u32, 1)?;
    expected(&at, "e_machine", u16le(h, 18, &at)? as u32, 94)?;
    expected(&at, "e_version", u32le(h, 20, &at)?, 1)?;
    expected(&at, "e_entry", u32le(h, 24, &at)?, 0)?;
    expected(&at, "e_phoff", u32le(h, 28, &at)?, 0)?;
    expected(&at, "e_flags", u32le(h, 36, &at)?, 0x300)?;
    expected(&at, "e_ehsize", u16le(h, 40, &at)? as u32, 52)?;
    expected(&at, "e_phnum", u16le(h, 44, &at)? as u32, 0)?;
    expected(&at, "e_shentsize", u16le(h, 46, &at)? as u32, 40)?;
    let shoff = u32le(h, 32, &at)? as usize;
    let count = u16le(h, 48, &at)? as usize;
    let names_index = u16le(h, 50, &at)? as usize;
    if count == 0 || count >= 0xff00 || names_index == 0 || names_index >= count {
        return Err(at.error(format!("unsupported e_shnum {count}, e_shstrndx {names_index} (extended indices refused)")));
    }
    let mut headers = Vec::new();
    for i in 0..count {
        let at = Location::new(archive, member, &format!("<section #{i}>"));
        let offset = shoff.checked_add(i * 40).ok_or_else(|| at.error("section header offset overflow"))?;
        let h = bytes(data, offset, 40, &at)?;
        expected(&at, "sh_addr", u32le(h, 12, &at)?, 0)?;
        headers.push(Header {
            name: u32le(h, 0, &at)?, kind: u32le(h, 4, &at)?, flags: u32le(h, 8, &at)?,
            offset: u32le(h, 16, &at)?, size: u32le(h, 20, &at)?, link: u32le(h, 24, &at)?,
            info: u32le(h, 28, &at)?, align: u32le(h, 32, &at)?, entsize: u32le(h, 36, &at)?,
        });
    }
    // names_index was checked against count, and exactly count headers were read.
    let nh = &headers[names_index];
    let name_at = Location::new(archive, member, &format!("<shstrtab #{names_index}>"));
    expected(&name_at, "sh_type", nh.kind, 3)?;
    let names = bytes(data, nh.offset as usize, nh.size as usize, &name_at)?;
    let mut sections = Vec::new();
    for (i, h) in headers.iter().enumerate() {
        let provisional = Location::new(archive, member, &format!("<section #{i}>"));
        let name = string(names, h.name, &provisional, "section name")?;
        let at = Location::new(archive, member, if name.is_empty() { "<null>" } else { &name });
        if h.align != 0 && !h.align.is_power_of_two() { return Err(at.error(format!("invalid sh_addralign {}", h.align))); }
        if h.link as usize >= count { return Err(at.error(format!("sh_link {} outside section table", h.link))); }
        if i == 0 && (h.kind != 0 || h.size != 0 || h.name != 0) {
            return Err(at.error("malformed null section"));
        }
        if i != 0 && (h.kind == 0 || name.is_empty()) { return Err(at.error("unexpected null/unnamed section")); }
        let class = classify(&name, h.kind, h.flags, &at)?;
        let contents = if h.kind == 8 { Vec::new() } else {
            bytes(data, h.offset as usize, h.size as usize, &at)?.to_vec()
        };
        sections.push(Section {
            name, kind: h.kind, flags: h.flags, size: h.size, data: contents,
            link: h.link, info: h.info, align: h.align, entsize: h.entsize, class,
        });
    }
    let mut object = Object {
        archive: archive.into(), member: member.into(), sections,
        symbols: BTreeMap::new(), relocations: BTreeMap::new(),
    };
    for (index, section) in object.sections.iter().enumerate().filter(|(_, s)| s.kind == 2) {
        let at = object.location(index);
        table_shape(section, 16, &at)?;
        let strings = &object.sections[section.link as usize]; // sh_link checked above
        expected(&at, "symbol strings sh_type", strings.kind, 3)?;
        let mut symbols = Vec::new();
        for (n, entry) in section.data.chunks_exact(16).enumerate() {
            let at = at.at((n * 16) as u32);
            let name = string(&strings.data, u32le(entry, 0, &at)?, &at, "symbol name")?;
            let value = u32le(entry, 4, &at)?;
            let size = u32le(entry, 8, &at)?;
            let bind = entry[12] >> 4;
            let kind = entry[12] & 15;
            if bind > 2 { return Err(at.error(format!("unsupported symbol binding {bind}"))); }
            if kind > 4 { return Err(at.error(format!("unsupported symbol type {kind}"))); }
            if entry[13] & !3 != 0 { return Err(at.error(format!("unsupported st_other {:#x}", entry[13]))); }
            let raw_index = u16le(entry, 14, &at)?;
            let shndx = match raw_index {
                0 => SymbolSection::Undef,
                0xfff1 => SymbolSection::Abs,
                0xfff2 => {
                    if value != 0 && !value.is_power_of_two() { return Err(at.error(format!("COMMON alignment {value} is not a power of two"))); }
                    SymbolSection::Common
                }
                n if (n as usize) < count && n < 0xff00 => SymbolSection::Section(n as usize),
                _ => return Err(at.error(format!("unsupported st_shndx {raw_index:#x}"))),
            };
            if n == 0 && (entry.iter().any(|b| *b != 0)) { return Err(at.error("malformed null symbol")); }
            if (n < section.info as usize) != (bind == 0) { return Err(at.error("sh_info does not partition local symbols")); }
            if n != 0 && bind != 0 && name.is_empty() { return Err(at.error("global/weak symbol has no name")); }
            symbols.push(Symbol { name, value, size, bind, kind, visibility: entry[13], shndx });
        }
        if symbols.is_empty() || section.info as usize > symbols.len() { return Err(at.error("invalid symbol table sh_info/size")); }
        object.symbols.insert(index, symbols);
    }
    for (index, section) in object.sections.iter().enumerate().filter(|(_, s)| s.kind == 4) {
        let at = object.location(index);
        table_shape(section, 12, &at)?;
        let target = section.info as usize;
        let target_section = object.sections.get(target).filter(|_| target != 0)
            .ok_or_else(|| at.error(format!("invalid relocation sh_info {target}")))?;
        let symbols = object.symbols.get(&(section.link as usize))
            .ok_or_else(|| at.error(format!("relocation sh_link {} is not SYMTAB", section.link)))?;
        for entry in section.data.chunks_exact(12) {
            let offset = u32le(entry, 0, &at)?;
            let info = u32le(entry, 4, &at)?;
            let symbol = (info >> 8) as usize;
            let kind = info as u8;
            let site = object.location(target).at(offset);
            if symbol >= symbols.len() { return Err(site.error(format!("relocation symbol index {symbol} out of bounds"))); }
            let width = if kind == 1 { 4 } else { 1 };
            if offset.checked_add(width).is_none_or(|end| end > target_section.size) {
                return Err(site.error("relocation site outside target section"));
            }
            if target_section.kind == 8 { return Err(site.error("relocation site in NOBITS")); }
            object.relocations.entry(target).or_default().push(Rela {
                offset, kind, symbol, symtab: section.link as usize,
                addend: u32le(entry, 8, &at)? as i32, source_section: index,
            });
        }
    }
    Ok(object)
}
