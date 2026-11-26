//! The library's verbs, named, and the keys that ask for them.
//!
//! Until now every one of these existed only as the body of a mouse handler,
//! so there was nothing for a key -- or, later, a controller -- to ask for
//! except by growing a second copy of the same sequence beside the first.
//! This module is the layer that stops that: the verbs have names, the keys
//! map onto the names, and both the hand and the keyboard reach the same
//! implementation. The MIDI half of the item this comes from is deliberately
//! not built (see the commit), but it becomes a binding table over these
//! names rather than a rewrite.
//!
//! Everything here is pure: no `Cx`, no widgets, no clock. The cursor walk,
//! the pick rules for a click and a key, and the re-mapping of the picks and
//! the cursor across a rebuilt listing are exactly the parts that are easy to
//! get subtly wrong and easy to test, so they live here and the widget keeps
//! only the drawing.

use crate::model::DeckId;
use makepad_widgets::*;

/// Where a cursor step goes. Rows and pages are signed so one arm handles
/// both directions and the clamp cannot disagree between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMove {
    Rows(i32),
    Pages(i32),
    First,
    Last,
}

/// What the library can be asked to do.
///
/// `LoadDeck` carries a [`DeckId`] rather than a deck TARGET, so a command
/// cannot ask for a deck that is not a deck -- the target's third state means
/// "load nothing", which is a setting, not a verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryCmd {
    /// Move where the keys are standing. `extend` takes the pick with it,
    /// the way a shift-click does.
    Cursor { step: CursorMove, extend: bool },
    /// Load the row under the cursor onto this deck, named outright.
    LoadDeck(DeckId),
    /// Load it wherever a plain click would have sent it.
    LoadTarget,
    /// Put it at the back of the set list.
    Enqueue,
    /// Put it at the front of the set list: the next a deck takes.
    PlayNext,
    /// Take it off the set list (the set list's own rows).
    Unqueue,
    FocusSearch,
}

impl LibraryCmd {
    /// The stable name of this verb.
    ///
    /// These are the item's "named commands": what a binding binds to, and
    /// what makes this a layer rather than a key match inlined in a handler.
    /// Treated as settings keys the day anything binds them, so renaming one
    /// is renaming a slug.
    ///
    /// `extend` is deliberately NOT part of the name. Taking the pick along
    /// is the shift modifier of the same verb, not a twelfth verb.
    pub fn name(self) -> &'static str {
        match self {
            LibraryCmd::Cursor { step, .. } => match step {
                CursorMove::Rows(n) if n >= 0 => "library_next_row",
                CursorMove::Rows(_) => "library_prev_row",
                CursorMove::Pages(n) if n >= 0 => "library_next_page",
                CursorMove::Pages(_) => "library_prev_page",
                CursorMove::First => "library_first_row",
                CursorMove::Last => "library_last_row",
            },
            LibraryCmd::LoadDeck(DeckId::A) => "library_load_a",
            LibraryCmd::LoadDeck(DeckId::B) => "library_load_b",
            LibraryCmd::LoadTarget => "library_load",
            LibraryCmd::Enqueue => "library_enqueue",
            LibraryCmd::PlayNext => "library_play_next",
            LibraryCmd::Unqueue => "library_unqueue",
            LibraryCmd::FocusSearch => "library_focus_search",
        }
    }
}

/// The keys, and the verb each one asks for unshifted.
pub const LIBRARY_KEYS: [(KeyCode, LibraryCmd); 14] = [
    (KeyCode::ArrowDown, LibraryCmd::Cursor { step: CursorMove::Rows(1), extend: false }),
    (KeyCode::ArrowUp, LibraryCmd::Cursor { step: CursorMove::Rows(-1), extend: false }),
    (KeyCode::PageDown, LibraryCmd::Cursor { step: CursorMove::Pages(1), extend: false }),
    (KeyCode::PageUp, LibraryCmd::Cursor { step: CursorMove::Pages(-1), extend: false }),
    (KeyCode::Home, LibraryCmd::Cursor { step: CursorMove::First, extend: false }),
    (KeyCode::End, LibraryCmd::Cursor { step: CursorMove::Last, extend: false }),
    (KeyCode::KeyA, LibraryCmd::LoadDeck(DeckId::A)),
    (KeyCode::KeyB, LibraryCmd::LoadDeck(DeckId::B)),
    (KeyCode::ReturnKey, LibraryCmd::LoadTarget),
    (KeyCode::KeyQ, LibraryCmd::Enqueue),
    (KeyCode::KeyN, LibraryCmd::PlayNext),
    (KeyCode::Delete, LibraryCmd::Unqueue),
    (KeyCode::Backspace, LibraryCmd::Unqueue),
    (KeyCode::Slash, LibraryCmd::FocusSearch),
];

