//! The virtual slide list. Dragging only proposes a move; the page submits it.

use makepad_story_deck::{DeckDoc, ops::{DeckOp, slide_drop_index}};
use super::thumbnails::SharedThumbnails;
use makepad_widgets::*;
use std::collections::HashMap;
use std::rc::Rc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.DeckSorter = set_type_default() do #(DeckSorter::register_widget(vm)){
        width: Fill height: Fill
        draw_hit +: {color: #0000}
        draw_line +: {color: stage.accent}
        draw_chip +: {color: stage.accent}
        draw_name +: {color: stage.on_accent text_style: theme.font_bold{font_size: 9}}
        list := PortalList{
            width: Fill height: Fill flow: Down
            drag_scrolling: false grab_key_focus: false
            // One row a slide: its number at the bottom left, the still
            // filling the rest; the shown slide sits on an accent backdrop.
            Slide := RoundedView{
                width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 1.0}
                margin: Inset{bottom: 4 right: 6} padding: Inset{left: 4 right: 5 top: 5 bottom: 5} show_bg: true
                draw_bg +: {color: #0000 border_radius: 6}
                number := Label{width: 18 height: Fit padding: 0 align: Align{x: 1.0} text: "" draw_text +: {color: stage.dim text_style: theme.font_regular{font_size: 8.5}}}
                poster := RoundedView{width: 120 height: 67.5 flow: Overlay show_bg: true
                    draw_bg +: {color: stage.well border_radius: 4}
                    // A slide with live inputs (an app, a camera, code) has no
                    // still: a card naming them.
                    app_card := View{visible: false width: Fill height: Fill flow: Down spacing: 4 align: Align{x: 0.5 y: 0.5}
                        Icon{icon_walk: Walk{width: 20 height: 20} draw_icon +: {svg: crate_resource("self:resources/icons/edit/app_run.svg") color: stage.dim}}
                        app_name := Label{width: Fit height: Fit padding: 0 text: "" draw_text +: {color: stage.dim text_style: theme.font_regular{font_size: 8}}}}
                    // An unbound Image samples black. Keep it hidden until the
                    // immutable still arrives, leaving the plain card visible.
                    picture := Image{visible: false width: Fill height: Fill fit: ImageFit.Smallest placeholder_width: 16 placeholder_height: 9}
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum SorterAction {
    #[default]
    None,
    Jump(usize),
    Edit { op: DeckOp, select: usize },
    /// A slide row right-clicked: its menu at the pointer. The slide count
    /// for the list's background below the last slide.
    Menu(usize, Vec2d),
}

/// A dropped reorder that has not landed yet: the rows show the new order
/// at once (display row -> slide index of `doc`) until the edited deck
/// replaces `doc`, or the edit ends without changing it.
struct Reordered {
    doc: Rc<DeckDoc>,
    order: Vec<usize>,
}

struct Drag {
    from: usize,
    start: Vec2d,
    at: Vec2d,
    moving: bool,
    doc: Rc<DeckDoc>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct DeckSorter {
    #[deref]
    view: View,
    #[live]
    draw_hit: DrawColor,
    #[live]
    draw_line: DrawColor,
    #[live]
    draw_chip: DrawColor,
    #[live]
    draw_name: DrawText,
    #[rust]
    overlay: Option<DrawList2d>,
    #[rust]
    shared: Option<SharedThumbnails>,
    #[rust]
    rows: Vec<(usize, WidgetRef)>,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    revision: u64,
    #[rust]
    scroll_timer: Timer,
    /// Where a library card is being dragged over the list: the insertion
    /// line is drawn there.
    #[rust]
    drop_at: Option<Vec2d>,
    /// What each reused row widget was last styled with (selected, still
    /// width and height, palette): a row is re-styled only when that
    /// changes, never on every frame (each style is a script evaluation).
    #[rust]
    styled: HashMap<WidgetUid, (bool, u64, u64, u64)>,
    #[rust]
    reordered: Option<Reordered>,
}

impl DeckSorter {
    pub fn bind(&mut self, cx: &mut Cx, shared: SharedThumbnails) {
        self.shared = Some(shared);
        self.cancel_drag(cx);
        self.rows.clear();
        self.styled.clear();
        self.reordered = None;
        self.view.portal_list(cx, ids!(list)).set_first_id_and_scroll(0, 0.0);
        self.redraw(cx);
    }

    pub fn follow(&mut self, cx: &mut Cx, index: usize) {
        self.view.portal_list(cx, ids!(list)).keep_index_visible(cx, index);
        self.redraw(cx);
    }

    pub fn dragging(&self) -> bool { self.drag.is_some() }

    /// The edit queue is empty and no edit is running: a dropped order whose
    /// deck was not replaced did not land (refused), so the rows go back.
    pub fn settle_reorder(&mut self, cx: &mut Cx) {
        let current = self.shared.as_ref().and_then(|shared| shared.borrow().doc.clone());
        if self.reordered.as_ref().is_some_and(|r| current.is_none_or(|doc| Rc::ptr_eq(&doc, &r.doc))) {
            self.reordered = None;
            self.redraw(cx);
        }
    }

    pub fn cancel_drag(&mut self, cx: &mut Cx) {
        self.drag = None;
        cx.stop_timer(self.scroll_timer);
        self.scroll_timer = Timer::default();
        self.redraw(cx);
    }

    fn scroll_direction(&self, cx: &Cx) -> f64 {
        let Some(drag) = self.drag.as_ref().filter(|drag| drag.moving) else { return 0.0 };
        let rect = self.draw_hit.area().rect(cx);
        if drag.at.x < rect.pos.x || drag.at.x > rect.pos.x + rect.size.x { return 0.0; }
        let edge = 28.0_f64.min(rect.size.y * 0.25);
        if edge <= 0.0 { return 0.0; }
        if drag.at.y < rect.pos.y + edge { ((rect.pos.y + edge - drag.at.y) / edge).min(1.0) }
        else if drag.at.y > rect.pos.y + rect.size.y - edge { -((drag.at.y - rect.pos.y - rect.size.y + edge) / edge).min(1.0) }
        else { 0.0 }
    }

    fn row_at(&self, cx: &Cx, at: Vec2d) -> Option<usize> {
        self.rows.iter().find(|(_, row)| row.area().rect(cx).contains(at)).map(|(index, _)| *index)
    }

    /// An insertion boundary in the original order. It is converted to a
    /// final slide index after removing the dragged row.
    fn insertion(&self, cx: &Cx, at: Vec2d) -> Option<(usize, f64)> {
        if !self.draw_hit.area().rect(cx).contains(at) {
            return None;
        }
        for (index, row) in &self.rows {
            let rect = row.area().rect(cx);
            if at.y < rect.pos.y + rect.size.y * 0.5 {
                return Some((*index, rect.pos.y));
            }
        }
        self.rows.last().map(|(index, row)| {
            let rect = row.area().rect(cx);
            (index + 1, rect.pos.y + rect.size.y)
        })
    }

    /// Whether `at` is over the slide list.
    pub fn contains(&self, cx: &Cx, at: Vec2d) -> bool {
        self.draw_hit.area().rect(cx).contains(at)
    }

    /// The slide a card dropped at `at` goes before (`slides.len()` for the
    /// end), when `at` is over the list.
    pub fn drop_slot(&self, cx: &Cx, at: Vec2d) -> Option<usize> {
        self.insertion(cx, at).map(|(boundary, _)| boundary)
    }

    /// Show where a dragged library card would land (None: nowhere).
    pub fn set_drop_hint(&mut self, cx: &mut Cx, at: Option<Vec2d>) {
        if self.drop_at != at {
            self.drop_at = at;
            self.view.redraw(cx);
        }
    }
}

impl Widget for DeckSorter {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        {
            let revision = self.shared.as_ref().map_or(0, |shared| shared.borrow().revision);
            if self.revision != revision {
                self.revision = revision;
                self.redraw(cx);
            }
            // The reordered deck landed (or another replaced it).
            if self.reordered.as_ref().is_some_and(|r| self.shared.as_ref().and_then(|shared| shared.borrow().doc.clone())
                .is_none_or(|doc| !Rc::ptr_eq(&doc, &r.doc))) {
                self.reordered = None;
                self.redraw(cx);
            }
            // An edit arriving during a drag invalidates its original indices.
            if self.drag.as_ref().is_some_and(|drag| self.shared.as_ref().and_then(|shared| shared.borrow().doc.clone())
                .is_none_or(|doc| !Rc::ptr_eq(&doc, &drag.doc))) {
                self.cancel_drag(cx);
            }
        }
        if matches!(event, Event::WindowLostFocus(_) | Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, .. })) {
            self.cancel_drag(cx);
        }
        if self.scroll_timer.is_event(event).is_some() {
            self.scroll_timer = Timer::default();
            let direction = self.scroll_direction(cx);
            if direction != 0.0 {
                let list = self.view.portal_list(cx, ids!(list));
                list.set_first_id_and_scroll(list.first_id(), list.scroll_position() + direction * 18.0);
                self.redraw(cx);
            }
        }
        // The content owns the press, leaving the list's scrollbar free.
        // Scroll events still go straight to PortalList.
        if !matches!(event, Event::Scroll(_)) {
            match event.hits(cx, self.draw_hit.area()) {
                // Rows are in the dropped order until it lands (a frame or
                // a few): a press then would name the old indices.
                Hit::FingerDown(_) if self.reordered.is_some() => {}
                Hit::FingerDown(fe) if fe.mouse_button().is_some_and(|button| button.is_secondary()) => {
                    self.cancel_drag(cx);
                    // On a slide: that slide's menu; below the last: the list's.
                    let slides = self.shared.as_ref().and_then(|shared| shared.borrow().doc.as_ref().map(|doc| doc.slides.len())).unwrap_or(0);
                    let index = self.row_at(cx, fe.abs).unwrap_or(slides);
                    cx.widget_action(self.widget_uid(), SorterAction::Menu(index, fe.abs));
                }
                Hit::FingerDown(fe) if fe.is_primary_hit() => {
                    cx.set_key_focus(Area::Empty);
                    self.drag = self.row_at(cx, fe.abs).and_then(|from| {
                        let doc = self.shared.as_ref()?.borrow().doc.clone()?;
                        Some(Drag { from, start: fe.abs, at: fe.abs, moving: false, doc })
                    });
                }
                Hit::FingerMove(fe) => {
                    if let Some(drag) = &mut self.drag {
                        drag.at = fe.abs;
                        drag.moving |= (drag.at - drag.start).length() > 5.0;
                        cx.set_cursor(MouseCursor::Hand);
                        self.redraw(cx);
                    }
                }
                Hit::FingerUp(fe) => {
                    if let Some(drag) = self.drag.take() {
                        cx.stop_timer(self.scroll_timer);
                        self.scroll_timer = Timer::default();
                        if !fe.cancelled {
                            if drag.moving {
                                if let Some((boundary, _)) = self.insertion(cx, fe.abs) {
                                    if let Some(to) = slide_drop_index(drag.doc.slides.len(), drag.from, boundary) {
                                        let mut slides: Vec<_> = drag.doc.slides.iter().map(|slide| slide.name.clone()).collect();
                                        let moved = slides.remove(drag.from);
                                        slides.insert(to, moved);
                                        let mut order: Vec<usize> = (0..drag.doc.slides.len()).collect();
                                        let moved = order.remove(drag.from);
                                        order.insert(to, moved);
                                        self.reordered = Some(Reordered { doc: drag.doc.clone(), order });
                                        cx.widget_action(self.widget_uid(), SorterAction::Edit { op: DeckOp::ReorderSlides { slides }, select: to });
                                    }
                                }
                            } else if self.row_at(cx, fe.abs) == Some(drag.from) {
                                cx.widget_action(self.widget_uid(), SorterAction::Jump(drag.from));
                            }
                        }
                        self.redraw(cx);
                    }
                }
                Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                    cx.set_cursor(MouseCursor::Hand);
                }
                _ => {}
            }
        }
        if self.scroll_direction(cx) != 0.0 {
            if self.scroll_timer.is_empty() { self.scroll_timer = cx.start_timeout(0.05); }
        } else {
            cx.stop_timer(self.scroll_timer);
            self.scroll_timer = Timer::default();
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.rows.clear();
        let shared = self.shared.clone();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let portal = item.as_portal_list();
            let Some(mut list) = portal.borrow_mut() else { continue };
            let state = shared.as_ref().map(|shared| shared.borrow());
            let doc = state.as_ref().and_then(|state| state.doc.as_ref());
            list.set_item_range(cx, 0, doc.map_or(0, |doc| doc.slides.len()));
            while let Some(index) = list.next_visible_item(cx) {
                // The slide this row shows: its own, or the dropped order's.
                let source = self.reordered.as_ref().filter(|r| doc.is_some_and(|doc| Rc::ptr_eq(doc, &r.doc)))
                    .and_then(|r| r.order.get(index).copied()).unwrap_or(index);
                let Some(slide) = doc.and_then(|doc| doc.slides.get(source)) else { continue };
                let mut row = list.item(cx, index, id!(Slide));
                let state = state.as_ref().unwrap();
                let selected = source == state.current;
                // The still fills the row: the list's width less the row's
                // margin (room for the scrollbar), padding and the number, at
                // the deck's aspect, whole pixels.
                let width = (cx.turtle().rect().size.x - 6.0 - 9.0 - 18.0 - 6.0).floor().max(1.0);
                let (ow, oh) = doc.map_or((16, 9), |doc| doc.output);
                let height = (width * oh.max(1) as f64 / ow.max(1) as f64).round();
                let style = (selected, width.to_bits(), height.to_bits(), makepad_look::palette::generation(cx));
                if self.styled.insert(row.widget_uid(), style) != Some(style) {
                    let palette = makepad_look::palette::get(cx);
                    let face = if selected { Vec4f::from_u32(palette.accent) } else { vec4f(0.0, 0.0, 0.0, 0.0) };
                    let ink = Vec4f::from_u32(if selected { palette.on_accent } else { palette.dim });
                    script_apply_eval!(cx, row, {draw_bg +: {color: #(face)}});
                    let mut number = row.widget(cx, ids!(number));
                    script_apply_eval!(cx, number, {draw_text +: {color: #(ink)}});
                    let mut poster = row.widget(cx, ids!(poster));
                    script_apply_eval!(cx, poster, {width: #(width) height: #(height)});
                }
                row.label(cx, ids!(number)).set_text(cx, &(index + 1).to_string());
                let live = super::thumbnails::live_label(slide);
                let texture = state.textures.get(&source).cloned();
                row.widget(cx, ids!(picture)).set_visible(cx, live.is_none() && texture.is_some());
                row.widget(cx, ids!(app_card)).set_visible(cx, live.is_some());
                row.label(cx, ids!(app_name)).set_text(cx, live.as_deref().unwrap_or(""));
                row.image(cx, ids!(picture)).set_texture(cx, texture);
                row.draw_all(cx, scope);
                self.rows.push((index, row));
            }
        }
        self.rows.sort_by_key(|(index, _)| *index);
        let mut rect = self.view.area().rect(cx);
        rect.size.x = (rect.size.x - 14.0).max(0.0);
        self.draw_hit.draw_abs(cx, rect);
        if let Some(shared) = &shared {
            let mut state = shared.borrow_mut();
            let output = state.doc.as_ref().map_or((16, 9), |doc| doc.output);
            state.sizes = self.rows.iter().map(|(index, row)| {
                let width = row.widget(cx, ids!(poster)).area().rect(cx).size.x;
                (*index, super::super::still_cache::Size::new(width, cx.current_dpi_factor(), output.0 as f64 / output.1.max(1) as f64))
            }).collect();
            state.visible = self.rows.iter().filter(|(_, row)| {
                let r = row.area().rect(cx);
                r.pos.y < rect.pos.y + rect.size.y && r.pos.y + r.size.y > rect.pos.y
            }).map(|(index, _)| *index).collect();
        }
        let insertion = self.drag.as_ref().filter(|drag| drag.moving).map(|drag| drag.at).or(self.drop_at).and_then(|at| self.insertion(cx, at));
        let overlay = self.overlay.get_or_insert_with(|| DrawList2d::new(cx));
        overlay.begin_overlay_reuse(cx);
        cx.begin_root_turtle(cx.current_pass_size(), Layout::flow_down());
        if let (None, Some((_, y))) = (&self.drag, insertion) {
            let bottom = rect.pos.y + (rect.size.y - 3.0).max(0.0);
            self.draw_line.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y.clamp(rect.pos.y, bottom)), size: dvec2(rect.size.x, 3.0) });
        }
        if let Some(drag) = &self.drag {
            if drag.moving {
                if let Some((_, y)) = insertion {
                    let bottom = rect.pos.y + (rect.size.y - 3.0).max(0.0);
                    self.draw_line.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y.clamp(rect.pos.y, bottom)), size: dvec2(rect.size.x, 3.0) });
                }
                let name = shared.as_ref().and_then(|shared| shared.borrow().doc.as_ref().and_then(|doc| doc.slides.get(drag.from).map(|slide| slide.name.clone()))).unwrap_or_default();
                let pos = drag.at + dvec2(12.0, 12.0);
                self.draw_chip.draw_abs(cx, Rect { pos, size: dvec2(200.0, 28.0) });
                self.draw_name.draw_abs(cx, pos + dvec2(8.0, 7.0), &format!("{}  {}", drag.from + 1, name));
            }
        }
        cx.end_pass_sized_turtle();
        overlay.end(cx);
        DrawStep::done()
    }
}
