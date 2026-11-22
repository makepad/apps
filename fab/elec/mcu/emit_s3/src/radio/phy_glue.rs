// Sequencing ported from ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c and phy_common.c and
// components/esp_wifi/esp32s3/esp_adapter.c (see fab/CREDITS.md).
// SPDX-FileCopyrightText: 2015-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
//! Windowed PHY call sequences; the kernel owns all state and decisions.
//!
//! Ported sequencing from ESP-IDF v6.1.1 (Apache-2.0),
//! components/esp_phy/src/phy_init.c, components/esp_phy/src/phy_common.c and
//! components/esp_wifi/esp32s3/esp_adapter.c (short names below).
//! Every target is an L32R literal relocated by the island linker, then CALLX8.
//! The pack enters past those literals through CALL8/CALLX8, with no arguments.
//!
//! ENTRY allocates 64 bytes. With post-entry SP = S and caller SP = S + 64,
//! emit_s3/src/radio/vectors.rs:90-116 (overflow8/underflow8) places the base
//! save area below S and the CALL8 extra save area at caller SP - 32..-16,
//! i.e. S + 32..48. The out block at S..S + 16 overlaps neither. Even the
//! CALL12 extra area (vectors.rs:119-134) starts at S + 16. a2 keeps the step
//! across callees, including window spills; the out block stays in this frame.
//!
//! Omitted IDF steps: phy_track_pll_init/deinit belong to R4d's timer task;
//! antenna updates depend on configuration we never perform (phy_common.c:
//! 209-219,353); brownout TX-power reduction and IMPROVE_RX_11B default off
//! (components/esp_phy/Kconfig:118-123,172-179). Deep-sleep integration and
//! the version log are outside this glue's sequence, not disabled defaults.
//! The hook is guarded by ESP_PHY_ENABLED (Kconfig:3-5 defaults on for PHY)
//! and SOC_DEEP_SLEEP_SUPPORTED (components/soc/esp32s3/include/soc/soc_caps.h:96
//! is 1), at phy_init.c:987-989; this glue installs no deep-sleep hook.
//! phy_init.c:899-900 logs the version unconditionally; logging is omitted here.
//! Unknown steps take the enable ON or disable NOT_LAST path, never
//! calibration, wake, backup or close.

use fab_elec_mcu_emit32::xtensa::{assemble_relaxed, cc, ccz, reg::*, Asm};
use fab_elec_mcu_island::{Glue, GlueRegion, GlueRelocation};
use super::kernel::{PHY_FIRST, PHY_LAST, PHY_OP_DISABLE, PHY_OP_ENABLE, PHY_WAKE, PHY_WAKE_RESTORE};

/// Import-map order, shared with the image builder.
pub const PHY_GATES: [&str; 4] = ["phy_lock", "phy_enter", "phy_init_done", "phy_leave"];
pub const PHY_PACK_SYMBOLS: [&str; 10] = [
    "phy_init_param_set", "phy_bbpll_en_usb", "register_chipv7_phy", "phy_wakeup_init",
    "phy_dig_reg_backup", "phy_param_track_tot", "phy_wifi_enable_set", "phy_close_rf",
    "phy_xpd_tsens", "phy_wait_freq_hw_hop_done",
];

pub struct PhyGlue {
    /// Enable, then disable; each starts with its relocated literal words.
    pub objects: Vec<Glue>,
    pub gates: &'static [&'static str],
    pub pack_symbols: &'static [&'static str],
}

const FRAME_BYTES: u32 = 64;
const OUT: u32 = 0;
const ENABLE_TARGETS: [&str; 11] = [
    "phy_lock", "phy_enter", "phy_init_done", "phy_leave", "phy_init_param_set",
    "phy_bbpll_en_usb", "register_chipv7_phy", "phy_wakeup_init", "phy_dig_reg_backup",
    "phy_param_track_tot", "phy_wifi_enable_set",
];
const DISABLE_TARGETS: [&str; 8] = [
    "phy_lock", "phy_enter", "phy_leave", "phy_wifi_enable_set", "phy_dig_reg_backup",
    "phy_close_rf", "phy_xpd_tsens", "phy_wait_freq_hw_hop_done",
];

fn call(a: &mut Asm, targets: &[&str], name: &str) {
    let literal = targets.iter().position(|&target| target == name).expect("PHY call target") as u32 * 4;
    a.op_l32r(A8, a.base.offset(), literal);
    a.callx8(A8);
}

fn object(name: &str, targets: &[&str], labels: usize, body: impl Fn(&mut Asm)) -> Glue {
    let entry = targets.len() as u32 * 4;
    let bytes = assemble_relaxed(&mut Asm::new(labels, A8), |a| {
        a.base.reserve(entry);
        body(a);
    });
    Glue {
        name: name.into(), region: GlueRegion::Text, align: 4, entry, bytes,
        relocations: targets.iter().enumerate().map(|(i, &symbol)| GlueRelocation {
            offset: i as u32 * 4, symbol: symbol.into(),
        }).collect(),
    }
}

