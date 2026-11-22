//! Per-export CALL0 routines for island import and separate OSI thunk sets.

use fab_elec_mcu_emit32::xtensa::{assemble, reg::*, Asm};
use crate::{emit::PortCharge, insns::instructions, EntryKind};

/// The kernel emitter copies a2 to a12 (state) and a3 to a13 (shared), just
/// like the app emitter; Domain::Kernel supplies slot addresses and costs,
/// not a different entry ABI. Supply KernelOptions.state and the address of
/// the installed Module::shared() words even when an export uses no shared data.
#[derive(Clone, Copy)]
pub struct EntryRegs { pub state: u32, pub shared: u32 }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateReport {
    pub name: String,
    pub address: u32,
    pub blocks: bool,
    pub stack_bytes: u32,
    pub max_insns: u64,
    pub port_charges: Vec<PortCharge>,
    /// Absolute instruction addresses in the export body called by this gate.
    pub proven_accesses: Vec<super::ProvenAccess>,
}

pub struct Gates { pub bytes: Vec<u8>, pub gates: Vec<GateReport> }

/// Exports that can leave current BLOCKED.
/// Object waits, sleeps and self-deletion can publish BLOCKED;
/// ISR variants pass zero and must not inspect the interrupted task's state.
pub const BLOCKING_EXPORTS: &[&str] = &[
    "queue_send_back", "queue_send_front", "queue_recv", "sem_take", "mutex_lock",
    "task_delay", "task_delete",
    "osi_semphr_take", "osi_mutex_lock", "osi_queue_send_to_back", "osi_queue_send_to_front",
    "osi_queue_recv", "osi_task_delay", "osi_task_delete",
    "phy_lock",
];

/// These routines return the requested exit INTLEVEL in a3. Their thunks
/// supply entry PS and caller RA after the pack arguments, then replace only
/// PS.INTLEVEL on exit. They never block.
pub const EXIT_LEVEL_EXPORTS: &[&str] = &["osi_wifi_int_disable", "osi_wifi_int_restore"];

/// Marshal the island's a2..a7 into absolute export slots under the thunk's
/// interrupt mask. The routines preserve the CALL0 ABI and never unmask or
/// block themselves: only the thunk may do that after restoring its own SP.
pub fn gates(base: u32, code: &super::KernelCode, entry: EntryRegs) -> Gates {
    assert_eq!(base & 3, 0, "gate literal table requires word alignment");
    assert_eq!(entry.state & 3, 0, "gate state requires word alignment");
    assert_eq!(entry.shared & 3, 0, "gate shared data requires word alignment");
    for e in &code.entries {
        if let EntryKind::Export { params, .. } = &e.kind {
            assert!(params.len() <= 6, "kernel gate {} exceeds six island arguments", e.name);
        }
    }
    let mut spans = Vec::new();
    let bytes = assemble(&mut Asm::new(0, A8), |a| {
        spans.clear();
        a.literal_table();
        for (index, e) in code.entries.iter().enumerate() {
            let EntryKind::Export { params, result } = &e.kind else { continue; };
            let start = a.base.offset();
            a.addi(A1, A1, -16);
            a.s32i(A0, A1, 0);
            for (n, &slot) in params.iter().enumerate() {
                a.mov_reg_i32(A8, slot);
                a.s32i(A2 + n as u8, A8, 0);
            }
            a.mov_reg_i32(A2, entry.state);
            a.mov_reg_i32(A3, entry.shared);
            a.mov_reg_i32(A8, code.base.checked_add(e.offset).expect("kernel entry address overflow"));
            a.callx0(A8);
            a.mov_reg_i32(A8, *result);
            a.l32i(A2, A8, 0);
            if e.name == "osi_esp_timer_get_time" {
                // Gate64: resolve the module variable just like export slots.
                // The non-blocking thunk never touches a3 after this return.
                a.mov_reg_i32(A8, code.osi_time_high.expect("Gate64 requires osi_time_high"));
                a.l32i(A3, A8, 0);
            }
            if EXIT_LEVEL_EXPORTS.contains(&e.name.as_str()) {
                a.mov_reg_i32(A8, code.osi_crit_level.expect("exit-level gate requires osi_crit_level"));
                a.l32i(A3, A8, 0);
            }
            a.l32i(A0, A1, 0);
            a.addi(A1, A1, 16);
            a.ret_n();
            spans.push((index, start, a.base.offset()));
        }
    });
    base.checked_add(u32::try_from(bytes.len()).expect("gate size overflow")).expect("gate address overflow");
    let gates = spans.into_iter().map(|(index, start, end)| {
        let e = &code.entries[index];
        GateReport { name: e.name.clone(), address: base + start,
            blocks: BLOCKING_EXPORTS.contains(&e.name.as_str()),
            stack_bytes: e.stack_bytes.checked_add(16).expect("gate stack size overflow"),
            max_insns: e.max_insns.checked_add(instructions(&bytes, start, end)).expect("gate bound overflow"),
            port_charges: e.port_charges.clone(), proven_accesses: e.proven_accesses.clone() }
    }).collect();
    Gates { bytes, gates }
}
