//! Link the ESP32-S3 station pack for the voicebox.

use super::{event_glue, ftm_words, helpers, phy_glue, regulatory, stand_in, start_glue};
use crate::{ISLAND_MMU_PAGE, SRAM_START, STACK_TOP, VOICEBOX};
use fab_elec_mcu_island::{import_map::parse_roots, link, Archive, Error, Glue, GlueRegion,
    GlueRelocation, ImportMap, Island, IslandLayout, Location, Region};

fn data(name: &str, region: GlueRegion, bytes: Vec<u8>) -> Glue {
    Glue { name: name.into(), region, align: 4, entry: 0, bytes, relocations: Vec::new() }
}

/// Windowed code and data supplied by Fab for the station pack's imports.
pub fn island_glue() -> Vec<Glue> {
    let mut glue = helpers::helpers();
    glue.extend(phy_glue::phy_glue().objects);
    glue.push(event_glue::event_post_glue());
    glue.push(start_glue::radio_start());
    glue.push(data(stand_in::STATE_OBJECT, GlueRegion::Bss, vec![0; stand_in::STATE_BYTES]));
    glue.extend(ftm_words::glue());
    glue.push(data("wifi_event_base_name", GlueRegion::Rodata, b"WIFI_EVENT\0".to_vec()));
    let mut event = data("WIFI_EVENT", GlueRegion::Rodata, vec![0; 4]);
    event.relocations.push(GlueRelocation { offset: 0, symbol: "wifi_event_base_name".into() });
    glue.push(event);
    glue.extend(regulatory::glue());
    glue
}

/// Link the tracked station roots and imports into the voicebox's reserved flash and SRAM.
pub fn link_pack(archives: &[Archive<'_>]) -> Result<Island, Error> {
    let map = ImportMap::parse(include_str!("../../../island/packs/esp32s3-idf-v6.1.1.imports"))
        .map_err(|e| Location::new("station", "manifest", "imports").error(e.to_string()))?;
    let roots = parse_roots(include_str!("../../../island/packs/esp32s3-idf-v6.1.1-sta.roots"))
        .map_err(|e| Location::new("station", "manifest", "roots").error(e.to_string()))?;
    let layout = IslandLayout {
        mmu_page: ISLAND_MMU_PAGE,
        flash_bytes: VOICEBOX.island_pages * 65536,
        window: Region { start: SRAM_START, bytes: STACK_TOP - SRAM_START },
        window_exec: 0x4037_8000,
    };
    link(archives, &roots, &map, &island_glue(), &layout)
}
