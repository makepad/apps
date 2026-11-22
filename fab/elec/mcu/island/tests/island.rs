mod support;

use fab_elec_mcu_island::{ar, audit::{Use, ValueBase}, audit, class::Class, elf::SymbolSection,
    import_map::{parse_roots, ImportClass}, xt, Archive, ImportMap,
    crc32, link, parse_glue, symbolize, Glue, GlueRegion, GlueRelocation, Header, Island, IslandLayout, Region, Segment, SegmentKind};
use fab_elec_mcu_xtensa::insn::{self, Op};
use std::collections::BTreeSet;

const MAP: &str = "# synthetic radio contracts\ngate 7 gate_fn\nglue glue_data\nrom rom_clock 0x40001000\nweak-absent optional\n";

#[test]
fn synthetic_pack_reader_reachability_and_audit() {
    for (bits, op, length, pc, target) in [
        (support::l32r(8, 35, 0), Op::L32r, 3, 35, Some(0)),
        (support::callx(8, 8), Op::Callx8, 3, 38, None),
        (support::call(8, 50, 52), Op::Call8, 3, 50, Some(52)),
        (support::entry(), Op::Entry, 3, 32, None),
        (support::retwn(), Op::RetwN, 2, 64, None),
        (support::jump(53, 58), Op::J, 3, 53, Some(58)),
        (support::beqz(2, 58, 64), Op::Beqz, 3, 58, Some(64)),
        (support::bnezn(2, 56, 61), Op::BnezN, 2, 56, Some(61)),
    ] {
        let (decoded, actual_length) = insn::decode(bits).expect("fixture helper must use a model-supported encoding");
        assert_eq!((decoded.op, actual_length), (op, length), "fixture opcode and length for {op:?}");
        assert_eq!(insn::target(&decoded, pc), target, "fixture PC-relative target for {op:?}");
    }
    for (r, name) in [(8, "LOOP"), (9, "LOOPNEZ"), (10, "LOOPGTZ")] {
        let bits = (support::loop_insn(2, 61, 320) & !0xf000) | (r << 12);
        let loop_insn = xt::instruction(bits, 61).expect("loop instructions have a supported length");
        assert_eq!((loop_insn.kind.as_str(), loop_insn.length, loop_insn.target()), (name, 3, Some(320)), "{name} uses unsigned imm8 even with its high bit set");
    }
    let unmodeled = xt::instruction(0x000004, 0).expect("MAC16 is readable without execution support");
    assert_eq!((unmodeled.length, unmodeled.kind.as_str()), (3, "OPTION op0=4/op1=0/op2=0 length=3"), "unsupported execution options retain length and opcode kind");
    for (bits, name) in [(0x030000, "RSR.SR0"), (0x130100, "WSR.SR1"), (0x610200, "XSR.SR2"),
        (0x006300, "RSIL level=3"), (0x007400, "WAITI level=4")]
    {
        assert_eq!(xt::instruction(bits, 0).expect("special instructions remain readable").kind, name, "register and interrupt-level operands are visible for {name}");
    }
    assert!(xt::instruction(14, 0).is_err(), "op0 14/15 cannot supply a supported instruction length");
    let pack = support::pack("");
    let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
    let members = ar::parse(archives[0].name, archives[0].data).expect("GNU long names parse");
    assert_eq!(members[0].name, "entry_member_with_a_long_name.o", "// offsets resolve the complete member name");
    assert_eq!(members.len(), 3, "the / symbol index is ignored and ordinary members are scanned");
    let roots = parse_roots("# roots\n\nstart # entry\n").expect("root comments and blank lines parse");
    let map = ImportMap::parse(MAP).expect("all four import map forms parse");
    let result = audit(&archives, &roots, Some(&map)).expect("synthetic pack is structurally valid");
    assert!(result.passed(), "complete fixture audit passes:\n{}", result.report());
    assert_eq!(result.objects.len(), 5, "both archives contribute all five members to the census");
    let kept: Vec<_> = result.kept_members().iter().map(|index| result.objects[*index].member.as_str()).collect();
    assert_eq!(kept, ["entry_member_with_a_long_name.o", "helper.o", "strong.o"], "section reachability pulls the callee and strong override, dropping weak and unreached members");
    assert!(!result.imports.contains_key("unreached_unknown"), "metadata and dropped code do not create imports");
    assert_eq!(result.imports.keys().map(String::as_str).collect::<Vec<_>>(), ["gate_fn", "glue_data", "optional", "rom_clock"], "reached unresolved symbols are exactly the import list");
    for (name, class, weak, uses) in [
        ("gate_fn", ImportClass::Gate(7), false, BTreeSet::from([Use::DataWord, Use::Call, Use::Load])),
        ("glue_data", ImportClass::Glue, false, BTreeSet::from([Use::DataWord, Use::Load])),
        ("optional", ImportClass::WeakAbsent, true, BTreeSet::from([Use::DataWord])),
        ("rom_clock", ImportClass::Rom(0x40001000), false, BTreeSet::from([Use::DataWord])),
    ] {
        let import = &result.imports[name];
        assert_eq!((&import.class, import.weak, &import.uses), (&Some(class), weak, &uses), "import contract and observed references for {name}");
        assert_eq!((import.first.archive.as_str(), import.first.member.as_str()), ("core.a", "entry_member_with_a_long_name.o"), "import {name} retains its first archive/member");
        assert_eq!(import.first.section, if matches!(name, "optional" | "rom_clock") { ".data.state" } else { ".text.start" }, "import {name} retains its first referencing section");
    }
    assert_eq!(result.calls.iter().map(|c| (c.symbol.as_str(), c.size, c.indirect)).collect::<Vec<_>>(), [("gate_fn", 8, true)], "every kept import call has its observed window size");
    assert_eq!((result.asm_expand_passes, result.slot0_passes), (2, 7), "both long calls and the seven filled PC-relative fields pass their site checks");
    assert_eq!(result.slot0_unwritten.iter().map(|site| (site.location.to_string(), site.symbol.as_str(), site.target)).collect::<Vec<_>>(),
        [("core.a(entry_member_with_a_long_name.o):.text.start+0x32".to_string(), "tail", 64)],
        "the zero CALL8 field records the resolved tail target for the linker");
    assert!(result.cross_section.is_empty(), "the normal pack has only same-section SLOT0_OP targets");
    assert_eq!(result.instruction_kinds.get("LOOP"), Some(&1), "LOOP contributes a readable instruction kind");
    for (class, bytes) in [(Class::Text, 70), (Class::Iram, 5), (Class::Rodata, 15), (Class::Data, 16), (Class::Dram, 4), (Class::Bss, 12)] {
        assert_eq!(result.kept_sizes[&class].bytes, bytes, "kept bytes for {class:?} include whole sections and COMMON storage");
    }
    assert_eq!(result.kept_by_archive["core.a"][&Class::Text].bytes, 68, "kept totals are also partitioned by archive");
    assert_eq!(result.kept_by_archive["radio.a"][&Class::Iram].bytes, 5, "IRAM placement class survives cross-archive extraction");
    assert_eq!(result.commons.iter().map(|c| (c.name.as_str(), c.size, c.align)).collect::<Vec<_>>(), [("scratch", 12, 8)], "COMMON preserves size and alignment without treating alignment as S");
    assert_eq!(result.relocations.get(&11), Some(&2), "ASM_EXPAND census counts every site");
    assert_eq!(result.relocations_by_class.get(&(20, Class::Text)), Some(&8), "relocation census retains the target section class");
    let strings_word = result.words.iter().find(|w| w.location.section == ".text.start" && w.location.offset == Some(20)).expect("string pointer word is reported");
    assert_eq!((strings_word.in_place, strings_word.addend, strings_word.value.offset), (2, 1, 3), "R_XTENSA_32 sums both nonzero word and RELA addend");
    let ValueBase::Section(strings_id) = &strings_word.value.base else { panic!("string pointer must retain a section identity"); };
    assert_eq!(result.objects[strings_id.object].sections[strings_id.section].name, ".rodata.str1.1", "MERGE|STRINGS is accepted and kept whole");
    assert_eq!(result.words.iter().filter(|w| w.location.section == ".rodata.table").map(|w| w.value.offset).collect::<Vec<_>>(), [64, 66], "section-symbol jump-table pointers retain in-place offsets");
    let numeric_word = result.words.iter().find(|w| w.location.section == ".dram1.state").expect("absolute reference is reported");
    assert_eq!((&numeric_word.value.base, numeric_word.value.offset), (&ValueBase::Absolute, 0x1007), "named ABS S, signed addend and word all participate in the expression");
    let report = result.report();
    assert!(report.contains("+ (3) + 0x4 = 0x00001007"), "the report shows the computed R_XTENSA_32 value and its inputs");
    assert!(report.contains(".rodata.str1.1+0x3"), "the report gives the resolved section-relative value");
    assert!(report.contains("SLOT0_OP: 7 in place, 1 left for the linker, 0 cross-section"), "the report distinguishes filled, unwritten and cross-section sites");
    assert!(report.contains(".text.start+0x32: left for the linker \"tail\", target 0x40"), "the report locates each unwritten site and its resolved target");
    let unmapped = audit(&archives, &roots, None).expect("map is optional");
    assert!(unmapped.passed() && unmapped.report().contains("ABSENT"), "unclassified imports are informational without a map");

    for (bits, op, external) in [
        (support::call(8, 0, 4), Op::Call8, false),
        (support::jump(0, 4), Op::J, false),
        (support::beqz(2, 0, 4), Op::Beqz, false),
        (0x000226, Op::Beqi, false),
        (0x001237, Op::Beq, false),
        (0x000276, Op::Bf, false),
        (support::bnezn(2, 0, 4), Op::BnezN, false),
        (support::call(8, 0, 4), Op::Call8, true),
    ] {
        let mut body = Vec::new(); support::emit(&mut body, bits);
        let length = body.len() as u32;
        body.resize(8, 0);
        for _ in 0..3 { support::emit(&mut body, support::retwn()); }
        let mut base = support::Object::new();
        let text = base.section(".text.slot", 1, 6, body);
        base.property(text, 0, length, 2);
        base.property(text, length, 8 - length, 8);
        base.property(text, 8, 6, 2);
        let mut source = base.clone();
        source.symbol("slot_root", 1, 2, text as u16, 0, length);
        let weak = source.symbol("slot_target", 2, 2, text as u16, 0, length);
        source.relocation(text, 0, 20, weak, 4);
        let mut members = Vec::new();
        if external {
            base.symbol("slot_target", 1, 2, text as u16, 8, 6);
            members.push(("target.o", base.encode()));
        } else { source.symbol("slot_target", 1, 2, text as u16, 8, 6); }
        members.insert(0, ("source.o", source.encode()));
        let bytes = support::archive(&members);
        let checked = audit(&[Archive { name: "slots.a", data: &bytes }], &["slot_root".into()], None)
            .expect("zero-offset fixtures are valid ELF");
        assert!(checked.passed(), "{op:?} zero field is readable for linking: {}", checked.report());
        assert_eq!(checked.decoded[0].instructions[&text][&0].op(), Some(op), "zero-offset {op:?} retains its instruction kind");
        assert_eq!(checked.slot0_passes, 0, "SLOT0_OP checks the strong definition instead of the raw weak value for {op:?}");
        if external {
            assert!(checked.slot0_unwritten.is_empty() && checked.cross_section.len() == 1,
                "a resolved definition in another object is cross-section even when section indices match");
        } else {
            assert_eq!(checked.slot0_unwritten.iter().map(|site| (site.symbol.as_str(), site.target)).collect::<Vec<_>>(), [("slot_target", 12)],
                "zero-offset {op:?} records resolved S plus the RELA addend");
        }
    }
}