/// Which verb a key press asks for, if any.
///
/// Control, alt and the platform key mean this is not a library command at
/// all: every OS combination and every existing shortcut stays free, and a
/// stray Ctrl+A while the listing has focus must not select a track.
///
/// Shift takes a RANGE. It turns a cursor move into one that carries the
/// pick, and it turns everything else into no command at all -- a shift that
/// loaded a deck would be a range gesture that put a record on air.
pub fn command_for_key(code: KeyCode, mods: KeyModifiers) -> Option<LibraryCmd> {
    if mods.control || mods.alt || mods.logo {
        return None;
    }
    let cmd = LIBRARY_KEYS.iter().find(|(key, _)| *key == code).map(|(_, cmd)| *cmd)?;
    match (cmd, mods.shift) {
        (cmd, false) => Some(cmd),
        (LibraryCmd::Cursor { step, .. }, true) => Some(LibraryCmd::Cursor { step, extend: true }),
        (_, true) => None,
    }
}

/// The page step when the list has not drawn yet and has no count to give.
pub const DEFAULT_PAGE_ROWS: usize = 8;

/// How many rows one page step moves, from the number of rows the list
/// actually DREW.
///
/// Measured rather than computed from a row height on purpose: the rows are
/// not a uniform height. The previewing row wears a taller template, so a
/// page worked out from a fixed stride over-counts by whatever the open
/// player is worth -- exactly when the operator is walking the library with
/// something in the phones.
///
/// One row of overlap is kept, so the row that was at the far edge is still
/// on screen after the step and the operator keeps their place.
pub fn page_rows(drawn: usize) -> usize {
    if drawn == 0 {
        return DEFAULT_PAGE_ROWS;
    }
    drawn.saturating_sub(1).max(1)
}

/// Where the cursor lands after one step.
///
/// The arithmetic is done in `i64` and clamped there, before anything is cast
/// back: a page step near the top of a long listing goes far negative first,
/// and clamping after a cast to `usize` would have wrapped it to the end.
pub fn step_cursor(
    cursor: Option<usize>,
    len: usize,
    page: usize,
    step: CursorMove,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len as i64 - 1;
    let at = match (cursor, step) {
        // Nothing is standing anywhere yet: the first move lands on the end
        // it came from, so a first press of Down reaches the top row rather
        // than the second one.
        (None, CursorMove::Rows(n) | CursorMove::Pages(n)) if n >= 0 => 0,
        (None, CursorMove::Rows(_) | CursorMove::Pages(_)) => last,
        (_, CursorMove::First) => 0,
        (_, CursorMove::Last) => last,
        (Some(at), CursorMove::Rows(n)) => at as i64 + n as i64,
        (Some(at), CursorMove::Pages(n)) => at as i64 + n as i64 * page.max(1) as i64,
    };
    Some(at.clamp(0, last) as usize)
}

/// Where the cursor goes when the listing under it is rebuilt.
///
/// A row number means a different record after a rebuild, and the explorer is
/// rebuilt for every badge and every keystroke. `None` when the record it was
/// standing on is gone -- the keys then start again from the end they are
/// moving towards, which is truer than guessing a neighbour.
pub fn remap_cursor<K: PartialEq>(was: Option<&K>, after: &[K]) -> Option<usize> {
    let was = was?;
    after.iter().position(|held| held == was)
}

/// The same, for the picks: every one whose record survived, in the new
/// order.
pub fn remap_picks<K: PartialEq>(was: &[K], after: &[K]) -> Vec<usize> {
    after
        .iter()
        .enumerate()
        .filter(|(_, held)| was.iter().any(|key| key == *held))
        .map(|(row, _)| row)
        .collect()
}

