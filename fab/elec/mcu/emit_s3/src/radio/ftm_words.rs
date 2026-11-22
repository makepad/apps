// Ported from ESP-IDF v6.1.1 components/esp_wifi/include/esp_private/ftm_calibration_data.h
// (the CONFIG_IDF_TARGET_ESP32S3 values) and components/esp_wifi/src/ftm_load_calibration.c
// (the symbol names),
// SPDX-FileCopyrightText: 2025-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-FileCopyrightText: 2026 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
// Modified: transcribed the ESP32-S3 values to Rust and encoded each word as island glue.

//! FTM compensation words the station pack imports: one 4-byte data object each.

use fab_elec_mcu_island::{Glue, GlueRegion};

const FTM_WORDS: [(&str, u32); 24] = [
    ("est_PHY_INIT_FTM_COMP_20_20D_MHZ", 453),
    ("est_PHY_INIT_FTM_COMP_20_20D_MHZ_DIS", 454),
    ("est_PHY_INIT_FTM_COMP_20_20U_MHZ", 453),
    ("est_PHY_INIT_FTM_COMP_20_20U_MHZ_DIS", 453),
    ("est_PHY_INIT_FTM_COMP_20_40D_MHZ", 270),
    ("est_PHY_INIT_FTM_COMP_20_40D_MHZ_DIS", 270),
    ("est_PHY_INIT_FTM_COMP_20_40U_MHZ", 271),
    ("est_PHY_INIT_FTM_COMP_20_40U_MHZ_DIS", 269),
    ("est_PHY_INIT_FTM_COMP_40_40D_MHZ", 443),
    ("est_PHY_INIT_FTM_COMP_40_40D_MHZ_DIS", 445),
    ("est_PHY_INIT_FTM_COMP_40_40U_MHZ", 443),
    ("est_PHY_INIT_FTM_COMP_40_40U_MHZ_DIS", 442),
    ("est_PHY_RESP_FTM_COMP_20_20D_MHZ", 440),
    ("est_PHY_RESP_FTM_COMP_20_20D_MHZ_DIS", 440),
    ("est_PHY_RESP_FTM_COMP_20_20U_MHZ", 444),
    ("est_PHY_RESP_FTM_COMP_20_20U_MHZ_DIS", 444),
    ("est_PHY_RESP_FTM_COMP_20_40D_MHZ", 261),
    ("est_PHY_RESP_FTM_COMP_20_40D_MHZ_DIS", 258),
    ("est_PHY_RESP_FTM_COMP_20_40U_MHZ", 262),
    ("est_PHY_RESP_FTM_COMP_20_40U_MHZ_DIS", 262),
    ("est_PHY_RESP_FTM_COMP_40_40D_MHZ", 84),
    ("est_PHY_RESP_FTM_COMP_40_40D_MHZ_DIS", 82),
    ("est_PHY_RESP_FTM_COMP_40_40U_MHZ", 88),
    ("est_PHY_RESP_FTM_COMP_40_40U_MHZ_DIS", 89),
];

/// The 24 words as island data glue, in the order above.
pub fn glue() -> Vec<Glue> {
    FTM_WORDS.iter().map(|&(name, value)| Glue {
        name: name.into(), region: GlueRegion::Dram, align: 4, entry: 0,
        bytes: value.to_le_bytes().to_vec(), relocations: Vec::new(),
    }).collect()
}
