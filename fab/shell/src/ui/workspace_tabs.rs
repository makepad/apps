//! The caption's workspace switcher: one button naming the shown workspace, whose `⌄` menu lists
//! every offered one (the row of tabs, design FABWS, is not shown: its code stays for the
//! rename, the menus and the drag, which a tab no longer starts). Cmd/Ctrl and a number switch. A click
//! shows one; a double click renames it in place; a right click (or a hold) opens its menu
//! (Rename, Duplicate, Reset, Delete); a drag reorders, the tab's chip at the pointer and a line
//! where it would land. `+` offers a duplicate, a temporary layout and the defaults. Past its
//! room the person's row scrolls under the wheel and `⌄` lists its tabs. A temporary layout has
//! a fixed tab after those controls. Its × goes Back, and its menu offers Keep, Rename and Back.
//! An edit from outside the app shows its cue after the row for a few seconds.
//!
//! The caption owns no state: the app syncs the row from the list ([`FabWorkspaceTabs::set`]) and
//! the row says what was asked ([`WorkspaceTabAction`]).
//!
//! Costs: a sync compares the list, O(tabs), and redraws only on a change; a draw lays out each
//! name, O(tabs). The temporary tab adds one title layout and one icon per draw, O(1) in the
//! number of tabs. A hover or a drag step is O(tabs). The cue redraws the row while it fades.

use crate::ui::topbar::TempTab;
use crate::workspaces::WorkspaceId;
use fab_elec_view::live::views::{Cue, EditOrigin, ElecOutsideEdit};
use makepad_widgets_core::{makepad_draw::text::layouter::LaidoutText, *};
use std::rc::Rc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabWorkspaceTabsBase = #(FabWorkspaceTabs::register_widget(vm))
    mod.widgets.FabWorkspaceTabs = set_type_default() do mod.widgets.FabWorkspaceTabsBase{
        width: Fit height: 26
        draw_face +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, fab.radius)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_text +: {text_style: theme.font_regular{font_size: fab.font_size_ui}}
        draw_key +: {color: fab.color_text_muted text_style: theme.font_regular{font_size: 6.75}}
        draw_cue +: {
            text_style: theme.font_regular{font_size: fab.font_size_ui}
            max_lines: 1
            text_overflow: TextOverflow.Ellipsis
        }
        draw_icon +: {svg: crate_resource("self://resources/icons/spark.svg")}
        draw_chevron +: {svg: crate_resource("self://resources/icons/chevron_down.svg")}
        // no well and no face for the shown one: the caption's ground, a face only under the
        // pointer; the names in the body's ink, the shown one lit
        well: #x00000000
        selected_face: #x00000000
        hover_face: fab.color_button_hover
        accent: fab.color_accent
        ink: fab.color_text
        selected_ink: fab.color_text_active
        color_cue: fab.color_text_active
        rename_input: mod.widgets.TextInput{
            a11y: {label: "Rename workspace"}
            padding: Inset{left: 6 right: 6 top: 1 bottom: 1}
            draw_text +: {text_style: theme.font_regular {font_size: theme.prop_font_size}}
        }
    }
}

/// What the row asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceTabAction {
    /// A tab clicked: show it.
    Pick(WorkspaceId),
    /// The temporary layout's ×: go back.
    Dismiss,
    /// A tab renamed in place.
    Rename(WorkspaceId, String),
    /// A tab's menu, under it.
    Menu(WorkspaceId, Rect),
    /// `+`'s menu, under it.
    Add(Rect),
    /// `⌄`'s list, under it.
    All(Rect),
    /// A tab dragged to place `to` of the row.
    Move(WorkspaceId, usize),
}

/// A part of the row.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Tab(usize),
    Add,
    More,
    Temp,
    TempClose,
}

const TAB_H: f64 = 22.0;
const PAD: f64 = 2.0;
const GLYPH_W: f64 = 24.0;
const TEMP_ICON: f64 = 14.0;
const TEMP_CLOSE: f64 = 16.0;
const TEMP_TITLE_MAX: f64 = 200.0;
/// The name's top below its tab's.
const NAME_Y: f64 = 4.5;
/// A press that moves this far is a drag.
const DRAG_START: f64 = 6.0;