/// Where one list's picks stand: which rows are picked, where a shift-range
/// measures from, and where the keys are standing.
///
/// All three are row numbers, so they mean something only against the
/// listing they were taken on; [`carry_picks`] moves them to a rebuilt one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picks {
    /// Rows the operator has picked, sorted. A pick is not a load: it is
    /// what the hand is about to drag somewhere.
    pub rows: Vec<usize>,
    /// Where a shift-range measures from — the last row picked outright.
    pub anchor: Option<usize>,
    /// Where the KEYS are standing. A pick says what the hand is about to
    /// drag; the cursor says which row an arrow moves from and which row a
    /// named load acts on. They travel together for a plain move — one idea
    /// on screen, not two — and part only under shift, which walks the
    /// cursor while dragging the pick out behind it.
    pub cursor: Option<usize>,
}

/// A click on `row` of a listing `len` rows long, with whatever the keyboard
/// was holding. `false`, with nothing changed, for a row past the end.
///
/// Plain picks that row alone; ctrl (or the platform key) toggles it; shift
/// takes everything between the anchor and it.
pub fn click_row(picks: &mut Picks, len: usize, row: usize, modifiers: KeyModifiers) -> bool {
    if row >= len {
        return false;
    }
    // The keys carry on from wherever the hand last was, so a click and
    // then an arrow is one continuous gesture rather than two.
    picks.cursor = Some(row);
    if modifiers.shift {
        let from = picks.anchor.unwrap_or(row);
        let (lo, hi) = if from <= row { (from, row) } else { (row, from) };
        picks.rows = (lo..=hi).collect();
    } else if modifiers.control || modifiers.logo {
        match picks.rows.iter().position(|picked| *picked == row) {
            Some(at) => {
                picks.rows.remove(at);
            }
            None => {
                picks.rows.push(row);
                picks.rows.sort_unstable();
            }
        }
        picks.anchor = Some(row);
    } else if picks.rows.len() > 1 && picks.rows.contains(&row) {
        // A plain press on a row that is ALREADY part of a set keeps the
        // set: the hand is most likely about to carry it somewhere, and
        // a press that collapsed the pick would leave one row in the
        // fist instead of the several the operator chose.
        picks.anchor = Some(row);
    } else {
        picks.rows = vec![row];
        picks.anchor = Some(row);
    }
    true
}

/// One step of where the keys are standing, over a listing `len` rows long
/// whose page is `page` rows, and the row it landed on. `None`, with nothing
/// changed, when the listing has no row to stand on.
///
/// A plain move IS the pick, so an arrow walk leaves exactly one idea on
/// screen rather than a cursor and a selection disagreeing about which
/// record the operator means. Under `extend` the cursor walks on its own
/// and drags the range out behind it from the anchor, which is the same
/// thing a shift-click does with the mouse.
pub fn move_cursor(
    picks: &mut Picks,
    len: usize,
    page: usize,
    step: CursorMove,
    extend: bool,
) -> Option<usize> {
    let row = step_cursor(picks.cursor, len, page, step)?;
    picks.cursor = Some(row);
    match extend {
        true => {
            let from = picks.anchor.unwrap_or(row);
            let (lo, hi) = if from <= row { (from, row) } else { (row, from) };
            picks.rows = (lo..=hi).collect();
        }
        false => {
            picks.rows = vec![row];
            picks.anchor = Some(row);
        }
    }
    Some(row)
}

/// The picks after the listing under them was rebuilt: `was` and `now` are
/// the records of the old and the new listing, in row order.
///
/// Each pick follows its record, and a pick whose record is gone is dropped.
/// The anchor and the cursor follow theirs the same way. The anchor is also
/// dropped when no pick survived: a rebuild that kept it would leave it
/// measuring from whichever record had landed on that number, and the next
/// shift-click would select a range nobody asked for.
pub fn carry_picks<K: PartialEq + Clone>(picks: &Picks, was: &[K], now: &[K]) -> Picks {
    let picked: Vec<K> = picks.rows.iter().filter_map(|row| was.get(*row).cloned()).collect();
    let rows = remap_picks(&picked, now);
    let anchor = match rows.is_empty() {
        true => None,
        false => remap_cursor(picks.anchor.and_then(|row| was.get(row)), now),
    };
    let cursor = remap_cursor(picks.cursor.and_then(|row| was.get(row)), now);
    Picks { rows, anchor, cursor }
}

/// Which end of the set list a track is added to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueHow {
    Tail,
    Next,
    Replace,
}

