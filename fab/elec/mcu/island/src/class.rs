use crate::{Error, Location};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class { Text, Iram, Dram, Rodata, Data, Bss, Meta }

fn family(name: &str, base: &str) -> bool {
    name == base || name.strip_prefix(base).is_some_and(|tail| tail.starts_with('.'))
}

/// Accepted flag values (not masks):
/// Text/Iram PROGBITS: 0x2, 0x6; Dram PROGBITS: 0x2, 0x3;
/// Data PROGBITS and Bss NOBITS: 0x3;
/// Rodata PROGBITS: 0x2, 0x12 (MERGE), 0x32 (MERGE|STRINGS);
/// Meta PROGBITS/NOTE: 0, 0x30 (mergeable comments); NULL/SYMTAB/STRTAB: 0;
/// RELA: 0, 0x40 (SHF_INFO_LINK, with sh_info validated by the reader).
/// MERGE sections are retained whole. Neither merging nor string suffix folding is done.
pub fn classify(name: &str, kind: u32, flags: u32, at: &Location) -> Result<Class, Error> {
    if kind == 9 { return Err(at.error("SHT_REL refused; expected RELA")); }
    if kind == 17 || flags & 0x200 != 0 { return Err(at.error("GROUP section/SHF_GROUP refused")); }
    if flags & 0x400 != 0 { return Err(at.error("SHF_TLS refused")); }
    if [".init_array", ".fini_array", ".ctors", ".dtors"].iter().any(|n| family(name, n))
        || family(name, ".fini")
    {
        return Err(at.error("initialization/finalization section refused"));
    }
    let class = if matches!(kind, 0 | 2 | 3 | 4) {
        Class::Meta
    } else if name.starts_with(".xt.prop") || name.starts_with(".xt.lit")
        || name == ".xtensa.info" || name == ".comment" || name.starts_with(".debug")
    {
        Class::Meta
    } else if family(name, ".text") || name.starts_with(".literal") {
        Class::Text
    } else if family(name, ".iram1") || [
        ".wifi0iram", ".wifirxiram", ".wifiextrairam", ".wifislpiram",
        ".wifislprxiram", ".wifiorslpiram",
    ].iter().any(|n| name.starts_with(*n)) {
        Class::Iram
    } else if name.starts_with(".dram1") {
        Class::Dram
    } else if family(name, ".rodata") || ["debug", "info", "warning", "error", "verbose"]
        .iter().any(|level| name.starts_with(format!(".rodata_wlog_{level}").as_str()))
    {
        Class::Rodata
    } else if family(name, ".data") {
        Class::Data
    } else if family(name, ".bss") {
        Class::Bss
    } else {
        return Err(at.error(format!("unknown section name {name:?}")));
    };
    if flags & 4 != 0 && !matches!(class, Class::Text | Class::Iram) {
        return Err(at.error("exec flag on non-code class"));
    }
    let valid = match class {
        Class::Text | Class::Iram => kind == 1 && matches!(flags, 2 | 6),
        Class::Dram => kind == 1 && matches!(flags, 2 | 3),
        Class::Data => kind == 1 && flags == 3,
        Class::Rodata => kind == 1 && matches!(flags, 2 | 0x12 | 0x32),
        Class::Bss => kind == 8 && flags == 3,
        Class::Meta => match kind {
            0 | 2 | 3 => flags == 0,
            4 => matches!(flags, 0 | 0x40),
            1 => flags == 0 || (name == ".comment" && flags == 0x30),
            7 => name == ".xtensa.info" && flags == 0,
            _ => false,
        },
    };
    if !valid { return Err(at.error(format!("unsupported {class:?} type {kind:#x}, flags {flags:#x}"))); }
    Ok(class)
}
