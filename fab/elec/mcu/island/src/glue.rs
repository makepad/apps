//! Glue relocations write whole 32-bit words. Data pointers hold the target
//! address. Text literals add that address to the original word. A glue symbol
//! names its entry, and relocation offsets are measured from the object's start.
//! A direct CALL8 from the pack is patched at the call site.

use crate::import_map::MapError;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlueRegion { Dram, Rodata, Text, Bss }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlueRelocation {
    pub offset: u32,
    pub symbol: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glue {
    pub name: String,
    pub region: GlueRegion,
    pub align: u32,
    /// Byte offset of the callable entry in text. Defaults to zero in a glue
    /// file and must be zero for data objects.
    pub entry: u32,
    pub bytes: Vec<u8>,
    pub relocations: Vec<GlueRelocation>,
}

impl Glue {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.region == GlueRegion::Text {
            if self.align < 4 { return Err("text alignment must be at least 4"); }
            if self.entry % 4 != 0 { return Err("entry offset must be a multiple of 4"); }
            if self.entry as usize >= self.bytes.len() { return Err("entry offset must be inside the text object"); }
        } else if self.entry != 0 {
            return Err("data glue must have entry offset zero");
        }
        if self.region == GlueRegion::Bss {
            if !self.relocations.is_empty() { return Err("BSS glue cannot have relocations"); }
            if self.bytes.iter().any(|&b| b != 0) { return Err("BSS glue must contain only zero bytes"); }
        }
        Ok(())
    }
}

pub fn parse_glue(text: &str) -> Result<Vec<Glue>, MapError> {
    let mut objects: Vec<Glue> = Vec::new();
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let words: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        if words.is_empty() { continue; }
        let fail = |message: String| MapError { line: index + 1, message };
        if words[0] == "reloc" {
            let ["reloc", name, offset, symbol] = words.as_slice() else {
                return Err(fail("expected reloc <earlier glue name> <offset> <symbol>".into()));
            };
            let object_index = names.get(name).copied()
                .ok_or_else(|| fail(format!("reloc: unknown glue {name}; declare it first")))?;
            let object = &mut objects[object_index];
            if object.region == GlueRegion::Bss { return Err(fail(format!("glue {name}: BSS glue cannot have relocations"))); }
            let offset = number(offset).ok_or_else(|| fail(format!("glue {name}: invalid reloc offset")))?;
            if offset % 4 != 0 { return Err(fail(format!("glue {name}: reloc offset {offset:#x} must be a multiple of 4"))); }
            if offset.checked_add(4).map_or(true, |end| end as usize > object.bytes.len()) {
                return Err(fail(format!("glue {name}: reloc offset {offset:#x} is past the end")));
            }
            if object.relocations.iter().any(|r| r.offset == offset) {
                return Err(fail(format!("glue {name}: duplicate reloc offset {offset:#x}")));
            }
            object.relocations.push(GlueRelocation { offset, symbol: (*symbol).into() });
            continue;
        }
        let ["glue", name, region, align, rest @ ..] = words.as_slice() else {
            return Err(fail("expected glue <name> text <align> [entry <offset>] hex <bytes>, glue <name> dram|rodata <align> hex <bytes> or <size> zero, glue <name> bss <align> <size> zero, or reloc <earlier glue name> <offset> <symbol>".into()));
        };
        if names.insert(*name, objects.len()).is_some() { return Err(fail(format!("duplicate glue {name}"))); }
        let region = match *region {
            "dram" => GlueRegion::Dram, "rodata" => GlueRegion::Rodata, "text" => GlueRegion::Text,
            "bss" => GlueRegion::Bss,
            _ => return Err(fail(format!("glue {name}: expected dram or rodata or text or bss"))),
        };
        let align = number(align).filter(|n| n.is_power_of_two())
            .ok_or_else(|| fail(format!("glue {name}: alignment must be a nonzero power of two")))?;
        let (entry, rest) = match rest {
            ["entry", ..] if region != GlueRegion::Text => return Err(fail(format!("glue {name}: entry requires text"))),
            ["entry", offset, payload @ ..] => (
                number(offset).ok_or_else(|| fail(format!("glue {name}: invalid entry offset")))?, payload),
            ["entry"] => return Err(fail(format!("glue {name}: expected entry <offset> hex <bytes>"))),
            _ => (0, rest),
        };
        let bytes = match rest {
            [size, "zero"] => {
                if region == GlueRegion::Text { return Err(fail(format!("glue {name}: text does not allow zero fill"))); }
                let size = number(size).ok_or_else(|| fail(format!("glue {name}: invalid size")))?;
                let mut data = Vec::new();
                data.try_reserve_exact(size as usize).map_err(|e| fail(format!("glue {name}: {e}")))?;
                data.resize(size as usize, 0);
                data
            }
            _ if region == GlueRegion::Bss => return Err(fail(format!("glue {name}: expected <size> zero for BSS"))),
            ["hex", hex @ ..] if !hex.is_empty() => {
                // Hex may be contiguous or split into whitespace-separated byte groups.
                let hex = hex.concat();
                if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(fail(format!("glue {name}: expected pairs of hexadecimal digits")));
                }
                let mut data = Vec::new();
                for offset in (0..hex.len()).step_by(2) {
                    data.push(u8::from_str_radix(&hex[offset..offset + 2], 16)
                        .map_err(|_| fail(format!("glue {name}: invalid hex byte")))?);
                }
                data
            }
            _ if region == GlueRegion::Text => return Err(fail(format!("glue {name}: expected [entry <offset>] hex <bytes>"))),
            _ => return Err(fail(format!("glue {name}: expected <size> zero or hex <bytes>"))),
        };
        let object = Glue { name: (*name).into(), region, align, entry, bytes, relocations: Vec::new() };
        object.validate().map_err(|message| fail(format!("glue {name}: {message}")))?;
        objects.push(object);
    }
    Ok(objects)
}

pub(crate) fn number(value: &str) -> Option<u32> {
    if let Some(hex) = value.strip_prefix("0x") {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) { return None; }
        u32::from_str_radix(hex, 16).ok()
    } else {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) { return None; }
        value.parse().ok()
    }
}