/// The set-list modifier rule, in one place so the row's `+` chip and the
/// keyboard cannot drift apart: control starts the list over, shift plays
/// this one next, a plain press adds to the tail.
pub fn queue_how(mods: KeyModifiers) -> QueueHow {
    if mods.control {
        QueueHow::Replace
    } else if mods.shift {
        QueueHow::Next
    } else {
        QueueHow::Tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> KeyModifiers {
        KeyModifiers::default()
    }

    fn shifted() -> KeyModifiers {
        KeyModifiers { shift: true, ..Default::default() }
    }

    #[test]
    fn every_named_command_answers_to_one_name() {
        let mut seen: Vec<(&'static str, LibraryCmd)> = Vec::new();
        for (_, cmd) in LIBRARY_KEYS {
            let name = cmd.name();
            assert!(name.starts_with("library_"), "{name} is not in the library's namespace");
            match seen.iter().find(|(held, _)| *held == name) {
                // Delete and Backspace both take a row off the set list.
                Some((_, held)) => assert_eq!(*held, cmd, "{name} names two different commands"),
                None => seen.push((name, cmd)),
            }
        }
        assert_eq!(seen.len(), LIBRARY_KEYS.len() - 1, "one verb has two keys");
    }

    #[test]
    fn taking_the_pick_along_is_the_same_verb_under_shift() {
        let walk = LibraryCmd::Cursor { step: CursorMove::Rows(1), extend: false };
        let drag = LibraryCmd::Cursor { step: CursorMove::Rows(1), extend: true };
        assert_eq!(walk.name(), drag.name(), "extend is a modifier, not a twelfth verb");
    }

    #[test]
    fn shift_and_an_arrow_takes_the_pick_with_it() {
        assert_eq!(
            command_for_key(KeyCode::ArrowDown, shifted()),
            Some(LibraryCmd::Cursor { step: CursorMove::Rows(1), extend: true }),
        );
        assert_eq!(
            command_for_key(KeyCode::ArrowDown, plain()),
            Some(LibraryCmd::Cursor { step: CursorMove::Rows(1), extend: false }),
        );
    }

    #[test]
    fn a_shift_that_is_not_a_range_is_not_a_command_at_all() {
        for code in [KeyCode::KeyA, KeyCode::KeyB, KeyCode::ReturnKey, KeyCode::KeyN] {
            assert_eq!(
                command_for_key(code, shifted()),
                None,
                "{code:?} under shift is a range gesture, and must not reach a deck",
            );
        }
    }

    #[test]
    fn a_key_with_a_modifier_the_library_did_not_ask_for_is_not_a_command() {
        for mods in [
            KeyModifiers { control: true, ..Default::default() },
            KeyModifiers { alt: true, ..Default::default() },
            KeyModifiers { logo: true, ..Default::default() },
            KeyModifiers { control: true, shift: true, ..Default::default() },
        ] {
            for (code, _) in LIBRARY_KEYS {
                assert_eq!(command_for_key(code, mods), None, "{code:?} with {mods:?}");
            }
        }
    }

    #[test]
    fn a_key_the_library_does_not_use_is_left_alone() {
        for code in [KeyCode::KeyZ, KeyCode::Escape, KeyCode::F3, KeyCode::Space] {
            assert_eq!(command_for_key(code, plain()), None);
        }
    }

    #[test]
    fn a_first_arrow_lands_on_the_end_it_came_from() {
        assert_eq!(step_cursor(None, 10, 5, CursorMove::Rows(1)), Some(0), "down reaches the top");
        assert_eq!(step_cursor(None, 10, 5, CursorMove::Rows(-1)), Some(9), "up reaches the last");
        assert_eq!(step_cursor(None, 10, 5, CursorMove::Pages(1)), Some(0));
    }

    #[test]
    fn walking_past_either_end_stays_on_it() {
        assert_eq!(step_cursor(Some(9), 10, 5, CursorMove::Rows(1)), Some(9));
        assert_eq!(step_cursor(Some(0), 10, 5, CursorMove::Rows(-1)), Some(0));
        // The clamp has to survive the page arithmetic going far past both
        // ends, which is where an unsigned cast would have wrapped it.
        assert_eq!(step_cursor(Some(1), 10, 900, CursorMove::Pages(-1)), Some(0));
        assert_eq!(step_cursor(Some(1), 10, 900, CursorMove::Pages(1)), Some(9));
    }

    #[test]
    fn the_ends_are_the_ends_whatever_the_page_size() {
        for page in [1, 5, 999] {
            assert_eq!(step_cursor(Some(4), 10, page, CursorMove::First), Some(0));
            assert_eq!(step_cursor(Some(4), 10, page, CursorMove::Last), Some(9));
        }
    }

    #[test]
    fn an_empty_listing_has_no_cursor_to_move() {
        for step in
            [CursorMove::Rows(1), CursorMove::Rows(-1), CursorMove::First, CursorMove::Last]
        {
            assert_eq!(step_cursor(Some(3), 0, 5, step), None);
            assert_eq!(step_cursor(None, 0, 5, step), None);
        }
    }

    #[test]
    fn a_page_keeps_one_row_of_the_last_page_and_is_never_nothing() {
        assert_eq!(page_rows(12), 11, "one row of overlap, so the edge row stays on screen");
        assert_eq!(page_rows(2), 1);
        assert_eq!(page_rows(1), 1, "a page never moves by nothing");
        assert_eq!(page_rows(0), DEFAULT_PAGE_ROWS, "nothing drawn yet: a sane guess");
    }

    #[test]
    fn a_rebuilt_listing_keeps_the_cursor_on_the_record_it_was_on() {
        let now = ["e", "c", "a"];
        assert_eq!(remap_cursor(Some(&"c"), &now), Some(1), "the record moved, the cursor with it");
        assert_eq!(remap_cursor(Some(&"b"), &now), None, "its record is gone");
        assert_eq!(remap_cursor(None::<&&str>, &now), None);
    }

    #[test]
    fn a_pick_and_the_cursor_survive_a_rebuild_together() {
        let was = ["b", "d"];
        let now = ["a", "b", "c", "e"];
        assert_eq!(remap_picks(&was, &now), vec![1], "d is gone, b keeps its pick");
        assert_eq!(remap_picks(&[] as &[&str], &now), Vec::<usize>::new());
        assert_eq!(remap_picks(&was, &[] as &[&str]), Vec::<usize>::new());
    }

    fn control() -> KeyModifiers {
        KeyModifiers { control: true, ..Default::default() }
    }

    fn picked(rows: &[usize], anchor: Option<usize>, cursor: Option<usize>) -> Picks {
        Picks { rows: rows.to_vec(), anchor, cursor }
    }

    #[test]
    fn a_plain_click_picks_that_row_alone_and_stands_the_keys_on_it() {
        let mut picks = picked(&[1], Some(1), Some(1));
        assert!(click_row(&mut picks, 10, 6, plain()));
        assert_eq!(picks, picked(&[6], Some(6), Some(6)));
    }

    #[test]
    fn control_toggles_a_row_and_moves_the_anchor_to_it() {
        let mut picks = picked(&[2], Some(2), Some(2));
        click_row(&mut picks, 10, 7, control());
        assert_eq!(picks, picked(&[2, 7], Some(7), Some(7)), "added, and kept in row order");
        click_row(&mut picks, 10, 2, control());
        assert_eq!(picks, picked(&[7], Some(2), Some(2)), "taken back out");
        let logo = KeyModifiers { logo: true, ..Default::default() };
        click_row(&mut picks, 10, 4, logo);
        assert_eq!(picks.rows, vec![4, 7], "the platform key toggles as control does");
    }

    #[test]
    fn shift_takes_the_range_from_the_anchor_either_way_and_leaves_the_anchor() {
        let mut picks = picked(&[5], Some(5), Some(5));
        click_row(&mut picks, 10, 8, shifted());
        assert_eq!(picks, picked(&[5, 6, 7, 8], Some(5), Some(8)));
        click_row(&mut picks, 10, 3, shifted());
        assert_eq!(picks, picked(&[3, 4, 5], Some(5), Some(3)), "measured from the anchor again");
        let mut fresh = Picks::default();
        click_row(&mut fresh, 10, 4, shifted());
        assert_eq!(fresh, picked(&[4], None, Some(4)), "no anchor: the range is the row");
    }

    #[test]
    fn a_plain_press_inside_a_set_keeps_the_set_for_the_carry() {
        let mut picks = picked(&[2, 3, 4], Some(2), Some(4));
        click_row(&mut picks, 10, 3, plain());
        assert_eq!(picks, picked(&[2, 3, 4], Some(3), Some(3)));
        // One pick is not a set: a press on it is an ordinary click.
        let mut single = picked(&[3], Some(3), Some(3));
        click_row(&mut single, 10, 3, plain());
        assert_eq!(single, picked(&[3], Some(3), Some(3)));
        // And a press outside the set replaces it.
        click_row(&mut picks, 10, 8, plain());
        assert_eq!(picks, picked(&[8], Some(8), Some(8)));
    }

    #[test]
    fn a_click_past_the_end_of_the_listing_changes_nothing() {
        let mut picks = picked(&[1], Some(1), Some(1));
        assert!(!click_row(&mut picks, 3, 3, plain()));
        assert_eq!(picks, picked(&[1], Some(1), Some(1)));
    }

    #[test]
    fn a_plain_key_step_moves_the_pick_and_the_anchor_with_the_cursor() {
        let mut picks = picked(&[2, 3], Some(2), Some(3));
        assert_eq!(move_cursor(&mut picks, 10, 4, CursorMove::Rows(1), false), Some(4));
        assert_eq!(picks, picked(&[4], Some(4), Some(4)));
        assert_eq!(move_cursor(&mut picks, 10, 4, CursorMove::Pages(1), false), Some(8));
        assert_eq!(picks, picked(&[8], Some(8), Some(8)));
    }

    #[test]
    fn a_shifted_key_step_drags_the_range_out_from_the_anchor() {
        let mut picks = picked(&[5], Some(5), Some(5));
        move_cursor(&mut picks, 10, 4, CursorMove::Rows(-1), true);
        move_cursor(&mut picks, 10, 4, CursorMove::Rows(-1), true);
        assert_eq!(picks, picked(&[3, 4, 5], Some(5), Some(3)));
        move_cursor(&mut picks, 10, 4, CursorMove::Last, true);
        assert_eq!(picks, picked(&[5, 6, 7, 8, 9], Some(5), Some(9)));
    }

    #[test]
    fn a_key_step_over_an_empty_listing_changes_nothing() {
        let mut picks = picked(&[1], Some(1), Some(1));
        assert_eq!(move_cursor(&mut picks, 0, 4, CursorMove::Rows(1), false), None);
        assert_eq!(picks, picked(&[1], Some(1), Some(1)));
    }

    #[test]
    fn the_picks_the_anchor_and_the_cursor_follow_their_records_across_a_rebuild() {
        let was = ["a", "b", "c", "d", "e"];
        let now = ["e", "d", "c", "b", "a"];
        let picks = picked(&[1, 3], Some(1), Some(3));
        assert_eq!(carry_picks(&picks, &was, &now), picked(&[1, 3], Some(3), Some(1)));
        let reversed = picked(&[0, 4], Some(4), Some(0));
        assert_eq!(carry_picks(&reversed, &was, &now), picked(&[0, 4], Some(0), Some(4)));
    }

    #[test]
    fn a_rebuild_drops_what_lost_its_record_and_the_anchor_with_the_last_pick() {
        let was = ["a", "b", "c", "d"];
        let picks = picked(&[1, 2], Some(1), Some(2));
        // "b" is gone: its pick and the anchor on it go; "c" stays picked.
        assert_eq!(carry_picks(&picks, &was, &["a", "c", "d"]), picked(&[1], None, Some(1)));
        // Nothing picked survived: no anchor either, though its record did.
        let picks = picked(&[2], Some(0), Some(3));
        assert_eq!(carry_picks(&picks, &was, &["a", "d"]), picked(&[], None, Some(1)));
        // The cursor keeps its own record whatever happened to the picks.
        let cursor_only = picked(&[], None, Some(3));
        assert_eq!(carry_picks(&cursor_only, &was, &["d", "a"]), picked(&[], None, Some(0)));
    }

    #[test]
    fn control_replaces_the_set_list_and_shift_plays_next() {
        assert_eq!(queue_how(plain()), QueueHow::Tail);
        assert_eq!(queue_how(shifted()), QueueHow::Next);
        assert_eq!(
            queue_how(KeyModifiers { control: true, ..Default::default() }),
            QueueHow::Replace,
        );
        assert_eq!(
            queue_how(KeyModifiers { control: true, shift: true, ..Default::default() }),
            QueueHow::Replace,
            "control wins, as the chip has always read it",
        );
    }
}
