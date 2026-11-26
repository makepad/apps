//! The host's hook for giving its CPU pool a turn during long board computations.
//!
//! A host that runs board work (a full zone fill, say) as jobs on a cooperative CPU pool installs
//! its "give way if due" function here, once; the computations call [`pace`] between their units
//! of work. Without a host (tools, tests) it does nothing.

use std::sync::OnceLock;

static PACE: OnceLock<fn()> = OnceLock::new();

/// Installs the host's "give way if due" function. Returns true for the first installation in the
/// process; later ones are ignored.
pub fn install_pace(pace: fn()) -> bool {
    PACE.set(pace).is_ok()
}

/// Gives the host's pool a turn if one is due. The host may park the thread here, so call it only
/// where no lock is held that a job holding a CPU token could wait on: computations building new
/// results, not edits of shared state. One atomic load when no host installed a hook.
#[inline]
pub fn pace() {
    if let Some(pace) = PACE.get() {
        pace();
    }
}
