//! The library's list mode for the LOOKS lanes (Effects, Transitions): the
//! same effects as the picker's tiles, as rows in the library's own list
//! (thumbnail, name, what it is, or why it is unavailable). Clicks, drags
//! and drops are the picker's own actions, so arming, drag-and-drop and
//! the refusals behave exactly as with the tiles.
//!
//! Which of the two shows is decided in one place ([`VideoCut::looks_view`])
//! from the page's lane and list mode on every library paint.

use makepad_library_browser::list::{CardKind, ListRow};
use super::{PageLane, VideoCut};
use makepad_content_packs::picker::{self as pack_picker, ListedPreset, PickerAction};
use makepad_widgets::*;

/// One listed effect: preset, title, why not (unavailable).
#[derive(Clone, Debug, Default)]
pub struct LookRow {
    pub preset: String,
    pub title: String,
    pub why: Option<String>,
}

impl VideoCut {
    pub(super) fn looks_lane(&self) -> Option<bool> {
        match self.lib.lane {
            Some(PageLane::Effects) => Some(false),
            Some(PageLane::Transitions) => Some(true),
            _ => None,
        }
    }

    /// Show the picker's tiles, or the library list: the one source of
    /// truth is `self.lib.lane` and `self.lib.list_mode`.
    pub(super) fn looks_view(&mut self, cx: &mut Cx, ui: &WidgetRef) -> Option<Vec<ListRow>> {
        let looks = self.looks_lane();
        let tiles = shows_tiles(self.lib.lane, self.lib.list_mode);
        self.lib.list(cx, ui).set_visible(cx, !tiles);
        ui.widget(cx, ids!(cut_fx_picker)).set_visible(cx, tiles);
        let transition = looks?;
        let picker = ui.widget(cx, ids!(cut_fx_picker));
        let mut listed: Option<Vec<ListedPreset>> = None;
        let found = pack_picker::with_picker(&picker, &mut |picker| {
            picker.set_category(cx, if transition { "transition" } else { "" });
            if !tiles {
                picker.ensure_loaded(cx);
                listed = Some(picker.listed());
            }
        });
        if !found {
            return None;
        }
        let listed = listed?;
        self.looks_rows = listed.iter().map(|row| LookRow { preset: row.preset.clone(), title: row.title.clone(), why: row.why.clone() }).collect();
        let search = self.lib.search.trim().to_lowercase();
        let rows: Vec<ListRow> = listed.into_iter().filter(|row| search.is_empty() || row.title.to_lowercase().contains(&search))
            .map(|ListedPreset { title, sub, thumbnail: texture, why, .. }| ListRow {
                title,
                detail: why.clone().unwrap_or(sub),
                kind: CardKind::Image,
                texture,
                badge: why.map(|_| ("UNAVAILABLE".to_string(), 0x3a2a2aff, 0xd98b8bff)),
                ..ListRow::default()
            }).collect();
        // Rows follow `looks_rows` one to one (search filters both).
        if !search.is_empty() {
            self.looks_rows.retain(|row| row.title.to_lowercase().contains(&search));
        }
        if rows.is_empty() {
            // The catalog is still loading: look again next pump.
            self.repaint_library = true;
        }
        Some(rows)
    }

    /// A list row of a LOOKS lane clicked, dragged or dropped: the
    /// picker's action for it (an unavailable one says why).
    pub(super) fn looks_action(&mut self, cx: &mut Cx, ui: &WidgetRef, index: usize, abs: Option<(Vec2d, bool)>) -> bool {
        let Some(transition) = self.looks_lane().filter(|_| self.lib.list_mode) else { return false };
        let Some(row) = self.looks_rows.get(index).cloned() else { return true };
        if let Some(why) = row.why {
            if abs.is_none_or(|(_, dropped)| dropped) {
                self.status = format!("{}: {why}", row.title);
                self.repaint = true;
            }
            return true;
        }
        let category = if transition { "transition".to_string() } else { String::new() };
        let action = match abs {
            None => PickerAction::Choose { preset: row.preset, title: row.title, category, shift: false },
            Some((abs, false)) => PickerAction::Drag { preset: row.preset, title: row.title, category, abs },
            Some((abs, true)) => PickerAction::Drop { preset: row.preset, title: row.title, category, abs, why: None },
        };
        self.picker_action(cx, ui, action);
        true
    }
}

/// The picker's tiles show only for a LOOKS lane in grid mode; every other
/// lane and list mode use the library list.
pub(super) fn shows_tiles(lane: Option<PageLane>, list_mode: bool) -> bool {
    matches!(lane, Some(PageLane::Effects | PageLane::Transitions)) && !list_mode
}

#[cfg(test)]
mod tests {
    use makepad_library_browser::lanes::LibLane;
    use super::*;

    #[test]
    fn the_view_follows_the_lane_and_the_mode() {
        for lane in [LibLane::Video, LibLane::Image, LibLane::Audio, LibLane::Cuts, LibLane::AppRuns] {
            assert!(!shows_tiles(Some(PageLane::Store(lane)), false));
            assert!(!shows_tiles(Some(PageLane::Store(lane)), true));
        }
        for lane in [PageLane::Effects, PageLane::Transitions] {
            assert!(shows_tiles(Some(lane), false));
            assert!(!shows_tiles(Some(lane), true));
        }
        assert!(!shows_tiles(None, false));
    }
}
