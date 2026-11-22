use crate::{audit::{Audit, Use, Value, ValueBase}, class::Class};
use std::fmt::{self, Write};

pub(crate) fn relocation_name(kind: u8) -> &'static str {
    match kind { 1 => "R_XTENSA_32", 11 => "R_XTENSA_ASM_EXPAND", 20 => "R_XTENSA_SLOT0_OP", _ => "UNKNOWN" }
}

impl Audit {
    pub fn value_text(&self, value: &Value) -> String {
        let base = match &value.base {
            ValueBase::Section(id) => self.objects[id.object].location(id.section).to_string(),
            ValueBase::Common(id) => format!("COMMON {}", self.objects[id.object].symbols[&id.table][id.index].name),
            ValueBase::Import(name) => format!("import {name}"),
            ValueBase::Absolute => return format!("{:#010x}", value.offset),
        };
        format!("{base}+{:#x}", value.offset)
    }

    pub fn report(&self) -> String {
        let mut text = String::new();
        // String's fmt::Write cannot fail; retain Display as the single report implementation.
        let _ = write!(text, "{self}");
        text
    }
}

impl fmt::Display for Audit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "radio island audit: {}", if self.passed() { "PASS" } else { "FAIL" })?;
        writeln!(f, "members: {} total, {} kept", self.objects.len(), self.kept_members().len())?;
        writeln!(f, "sections and bytes (input / kept; kept Bss includes COMMON):")?;
        for class in [Class::Text, Class::Iram, Class::Dram, Class::Rodata, Class::Data, Class::Bss, Class::Meta] {
            let all = self.counts.get(&class).copied().unwrap_or_default();
            let kept = self.kept_sizes.get(&class).copied().unwrap_or_default();
            writeln!(f, "  {class:?}: {} sections, {} bytes / {} sections, {} bytes", all.sections, all.bytes, kept.sections, kept.bytes)?;
        }
        writeln!(f, "relocations (all members):")?;
        for (kind, count) in &self.relocations {
            writeln!(f, "  {} ({kind}): {count}", relocation_name(*kind))?;
            for ((k, class), n) in &self.relocations_by_class {
                if k == kind { writeln!(f, "    {class:?}: {n}")?; }
            }
        }
        writeln!(f, "ASM_EXPAND: {} passed", self.asm_expand_passes)?;
        writeln!(f, "SLOT0_OP: {} in place, {} left for the linker, {} cross-section", self.slot0_passes, self.slot0_unwritten.len(), self.cross_section.len())?;
        for site in &self.slot0_unwritten {
            writeln!(f, "  {}: left for the linker {:?}, target {:#x}", site.location, site.symbol, site.target)?;
        }
        for site in &self.cross_section { writeln!(f, "  {}: cross-section {:?}", site.location, site.symbol)?; }
        writeln!(f, "imports (kept sections):")?;
        for import in self.imports.values() {
            let uses = import.uses.iter().map(|usage| match usage { Use::DataWord => "data word", Use::Call => "call", Use::Load => "load", Use::Branch => "branch" }).collect::<Vec<_>>().join(", ");
            let class = import.class.as_ref().map(ToString::to_string).unwrap_or_else(|| "ABSENT".into());
            writeln!(f, "  {}: {class}; {}; {uses}; first {}", import.name, if import.weak { "weak" } else { "strong" }, import.first)?;
        }
        writeln!(f, "calls into imports (kept sections):")?;
        for call in &self.calls {
            writeln!(f, "  {}: CALL{}{} -> {}", call.location, if call.indirect { "X" } else { "" }, call.size, call.symbol)?;
        }
        writeln!(f, "R_XTENSA_32 values (unplaced; S + A + word, modulo 2^32):")?;
        for word in &self.words {
            let symbol = &self.objects[word.symbol.object].symbols[&word.symbol.table][word.symbol.index];
            writeln!(f, "  {}: S({:?}) + ({}) + {:#x} = {} [{}]", word.location,
                symbol.name, word.addend, word.in_place, self.value_text(&word.value),
                if word.kept { "kept" } else { "not placed" })?;
        }
        writeln!(f, "COMMON allocations:")?;
        for common in &self.commons {
            writeln!(f, "  {}: {} bytes, alignment {}, {}({})", common.name, common.size, common.align,
                self.objects[common.symbol.object].archive, self.objects[common.symbol.object].member)?;
        }
        writeln!(f, "kept bytes by archive and class:")?;
        for (archive, sizes) in &self.kept_by_archive {
            for (class, size) in sizes { writeln!(f, "  {archive} {class:?}: {} bytes, {} sections", size.bytes, size.sections)?; }
        }
        writeln!(f, "instruction kinds (kept sections):")?;
        for (kind, count) in &self.instruction_kinds { writeln!(f, "  {kind}: {count}")?; }
        writeln!(f, "failures: {}", self.failures.len())?;
        for error in &self.failures { writeln!(f, "  {error}")?; }
        Ok(())
    }
}
