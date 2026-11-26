//! The per-track menu (donor `track_menu`, deleted in ef48f3d9): a right
//! click on a library row offers what can be done with that track from where
//! it is listed, each verb with the key that does the same.
//!
//! Pure: which rows a menu has follows from where the track is listed and
//! whether a deck can take it.

use crate::library::LibraryMode;

/// What a track menu row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackVerb {
    LoadA,
    LoadB,
    Enqueue,
    PlayNext,
    Unqueue,
}

/// One row: the verb, its label, its key, and a rule above it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackMenuRow {
    pub verb: TrackVerb,
    pub label: &'static str,
    pub key: &'static str,
    pub separator: bool,
}

/// The rows for a track listed in `mode`; `can_load` when a deck can take it.
pub fn track_menu(mode: LibraryMode, can_load: bool) -> Vec<TrackMenuRow> {
    let mut rows: Vec<TrackMenuRow> = Vec::new();
    fn row(rows: &mut Vec<TrackMenuRow>, verb: TrackVerb, label: &'static str, key: &'static str) {
        rows.push(TrackMenuRow { verb, label, key, separator: false });
    }
    if can_load {
        row(&mut rows, TrackVerb::LoadA, "Load to deck A", "A");
        row(&mut rows, TrackVerb::LoadB, "Load to deck B", "B");
    }
    let set_group = rows.len();
    if mode != LibraryMode::Queue {
        row(&mut rows, TrackVerb::Enqueue, "Add to the set list", "Q");
    }
    row(&mut rows, TrackVerb::PlayNext, "Play it next", "N");
    if mode == LibraryMode::Queue {
        row(&mut rows, TrackVerb::Unqueue, "Take off the set list", "Delete");
    }
    if set_group > 0 {
        rows[set_group].separator = true;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verbs(mode: LibraryMode, can_load: bool) -> Vec<TrackVerb> {
        track_menu(mode, can_load).into_iter().map(|row| row.verb).collect()
    }

    #[test]
    fn each_list_offers_its_own_verbs() {
        use TrackVerb::*;
        assert_eq!(verbs(LibraryMode::Explorer, true), [LoadA, LoadB, Enqueue, PlayNext]);
        assert_eq!(verbs(LibraryMode::Queue, true), [LoadA, LoadB, PlayNext, Unqueue], "a queued track is not offered a second seat");
        assert_eq!(verbs(LibraryMode::History, false), [Enqueue, PlayNext], "no deck to load: no load rows");
    }

    #[test]
    fn a_rule_parts_the_decks_from_the_set_list_and_never_opens_a_menu() {
        let rows = track_menu(LibraryMode::Explorer, true);
        assert!(!rows[0].separator);
        assert!(rows[2].separator);
        assert!(track_menu(LibraryMode::History, false).iter().all(|row| !row.separator));
    }
}