#[test]
fn named_faults_are_rejected_or_reported() {
    let roots = vec!["start".to_string()];
    let map = ImportMap::parse(MAP).expect("fixture map is valid");
    for (fault, diagnostic, member, section, offset) in [
        ("thin", "thin archive", "<archive>", "<ar-header>", None),
        ("rel", "SHT_REL", "entry_member_with_a_long_name.o", ".rel.text.start", None),
        ("tls", "SHF_TLS", "entry_member_with_a_long_name.o", ".data.state", None),
        ("group", "GROUP", "entry_member_with_a_long_name.o", ".group", None),
        ("unknown", "unknown section name", "entry_member_with_a_long_name.o", ".surprise", None),
        ("machine", "e_machine = 0x5d", "entry_member_with_a_long_name.o", "<elf-header>", None),
        ("relocation", "unsupported relocation kind 99", "entry_member_with_a_long_name.o", ".text.start", Some(0)),
        ("callee", "ASM_EXPAND literal names another callee", "entry_member_with_a_long_name.o", ".text.start", Some(35)),
        ("no-callx", "ASM_EXPAND not followed by CALLXn", "entry_member_with_a_long_name.o", ".text.start", Some(35)),
        ("slot", "SLOT0_OP in-place target", "entry_member_with_a_long_name.o", ".text.start", Some(35)),
        ("padding", "exec bytes not covered by .xt.prop", "unused.o", ".text.unreached", Some(6)),
        ("callx4", "CALLX4 into import gate_fn", "entry_member_with_a_long_name.o", ".text.start", Some(44)),
        ("call4", "CALL4 into import gate_fn", "entry_member_with_a_long_name.o", ".text.start", Some(50)),
        ("callx0", "CALLX0 into import gate_fn", "entry_member_with_a_long_name.o", ".text.start", Some(44)),
        ("call0", "CALL0 into import gate_fn", "entry_member_with_a_long_name.o", ".text.start", Some(50)),
        ("duplicate", "two strong definitions of override", "entry_member_with_a_long_name.o", ".text.start", Some(32)),
        ("truncated", "truncated byte range", "entry_member_with_a_long_name.o", "<section #", None),
    ] {
        let pack = support::pack(fault);
        let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
        let message = match audit(&archives, &roots, Some(&map)) {
            Err(error) => error.to_string(),
            Ok(result) => {
                assert!(!result.passed(), "planted {fault} must fail the audit");
                result.report()
            }
        };
        assert!(message.contains(diagnostic), "{fault} is named: wanted {diagnostic:?}, got {message}");
        let located = message.lines().find(|line| line.contains(diagnostic) && line.contains("core.a") && line.contains(member) && line.contains(section));
        assert!(located.is_some(), "{fault} names archive, member and section on the diagnostic: {message}");
        if let Some(offset) = offset { assert!(located.is_some_and(|line| line.contains(&format!("+0x{offset:x}"))), "{fault} retains the failing site offset: {message}"); }
        if fault == "duplicate" { assert!(message.contains("radio.a(strong.o):.text.override.strong"), "duplicate definitions name both locations: {message}"); }
    }
    for (source, line, diagnostic) in [
        ("glue x\nglue x", 2, "duplicate symbol"),
        ("gate 2 x\ngate 2 y", 2, "duplicate gate index"),
        ("\nrom x 123", 2, "ROM address"),
        ("gate -1 x", 1, "gate index"),
        ("glue x extra", 1, "expected"),
    ] {
        let error = ImportMap::parse(source).expect_err("malformed map is refused");
        assert_eq!(error.line, line, "map error gives its line for {source:?}");
        assert!(error.message.contains(diagnostic), "map error names {diagnostic}: {error}");
    }
    assert_eq!(parse_roots("a b").expect_err("multiple roots on a line are refused").line, 1, "roots file errors name the line");
    let pack = support::pack("");
    let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
    for (map, diagnostic) in [(ImportMap::default(), "ABSENT"), (ImportMap::parse(&MAP.replace("glue glue_data", "weak-absent glue_data")).expect("map syntax stays valid"), "referenced strongly")] {
        let result = audit(&archives, &roots, Some(&map)).expect("contract failures do not break parsing");
        assert!(!result.passed() && result.report().contains(diagnostic), "import contract failure is named: {diagnostic}");
    }
    let mut bsd = support::archive(&[("small.o", vec![0; 4])]);
    bsd[72..88].copy_from_slice(b"#1/4            ");
    assert!(ar::parse("bsd.a", &bsd).expect_err("BSD extended names are refused").to_string().contains("BSD #1/"), "BSD name failure is explicit");
}

