//! The loops tab: each deck's loop splat (donor `loop_splat_view`, deleted
//! in ef48f3d9). The analysis slices a track into up to eight sections on
//! whole bars and finds, per stem and for the whole mix, the steady loop in
//! each section; here they stand as a grid per deck, a row per stem (DRUMS,
//! BASS, VOCALS, OTHER) and the MIX, a column per section. A cell's
//! brightness is its loop's energy; a stem that is silent in a section has
//! no cell there. A press on a cell reports it; the cell that loops now is
//! outlined.
//!
//! Plain view data in, presses out: the parent owns the decks.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    set_type_default() do #(DrawSplatCell::script_shader(vm)){
        ..mod.draw.DrawQuad
        ring: uniform(stage.text_hi)
        ghost: uniform(stage.ghost_face)
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, 2.5)
            let lit = self.tint.xyz * (0.22 + 0.78 * self.energy) * (1.0 + 0.25 * self.hover)
            let fill = vec4(lit, 1.0).mix(self.ghost, self.empty)
            sdf.fill_keep(fill)
            sdf.stroke(self.ring.mix(vec4(0.0, 0.0, 0.0, 0.0), 1.0 - self.active), 3.0)
            return sdf.result
        }
    }

    mod.widgets.StageLoopSplatViewBase = #(LoopSplatView::register_widget(vm))
    mod.widgets.StageLoopSplatView = set_type_default() do mod.widgets.StageLoopSplatViewBase{
        width: Fill
        height: Fill
        draw_bg +: {color: #x00000000}
        draw_label +: {color: stage.label text_style: theme.font_bold{font_size: 8}}
        draw_note +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}
        draw_count +: {color: stage.on_accent text_style: theme.font_bold{font_size: 8}}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawSplatCell {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub tint: Vec4f,
    #[live]
    pub energy: f32,
    #[live]
    pub empty: f32,
    #[live]
    pub active: f32,
    #[live]
    pub hover: f32,
}

/// The rows, top to bottom, as the analysis orders them.
pub const ROW_LABELS: [&str; 5] = ["DRUMS", "BASS", "VOCALS", "OTHER", "MIX"];

/// One loop cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplatCellView {
    pub bars: u8,
    /// 0..1, relative to the deck's loudest cell.
    pub energy: f32,
}

/// One deck's grid: `cells[row][col]`, `None` where there is no loop.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DeckSplatView {
    pub title: String,
    /// What to say instead of a grid (no analysis, loading...).
    pub note: String,
    pub cells: Vec<Vec<Option<SplatCellView>>>,
    /// The cell that loops now.
    pub active: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum LoopSplatAction {
    #[default]
    None,
    /// A press on deck, row, column.
    Cell(usize, usize, usize),
}

const HEAD_H: f64 = 20.0;
const LABEL_W: f64 = 52.0;
const GAP: f64 = 12.0;

#[derive(Script, ScriptHook, Widget)]
pub struct LoopSplatView {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_cell: DrawSplatCell,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_note: DrawText,
    #[live]
    draw_count: DrawText,
    #[rust]
    decks: [DeckSplatView; 2],
    #[rust]
    hover: Option<(usize, usize, usize)>,
    /// Where each cell was drawn, relative to the view.
    #[rust]
    rects: Vec<((usize, usize, usize), Rect)>,
}

impl LoopSplatView {
    pub fn set_decks(&mut self, cx: &mut Cx, decks: [DeckSplatView; 2]) {
        if self.decks != decks {
            self.decks = decks;
            self.draw_bg.redraw(cx);
        }
    }

    fn cell_at(&self, at: DVec2) -> Option<(usize, usize, usize)> {
        self.rects.iter().find(|(_, rect)| rect.contains(at)).map(|(cell, _)| *cell)
    }
}

impl Widget for LoopSplatView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let origin = self.draw_bg.area().rect(cx).pos;
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hover = self.cell_at(fe.abs - origin);
                if hover != self.hover {
                    self.hover = hover;
                    self.draw_bg.redraw(cx);
                }
                cx.set_cursor(if hover.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                if self.hover.take().is_some() {
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() => {
                if let Some((deck, row, col)) = self.cell_at(fe.abs - origin) {
                    cx.widget_action(self.uid, LoopSplatAction::Cell(deck, row, col));
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        self.rects.clear();
        let palette = makepad_look::palette::get(cx);
        // The analysis' stems are drums, bass, vocals, other; the palette
        // names them vocals, drums, bass, other.
        let [vocals, drums, bass, other] = palette.stem_colors();
        let tints = [drums, bass, vocals, other, palette.accent].map(Vec4f::from_u32);
        let half = ((rect.size.x - GAP) * 0.5).max(0.0);
        for deck in 0..2 {
            let x0 = rect.pos.x + deck as f64 * (half + GAP);
            let view = &self.decks[deck];
            let head = format!("{} · {}", ["A", "B"][deck], if view.title.is_empty() { "no track" } else { view.title.as_str() });
            self.draw_label.draw_abs(cx, dvec2(x0, rect.pos.y + 4.0), &head);
            if view.cells.is_empty() {
                self.draw_note.draw_abs(cx, dvec2(x0, rect.pos.y + HEAD_H + 6.0), &view.note);
                continue;
            }
            let rows = view.cells.len();
            let cols = view.cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
            let row_h = ((rect.size.y - HEAD_H) / rows as f64).clamp(14.0, 44.0);
            let col_w = ((half - LABEL_W) / cols as f64).max(8.0);
            for (row, line) in view.cells.iter().enumerate() {
                let y = rect.pos.y + HEAD_H + row as f64 * row_h;
                self.draw_note.draw_abs(cx, dvec2(x0, y + row_h * 0.5 - 5.0), ROW_LABELS.get(row).copied().unwrap_or(""));
                for col in 0..cols {
                    let cell = line.get(col).copied().flatten();
                    let at = Rect { pos: dvec2(x0 + LABEL_W + col as f64 * col_w, y), size: dvec2(col_w, row_h) };
                    self.draw_cell.tint = tints[row.min(4)];
                    self.draw_cell.energy = cell.map_or(0.0, |cell| cell.energy.clamp(0.0, 1.0));
                    self.draw_cell.empty = if cell.is_some() { 0.0 } else { 1.0 };
                    self.draw_cell.active = if view.active == Some((row, col)) { 1.0 } else { 0.0 };
                    self.draw_cell.hover = if cell.is_some() && self.hover == Some((deck, row, col)) { 1.0 } else { 0.0 };
                    self.draw_cell.draw_abs(cx, at);
                    if let Some(cell) = cell {
                        self.draw_count.draw_abs(cx, dvec2(at.pos.x + 5.0, at.pos.y + row_h * 0.5 - 5.0), &cell.bars.to_string());
                        self.rects.push(((deck, row, col), Rect { pos: at.pos - rect.pos, size: at.size }));
                    }
                }
            }
        }
        DrawStep::done()
    }
}

impl LoopSplatViewRef {
    pub fn set_decks(&self, cx: &mut Cx, decks: [DeckSplatView; 2]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_decks(cx, decks);
        }
    }

    /// The cell pressed in `actions`.
    pub fn pressed(&self, actions: &Actions) -> Option<(usize, usize, usize)> {
        match actions.find_widget_action(self.widget_uid())?.cast() {
            LoopSplatAction::Cell(deck, row, col) => Some((deck, row, col)),
            LoopSplatAction::None => None,
        }
    }
}
