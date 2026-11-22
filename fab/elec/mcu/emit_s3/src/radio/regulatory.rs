// Ported from ESP-IDF v6.1.1 components/esp_wifi/regulatory/esp_wifi_regulatory.c,
// SPDX-FileCopyrightText: 2025-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
// Modified: transcribed the non-5G tables to Rust and encoded them as island glue.

//! ESP32-S3 regulatory tables. ESP-IDF v6.1.1 components/esp_wifi/include/esp_wifi_types_generic.h
//! defines the following little-endian Xtensa GCC layouts when `CONFIG_SOC_WIFI_SUPPORT_5G` is unset.
//!
//! - `wifi_regdomain_t` occupies 3 bytes, alignment 1. Bytes 0 and 1 hold the two country
//!   characters without a terminator. Byte 2 holds the profile index as a `uint8_t`.
//! - `wifi_reg_rule_t` occupies 4 bytes, alignment 2. Bytes 0 and 1 hold start_channel and
//!   end_channel. The little-endian `uint16_t` at offset 2 holds max_bandwidth in bits 0–2,
//!   max_eirp in bits 3–8, is_dfs in bit 9, and reserved in bits 10–15.
//! - `wifi_regulatory_t` occupies 10 bytes, alignment 2. Byte 0 holds n_reg_rules, byte 1
//!   is padding, and two rule slots start at offsets 2 and 6. Unused slots are zero.
//!
//! All three array strides equal the sizes above. The 4-byte C enum supplies the numeric
//! constants assigned to the single-byte `regulatory_type` field.

use fab_elec_mcu_island::{Glue, GlueRegion};

// esp_wifi_regulatory_type_t in declaration order.
const DEFAULT: u8 = 0;
const CE: u8 = 1;
const ACMA: u8 = 2;
const ANATEL: u8 = 3;
const ISED: u8 = 4;
const SRRC: u8 = 5;
const OFCA: u8 = 6;
const WPC: u8 = 7;
const MIC: u8 = 8;
const KCC: u8 = 9;
const IFETEL: u8 = 10;
const RCM: u8 = 11;
const NCC: u8 = 12;
const FCC: u8 = 13;
const GT: u8 = 14;
const KE: u8 = 15;
const KP: u8 = 16;
const MY: u8 = 17;
const PK: u8 = 18;
const TG: u8 = 19;
const MAX: u8 = 20;