#[test]
fn tracked_pack_contracts_parse() {
    let map = ImportMap::parse(include_str!("../packs/esp32s3-idf-v6.1.1.imports")).expect("the tracked import map parses");
    let roots = parse_roots(include_str!("../packs/esp32s3-idf-v6.1.1-sta.roots")).expect("the tracked roots parse");
    let gates: Vec<u32> = map.entries.values().filter_map(|c| if let ImportClass::Gate(i) = c { Some(*i) } else { None }).collect::<BTreeSet<_>>().into_iter().collect();
    assert_eq!(gates, (0..gates.len() as u32).collect::<Vec<_>>(), "gate indices are dense from zero, as the gate table needs");
    // 137 existing names + 1 BSS glue record for the stand-in.
    assert_eq!((map.entries.len(), gates.len()), (138, 13), "the station contract includes the pack imports and the names used by island glue");
    assert_eq!(roots.iter().collect::<BTreeSet<_>>().len(), roots.len(), "roots are unique, so entry-table indices are unambiguous");
    assert_eq!(roots.len(), 55, "the station entry list has 55 roots");
}

const LINK_MAP: &str = "gate 0 gate_fn\ngate 1 spare_gate\nglue glue_data\nglue glue_rodata\nrom rom_clock 0x40001000\nweak-absent optional\n";

fn layout() -> IslandLayout {
    IslandLayout {
        mmu_page: 192, flash_bytes: 0x1_0000,
        window: Region { start: 0x3fc8_8000, bytes: 0x1000 }, window_exec: 0x4037_8000,
    }
}

fn fixture_glue() -> Vec<Glue> {
    parse_glue("# fixture objects\nglue glue_data dram 16 12 zero\nglue glue_rodata rodata 8 hex 01 0203 0405\n")
        .expect("fixture glue parses both zero fill and hex groups")
}

fn link_fixture(pack: &support::Pack, map: &ImportMap, glue: &[Glue], layout: &IslandLayout) -> Island {
    let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
    link(&archives, &["start".into(), "helper".into()], map, glue, layout).expect("fixture island links")
}

fn map_address(island: &Island, name: &str) -> u32 {
    island.map.lines().find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields[3] == name { Some(u32::from_str_radix(&fields[0][2..], 16).expect("map address is hex")) } else { None }
    }).unwrap_or_else(|| panic!("symbol map contains {name}"))
}

fn image_offset(island: &Island, address: u32) -> usize {
    island.segments.iter().find_map(|segment| {
        if segment.kind != SegmentKind::Bss && address >= segment.address && address - segment.address < segment.bytes {
            Some((segment.file_offset + address - segment.address) as usize)
        } else { None }
    }).unwrap_or_else(|| panic!("address {address:#x} belongs to an initialized segment"))
}

fn word_at(island: &Island, address: u32) -> u32 {
    let offset = image_offset(island, address);
    u32::from_le_bytes(island.image[offset..offset + 4].try_into().expect("word fits image"))
}

fn instruction_at(island: &Island, address: u32) -> xt::Instruction {
    let offset = image_offset(island, address);
    let length = if island.image[offset] & 15 >= 8 { 2 } else { 3 };
    let bits = island.image[offset..offset + length].iter().enumerate()
        .fold(0, |bits, (n, b)| bits | (u32::from(*b) << (8 * n)));
    xt::instruction(bits, address).expect("linked instruction has a readable format")
}

