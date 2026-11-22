//! A slot's firmware debug table in absolute byte addresses.

use fab_elec_mcu::{ChipMap, Compiled};
use fab_elec_mcu_emu::FwDebug;
use fab_elec_mcu_rt::SETUP;

use crate::Slot;

/// The slot's debug table and any note about an unusable step map.
pub fn debug_table(c: &Compiled, source_len: usize, slot: &Slot) -> (FwDebug, Option<String>) {
    let mut entries: Vec<_> = slot.handlers.iter().map(|h| (h.address, h.id)).collect();
    entries.sort_unstable();
    let mut returns = Vec::new();
    if slot.handlers.iter().any(|h| h.name == SETUP) {
        returns.push(slot.symbols.setup_call + 3);
    }
    if slot.handlers.iter().any(|h| h.name != SETUP) {
        returns.push(slot.symbols.handler_return);
    }
    returns.sort_unstable();
    returns.dedup();
    let mut runtime: Vec<_> = slot.map.runtime.iter().map(|r| (r.start, r.name)).collect();
    runtime.sort_by_key(|r| r.0);
    fab_elec_mcu::debug_table(c, source_len, ChipMap {
        funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(),
        image_id: slot.image_id, stmt_starts: &slot.map.handlers.stmt_starts, conds: Vec::new(), entries, returns,
        state: slot.layout.state, runtime, runtime_end: slot.code_base,
    })
}
