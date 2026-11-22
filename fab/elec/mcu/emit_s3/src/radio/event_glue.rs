//! Windowed Wi-Fi event posting. IDF v6.1.1 esp_private/wifi_os_adapter.h:75
//! gives the ABI; esp_wifi_types_generic.h:1811-1816 describes esp_event_post's
//! owned heap copy, including variable-length DPP data. esp-radio
//! src/wifi/os_adapter/mod.rs:673-716 publishes without waiting for ticks.

use fab_elec_mcu_emit32::xtensa::{assemble_relaxed, cc, ccz, reg::*, Asm};
use fab_elec_mcu_island::{Glue, GlueRegion, GlueRelocation};

/// Relocated gate literals in station import-map order.
pub const EVENT_GATES: [&str; 2] = ["malloc", "event_commit"];

pub fn event_post_glue() -> Glue {
    let entry = EVENT_GATES.len() as u32 * 4;
    let (allocate, copy, commit, failed) = (0, 1, 2, 3);
    let bytes = assemble_relaxed(&mut Asm::new(4, A8), |a| {
        a.base.reserve(entry);
        a.entry(A1, 32);
        // a2..a5 (base, id, data, size) survive the CALLX8 window rotation.
        // Saturate an unrepresentable request so malloc reports heap exhaustion.
        a.addi(A10, A5, 16);
        a.bcc_reg_reg_label(cc::GEU, A10, A5, allocate);
        a.movi(A10, -1);
        a.base.label_assign(allocate);
        a.op_l32r(A8, a.base.offset(), 0); a.callx8(A8);
        a.bccz_reg_label(ccz::EQ, A10, failed);
        // Only this poster owns the node until commit. Another task deleting
        // the poster in this interval leaks that one node until radio restart.
        a.movi(A6, 0); a.s32i(A6, A10, 0); a.s32i(A2, A10, 4);
        a.s32i(A3, A10, 8); a.s32i(A5, A10, 12);
        a.bccz_reg_label(ccz::EQ, A5, commit); // NULL data is valid at size zero.
        a.addi(A7, A10, 16);
        a.base.label_assign(copy);
        a.l8ui(A6, A4, 0); a.s8i(A6, A7, 0);
        a.addi(A4, A4, 1); a.addi(A7, A7, 1); a.addi(A5, A5, -1);
        a.bccz_reg_label(ccz::NE, A5, copy);
        a.base.label_assign(commit);
        a.op_l32r(A8, a.base.offset(), 4); a.callx8(A8);
        a.movi(A2, 0); a.retw_n();
        a.base.label_assign(failed);
        a.movi(A2, -1); a.retw_n();
    });
    Glue { name: "event_post_glue".into(), region: GlueRegion::Text, align: 4, entry, bytes,
        relocations: EVENT_GATES.iter().enumerate().map(|(i, &name)| GlueRelocation {
            offset: i as u32 * 4, symbol: name.into(),
        }).collect() }
}