#[test]
fn link_places_patches_and_describes() {
    let pack = support::pack("data-root");
    let map = ImportMap::parse(LINK_MAP).expect("link map is valid");
    let glue = fixture_glue();
    let layout = layout();
    let island = link_fixture(&pack, &map, &glue, &layout);
    assert!(island.audit.passed(), "linked island retains its passing audit");
    assert_eq!(Header::decode(&island.image).expect("image header decodes"), island.header, "header round trip preserves every field");
    assert_eq!(&island.image[..Header::SIZE as usize], &island.header.encode(), "encoded header occupies offset zero");
    assert_eq!(crc32(b"123456789"), 0xcbf43926, "CRC uses the IEEE reflected check value");
    assert_eq!(island.header.crc, crc32(&island.image[Header::SIZE as usize..]), "CRC covers every byte after the header");
    assert_eq!(island.header.image_size as usize, island.image.len(), "image size covers the complete output");
    assert_eq!((island.header.magic, island.header.format, island.header.header_size, island.header.mmu_page),
        (0x454c5349, 1, 64, layout.mmu_page), "header identifies the version and mapping");
    assert_eq!((island.header.entry_count, island.header.gate_count, island.header.gate_table), (2, 2, layout.window.start), "header describes entry and gate ABI tables");
    assert_eq!(island.header.entry_offset, Header::SIZE + island.header.segment_count * Segment::SIZE, "entry table immediately follows the compact segment table");
    // The header and tables have their sectors to themselves, so Flash to can commit by the header.
    let (tables_end, body) = ((island.header.entry_offset + island.header.entry_count * 4) as usize, island.header.body_offset() as usize);
    assert!(body % Header::SECTOR as usize == 0 && body >= tables_end && body - tables_end < Header::SECTOR as usize,
        "code starts at the first sector boundary past the tables");
    assert!(island.image[tables_end..body].iter().all(|&b| b == 0xff), "the rest of the header sectors is erased flash");
    assert_eq!(island.segments.iter().filter(|s| s.file_offset != 0).map(|s| s.file_offset).min(), Some(body as u32),
        "the first segment with file bytes begins the body");
    assert_eq!(island.segments.iter().map(|s| s.kind).collect::<Vec<_>>(),
        [SegmentKind::Text, SegmentKind::Rodata, SegmentKind::Iram, SegmentKind::Data, SegmentKind::Bss], "nonempty segments retain placement order");
    assert_eq!(island.segments.len(), island.header.segment_count as usize, "segment count matches emitted records");
    let (iram, data, bss) = (island.segments[2], island.segments[3], island.segments[4]);
    let align16 = |end: u32| (end + 15) & !15;
    let iram_offset = align16(island.header.gate_count * 4);
    assert_eq!(iram.address, layout.window_exec + iram_offset, "IRAM follows the gate table on the instruction alias");
    assert_eq!(data.address, align16(layout.window.start + iram_offset + iram.bytes), "DATA follows IRAM on the data alias");
    assert_eq!(bss.address, align16(data.address + data.bytes), "BSS follows DATA");
    assert_eq!(island.guard, align16(bss.address + bss.bytes), "guard follows BSS including COMMON");
    assert_eq!(island.window_end, island.guard + 4, "the occupied window ends just after the guard word");
    for (index, segment) in island.segments.iter().enumerate() {
        let offset = island.header.segment_offset as usize + index * Segment::SIZE as usize;
        assert_eq!(Segment::decode(&island.image[offset..]).expect("segment decodes"), *segment, "segment {index} wire record matches placement");
        if matches!(segment.kind, SegmentKind::Text | SegmentKind::Rodata) {
            let base = if segment.kind == SegmentKind::Text { 0x4200_0000 } else { 0x3c00_0000 };
            assert_eq!(segment.address, base + layout.mmu_page * 0x1_0000 + segment.file_offset, "{:?} uses its specified flash alias", segment.kind);
        }
        if segment.kind == SegmentKind::Bss { assert_eq!(segment.file_offset, 0, "BSS has no file bytes"); }
    }
    for id in &island.audit.kept {
        let section = &island.audit.objects[id.object].sections[id.section];
        let address = map_address(&island, &section.name);
        let kind = match section.class {
            Class::Text => SegmentKind::Text, Class::Iram => SegmentKind::Iram,
            Class::Rodata => SegmentKind::Rodata, Class::Dram | Class::Data => SegmentKind::Data,
            Class::Bss => SegmentKind::Bss, Class::Meta => panic!("metadata cannot be kept"),
        };
        let segment = island.segments.iter().find(|s| s.kind == kind).expect("placed class has a segment");
        assert!(address >= segment.address && u64::from(address) + u64::from(section.size) <= u64::from(segment.address) + u64::from(segment.bytes), "segment covers all bytes of {}", section.name);
        let align = section.align.max(if matches!(kind, SegmentKind::Text | SegmentKind::Iram) { 4 } else { 1 });
        assert_eq!(address % align, 0, "{} satisfies its address alignment", section.name);
    }
    assert_eq!(island.entries, [map_address(&island, "start"), map_address(&island, "helper")], "entry addresses preserve roots order");
    for (index, entry) in island.entries.iter().enumerate() {
        let offset = island.header.entry_offset as usize + index * 4;
        assert_eq!(&island.image[offset..offset + 4], &entry.to_le_bytes(), "entry {index} is stored little-endian");
    }
    for (index, name) in ["gate_fn", "spare_gate"].into_iter().enumerate() {
        let address = map_address(&island, name);
        assert_eq!(address % 4, 0, "gate {index} stub is four-aligned");
        for (offset, op, s, t) in [(0, Op::L32r, 0, 9), (3, Op::L32i, 9, 9), (6, Op::Jx, 9, 0)] {
            let instruction = instruction_at(&island, address + offset);
            let decoded = instruction.decoded.expect("gate instruction decodes in the model");
            assert_eq!((instruction.length, decoded.op, decoded.s, decoded.t), (3, op, s, t), "gate {index} opcode/registers at +{offset}");
            if op == Op::L32r {
                let literal = instruction.target().expect("gate load has a literal address");
                assert_eq!(word_at(&island, literal), layout.window.start + index as u32 * 4, "gate {index} literal selects its SRAM table word");
                assert_eq!(literal + (2 - index as u32) * 4, map_address(&island, "gate_fn"), "all gate literal words precede all stubs");
            }
            if op == Op::L32i { assert_eq!(decoded.imm, 0, "gate table dereference uses offset zero"); }
        }
        let offset = image_offset(&island, address);
        assert_eq!(&island.image[offset + 9..offset + 12], &[0; 3], "gate {index} ends in three zero bytes");
    }
    for word in island.audit.words.iter().filter(|w| w.kept) {
        let base = match &word.value.base {
            ValueBase::Section(id) => map_address(&island, &island.audit.objects[id.object].sections[id.section].name),
            ValueBase::Common(id) => map_address(&island, &island.audit.objects[id.object].symbols[&id.table][id.index].name),
            ValueBase::Absolute => 0,
            ValueBase::Import(name) => map_address(&island, name),
        };
        let site = map_address(&island, &word.location.section) + word.location.offset.expect("word location has an offset");
        assert_eq!(word_at(&island, site), base.wrapping_add(word.value.offset), "R_XTENSA_32 {} equals map-derived S + A + original word", word.location);
    }
    for id in &island.audit.kept {
        let object = &island.audit.objects[id.object];
        for relocation in object.relocations.get(&id.section).into_iter().flatten().filter(|r| r.kind == 20) {
            let symbol = &object.symbols[&relocation.symtab][relocation.symbol];
            let base = if symbol.name.is_empty() {
                let SymbolSection::Section(section) = symbol.shndx else { panic!("fixture section symbol identifies a section"); };
                map_address(&island, &object.sections[section].name) + symbol.value
            } else { map_address(&island, &symbol.name) };
            let site = map_address(&island, &object.sections[id.section].name) + relocation.offset;
            let patched = instruction_at(&island, site);
            let original = &island.audit.decoded[id.object].instructions[&id.section][&relocation.offset];
            assert_eq!(patched.target(), Some(base.wrapping_add(relocation.addend as u32)), "SLOT0_OP {} reaches map-derived S + A", object.location(id.section).at(relocation.offset));
            assert_eq!((&patched.kind, patched.op()), (&original.kind, original.op()), "SLOT0_OP preserves its operation");
        }
    }
    let start_section = map_address(&island, ".text.start");
    assert_eq!(instruction_at(&island, start_section + 50).target(), Some(map_address(&island, "tail")), "zero-field CALL8 is rewritten to tail");
    assert_ne!(&island.image[image_offset(&island, start_section + 50)..][..3], &support::call(8, 50, 52).to_le_bytes()[..3], "zero-field CALL8 bytes actually change");
    let helper = map_address(&island, "helper");
    assert_eq!(helper, iram.address, "IRAM function keeps its instruction-bus address and alignment");
    assert_eq!(instruction_at(&island, helper).op(), Some(Op::Entry), "IRAM initial image contains its function");
    let common = map_address(&island, "scratch");
    assert!(common >= bss.address && common + 12 <= bss.address + bss.bytes, "BSS zero extent covers COMMON and alignment padding");
    assert_eq!(common % 8, 0, "COMMON uses its declared alignment");
    for object in &glue {
        let address = map_address(&island, &object.name);
        let offset = image_offset(&island, address);
        assert_eq!(address % object.align, 0, "glue {} alignment is preserved", object.name);
        assert_eq!(&island.image[offset..offset + object.bytes.len()], object.bytes.as_slice(), "glue {} initial bytes are emitted", object.name);
    }
    let state = map_address(&island, "state");
    assert_eq!(island.objects["data_root"], Region { start: map_address(&island, "data_root"), bytes: 16 });
    assert_eq!(island.objects["scratch"], Region { start: common, bytes: 12 });
    assert!(!island.objects.contains_key("start"), "function");
    assert!(!island.objects.contains_key("state"), "local data alias");
    assert_eq!(island.binding().objects, island.objects);
    assert_eq!(island.functions["start"], Region { start: island.entries[0], bytes: 36 });
    assert_eq!(island.functions["helper"], Region { start: island.entries[1], bytes: 5 });
    assert!(!island.functions.contains_key("data_root"));
    assert_eq!(island.binding().functions, island.functions);
    assert_eq!(island.binding().code, island.code);
    assert_eq!(word_at(&island, state), map_address(&island, "glue_data") + 4, "glue address participates in relocations");
    assert_eq!(word_at(&island, state + 4), 0, "weak-absent resolves to zero");
    assert_eq!(word_at(&island, state + 8), 0x40001000, "ROM import resolves to its absolute address");
    let rows: Vec<_> = island.map.lines().map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 5, "symbol map rows have address, size, kind, name and location");
        (u32::from_str_radix(&fields[0][2..], 16).expect("map address parses"), fields[3])
    }).collect();
    assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]), "symbol map sorts by address then name");
    let kind_of = |name: &str| island.map.lines().map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|fields| fields[3] == name).map(|fields| fields[2].to_owned());
    assert_eq!(kind_of("start").as_deref(), Some("TEXT/FUNC"), "an STT_FUNC symbol keeps its class and is marked FUNC");
    assert_eq!(kind_of("state").as_deref(), Some("DATA"), "an object symbol keeps its bare class");
    assert!(!island.map.contains("unreached") && !island.map.contains("override.weak"), "dropped sections do not appear in the map");
    for name in ["FLASH:", "WINDOW:", "IRAM:", "TEXT:", "RODATA:", "DATA:", "BSS:"] {
        assert!(island.size_report(&layout).contains(name), "size report names region or segment {name}");
    }

    // Every accepted operand family is rewritten against a selected definition
    // in another object, with a nonzero RELA addend. No instruction is executed.
    let pc = 0x4200_0000 + layout.mmu_page * 0x1_0000 + (Header::SIZE + Segment::SIZE + 4).next_multiple_of(Header::SECTOR);
    let mut forms = vec![0xffff91, 0x000005, 0x000015, 0x000025, 0x000035, 0x000006,
        0x000216, 0x000256, 0x000296, 0x0002d6,
        0x000226, 0x000266, 0x0002a6, 0x0002e6, 0x0002b6, 0x0002f6,
        0x000276, 0x001276, 0x028c, 0x02cc, 0x008276, 0x009276, 0x00a276];
    forms.extend((0..16).map(|r| 7 | (3 << 4) | (2 << 8) | (r << 12)));
    for bits in forms {
        let original = xt::instruction(bits, pc).expect("operand fixture is readable");
        assert!(original.slot_format(), "fixture {} is an accepted SLOT0_OP format", original.kind);
        let target = if original.op() == Some(Op::L32r) { pc - 4 } else { pc + 28 };
        let bytes = support::slot_pack(bits, target - 4, 4);
        let linked = link(&[Archive { name: "slots.a", data: &bytes }], &["slot_root".into()], &ImportMap::default(), &[], &layout)
            .unwrap_or_else(|e| panic!("{} operand links: {e}", original.kind));
        assert_eq!(linked.entries, [pc], "single-text fixture placement is independent of the target");
        let decoded = instruction_at(&linked, pc);
        assert_eq!(decoded.target(), Some(target), "{} encodes resolved strong S + A", original.kind);
        assert_eq!(decoded.kind, original.kind, "{} preserves its opcode", original.kind);
        if let (Some(before), Some(after)) = (original.decoded, decoded.decoded) {
            assert_eq!((after.r, after.s, after.t), (before.r, before.s, before.t), "{} preserves register operands", original.kind);
        }
        assert_eq!(linked.segments.len(), 1, "empty RODATA, IRAM, DATA and BSS segments are omitted");
    }
    for (name, bits, base, offsets) in [
        ("L32R one-extension", 0xffff91, pc, [-262144i32, -4]),
        ("CALL signed words", 0x25, pc + 4, [-524288, 524284]),
        ("J signed bytes", 6, pc + 4, [-131072, 131071]),
        ("BRI12 signed bytes", 0x216, pc + 4, [-2048, 2047]),
        ("BRI8 signed bytes", 0x226, pc + 4, [-128, 127]),
        ("RRI8 signed bytes", 0x1237, pc + 4, [-128, 127]),
        ("BF signed bytes", 0x276, pc + 4, [-128, 127]),
        ("RI6 unsigned split", 0x28c, pc + 4, [0, 63]),
        ("LOOP unsigned LEND", 0x8276, pc + 4, [0, 255]),
    ] {
        for offset in offsets {
            let target = base.wrapping_add(offset as u32);
            let bytes = support::slot_pack(bits, target, 0);
            let linked = link(&[Archive { name: "bounds.a", data: &bytes }], &["slot_root".into()], &ImportMap::default(), &[], &layout)
                .unwrap_or_else(|e| panic!("{name} boundary {offset} links: {e}"));
            assert_eq!(instruction_at(&linked, pc).target(), Some(target), "{name} boundary {offset} decodes to its exact target");
        }
    }
    let bytes = support::storage_pack();
    let storage = link(&[Archive { name: "storage.a", data: &bytes }], &["ram_root".into()], &ImportMap::default(), &[], &layout)
        .expect("IRAM relocations and explicit BSS link");
    let ram = map_address(&storage, ".iram1.root");
    for (offset, name) in [(0, "wrap_data"), (4, "live_bss"), (8, "extra_common")] {
        assert_eq!(word_at(&storage, ram + offset), map_address(&storage, name), "IRAM initial image word references {name}");
    }
    assert_eq!(instruction_at(&storage, storage.entries[0]).target(), Some(ram), "IRAM SLOT0_OP uses runtime PC rather than flash image address");
    assert_eq!(word_at(&storage, map_address(&storage, "wrap_data")), 0xffff_fffd, "R_XTENSA_32 adds a negative addend and wraps modulo 2^32");
    assert_eq!(storage.segments.iter().map(|s| s.kind).collect::<Vec<_>>(), [SegmentKind::Iram, SegmentKind::Data, SegmentKind::Bss], "RAM-only island omits both XIP segments");
    let zeroed = storage.segments.iter().find(|s| s.kind == SegmentKind::Bss).expect("storage BSS segment exists");
    let live = map_address(&storage, "live_bss");
    let common = map_address(&storage, "extra_common");
    assert_eq!((live % 16, common % 32), (0, 0), "BSS and COMMON satisfy separate alignments");
    assert!(live >= zeroed.address && common >= live + 7 && common + 3 == zeroed.address + zeroed.bytes, "BSS includes the section, padding and subsequent COMMON");
    assert_eq!(storage.image.len() as u32, storage.segments.iter().filter(|s| s.kind != SegmentKind::Bss).map(|s| s.file_offset + s.bytes).max().expect("storage has initialized segments"), "BSS consumes no bytes in the flash image");

    let glue = parse_glue("glue record bss 8 13 zero").unwrap();
    assert_eq!(glue[0].bytes, [0; 13]);
    let map = ImportMap::parse("glue record").unwrap();
    let extended = link(&[Archive { name: "storage.a", data: &bytes }], &["ram_root".into()], &map, &glue, &layout).unwrap();
    let bss = extended.segments.iter().find(|s| s.kind == SegmentKind::Bss).unwrap();
    let record = extended.glue["record"];
    assert_eq!(record, (zeroed.address + zeroed.bytes).next_multiple_of(8), "record follows all pack BSS and COMMONs");
    assert_eq!((bss.address, bss.address + bss.bytes), (zeroed.address, record + 13));
    assert_eq!(extended.guard, (record + 13).next_multiple_of(16));
    assert_eq!((bss.file_offset, extended.image.len()), (0, storage.image.len()), "the loader zeroes the record without a flash payload");
    assert!(extended.map.lines().any(|row| row.starts_with(&format!("0x{record:08x} 13 GLUE record "))));
}

