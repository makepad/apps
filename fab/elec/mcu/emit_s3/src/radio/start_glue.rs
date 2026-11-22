// Sequence ported from ESP-IDF v6.1.1 components/esp_wifi/src/wifi_init.c (see fab/CREDITS.md).
// SPDX-FileCopyrightText: 2015-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
//! ESP-IDF v6.1.1 components/esp_wifi/src/wifi_init.c:460-512. island_start
//! powers the domain before this task calls esp_wifi_init_internal. On S3 the
//! PHY modem retention allocation is our static 84-byte phy_dig_regs. The
//! supplicant registration belongs to R12. None of the four MD5 check roots
//! is called by wifi_init.c:372-554. Its sleep setters at :387-406 are not
//! station roots and power save is off.

use super::adapter::CRYPTO_FIELDS;
use fab_elec_mcu_emit32::xtensa::{assemble_relaxed, ccz, reg::*, Asm};
use fab_elec_mcu_island::{Glue, GlueRegion, GlueRelocation};

pub const START_GATES: [&str; 2] = ["radio_start_failed", "osi_task_delay"];
pub const CONFIG_WORDS: usize = 40;

/// Xtensa layout from ESP-IDF v6.1.1 esp_wifi.h:97-127,359-389. Byte 0 is
/// our OSI pointer. At 4, esp_wifi_crypto_types.h:24,395-409 gives size 52,
/// version 1 and eleven function pointers at 12..56, in declaration order.
/// The remaining byte offsets follow WIFI_INIT_CONFIG_DEFAULT in esp_wifi.h:359-389.
/// Kconfig line numbers below refer to IDF's components/esp_wifi/Kconfig.
///
/// | Byte | Field and selected value | Source |
/// |---|---|---|
/// | 56, 60 | static RX 10, dynamic RX 32 | Kconfig:29,45, no Wi-Fi PSRAM |
/// | 64, 68, 72 | dynamic TX (1), static count 0, dynamic count 32 | Kconfig:62,87,91,121 and esp_wifi.h:129-133 |
/// | 76, 80, 84 | static management RX (0), count 5, TX cache 0 | Kconfig:133,152,158,106 and esp_wifi.h:135-139 |
/// | 88, 92, 96, 100 | CSI 0, AMPDU RX/TX 1/1, AMSDU TX 0 | Kconfig:168,195,176,216-217, no cache |
/// | 104, 108 | NVS 0, nano 0 | Our NVS rows return not found (IDF default on at Kconfig:223). Our log writer discards output. |
/// | 112, 116, 120, 124 | BA window 6, core 0, beacon max 752, management buffers 32 | Kconfig:204,230,243,266 |
/// | 128 | feature_caps 0 (aligned u64) | esp_wifi.h:333-345. No supplicant or SoftAP (Kconfig:302,334,351,451,902). FTM, GCMP, 11R and passive hidden default off (:420,444,787,934), S3 has no HE and WNM is off (:391). |
/// | 136 | disconnected PM false, three padding bytes | Our power save is off (IDF default on at Kconfig:437). |
/// | 140, 144 | ESP-NOW encrypted peers 7, HE trigger queue 1 | Kconfig:587,823 and esp_wifi.h:272-281, S3 has no HE |
/// | 148, 149, 150 | HE SIG-B dump 0, privacy 0, MAC reset interval 0, then one padding byte | Kconfig:832,941,949 and esp_wifi.h:272-281,347-357 |
/// | 152, 156 | Wi-Fi task stack 3584, magic 0x1f2f3f4f | esp_wifi.h:309-317,192. Base 3072 without SAE/OWE plus 512 for non-nano formatting. |
pub fn config(osi: u32, crypto: &[u32; CRYPTO_FIELDS.len()]) -> [u32; CONFIG_WORDS] {
    let mut words = [0; CONFIG_WORDS];
    words[..3].copy_from_slice(&[osi, 52, 1]);
    words[3..14].copy_from_slice(crypto);
    words[14..].copy_from_slice(&[
        10, 32, 1, 0, 32, 0, 5, 0, // RX, TX and management buffers
        0, 1, 1, 0, 0, 0,           // CSI, aggregation, NVS and nano
        6, 0, 752, 32,               // BA window, core, beacon and management buffers
        0, 0, 0, 7, 1, 0,            // feature_caps, PM, ESP-NOW, HE and packed byte fields
        3584, 0x1f2f_3f4f,
    ]);
    words
}

/// The task itself executes 5 instructions through the first CALLX8, 5 more
/// through set_mode, 4 through start and 5 through the first delay (19 total).
/// A failing root takes 7 from its return through the failure gate call.
/// All of these are in the task context, outside the kernel mask.
pub fn radio_start() -> Glue {
    // Gate literals are first, in import-map order, followed by station roots.
    let targets = [START_GATES[0], START_GATES[1], "esp_wifi_init_internal", "esp_wifi_set_mode", "esp_wifi_start"];
    let entry = targets.len() as u32 * 4;
    let (failed, park) = (0, 1);
    let bytes = assemble_relaxed(&mut Asm::new(2, A8), |a| {
        let call = |a: &mut Asm, index| { a.op_l32r(A8, a.base.offset(), index * 4); a.callx8(A8); };
        a.base.reserve(entry);
        a.entry(A1, 32);
        a.mov_n(A10, A2);
        for step in 1..=3 {
            // a2 survives CALLX8, including a window spill. The result returns in a10.
            a.movi(A2, step);
            if step == 2 { a.movi(A10, 1); } // WIFI_MODE_STA, esp_wifi_types_generic.h:27-28.
            call(a, step as u32 + 1); // esp_wifi.h:438,467: set mode, then start.
            a.bccz_reg_label(ccz::NE, A10, failed);
        }
        a.j_label(park);
        a.base.label_assign(failed);
        a.slli(A10, A10, 8); a.srli(A10, A10, 8);
        a.slli(A2, A2, 24); a.or(A10, A10, A2);
        call(a, 0);
        a.base.label_assign(park);
        a.movi(A10, -1); // wifi_os_adapter.h:23, OSI_FUNCS_TIME_BLOCKING.
        call(a, 1); a.j_label(park);
    });
    Glue { name: "radio_start".into(), region: GlueRegion::Text, align: 4, entry, bytes,
        relocations: targets.iter().enumerate().map(|(index, &symbol)| GlueRelocation {
            offset: index as u32 * 4, symbol: symbol.into(),
        }).collect() }
}
