//! Small, synthetic GNU ar / ELF32 fixture encoder. No vendor archive is involved.

use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Section {
    pub name: String,
    pub kind: u32,
    pub flags: u32,
    pub data: Vec<u8>,
    pub size: u32,
    pub align: u32,
    pub link: u32,
    pub info: u32,
    pub entsize: u32,
}

impl Section {
    fn new(name: &str, kind: u32, flags: u32, data: Vec<u8>) -> Self {
        Self { name: name.into(), kind, flags, size: data.len() as u32, data, align: 4, link: 0, info: 0, entsize: 0 }
    }
}

#[derive(Clone)]
struct Symbol { name: String, value: u32, size: u32, bind: u8, kind: u8, section: u16 }

#[derive(Clone)]
pub struct Relocation { pub target: usize, pub offset: u32, pub kind: u8, pub symbol: usize, pub addend: i32 }

#[derive(Clone)]
pub struct Object {
    pub sections: Vec<Section>,
    symbols: Vec<Symbol>,
    pub relocations: Vec<Relocation>,
    properties: Vec<(usize, u32, u32, u32)>,
    pub machine: u16,
}

fn put16(data: &mut [u8], offset: usize, value: u16) { data[offset..offset + 2].copy_from_slice(&value.to_le_bytes()); }
fn put32(data: &mut [u8], offset: usize, value: u32) { data[offset..offset + 4].copy_from_slice(&value.to_le_bytes()); }
fn word(data: &mut Vec<u8>, value: u32) { data.extend_from_slice(&value.to_le_bytes()); }
fn string(table: &mut Vec<u8>, value: &str) -> u32 {
    let offset = table.len() as u32;
    table.extend_from_slice(value.as_bytes());
    table.push(0);
    offset
}

impl Object {
    pub fn new() -> Self {
        Self {
            sections: vec![Section::new("", 0, 0, Vec::new())],
            symbols: vec![Symbol { name: String::new(), value: 0, size: 0, bind: 0, kind: 0, section: 0 }],
            relocations: Vec::new(), properties: Vec::new(), machine: 94,
        }
    }

    pub fn section(&mut self, name: &str, kind: u32, flags: u32, data: Vec<u8>) -> usize {
        let id = self.sections.len();
        self.sections.push(Section::new(name, kind, flags, data));
        id
    }

    pub fn symbol(&mut self, name: &str, bind: u8, kind: u8, section: u16, value: u32, size: u32) -> usize {
        let id = self.symbols.len();
        self.symbols.push(Symbol { name: name.into(), value, size, bind, kind, section });
        id
    }

    pub fn section_symbol(&mut self, section: usize) -> usize {
        if let Some(id) = self.symbols.iter().position(|s| s.kind == 3 && s.section as usize == section) { return id; }
        self.symbol("", 0, 3, section as u16, 0, 0)
    }

    pub fn relocation(&mut self, target: usize, offset: u32, kind: u8, symbol: usize, addend: i32) {
        self.relocations.push(Relocation { target, offset, kind, symbol, addend });
    }

    pub fn property(&mut self, section: usize, offset: u32, size: u32, flags: u32) {
        self.properties.push((section, offset, size, flags));
    }

