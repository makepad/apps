//! The menu engine: one overlay layer that draws every dropdown, flyout and
//! right-click menu in the app, with real hover tracking, real keyboard
//! navigation and real submenus.
//!
//! It lives at the very end of the shell's overlay stack — after every panel,
//! before the `TipLayer` — for the same reason `TipLayer` does: an overlay
//! draw list floats over every area and splitter, and being last in the tree
//! means the layer wins hover against whatever sits underneath it.
//!
//! Anyone can raise a menu without owning one. A control emits
//! [`crate::ui::popover::FabUiAction::OpenMenu`] with the item list and the
//! anchor rect it measured from its own drawn area (never mid-pass turtle
//! state — the overlay-layer law), and reads `MenuPicked` back out of the next
//! actions pass. That is the whole contract; the vocabulary
//! (`MenuItem` / `MenuIcon` / `MenuPlace`) lives in `popover.rs` so the
//! dropdowns and this layer speak the same language.
//!
//! **Why the layer draws its own rows instead of instantiating row widgets.**
//! A menu has to answer two questions per pointer move — *which row is the
//! pointer on* and *does that row open a flyout* — and both answers must agree
//! with what is on screen to the pixel. Owning the geometry (`level_rect`,
//! `row_rect`) makes hit testing and drawing the same arithmetic, which is
//! what makes hover-into-submenu and arrow-key navigation behave (the
//! `TipLayer` idiom).
//!
//! The check / radio / flyout marks come out of the row shader — the same way
//! the stock `CheckBox` draws its tick.

use crate::ui::popover::{display_rgb, FabUiAction, MenuIcon, MenuItem, MenuPlace, MenuRegion, MenuSwatch, OpenPopup, PopupChange};
use makepad_widgets_core::*;
use std::collections::HashMap;
use std::ops::Range;

/// Row metrics. The layer sizes itself from the strings — text is not measured
/// until it is drawn, so drawing and hit testing share this arithmetic and
/// never disagree.
const ROW_H: f64 = 24.0;
const SEP_H: f64 = 9.0;
const HEADING_H: f64 = 22.0;
const COLUMN_GAP: f64 = 12.0;
/// Between a wrapped band of columns and the band above it.
const BAND_GAP: f64 = 8.0;
const MENU_PAD: f64 = 4.0;
const MENU_MIN_W: f64 = 150.0;
/// Left column that carries the check / radio mark, in a menu where a row has one.
const MARK_COL: f64 = 24.0;
const RIGHT_PAD: f64 = 10.0;
/// Left inset of the labels in a group with no marks: the same as the right.
const LEFT_PAD: f64 = RIGHT_PAD;
/// The swatch column after the mark gutter: a chip of `SWATCH` and a gap (the row shader draws
/// the chip at `MARK_COL`).
const SWATCH: f64 = 14.0;
const SWATCH_COL: f64 = SWATCH + 8.0;
/// Gap after the tag column.
const TAG_GAP: f64 = 8.0;
/// A grid menu's tiles (`Level::lay_out_grid`): the box, the gap between boxes, the icon's
/// side and its inset from the tile's top, and the label band's top.
const TILE_W: f64 = 92.0;
const TILE_H: f64 = 62.0;
const TILE_GAP: f64 = 2.0;
const TILE_ICON: f64 = 22.0;
const TILE_ICON_TOP: f64 = 10.0;
const TILE_LABEL_TOP: f64 = 38.0;

/// How long a grid menu takes to slide from one set of tiles to another (s).
const MORPH: f64 = 0.22;

/// The id of the row a grid menu shows above a group's tiles: it slides back to the tiles the
/// group was picked from (`FabMenuLayer::back`).
fn back_id() -> LiveId {
    live_id!(fab_menu_back)
}

/// True when the list draws as tiles: every row carries an icon and none is a heading or
/// sits in a column. Separators break the grid into bands.
pub(crate) fn is_grid(items: &[MenuItem]) -> bool {
    items.iter().any(|it| !it.separator && it.id != back_id())
        && items.iter().all(|it| it.separator || it.id == back_id() || (it.svg.is_some() && !it.heading && it.region == MenuRegion::Main))
}

/// Columns of a grid band: a short band is one row, a long one three or four wide.
fn grid_columns(items: &[MenuItem]) -> usize {
    let longest = items.split(|it| it.separator).map(|band| band.iter().filter(|it| it.id != back_id()).count()).max().unwrap_or(1);
    match longest {
        0..=5 => longest.max(1),
        6..=9 => 3,
        _ => 4,
    }
}

/// How long the pointer, on its way into an open flyout, rests on a row it crosses before that
/// row takes over (s).
const REST: f64 = 0.2;
/// How long a closed menu takes to fade out (s).
const FADE_OUT: f64 = 0.12;
/// The most of a rest one frame counts (s): a longer frame is a hitch.
const HITCH: f64 = 0.05;

/// Label insets for a whole list, with one mark gutter per separator-delimited group.
/// Swatch lists reserve the same swatch and tag columns throughout.
pub(crate) fn label_offsets(items: &[MenuItem], tag_w: f64) -> Vec<f64> {
    if items.iter().any(|it| it.swatch.is_some()) {
        return items.iter().map(|it| {
            if it.swatch.is_some() || !it.tag.is_empty() {
                MARK_COL + SWATCH_COL + if tag_w > 0.0 { tag_w + TAG_GAP } else { 0.0 }
            } else {
                MARK_COL
            }
        }).collect();
    }
    let mut offsets = vec![LEFT_PAD; items.len()];
    let mut start = 0;
    for group in items.split(|it| it.separator) {
        if group.iter().any(|it| it.icon != MenuIcon::None) {
            offsets[start..start + group.len()].fill(MARK_COL);
        }
        start += group.len() + 1;
    }
    offsets
}

/// What menu rows are drawn with. The menu layer and inline row lists (the Part panel's
/// trays) draw rows through it, so a row reads the same in both.
pub(crate) struct RowInk<'a> {
    pub row: &'a mut DrawMenuRow,
    pub label: &'a mut DrawText,
    pub side: &'a mut DrawText,
    pub label_on: Vec4f,
    pub side_on: Vec4f,
    pub disabled: Vec4f,
}

impl RowInk<'_> {
    pub fn width(&self, cx: &mut Cx2d, small: bool, text: &str) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        let draw = if small { &*self.side } else { &*self.label };
        draw.prepare_single_line_run(cx, text).map(|r| r.width_in_lpxs as f64).unwrap_or_else(|| text.chars().count() as f64 * CHAR_W)
    }

    /// Where a one-line text sits in a row so its ink is on the row's midline: its line box
    /// centred (the ink shift of `ink_centered` does the rest). Draw-pass only.
    fn top(&self, cx: &mut Cx2d, small: bool, text: &str, row: Rect) -> f64 {
        let draw = if small { &*self.side } else { &*self.label };
        let h = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text).size_in_lpxs.height as f64 * draw.font_scale as f64;
        row.pos.y + ((row.size.y - h) * 0.5).max(0.0)
    }

    /// The tag column's width: the widest tag of `items`.
    pub fn tag_w(&self, cx: &mut Cx2d, items: &[MenuItem]) -> f64 {
        items.iter().filter(|it| !it.tag.is_empty()).map(|it| self.width(cx, false, &it.tag)).fold(0.0, f64::max).ceil()
    }

    /// Measure one region's width once per draw, including its dim headings.
    pub fn need(&self, cx: &mut Cx2d, items: &[MenuItem], lefts: &[f64]) -> f64 {
        let mut need: f64 = 0.0;
        for (i, item) in items.iter().enumerate().filter(|(_, it)| !it.separator) {
            if item.heading {
                need = need.max(LEFT_PAD + self.width(cx, true, &item.label) + RIGHT_PAD);
                continue;
            }
            let side = side_text(item);
            let short_w = if side.is_empty() { 0.0 } else { self.width(cx, true, &side) + SHORT_GAP };
            let arrow_w = if item.submenu.is_empty() { 0.0 } else { ARROW_COL };
            need = need.max(lefts[i] + self.width(cx, false, &item.label) + short_w + arrow_w + RIGHT_PAD);
        }
        need.ceil()
    }

    /// Paint one visible row per draw, including its current, hover and press fills.
    pub fn paint(&mut self, cx: &mut Cx2d, r: Rect, items: &[MenuItem], i: usize, tag_w: f64, left: f64, hover: f32, down: bool) {
        let item = &items[i];
        self.row.current = if item.current { 1.0 } else { 0.0 };
        self.row.hover = hover;
        self.row.down = if down && item.enabled { 1.0 } else { 0.0 };
        self.row.disabled = if item.enabled { 0.0 } else { 1.0 };
        self.row.mark = match item.icon {
            MenuIcon::Check => 1.0,
            MenuIcon::Dot => 2.0,
            MenuIcon::None => 0.0,
        };
        self.row.arrow = if item.submenu.is_empty() { 0.0 } else { 1.0 };
        self.row.swatch = match item.swatch {
            Some(MenuSwatch::Colour(c)) => {
                let [r, g, b] = display_rgb(c);
                vec4(r, g, b, 1.0)
            }
            Some(MenuSwatch::Empty) => vec4(0.0, 0.0, 0.0, 0.5),
            Some(MenuSwatch::Unknown) | None => vec4(0.0, 0.0, 0.0, 0.0),
        };
        self.row.draw_abs(cx, r);
        self.label.color = if item.enabled { self.label_on } else { self.disabled };
        if !item.tag.is_empty() {
            let y = self.top(cx, false, &item.tag, r);
            let x = r.pos.x + MARK_COL + SWATCH_COL;
            self.label.draw_walk(cx, Walk::abs_rect(Rect { pos: dvec2(x, y), size: dvec2(tag_w.max(1.0), r.size.y - (y - r.pos.y) * 2.0) }), Align { x: 0.0, y: 0.5 }, &item.tag);
        }
        let side = side_text(item);
        let short_w = self.width(cx, true, &side);
        let arrow_w = if item.submenu.is_empty() { 0.0 } else { ARROW_COL };
        let text_w = (r.size.x - left - RIGHT_PAD - short_w - arrow_w).max(8.0);
        let label_y = self.top(cx, false, &item.label, r);
        self.label.draw_walk(cx, Walk::abs_rect(Rect { pos: dvec2(r.pos.x + left, label_y), size: dvec2(text_w, r.size.y - (label_y - r.pos.y) * 2.0) }), Align { x: 0.0, y: 0.5 }, &item.label);
        if !side.is_empty() {
            // Dimmer than the label, and dimmer still on a disabled row.
            self.side.color = if item.enabled { self.side_on } else { self.disabled };
            let short_y = self.top(cx, true, &side, r);
            self.side.draw_walk(
                cx,
                Walk::abs_rect(Rect { pos: dvec2(r.pos.x + r.size.x - RIGHT_PAD - arrow_w - short_w, short_y), size: dvec2(short_w, r.size.y - (short_y - r.pos.y) * 2.0) }),
                Align { x: 1.0, y: 0.5 },
                &side,
            );
        }
    }

    /// Draw a title in the shortcut text style and underline its column. O(label) per draw.
    fn paint_heading(&mut self, cx: &mut Cx2d, r: Rect, label: &str, rule: &mut DrawColor) {
        let y = self.top(cx, true, label, r);
        self.side.color = self.side_on;
        let text = Rect {
            pos: dvec2(r.pos.x + LEFT_PAD, y),
            size: dvec2(r.size.x - LEFT_PAD - RIGHT_PAD, r.size.y - (y - r.pos.y) * 2.0),
        };
        self.side.draw_walk(cx, Walk::abs_rect(text), Align { x: 0.0, y: 0.5 }, label);
        rule.draw_abs(cx, Rect { pos: dvec2(r.pos.x, r.pos.y + r.size.y - 1.0), size: dvec2(r.size.x, 1.0) });
    }
}