const REGDOMAIN_TABLE: &[(&[u8; 2], u8)] = &[
    (b"01", DEFAULT),
    (b"EU", IFETEL),
    (b"AD", IFETEL),
    (b"AE", IFETEL),
    (b"AF", IFETEL),
    (b"AI", IFETEL),
    (b"AL", IFETEL),
    (b"AM", IFETEL),
    (b"AN", IFETEL),
    (b"AR", IFETEL),
    (b"AS", FCC),
    (b"AT", IFETEL),
    (b"AU", RCM),
    (b"AW", IFETEL),
    (b"AZ", IFETEL),
    (b"BA", IFETEL),
    (b"BB", IFETEL),
    (b"BD", IFETEL),
    (b"BE", IFETEL),
    (b"BF", IFETEL),
    (b"BG", IFETEL),
    (b"BH", IFETEL),
    (b"BL", IFETEL),
    (b"BM", FCC),
    (b"BN", IFETEL),
    (b"BO", IFETEL),
    (b"BR", NCC),
    (b"BS", IFETEL),
    (b"BT", IFETEL),
    (b"BW", IFETEL),
    (b"BY", IFETEL),
    (b"BZ", NCC),
    (b"CA", RCM),
    (b"CF", IFETEL),
    (b"CH", IFETEL),
    (b"CI", IFETEL),
    (b"CL", IFETEL),
    (b"CN", IFETEL),
    (b"CO", IFETEL),
    (b"CR", RCM),
    (b"CU", KCC),
    (b"CX", IFETEL),
    (b"CY", IFETEL),
    (b"CZ", IFETEL),
    (b"DE", IFETEL),
    (b"DK", IFETEL),
    (b"DM", FCC),
    (b"DO", FCC),
    (b"DZ", IFETEL),
    (b"EC", NCC),
    (b"EE", IFETEL),
    (b"EG", IFETEL),
    (b"ES", IFETEL),
    (b"ET", IFETEL),
    (b"FI", IFETEL),
    (b"FM", FCC),
    (b"FO", IFETEL),
    (b"FR", IFETEL),
    (b"GB", IFETEL),
    (b"GD", FCC),
    (b"GE", IFETEL),
    (b"GF", IFETEL),
    (b"GH", IFETEL),
    (b"GI", IFETEL),
    (b"GL", IFETEL),
    (b"GP", IFETEL),
    (b"GR", IFETEL),
    (b"GT", GT),
    (b"GU", FCC),
    (b"GY", NCC),
    (b"HK", RCM),
    (b"HN", IFETEL),
    (b"HR", IFETEL),
    (b"HT", FCC),
    (b"HU", IFETEL),
    (b"ID", GT),
    (b"IE", IFETEL),
    (b"IL", IFETEL),
    (b"IM", IFETEL),
    (b"IN", NCC),
    (b"IR", IFETEL),
    (b"IS", IFETEL),
    (b"IT", IFETEL),
    (b"JM", IFETEL),
    (b"JO", IFETEL),
    (b"JP", MIC),
    (b"KE", KE),
    (b"KH", IFETEL),
    (b"KN", IFETEL),
    (b"KP", KP),
    (b"KR", KCC),
    (b"KW", IFETEL),
    (b"KY", NCC),
    (b"KZ", IFETEL),
    (b"LB", IFETEL),
    (b"LC", IFETEL),
    (b"LI", IFETEL),
    (b"LK", IFETEL),
    (b"LS", IFETEL),
    (b"LT", IFETEL),
    (b"LU", IFETEL),
    (b"LV", IFETEL),
    (b"MA", IFETEL),
    (b"MC", IFETEL),
    (b"MD", IFETEL),
    (b"ME", IFETEL),
    (b"MF", IFETEL),
    (b"MH", FCC),
    (b"MK", IFETEL),
    (b"MN", IFETEL),
    (b"MO", KCC),
    (b"MP", FCC),
    (b"MQ", IFETEL),
    (b"MR", IFETEL),
    (b"MT", IFETEL),
    (b"MU", IFETEL),
    (b"MV", IFETEL),
    (b"MW", IFETEL),
    (b"MX", IFETEL),
    (b"MY", MY),
    (b"NA", IFETEL),
    (b"NG", IFETEL),
    (b"NI", FCC),
    (b"NL", IFETEL),
    (b"NO", IFETEL),
    (b"NP", IFETEL),
    (b"NZ", RCM),
    (b"OM", IFETEL),
    (b"PA", RCM),
    (b"PE", IFETEL),
    (b"PF", IFETEL),
    (b"PG", IFETEL),
    (b"PH", IFETEL),
    (b"PK", PK),
    (b"PL", IFETEL),
    (b"PM", IFETEL),
    (b"PR", FCC),
    (b"PT", IFETEL),
    (b"PW", FCC),
    (b"PY", IFETEL),
    (b"QA", IFETEL),
    (b"RE", IFETEL),
    (b"RO", IFETEL),
    (b"RS", IFETEL),
    (b"RU", IFETEL),
    (b"RW", IFETEL),
    (b"SA", IFETEL),
    (b"SE", IFETEL),
    (b"SG", KCC),
    (b"SI", IFETEL),
    (b"SK", IFETEL),
    (b"SM", IFETEL),
    (b"SN", IFETEL),
    (b"SR", IFETEL),
    (b"SV", IFETEL),
    (b"SX", IFETEL),
    (b"SY", IFETEL),
    (b"TC", IFETEL),
    (b"TD", IFETEL),
    (b"TG", TG),
    (b"TH", IFETEL),
    (b"TN", IFETEL),
    (b"TR", IFETEL),
    (b"TT", IFETEL),
    (b"TW", NCC),
    (b"TZ", IFETEL),
    (b"UA", IFETEL),
    (b"UG", IFETEL),
    (b"US", FCC),
    (b"UY", IFETEL),
    (b"UZ", IFETEL),
    (b"VA", IFETEL),
    (b"VC", IFETEL),
    (b"VE", NCC),
    (b"VI", FCC),
    (b"VN", KCC),
    (b"VU", IFETEL),
    (b"WF", IFETEL),
    (b"WS", IFETEL),
    (b"YE", IFETEL),
    (b"YT", IFETEL),
    (b"ZA", IFETEL),
    (b"ZW", IFETEL),
    (b"##", MAX),
];

