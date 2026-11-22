//! Whole-pack validation followed by section reachability and import classification.

use crate::{ar, class::Class, elf::{self, Object, Rela, SymbolSection}, import_map::{ImportClass, ImportMap}, resolve::Resolver, u32le, xt::{self, Decoded, Instruction}, Error, Location};
use fab_elec_mcu_xtensa::insn::Op;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> { pub name: &'a str, pub data: &'a [u8] }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SectionId { pub object: usize, pub section: usize }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolId { pub object: usize, pub table: usize, pub index: usize }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueBase { Section(SectionId), Common(SymbolId), Import(String), Absolute }

/// An unplaced address. Offset arithmetic is modulo 2^32, as for ELF32 words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value { pub base: ValueBase, pub offset: u32 }

#[derive(Clone, Debug)]
pub struct Word {
    pub location: Location,
    pub symbol: SymbolId,
    pub addend: i32,
    pub in_place: u32,
    pub value: Value,
    pub kept: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Use { DataWord, Call, Load, Branch }

#[derive(Clone, Debug)]
pub struct Import {
    pub name: String,
    pub uses: BTreeSet<Use>,
    pub first: Location,
    /// True only when every reached reference is weak.
    pub weak: bool,
    pub class: Option<ImportClass>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub symbol: String,
    pub size: u8,
    pub location: Location,
    pub indirect: bool,
}

#[derive(Clone, Debug)]
pub struct Common {
    pub symbol: SymbolId,
    pub name: String,
    pub size: u32,
    pub align: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size { pub sections: u64, pub bytes: u64 }

impl Size {
    fn add(&mut self, bytes: u32) { self.sections += 1; self.bytes += u64::from(bytes); }
}

#[derive(Clone, Debug)]
pub struct CrossSection {
    pub location: Location,
    pub symbol: String,
}

#[derive(Clone, Debug)]
pub struct Slot0Unwritten {
    pub location: Location,
    pub symbol: String,
    /// Resolved S + A, relative to the section containing the instruction.
    pub target: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Audit {
    pub objects: Vec<Object>,
    pub decoded: Vec<Decoded>,
    pub counts: BTreeMap<Class, Size>,
    pub relocations: BTreeMap<u8, u64>,
    pub relocations_by_class: BTreeMap<(u8, Class), u64>,
    pub kept: BTreeSet<SectionId>,
    pub commons: Vec<Common>,
    pub kept_sizes: BTreeMap<Class, Size>,
    pub kept_by_archive: BTreeMap<String, BTreeMap<Class, Size>>,
    pub imports: BTreeMap<String, Import>,
    /// Import calls in kept sections. All sites, including dropped ones, are checked.
    pub calls: Vec<Call>,
    pub words: Vec<Word>,
    pub asm_expand_passes: u64,
    /// Sites whose existing field agrees with resolved S + A.
    pub slot0_passes: u64,
    /// Zero-offset placeholders. R2b writes these and every other SLOT0_OP field.
    pub slot0_unwritten: Vec<Slot0Unwritten>,
    /// Informational: R2b decides how to link these; they are not audit failures.
    pub cross_section: Vec<CrossSection>,
    pub instruction_kinds: BTreeMap<String, u64>,
    pub failures: Vec<Error>,
}

impl Audit {
    pub fn passed(&self) -> bool { self.failures.is_empty() }