#[derive(Script, ScriptHook, Widget)]
pub struct FabWorkspaceTabs {
    #[uid]
    uid: WidgetUid,
    #[source]
    script_source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_face: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_key: DrawText,
    #[live]
    draw_cue: DrawText,
    #[live]
    draw_icon: DrawSvg,
    /// The list's chevron, an icon on the row's middle (a `⌄` glyph sits low in its line).
    #[live]
    draw_chevron: DrawSvg,
    #[live]
    well: Vec4f,
    #[live]
    selected_face: Vec4f,
    #[live]
    hover_face: Vec4f,
    #[live]
    accent: Vec4f,
    #[live]
    ink: Vec4f,
    #[live]
    selected_ink: Vec4f,
    #[live]
    color_cue: Vec4f,
    #[find]
    #[live]
    rename_input: WidgetRef,
    #[redraw]
    #[rust]
    area: Area,
    /// The offered workspaces, in order, and the one shown.
    #[rust]
    tabs: Vec<(WorkspaceId, String)>,
    #[rust]
    current: WorkspaceId,
    #[rust]
    temp: Option<TempTab>,
    /// Each tab's rect as last laid out, in the row's content (before the scroll).
    #[rust]
    parts: Vec<Rect>,
    #[rust]
    add: Rect,
    #[rust]
    more: Option<Rect>,
    #[rust]
    temp_rect: Rect,
    #[rust]
    temp_close: Rect,
    /// The tabs' room on screen, and how far they are scrolled.
    #[rust]
    room: f64,
    #[rust]
    scroll: f64,
    /// The shown tab's place last scrolled into view.
    #[rust]
    revealed: Option<WorkspaceId>,
    #[rust]
    hover: Option<Part>,
    #[rust]
    pressed: Option<(Part, DVec2)>,
    /// A tab being dragged: which, where the pointer is, and the place it would land.
    #[rust]
    drag: Option<(usize, DVec2, usize)>,
    #[rust]
    editing: Option<WorkspaceId>,
    #[rust]
    focus_input: bool,
    #[rust]
    cue: Option<Cue>,
    /// Each tab's and control's rect as drawn: the semantic tree reads a part's bounds
    /// from its area once the caption has aligned the row (a rect given while drawing would be
    /// where the row stood before that).
    #[rust]
    part_areas: Vec<Area>,
    #[rust]
    add_area: Area,
    #[rust]
    more_area: Area,
    #[rust]
    temp_area: Area,
    #[rust]
    temp_close_area: Area,
}

/// A workspace tab's part key.
fn tab_key(id: WorkspaceId) -> u64 {
    live_id!(fab_workspace).0 ^ (id.0 as u64 + 1)
}

fn ai_ink(cx: &mut Cx) -> Vec4f {
    let ai = fab_elec_view::canvas::looks::Tokens::for_theme(base_theme(cx) != BaseTheme::Light).ai;
    vec4(ai[0], ai[1], ai[2], ai[3])
}

impl FabWorkspaceTabs {
    /// Sets the caption's right side, where the row ends.
    /// The row as the list has it now; a redraw only when it changed. O(tabs).
    pub fn set(&mut self, cx: &mut Cx, tabs: Vec<(WorkspaceId, String)>, current: WorkspaceId, temp: Option<TempTab>) {
        let temp_changed = self.temp != temp;
        if self.tabs != tabs || self.current != current || temp_changed {
            if temp_changed && self.temp.as_ref().is_some_and(|t| self.editing == Some(t.id)) {
                self.editing = None;
                self.focus_input = false;
                let input = self.rename_input.area();
                if !input.is_empty() && cx.has_key_focus(input) {
                    cx.set_key_focus(self.area);
                }
            }
            self.tabs = tabs;
            self.current = current;
            self.temp = temp;
            self.area.redraw(cx);
        }
    }