#[test]
fn window_with_empty_iram_places_data_after_gates() {
    let map = ImportMap::parse("gate 0 gate_fn\nglue state").expect("map parses");
    let glue = parse_glue("glue state dram 4 4 zero").expect("data glue parses");
    let layout = layout();
    let island = link(&[], &[], &map, &glue, &layout).expect("gate and data glue link without IRAM");
    assert_eq!(island.segments.iter().map(|s| s.kind).collect::<Vec<_>>(), [SegmentKind::Text, SegmentKind::Data]);
    assert_eq!(island.segments[1].address, layout.window.start + 16, "DATA starts at the next aligned address after one gate word");
    assert_eq!(island.glue["state"], island.segments[1].address);
    assert_eq!(island.guard, layout.window.start + 32, "empty BSS consumes no space before the guard");
}

#[test]
fn glue_parser_refusals() {
    for (text, line, diagnostic) in [
        ("glue x text 2 hex 1df0", 1, "text alignment must be at least 4"),
        ("glue x text 4 entry 2 hex 00000000", 1, "entry offset must be a multiple of 4"),
        ("glue x text 4 entry 4 hex 00000000", 1, "entry offset must be inside"),
        ("glue x text 4 entry 8 hex 00000000", 1, "entry offset must be inside"),
        ("glue x text 4 entry nope hex 00000000", 1, "invalid entry offset"),
        ("glue x dram 4 entry 4 8 zero", 1, "entry requires text"),
        ("glue x rodata 4 entry 0 hex 01", 1, "entry requires text"),
        ("glue x bss 4 entry 0 4 zero", 1, "entry requires text"),
        ("glue x bss 4 hex 00000000", 1, "<size> zero"),
        ("glue x bss 4 4 zero\nreloc x 0 target", 2, "BSS glue cannot have relocations"),
        ("glue x text 4 8 zero", 1, "text does not allow zero fill"),
        ("reloc x 0 target\nglue x text 4 hex 00000000", 1, "unknown glue x"),
        ("glue x dram 4 4 zero\nreloc x 4 target", 2, "past the end"),
        ("glue x rodata 4 hex 00000000\nreloc x 1 target", 2, "multiple of 4"),
        ("glue x rodata 4 4 zero\nreloc x 0 first\nreloc x 0 second", 3, "duplicate reloc offset"),
        ("glue x text 4 hex 0000000000000000\nreloc x 2 target", 2, "multiple of 4"),
        ("glue x text 4 hex 00000000\nreloc x 4 target", 2, "past the end"),
        ("glue x text 4 hex 0000000000\nreloc x 4 target", 2, "past the end"),
        ("glue x text 4 hex 00000000\nreloc x 0xfffffffc target", 2, "past the end"),
        ("glue x text 4 hex 00000000\nreloc x 0 first\nreloc x 0x0 second", 3, "duplicate reloc offset"),
        ("glue x text 4 hex 00000000\nreloc x nope target", 2, "invalid reloc offset"),
        ("glue x text 4 hex 00000000\nreloc x 0", 2, "expected reloc"),
    ] {
        let error = parse_glue(text).expect_err("invalid glue refuses");
        assert_eq!(error.line, line, "glue parser locates {diagnostic}");
        assert!(error.message.contains(diagnostic), "glue parser explains {diagnostic}: {error}");
    }
}