    pub fn kept_members(&self) -> BTreeSet<usize> {
        self.kept.iter().map(|s| s.object).chain(self.commons.iter().map(|c| c.symbol.object)).collect()
    }
}

fn symbol_id(object: usize, relocation: &Rela) -> SymbolId {
    SymbolId { object, table: relocation.symtab, index: relocation.symbol }
}

fn keep_definition(resolver: &Resolver<'_>, id: SymbolId, kept: &mut BTreeSet<SectionId>, commons: &mut BTreeSet<SymbolId>, queue: &mut VecDeque<SectionId>) {
    match resolver.symbol(id).shndx {
        SymbolSection::Section(section) => {
            let section_id = SectionId { object: id.object, section };
            if resolver.objects[id.object].sections[section].class != Class::Meta && kept.insert(section_id) {
                queue.push_back(section_id);
            }
        }
        SymbolSection::Common => { commons.insert(id); }
        SymbolSection::Undef | SymbolSection::Abs => {},
    }
}

fn reach(resolver: &Resolver<'_>, roots: &[String], result: &mut Audit) {
    let mut queue = VecDeque::new();
    let mut commons = BTreeSet::new();
    for root in roots {
        if let Some(definition) = resolver.globals.get(root) {
            keep_definition(resolver, definition.id, &mut result.kept, &mut commons, &mut queue);
        } else {
            result.failures.push(Location::new("<roots>", "<root>", root).error(format!("root {root} has no definition")));
        }
    }
    while let Some(id) = queue.pop_front() {
        let object = &resolver.objects[id.object];
        for relocation in object.relocations.get(&id.section).into_iter().flatten() {
            if let Some(definition) = resolver.definition(symbol_id(id.object, relocation)) {
                keep_definition(resolver, definition, &mut result.kept, &mut commons, &mut queue);
            }
        }
    }
    for id in commons {
        let symbol = resolver.symbol(id);
        result.commons.push(Common { symbol: id, name: symbol.name.clone(), size: symbol.size, align: symbol.value.max(1) });
    }
}

fn instruction_at(decoded: &Decoded, section: usize, offset: u32) -> Option<&Instruction> {
    decoded.instructions.get(&section)?.get(&offset)
}

struct LongCall<'a> {
    instruction: &'a Instruction,
    literal_offset: u32,
}

fn long_call<'a>(resolver: &Resolver<'_>, decoded: &'a Decoded, object: usize, section: usize, relocation: &Rela) -> Result<LongCall<'a>, Error> {
    let obj = &resolver.objects[object];
    let at = obj.location(section).at(relocation.offset);
    let load = instruction_at(decoded, section, relocation.offset)
        .ok_or_else(|| at.error("ASM_EXPAND site is not on an instruction boundary"))?;
    let insn = load.decoded.filter(|i| i.op == Op::L32r).ok_or_else(|| at.error("ASM_EXPAND site is not L32R"))?;
    let next = relocation.offset.checked_add(load.length).ok_or_else(|| at.error("ASM_EXPAND next instruction overflow"))?;
    let call = instruction_at(decoded, section, next).ok_or_else(|| at.error("ASM_EXPAND not followed by CALLXn"))?;
    let call_insn = call.decoded.filter(|i| matches!(i.op, Op::Callx0 | Op::Callx4 | Op::Callx8 | Op::Callx12))
        .ok_or_else(|| at.error("ASM_EXPAND not followed by CALLXn"))?;
    if insn.t != call_insn.s { return Err(at.error("ASM_EXPAND CALLXn uses a different register from L32R")); }
    let literal_offset = load.target().ok_or_else(|| at.error("ASM_EXPAND L32R has no target"))?;
    let relocs = obj.relocations.get(&section).map(Vec::as_slice).unwrap_or(&[]);
    let mut literals = relocs.iter().filter(|r| r.offset == literal_offset && r.kind == 1);
    let literal = literals.next().ok_or_else(|| at.error(format!("ASM_EXPAND literal at {literal_offset:#x} lacks R_XTENSA_32")))?;
    if literals.next().is_some() { return Err(at.error("ASM_EXPAND literal has multiple R_XTENSA_32 relocations")); }
    let in_place = u32le(&obj.sections[section].data, literal_offset as usize, &at)?;
    if !resolver.same_callee(symbol_id(object, relocation), relocation.addend as u32,
        symbol_id(object, literal), (literal.addend as u32).wrapping_add(in_place))
    {
        let a = resolver.symbol(symbol_id(object, relocation));
        let b = resolver.symbol(symbol_id(object, literal));
        return Err(at.error(format!("ASM_EXPAND literal names another callee: {:?} versus {:?} at {literal_offset:#x}", a.name, b.name)));
    }
    Ok(LongCall { instruction: call, literal_offset })
}

fn slot(resolver: &Resolver<'_>, decoded: &Decoded, object: usize, section: usize, relocation: &Rela, result: &mut Audit) {
    let obj = &resolver.objects[object];
    let at = obj.location(section).at(relocation.offset);
    let Some(instruction) = instruction_at(decoded, section, relocation.offset) else {
        result.failures.push(at.error("SLOT0_OP site is not on an instruction boundary")); return;
    };
    if !instruction.slot_format() {
        result.failures.push(at.error(format!("SLOT0_OP unsupported instruction format {}", instruction.kind))); return;
    }
    let id = symbol_id(object, relocation);
    let symbol = resolver.symbol(id);
    let Some(definition) = resolver.definition(id).filter(|definition| {
        definition.object == object && resolver.symbol(*definition).shndx == SymbolSection::Section(section)
    }) else {
        result.cross_section.push(CrossSection { location: at, symbol: symbol.name.clone() });
        return;
    };
    let expected = resolver.symbol(definition).value.wrapping_add(relocation.addend as u32);
    let target = instruction.target();
    let zero_offset_target = match instruction.op() {
        Some(Op::Call0 | Op::Call4 | Op::Call8 | Op::Call12) => Some((instruction.offset & !3).wrapping_add(4)),
        Some(Op::L32r) | None => None, // L32R and the locally recognized LOOP forms cannot be unwritten.
        _ => Some(instruction.offset.wrapping_add(4)), // J and the branch formats accepted above.
    };
    if target == Some(expected) {
        result.slot0_passes += 1;
    } else if zero_offset_target.is_some() && target == zero_offset_target {
        result.slot0_unwritten.push(Slot0Unwritten { location: at, symbol: symbol.name.clone(), target: expected });
    } else {
        result.failures.push(at.error(format!("SLOT0_OP in-place target {target:?} differs from S + A = {expected:#x}")));
    }
}

fn import_use(resolver: &Resolver<'_>, id: SymbolId, at: Location, uses: &BTreeSet<Use>, map: Option<&ImportMap>, result: &mut Audit) {
    let Some(symbol) = resolver.import(id) else { return; };
    let entry = result.imports.entry(symbol.name.clone()).or_insert_with(|| Import {
        name: symbol.name.clone(), uses: BTreeSet::new(), first: at, weak: true,
        class: map.and_then(|m| m.entries.get(&symbol.name)).cloned(),
    });
    entry.weak &= symbol.bind == 2;
    entry.uses.extend(uses);
}

fn call_import(resolver: &Resolver<'_>, id: SymbolId, instruction: &Instruction, at: Location, indirect: bool, kept: bool, result: &mut Audit) {
    let Some(symbol) = resolver.import(id) else { return; };
    let Some(size) = instruction.call_size() else { return; };
    if size != 8 {
        result.failures.push(at.error(format!("{} into import {} uses forbidden CALL{size} window", instruction.kind, symbol.name)));
    }
    if kept { result.calls.push(Call { symbol: symbol.name.clone(), size, location: at, indirect }); }
}

/// Parse and validate the entire pack. Structural errors return Err; independent
/// relocation failures are collected in Audit so the CLI can report all sites.
/// Input order breaks ties between weak definitions. COMMON is a definition too:
/// two strong declarations are refused, and a selected COMMON owns a Bss allocation.
pub fn audit(archives: &[Archive<'_>], roots: &[String], map: Option<&ImportMap>) -> Result<Audit, Error> {
    let mut objects = Vec::new();
    for archive in archives {
        for member in ar::parse(archive.name, archive.data)? {
            objects.push(elf::parse(archive.name, &member.name, member.data)?);
        }
    }
    let resolver = Resolver::new(&objects)?;
    let mut result = Audit::default();
    reach(&resolver, roots, &mut result);
    for (object, obj) in objects.iter().enumerate() {
        let decoded = xt::inspect(obj);
        result.failures.extend(decoded.failures.iter().cloned());
        for (section, s) in obj.sections.iter().enumerate() {
            result.counts.entry(s.class).or_default().add(s.size);
            let kept = result.kept.contains(&SectionId { object, section });
            if kept {
                result.kept_sizes.entry(s.class).or_default().add(s.size);
                result.kept_by_archive.entry(obj.archive.clone()).or_default().entry(s.class).or_default().add(s.size);
                for instruction in decoded.instructions.get(&section).into_iter().flat_map(|m| m.values()) {
                    *result.instruction_kinds.entry(instruction.kind.clone()).or_default() += 1;
                }
            }
            let relocs = obj.relocations.get(&section).map(Vec::as_slice).unwrap_or(&[]);
            let mut literal_uses: BTreeMap<u32, BTreeSet<Use>> = BTreeMap::new();
            for instruction in decoded.instructions.get(&section).into_iter().flat_map(|m| m.values()) {
                if instruction.op() == Some(Op::L32r) {
                    if let Some(target) = instruction.target() { literal_uses.entry(target).or_default().insert(Use::Load); }
                }
            }
            // Establish long-call literal roles before visiting words (which commonly precede code).
            for relocation in relocs.iter().filter(|r| r.kind == 11) {
                match long_call(&resolver, &decoded, object, section, relocation) {
                    Ok(call) => {
                        result.asm_expand_passes += 1;
                        literal_uses.entry(call.literal_offset).or_default().insert(Use::Call);
                        let id = symbol_id(object, relocation);
                        let at = obj.location(section).at(call.instruction.offset);
                        call_import(&resolver, id, call.instruction, at, true, kept, &mut result);
                    }
                    Err(error) => result.failures.push(error),
                }
            }
            for relocation in relocs {
                *result.relocations.entry(relocation.kind).or_default() += 1;
                *result.relocations_by_class.entry((relocation.kind, s.class)).or_default() += 1;
                let at = obj.location(section).at(relocation.offset);
                let id = symbol_id(object, relocation);
                let mut uses = BTreeSet::new();
                match relocation.kind {
                    1 => {
                        let in_place = u32le(&s.data, relocation.offset as usize, &at)?;
                        let value = resolver.value(id, (relocation.addend as u32).wrapping_add(in_place));
                        result.words.push(Word { location: at.clone(), symbol: id, addend: relocation.addend, in_place, value, kept });
                        uses.insert(Use::DataWord);
                        if let Some(extra) = literal_uses.get(&relocation.offset) { uses.extend(extra); }
                    }
                    11 => { uses.insert(Use::Call); }
                    20 => {
                        slot(&resolver, &decoded, object, section, relocation, &mut result);
                        if let Some(instruction) = instruction_at(&decoded, section, relocation.offset) {
                            if instruction.call_size().is_some() {
                                uses.insert(Use::Call);
                                call_import(&resolver, id, instruction, at.clone(), false, kept, &mut result);
                            } else if instruction.op() == Some(Op::L32r) {
                                uses.insert(Use::Load);
                            } else { uses.insert(Use::Branch); }
                        }
                    }
                    kind => result.failures.push(at.error(format!("unsupported relocation kind {kind}; expected R_XTENSA_32 (1), R_XTENSA_ASM_EXPAND (11), R_XTENSA_SLOT0_OP (20)"))),
                }
                if kept { import_use(&resolver, id, at, &uses, map, &mut result); }
            }
        }
        result.decoded.push(decoded);
    }
    for common in &result.commons {
        result.kept_sizes.entry(Class::Bss).or_default().add(common.size);
        result.kept_by_archive.entry(objects[common.symbol.object].archive.clone()).or_default()
            .entry(Class::Bss).or_default().add(common.size);
    }
    for import in result.imports.values() {
        if map.is_some() && import.class.is_none() {
            result.failures.push(import.first.error(format!("import {} is ABSENT from import map", import.name)));
        }
        if import.class == Some(ImportClass::WeakAbsent) && !import.weak {
            result.failures.push(import.first.error(format!("weak-absent import {} is referenced strongly", import.name)));
        }
    }
    result.objects = objects;
    Ok(result)
}