pub fn phy_glue() -> PhyGlue {
    let enable = object("phy_enable_glue", &ENABLE_TARGETS, 6, |a| {
        let (first, wake, wake_done, done, leave, ret) = (0, 1, 2, 3, 4, 5);
        a.entry(A1, FRAME_BYTES);
        call(a, &ENABLE_TARGETS, "phy_lock"); // phy_init.c:284: PHY mutex.
        a.movi(A10, PHY_OP_ENABLE as i32);
        a.addi(A11, A1, OUT as i32);
        call(a, &ENABLE_TARGETS, "phy_enter"); // phy_init.c:285-300: state and clocks before libphy.
        a.mov_n(A2, A10);
        a.bccz_reg_label(ccz::EQ, A2, ret); // PHY_SKIP = 0: no lock held.
        a.bcci_reg_label(cc::EQ, A2, PHY_FIRST as i32, first);
        a.bcci_reg_label(cc::EQ, A2, PHY_WAKE as i32, wake);
        a.bcci_reg_label(cc::EQ, A2, PHY_WAKE_RESTORE as i32, wake);
        a.movi(A11, 0); // ON, or an unknown step.
        a.j_label(done);

        a.base.label_assign(first);
        a.movi(A10, 1);
        call(a, &ENABLE_TARGETS, "phy_init_param_set"); // phy_init.c:908-910: S3 combo module.
        a.movi(A10, 1);
        call(a, &ENABLE_TARGETS, "phy_bbpll_en_usb"); // phy_init.c:944-946: S3 USB enabled by default.
        a.l32i(A10, A1, OUT);
        a.l32i(A11, A1, OUT + 4);
        a.l32i(A12, A1, OUT + 8);
        call(a, &ENABLE_TARGETS, "register_chipv7_phy"); // phy_init.c:962.
        a.mov_n(A11, A10);
        a.j_label(done);

        a.base.label_assign(wake);
        call(a, &ENABLE_TARGETS, "phy_wakeup_init"); // phy_init.c:336: S3 has no REGDMA PHY trigger.
        a.bcci_reg_label(cc::EQ, A2, PHY_WAKE as i32, wake_done);
        a.movi(A10, 0);
        a.l32i(A11, A1, OUT + 12);
        call(a, &ENABLE_TARGETS, "phy_dig_reg_backup"); // phy_init.c:262-266,339-340: restore.
        a.base.label_assign(wake_done);
        a.movi(A11, 0);

        a.base.label_assign(done);
        a.mov_n(A10, A2);
        call(a, &ENABLE_TARGETS, "phy_init_done"); // phy_init.c:300,361-366: calibration, clocks and period.
        a.bccz_reg_label(ccz::EQ, A10, leave);
        a.movi(A10, 1);
        a.movi(A11, 0);
        call(a, &ENABLE_TARGETS, "phy_param_track_tot"); // phy_init.c:366; phy_common.c:83-122: Wi-Fi only.
        a.base.label_assign(leave);
        a.movi(A10, PHY_OP_ENABLE as i32);
        a.mov_n(A11, A2);
        call(a, &ENABLE_TARGETS, "phy_leave"); // phy_init.c:372: unlock.
        a.movi(A10, 1);
        call(a, &ENABLE_TARGETS, "phy_wifi_enable_set"); // esp_adapter.c:626: after esp_phy_enable.
        a.base.label_assign(ret);
        a.retw_n();
    });
    let disable = object("phy_disable_glue", &DISABLE_TARGETS, 3, |a| {
        let (last, leave, ret) = (0, 1, 2);
        a.entry(A1, FRAME_BYTES);
        a.movi(A10, 0);
        call(a, &DISABLE_TARGETS, "phy_wifi_enable_set"); // esp_adapter.c:631: before esp_phy_disable.
        call(a, &DISABLE_TARGETS, "phy_lock"); // phy_init.c:377: PHY mutex.
        a.movi(A10, PHY_OP_DISABLE as i32);
        a.addi(A11, A1, OUT as i32);
        call(a, &DISABLE_TARGETS, "phy_enter"); // phy_init.c:381-383: clear modem flag and choose last owner.
        a.mov_n(A2, A10);
        a.bccz_reg_label(ccz::EQ, A2, ret);
        a.bcci_reg_label(cc::EQ, A2, PHY_LAST as i32, last);
        a.j_label(leave); // NOT_LAST, or an unknown step.
        a.base.label_assign(last);
        a.movi(A10, 1);
        a.l32i(A11, A1, OUT + 12);
        call(a, &DISABLE_TARGETS, "phy_dig_reg_backup"); // phy_init.c:254-260,388-389: save.
        call(a, &DISABLE_TARGETS, "phy_close_rf"); // phy_init.c:409-410.
        call(a, &DISABLE_TARGETS, "phy_xpd_tsens"); // phy_init.c:411-414.
        call(a, &DISABLE_TARGETS, "phy_wait_freq_hw_hop_done"); // phy_init.c:420.
        a.base.label_assign(leave);
        a.movi(A10, PHY_OP_DISABLE as i32);
        a.mov_n(A11, A2);
        call(a, &DISABLE_TARGETS, "phy_leave"); // phy_init.c:421-424: clocks and unlock.
        a.base.label_assign(ret);
        a.retw_n();
    });
    PhyGlue { objects: vec![enable, disable], gates: &PHY_GATES, pack_symbols: &PHY_PACK_SYMBOLS }
}