#[test]
fn data_glue_points_to_placed_glue_and_keeps_its_pack_target() {
    let pack = support::glue_pack();
    let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
    let roots = ["start".into(), "helper".into()];
    let map = ImportMap::parse(&format!("{LINK_MAP}glue event_ptr\n")).unwrap();
    let mut glue = parse_glue("glue glue_data dram 16 12 zero\n\
        reloc glue_data 4 glue_only\n\
        glue event_ptr rodata 4 hex ffffffff\n\
        reloc event_ptr 0 glue_rodata\n\
        glue glue_rodata rodata 8 hex 01 0203 0405").unwrap();
    let island = link(&archives, &roots, &map, &glue, &layout()).expect("data glue relocations link");
    assert_eq!(word_at(&island, island.glue["event_ptr"]), island.glue["glue_rodata"],
        "the rodata pointer holds the placed address of a later object");
    assert_eq!(word_at(&island, island.glue["glue_data"] + 4), map_address(&island, "glue_only"),
        "the DRAM pointer keeps and addresses a pack function outside the entry roots");

    for (offset, target, reason) in [(4, "rom_clock", "glue may not reach the ROM"), (12, "glue_only", "past the end")] {
        glue[0].relocations[0] = GlueRelocation { offset, symbol: target.into() };
        let error = link(&archives, &roots, &map, &glue, &layout()).expect_err("invalid data pointer").to_string();
        assert!(error.contains(&format!("glue_data+{offset:#x}")) && error.contains(target) && error.contains(reason),
            "the refusal locates the data pointer and names its target: {error}");
    }
}

#[test]
fn direct_calls_and_relocated_words_use_glue_entries() {
    let mut caller = support::Object::new();
    let mut code = Vec::new();
    for bits in [support::entry(), support::call(8, 3, 4), support::call(8, 6, 8), support::retwn()] {
        support::emit(&mut code, bits);
    }
    let size = code.len() as u32;
    let text = caller.section(".text.caller", 1, 6, code);
    caller.symbol("caller", 1, 2, text as u16, 0, size);
    caller.property(text, 0, size, 2);
    let calls = [(3, "prefixed", 12), (6, "leaf", 0)];
    for (site, name, _) in calls {
        let import = caller.symbol(name, 1, 0, 0, 0, 0);
        caller.relocation(text, site, 20, import, 0);
    }
    let archive = support::archive(&[("caller.o", caller.encode())]);
    let map = ImportMap::parse("glue reader\nglue prefixed\nglue leaf").unwrap();
    // Each function body is ENTRY a1,32 followed by RETW.N. The reader's
    // literal refers forward to prefixed, whose first three words are literals.
    let glue = parse_glue("glue reader text 4 entry 4 hex 00000000 3641001df0\n\
        reloc reader 0 prefixed\n\
        glue prefixed text 4 entry 12 hex 00000000 00000000 00000000 3641001df0\n\
        glue leaf text 4 hex 3641001df0").unwrap();
    let island = link(&[Archive { name: "calls.a", data: &archive }], &["caller".into()], &map, &glue, &layout())
        .expect("both pack calls and the forward glue reference link");
    for (site, name, offset) in calls {
        let start = map_address(&island, name);
        let entry = start + offset;
        let call = instruction_at(&island, island.entries[0] + site);
        assert_eq!((call.op(), call.target()), (Some(Op::Call8), Some(entry)), "direct pack CALL8 reaches {name} at +{offset}");
        assert_eq!(island.glue[name], entry, "published {name} address matches its call target");
        assert_eq!(instruction_at(&island, entry).op(), Some(Op::Entry), "{name} starts execution at ENTRY");
        assert_eq!(symbolize(&island.map, entry + 4), Some((name, offset + 4)), "the map covers the last byte of {name}");
    }
    let start = map_address(&island, "prefixed");
    assert_eq!(word_at(&island, map_address(&island, "reader")), start + 12, "glue literal resolves to the callable entry");
    assert_eq!(symbolize(&island.map, start + 8), Some(("prefixed", 8)), "PC in the literal prefix symbolizes from the object start");
}