/// A row's right-hand column: its shortcut the way the platform writes it (`⇧⌘N` on macOS,
/// `Ctrl+Shift+N` elsewhere; Fab's `Cmd` is the primary modifier), else its note.
fn side_text(item: &MenuItem) -> String {
    if item.shortcut.is_empty() {
        return item.note.clone();
    }
    let chord: Vec<&str> = item.shortcut.split('+').map(|t| if t.eq_ignore_ascii_case("cmd") { "Cmd/Ctrl" } else { t }).collect();
    makepad_widgets_core::kbd::shortcut_label(&chord.join("+"))
}
const ARROW_COL: f64 = 14.0;
const CHAR_W: f64 = 5.9;
const SHORT_CHAR_W: f64 = 5.2;
/// Gap between the shortcut column and the label.
const SHORT_GAP: f64 = 18.0;

// ===========================================================================
// Geometry (drawing and hit testing read the same functions)
// ===========================================================================

fn item_height(item: &MenuItem) -> f64 {
    if item.separator {
        SEP_H
    } else if item.heading {
        HEADING_H
    } else {
        ROW_H
    }
}

/// The one-point rule inside a separator's full row. O(1) per separator drawn.
fn separator_line(r: Rect) -> Rect {
    Rect {
        pos: dvec2(r.pos.x + LEFT_PAD, r.pos.y + (SEP_H - 1.0) * 0.5),
        size: dvec2((r.size.x - LEFT_PAD - RIGHT_PAD).max(0.0), 1.0),
    }
}

/// Estimate a region's tag width, label insets and row width from its strings once per open.
fn estimate_region(items: &[MenuItem]) -> (f64, Vec<f64>, f64) {
    let tag_w = items.iter().map(|it| it.tag.chars().count() as f64 * CHAR_W).fold(0.0, f64::max);
    let lefts = label_offsets(items, tag_w);
    let mut w: f64 = MENU_MIN_W;
    for (i, it) in items.iter().enumerate() {
        if it.separator {
            continue;
        }
        if it.heading {
            w = w.max(LEFT_PAD + it.label.chars().count() as f64 * SHORT_CHAR_W + RIGHT_PAD);
            continue;
        }
        let label = it.label.chars().count() as f64 * CHAR_W;
        let side = side_text(it);
        let short = if side.is_empty() {
            0.0
        } else {
            side.chars().count() as f64 * SHORT_CHAR_W + SHORT_GAP
        };
        let arrow = if it.submenu.is_empty() { 0.0 } else { ARROW_COL };
        w = w.max(lefts[i] + label + short + arrow + RIGHT_PAD);
    }
    (tag_w, lefts, w.ceil())
}

#[derive(Clone, Copy)]
enum Placement {
    Root { anchor: Rect, place: MenuPlace },
    Flyout { parent: usize, row: usize },
}

/// Consecutive rows that share label insets and a measured tag column.
struct RegionRows {
    region: MenuRegion,
    rows: Range<usize>,
    tag_w: f64,
}

/// Estimate the bubble size before wrapping to a window. Linear in the rows and their text
/// for a list in column reading order.
pub fn measure(items: &[MenuItem]) -> Vec2d {
    if is_grid(items) || items.iter().any(|it| it.region != MenuRegion::Main) {
        let placement = Placement::Root { anchor: Rect::default(), place: MenuPlace::At };
        return Level::new(LiveId(0), items.to_vec(), placement).rect.size;
    }
    let (_, _, width) = estimate_region(items);
    dvec2(width, MENU_PAD * 2.0 + items.iter().map(item_height).sum::<f64>())
}

/// One open menu. Level 0 is the root; every further level is a flyout.
struct Level {
    owner: LiveId,
    items: Vec<MenuItem>,
    rect: Rect,
    placement: Placement,
    regions: Vec<RegionRows>,
    /// Rows and the rule above the footer in unscrolled bubble coordinates.
    row_boxes: Vec<Rect>,
    footer_rule: Option<Rect>,
    label_offsets: Vec<f64>,
    /// How far the rows are scrolled up inside the bubble (0 when the whole list fits).
    scroll: f64,
    /// Highlighted row (index into `items`), pointer- or keyboard-driven.
    hi: Option<usize>,
    /// Row currently held down.
    press: Option<usize>,
    /// Drawn as tiles (`is_grid`).
    grid: bool,
    /// The tiles a grid menu came from, newest last, each with the tile picked there: a group
    /// opens in place of them (`FabMenuLayer::drill`).
    trail: Vec<(Vec<MenuItem>, usize)>,
    /// The slide from the previous tiles to these, while it runs.
    morph: Option<Morph>,
}

/// A grid menu sliding from one set of tiles to another: the old tiles and where they were, the
/// bubble's old size, the way it slides (1 in to a group, -1 back) and how far it is (0 to 1).
struct Morph {
    items: Vec<MenuItem>,
    boxes: Vec<Rect>,
    from_size: Vec2d,
    dir: f64,
    t: f64,
    start: Option<f64>,
}

impl Morph {
    /// Eased: quick, then settling.
    fn eased(&self) -> f64 {
        1.0 - (1.0 - self.t).powi(3)
    }
}

impl Level {
    /// Estimate the unwrapped menu once, for callers without a window.
    fn new(owner: LiveId, items: Vec<MenuItem>, placement: Placement) -> Self {
        Self::new_in(owner, items, placement, f64::INFINITY)
    }

    /// Group and size the rows once per open. Column menus arrive in reading order through
    /// `open_menu_columns`. A caller that interleaves regions needs a stable sort first.
    fn new_in(owner: LiveId, mut items: Vec<MenuItem>, placement: Placement, room_w: f64) -> Self {
        let order = |it: &MenuItem| match it.region {
            MenuRegion::Column(c) => u16::from(c),
            MenuRegion::Main => u16::MAX,
        };
        if !items.iter().is_sorted_by_key(order) {
            items.sort_by_key(order);
        }
        let mut start = 0;
        let regions = items.chunk_by(|a, b| a.region == b.region).map(|group| {
            let rows = start..start + group.len();
            start = rows.end;
            RegionRows { region: group[0].region, rows, tag_w: 0.0 }
        }).collect();
        let grid = is_grid(&items);
        let mut level = Self {
            grid, trail: Vec::new(), morph: None,
            row_boxes: vec![Rect::default(); items.len()],
            label_offsets: Vec::with_capacity(items.len()),
            owner, items, rect: Rect::default(), placement, regions,
            footer_rule: None, scroll: 0.0, hi: None, press: None,
        };
        level.size_regions(room_w, estimate_region);
        level
    }