    fn tab_name(&self, id: WorkspaceId) -> Option<&str> {
        self.temp.as_ref().filter(|t| t.id == id).map(|t| t.title.as_str())
            .or_else(|| self.tabs.iter().find(|(t, _)| *t == id).map(|(_, name)| name.as_str()))
    }

    /// The name of `id`'s tab in an input in its place.
    pub fn begin_rename(&mut self, cx: &mut Cx, id: WorkspaceId) {
        let Some(name) = self.tab_name(id) else { return };
        if let Some(mut input) = self.rename_input.borrow_mut::<TextInput>() {
            input.set_text(cx, name);
            input.select_all(cx);
            self.editing = Some(id);
            self.focus_input = true;
            self.area.redraw(cx);
        }
    }

    fn origin(&self, cx: &Cx) -> DVec2 {
        self.area.rect(cx).pos
    }

    /// A tab's rect on screen (scrolled).
    fn tab_rect(&self, cx: &Cx, i: usize) -> Option<Rect> {
        let r = *self.parts.get(i)?;
        Some(Rect { pos: r.pos + self.origin(cx) - dvec2(self.scroll, 0.0), ..r })
    }

    fn on_screen(&self, cx: &Cx, r: Rect) -> Rect {
        Rect { pos: r.pos + self.origin(cx), ..r }
    }

    fn part_at(&self, cx: &Cx, abs: DVec2) -> Option<Part> {
        let o = self.origin(cx);
        let local = abs - o;
        if self.temp.is_some() && self.temp_rect.contains(local) {
            return Some(if self.temp_close.contains(local) { Part::TempClose } else { Part::Temp });
        }
        if self.more.is_some_and(|r| r.contains(local)) {
            return Some(Part::More);
        }
        if self.add.contains(local) {
            return Some(Part::Add);
        }
        if local.x < PAD || local.x > PAD + self.room {
            return None;
        }
        (0..self.parts.len()).find(|i| self.tab_rect(cx, *i).is_some_and(|r| r.contains(abs))).map(Part::Tab)
    }

    fn workspace_tab(&self, cx: &Cx, part: Part) -> Option<(WorkspaceId, Rect)> {
        match part {
            Part::Tab(i) => Some((self.tabs.get(i)?.0, self.tab_rect(cx, i)?)),
            Part::Temp | Part::TempClose => Some((self.temp.as_ref()?.id, self.on_screen(cx, self.temp_rect))),
            _ => None,
        }
    }

    /// The place a tab dropped at `x` lands: before the first tab whose middle is past it.
    fn place_at(&self, cx: &Cx, x: f64) -> usize {
        (0..self.parts.len()).find(|i| self.tab_rect(cx, *i).is_some_and(|r| x < r.pos.x + r.size.x * 0.5)).unwrap_or(self.parts.len())
    }