#[test]
fn text_glue_reaches_patches_and_preserves_entries() {
    let pack = support::glue_pack();
    let layout = layout();
    let mut glue = fixture_glue();
    let baseline = link_fixture(&pack, &ImportMap::parse(LINK_MAP).expect("baseline map parses"), &glue, &layout);
    let text = parse_glue("glue z_text_prefix text 4 hex 1df0\n\
        glue phy_calls text 4 entry 8 hex 07000000 ffffffff 3641001df0\n\
        reloc phy_calls 0 glue_only\n\
        reloc phy_calls 0x4 spare_gate\n\
        glue a_common_word text 8 entry 0x8 hex 03000000 00000000 3641001df0\n\
        reloc a_common_word 0 glue_common\n\
        reloc a_common_word 4 glue_data\n").expect("text glue and decimal/hex offsets parse");
    assert_eq!((text[1].region, text[1].align, text[1].entry), (GlueRegion::Text, 4, 8), "text declares its alignment and entry");
    assert_eq!(text[1].relocations, [
        GlueRelocation { offset: 0, symbol: "glue_only".into() },
        GlueRelocation { offset: 4, symbol: "spare_gate".into() },
    ], "parser attaches each relocation to its earlier text object");
    assert!(glue.iter().all(|g| g.relocations.is_empty()), "the baseline data objects have no relocations");
    let calls_size = text[1].bytes.len() as u32;
    glue.extend(text);
    let map = ImportMap::parse(&format!("{LINK_MAP}glue z_text_prefix\nglue phy_calls\nglue a_common_word\n"))
        .expect("text glue map parses");
    let island = link_fixture(&pack, &map, &glue, &layout);
    let address = map_address(&island, "phy_calls");
    let target = map_address(&island, "glue_only");
    let gate = map_address(&island, "spare_gate");
    assert_eq!(word_at(&island, address), target.wrapping_add(7), "glue pack relocation adds the original word to the selected function address");
    assert_eq!(word_at(&island, address + 4), gate.wrapping_add(u32::MAX), "glue gate relocation uses the stub address and wraps the original word as R_XTENSA_32");
    assert!(!baseline.map.contains("glue_only") && !baseline.map.contains("glue_common"), "pack function and COMMON are unreachable without glue");
    assert!(island.audit.kept.iter().any(|id| island.audit.objects[id.object].sections[id.section].name == ".text.glue_only"), "glue-only pack function is kept by the audit");
    assert_eq!(target, map_address(&island, ".text.glue_only"), "glue-only function has a placed text section");
    assert!(island.audit.commons.iter().any(|c| c.name == "glue_common"), "glue-only COMMON is selected and kept");
    assert_eq!(word_at(&island, map_address(&island, "a_common_word")), map_address(&island, "glue_common") + 3, "glue can relocate a word to a selected COMMON");
    assert_eq!(word_at(&island, map_address(&island, "a_common_word") + 4), map_address(&island, "glue_data"), "data glue resolves to the start of its storage");
    let segment = island.segments.iter().find(|s| s.kind == SegmentKind::Text).expect("text glue has a TEXT segment");
    let prefix = map_address(&island, "z_text_prefix");
    assert_eq!(prefix, gate + 12, "text glue starts after the last gate stub");
    assert_eq!(address, prefix + 4, "text starts at the next aligned address after the two-byte object");
    assert_eq!(address % 4, 0, "the object's start is word aligned");
    assert!(map_address(&island, "a_common_word") >= address + calls_size, "text glue follows caller order rather than name order");
    for object in &glue {
        let placed = map_address(&island, &object.name);
        assert_eq!(island.glue[&object.name], placed + object.entry, "Island.glue exposes the resolved address of {} in every region", object.name);
        let row = island.map.lines().find(|line| line.split_whitespace().nth(3) == Some(object.name.as_str())).expect("glue object has a map row");
        let kind = if object.region == GlueRegion::Text { "GLUE/FUNC" } else { "GLUE" };
        assert_eq!(row.split_whitespace().nth(2), Some(kind), "{} has the correct data/function map kind", object.name);
        if object.region == GlueRegion::Text {
            assert!(placed >= segment.address && placed + object.bytes.len() as u32 <= segment.address + segment.bytes, "TEXT covers all bytes of {}", object.name);
            assert_eq!(placed % object.align, 0, "{} satisfies text alignment", object.name);
        }
    }
    assert_eq!(island.glue.len(), glue.len(), "Island.glue contains exactly the caller's objects");
    let in_code = |pc: u32| island.code.iter().any(|r| (r.start..r.start + r.bytes).contains(&pc));
    assert!(island.entries.iter().all(|&pc| in_code(pc)), "pack text and IRAM entries use fetch addresses");
    assert!(island.code.iter().all(|r| r.bytes != 0));
    assert!(island.code.windows(2).all(|pair| pair[0].start + pair[0].bytes <= pair[1].start));
    for object in glue.iter().filter(|g| g.region == GlueRegion::Text) {
        let start = map_address(&island, &object.name);
        assert!(!(start..start + object.bytes.len() as u32).any(in_code), "{} executes as glue", object.name);
    }
    assert!(!(gate..gate + 12).any(in_code), "the gate stub executes as linked");
    assert_eq!(symbolize(&island.map, address + 5), Some(("phy_calls", 5)), "a PC inside glue symbolizes to the glue name");
    let overlapping = format!("0x{:08x} 1 TEXT small_section fixture\n{}", address + 5, island.map);
    assert_eq!(symbolize(&overlapping, address + 5), Some(("phy_calls", 5)), "GLUE/FUNC wins over a smaller overlapping nonfunction row");
    assert_eq!(island.entries, baseline.entries, "glue reach roots do not enter or change the fixture entry addresses");
    assert_eq!((island.header.entry_offset, island.header.entry_count), (baseline.header.entry_offset, baseline.header.entry_count), "header entry table metadata still describes roots only");
    let entry_bytes = |island: &Island| {
        let start = island.header.entry_offset as usize;
        island.image[start..start + island.header.entry_count as usize * 4].to_vec()
    };
    assert_eq!(entry_bytes(&island), entry_bytes(&baseline), "encoded entry words are unchanged by glue reach roots");
    assert_eq!(island.header.abi_hash, baseline.header.abi_hash, "glue reach roots and glue names do not affect the ABI hash");

    let only = parse_glue("glue only_text text 4 hex 1df0").expect("text without relocations parses");
    let only_map = ImportMap::parse("glue only_text").expect("glue-only map parses");
    let only = link(&[], &[], &only_map, &only, &layout).expect("nonempty text glue links without pack text or gates");
    assert_eq!(only.segments.iter().map(|s| s.kind).collect::<Vec<_>>(), [SegmentKind::Text], "text glue alone creates the TEXT segment");
    assert_eq!(only.segments[0].address, only.glue["only_text"], "glue-only TEXT segment starts at its object");
    assert_eq!(only.segments[0].bytes, 2, "glue-only TEXT segment covers its object");
}

#[test]
fn text_glue_forbidden_targets_are_located_before_placement() {
    let pack = support::pack("");
    let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
    let map = ImportMap::parse(&format!("{LINK_MAP}glue forbidden_text\n")).expect("refusal map parses");
    let layout = IslandLayout { flash_bytes: 0, ..layout() };
    for (symbol, reason) in [
        ("rom_clock", "glue may not reach the ROM"),
        ("optional", "weak-absent"),
        ("absolute", "absolute (SHN_ABS)"),
        ("not_defined", "undefined symbol"),
    ] {
        let mut glue = fixture_glue();
        glue.extend(parse_glue(&format!("glue forbidden_text text 4 hex 00000000 00000000\nreloc forbidden_text 4 {symbol}\n"))
            .expect("forbidden target has valid relocation syntax"));
        let error = link(&archives, &["start".into()], &map, &glue, &layout).expect_err("forbidden glue target refuses");
        let message = error.to_string();
        assert!(message.contains("forbidden_text+0x4") && message.contains(symbol), "glue target refusal names object, offset and symbol {symbol}: {message}");
        assert!(message.contains(reason), "{symbol} target rule is checked before the zero-capacity placement failure: {message}");
    }
}

#[test]
fn relink_is_byte_identical_and_hashed() {
    let pack = support::pack("");
    let map = ImportMap::parse(LINK_MAP).expect("link map parses");
    let glue = fixture_glue();
    let layout = layout();
    let first = link_fixture(&pack, &map, &glue, &layout);
    let second = link_fixture(&pack, &map, &glue, &layout);
    assert_eq!(first.image, second.image, "relinking identical inputs produces identical bytes");
    assert_eq!(first.map, second.map, "relinking identical inputs produces identical symbol maps");
    let renamed = ImportMap::parse(&LINK_MAP.replace("spare_gate", "renamed_gate")).expect("renamed unused gate map parses");
    let renamed = link_fixture(&pack, &renamed, &glue, &layout);
    assert_ne!(first.header.abi_hash, renamed.header.abi_hash, "renaming any gate changes the ABI hash");
    assert_eq!(first.header.pack_hash, renamed.header.pack_hash, "gate names do not affect the pack hash");
    let mut changed = support::pack("");
    // The first archive member's decimal timestamp is ignored by the reader.
    changed.archives[0].1[24] = b'1';
    let changed = link_fixture(&changed, &map, &glue, &layout);
    assert_ne!(first.header.pack_hash, changed.header.pack_hash, "changing one archive byte changes the pack hash");
    assert_eq!(first.header.abi_hash, changed.header.abi_hash, "archive bytes do not affect the ABI hash");
    let fnv = |bytes: &[u8]| bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3));
    assert_eq!(first.header.abi_hash, fnv(b"entry start\nentry helper\ngate 0 gate_fn\ngate 1 spare_gate\n"), "ABI hash uses the specified ordered UTF-8 lines");
    let mut pack_input = Vec::new();
    for (name, bytes) in &pack.archives {
        pack_input.extend_from_slice(&(name.len() as u32).to_le_bytes());
        pack_input.extend_from_slice(name.as_bytes());
        pack_input.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        pack_input.extend_from_slice(bytes);
    }
    assert_eq!(first.header.pack_hash, fnv(&pack_input), "pack hash includes name and data lengths in input order");
    let paths: Vec<_> = pack.archives.iter().map(|(name, _)| format!("packs/{name}")).collect();
    let archives: Vec<_> = paths.iter().zip(&pack.archives).map(|(name, (_, data))| Archive { name, data }).collect();
    let relocated = link(&archives, &["start".into(), "helper".into()], &map, &glue, &layout).expect("archive directory changes are valid");
    assert_eq!(first.header.pack_hash, relocated.header.pack_hash, "pack hash uses only final path components");
    assert_eq!(first.image, relocated.image, "archive directory names do not change image bytes");
    let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
    let reversed = link(&archives, &["helper".into(), "start".into()], &map, &glue, &layout).expect("reordered roots link");
    assert_eq!(reversed.entries, [first.entries[1], first.entries[0]], "entry table follows roots order");
    assert_ne!(reversed.header.abi_hash, first.header.abi_hash, "root order affects ABI hash");
    assert_eq!(reversed.header.pack_hash, first.header.pack_hash, "root order does not affect pack hash");
}