    /// Size each region from its own strings and marks once per open or draw.
    fn size_regions(&mut self, room_w: f64, mut measure: impl FnMut(&[MenuItem]) -> (f64, Vec<f64>, f64)) {
        if self.grid {
            self.label_offsets.clear();
            self.label_offsets.resize(self.items.len(), LEFT_PAD);
            self.lay_out_grid();
            return;
        }
        let mut col_widths = Vec::new();
        let mut main_w = MENU_MIN_W;
        self.label_offsets.clear();
        for group in &mut self.regions {
            let (tag_w, offsets, width) = measure(&self.items[group.rows.clone()]);
            group.tag_w = tag_w;
            self.label_offsets.extend(offsets);
            match group.region {
                MenuRegion::Main => main_w = width.max(MENU_MIN_W),
                MenuRegion::Column(c) => {
                    col_widths.resize(usize::from(c) + 1, 0.0);
                    col_widths[usize::from(c)] = width.max(MENU_MIN_W);
                }
            }
        }
        self.lay_out(&col_widths, main_w, room_w);
    }

    /// Place each row once per open or draw. A band ends at its tallest column's bottom.
    fn lay_out(&mut self, col_widths: &[f64], main_w: f64, room_w: f64) {
        let mut width = main_w;
        let mut next_x = MENU_PAD;
        let mut band_top = MENU_PAD;
        let mut bottom = MENU_PAD;
        self.footer_rule = None;
        for group in &self.regions {
            let (x, mut y, row_w) = match group.region {
                MenuRegion::Column(c) => {
                    let w = col_widths[usize::from(c)];
                    if next_x > MENU_PAD && next_x + w + MENU_PAD > room_w {
                        next_x = MENU_PAD;
                        band_top = bottom + BAND_GAP;
                    }
                    let x = next_x;
                    width = width.max(x + w + MENU_PAD);
                    next_x = x + w + COLUMN_GAP;
                    (x, band_top, w)
                }
                MenuRegion::Main => {
                    if !col_widths.is_empty() {
                        width = width.max(main_w + MENU_PAD * 2.0);
                        self.footer_rule = Some(Rect {
                            pos: dvec2(MENU_PAD, bottom),
                            size: dvec2(width - MENU_PAD * 2.0, SEP_H),
                        });
                        bottom += SEP_H;
                    }
                    (MENU_PAD, bottom, width - MENU_PAD * 2.0)
                }
            };
            for i in group.rows.clone() {
                let h = item_height(&self.items[i]);
                self.row_boxes[i] = Rect { pos: dvec2(x, y), size: dvec2(row_w, h) };
                y += h;
            }
            bottom = bottom.max(y);
        }
        self.rect.size = dvec2(width, bottom + MENU_PAD);
    }

    /// Place the tiles row by row, a separator starting a new band under a rule. Once per
    /// open or draw.
    fn lay_out_grid(&mut self) {
        let cols = grid_columns(&self.items);
        let width = MENU_PAD * 2.0 + cols as f64 * TILE_W + (cols - 1) as f64 * TILE_GAP;
        let (mut col, mut y) = (0usize, MENU_PAD);
        for i in 0..self.items.len() {
            if self.items[i].id == back_id() {
                // the way back: a full-width row over the tiles
                self.row_boxes[i] = Rect { pos: dvec2(MENU_PAD, y), size: dvec2(width - MENU_PAD * 2.0, ROW_H) };
                y += ROW_H + TILE_GAP;
                col = 0;
                continue;
            }
            if self.items[i].separator {
                if col > 0 {
                    y += TILE_H;
                    col = 0;
                }
                self.row_boxes[i] = Rect { pos: dvec2(MENU_PAD, y), size: dvec2(width - MENU_PAD * 2.0, SEP_H) };
                y += SEP_H;
                continue;
            }
            if col == cols {
                col = 0;
                y += TILE_H + TILE_GAP;
            }
            let x = MENU_PAD + col as f64 * (TILE_W + TILE_GAP);
            self.row_boxes[i] = Rect { pos: dvec2(x, y), size: dvec2(TILE_W, TILE_H) };
            col += 1;
        }
        if col > 0 {
            y += TILE_H;
        }
        self.footer_rule = None;
        self.rect.size = dvec2(width, y + MENU_PAD);
    }

    /// The nearest tile up (`dir` < 0) or down from `from` in a grid; the first or last
    /// choice when nothing is highlighted. O(rows) per key.
    fn step_vertical(&self, from: Option<usize>, dir: isize) -> Option<usize> {
        let Some(from) = from else { return self.step(None, dir) };
        let at = self.row_boxes[from].center();
        (0..self.items.len())
            .filter(|&i| i != from && self.selectable(i))
            .filter(|&i| {
                let dy = self.row_boxes[i].center().y - at.y;
                if dir > 0 { dy > 1.0 } else { dy < -1.0 }
            })
            .min_by(|&a, &b| {
                let key = |i: usize| {
                    let c = self.row_boxes[i].center();
                    ((c.y - at.y).abs(), (c.x - at.x).abs())
                };
                key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)
            })
            .or(Some(from))
    }

    fn from_row(&self) -> Option<usize> {
        match self.placement {
            Placement::Flyout { row, .. } => Some(row),
            Placement::Root { .. } => None,
        }
    }

    /// The height before clipping. The last row of each region supplies its bottom.
    fn content_h(&self) -> f64 {
        self.regions.iter().map(|group| {
            let r = self.row_boxes[group.rows.end - 1];
            r.pos.y + r.size.y
        }).fold(MENU_PAD, f64::max) + MENU_PAD
    }

    /// How far the rows can scroll: 0 unless the bubble was cut to fit the window.
    fn max_scroll(&self) -> f64 {
        // a sliding grid is short of its tiles only until it has grown to them
        if self.morph.is_some() {
            return 0.0;
        }
        (self.content_h() - self.rect.size.y).max(0.0)
    }

    /// Scroll by `dy`; true when the offset moved.
    fn scroll_by(&mut self, dy: f64) -> bool {
        let next = (self.scroll + dy).clamp(0.0, self.max_scroll());
        let moved = (next - self.scroll).abs() > 1e-6;
        self.scroll = next;
        moved
    }

    /// Reveal a row and the padding at either end of the content. O(regions) per reveal.
    fn reveal(&mut self, i: usize) {
        let r = self.row_boxes[i];
        let top = if r.pos.y == MENU_PAD { 0.0 } else { r.pos.y };
        let mut bottom = r.pos.y + r.size.y;
        let full = self.content_h();
        if bottom + MENU_PAD == full {
            bottom = full;
        }
        let view = self.rect.size.y;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + view {
            self.scroll = bottom - view;
        }
        self.scroll = self.scroll.clamp(0.0, (full - view).max(0.0));
    }

    /// Window coordinates for drawing, flyout placement and accessibility. O(1) per row.
    fn row_rect(&self, i: usize) -> Rect {
        self.row_boxes[i].translate(self.rect.pos - dvec2(0.0, self.scroll))
    }

    /// Hit-test the visible bubble in content coordinates. Menus have at most a few dozen
    /// rows, so a linear scan per pointer move also covers gaps and partially filled bands.
    fn row_at(&self, p: Vec2d) -> Option<usize> {
        if !self.rect.contains(p) || self.morph.is_some() {
            return None;
        }
        let local = p - self.rect.pos + dvec2(0.0, self.scroll);
        self.row_boxes.iter().position(|r| r.contains(local) && local.y < r.pos.y + r.size.y)
    }

    fn selectable(&self, i: usize) -> bool {
        self.items
            .get(i)
            .is_some_and(|it| !it.separator && !it.heading && it.enabled)
    }

    /// Step within the highlighted region, or start in reading order. O(rows) per key.
    fn step(&self, from: Option<usize>, dir: isize) -> Option<usize> {
        let rows = match from {
            Some(i) => self.regions.iter().find(|group| group.rows.contains(&i))?.rows.clone(),
            None => 0..self.items.len(),
        };
        let n = rows.len();
        if n == 0 {
            return None;
        }
        let start = match from {
            Some(i) => (i - rows.start) as isize,
            None => {
                if dir > 0 {
                    -1
                } else {
                    n as isize
                }
            }
        };
        for k in 1..=n as isize {
            let i = rows.start + (start + dir * k).rem_euclid(n as isize) as usize;
            if self.selectable(i) {
                return Some(i);
            }
        }
        None
    }

    /// Move to the nearest row in the adjacent region, cycling through the footer in
    /// reading order and skipping regions without a choice. O(rows) per horizontal key.
    fn step_region(&mut self, dir: isize) -> bool {
        let Some(from) = self.hi else { return false };
        if self.regions.len() < 2 {
            return false;
        }
        let Some(start) = self.regions.iter().position(|g| g.rows.contains(&from)) else { return false };
        let y = self.row_boxes[from].center().y;
        let count = self.regions.len() as isize;
        for step in 1..count {
            let group = (start as isize + dir * step).rem_euclid(count) as usize;
            let distance = |i: usize| (self.row_boxes[i].center().y - y).abs();
            let nearest = self.regions[group].rows.clone().filter(|&i| self.selectable(i))
                .min_by(|&a, &b| distance(a).total_cmp(&distance(b)));
            if let Some(row) = nearest {
                self.hi = Some(row);
                self.reveal(row);
                return true;
            }
        }
        false
    }
}

