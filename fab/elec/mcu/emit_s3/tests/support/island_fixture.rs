use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emit_s3::{radio::{event_glue::{event_post_glue, EVENT_GATES},
    phy_glue::{phy_glue, PHY_PACK_SYMBOLS}, start_glue::{radio_start, START_GATES}}, ISLAND_MMU_PAGE, SRAM_START, STACK_TOP, VOICEBOX};
use fab_elec_mcu_island::{import_map::parse_roots, link, Archive, ImportMap, Island, IslandLayout, Region};
use fab_elec_mcu_s3::{S3Machine, S3Profile};
use std::sync::{Arc, OnceLock};

#[allow(dead_code)]
#[path = "../../../island/tests/support/mod.rs"]
mod elf;

pub fn small_island() -> &'static Island {
    static ISLAND: OnceLock<Island> = OnceLock::new();
    ISLAND.get_or_init(|| linked_island(vec![1, 3, 5], 17))
}

/// The machine's flash is immutable from handoff. A short backing image reads as erased NOR.
pub fn machine_with_island(slot_flash: &[u8], island_image: &[u8], profile: &S3Profile) -> S3Machine {
    let flash = fab_elec_mcu_emit_s3::firmware::flash_with_island(slot_flash, island_image);
    S3Machine::new(Arc::new(flash), profile)
}

pub fn station_archive(initial_data: Vec<u8>, bss_bytes: u32) -> (Vec<String>, Vec<u8>) {
    station_archive_with_virtual_roots(initial_data, bss_bytes, &[])
}

/// Reserve 16-byte FUNCs for the requested stand-ins, including any non-root callbacks.
/// Passing all the stand-in row names keeps them together in the fixture's single IRAM section.
pub fn station_archive_with_virtual_roots(initial_data: Vec<u8>, bss_bytes: u32, virtual_roots: &[&str]) -> (Vec<String>, Vec<u8>) {
    let roots = parse_roots(include_str!("../../../island/packs/esp32s3-idf-v6.1.1-sta.roots")).unwrap();
    let mut names = roots.clone();
    for symbol in PHY_PACK_SYMBOLS.into_iter().chain(virtual_roots.iter().copied()) {
        if !names.iter().any(|name| name == symbol) { names.push(symbol.into()); }
    }

    let mut object = elf::Object::new();
    let mut body = vec![0; 8];
    let mut entries = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let offset = body.len() as u32;
        elf::emit(&mut body, elf::entry());
        elf::emit(&mut body, enc::movi(2, index as i32));
        elf::emit(&mut body, elf::retwn());
        if virtual_roots.contains(&name.as_str()) {
            for _ in 0..4 { elf::emit(&mut body, elf::retwn()); }
        }
        entries.push((name, offset, body.len() as u32 - offset));
    }
    elf::emit(&mut body, elf::retwn()); // A two-byte IRAM tail after the last complete entry.
    let size = body.len() as u32;
    let iram = object.section(".iram1.fixture", 1, 6, body);
    for (name, offset, len) in entries { object.symbol(name, 1, 2, iram as u16, offset, len); }
    object.property(iram, 0, 8, 1);
    object.property(iram, 8, size - 8, 2);
    let data = object.section(".data.fixture", 1, 3, initial_data);
    let bss = object.section(".bss.fixture", 8, 3, Vec::new());
    object.sections[bss].size = bss_bytes;
    object.symbol(fab_elec_mcu_emit_s3::radio::PHY_FUNS_OBJECT, 1, 1, bss as u16, 0, 4);
    for (offset, section) in [(0, data), (4, bss)] {
        let symbol = object.section_symbol(section);
        object.relocation(iram, offset, 1, symbol, 0);
    }
    (roots, elf::archive(&[("fixture.o", object.encode())]))
}

pub fn linked_island(initial_data: Vec<u8>, bss_bytes: u32) -> Island {
    let (roots, archive) = station_archive(initial_data, bss_bytes);
    let phy = phy_glue();
    let mut imports = String::from("gate 0 free\n");
    for (i, name) in phy.gates.iter().chain(&EVENT_GATES).chain(&START_GATES).enumerate() { imports.push_str(&format!("gate {} {name}\n", i + 1)); }
    let mut objects = phy.objects;
    objects.push(event_post_glue());
    objects.push(radio_start());
    for glue in &objects { imports.push_str(&format!("glue {}\n", glue.name)); }
    link(&[Archive { name: "fixture.a", data: &archive }], &roots, &ImportMap::parse(&imports).unwrap(),
        &objects, &IslandLayout { mmu_page: ISLAND_MMU_PAGE, flash_bytes: VOICEBOX.island_pages * 65536,
            window: Region { start: SRAM_START, bytes: STACK_TOP - SRAM_START }, window_exec: 0x4037_8000 })
        .expect("link synthetic station entries, PHY, event and start glue, segments and indexed gates")
}