#[test]
fn link_refusals_are_named() {
    let pack = support::pack("");
    let map = ImportMap::parse(LINK_MAP).expect("link map parses");
    let glue = fixture_glue();
    let layout = layout();
    let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
    let roots = ["start".into()];
    let mut exact = layout;
    exact.window.bytes = 100;
    let island = link(&archives, &roots, &map, &glue, &exact).expect("all window parts fit exactly");
    assert_eq!(island.window_end - layout.window.start, exact.window.bytes, "the guard uses the final four bytes");
    // The full breakdown is available even if the short window also crosses the address limit.
    for start in [layout.window.start, 0xffff_fff0] {
        let short = IslandLayout { window: Region { start, bytes: exact.window.bytes - 1 }, ..layout };
        let error = link(&archives, &roots, &map, &glue, &short).expect_err("one byte over the window bound refuses").to_string();
        for detail in ["WINDOW overflow: capacity 99 bytes, needed 100 bytes", "gate table 8", "IRAM 5", "DATA 44", "BSS 12", "guard 4", "inter-part padding 27"] {
            assert!(error.contains(detail), "window overflow reports {detail}: {error}");
        }
    }
    let tiny = IslandLayout { flash_bytes: 1, ..layout };
    let error = link(&archives, &roots, &map, &glue, &tiny).expect_err("tiny FLASH refuses").to_string();
    assert!(error.contains("FLASH") && error.contains("capacity 1 bytes") && error.contains("needed"), "FLASH overflow names capacity and required bytes: {error}");
    let gap = ImportMap::parse(&LINK_MAP.replace("gate 1", "gate 2")).expect("gapped map parses before linking");
    let error = link(&archives, &roots, &gap, &glue, &layout).expect_err("gate gap refuses").to_string();
    assert!(error.contains("missing gate index 1"), "gate gap names the missing index: {error}");
    let error = link(&archives, &roots, &map, &glue[..1], &layout).expect_err("missing glue refuses").to_string();
    assert!(error.contains("missing glue object glue_rodata"), "missing glue names its map symbol: {error}");
    let mut extra = glue.clone();
    extra.push(Glue { name: "extra_object".into(), ..glue[0].clone() });
    let error = link(&archives, &roots, &map, &extra, &layout).expect_err("extra glue refuses").to_string();
    assert!(error.contains("extra glue object extra_object"), "extra glue names its symbol: {error}");
    let mut duplicate = glue.clone();
    duplicate.push(glue[0].clone());
    let error = link(&archives, &roots, &map, &duplicate, &layout).expect_err("duplicate glue refuses").to_string();
    assert!(error.contains("duplicate glue object glue_data"), "glue cardinality is exactly one: {error}");
    let raw_map = ImportMap::parse("glue raw").unwrap();
    for (region, align, entry, len, reason) in [
        (GlueRegion::Text, 2, 0, 4, "alignment must be at least 4"),
        (GlueRegion::Text, 4, 2, 4, "entry offset must be a multiple of 4"),
        (GlueRegion::Text, 4, 0, 0, "entry offset must be inside"),
        (GlueRegion::Text, 4, 4, 4, "entry offset must be inside"),
        (GlueRegion::Dram, 4, 4, 8, "entry offset zero"),
        (GlueRegion::Rodata, 4, 4, 8, "entry offset zero"),
        (GlueRegion::Bss, 4, 4, 8, "entry offset zero"),
    ] {
        let raw = Glue { name: "raw".into(), region, align, entry, bytes: vec![0; len], relocations: Vec::new() };
        let error = link(&[], &[], &raw_map, &[raw], &layout).expect_err("invalid in-memory glue refuses");
        assert_eq!(error.location.section, "raw", "entry error identifies the glue object");
        assert!(error.message.contains(reason), "the linker validates caller-supplied glue: {error}");
    }
    for (bytes, relocations, reason) in [
        (vec![0, 0, 0, 1], vec![], "only zero bytes"),
        (vec![0; 4], vec![GlueRelocation { offset: 0, symbol: "missing".into() }], "cannot have relocations"),
    ] {
        let raw = Glue { name: "raw".into(), region: GlueRegion::Bss, align: 4, entry: 0, bytes, relocations };
        let error = link(&[], &[], &raw_map, &[raw], &layout).expect_err("invalid in-memory BSS glue refuses");
        assert_eq!(error.location.section, "raw");
        assert!(error.message.contains(reason), "{error}");
    }
    for (fault, root, diagnostic) in [
        ("literal", "start", "literal sections are empty in the census; this pack needs a linker change"),
        ("data-root", "data_root", "root data_root must resolve to Text or Iram"),
        ("callee", "start", "audit failed"),
    ] {
        let pack = support::pack(fault);
        let archives: Vec<_> = pack.archives.iter().map(|(name, data)| Archive { name, data }).collect();
        let error = link(&archives, &[root.into()], &map, &glue, &layout).expect_err("planted link fault refuses").to_string();
        assert!(error.contains(diagnostic), "{fault} refusal is named: {error}");
        if fault == "literal" { assert!(error.contains(".literal.nonempty"), "literal refusal names the section: {error}"); }
        if fault == "callee" { assert!(error.contains("ASM_EXPAND literal names another callee") && error.contains(".text.start+0x23"), "failed audit returns its original located failure: {error}"); }
    }
    let pc = 0x4200_0000 + layout.mmu_page * 0x1_0000 + (Header::SIZE + Segment::SIZE + 4).next_multiple_of(Header::SECTOR);
    for (name, bits, target, diagnostic) in [
        ("narrow branch", 0x02cc, pc + 68, "out of range"),
        ("negative narrow branch", 0x028c, pc + 3, "out of range"),
        ("misaligned call", 0x25, pc + 5, "misaligned"),
        ("aligned CALL8 beyond reach", 0x25, pc + 4 + 524288, "out of range"),
        ("misaligned literal", 0xffff91, pc - 1, "misaligned"),
        ("forward literal", 0xffff91, pc + 4, "out of range"),
        ("loop range", 0x8276, pc + 260, "out of range"),
        ("reserved minus three", 0x216, pc + 1, "reserved conditional branch"),
        ("reserved minus two", 0x216, pc + 2, "reserved conditional branch"),
        ("reserved minus one", 0x216, pc + 3, "reserved conditional branch"),
        ("reserved zero", 0x216, pc + 4, "reserved conditional branch"),
    ] {
        let bytes = support::slot_pack(bits, target, 0);
        let error = link(&[Archive { name: "slots.a", data: &bytes }], &["slot_root".into()], &ImportMap::default(), &[], &layout)
            .expect_err("bad operand refuses").to_string();
        assert!(error.contains(diagnostic) && error.contains("slots.a(source.o):.text.slot+0x0") && error.contains("slot_target") && error.contains(&format!("{target:#010x}")), "{name} refusal names site, symbol, target and cause: {error}");
    }
    for (text, line, diagnostic) in [
        ("# comment\nglue x dram 3 4 zero", 2, "alignment"),
        ("glue x rodata 4 hex 123", 1, "hexadecimal"),
        ("glue x rodata 4 hex zz", 1, "hexadecimal"),
        ("glue x elsewhere 4 4 zero", 1, "dram or rodata"),
        ("glue x dram 4 4 zero\nglue x dram 4 4 zero", 2, "duplicate glue x"),
    ] {
        let error = parse_glue(text).expect_err("invalid glue line refuses");
        assert_eq!(error.line, line, "glue parser locates {diagnostic}");
        assert!(error.message.contains(diagnostic), "glue parser explains {diagnostic}: {error}");
    }
}