// Each rule lists start_channel, end_channel, max_bandwidth, max_eirp, is_dfs and reserved.
type Rule = [u8; 6];
const REGULATORY_DATA: [(u8, &[Rule]); MAX as usize] = [
    (DEFAULT, &[[1, 11, 2, 20, 0, 0]]),
    (CE, &[[1, 13, 2, 20, 0, 0]]),
    (ACMA, &[[1, 13, 2, 36, 0, 0]]),
    (ANATEL, &[[1, 13, 2, 30, 0, 0]]),
    (ISED, &[[1, 13, 2, 36, 0, 0]]),
    (SRRC, &[[1, 13, 2, 20, 0, 0]]),
    (OFCA, &[[1, 13, 2, 36, 0, 0]]),
    (WPC, &[[1, 13, 2, 30, 0, 0]]),
    (MIC, &[
        [1, 13, 2, 20, 0, 0],
        [14, 14, 1, 20, 0, 0],
    ]),
    (KCC, &[[1, 13, 2, 23, 0, 0]]),
    (IFETEL, &[[1, 13, 2, 20, 0, 0]]),
    (RCM, &[[1, 13, 2, 36, 0, 0]]),
    (NCC, &[[1, 13, 2, 30, 0, 0]]),
    (FCC, &[[1, 11, 2, 30, 0, 0]]),
    (GT, &[[1, 13, 2, 26, 0, 0]]),
    (KE, &[[1, 13, 2, 33, 0, 0]]),
    (KP, &[[1, 13, 1, 20, 0, 0]]),
    (MY, &[[1, 14, 2, 26, 0, 0]]),
    (PK, &[[1, 14, 2, 30, 0, 0]]),
    (TG, &[[1, 13, 2, 20, 1, 0]]),
];

/// Encode both tables at their C array strides, with a word-aligned base for each symbol.
pub fn glue() -> Vec<Glue> {
    let mut domains = Vec::with_capacity(REGDOMAIN_TABLE.len() * 3);
    for &(country, profile) in REGDOMAIN_TABLE {
        domains.extend_from_slice(country);
        domains.push(profile);
    }

    const PROFILE_BYTES: usize = 10;
    let mut data = vec![0; REGULATORY_DATA.len() * PROFILE_BYTES];
    for &(profile, rules) in &REGULATORY_DATA {
        let start = usize::from(profile) * PROFILE_BYTES;
        let row = &mut data[start..start + PROFILE_BYTES];
        row[0] = rules.len() as u8;
        for (index, &[first, last, bandwidth, eirp, dfs, reserved]) in rules.iter().enumerate() {
            let flags = u16::from(bandwidth) | (u16::from(eirp) << 3)
                | (u16::from(dfs) << 9) | (u16::from(reserved) << 10);
            let [low, high] = flags.to_le_bytes();
            let offset = 2 + index * 4;
            row[offset..offset + 4].copy_from_slice(&[first, last, low, high]);
        }
    }

    let rodata = |name: &str, bytes| Glue {
        name: name.into(), region: GlueRegion::Rodata, align: 4, entry: 0, bytes, relocations: Vec::new(),
    };
    vec![rodata("regdomain_table", domains), rodata("regulatory_data", data)]
}