    fn rename_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // After Enter or Esc the input still needs its focus loss to close the IME and stop the caret.
        let losing_focus = matches!(event, Event::KeyFocus(e) if !e.prev.is_empty() && e.prev == self.rename_input.area());
        if self.editing.is_none() && !losing_focus {
            return;
        }
        let actions = cx.capture_actions(|cx| self.rename_input.handle_event(cx, event, scope));
        let Some(id) = self.editing else { return };
        for action in actions.filter_widget_actions(self.rename_input.widget_uid()) {
            match action.action.downcast_ref::<TextInputAction>() {
                Some(TextInputAction::Escaped) => cx.set_key_focus(self.area),
                Some(TextInputAction::Returned(..) | TextInputAction::KeyFocusLost) => {
                    let text = self.rename_input.text();
                    let name = text.trim();
                    if !name.is_empty() && self.tab_name(id).is_some_and(|old| old != name) {
                        cx.action(WorkspaceTabAction::Rename(id, name.to_owned()));
                    }
                }
                _ => continue,
            }
            self.editing = None;
            self.area.redraw(cx);
            break;
        }
    }

    fn face(&mut self, cx: &mut Cx2d, rect: Rect, color: Vec4f) {
        self.draw_face.color = color;
        self.draw_face.draw_abs(cx, rect);
    }

    fn draw_rename(&mut self, cx: &mut Cx2d, scope: &mut Scope, rect: Rect) {
        self.rename_input.draw_walk_all(cx, scope, Walk::abs_rect(rect));
        if std::mem::take(&mut self.focus_input) {
            self.rename_input.as_text_input().take_key_focus_from_user(cx);
        }
    }

    fn glyph(&mut self, cx: &mut Cx2d, rect: Rect, text: &str, lit: bool) {
        if lit {
            self.face(cx, rect, self.hover_face);
        }
        self.draw_text.color = self.ink;
        let size = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), text).size_in_lpxs * self.draw_text.font_scale;
        self.draw_text.draw_abs(cx, rect.pos + (rect.size - dvec2(size.width.into(), size.height.into())) * 0.5, text);
    }

    /// The cue and temporary title share a single-line layout with an ellipsis at its bound.
    fn note_layout(&self, cx: &mut Cx2d, text: &str, room: f64) -> Rc<LaidoutText> {
        let width = room as f32 / self.draw_cue.font_scale.max(0.0001);
        self.draw_cue.layout(cx, 0.0, 0.0, Some(width), false, Align::default(), text)
    }

    fn draw_note(&mut self, cx: &mut Cx2d, at: DVec2, layout: &LaidoutText, color: Vec4f) {
        let size = layout.size_in_lpxs * self.draw_cue.font_scale;
        self.draw_cue.color = color;
        self.draw_cue.draw_walk_laidout(cx, Walk::fixed(size.width.into(), size.height.into()).with_abs_pos(at), layout);
    }

    fn draw_temp(&mut self, cx: &mut Cx2d, scope: &mut Scope, rect: Rect, title: &LaidoutText) {
        let Some(temp) = &self.temp else { return };
        let (id, by_ai) = (temp.id, temp.ai);
        self.temp_rect = rect;
        self.temp_close = Rect {
            pos: rect.pos + dvec2(rect.size.x - 4.0 - TEMP_CLOSE, (TAB_H - TEMP_CLOSE) * 0.5),
            size: dvec2(TEMP_CLOSE, TEMP_CLOSE),
        };
        let (tab, close) = (self.on_screen(cx, rect), self.on_screen(cx, self.temp_close));
        cx.add_rect_area(&mut self.temp_area, tab);
        cx.add_rect_area(&mut self.temp_close_area, close);
        if self.editing == Some(id) {
            self.draw_rename(cx, scope, tab);
            return;
        }
        let ai = ai_ink(cx);
        let selected = self.current == id;
        let alpha = if selected { 0.30 } else if matches!(self.hover, Some(Part::Temp | Part::TempClose)) { 0.22 } else { 0.14 };
        self.face(cx, tab, vec4(ai.x, ai.y, ai.z, alpha));
        self.draw_icon.color = if by_ai { ai } else { self.ink };
        self.draw_icon.draw_abs(cx, Rect {
            pos: tab.pos + dvec2(8.0, (TAB_H - TEMP_ICON) * 0.5),
            size: dvec2(TEMP_ICON, TEMP_ICON),
        });
        let at = tab.pos + dvec2(8.0 + TEMP_ICON + 6.0, NAME_Y);
        self.draw_note(cx, at, title, if selected { self.selected_ink } else { self.ink });
        self.glyph(cx, close, "×", self.hover == Some(Part::TempClose));
    }

    /// The cue's words ending at `right` (the menu sits among the caption's right-hand controls, so
    /// its cue reads before it), cut short to `room`, fading at the end.
    fn draw_cue(&mut self, cx: &mut Cx2d, right: f64, y: f64, room: f64) {
        let Some(cue) = &self.cue else { return };
        let Some(alpha) = cue.alpha(Cx::monotonic_now()) else {
            self.cue = None;
            return;
        };
        if room >= 24.0 {
            let color = match cue.origin {
                EditOrigin::Ai | EditOrigin::Agent => ai_ink(cx),
                EditOrigin::Disk | EditOrigin::Elsewhere => self.color_cue,
            };
            let words = self.note_layout(cx, &cue.words(), room);
            let width = f64::from(words.size_in_lpxs.width * self.draw_cue.font_scale);
            let at = dvec2(right - width.min(room), y);
            self.draw_note(cx, at, &words, vec4(color.x, color.y, color.z, color.w * alpha));
        }
        cx.new_next_frame();
    }
}

