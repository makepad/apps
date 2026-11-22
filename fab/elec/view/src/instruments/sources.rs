//! The host's evaluated bench, shared by all source cards: ELANG's `BenchView`, which says per value
//! whether an edit splices in place, goes through `splice_verified` or is refused.

// Re-exported so a stand-in host needs no further dependencies.
pub use fab_elec_core::{Diag, DocId};
pub use fab_elec_lang::{eval::{BenchOut, BenchScope, UnitState}, map::BenchKey, BenchEdit, BenchView};
use std::sync::LazyLock;

/// The edit decision for one value; a value the view does not list is refused.
pub fn edit<'a>(view: &'a BenchView, key: &BenchKey) -> &'a BenchEdit {
    static MISSING: LazyLock<BenchEdit> = LazyLock::new(|| BenchEdit::Refused(Diag::error("no source site for this value", Vec::new())));
    view.edits.iter().find(|(site, _)| site == key).map_or(&*MISSING, |(_, edit)| edit)
}
