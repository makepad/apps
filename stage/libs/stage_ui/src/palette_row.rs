//! A row of effect palettes to choose from: each palette a chip -- its three
//! drawing colours side by side over its ground at the foot, the order the
//! swatches stand in -- with an arrow at either end that pages through a
//! row longer than the width. A press on a chip reports it; the chip of the
//! palette in force is outlined. The donor console's chooser rows
//! (`palette_chooser.rs`, deleted in ef48f3d9).
//!
//! The parent hands the palettes in (bg, a, b, c) and which one is chosen;
//! the row keeps nothing else but its page.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    set_type_default() do #(DrawPaletteChip::script_shader(vm)){
        ..mod.draw.DrawQuad
        ring: uniform(stage.accent)
        rim: uniform(stage.border_hi)
        pixel: fn() {
            let p = self.pos
            let ground = 0.32
            let col = self.c_a.mix(self.c_b, step(1.0 / 3.0, p.x)).mix(self.c_c, step(2.0 / 3.0, p.x))
            let fill = col.mix(self.c_bg, step(1.0 - ground, p.y))
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, 2.0)
            sdf.fill_keep(vec4(fill.xyz, 1.0))
            let edge = self.rim.mix(self.rim * 2.0, self.hover).mix(self.ring, self.chosen)
            sdf.stroke(edge, 1.0 + self.chosen)
            return sdf.result
        }
    }

    set_type_default() do #(DrawPaletteArrow::script_shader(vm)){
        ..mod.draw.DrawQuad
        ink: uniform(stage.label)
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            let tip = mix(w * 0.3, w * 0.7, self.dir)
            let back = mix(w * 0.65, w * 0.35, self.dir)
            sdf.move_to(back, h * 0.3)
            sdf.line_to(tip, h * 0.5)
            sdf.line_to(back, h * 0.7)
            sdf.stroke(self.ink * (0.35 + 0.65 * self.live), 1.5)
            return sdf.result
        }
    }

    mod.widgets.StagePaletteRowBase = #(PaletteRow::register_widget(vm))
    mod.widgets.StagePaletteRow = set_type_default() do mod.widgets.StagePaletteRowBase{
        width: Fill
        height: 28
        draw_bg +: {color: #x00000000}
        chip_width: 42.0
        spacing: 4.0
        arrow_width: 14.0
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawPaletteChip {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub c_bg: Vec4f,
    #[live]
    pub c_a: Vec4f,
    #[live]
    pub c_b: Vec4f,
    #[live]
    pub c_c: Vec4f,
    #[live]
    pub chosen: f32,
    #[live]
    pub hover: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawPaletteArrow {
    #[deref]
    pub draw_super: DrawQuad,
    /// 0 points left, 1 right.
    #[live]
    pub dir: f32,
    /// 1 when there is a page that way.
    #[live]
    pub live: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PaletteRowAction {
    #[default]
    None,
    /// The chip at this index of the parent's palettes was pressed.
    Picked(usize),
}

#[derive(Script, ScriptHook, Widget)]
pub struct PaletteRow {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    /// The row's own quad, transparent: what the pointer hits.
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_chip: DrawPaletteChip,
    #[live]
    draw_arrow: DrawPaletteArrow,
    #[live(42.0)]
    chip_width: f64,
    #[live(4.0)]
    spacing: f64,
    #[live(14.0)]
    arrow_width: f64,
    /// bg, a, b, c each.
    #[rust]
    palettes: Vec<[Vec4f; 4]>,
    #[rust]
    chosen: Option<usize>,
    /// The first chip shown.
    #[rust]
    first: usize,
    /// Chips that fit, from the last draw.
    #[rust]
    fits: usize,
    #[rust]
    hover: Option<usize>,
    /// Where each shown chip was drawn, by palette index.
    #[rust]
    chips: Vec<(usize, Rect)>,
    #[rust]
    arrows: [Rect; 2],
}

/// The first chip of the page that shows `chosen`, `fits` chips to a page.
pub fn page_of(chosen: usize, fits: usize) -> usize {
    let fits = fits.max(1);
    chosen / fits * fits
}

impl PaletteRow {
    pub fn set_palettes(&mut self, cx: &mut Cx, palettes: Vec<[Vec4f; 4]>, chosen: Option<usize>) {
        if self.palettes == palettes && self.chosen == chosen {
            return;
        }
        let new_row = self.palettes != palettes;
        self.palettes = palettes;
        self.chosen = chosen.filter(|index| *index < self.palettes.len());
        if new_row {
            // A new row shows the page of its chosen chip, else its start.
            self.first = self.chosen.map_or(0, |chosen| page_of(chosen, self.fits));
        }
        self.draw_bg.redraw(cx);
    }

    pub fn len(&self) -> usize {
        self.palettes.len()
    }

    fn page(&mut self, cx: &mut Cx, dir: isize) {
        let fits = self.fits.max(1);
        let next = if dir < 0 { self.first.saturating_sub(fits) } else { self.first + fits };
        if next < self.palettes.len() && next != self.first {
            self.first = next;
            self.draw_bg.redraw(cx);
        }
    }
}

impl Widget for PaletteRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let hit = event.hits(cx, self.draw_bg.area());
        // The chips are kept relative to the row: a row drawn in a modal or
        // a scrolled view is drawn at other coordinates than it is hit.
        let origin = self.draw_bg.area().rect(cx).pos;
        let local = |abs: DVec2| abs - origin;
        match hit {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hover = self.chips.iter().find(|(_, rect)| rect.contains(local(fe.abs))).map(|(index, _)| *index);
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
            Hit::FingerDown(_) => {}
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() => {
                let at = local(fe.abs);
                if self.arrows[0].contains(at) {
                    self.page(cx, -1);
                } else if self.arrows[1].contains(at) {
                    self.page(cx, 1);
                } else if let Some((index, _)) = self.chips.iter().find(|(_, rect)| rect.contains(at)) {
                    cx.widget_action(self.uid, PaletteRowAction::Picked(*index));
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        let chips_x = rect.pos.x + self.arrow_width + self.spacing;
        let chips_w = (rect.size.x - 2.0 * (self.arrow_width + self.spacing)).max(0.0);
        let pitch = self.chip_width + self.spacing;
        self.fits = (((chips_w + self.spacing) / pitch).floor() as usize).max(1);
        if self.first >= self.palettes.len() {
            self.first = page_of(self.palettes.len().saturating_sub(1), self.fits);
        }
        self.chips.clear();
        for (slot, index) in (self.first..self.palettes.len()).take(self.fits).enumerate() {
            let [bg, a, b, c] = self.palettes[index];
            let chip = Rect { pos: dvec2(chips_x + slot as f64 * pitch, rect.pos.y), size: dvec2(self.chip_width, rect.size.y) };
            self.draw_chip.c_bg = bg;
            self.draw_chip.c_a = a;
            self.draw_chip.c_b = b;
            self.draw_chip.c_c = c;
            self.draw_chip.chosen = if self.chosen == Some(index) { 1.0 } else { 0.0 };
            self.draw_chip.hover = if self.hover == Some(index) { 1.0 } else { 0.0 };
            self.draw_chip.draw_abs(cx, chip);
            self.chips.push((index, Rect { pos: chip.pos - rect.pos, size: chip.size }));
        }
        let left = Rect { pos: rect.pos, size: dvec2(self.arrow_width, rect.size.y) };
        let right = Rect { pos: dvec2(rect.pos.x + rect.size.x - self.arrow_width, rect.pos.y), size: dvec2(self.arrow_width, rect.size.y) };
        for (dir, at, live) in [(0.0, left, self.first > 0), (1.0, right, self.first + self.fits < self.palettes.len())] {
            self.draw_arrow.dir = dir;
            self.draw_arrow.live = if live { 1.0 } else { 0.0 };
            self.draw_arrow.draw_abs(cx, at);
        }
        self.arrows = [left, right].map(|arrow| Rect { pos: arrow.pos - rect.pos, size: arrow.size });
        DrawStep::done()
    }
}

impl PaletteRowRef {
    pub fn set_palettes(&self, cx: &mut Cx, palettes: Vec<[Vec4f; 4]>, chosen: Option<usize>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_palettes(cx, palettes, chosen);
        }
    }

    /// The chip pressed in `actions`, by index.
    pub fn picked(&self, actions: &Actions) -> Option<usize> {
        match actions.find_widget_action(self.widget_uid())?.cast() {
            PaletteRowAction::Picked(index) => Some(index),
            PaletteRowAction::None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_of_a_chip_starts_at_its_multiple_of_the_fit() {
        assert_eq!(page_of(0, 8), 0);
        assert_eq!(page_of(7, 8), 0);
        assert_eq!(page_of(8, 8), 8);
        assert_eq!(page_of(19, 8), 16);
        assert_eq!(page_of(5, 0), 5, "nothing fits yet: one to a page");
    }
}