/// How wide the cue before the menu may run.
const CUE_ROOM: f64 = 260.0;

impl Widget for FabWorkspaceTabs {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // the tabs' natural widths: the name, and the key's tag on the first nine
        let mut x = 0.0;
        self.parts.clear();
        for (i, (id, name)) in self.tabs.iter().enumerate() {
            let label = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), name);
            let mut w = 24.0 + f64::from(label.size_in_lpxs.width * self.draw_text.font_scale);
            if i < 9 {
                w += 10.0;
            }
            if self.editing == Some(*id) {
                w = w.max(120.0);
            }
            self.parts.push(Rect { pos: dvec2(x + PAD, PAD), size: dvec2(w, TAB_H) });
            x += w + PAD;
        }
        let natural = x;
        let temp = self.temp.as_ref().map(|t| {
            let title = self.note_layout(cx, &t.title, TEMP_TITLE_MAX);
            let text_width = f64::from(title.size_in_lpxs.width * self.draw_cue.font_scale);
            let width = 8.0 + TEMP_ICON + 6.0 + text_width + 8.0 + TEMP_CLOSE + 4.0;
            (title, if self.editing == Some(t.id) { width.max(120.0) } else { width })
        });
        let temp_space = temp.as_ref().map_or(0.0, |(_, width)| width + PAD);
        // No tabs show: one button names the shown workspace and lists them all (`⌄`'s menu); `+`
        // and a temporary layout's tab follow it. Cmd/Ctrl and a number still switch.
        self.parts.clear();
        self.room = 0.0;
        let shown_name = self.tabs.iter().find(|(id, _)| *id == self.current).map(|(_, name)| name.clone())
            .or_else(|| self.temp.as_ref().filter(|t| t.id == self.current).map(|t| t.title.clone()))
            .unwrap_or_default();
        let name_w = {
            let laid = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &shown_name);
            f64::from(laid.size_in_lpxs.width * self.draw_text.font_scale)
        };
        let more_w = (12.0 + name_w + 4.0 + GLYPH_W).ceil();
        let max_scroll = (natural - self.room).max(0.0);
        // the shown tab comes into view when it changes
        if self.temp.as_ref().is_some_and(|t| t.id == self.current) {
            self.revealed = None;
        } else if self.revealed != Some(self.current) {
            self.revealed = Some(self.current);
            if let Some(r) = self.tabs.iter().position(|(id, _)| *id == self.current).and_then(|i| self.parts.get(i)) {
                if r.pos.x < self.scroll {
                    self.scroll = r.pos.x - PAD;
                } else if r.pos.x + r.size.x > self.scroll + self.room {
                    self.scroll = r.pos.x + r.size.x - self.room + PAD;
                }
            }
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let width = PAD + more_w + GLYPH_W + PAD + temp_space;
        // Align the caption's controls as one row, including the note and inline input.
        // The cue and drag chip can extend beyond it. Only the scrolling well clips its tabs.
        let layout = Layout { clip_x: false, clip_y: false, ..Layout::flow_overlay() };
        cx.begin_turtle(Walk { width: Size::Fixed(width), ..walk }, layout);
        let row = cx.turtle().rect();
        cx.add_rect_area(&mut self.area, row);
        self.more = Some(Rect { pos: dvec2(PAD, PAD), size: dvec2(more_w, TAB_H) });
        self.add = Rect { pos: dvec2(PAD + more_w, PAD), size: dvec2(GLYPH_W, TAB_H) };
        let clip = Rect { pos: row.pos + dvec2(PAD, 0.0), size: dvec2(self.room, row.size.y) };
        let dragging = self.drag.map(|(from, _, _)| from);
        // the key's number sits on the name's baseline: its top drops by what the name's baseline
        // lies below the key's (two cached layouts per draw)
        let baseline = |cx: &mut Cx2d, draw: &mut DrawText, text: &str| {
            let laid = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
            laid.rows.first().map_or(0.0, |row| f64::from(row.origin_in_lpxs.y * draw.font_scale))
        };
        let key_y = NAME_Y + baseline(cx, &mut self.draw_text, "Design") - baseline(cx, &mut self.draw_key, "1");
        // the tabs clipped to their room: one cut at an end shows as far as it fits
        cx.begin_turtle(Walk::abs_rect(clip), Layout::flow_overlay());
        self.part_areas.resize(self.parts.len(), Area::Empty);
        for i in 0..self.parts.len() {
            let Some(r) = self.tab_rect(cx, i) else { continue };
            cx.add_rect_area(&mut self.part_areas[i], r);
            if r.pos.x + r.size.x <= clip.pos.x || r.pos.x >= clip.pos.x + clip.size.x {
                continue;
            }
            let (id, name) = self.tabs[i].clone();
            if self.editing == Some(id) {
                self.draw_rename(cx, scope, r);
                continue;
            }
            let selected = id == self.current;
            if selected {
                self.face(cx, r, self.selected_face);
            } else if self.hover == Some(Part::Tab(i)) || dragging == Some(i) {
                self.face(cx, r, self.hover_face);
            }
            let mut tx = r.pos.x + 12.0;
            if i < 9 {
                self.draw_key.draw_abs(cx, dvec2(tx, r.pos.y + key_y), &format!("{}", i + 1));
                tx += 10.0;
            }
            self.draw_text.color = if selected { self.selected_ink } else { self.ink };
            self.draw_text.draw_abs(cx, dvec2(tx, r.pos.y + NAME_Y), &name);
        }
        cx.end_turtle();
        let (add, more) = (self.on_screen(cx, self.add), self.more.map(|m| self.on_screen(cx, m)));
        cx.add_rect_area(&mut self.add_area, add);
        self.glyph(cx, add, "+", self.hover == Some(Part::Add));
        if let Some(more) = more {
            cx.add_rect_area(&mut self.more_area, more);
            // the shown workspace's name in its well, then the list's chevron
            self.face(cx, more, if self.hover == Some(Part::More) { self.hover_face } else { self.well });
            self.draw_text.color = self.selected_ink;
            self.draw_text.draw_abs(cx, dvec2(more.pos.x + 12.0, more.pos.y + NAME_Y), &shown_name);
            // a 10 pt chevron centred in the glyph's cell, on the row's middle as the `+`
            let size = 10.0;
            let cell = Rect { pos: dvec2(more.pos.x + more.size.x - GLYPH_W, more.pos.y), size: dvec2(GLYPH_W, TAB_H) };
            self.draw_chevron.color = self.ink;
            self.draw_chevron.draw_abs(cx, Rect { pos: cell.pos + (cell.size - dvec2(size, size)) * 0.5, size: dvec2(size, size) });
        } else {
            self.more_area = Area::Empty;
        }
        if let Some((title, tab_width)) = temp {
            let rect = Rect { pos: dvec2(width - temp_space, PAD), size: dvec2(tab_width, TAB_H) };
            self.draw_temp(cx, scope, rect, &title);
        } else {
            self.temp_area = Area::Empty;
            self.temp_close_area = Area::Empty;
        }
        // a reorder: a line where the tab would land, the tab's chip at the pointer
        if let Some((from, at, place)) = self.drag {
            let x = match self.tab_rect(cx, place) {
                Some(r) => r.pos.x - PAD * 0.5,
                None => self.tab_rect(cx, self.parts.len().saturating_sub(1)).map_or(clip.pos.x, |r| r.pos.x + r.size.x + PAD * 0.5),
            };
            let x = x.clamp(clip.pos.x, clip.pos.x + clip.size.x);
            self.face(cx, Rect { pos: dvec2(x - 1.0, row.pos.y + 1.0), size: dvec2(2.0, row.size.y - 2.0) }, self.accent);
            if let (Some(r), Some((_, name))) = (self.parts.get(from).copied(), self.tabs.get(from).cloned()) {
                let chip = Rect { pos: dvec2(at.x - r.size.x * 0.5, row.pos.y + PAD), size: r.size };
                self.face(cx, chip, self.selected_face);
                self.draw_text.color = self.selected_ink;
                self.draw_text.draw_abs(cx, dvec2(chip.pos.x + 12.0, chip.pos.y + 4.5), &name);
            }
        }
        self.draw_cue(cx, row.pos.x - 10.0, row.pos.y + 6.5, CUE_ROOM);
        cx.end_turtle();
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let (Event::NextFrame(_), Some(_)) = (event, &self.cue) {
            self.area.redraw(cx);
        }
        if let Event::Actions(actions) = event {
            for edit in actions.iter().filter_map(|a| a.downcast_ref::<ElecOutsideEdit>()) {
                Cue::hear(&mut self.cue, edit, Cx::monotonic_now());
                self.area.redraw(cx);
            }
        }
        self.rename_event(cx, event, scope);
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let over = self.part_at(cx, e.abs);
                cx.set_cursor(if over.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
                if over != self.hover {
                    self.hover = over;
                    self.area.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.hover.take().is_some() {
                    self.area.redraw(cx);
                }
            }
            Hit::FingerScroll(e) => {
                let by = if e.scroll.x != 0.0 { e.scroll.x } else { e.scroll.y };
                self.scroll += by;
                self.area.redraw(cx);
            }
            Hit::FingerDown(e) => {
                let part = self.part_at(cx, e.abs);
                let tab = part.and_then(|p| self.workspace_tab(cx, p));
                if let (false, Some((id, rect))) = (e.device.is_primary_hit(), tab) {
                    cx.action(WorkspaceTabAction::Menu(id, rect));
                    self.pressed = None;
                    return;
                }
                match (e.tap_count, part) {
                    (2, Some(Part::Tab(_) | Part::Temp)) if self.editing.is_none() => {
                        self.pressed = None;
                        if let Some((id, _)) = tab {
                            self.begin_rename(cx, id);
                        }
                    }
                    (_, Some(part)) => self.pressed = Some((part, e.abs)),
                    _ => self.pressed = None,
                }
            }
            Hit::FingerLongPress(e) => {
                if self.drag.is_none() {
                    if let Some((id, rect)) = self.part_at(cx, e.abs).and_then(|p| self.workspace_tab(cx, p)) {
                        cx.action(WorkspaceTabAction::Menu(id, rect));
                        self.pressed = None;
                    }
                }
            }
            Hit::FingerMove(e) => {
                if let Some((Part::Tab(i), start)) = self.pressed {
                    if self.drag.is_some() || (e.abs - start).length() > DRAG_START {
                        let place = self.place_at(cx, e.abs.x);
                        self.drag = Some((i, e.abs, place));
                        cx.set_cursor(MouseCursor::Hand);
                        self.area.redraw(cx);
                    }
                }
            }
            Hit::FingerUp(e) => {
                let pressed = self.pressed.take();
                if let Some((from, _, place)) = self.drag.take() {
                    // the place among the others once it is out of the row
                    let to = if place > from { place - 1 } else { place };
                    if let (Some((id, _)), true) = (self.tabs.get(from), to != from) {
                        cx.action(WorkspaceTabAction::Move(*id, to));
                    }
                    self.area.redraw(cx);
                    return;
                }
                if !(e.is_over && e.was_tap()) {
                    return;
                }
                let over = self.part_at(cx, e.abs);
                match (pressed.map(|p| p.0), over) {
                    (Some(Part::Tab(i)), Some(Part::Tab(j))) if i == j => {
                        if let Some((id, _)) = self.tabs.get(i) {
                            cx.action(WorkspaceTabAction::Pick(*id));
                        }
                    }
                    (Some(Part::Temp), Some(Part::Temp)) => {
                        if let Some(temp) = &self.temp {
                            cx.action(WorkspaceTabAction::Pick(temp.id));
                        }
                    }
                    (Some(Part::TempClose), Some(Part::TempClose)) => cx.action(WorkspaceTabAction::Dismiss),
                    (Some(Part::Add), Some(Part::Add)) => cx.action(WorkspaceTabAction::Add(self.on_screen(cx, self.add))),
                    (Some(Part::More), Some(Part::More)) => {
                        if let Some(more) = self.more {
                            cx.action(WorkspaceTabAction::All(self.on_screen(cx, more)));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Workspaces");
    }

    /// The workspace tabs and controls in their on-screen order.
    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let mut button = |key, rect, name, selected| {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                bounds: makepad_platform::a11y::semantic_rect(rect),
                name,
                actions: a11y_tree::Actions::PRESS,
                ..Default::default()
            };
            node.states.set(a11y_tree::States::SELECTED, selected);
            out.push((key, None, node));
        };
        for i in 0..self.parts.len() {
            let (Some((id, name)), Some(r)) = (self.tabs.get(i), self.tab_rect(cx, i)) else { continue };
            button(tab_key(*id), r, name.clone(), *id == self.current);
        }
        button(live_id!(fab_workspace_add).0, self.on_screen(cx, self.add), "New workspace".into(), false);
        if let Some(rect) = self.more {
            button(live_id!(fab_workspace_all).0, self.on_screen(cx, rect), "All workspaces".into(), false);
        }
        if let Some(temp) = &self.temp {
            button(live_id!(fab_workspace_temp).0, self.on_screen(cx, self.temp_rect), format!("Temporary layout: {}", temp.title), self.current == temp.id);
            button(live_id!(fab_workspace_temp_close).0, self.on_screen(cx, self.temp_close), "Back".into(), false);
        }
    }

    fn semantics_part_area(&self, key: u64) -> Area {
        if key == live_id!(fab_workspace_add).0 {
            return self.add_area;
        }
        if key == live_id!(fab_workspace_all).0 {
            return self.more_area;
        }
        if key == live_id!(fab_workspace_temp).0 {
            return self.temp_area;
        }
        if key == live_id!(fab_workspace_temp_close).0 {
            return self.temp_close_area;
        }
        self.tabs.iter().position(|(id, _)| tab_key(*id) == key).and_then(|i| self.part_areas.get(i).copied()).unwrap_or(Area::Empty)
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        let Some(key) = cx.a11y.action_part_key(request.node) else { return a11y_tree::ActionResult::Unhandled };
        if request.action != a11y_tree::Actions::PRESS {
            return a11y_tree::ActionResult::Unsupported;
        }
        if key == live_id!(fab_workspace_add).0 {
            cx.action(WorkspaceTabAction::Add(self.on_screen(cx, self.add)));
        } else if let (true, Some(more)) = (key == live_id!(fab_workspace_all).0, self.more) {
            cx.action(WorkspaceTabAction::All(self.on_screen(cx, more)));
        } else if let (true, Some(temp)) = (key == live_id!(fab_workspace_temp).0, &self.temp) {
            cx.action(WorkspaceTabAction::Pick(temp.id));
        } else if key == live_id!(fab_workspace_temp_close).0 && self.temp.is_some() {
            cx.action(WorkspaceTabAction::Dismiss);
        } else if let Some((id, _)) = self.tabs.iter().find(|(id, _)| tab_key(*id) == key) {
            cx.action(WorkspaceTabAction::Pick(*id));
        } else {
            return a11y_tree::ActionResult::Unhandled;
        }
        a11y_tree::ActionResult::Handled
    }
}