    pub fn encode(mut self) -> Vec<u8> {
        if !self.properties.is_empty() {
            let properties = self.properties.clone();
            let prop = self.section(".xt.prop", 1, 0, Vec::new());
            for (target, offset, size, flags) in properties {
                let symbol = self.section_symbol(target);
                let at = self.sections[prop].data.len() as u32;
                self.relocation(prop, at, 1, symbol, 0);
                word(&mut self.sections[prop].data, offset);
                word(&mut self.sections[prop].data, size);
                word(&mut self.sections[prop].data, flags);
            }
            self.sections[prop].size = self.sections[prop].data.len() as u32;
        }
        let mut order: Vec<_> = (0..self.symbols.len()).collect();
        order.sort_by_key(|i| self.symbols[*i].bind != 0);
        let locals = self.symbols.iter().filter(|s| s.bind == 0).count();
        let mut remap = vec![0; order.len()];
        let mut strings = vec![0];
        let mut symbols = Vec::new();
        for (new, old) in order.into_iter().enumerate() {
            remap[old] = new;
            let symbol = &self.symbols[old];
            let name = if symbol.name.is_empty() { 0 } else { string(&mut strings, &symbol.name) };
            word(&mut symbols, name);
            word(&mut symbols, symbol.value);
            word(&mut symbols, symbol.size);
            symbols.push((symbol.bind << 4) | symbol.kind);
            symbols.push(0);
            symbols.extend_from_slice(&symbol.section.to_le_bytes());
        }
        let strtab = self.section(".strtab", 3, 0, strings);
        let symtab = self.section(".symtab", 2, 0, symbols);
        self.sections[symtab].link = strtab as u32;
        self.sections[symtab].info = locals as u32;
        self.sections[symtab].entsize = 16;
        let mut groups: BTreeMap<usize, Vec<Relocation>> = BTreeMap::new();
        for relocation in std::mem::take(&mut self.relocations) { groups.entry(relocation.target).or_default().push(relocation); }
        for (target, relocations) in groups {
            let mut data = Vec::new();
            for relocation in relocations {
                word(&mut data, relocation.offset);
                word(&mut data, ((remap[relocation.symbol] as u32) << 8) | u32::from(relocation.kind));
                word(&mut data, relocation.addend as u32);
            }
            let name = format!(".rela{}", self.sections[target].name);
            let index = self.section(&name, 4, 0x40, data);
            self.sections[index].link = symtab as u32;
            self.sections[index].info = target as u32;
            self.sections[index].entsize = 12;
        }
        let shstrtab = self.section(".shstrtab", 3, 0, Vec::new());
        let mut names = vec![0];
        let name_offsets: Vec<_> = self.sections.iter().map(|s| if s.name.is_empty() { 0 } else { string(&mut names, &s.name) }).collect();
        self.sections[shstrtab].size = names.len() as u32;
        self.sections[shstrtab].data = names;
        let mut data = vec![0; 52];
        let mut offsets = vec![0];
        for section in self.sections.iter().skip(1) {
            while data.len() % section.align as usize != 0 { data.push(0); }
            offsets.push(data.len() as u32);
            if section.kind != 8 { data.extend_from_slice(&section.data); }
        }
        while data.len() % 4 != 0 { data.push(0); }
        let shoff = data.len() as u32;
        data.extend_from_slice(&[0; 40]);
        for (index, section) in self.sections.iter().enumerate().skip(1) {
            for value in [name_offsets[index], section.kind, section.flags, 0, offsets[index], section.size,
                section.link, section.info, section.align, section.entsize]
            { word(&mut data, value); }
        }
        data[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        put16(&mut data, 16, 1);
        put16(&mut data, 18, self.machine);
        put32(&mut data, 20, 1);
        put32(&mut data, 32, shoff);
        put32(&mut data, 36, 0x300);
        put16(&mut data, 40, 52);
        put16(&mut data, 46, 40);
        put16(&mut data, 48, self.sections.len() as u16);
        put16(&mut data, 50, shstrtab as u16);
        data
    }
}

fn ar_member(data: &mut Vec<u8>, name: &str, contents: &[u8]) {
    let header = format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", 0, 0, 0, "100644", contents.len());
    assert_eq!(header.len(), 60, "fixture archive header has fixed-width fields");
    data.extend_from_slice(header.as_bytes());
    data.extend_from_slice(contents);
    if contents.len() % 2 != 0 { data.push(b'\n'); }
}

pub fn archive(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut data = b"!<arch>\n".to_vec();
    ar_member(&mut data, "/", &[0, 0, 0, 0]); // Empty index: readers must scan members.
    let mut names = Vec::new();
    let mut offsets = BTreeMap::new();
    for (name, _) in members.iter().filter(|(name, _)| name.len() > 15) {
        offsets.insert(*name, names.len());
        names.extend_from_slice(name.as_bytes());
        names.extend_from_slice(b"/\n");
    }
    if !names.is_empty() { ar_member(&mut data, "//", &names); }
    for (name, contents) in members {
        let token = offsets.get(name).map(|offset| format!("/{offset}")).unwrap_or_else(|| format!("{name}/"));
        ar_member(&mut data, &token, contents);
    }
    data
}

pub fn emit(data: &mut Vec<u8>, bits: u32) {
    let length = if bits & 15 >= 8 { 2 } else { 3 };
    data.extend_from_slice(&bits.to_le_bytes()[..length]);
}

pub fn l32r(reg: u8, pc: u32, target: u32) -> u32 {
    let delta = i64::from(target) - i64::from((pc + 3) & !3);
    assert!(delta < 0 && delta >= -262144 && delta % 4 == 0, "fixture L32R is a negative aligned displacement");
    1 | (u32::from(reg) << 4) | (((delta / 4) as u32 & 0xffff) << 8)
}

pub fn callx(size: u8, reg: u8) -> u32 { 0xc0 | (u32::from(size / 4) << 4) | (u32::from(reg) << 8) }
pub fn call(size: u8, pc: u32, target: u32) -> u32 {
    let delta = i64::from(target) - i64::from((pc & !3) + 4);
    assert_eq!(delta % 4, 0, "fixture CALL target is aligned relative to its base");
    5 | (u32::from(size / 4) << 4) | (((delta / 4) as u32 & 0x3ffff) << 6)
}
pub fn entry() -> u32 { 0x004136 } // ENTRY a1, 32
pub fn retwn() -> u32 { 0xf01d }
pub fn jump(pc: u32, target: u32) -> u32 { 6 | ((target.wrapping_sub(pc + 4) & 0x3ffff) << 6) }
pub fn beqz(reg: u8, pc: u32, target: u32) -> u32 { 0x16 | (u32::from(reg) << 8) | ((target.wrapping_sub(pc + 4) & 0xfff) << 12) }
pub fn bnezn(reg: u8, pc: u32, target: u32) -> u32 {
    let delta = target - pc - 4;
    0xcc | (u32::from(reg) << 8) | ((delta & 15) << 12) | (delta & 0x30)
}
pub fn loop_insn(reg: u8, pc: u32, target: u32) -> u32 { 0x8076 | (u32::from(reg) << 8) | ((target - pc - 4) << 16) }

pub struct Pack { pub archives: Vec<(&'static str, Vec<u8>)> }

pub fn pack(fault: &str) -> Pack {
    let mut main = Object::new();
    let mut code = vec![0; 32];
    put32(&mut code, 20, 2);
    emit(&mut code, entry());
    emit(&mut code, l32r(8, 35, if fault == "slot" { 4 } else { 0 }));
    emit(&mut code, if fault == "no-callx" { jump(38, 64) } else { callx(8, 8) });
    emit(&mut code, l32r(8, 41, 4));
    emit(&mut code, callx(match fault { "callx4" => 4, "callx0" => 0, _ => 8 }, 8));
    emit(&mut code, l32r(3, 47, 8));
    emit(&mut code, call(match fault { "call4" => 4, "call0" => 0, _ => 8 }, 50, 52));
    emit(&mut code, jump(53, 58));
    emit(&mut code, bnezn(2, 56, 61));
    emit(&mut code, beqz(2, 58, 64));
    emit(&mut code, loop_insn(2, 61, 68));
    emit(&mut code, retwn());
    emit(&mut code, retwn());
    let text = main.section(".text.start", 1, 6, code);
    let strings = main.section(".rodata.str1.1", 1, 0x32, b"a\0word\0".to_vec());
    let mut table = Vec::new();
    word(&mut table, 64); word(&mut table, 66);
    let table = main.section(".rodata.table", 1, 2, table);
    let data = main.section(".data.state", 1, 3, vec![0; 16]);
    let dram = main.section(".dram1.state", 1, 3, 4u32.to_le_bytes().to_vec());
    main.symbol("start", 1, 2, text as u16, 32, 36);
    let tail = main.symbol("tail", 1, 2, text as u16, 64, 4);
    let helper = main.symbol("helper", 1, 0, 0, 0, 0);
    let gate = main.symbol("gate_fn", 1, 0, 0, 0, 0);
    let glue = main.symbol("glue_data", 1, 0, 0, 0, 0);
    let overridden = main.symbol("override", 1, 0, 0, 0, 0);
    let common = main.symbol("scratch", 1, 1, 0xfff2, 8, 12);
    let optional = main.symbol("optional", 2, 0, 0, 0, 0);
    let rom = main.symbol("rom_clock", 1, 0, 0, 0, 0);
    let absolute = main.symbol("absolute", 1, 0, 0xfff1, 0x1000, 0);
    let own = main.section_symbol(text);
    let string_symbol = main.section_symbol(strings);
    let table_symbol = main.section_symbol(table);
    let data_symbol = main.symbol("state", 0, 1, data as u16, 0, 16);
    let dram_symbol = main.section_symbol(dram);
    for (offset, symbol, addend) in [(0, helper, 0), (4, gate, 0), (8, glue, 0), (12, overridden, 0),
        (16, common, 0), (20, string_symbol, 1), (24, table_symbol, 0), (28, data_symbol, 0)]
    { main.relocation(text, offset, 1, symbol, addend); }
    for (offset, target) in [(35, 0), (41, 4), (47, 8), (50, 64), (53, 58), (56, 61), (58, 64), (61, 68)] {
        let (symbol, addend) = if offset == 50 {
            (if matches!(fault, "call4" | "call0") { gate } else { tail }, 0)
        } else { (own, target) };
        main.relocation(text, offset, 20, symbol, addend);
    }
    main.relocation(text, 35, 11, if fault == "callee" { gate } else { helper }, 0);
    main.relocation(text, 41, 11, gate, 0);
    main.relocation(table, 0, 1, own, 0);
    main.relocation(table, 4, 1, own, 0);
    main.relocation(data, 0, 1, glue, 4);
    main.relocation(data, 4, 1, optional, 0);
    main.relocation(data, 8, 1, rom, 0);
    main.relocation(data, 12, 1, dram_symbol, 0);
    main.relocation(dram, 0, 1, absolute, 3);
    main.property(text, 0, 32, 1);
    main.property(text, 32, 36, 2);
    match fault {
        "machine" => main.machine = 93,
        "rel" => { main.section(".rel.text.start", 9, 0, Vec::new()); }
        "tls" => main.sections[data].flags |= 0x400,
        "group" => { main.section(".group", 17, 0, Vec::new()); }
        "unknown" => main.sections[data].name = ".surprise".into(),
        "relocation" => main.relocations[0].kind = 99,
        "duplicate" => { main.symbol("override", 1, 2, text as u16, 32, 36); }
        "literal" => { main.section(".literal.nonempty", 1, 2, vec![0; 4]); }
        "data-root" => { main.symbol("data_root", 1, 1, data as u16, 0, 16); }
        _ => {},
    }
    let definition = |name: &str, section_name: &str, bind: u8, body: Vec<u8>| {
        let mut object = Object::new();
        let size = body.len() as u32;
        let section = object.section(section_name, 1, 6, body);
        object.symbol(name, bind, 2, section as u16, 0, size);
        object.property(section, 0, size, 2);
        object.encode()
    };
    let mut ret = Vec::new(); emit(&mut ret, retwn());
    let weak = definition("override", ".text.override.weak", 2, ret.clone());
    let strong = definition("override", ".text.override.strong", 1, ret.clone());
    let mut helper_code = Vec::new(); emit(&mut helper_code, entry()); emit(&mut helper_code, retwn());
    let helper = definition("helper", ".iram1.helper", 1, helper_code);
    let mut unused = Object::new();
    let mut unused_code = vec![0; 4]; unused_code.extend_from_slice(&ret);
    unused_code.extend_from_slice(&[0; 6]); // Alignment fill at 6..8, literal at 8..12.
    if fault == "padding" { unused_code[6] = 1; }
    let unused_text = unused.section(".text.unreached", 1, 6, unused_code);
    let unknown = unused.symbol("unreached_unknown", 1, 0, 0, 0, 0);
    unused.relocation(unused_text, 0, 1, unknown, 0);
    unused.property(unused_text, 0, 4, 1); unused.property(unused_text, 4, 2, 2);
    unused.property(unused_text, 8, 4, 1);
    let mut main_bytes = main.encode();
    if fault == "truncated" { main_bytes.truncate(main_bytes.len() - 3); }
    let mut first = archive(&[("entry_member_with_a_long_name.o", main_bytes), ("weak.o", weak), ("unused.o", unused.encode())]);
    if fault == "thin" { first = b"!<thin>\n".to_vec(); }
    Pack { archives: vec![("core.a", first), ("radio.a", archive(&[("helper.o", helper), ("strong.o", strong)]))] }
}

/// Extra definitions with no incoming pack references, for glue reachability.
pub fn glue_pack() -> Pack {
    let mut pack = pack("");
    let mut object = Object::new();
    let mut body = Vec::new();
    emit(&mut body, entry());
    emit(&mut body, retwn());
    let size = body.len() as u32;
    let text = object.section(".text.glue_only", 1, 6, body);
    object.symbol("glue_only", 1, 2, text as u16, 0, size);
    object.symbol("glue_common", 1, 1, 0xfff2, 16, 4);
    object.property(text, 0, size, 2);
    pack.archives.push(("glue_targets.a", archive(&[("glue_targets.o", object.encode())])));
    pack
}

/// A cross-object SLOT0_OP with a weak definition at the source and a strong
/// selected definition in the other object. The target is absolute so tests can
/// choose exact displacement boundaries independently of metadata placement.
pub fn slot_pack(bits: u32, target: u32, addend: i32) -> Vec<u8> {
    let mut source = Object::new();
    let mut body = Vec::new();
    emit(&mut body, bits);
    let size = body.len() as u32;
    let text = source.section(".text.slot", 1, 6, body);
    source.symbol("slot_root", 1, 2, text as u16, 0, size);
    let weak = source.symbol("slot_target", 2, 2, text as u16, 0, size);
    source.property(text, 0, size, 2);
    source.relocation(text, 0, 20, weak, addend);
    let mut target_object = Object::new();
    target_object.symbol("slot_target", 1, 0, 0xfff1, target, 0);
    archive(&[("source.o", source.encode()), ("target.o", target_object.encode())])
}

pub fn storage_pack() -> Vec<u8> {
    let mut object = Object::new();
    let mut body = vec![0; 12];
    emit(&mut body, l32r(3, 12, 0));
    emit(&mut body, retwn());
    let iram = object.section(".iram1.root", 1, 6, body);
    let data = object.section(".data.wrap", 1, 3, u32::MAX.to_le_bytes().to_vec());
    let bss = object.section(".bss.live", 8, 3, Vec::new());
    object.sections[bss].size = 7;
    object.sections[bss].align = 16;
    object.symbol("ram_root", 1, 2, iram as u16, 12, 5);
    let data_symbol = object.symbol("wrap_data", 1, 1, data as u16, 0, 4);
    let bss_symbol = object.symbol("live_bss", 1, 1, bss as u16, 0, 7);
    let common = object.symbol("extra_common", 1, 1, 0xfff2, 32, 3);
    let absolute = object.symbol("small_absolute", 1, 0, 0xfff1, 3, 0);
    let own = object.section_symbol(iram);
    object.relocation(iram, 0, 1, data_symbol, 0);
    object.relocation(iram, 4, 1, bss_symbol, 0);
    object.relocation(iram, 8, 1, common, 0);
    object.relocation(iram, 12, 20, own, 0);
    object.relocation(data, 0, 1, absolute, -5);
    object.property(iram, 0, 12, 1);
    object.property(iram, 12, 5, 2);
    archive(&[("storage.o", object.encode())])
}