/// The room a bubble keeps to the window's edges.
const WINDOW_MARGIN: f64 = 4.0;
/// The scroll hints' height at a cut bubble's top and bottom edge.
const EDGE_H: f64 = 16.0;

/// Clamp a bubble of `size` into `win`.
fn clamp_into(mut pos: Vec2d, size: Vec2d, win: Rect) -> Vec2d {
    let max_x = (win.pos.x + win.size.x - size.x - 4.0).max(win.pos.x + 4.0);
    let max_y = (win.pos.y + win.size.y - size.y - 4.0).max(win.pos.y + 4.0);
    pos.x = pos.x.clamp(win.pos.x + 4.0, max_x);
    pos.y = pos.y.clamp(win.pos.y + 4.0, max_y);
    pos
}

// ===========================================================================
// The layer
// ===========================================================================

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    set_type_default() do #(DrawMenuRow::script_shader(vm)){
        ..mod.draw.DrawQuad

        hover: 0.0
        down: 0.0
        mark: 0.0
        arrow: 0.0
        disabled: 0.0
        swatch: vec4(0.0, 0.0, 0.0, 0.0)
        edge: 0.0
        current: 0.0
        fade: 1.0

        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let cy = self.rect_size.y * 0.5
            if self.edge > 0.5 {
                // A scroll hint at a cut bubble's edge: the rows fade out under a chevron.
                let t = if self.edge > 1.5 { self.pos.y } else { 1.0 - self.pos.y }
                let fade = t * t * 0.95
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 0.0)
                sdf.fill(vec4(fab.color_popover.xyz, fade))
                let cx = self.rect_size.x * 0.5
                let dir = if self.edge > 1.5 { 1.0 } else { -1.0 }
                let ty = cy + dir * 3.0
                sdf.move_to(cx - 3.5, ty - dir * 2.0)
                sdf.line_to(cx, ty + dir * 1.5)
                sdf.line_to(cx + 3.5, ty - dir * 2.0)
                sdf.stroke(fab.color_text_dim, 1.3)
                return sdf.result * self.fade
            }
            sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, fab.radius)
            if self.current > 0.0 {
                // the current choice in the accent, under the hover fill (which differs from it)
                sdf.fill_keep(vec4(fab.color_accent.xyz, 0.35 * self.current))
            }
            let hot = self.hover * (1.0 - self.disabled)
            sdf.fill(vec4(fab.color_menu_row_hover.xyz, hot).mix(vec4(fab.color_accent_dim.xyz, hot), self.down))
            if self.swatch.w > 0.75 {
                // A colour chip in the swatch column (`MARK_COL`, `SWATCH` wide).
                sdf.box(24.5, cy - 6.5, 13.0, 13.0, 3.0)
                sdf.fill_keep(vec4(self.swatch.xyz, 1.0))
                sdf.stroke(fab.color_popover_border, 1.0)
            } else if self.swatch.w > 0.25 {
                // Nothing loaded: the chip's outline only.
                sdf.box(25.0, cy - 6.0, 12.0, 12.0, 3.0)
                sdf.stroke(vec4(fab.color_text_muted.xyz, 0.7), 1.0)
            }
            if self.mark > 1.5 {
                sdf.circle(12.0, cy, 2.6)
                sdf.fill(fab.color_text)
            } else {
                if self.mark > 0.5 {
                    sdf.move_to(7.5, cy + 0.5)
                    sdf.line_to(10.5, cy + 3.5)
                    sdf.line_to(16.0, cy - 3.5)
                    sdf.stroke(fab.color_text, 1.5)
                }
            }
            if self.arrow < -0.5 {
                // a grid menu's way back (`back_id`)
                let bx = 14.0
                sdf.move_to(bx + 1.75, cy - 3.5)
                sdf.line_to(bx - 1.75, cy)
                sdf.line_to(bx + 1.75, cy + 3.5)
                sdf.stroke(fab.color_text_dim, 1.2)
            }
            if self.arrow > 0.5 {
                let ax = self.rect_size.x - 12.0
                sdf.move_to(ax, cy - 3.5)
                sdf.line_to(ax + 3.5, cy)
                sdf.line_to(ax, cy + 3.5)
                sdf.stroke(fab.color_text_dim, 1.2)
            }
            return sdf.result * self.fade
        }
    }

    mod.widgets.FabMenuLayerBase = #(FabMenuLayer::register_widget(vm))
    mod.widgets.FabMenuLayer = set_type_default() do mod.widgets.FabMenuLayerBase{
        width: Fill
        height: Fill
        // the floating cards' ground and shadow; `fade` is a closing menu's (`FabMenuLayer::fading`)
        draw_bg: mod.draw.FabFloatBg{ color: uniform(fab.color_popover) border_color: uniform(fab.color_popover_border) }
        label_color: fab.color_text
        shortcut_color: fab.color_text_dim
        disabled_color: fab.color_text_muted
        draw_row +: { }
        // a grid menu's tile icon (`tile_icon`)
        TileIcon := mod.widgets.FabIcon{
            width: 22 height: 22
            icon_walk: Walk{ width: 22 height: 22 }
        }
        draw_sep +: {
            color: fab.color_menu_sep
        }
        draw_label +: {
            color: fab.color_text
            ink_centered: true
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
        draw_shortcut +: {
            color: fab.color_text_dim
            ink_centered: true
            text_style: theme.font_regular{
                font_size: fab.font_size_small
            }
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMenuRow {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    hover: f32,
    #[live]
    down: f32,
    /// 0 none, 1 check, 2 radio dot.
    #[live]
    mark: f32,
    #[live]
    arrow: f32,
    #[live]
    disabled: f32,
    /// A colour chip in the mark column (alpha 0: none).
    #[live]
    swatch: Vec4f,
    /// 0 a row; 1 / 2 the scroll hint at the bubble's top / bottom edge.
    #[live]
    edge: f32,
    #[live]
    current: f32,
    /// A closing menu's fade: 1 drawn as it is, 0 gone.
    #[live]
    fade: f32,
}

struct PressOpen {
    origin: Vec2d,
    anchor: Rect,
    travelled: bool,
    hovered: Option<(usize, usize)>,
}

impl PressOpen {
    fn moved(&mut self, at: Vec2d) {
        self.travelled |= (at - self.origin).length() >= makepad_platform::event::finger::TAP_COUNT_DISTANCE;
    }
}

#[derive(Script, Widget)]
pub struct FabMenuLayer {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[live]
    draw_list: DrawList2d,
    #[redraw]
    #[live]
    draw_bg: DrawQuad,
    #[live]
    draw_row: DrawMenuRow,
    #[live]
    draw_sep: DrawColor,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_shortcut: DrawText,
    #[walk]
    walk: Walk,
    // Semantics follow the layout area, which is renewed even with no open
    // levels. The last painted background belongs to a cached overlay and
    // would keep closed menu rows alive until that overlay is painted again.
    #[area]
    #[rust]
    area: Area,
    #[rust]
    levels: Vec<Level>,
    /// The menus just closed: drawn fading out for `FADE_OUT`, deaf to input and unseen by the
    /// semantics, then dropped. Their fade (1 to 0), its start and its frames.
    #[rust]
    fading: Vec<Level>,
    #[rust]
    fade_alpha: f32,
    #[rust]
    fade_from: Option<f64>,
    #[rust]
    fade_frame: NextFrame,
    #[live]
    label_color: Vec4f,
    #[live]
    shortcut_color: Vec4f,
    #[live]
    disabled_color: Vec4f,
    /// Window rect from the last draw — placement clamps against it.
    #[rust]
    window: Rect,
    /// True while this layer holds `cx.sweep_lock`, which is what stops a
    /// click on a menu row from *also* reaching the control underneath it.
    #[rust]
    locked: bool,
    /// A click-away closed the menu on the press; eat the matching release so
    /// the dismissing click does not act on what was under the menu.
    #[rust]
    swallow_up: bool,
    #[rust]
    menu_bar: bool,
    /// The opening title's primary press, retained across menu-bar hover switches.
    #[rust]
    press_open: Option<PressOpen>,
    /// The pointer's last place over the menus: a move from there toward an open flyout
    /// crosses the rows between without switching to them.
    #[rust]
    last_pointer: Option<Vec2d>,
    /// One pending row for both submenu opening and travel toward an existing flyout.
    #[rust]
    pending_row: Option<(usize, usize)>,
    /// The frames that time the pending row's rest, the rest so far (s) and the last one's time.
    #[rust]
    rest_frame: NextFrame,
    #[rust]
    rest: f64,
    #[rust]
    rest_frame_at: Option<f64>,
    /// The single source of truth for "which popup is open" — every root
    /// open/close funnels through [`Self::set_open`] and is broadcast for
    /// the popup buttons and the menu bar to mirror.
    #[rust]
    popup: OpenPopup,
    /// A grid menu's tile icon (the `TileIcon` template) and the icons made from it, one per
    /// file and enabled state.
    /// The frames of a grid menu's slide (`Morph`).
    #[rust]
    morph_frame: NextFrame,
    #[rust]
    tile_template: Option<ScriptObjectRef>,
    #[rust]
    tile_icons: HashMap<(&'static str, bool), WidgetRef>,
}

impl FabMenuLayer {
    /// Draw a grid menu's tiles, their boxes laid out from `origin`, skipping those outside
    /// `view`: each tile's ground, icon and label, and a group's way back as a row. O(tiles).
    #[allow(clippy::too_many_arguments)]
    fn draw_tiles(&mut self, cx: &mut Cx2d, items: &[MenuItem], boxes: &[Rect], origin: Vec2d, view: Rect, hi: Option<usize>, press: Option<usize>, label_on: Vec4f, disabled: Vec4f) {
        for (i, item) in items.iter().enumerate() {
            let r = boxes[i].translate(origin);
            if r.pos.y + r.size.y < view.pos.y || r.pos.y > view.pos.y + view.size.y || r.pos.x > view.pos.x + view.size.x || r.pos.x + r.size.x < view.pos.x {
                continue;
            }
            if item.separator {
                self.draw_sep.draw_abs(cx, separator_line(r));
                continue;
            }
            let back = item.id == back_id();
            // the tile's ground is the row shader's, without the row's text
            let row = &mut self.draw_row;
            row.hover = if hi == Some(i) && item.enabled { 1.0 } else { 0.0 };
            row.down = if press == Some(i) && item.enabled { 1.0 } else { 0.0 };
            row.current = if item.current { 1.0 } else { 0.0 };
            row.disabled = if item.enabled { 0.0 } else { 1.0 };
            row.mark = 0.0;
            row.arrow = if back { -1.0 } else if item.submenu.is_empty() { 0.0 } else { 1.0 };
            row.swatch = vec4(0.0, 0.0, 0.0, 0.0);
            row.draw_abs(cx, r);
            row.arrow = 0.0;
            if back {
                self.draw_label.color = label_on;
                let text = Rect { pos: dvec2(r.pos.x + MARK_COL, r.pos.y), size: dvec2(r.size.x - MARK_COL - RIGHT_PAD, r.size.y) };
                self.draw_label.draw_walk(cx, Walk::abs_rect(text), Align { x: 0.0, y: 0.5 }, &item.label);
                continue;
            }
            if let Some(icon) = item.svg.and_then(|svg| tile_icon(cx, &mut self.tile_icons, self.tile_template.as_ref(), svg, item.enabled)) {
                let at = Rect { pos: dvec2(r.pos.x + (r.size.x - TILE_ICON) * 0.5, r.pos.y + TILE_ICON_TOP), size: dvec2(TILE_ICON, TILE_ICON) };
                let _ = icon.draw_walk(cx, &mut Scope::empty(), Walk::abs_rect(at));
            }
            self.draw_shortcut.color = if item.enabled { label_on } else { disabled };
            let band = Rect { pos: dvec2(r.pos.x + 4.0, r.pos.y + TILE_LABEL_TOP), size: dvec2(r.size.x - 8.0, r.size.y - TILE_LABEL_TOP - 6.0) };
            self.draw_shortcut.draw_walk(cx, Walk::abs_rect(band), Align { x: 0.5, y: 0.5 }, &item.label);
        }
    }
}

impl ScriptHook for FabMenuLayer {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if kv.key.as_id() == Some(live_id!(TileIcon)) {
                            if let Some(t) = kv.value.as_object() {
                                self.tile_template = Some(vm.new_object_ref(t));
                            }
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            self.tile_icons.clear();
        }
    }
}

/// The tile icon for `svg`, made from the template on first use; a disabled tile's is dimmed.
fn tile_icon(cx: &mut Cx, icons: &mut HashMap<(&'static str, bool), WidgetRef>, template: Option<&ScriptObjectRef>, svg: &'static str, enabled: bool) -> Option<WidgetRef> {
    if let Some(w) = icons.get(&(svg, enabled)) {
        return Some(w.clone());
    }
    let value: ScriptValue = template?.as_object().into();
    let mut w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
    let opacity = if enabled { 1.0 } else { 0.35 };
    script_apply_eval!(cx, w, { draw_icon +: { svg: mod.res.crate_resource(#(svg)) opacity: #(opacity) } });
    icons.insert((svg, enabled), w.clone());
    Some(w)
}

impl FabMenuLayer {
    fn owner(&self) -> LiveId {
        self.levels.first().map(|l| l.owner).unwrap_or(LiveId(0))
    }

    fn redraw_menus(&mut self, cx: &mut Cx) {
        self.draw_list.redraw(cx);
        self.area.redraw(cx);
    }

    /// Take the pointer for as long as a menu is up.
    ///
    /// Without this an open menu is only *drawn* on top: the press that picks
    /// a row travels on down the tree and every widget under the bubble that
    /// hit-tests normally answers it too — picking a row also pressed the
    /// button that happened to sit under it. `sweep_lock` makes `Event::hits` answer nothing outside this
    /// layer, which is exactly the modal grab a menu needs.
    fn lock_input(&mut self, cx: &mut Cx) {
        if !self.locked {
            cx.sweep_lock(self.area);
            self.locked = true;
        }
    }

    fn unlock_input(&mut self, cx: &mut Cx) {
        if self.locked {
            cx.sweep_unlock(self.area);
            self.locked = false;
        }
    }

    /// Drop every level. Each close path funnels through here, so the
    /// broadcast can never miss one. Returns the owner that was up.
    pub fn close(&mut self, cx: &mut Cx) -> Option<LiveId> {
        if self.levels.is_empty() {
            return None;
        }
        let owner = self.owner();
        self.swallow_up |= self.press_open.take().is_some()
            || self.levels.iter().any(|level| level.press.is_some());
        self.cancel_pending();
        // what was up fades out (a menu that was fading already goes)
        self.fading = std::mem::take(&mut self.levels);
        for level in &mut self.fading {
            level.press = None;
        }
        self.fade_alpha = 1.0;
        self.fade_from = None;
        self.fade_frame = cx.new_next_frame();
        self.last_pointer = None;
        if !self.swallow_up {
            self.unlock_input(cx);
        }
        self.redraw_menus(cx);
        self.set_open(cx, None);
        Some(owner)
    }

    /// Every change of "which popup is open" goes through the tracker and
    /// out as `MenuClosed` / `MenuOpened`, close before open, for the popup
    /// buttons and the menu bar to mirror. The modal grab also kept every
    /// hover-out from arriving while a menu was up, so any transition ends
    /// by clearing hover/press visuals tree-wide; the next pointer move
    /// re-hovers whatever really is under the pointer.
    fn set_open(&mut self, cx: &mut Cx, next: Option<LiveId>) {
        let changes = self.popup.set(next);
        if changes.is_empty() {
            return;
        }
        for change in changes {
            match change {
                PopupChange::Closed(owner) => cx.action(FabUiAction::MenuClosed { owner }),
                PopupChange::Opened(owner) => cx.action(FabUiAction::MenuOpened { owner, menu_bar: self.menu_bar }),
            }
        }
        cx.clear_all_hovers();
    }

    /// Rebuild the position from its request after sizing; ancestors are placed first.
    fn position_level(&mut self, level: usize) {
        // A list taller than the window is cut to it and scrolls; one that fits is untouched.
        let room = (self.window.size.y - WINDOW_MARGIN * 2.0).max(ROW_H);
        let full = self.levels[level].content_h();
        self.levels[level].rect.size.y = full.min(room);
        let max = self.levels[level].max_scroll();
        self.levels[level].scroll = self.levels[level].scroll.min(max);
        if let Some(m) = &self.levels[level].morph {
            // a grid menu sliding to other tiles grows or shrinks to them as it goes
            let e = m.eased();
            let to = self.levels[level].rect.size;
            self.levels[level].rect.size = m.from_size + (to - m.from_size) * e;
        }
        let size = self.levels[level].rect.size;
        let pos = match self.levels[level].placement {
            Placement::Root { anchor, place } => match place {
                MenuPlace::Below => dvec2(anchor.pos.x, anchor.pos.y + anchor.size.y + 2.0),
                MenuPlace::BelowRight => dvec2(
                    anchor.pos.x + anchor.size.x - size.x,
                    anchor.pos.y + anchor.size.y + 2.0,
                ),
                MenuPlace::At => anchor.pos,
            },
            Placement::Flyout { parent, row } => {
                let parent_rect = self.levels[parent].rect;
                let anchor = self.levels[parent].row_rect(row);
                let mut pos = dvec2(parent_rect.pos.x + parent_rect.size.x, anchor.pos.y - MENU_PAD);
                if pos.x + size.x > self.window.pos.x + self.window.size.x - 4.0 {
                    pos.x = parent_rect.pos.x - size.x;
                }
                pos
            }
        };
        self.levels[level].rect.pos = clamp_into(pos, size, self.window);
    }

    /// Estimate and place all root rows once per open, with columns wrapped to the window.
    fn open_root(&mut self, cx: &mut Cx, owner: LiveId, items: Vec<MenuItem>, anchor: Rect, place: MenuPlace, menu_bar: bool, press: Option<Vec2d>) {
        if items.is_empty() {
            return;
        }
        if self.window.size.x <= 1.0 {
            self.window = Rect { pos: dvec2(0.0, 0.0), size: cx.default_window_size() };
        }
        self.cancel_pending();
        let continuing = if menu_bar && self.menu_bar { self.press_open.take() } else { None };
        self.press_open = press.map(|origin| PressOpen { origin, anchor, travelled: false, hovered: None }).or(continuing);
        if let Some(tracking) = &mut self.press_open {
            tracking.anchor = anchor;
            tracking.hovered = None;
            // A re-press on a title owns the dismissing press's release.
            self.swallow_up = false;
        }
        self.menu_bar = menu_bar;
        self.levels.clear();
        let room_w = self.window.size.x - WINDOW_MARGIN * 2.0;
        self.levels.push(Level::new_in(owner, items, Placement::Root { anchor, place }, room_w));
        self.position_level(0);
        self.last_pointer = None;
        self.lock_input(cx);
        self.redraw_menus(cx);
        self.set_open(cx, Some(owner));
    }

    /// Size the submenu rows once per flyout open. An open flyout needs no work.
    fn open_flyout(&mut self, cx: &mut Cx, level: usize, row: usize) {
        if self.levels.get(level + 1).is_some_and(|fly| fly.from_row() == Some(row)) {
            return;
        }
        let parent = &self.levels[level];
        let Some(item) = parent.items.get(row) else { return };
        if item.submenu.is_empty() || !parent.selectable(row) {
            return;
        }
        let room_w = self.window.size.x - WINDOW_MARGIN * 2.0;
        let flyout = Level::new_in(parent.owner, item.submenu.clone(), Placement::Flyout { parent: level, row }, room_w);
        if let Some(tracking) = &mut self.press_open {
            if tracking.hovered.is_some_and(|(li, _)| li > level) {
                tracking.hovered = None;
            }
        }
        self.levels.truncate(level + 1);
        self.levels.push(flyout);
        self.position_level(level + 1);
        self.redraw_menus(cx);
    }

    /// The pointer moved from `from` to `to`, now over `row` of `level`, on its way into the
    /// flyout that level has open from another row: `to` lies in the triangle between `from`
    /// and the flyout's near edge.
    fn aiming_at_flyout(&self, level: usize, row: usize, from: Vec2d, to: Vec2d) -> bool {
        let Some(fly) = self.levels.get(level + 1) else { return false };
        if fly.from_row() == Some(row) || fly.from_row().is_none() {
            return false;
        }
        let r = fly.rect;
        let right = r.pos.x >= self.levels[level].rect.pos.x;
        let x = if right { r.pos.x } else { r.pos.x + r.size.x };
        let (a, b) = (dvec2(x, r.pos.y), dvec2(x, r.pos.y + r.size.y));
        let side = |p: Vec2d, q: Vec2d, t: Vec2d| (q.x - p.x) * (t.y - p.y) - (q.y - p.y) * (t.x - p.x);
        let (d1, d2, d3) = (side(from, a, to), side(a, b, to), side(b, from, to));
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos) && (to.x - from.x).abs() > 0.0
    }

    fn cancel_pending(&mut self) {
        self.pending_row = None;
    }

    /// Row `row` of `level` takes over once the pointer has rested on it for `REST` s.
    fn start_rest(&mut self, cx: &mut Cx, level: usize, row: usize) {
        self.pending_row = Some((level, row));
        self.rest = 0.0;
        self.rest_frame_at = None;
        self.rest_frame = cx.new_next_frame();
    }

    /// A frame of the pending row's rest: the rest grows by the time since the last frame, but by
    /// one `HITCH` at most. An app that stalls shows the person nothing and holds their moves back,
    /// so its stall is no rest on a row they were crossing.
    fn rest_tick(&mut self, cx: &mut Cx, time: f64) {
        let Some((li, row)) = self.pending_row else { return };
        if let Some(at) = self.rest_frame_at {
            self.rest += (time - at).clamp(0.0, HITCH);
        }
        self.rest_frame_at = Some(time);
        if self.rest < REST {
            self.rest_frame = cx.new_next_frame();
            return;
        }
        self.pending_row = None;
        if self.last_pointer.and_then(|p| self.hit(p)) == Some((li, row)) {
            self.point_at(cx, li, row);
        }
    }

    /// Headings carry no highlight. O(1) per pointer update.
    fn highlight(&mut self, cx: &mut Cx, level: usize, row: Option<usize>) {
        let row = row.filter(|&i| !self.levels[level].items[i].heading);
        if self.levels[level].hi != row {
            self.levels[level].hi = row;
            self.redraw_menus(cx);
        }
    }

    /// Commit a row after its hover delay, or immediately for a press or key.
    fn point_at(&mut self, cx: &mut Cx, level: usize, row: usize) {
        self.cancel_pending();
        self.highlight(cx, level, Some(row));
        if self.levels[level].selectable(row) && !self.levels[level].items[row].submenu.is_empty() && !self.drills(level, row) {
            self.open_flyout(cx, level, row);
        } else if self.levels.len() > level + 1 {
            self.levels.truncate(level + 1);
            self.redraw_menus(cx);
        }
    }

    /// The row under the pointer takes over at once: its highlight, and its flyout opens (or
    /// the open one closes). Only a move heading into the open flyout, across the rows between
    /// (its aim triangle), keeps that flyout; the row there takes over if the pointer rests on it.
    fn hover_row(&mut self, cx: &mut Cx, level: usize, row: usize, aiming: bool) {
        let flyout_row = self.levels.get(level + 1).and_then(Level::from_row);
        if flyout_row == Some(row) {
            self.cancel_pending();
            self.highlight(cx, level, Some(row));
        } else if aiming {
            self.start_rest(cx, level, row);
        } else {
            let opens = self.levels[level].selectable(row) && !self.levels[level].items[row].submenu.is_empty() && !self.drills(level, row);
            if self.levels[level].hi != Some(row) || flyout_row.is_some() || opens {
                self.point_at(cx, level, row);
            }
        }
    }

    /// Pick a selectable row or enter its flyout, sizing the child rows on open.
    fn activate(&mut self, cx: &mut Cx, level: usize, row: usize) -> bool {
        if !self.levels[level].selectable(row) {
            return false;
        }
        let id = self.levels[level].items[row].id;
        if id == back_id() {
            self.back(cx, level);
            return false;
        }
        if self.drills(level, row) {
            self.drill(cx, level, row);
            return false;
        }
        let opens = !self.levels[level].items[row].submenu.is_empty();
        self.point_at(cx, level, row);
        if opens {
            let last = self.levels.len() - 1;
            self.levels[last].hi = self.levels[last].step(None, 1);
            if let Some(hi) = self.levels[last].hi {
                self.levels[last].reveal(hi);
            }
            self.redraw_menus(cx);
            return false;
        }
        let owner = self.owner();
        self.close(cx);
        cx.action(FabUiAction::MenuPicked { owner, id });
        true
    }

    /// True when a grid tile's group opens in place of the tiles instead of in a flyout: its
    /// rows are tiles too.
    fn drills(&self, level: usize, row: usize) -> bool {
        let lv = &self.levels[level];
        lv.grid && lv.items.get(row).is_some_and(|it| is_grid(&it.submenu))
    }

    /// Open tile `row`'s group in place: its tiles slide in, under a row back to these.
    fn drill(&mut self, cx: &mut Cx, level: usize, row: usize) {
        let item = self.levels[level].items[row].clone();
        let mut items = vec![MenuItem::new(back_id(), &item.label)];
        items.extend(item.submenu);
        let from = self.morph_to(cx, level, items, 1.0);
        self.levels[level].trail.push((from, row));
    }

    /// Slide back to the tiles the shown group was picked from, that tile highlighted.
    fn back(&mut self, cx: &mut Cx, level: usize) {
        let Some((items, row)) = self.levels[level].trail.pop() else { return };
        self.morph_to(cx, level, items, -1.0);
        self.levels[level].hi = Some(row);
    }

    /// Show `items` in `level` in place of its tiles, sliding `dir`; returns the tiles it had.
    fn morph_to(&mut self, cx: &mut Cx, level: usize, items: Vec<MenuItem>, dir: f64) -> Vec<MenuItem> {
        self.cancel_pending();
        self.levels.truncate(level + 1);
        let room_w = self.window.size.x - WINDOW_MARGIN * 2.0;
        let lv = &mut self.levels[level];
        let from_size = lv.rect.size;
        let boxes = std::mem::take(&mut lv.row_boxes);
        let old = std::mem::replace(&mut lv.items, items);
        let n = lv.items.len();
        lv.regions = vec![RegionRows { region: MenuRegion::Main, rows: 0..n, tag_w: 0.0 }];
        lv.row_boxes = vec![Rect::default(); n];
        lv.grid = is_grid(&lv.items);
        lv.hi = None;
        lv.press = None;
        lv.scroll = 0.0;
        lv.size_regions(room_w, estimate_region);
        lv.morph = Some(Morph { items: old.clone(), boxes, from_size, dir, t: 0.0, start: None });
        self.morph_frame = cx.new_next_frame();
        self.position_level(level);
        self.redraw_menus(cx);
        old
    }

    fn hit(&self, p: Vec2d) -> Option<(usize, usize)> {
        for li in (0..self.levels.len()).rev() {
            if self.levels[li].rect.contains(p) {
                return self.levels[li].row_at(p).map(|row| (li, row));
            }
        }
        None
    }

    fn inside_any(&self, p: Vec2d) -> Option<usize> {
        (0..self.levels.len()).rev().find(|li| self.levels[*li].rect.contains(p))
    }

    /// Navigate the deepest menu. Moving the highlight costs O(rows) per key.
    fn key_down(&mut self, cx: &mut Cx, key: KeyCode) {
        self.cancel_pending();
        if let Some(tracking) = &mut self.press_open {
            tracking.hovered = None;
        }
        let last = self.levels.len() - 1;
        match key {
            KeyCode::Escape => {
                if last > 0 {
                    self.levels.truncate(last);
                    self.redraw_menus(cx);
                } else {
                    self.close(cx);
                }
            }
            KeyCode::ArrowDown | KeyCode::ArrowUp => {
                let dir = if key == KeyCode::ArrowDown { 1 } else { -1 };
                let from = self.levels[last].hi;
                self.levels[last].hi = if self.levels[last].grid {
                    self.levels[last].step_vertical(from, dir)
                } else {
                    self.levels[last].step(from, dir)
                };
                if let Some(hi) = self.levels[last].hi {
                    self.levels[last].reveal(hi);
                }
                self.levels.truncate(last + 1);
                self.redraw_menus(cx);
            }
            KeyCode::ArrowRight => {
                let hi = self.levels[last].hi;
                let has_sub = hi
                    .and_then(|i| self.levels[last].items.get(i))
                    .map_or(false, |it| !it.submenu.is_empty() && it.enabled);
                if self.levels[last].grid {
                    // tiles: right moves along the grid; Return opens a tile's flyout
                    self.levels[last].hi = self.levels[last].step(hi, 1);
                    self.levels.truncate(last + 1);
                    self.redraw_menus(cx);
                } else if has_sub {
                    self.activate(cx, last, hi.unwrap());
                } else if self.levels[last].step_region(1) {
                    self.redraw_menus(cx);
                }
            }
            KeyCode::ArrowLeft => {
                let hi = self.levels[last].hi;
                let first_col = hi.is_none_or(|i| self.levels[last].row_boxes[i].pos.x <= MENU_PAD + 0.5);
                if self.levels[last].grid && first_col && !self.levels[last].trail.is_empty() {
                    self.back(cx, last);
                } else if self.levels[last].grid && !(first_col && last > 0) {
                    self.levels[last].hi = self.levels[last].step(hi, -1);
                    self.levels.truncate(last + 1);
                    self.redraw_menus(cx);
                } else if last > 0 {
                    self.levels.truncate(last);
                    self.redraw_menus(cx);
                } else if self.levels[last].step_region(-1) {
                    self.redraw_menus(cx);
                }
            }
            KeyCode::Backspace if !self.levels[last].trail.is_empty() => {
                self.back(cx, last);
            }
            KeyCode::ReturnKey | KeyCode::NumpadEnter => {
                if let Some(i) = self.levels[last].hi {
                    self.activate(cx, last, i);
                }
            }
            _ => {}
        }
    }
}

impl Widget for FabMenuLayer {
    /// Publish one node per title or menu item in each open level. O(rows and text).
    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (li, level) in self.levels.iter().enumerate() {
            let menu_key = ((li as u64) << 32) | 0xffff_ffff;
            out.push((menu_key, None, a11y_tree::Node {
                role: a11y_tree::Role::Menu,
                bounds: makepad_platform::a11y::semantic_rect(level.rect),
                set_size: level.items.iter().filter(|i| !i.separator && !i.heading).count() as u32,
                ..Default::default()
            }));
            let mut pos = 0;
            for (ri, item) in level.items.iter().enumerate() {
                if item.separator {
                    continue;
                }
                if item.heading {
                    let mut node = a11y_tree::Node {
                        role: a11y_tree::Role::Heading,
                        bounds: makepad_platform::a11y::semantic_rect(level.row_rect(ri)),
                        ..Default::default()
                    };
                    makepad_platform::a11y::set_string(&mut node.name, &item.label);
                    out.push((((li as u64) << 32) | (ri as u64 + 1), Some(menu_key), node));
                    continue;
                }
                pos += 1;
                let mut node = a11y_tree::Node {
                    role: a11y_tree::Role::MenuItem,
                    bounds: makepad_platform::a11y::semantic_rect(level.row_rect(ri)),
                    pos_in_set: pos,
                    key_shortcut: item.shortcut.clone(),
                    actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::FOCUS,
                    ..Default::default()
                };
                // A tray row reads as it shows: its id, its label, its note ("B2 PLA Orange").
                let name = [item.tag.as_str(), item.label.as_str(), item.note.as_str()].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join(" ");
                makepad_platform::a11y::set_string(&mut node.name, &name);
                node.states.set(a11y_tree::States::DISABLED, !item.enabled);
                node.states.set(a11y_tree::States::HAS_POPUP, !item.submenu.is_empty());
                node.states.set(a11y_tree::States::SELECTED, level.hi == Some(ri));
                node.states.set(a11y_tree::States::ACTIVE, item.current);
                out.push((((li as u64) << 32) | (ri as u64 + 1), Some(menu_key), node));
            }
        }
    }

    /// Look up an accessible row in O(1), then reveal or activate it.
    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let level = (key >> 32) as usize;
        let Some(row) = (key as u32).checked_sub(1).map(|r| r as usize) else { return ActionResult::Unsupported };
        let Some(item) = self.levels.get(level).and_then(|l| l.items.get(row)) else { return ActionResult::Unsupported };
        if !item.enabled || item.heading {
            return ActionResult::Disabled;
        }
        match request.action {
            Actions::PRESS => {
                self.activate(cx, level, row);
                ActionResult::Handled
            }
            Actions::FOCUS => {
                self.cancel_pending();
                self.levels[level].hi = Some(row);
                self.levels[level].reveal(row);
                self.draw_list.redraw(cx);
                ActionResult::Handled
            }
            _ => ActionResult::Unhandled,
        }
    }

    /// Measure and lay out each open level once per draw, then paint its visible rows.
    /// The work scales with the open rows and their text.
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, Layout::default());
        let window = cx.turtle().rect();
        cx.end_turtle_with_area(&mut self.area);
        self.window = window;
        if self.levels.is_empty() && self.fading.is_empty() {
            return DrawStep::done();
        }

        let label_on = self.label_color;
        let side_on = self.shortcut_color;
        let disabled = self.disabled_color;
        let room_w = window.size.x - WINDOW_MARGIN * 2.0;
        for li in 0..self.levels.len() {
            let ink = RowInk { row: &mut self.draw_row, label: &mut self.draw_label, side: &mut self.draw_shortcut, label_on, side_on, disabled };
            self.levels[li].size_regions(room_w, |items| {
                let tag_w = ink.tag_w(cx, items);
                let lefts = label_offsets(items, tag_w);
                let width = ink.need(cx, items, &lefts);
                (tag_w, lefts, width)
            });
            self.position_level(li);
        }

        self.draw_list.begin_overlay_reuse(cx);
        let pass = cx.current_pass_size();
        cx.begin_root_turtle(pass, Layout::flow_down());

        // the closing menus under the open ones, each drawn at its fade
        let fading = std::mem::take(&mut self.fading);
        let live = std::mem::take(&mut self.levels);
        let base_sep = self.draw_sep.color;
        for (levels, a) in [(&fading, self.fade_alpha), (&live, 1.0)] {
        let faded = |c: Vec4f| vec4(c.x, c.y, c.z, c.w * a);
        let (label_on, side_on, disabled) = (faded(label_on), faded(side_on), faded(disabled));
        self.draw_row.fade = a;
        self.draw_bg.set_dyn_instance(cx, live_id!(fade), &[a]);
        self.draw_sep.color = faded(base_sep);
        for level in levels {
            let rect = level.rect;
            self.draw_bg.draw_abs(cx, rect);
            let max_scroll = level.max_scroll();
            let cut = max_scroll > 0.0;
            if cut {
                cx.push_clip_rect(Rect { pos: dvec2(rect.pos.x, rect.pos.y + 1.0), size: dvec2(rect.size.x, rect.size.y - 2.0) });
            }
            if let Some(rule) = level.footer_rule {
                let r = rule.translate(rect.pos - dvec2(0.0, level.scroll));
                self.draw_sep.draw_abs(cx, separator_line(r));
            }
            if level.grid {
                let origin = rect.pos - dvec2(0.0, level.scroll);
                if let Some(m) = &level.morph {
                    // the old tiles slide out one way as the new ones slide in after them
                    let e = m.eased();
                    let w = rect.size.x.max(m.from_size.x);
                    cx.push_clip_rect(Rect { pos: dvec2(rect.pos.x + 1.0, rect.pos.y + 1.0), size: dvec2(rect.size.x - 2.0, rect.size.y - 2.0) });
                    self.draw_tiles(cx, &m.items, &m.boxes, origin - dvec2(m.dir * e * w, 0.0), rect, None, None, label_on, disabled);
                    self.draw_tiles(cx, &level.items, &level.row_boxes, origin + dvec2(m.dir * (1.0 - e) * w, 0.0), rect, None, None, label_on, disabled);
                    cx.pop_clip_rect();
                } else {
                    self.draw_tiles(cx, &level.items, &level.row_boxes, origin, rect, level.hi, level.press, label_on, disabled);
                }
                self.draw_shortcut.color = side_on;
            } else {
            let mut ink = RowInk { row: &mut self.draw_row, label: &mut self.draw_label, side: &mut self.draw_shortcut, label_on, side_on, disabled };
            for group in &level.regions {
                for i in group.rows.clone() {
                    let item = &level.items[i];
                    let r = level.row_rect(i);
                    if r.pos.y + r.size.y < rect.pos.y || r.pos.y > rect.pos.y + rect.size.y {
                        continue;
                    }
                    if item.separator {
                        self.draw_sep.draw_abs(cx, separator_line(r));
                    } else if item.heading {
                        ink.paint_heading(cx, r, &item.label, &mut self.draw_sep);
                    } else {
                        let hover = if level.hi == Some(i) && level.selectable(i) { 1.0 } else { 0.0 };
                        let down = level.press == Some(i);
                        ink.paint(cx, r, &level.items, i, group.tag_w, level.label_offsets[i], hover, down);
                    }
                }
            }
            }
            if cut {
                cx.pop_clip_rect();
                // Fades with a chevron at the edge that has more behind it.
                let scroll = level.scroll;
                for (edge, on) in [(1.0, scroll > 0.5), (2.0, scroll < max_scroll - 0.5)] {
                    if on {
                        let y = if edge < 1.5 { rect.pos.y + 1.0 } else { rect.pos.y + rect.size.y - 1.0 - EDGE_H };
                        self.draw_row.edge = edge;
                        self.draw_row.hover = 0.0;
                        self.draw_row.down = 0.0;
                        self.draw_row.current = 0.0;
                        self.draw_row.mark = 0.0;
                        self.draw_row.arrow = 0.0;
                        self.draw_row.swatch = vec4(0.0, 0.0, 0.0, 0.0);
                        self.draw_row.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + 3.0, y), size: dvec2(rect.size.x - 6.0, EDGE_H) });
                        self.draw_row.edge = 0.0;
                    }
                }
            }
        }
        }
        self.draw_sep.color = base_sep;
        self.draw_row.fade = 1.0;
        self.fading = fading;
        self.levels = live;

        cx.end_pass_sized_turtle_with_shift(self.area, dvec2(0.0, 0.0) - window.pos);
        self.draw_list.end(cx);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Event::NextFrame(ne) = event {
            if ne.set.contains(&self.morph_frame) {
                let mut running = false;
                for lv in &mut self.levels {
                    if let Some(m) = &mut lv.morph {
                        let start = *m.start.get_or_insert(ne.time);
                        m.t = ((ne.time - start) / MORPH).clamp(0.0, 1.0);
                        if m.t >= 1.0 {
                            lv.morph = None;
                        } else {
                            running = true;
                        }
                    }
                }
                if running {
                    self.morph_frame = cx.new_next_frame();
                }
                self.draw_list.redraw(cx);
            }
            if ne.set.contains(&self.fade_frame) && !self.fading.is_empty() {
                let from = *self.fade_from.get_or_insert(ne.time);
                let t = ((ne.time - from) / FADE_OUT).clamp(0.0, 1.0);
                if t >= 1.0 {
                    self.fading.clear();
                    self.fade_from = None;
                } else {
                    // eased: it leaves softly, then quickly
                    let f = 1.0 - t;
                    self.fade_alpha = (f * f * (3.0 - 2.0 * f)) as f32;
                    self.fade_frame = cx.new_next_frame();
                }
                self.draw_list.redraw(cx);
            }
        }
        if let Event::Actions(actions) = event {
            let mut request = None;
            for a in actions.iter() {
                if let Some(FabUiAction::OpenMenu {
                    owner, items, anchor, place, menu_bar, press,
                }) = a.downcast_ref::<FabUiAction>()
                {
                    request = Some((*owner, items.clone(), *anchor, *place, *menu_bar, *press));
                }
            }
            if let Some((owner, items, anchor, place, menu_bar, press)) = request {
                self.open_root(cx, owner, items, anchor, place, menu_bar, press);
            }
        }
        // A dismissing press owns its release even when another dropdown has opened.
        if let Event::MouseUp(_) = event {
            if self.swallow_up {
                self.swallow_up = false;
                if self.levels.is_empty() {
                    self.unlock_input(cx);
                }
                return;
            }
        }
        if matches!(event, Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            self.close(cx);
            self.press_open = None;
            self.swallow_up = false;
            self.unlock_input(cx);
        }
        if self.levels.is_empty() {
            return;
        }
        match event {
            Event::MouseMove(e) => {
                let hit = self.hit(e.abs);
                if let Some(tracking) = &mut self.press_open {
                    tracking.moved(e.abs);
                    tracking.hovered = hit;
                }
                let from = self.last_pointer.replace(e.abs);
                if let Some((li, row)) = hit {
                    let aiming = from.is_some_and(|f| self.aiming_at_flyout(li, row, f, e.abs));
                    self.hover_row(cx, li, row, aiming);
                } else {
                    self.cancel_pending();
                    // Padding is the bridge into a flyout; only the deepest highlight clears.
                    if let Some(li) = self.inside_any(e.abs).filter(|li| li + 1 == self.levels.len()) {
                        self.highlight(cx, li, None);
                    }
                }
            }
            Event::MouseLeave(_) => {
                self.cancel_pending();
                self.last_pointer = None;
                if let Some(tracking) = &mut self.press_open {
                    tracking.hovered = None;
                }
            }
            Event::Scroll(e) => {
                // The wheel scrolls the deepest bubble under the pointer, when it was cut to fit.
                if let Some(li) = self.inside_any(e.abs) {
                    if self.levels[li].max_scroll() > 0.0 {
                        if self.levels[li].scroll_by(e.scroll.y) {
                            // The rows moved under a still pointer: its row is the one it now rests on.
                            if let Some((hl, row)) = self.hit(e.abs).filter(|(hl, _)| *hl == li) {
                                if self.levels.len() == li + 1 {
                                    self.highlight(cx, hl, Some(row));
                                }
                            }
                            self.redraw_menus(cx);
                        }
                        e.handled_y.set(true);
                    }
                }
            }
            Event::NextFrame(ne) if ne.set.contains(&self.rest_frame) => self.rest_tick(cx, ne.time),
            Event::MouseDown(e) => {
                if let Some((li, row)) = self.hit(e.abs) {
                    if e.button.is_primary() {
                        self.last_pointer = Some(e.abs);
                        self.point_at(cx, li, row);
                        if self.levels[li].selectable(row) {
                            self.levels[li].press = Some(row);
                            self.redraw_menus(cx);
                        }
                    }
                } else if self.inside_any(e.abs).is_none() {
                    // Hold the grab until the release or a menu-bar re-press takes it over.
                    self.swallow_up = true;
                    if self.close(cx).is_some() {
                        cx.action(FabUiAction::MenuClickAway { at: e.abs });
                    }
                }
            }
            Event::MouseUp(e) if e.button.is_primary() => {
                let pressed = self.levels.iter().any(|level| level.press.is_some());
                for level in &mut self.levels {
                    level.press = None;
                }
                self.redraw_menus(cx);
                let hit = self.hit(e.abs);
                if let Some(mut tracking) = self.press_open.take() {
                    tracking.moved(e.abs);
                    if tracking.anchor.contains(e.abs) || !tracking.travelled {
                        return;
                    }
                    if let Some((li, row)) = hit {
                        // Only a row reached by a pointer move belongs to the opening drag.
                        if tracking.hovered == Some((li, row)) {
                            self.activate(cx, li, row);
                        }
                    } else if self.inside_any(e.abs).is_none() {
                        self.close(cx);
                    }
                } else if pressed {
                    if let Some((li, row)) = hit {
                        self.activate(cx, li, row);
                    }
                }
            }
            Event::KeyDown(ke) => {
                self.key_down(cx, ke.key_code);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_level(n: usize, room: f64) -> Level {
        let items = (0..n).map(|i| MenuItem::new(LiveId(i as u64 + 1), "Row")).collect();
        let mut level = Level::new(LiveId(1), items, Placement::Root { anchor: Rect::default(), place: MenuPlace::At });
        level.rect.size.y = level.content_h().min(room);
        level
    }

    #[test]
    fn a_list_that_fits_never_scrolls_and_a_taller_one_reveals_and_hits_the_right_rows() {
        let mut fits = long_level(5, 400.0);
        assert_eq!(fits.max_scroll(), 0.0, "a list that fits keeps its full height and cannot scroll");
        assert!(!fits.scroll_by(50.0), "scrolling a list that fits moves nothing");

        let mut tall = long_level(40, 300.0);
        assert_eq!(tall.rect.size.y, 300.0, "the bubble is cut to the room");
        assert!(tall.max_scroll() > 0.0);
        tall.reveal(39);
        assert_eq!(tall.scroll, tall.max_scroll(), "revealing the last row scrolls to the end");
        let r = tall.row_rect(39);
        assert!(r.pos.y + r.size.y <= tall.rect.pos.y + tall.rect.size.y, "the last row lies inside the bubble");
        assert_eq!(tall.row_at(dvec2(MENU_PAD + 5.0, 300.0 - MENU_PAD - 5.0)), Some(39), "a hit at the bottom finds the last row");
        tall.reveal(0);
        assert_eq!(tall.scroll, 0.0, "revealing the first row scrolls back to the top");
        assert!(!tall.scroll_by(-10.0), "the top is a stop");
    }
}
