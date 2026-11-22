//! The firmware variables, PC and cycle at the playhead.
//! A draw lays out one label per name and one per displayed value line, scanning the
//! words of long values. Unchanged rows request no frames.
//!
//! A block value (an array, or another layout's bytes) folds to one line, its size and as many of
//! its first cells as fit; a click on that line (or an a11y press) opens it as a grid, 16 cells a
//! row under an offset column (fewer when the panel is narrow). The open blocks are kept by
//! variable name, so a block stays open across publications and images. Per draw O(lines shown).

use crate::fw_state::{Block, StateRow};
use makepad_widgets_core::{makepad_platform::a11y, *};
use std::collections::HashSet;

script_mod! {
    use mod.prelude.widgets_internal.*

    mod.widgets.FwRowsBase = #(FwRows::register_widget(vm))
    mod.widgets.FwRows = set_type_default() do mod.widgets.FwRowsBase {
        width: Fill height: Fit
        draw_name +: {color: theme.color_text_meta text_style: theme.font_code {font_size: 8.5}}
        draw_value +: {color: theme.color_text text_style: theme.font_code {font_size: 8.5}}
        color_true: theme.color_success
        color_false: theme.color_error
        row_h: 19
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FwRows {
    #[uid]
    uid: WidgetUid,
    #[source]
    script_source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[rust]
    area: Area,
    #[live]
    draw_name: DrawText,
    #[live]
    draw_value: DrawText,
    #[live]
    color_true: Vec4f,
    #[live]
    color_false: Vec4f,
    #[live]
    row_h: f64,
    #[rust]
    rows: Vec<StateRow>,
    /// The variables whose blocks are open, by name.
    #[rust]
    open: HashSet<String>,
    /// Each block's first line as drawn: (its top from the rows' top, the row).
    #[rust]
    heads: Vec<(f64, usize)>,
}

/// Visits the value lines and returns the height in lines, including the name.
fn row_lines(row: &StateRow, width: f64, cell: f64, mut visit: impl FnMut(usize, &str, f64)) -> usize {
    let text_width = |s: &str| s.chars().count() as f64 * cell;
    let value_width = text_width(&row.value);
    if text_width(&row.name) + 12.0 + value_width <= width {
        visit(0, &row.value, value_width);
        return 1;
    }

    let mut words = row.value.split(' ').map(|word| (word, text_width(word))).peekable();
    let mut start = 0;
    let mut line = 1;
    while let Some((word, mut line_width)) = words.next() {
        let mut end = start + word.len();
        while let Some(&(next, next_width)) = words.peek() {
            if line_width + cell + next_width > width {
                break;
            }
            end += 1 + next.len();
            line_width += cell + next_width;
            words.next();
        }
        visit(line, &row.value[start..end], line_width);
        start = end + 1;
        line += 1;
    }
    line
}

/// An open block's offset column in characters and its cells a line: 16, else the most of 8, 4,
/// 2 or 1 that fit `width`.
fn grid(block: &Block, width: f64, cell: f64) -> (usize, usize) {
    let offset = block.offset(block.len().saturating_sub(1)).len();
    let per = [16, 8, 4, 2].into_iter()
        .find(|per| (offset + 2 + per * (block.width + 1) - 1) as f64 * cell <= width)
        .unwrap_or(1);
    (offset, per)
}

/// The folded line's preview: the first cells that fit `width`, an ellipsis when some do not.
fn preview(block: &Block, width: f64, cell: f64) -> String {
    let fits = |n: usize, more: bool| (n * (block.width + 1) - 1 + if more { 2 } else { 0 }) as f64 * cell <= width;
    let n = if fits(block.len(), false) {
        block.len()
    } else {
        (0..block.len()).rev().find(|&n| n > 0 && fits(n, true)).unwrap_or(0)
    };
    let mut text = (0..n).map(|i| block.cell(i).trim_start()).collect::<Vec<_>>().join(" ");
    if n < block.len() && n > 0 {
        text.push_str(" \u{2026}");
    }
    text
}

impl FwRows {
    /// Replaces the displayed values when they change.
    pub fn set_rows(&mut self, cx: &mut Cx, rows: &[StateRow]) {
        if self.rows != rows {
            rows.clone_into(&mut self.rows);
            self.area.redraw(cx);
        }
    }

    fn cell_size(&self, cx: &mut Cx) -> DVec2 {
        let cell = self.draw_value.layout(cx, 0.0, 0.0, None, false, Align::default(), "0");
        let scale = self.draw_value.font_scale;
        dvec2(f64::from(cell.size_in_lpxs.width * scale), f64::from(cell.size_in_lpxs.height * scale))
    }

    fn lines_of(&self, row: &StateRow, width: f64, cell: f64) -> usize {
        match &row.block {
            Some(block) if self.open.contains(&row.name) => {
                let (_, per) = grid(block, width, cell);
                1 + block.len().div_ceil(per)
            }
            Some(_) => 1,
            None => row_lines(row, width, cell, |_, _, _| {}),
        }
    }

    fn line_count(&self, width: f64, cell: f64) -> usize {
        self.rows.iter().map(|row| self.lines_of(row, width, cell)).sum()
    }

    /// The height of the rows at the given content width.
    pub fn height_for(&self, cx: &mut Cx, width: f64) -> f64 {
        self.line_count(width, self.cell_size(cx).x) as f64 * self.row_h
    }

    /// The block row whose first line is under `abs`.
    fn head_at(&self, cx: &Cx, abs: DVec2) -> Option<usize> {
        let rect = self.area.rect(cx);
        let y = abs.y - rect.pos.y;
        self.heads.iter().find(|(top, _)| y >= *top && y < top + self.row_h).map(|&(_, row)| row)
    }

    fn toggle(&mut self, cx: &mut Cx, row: usize) {
        let Some(name) = self.rows.get(row).map(|r| r.name.clone()) else { return };
        if !self.open.remove(&name) {
            self.open.insert(name);
        }
        self.area.redraw(cx);
    }

    fn part_key(name: &str) -> u64 {
        LiveId::from_str("fw rows").bytes_append(name.as_bytes()).0.max(1)
    }
}

impl Widget for FwRows {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let width = cx.peek_walk_turtle(walk).size.x;
        let cell = self.cell_size(cx);
        let height = self.line_count(width, cell.x) as f64 * self.row_h;
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(height), ..walk });
        cx.add_rect_area(&mut self.area, rect);
        cx.push_clip_rect(rect);
        let normal = self.draw_value.color;
        let inset = (self.row_h - cell.y) * 0.5;
        let mut top = 0.0;
        self.heads.clear();
        for (index, row) in self.rows.iter().enumerate() {
            let y = rect.pos.y + top + inset;
            let lines = match &row.block {
                Some(block) => {
                    self.heads.push((top, index));
                    let open = self.open.contains(&row.name);
                    let head = format!("{}  {} {}", row.name, row.value, if open { "\u{25be}" } else { "\u{25b8}" });
                    self.draw_name.draw_abs(cx, dvec2(rect.pos.x, y), &head);
                    if open {
                        let (offset, per) = grid(block, rect.size.x, cell.x);
                        let cells_x = rect.pos.x + (offset + 2) as f64 * cell.x;
                        for (line, start) in (0..block.len()).step_by(per).enumerate() {
                            let ly = y + (line + 1) as f64 * self.row_h;
                            let label = block.offset(start);
                            self.draw_name.draw_abs(cx, dvec2(rect.pos.x + (offset - label.len()) as f64 * cell.x, ly), &label);
                            let text = (start..(start + per).min(block.len())).map(|i| block.cell(i)).collect::<Vec<_>>().join(" ");
                            self.draw_value.draw_abs(cx, dvec2(cells_x, ly), &text);
                        }
                        1 + block.len().div_ceil(per)
                    } else {
                        let used = (head.chars().count() + 2) as f64 * cell.x;
                        let text = preview(block, rect.size.x - used, cell.x);
                        let x = rect.pos.x + rect.size.x - text.chars().count() as f64 * cell.x;
                        self.draw_value.draw_abs(cx, dvec2(x, y), &text);
                        1
                    }
                }
                None => {
                    self.draw_name.draw_abs(cx, dvec2(rect.pos.x, y), &row.name);
                    self.draw_value.color = match row.truth {
                        Some(true) => self.color_true,
                        Some(false) => self.color_false,
                        None => normal,
                    };
                    let lines = row_lines(row, rect.size.x, cell.x, |line, value, value_width| {
                        let x = rect.pos.x + rect.size.x - value_width;
                        self.draw_value.draw_abs(cx, dvec2(x, y + line as f64 * self.row_h), value);
                    });
                    self.draw_value.color = normal;
                    lines
                }
            };
            top += lines as f64 * self.row_h;
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                if self.head_at(cx, e.abs).is_some() {
                    cx.set_cursor(MouseCursor::Hand);
                }
            }
            Hit::FingerUp(e) if e.is_over && e.is_primary_hit() && e.was_tap() => {
                if let Some(row) = self.head_at(cx, e.abs) {
                    self.toggle(cx, row);
                }
            }
            _ => {}
        }
    }

    fn is_interactive(&self) -> bool {
        self.rows.iter().any(|row| row.block.is_some())
    }

    fn text(&self) -> String {
        let mut text = String::new();
        for row in &self.rows {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&row.name);
            text.push_str(" = ");
            text.push_str(&row.value);
            if let Some(block) = row.block.as_ref().filter(|_| self.open.contains(&row.name)) {
                for i in 0..block.len() {
                    text.push(if i == 0 { ' ' } else { ',' });
                    text.push_str(block.cell(i).trim_start());
                }
            }
        }
        text
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::StaticText;
        node.name = self.text();
        node.text.get_or_insert_with(Default::default).content.clone_from(&node.name);
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let rect = self.area.rect(cx);
        let clip = self.area.clipped_rect(cx);
        for &(top, index) in &self.heads {
            let Some(row) = self.rows.get(index) else { continue };
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                name: format!("{} {}", row.name, row.value),
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::SCROLL_INTO_VIEW,
                ..Default::default()
            };
            let open = self.open.contains(&row.name);
            node.states.set(a11y_tree::States::EXPANDED, open);
            node.states.set(a11y_tree::States::COLLAPSED, !open);
            let head = Rect { pos: dvec2(rect.pos.x, rect.pos.y + top), size: dvec2(rect.size.x, self.row_h) };
            let lo = dvec2(head.pos.x.max(clip.pos.x), head.pos.y.max(clip.pos.y));
            let hi = dvec2((head.pos.x + head.size.x).min(clip.pos.x + clip.size.x), (head.pos.y + head.size.y).min(clip.pos.y + clip.size.y));
            node.bounds = if hi.x > lo.x && hi.y > lo.y { a11y::semantic_rect(Rect { pos: lo, size: hi - lo }) } else { Default::default() };
            node.states.set(a11y_tree::States::OFFSCREEN, node.bounds.is_empty());
            out.push((Self::part_key(&row.name), None, node));
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions as A};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(&(_, row)) = self.heads.iter().find(|(_, row)| self.rows.get(*row).is_some_and(|r| Self::part_key(&r.name) == key)) else {
            return ActionResult::Unhandled;
        };
        if request.action != A::PRESS {
            return ActionResult::Unsupported;
        }
        self.toggle(cx, row);
        ActionResult::Handled
    }
}
