use crate::{audit, audit::{SectionId, SymbolId, Value, ValueBase}, class::Class,
    elf::SymbolSection, image::Fnv, import_map::ImportClass, patch, resolve::Resolver,
    u32le, Archive, Audit, Error, Glue, GlueRegion, Header, ImportMap, Location, Segment, SegmentKind, crc32};
use std::{collections::{BTreeMap, BTreeSet}, fmt::Write, path::Path};

/// FNV-1a 64 of the ordered entry names and gate names; each gate's index
/// is its position. Shared by the island linker and runtime byte producers.
pub fn abi_hash(roots: &[String], gates: &[&str]) -> u64 {
    let mut hash = Fnv::new();
    for root in roots { hash.add(format!("entry {root}\n").as_bytes()); }
    for (index, name) in gates.iter().enumerate() { hash.add(format!("gate {index} {name}\n").as_bytes()); }
    hash.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region { pub start: u32, pub bytes: u32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IslandLayout {
    pub mmu_page: u32,
    pub flash_bytes: u32,
    /// Data-alias base and upper bound of the island's SRAM window.
    pub window: Region,
    /// Instruction-alias address of the same first byte.
    pub window_exec: u32,
}

#[derive(Clone, Debug)]
pub struct Island {
    pub image: Vec<u8>,
    pub header: Header,
    /// Exclusive data-alias end of the occupied window, including the guard.
    pub window_end: u32,
    /// Data-alias address of the final guard word.
    pub guard: u32,
    pub entries: Vec<u32>,
    pub root_names: Vec<String>,
    pub gate_names: Vec<String>,
    /// Resolved glue addresses: the callable entry for text, the object start for data.
    pub glue: BTreeMap<String, u32>,
    /// Named global data objects, with address and size: the resolved global or weak definitions
    /// (not STT_FUNC) in placed Data, Bss or Rodata sections, and selected COMMONs.
    pub objects: BTreeMap<String, Region>,
    /// Selected global and weak STT_FUNC definitions in placed pack sections.
    pub functions: BTreeMap<String, Region>,
    /// Nonempty executable pack sections in fetch address order.
    pub code: Vec<Region>,
    pub map: String,
    pub segments: Vec<Segment>,
    pub audit: Audit,
}

/// The image builder's link inputs. Entry and gate indices follow the wire tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandBinding {
    pub header: Header,
    pub entries: Vec<(String, u32)>,
    pub glue: BTreeMap<String, u32>,
    /// Named global data objects, with address and size: the resolved global or weak definitions
    /// (not STT_FUNC) in placed Data, Bss or Rodata sections, and selected COMMONs.
    pub objects: BTreeMap<String, Region>,
    /// Selected global and weak STT_FUNC definitions in placed pack sections.
    pub functions: BTreeMap<String, Region>,
    /// Nonempty executable pack sections in fetch address order.
    pub code: Vec<Region>,
    pub segments: Vec<Segment>,
    pub guard: u32,
    pub window_end: u32,
    pub gate_names: Vec<String>,
}

/// Map rows are `0x{address:08x} {size} {CLASS}[/FUNC] {name} {at}`.
/// Find a containing map row, preferring a function over other overlapping
/// rows, then the smallest range. Malformed rows and zero-sized symbols are
/// ignored. Subtraction keeps a range ending at 2^32 from wrapping.
pub fn symbolize(map: &str, pc: u32) -> Option<(&str, u32)> {
    let mut best = None;
    let mut rank = None;
    for line in map.lines() {
        let mut fields = line.split_whitespace();
        let Some(address) = fields.next().and_then(|s| s.strip_prefix("0x"))
            .and_then(|s| u32::from_str_radix(s, 16).ok()) else { continue; };
        let Some(size) = fields.next().and_then(|s| s.parse::<u32>().ok()) else { continue; };
        let (Some(kind), Some(name), Some(_at)) = (fields.next(), fields.next(), fields.next()) else { continue; };
        let Some(offset) = pc.checked_sub(address).filter(|&offset| offset < size) else { continue; };
        let candidate = (!kind.ends_with("/FUNC"), size);
        if rank.map_or(true, |previous| candidate < previous) {
            rank = Some(candidate); best = Some((name, offset));
        }
    }
    best
}

impl Island {
    pub fn binding(&self) -> IslandBinding {
        IslandBinding {
            header: self.header.clone(),
            entries: self.root_names.iter().cloned().zip(self.entries.iter().copied()).collect(),
            glue: self.glue.clone(), objects: self.objects.clone(), functions: self.functions.clone(),
            code: self.code.clone(), segments: self.segments.clone(),
            guard: self.guard, window_end: self.window_end, gate_names: self.gate_names.clone(),
        }
    }

    /// Window usage includes the gate table, alignment padding and guard word.
    pub fn size_report(&self, layout: &IslandLayout) -> String {
        let mut report = String::new();
        for (name, bytes, capacity) in [
            ("FLASH", self.image.len() as u64, layout.flash_bytes),
            ("WINDOW", u64::from(self.window_end - self.header.gate_table), layout.window.bytes),
        ] { let _ = writeln!(report, "{name}: {bytes} / {capacity} bytes"); }
        for segment in &self.segments {
            let _ = writeln!(report, "{}: {} bytes at {:#010x}, file {:#x}", segment.kind.name(), segment.bytes, segment.address, segment.file_offset);
        }
        report
    }
}

fn location(name: &str) -> Location { Location::new("<link>", "<generated>", name) }

struct Cursor { name: &'static str, base: u32, capacity: u32, used: u64 }

fn align_up(address: u64, align: u32) -> u64 {
    let align = u64::from(align.max(1));
    (address + align - 1) & !(align - 1)
}

impl Cursor {
    fn new(name: &'static str, region: Region) -> Self { Self { name, base: region.start, capacity: region.bytes, used: 0 } }

    fn overflow(&self, needed: u64) -> Error {
        location(self.name).error(format!("{} overflow: capacity {} bytes, needed {needed} bytes", self.name, self.capacity))
    }

    fn reserve(&mut self, align: u32, bytes: u64) -> Result<Placed, Error> {
        let address = align_up(u64::from(self.base) + self.used, align);
        let offset = address - u64::from(self.base);
        let end = offset.checked_add(bytes).ok_or_else(|| self.overflow(u64::MAX))?;
        if address > u64::from(u32::MAX) || end > u64::from(u32::MAX)
            || u64::from(self.base) + end > u64::from(u32::MAX) + 1
        { return Err(self.overflow(end)); }
        // A zero-size section still has an aligned address but consumes no bytes.
        if bytes != 0 { self.used = end; }
        Ok(Placed { address: address as u32, offset: offset as u32, storage: Storage::Flash })
    }

    fn check(&self) -> Result<(), Error> {
        if self.used > u64::from(self.capacity) { Err(self.overflow(self.used)) } else { Ok(()) }
    }
}

#[derive(Clone, Copy)]
enum Storage { Flash, Iram, Data, Bss }

#[derive(Clone, Copy)]
struct Placed { address: u32, offset: u32, storage: Storage }

impl Placed {
    fn file(self, iram_file: u32, data_file: u32) -> Option<u32> {
        match self.storage {
            Storage::Flash => Some(self.offset), Storage::Iram => iram_file.checked_add(self.offset),
            Storage::Data => data_file.checked_add(self.offset), Storage::Bss => None,
        }
    }
}

/// Measure the complete window even if an object cannot be addressed, so the
/// capacity diagnostic includes all parts. Publish only representable placements.
struct WindowPart { start: u64, end: u64, storage: Storage, address_overflow: bool }

impl WindowPart {
    fn after(previous_end: u64, storage: Storage) -> Self {
        let start = align_up(previous_end, 16);
        Self { start, end: start, storage, address_overflow: false }
    }

    fn bytes(&self) -> u64 { self.end - self.start }

    fn place(&mut self, align: u32, bytes: u64, save: impl FnOnce(Placed)) {
        let address = align_up(self.end, align);
        let end = address + bytes;
        if bytes != 0 { self.end = end; }
        if address > u64::from(u32::MAX) || end > u64::from(u32::MAX) + 1 {
            self.address_overflow = true;
        } else {
            save(Placed { address: address as u32, offset: (address - self.start) as u32, storage: self.storage });
        }
    }
}

struct Addresses {
    sections: BTreeMap<SectionId, Placed>,
    commons: BTreeMap<SymbolId, u32>,
    imports: BTreeMap<String, u32>,
}

impl Addresses {
    fn value(&self, value: Value, symbol: &str, at: &Location) -> Result<u32, Error> {
        let base = match value.base {
            ValueBase::Section(id) => self.sections.get(&id).map(|p| p.address),
            ValueBase::Common(id) => self.commons.get(&id).copied(),
            ValueBase::Import(name) => self.imports.get(&name).copied(),
            ValueBase::Absolute => Some(0),
        }.ok_or_else(|| at.error(format!("linker bug: {symbol} resolves to an unplaced section, COMMON or import")))?;
        Ok(base.wrapping_add(value.offset))
    }
}

struct MapRow { address: u32, size: u32, kind: &'static str, function: bool, name: String, at: Location }

fn class_name(class: Class) -> &'static str {
    match class { Class::Text => "TEXT", Class::Iram => "IRAM", Class::Rodata => "RODATA",
        Class::Dram | Class::Data => "DATA", Class::Bss => "BSS", Class::Meta => "META" }
}

fn put(image: &mut [u8], offset: u32, data: &[u8], at: &Location) -> Result<(), Error> {
    let start = offset as usize;
    let end = start.checked_add(data.len()).ok_or_else(|| at.error("image write overflow"))?;
    image.get_mut(start..end).ok_or_else(|| at.error("image write outside placed extent"))?.copy_from_slice(data);
    Ok(())
}

fn count(value: usize, what: &str) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| location(what).error(format!("{what} count exceeds u32")))
}

/// Audit first, then place and patch only reached sections. Failed audits return
/// all their diagnostics in Error.message, with the first failure's location.
/// Each segment spans its whole region group, including zero alignment padding.
/// Metadata precedes TEXT and is not itself a loadable segment. Zero-size
/// sections have map addresses but do not extend segments. IRAM uses the
/// instruction alias of its window offset. Glue order is the caller's order
/// within each glue region.
pub fn link(archives: &[Archive<'_>], roots: &[String], map: &ImportMap, glue: &[Glue], layout: &IslandLayout) -> Result<Island, Error> {
    // Glue targets are reach roots only; entries and the ABI hash use roots.
    let mut reach_roots = roots.to_vec();
    for object in glue {
        object.validate().map_err(|message| location(&object.name).error(message))?;
        reach_roots.extend(object.relocations.iter().filter(|r| !map.entries.contains_key(&r.symbol))
            .map(|r| r.symbol.clone()));
    }
    let audit = audit(archives, &reach_roots, Some(map))?;
    let resolver = Resolver::new(&audit.objects)?;
    let mut glue_targets = BTreeMap::new();
    // Check before placement, and before reporting an undefined reach root, so
    // every forbidden glue target is diagnosed at its object and word offset.
    for object in glue {
        let mut offsets = BTreeSet::new();
        for relocation in &object.relocations {
            let at = location(&object.name).at(relocation.offset);
            let fail = |reason: &str| at.error(format!("glue relocation {}: {reason}", relocation.symbol));
            if relocation.offset % 4 != 0 { return Err(fail("reloc offset must be a multiple of 4")); }
            if relocation.offset.checked_add(4).map_or(true, |end| end as usize > object.bytes.len()) {
                return Err(fail("reloc offset is past the end"));
            }
            if !offsets.insert(relocation.offset) { return Err(fail("duplicate reloc offset")); }
            // Selected definitions take precedence over imports, as in pack relocations.
            let value = if let Some(definition) = resolver.globals.get(&relocation.symbol) {
                match resolver.symbol(definition.id).shndx {
                    SymbolSection::Section(section) => {
                        if !audit.kept.contains(&SectionId { object: definition.id.object, section }) {
                            return Err(fail("pack definition is not in a kept section"));
                        }
                    }
                    SymbolSection::Common => {
                        if !audit.commons.iter().any(|c| c.symbol == definition.id) {
                            return Err(fail("pack definition is not a selected COMMON"));
                        }
                    }
                    SymbolSection::Abs => return Err(fail("glue may not reach an absolute (SHN_ABS) symbol")),
                    SymbolSection::Undef => return Err(fail("undefined symbol")),
                }
                resolver.value(definition.id, 0)
            } else {
                match map.entries.get(&relocation.symbol) {
                    Some(ImportClass::Gate(_) | ImportClass::Glue) => Value { base: ValueBase::Import(relocation.symbol.clone()), offset: 0 },
                    Some(ImportClass::Rom(_)) => return Err(fail("glue may not reach the ROM")),
                    Some(ImportClass::WeakAbsent) => return Err(fail("glue may not reach a weak-absent import")),
                    None => return Err(fail("undefined symbol")),
                }
            };
            glue_targets.insert((object.name.as_str(), relocation.offset), value);
        }
    }
    if let Some(first) = audit.failures.first() {
        return Err(first.location.error(format!("audit failed:\n{}", audit.failures.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))));
    }
    let mut gates = BTreeMap::new();
    for (name, class) in &map.entries {
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(location(name).error("invalid import name"));
        }
        if let ImportClass::Gate(index) = class {
            if gates.insert(*index, name.as_str()).is_some() {
                return Err(location(name).error(format!("duplicate gate index {index}")));
            }
        }
    }
    for (expected, index) in gates.keys().enumerate() {
        if *index != expected as u32 { return Err(location("gates").error(format!("missing gate index {expected}"))); }
    }
    let gate_count = count(gates.len(), "gate")?;
    if layout.window.start % 16 != 0 || layout.window_exec % 16 != 0 {
        return Err(location("WINDOW").error("window and window_exec must be 16-byte aligned"));
    }
    let mut glue_names = BTreeMap::new();
    for object in glue {
        let at = location(&object.name);
        if map.entries.get(&object.name) != Some(&ImportClass::Glue) {
            return Err(at.error(format!("extra glue object {}: no glue entry in map", object.name)));
        }
        if glue_names.insert(object.name.as_str(), object).is_some() {
            return Err(at.error(format!("duplicate glue object {}", object.name)));
        }
        if !object.align.is_power_of_two() { return Err(at.error(format!("glue {} has invalid alignment {}", object.name, object.align))); }
        count(object.bytes.len(), "glue bytes")?;
    }
    for (name, class) in &map.entries {
        if *class == ImportClass::Glue && !glue_names.contains_key(name.as_str()) {
            return Err(location(name).error(format!("missing glue object {name}")));
        }
    }
    // Even an unreferenced nonempty literal section contradicts this pack contract.
    for object in &audit.objects {
        for (index, section) in object.sections.iter().enumerate() {
            if section.name.starts_with(".literal") && section.size != 0 {
                return Err(object.location(index).error("literal sections are empty in the census; this pack needs a linker change"));
            }
        }
    }
    let page = layout.mmu_page.checked_mul(0x1_0000).ok_or_else(|| location("FLASH").error("MMU page address overflow"))?;
    let exec_base = 0x4200_0000u32.checked_add(page).ok_or_else(|| location("FLASH").error("execute alias overflow"))?;
    let read_base = 0x3c00_0000u32.checked_add(page).ok_or_else(|| location("FLASH").error("read alias overflow"))?;
    let mut addresses = Addresses { sections: BTreeMap::new(), commons: BTreeMap::new(), imports: BTreeMap::new() };
    let mut rows = Vec::new();
    let window_start = u64::from(layout.window.start);
    let window_exec = u64::from(layout.window_exec);
    let gate_bytes = u64::from(gate_count) * 4;
    let mut iram = WindowPart::after(window_exec + gate_bytes, Storage::Iram);
    for id in &audit.kept {
        let section = &audit.objects[id.object].sections[id.section];
        if section.class == Class::Iram {
            iram.place(section.align.max(4), u64::from(section.size), |placed| {
                addresses.sections.insert(*id, placed);
            });
        }
    }
    let mut data = WindowPart::after(window_start + (iram.end - window_exec), Storage::Data);
    for id in &audit.kept {
        let section = &audit.objects[id.object].sections[id.section];
        if matches!(section.class, Class::Dram | Class::Data) {
            data.place(section.align, u64::from(section.size), |placed| {
                addresses.sections.insert(*id, placed);
            });
        }
    }
    let mut glue_placements = Vec::new();
    for object in glue.iter().filter(|g| g.region == GlueRegion::Dram) {
        data.place(object.align, object.bytes.len() as u64, |placed| {
            addresses.imports.insert(object.name.clone(), placed.address);
            glue_placements.push((object, placed));
        });
    }
    let mut bss = WindowPart::after(data.end, Storage::Bss);
    for id in &audit.kept {
        let section = &audit.objects[id.object].sections[id.section];
        if section.class == Class::Bss {
            bss.place(section.align, u64::from(section.size), |placed| {
                addresses.sections.insert(*id, placed);
            });
        }
    }
    for common in &audit.commons {
        bss.place(common.align, u64::from(common.size), |placed| {
            addresses.commons.insert(common.symbol, placed.address);
        });
    }
    for object in glue.iter().filter(|g| g.region == GlueRegion::Bss) {
        bss.place(object.align, object.bytes.len() as u64, |placed| {
            addresses.imports.insert(object.name.clone(), placed.address);
            glue_placements.push((object, placed));
        });
    }
    let guard = align_up(bss.end, 16);
    let needed = guard + 4 - window_start;
    let (iram_bytes, data_bytes, bss_bytes) = (iram.bytes(), data.bytes(), bss.bytes());
    if needed > u64::from(layout.window.bytes) {
        let padding = needed - gate_bytes - iram_bytes - data_bytes - bss_bytes - 4;
        return Err(location("WINDOW").error(format!(
            "WINDOW overflow: capacity {} bytes, needed {needed} bytes (gate table {gate_bytes} bytes, \
             IRAM {iram_bytes} bytes, DATA {data_bytes} bytes, BSS {bss_bytes} bytes, \
             guard 4 bytes, inter-part padding {padding} bytes)",
            layout.window.bytes)));
    }
    let window_end = u32::try_from(guard + 4).map_err(|_| location("WINDOW").error("exclusive window end exceeds the address space"))?;
    for (name, part) in [("IRAM", &iram), ("DATA", &data), ("BSS", &bss)] {
        if part.address_overflow { return Err(location(name).error(format!("{name} placement exceeds the address space"))); }
    }
    let guard = guard as u32;
    let has = |class| audit.kept.iter().any(|id| {
        let section = &audit.objects[id.object].sections[id.section];
        section.class == class && section.size != 0
    });
    let text_present = has(Class::Text) || gate_count != 0
        || glue.iter().any(|g| g.region == GlueRegion::Text && !g.bytes.is_empty());
    let rodata_present = has(Class::Rodata) || glue.iter().any(|g| g.region == GlueRegion::Rodata && !g.bytes.is_empty());
    let segment_count = [text_present, rodata_present, iram_bytes != 0, data_bytes != 0, bss_bytes != 0].into_iter().filter(|v| *v).count() as u32;
    let entry_count = count(roots.len(), "entry")?;
    let entry_offset = Header::SIZE + segment_count * Segment::SIZE;
    let mut flash = Cursor::new("FLASH", Region { start: exec_base, bytes: layout.flash_bytes });
    let tables_end = u64::from(entry_offset) + u64::from(entry_count) * 4;
    // The tables keep their sectors to themselves (`Header::body_offset`): the code starts at the
    // next sector boundary, and the bytes between are erased flash.
    flash.reserve(4, tables_end.next_multiple_of(u64::from(Header::SECTOR)))?;
    let text_offset = flash.used as u32;
    for id in &audit.kept {
        let section = &audit.objects[id.object].sections[id.section];
        if section.class == Class::Text {
            addresses.sections.insert(*id, flash.reserve(section.align.max(4), u64::from(section.size))?);
        }
    }
    let gate_block = flash.reserve(4, u64::from(gate_count) * 16)?;
    let stub_offset = gate_block.offset + gate_count * 4;
    for (index, name) in &gates {
        addresses.imports.insert((*name).into(), exec_base + stub_offset + index * 12);
    }
    for object in glue.iter().filter(|g| g.region == GlueRegion::Text) {
        let placed = flash.reserve(object.align, object.bytes.len() as u64)?;
        addresses.imports.insert(object.name.clone(), placed.address + object.entry);
        glue_placements.push((object, placed));
    }
    let text_bytes = flash.used as u32 - text_offset;
    let rodata_offset = flash.used as u32;
    flash.base = read_base;
    for id in &audit.kept {
        let section = &audit.objects[id.object].sections[id.section];
        if section.class == Class::Rodata {
            addresses.sections.insert(*id, flash.reserve(section.align, u64::from(section.size))?);
        }
    }
    for object in glue.iter().filter(|g| g.region == GlueRegion::Rodata) {
        let placed = flash.reserve(object.align, object.bytes.len() as u64)?;
        addresses.imports.insert(object.name.clone(), placed.address);
        glue_placements.push((object, placed));
    }
    let rodata_bytes = flash.used as u32 - rodata_offset;
    flash.base = exec_base;
    let iram_file = flash.reserve(4, iram_bytes)?.offset;
    let data_file = flash.reserve(4, data_bytes)?.offset;
    flash.check()?;
    for (name, class) in &map.entries {
        match class {
            ImportClass::Rom(address) => { addresses.imports.insert(name.clone(), *address); }
            ImportClass::WeakAbsent => { addresses.imports.insert(name.clone(), 0); }
            _ => {},
        }
    }
    let mut entries = Vec::new();
    for root in roots {
        let definition = resolver.globals.get(root).ok_or_else(|| location(root).error(format!("root {root} has no definition")))?;
        let symbol = resolver.symbol(definition.id);
        let SymbolSection::Section(section) = symbol.shndx else {
            return Err(location(root).error(format!("root {root} must resolve to Text or Iram")));
        };
        if !matches!(audit.objects[definition.id.object].sections[section].class, Class::Text | Class::Iram) {
            return Err(location(root).error(format!("root {root} must resolve to Text or Iram")));
        }
        entries.push(addresses.value(resolver.value(definition.id, 0), root, &location(root))?);
    }
    let mut segments = Vec::new();
    for (kind, address, file_offset, bytes) in [
        (SegmentKind::Text, exec_base + text_offset, text_offset, text_bytes),
        (SegmentKind::Rodata, read_base + rodata_offset, rodata_offset, rodata_bytes),
        (SegmentKind::Iram, iram.start as u32, iram_file, iram_bytes as u32),
        (SegmentKind::Data, data.start as u32, data_file, data_bytes as u32),
        (SegmentKind::Bss, bss.start as u32, 0, bss_bytes as u32),
    ] {
        if bytes != 0 { segments.push(Segment { kind, address, file_offset, bytes }); }
    }
    if segments.len() != segment_count as usize { return Err(location("segments").error("linker bug: segment count changed during placement")); }
    let mut image = Vec::new();
    image.try_reserve_exact(flash.used as usize).map_err(|e| location("FLASH").error(format!("image allocation: {e}")))?;
    image.resize(flash.used as usize, 0);
    image[tables_end as usize..text_offset as usize].fill(0xff);
    for (index, segment) in segments.iter().enumerate() {
        put(&mut image, Header::SIZE + index as u32 * Segment::SIZE, &segment.encode(), &location("segments"))?;
    }
    for (index, entry) in entries.iter().enumerate() {
        put(&mut image, entry_offset + index as u32 * 4, &entry.to_le_bytes(), &location("entries"))?;
    }
    let mut code = Vec::new();
    for (id, placed) in &addresses.sections {
        let object = &audit.objects[id.object];
        let section = &object.sections[id.section];
        let at = object.location(id.section);
        if matches!(section.class, Class::Text | Class::Iram) && section.size != 0 {
            code.push(Region { start: placed.address, bytes: section.size });
        }
        if section.size != 0 && section.class != Class::Bss {
            let file = placed.file(iram_file, data_file).ok_or_else(|| at.error("linker bug: missing initial image"))?;
            put(&mut image, file, &section.data, &at)?;
        }
        rows.push(MapRow { address: placed.address, size: section.size, kind: class_name(section.class), function: false, name: section.name.clone(), at });
    }
    code.sort_by_key(|r| r.start);
    for (object, placed) in &glue_placements {
        let at = location(&object.name);
        if object.region != GlueRegion::Bss && !object.bytes.is_empty() {
            let file = placed.file(iram_file, data_file).ok_or_else(|| at.error("linker bug: missing glue image"))?;
            put(&mut image, file, &object.bytes, &at)?;
            for relocation in &object.relocations {
                let at = at.at(relocation.offset);
                let target = addresses.value(glue_targets[&(object.name.as_str(), relocation.offset)].clone(), &relocation.symbol, &at)?;
                let word = if object.region == GlueRegion::Text {
                    target.wrapping_add(u32le(&object.bytes, relocation.offset as usize, &at)?)
                } else { target };
                let file = file.checked_add(relocation.offset).ok_or_else(|| at.error("glue relocation image offset overflow"))?;
                put(&mut image, file, &word.to_le_bytes(), &at)?;
            }
        }
        rows.push(MapRow { address: placed.address, size: object.bytes.len() as u32, kind: "GLUE", function: object.region == GlueRegion::Text, name: object.name.clone(), at });
    }
    for (index, name) in &gates {
        let at = location(name);
        let offset = stub_offset + index * 12;
        let literal = gate_block.offset + index * 4;
        let stub = patch::gate(exec_base + offset, exec_base + literal, name, &at)?;
        put(&mut image, literal, &(layout.window.start + index * 4).to_le_bytes(), &at)?;
        put(&mut image, offset, &stub, &at)?;
        rows.push(MapRow { address: exec_base + offset, size: 12, kind: "GATE", function: false, name: (*name).into(), at });
    }
    for import in audit.imports.values() {
        let kind = match import.class {
            Some(ImportClass::Rom(_)) => "ROM",
            Some(ImportClass::WeakAbsent) => "WEAK-ABSENT",
            _ => continue, // Gates and glue already have their own rows.
        };
        rows.push(MapRow {
            address: addresses.imports[&import.name], size: 0, kind, function: false, name: import.name.clone(),
            at: Location::new("<imports>", "<generated>", &import.name),
        });
    }
    for id in &audit.kept {
        let object = &audit.objects[id.object];
        let placed = addresses.sections.get(id).ok_or_else(|| object.location(id.section).error("linker bug: kept section not placed"))?;
        for relocation in object.relocations.get(&id.section).into_iter().flatten() {
            if relocation.kind == 11 { continue; }
            let at = object.location(id.section).at(relocation.offset);
            let symbol_id = SymbolId { object: id.object, table: relocation.symtab, index: relocation.symbol };
            let symbol = resolver.symbol(symbol_id);
            let name = if !symbol.name.is_empty() {
                symbol.name.as_str()
            } else if let SymbolSection::Section(section) = symbol.shndx {
                object.sections[section].name.as_str()
            } else { "<unnamed-symbol>" };
            let target = addresses.value(resolver.value(symbol_id, relocation.addend as u32), name, &at)?;
            let file = placed.file(iram_file, data_file).and_then(|f| f.checked_add(relocation.offset))
                .ok_or_else(|| at.error("linker bug: relocation has no image location"))?;
            match relocation.kind {
                1 => {
                    let original = u32le(&object.sections[id.section].data, relocation.offset as usize, &at)?;
                    put(&mut image, file, &target.wrapping_add(original).to_le_bytes(), &at)?;
                }
                20 => {
                    let original = audit.decoded[id.object].instructions.get(&id.section).and_then(|m| m.get(&relocation.offset))
                        .ok_or_else(|| at.error("linker bug: missing audited instruction"))?;
                    let pc = placed.address.checked_add(relocation.offset).ok_or_else(|| at.error("instruction address overflow"))?;
                    let bits = patch::slot(original, pc, target, name, &at)?;
                    put(&mut image, file, &bits.to_le_bytes()[..original.length as usize], &at)?;
                }
                _ => return Err(at.error("linker bug: unsupported audited relocation")),
            }
        }
    }
    // Named definitions in placed sections, selected COMMONs and absolute
    // symbols are described. Unselected global/weak definitions are omitted;
    // local symbols retain their object identity in the location column.
    let mut objects = BTreeMap::new();
    let mut functions = BTreeMap::new();
    for (object_index, object) in audit.objects.iter().enumerate() {
        for (table, symbols) in &object.symbols {
            for (index, symbol) in symbols.iter().enumerate() {
                if symbol.name.is_empty() || symbol.shndx == SymbolSection::Undef { continue; }
                let id = SymbolId { object: object_index, table: *table, index };
                if resolver.definition(id) != Some(id) { continue; }
                let (address, class, at) = match symbol.shndx {
                    SymbolSection::Section(section) => {
                        let Some(placed) = addresses.sections.get(&SectionId { object: object_index, section }) else { continue; };
                        (placed.address.wrapping_add(symbol.value), Some(object.sections[section].class), object.location(section))
                    }
                    SymbolSection::Common => {
                        let Some(address) = addresses.commons.get(&id) else { continue; };
                        (*address, Some(Class::Bss), Location::new(&object.archive, &object.member, "COMMON"))
                    }
                    SymbolSection::Abs => (symbol.value, None, Location::new(&object.archive, &object.member, "ABS")),
                    SymbolSection::Undef => continue,
                };
                let function = symbol.kind == 2;
                if matches!(symbol.bind, 1 | 2) {
                    let region = Region { start: address, bytes: symbol.size };
                    if function && matches!(symbol.shndx, SymbolSection::Section(_)) {
                        functions.insert(symbol.name.clone(), region);
                    } else if !function && matches!(class, Some(Class::Data | Class::Dram | Class::Bss | Class::Rodata)) {
                        objects.insert(symbol.name.clone(), region);
                    }
                }
                // Preserve CLASS/FUNC so symbolization prefers a function over overlapping rows.
                rows.push(MapRow { address, size: symbol.size, kind: class.map_or("ABS", class_name), function, name: symbol.name.clone(), at });
            }
        }
    }
    rows.sort_by(|a, b| (a.address, &a.name).cmp(&(b.address, &b.name)));
    let mut symbol_map = String::new();
    for row in rows {
        let function = if row.function { "/FUNC" } else { "" };
        let _ = writeln!(symbol_map, "0x{:08x} {} {}{} {} {}", row.address, row.size, row.kind, function, row.name, row.at);
    }
    let mut pack_hash = Fnv::new();
    for archive in archives {
        let name = Path::new(archive.name).file_name().and_then(|n| n.to_str())
            .ok_or_else(|| location("pack hash").error("archive has no UTF-8 file name"))?;
        pack_hash.add(&count(name.len(), "archive name")?.to_le_bytes());
        pack_hash.add(name.as_bytes());
        pack_hash.add(&(archive.data.len() as u64).to_le_bytes());
        pack_hash.add(archive.data);
    }
    let gate_names: Vec<_> = gates.values().copied().collect();
    let header = Header {
        magic: Header::MAGIC, format: Header::FORMAT, header_size: Header::SIZE,
        image_size: image.len() as u32, pack_hash: pack_hash.0, abi_hash: abi_hash(roots, &gate_names),
        mmu_page: layout.mmu_page, entry_offset, entry_count, gate_count,
        gate_table: layout.window.start, segment_offset: Header::SIZE, segment_count,
        crc: crc32(&image[Header::SIZE as usize..]),
    };
    put(&mut image, 0, &header.encode(), &location("header"))?;
    let glue = glue.iter().map(|object| (object.name.clone(), addresses.imports[&object.name])).collect();
    Ok(Island { image, header, window_end, guard, entries, root_names: roots.to_vec(),
        gate_names: gate_names.into_iter().map(str::to_owned).collect(), glue, objects, functions, code,
        map: symbol_map, segments, audit })
}
