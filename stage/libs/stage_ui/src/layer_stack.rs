//! A VJ side's layer stack: rows top first (a live thumbnail, the name,
//! what the layer does, its opacity, blend and eye; an adjustment layer a
//! slim row of its name, what it does, its switch and a thin slider for its
//! strength; a spanned layer a
//! SPAN chip, a failed one a red edge and its reason as the row's tip), the
//! crossfade line under the shared overlays, the header's add, adjustment,
//! span and remove buttons and under them why the stack refused the last
//! ask ([`LayerStackViewRef::set_status`]). It draws what the host gives it
//! ([`LayerStackRow`]) and reports what was asked ([`LayerStackAction`]);
//! the host owns the stacks.
//!
//! Rows are DISPLAY order, top first, as the host hands them over and as
//! every index this widget reports back is; the host converts to its own
//! stack order. A row is dragged by its grip ([`LayerStackAction::Move`]).
//! A tile dragged in from elsewhere is the host's gesture: the host asks
//! [`LayerStackViewRef::drop_row`] where it would land and shows that with
//! [`LayerStackViewRef::set_drop_gap`], the gap the list opens for its own
//! rows.
//!
//! Registered by `makepad_stage_ui::script_mod`, which the host calls after
//! `makepad_live_frame::script_mod`: the content row composes
//! `LiveFrameView`.

use makepad_live_frame::{FrameBinding, FrameSource, LiveFrameViewWidgetRefExt};
use makepad_widgets::tip::TipWidgetRefExt;
use makepad_widgets::*;
use std::rc::Rc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    // A name is the operator's own title: the one thing a row may cut.
    let RowName = Label{width: Fill text: "" padding: 0 max_lines: 1 text_overflow: TextOverflow.Ellipsis
        draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}
    // What the layer does is never cut: a long one wraps.
    let RowDetail = Label{width: Fill text: "" padding: 0 flow: Flow.Right{wrap: true}
        draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7}}}
    let AdjustDetail = Label{width: Fit text: "" padding: 0
        draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7}}}
    // A spanned layer's mark: the word in a chip at the end of the name
    // line, so cutting the name never takes it.
    let SpanBadge = RoundedView{visible: false width: Fit height: 12 padding: Inset{left: 3 right: 3} align: Align{y: 0.5}
        show_bg: true draw_bg +: {color: #0000 border_color: stage.accent border_size: 1.0 border_radius: 2.0}
        Label{width: Fit text: "SPAN" padding: 0 draw_text +: {color: stage.accent text_style: theme.font_bold{font_size: 6}}}
    }
    // The line between the shared overlays and the layers the crossfader
    // mixes: shown on the first row under the overlays, a rule drawn as the
    // app draws its others.
    let CrossLine = View{visible: false width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 0.5} margin: Inset{top: 1 bottom: 3}
        Label{width: Fit text: "CROSSFADE" padding: 0 draw_text +: {color: stage.label text_style: theme.font_bold{font_size: 7}}}
        SolidView{width: Fill height: 1 draw_bg +: {color: stage.label}}
    }
    // Three bars, drawn, so the grip needs no glyph the UI font lacks.
    let LayerGrip = View{width: 8 height: Fill cursor: MouseCursor.Grab show_bg: true
        draw_bg +: {color: stage.dim pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let y = self.rect_size.y * 0.5
            sdf.rect(1., y - 3.5, self.rect_size.x - 2., 1.) sdf.fill(self.color)
            sdf.rect(1., y - 0.5, self.rect_size.x - 2., 1.) sdf.fill(self.color)
            sdf.rect(1., y + 2.5, self.rect_size.x - 2., 1.) sdf.fill(self.color)
            return sdf.result
        }}
    }
    // The eye: open with a pupil while the layer shows, struck through while
    // it does not. A CheckBox, as the app's play toggle is.
    let LayerEye = CheckBox{
        width: 20 height: 14 margin: 0 padding: 0 text: ""
        icon_walk: Walk{width: 0 height: 0}
        label_walk: Walk{width: 0 height: 0 margin: 0}
        draw_bg +: {
            glyph: uniform(stage.text) glyph_off: uniform(stage.dim) glyph_hover: uniform(stage.accent)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let c = self.rect_size * 0.5
                let ink = self.glyph_off.mix(self.glyph, self.active).mix(self.glyph_hover, self.hover * 0.6)
                sdf.ellipse(c.x, c.y, 6.5, 3.5)
                sdf.stroke(ink, 1.2)
                if self.active > 0.5 {
                    sdf.circle(c.x, c.y, 1.8)
                    sdf.fill(ink)
                } else {
                    sdf.move_to(c.x - 5., c.y + 4.)
                    sdf.line_to(c.x + 5., c.y - 4.)
                    sdf.stroke(ink, 1.2)
                }
                return sdf.result
            }
        }
    }
    // A double tap resets it to its default: fully shown, never hidden.
    let LayerOpacity = StageHSlider{width: Fill height: 14 margin: 0 default: 1.0 scroll_step: 0.025 label_walk: Walk{width: 0 height: 0 margin: 0}}
    // An adjustment's strength: the opacity slider drawn thin, its track a
    // hairline and its cap a short tick, so the slim row stays slim. A
    // double tap puts it back at full strength.
    let AdjustStrength = LayerOpacity{height: 10
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let track_h = 2.
                let track_y = (self.rect_size.y - track_h) * 0.5
                sdf.box(1., track_y, self.rect_size.x - 2., track_h, 1.)
                sdf.fill(self.track_color)
                let cap_w = 6.
                let left = 1. + cap_w * 0.5
                let w = self.rect_size.x - 2. - cap_w
                let fill_w = max(1., w * self.slide_pos + cap_w * 0.5)
                sdf.box(1., track_y, fill_w, track_h, 1.)
                sdf.fill(self.fill_color)
                let cap_x = left + w * self.slide_pos - cap_w * 0.5
                sdf.box(cap_x, 1., cap_w, self.rect_size.y - 2., 2.)
                sdf.fill(self.cap_color)
                return sdf.result * (1.0 - self.inert * 0.72)
            }
        }
    }
    // The console's thin dropdown, a size down to sit in a row; MULTIPLY
    // fits with room to spare.
    let LayerBlend = DropDown{
        width: 72 height: 16 margin: 0 padding: Inset{left: 4 right: 12 top: 0 bottom: 0}
        labels: ["NORMAL" "ADD" "SCREEN" "MULTIPLY" "ALPHA" "MASK"]
        draw_bg +: {
            color: stage.control color_hover: stage.control_hover color_down: stage.control_down
            color_disabled: stage.control_down border_color: stage.border_hi
            arrow_color: stage.label arrow_color_disabled: stage.dim_lo
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1., self.rect_size.y - 1., 2.5)
                sdf.fill(self.color.mix(self.color_hover, self.hover).mix(self.color_down, self.down).mix(self.color_disabled, self.disabled))
                sdf.stroke(self.border_color, 1.)
                let x = self.rect_size.x - 6.
                let y = self.rect_size.y * 0.5
                sdf.move_to(x - 2., y - 1.) sdf.line_to(x + 2., y - 1.) sdf.line_to(x, y + 1.5)
                sdf.close_path() sdf.fill(self.arrow_color.mix(self.arrow_color_disabled, self.disabled))
                return sdf.result
            }
        }
        draw_text +: {color: stage.label color_hover: stage.text_hi color_focus: stage.label color_down: stage.text_hi color_disabled: stage.dim_lo text_style: theme.font_regular{font_size: 7}}
        popup_menu: PopupMenu{
            draw_bg +: {color: stage.menu border_color: stage.border_hi}
            menu_item: PopupMenuItem{
                draw_bg +: {color_hover: stage.control_hover color_active: stage.accent}
                draw_text +: {color: stage.label color_hover: stage.text_hi color_active: stage.on_accent}
            }
        }
    }
    let HeadButton = StageButton{height: 22 margin: 0 padding: Inset{left: 5 right: 5}}

    // A content layer: its picture; then its name, span mark and eye; what
    // it does; its opacity and blend. The row's tip says what it does, or
    // why it failed (its edge red).
    mod.widgets.StageLayerRow = View{
        width: Fill height: Fit flow: Down margin: Inset{bottom: 3}
        cross_line := CrossLine{}
        row_tip := Tip{text: ""
            body := RoundedView{
                width: Fill height: Fit flow: Right spacing: 4 padding: 3 align: Align{y: 0.5}
                cursor: MouseCursor.Hand
                draw_bg +: {color: stage.row border_color: stage.line border_size: 1.0 border_radius: 3.0}
                Tip{text: "Drag to move this layer up or down the stack" layer_grip := LayerGrip{height: 44}}
                layer_thumb := LiveFrameView{width: 44 height: 25 fit: FrameFit.Cover
                    draw_frame +: {color: stage.cell border_radius: 2.0}
                    draw_status +: {color: #0000}
                }
                View{width: Fill height: Fit flow: Down spacing: 2
                    View{width: Fill height: Fit flow: Right spacing: 3 align: Align{y: 0.5}
                        layer_name := RowName{}
                        span_badge := SpanBadge{}
                        Tip{text: "Show or hide this layer" layer_eye := LayerEye{}}
                    }
                    layer_detail := RowDetail{}
                    View{width: Fill height: Fit flow: Right spacing: 3 align: Align{y: 0.5}
                        Tip{text: "How much of this layer shows over the ones under it" layer_opacity := LayerOpacity{}}
                        Tip{text: "How this layer lies over the ones under it" layer_blend := LayerBlend{}}
                    }
                }
            }
        }
    }

    // An adjustment layer: an effect over everything under it, so a slim
    // row of its name, its strength in words, its span mark and its switch,
    // with its strength's thin slider under them.
    mod.widgets.StageAdjustRow = View{
        width: Fill height: Fit flow: Down margin: Inset{bottom: 3}
        cross_line := CrossLine{}
        row_tip := Tip{text: ""
            body := RoundedView{
                width: Fill height: Fit flow: Right spacing: 4 padding: Inset{left: 3 right: 3 top: 2 bottom: 2} align: Align{y: 0.5}
                cursor: MouseCursor.Hand
                draw_bg +: {color: stage.raised border_color: stage.line border_size: 1.0 border_radius: 3.0}
                Tip{text: "Drag to move this adjustment up or down the stack" layer_grip := LayerGrip{height: 26}}
                View{width: Fill height: Fit flow: Down spacing: 1
                    View{width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
                        layer_name := RowName{}
                        layer_detail := AdjustDetail{}
                        span_badge := SpanBadge{}
                        Tip{text: "Switch this adjustment on or off" layer_eye := LayerEye{}}
                    }
                    Tip{text: "How strongly this adjustment bends the layers under it" layer_opacity := AdjustStrength{}}
                }
            }
        }
    }

    mod.widgets.LayerStackViewBase = #(LayerStackView::register_widget(vm))
    mod.widgets.LayerStackView = set_type_default() do mod.widgets.LayerStackViewBase{
        width: 200 height: Fill flow: Down spacing: 4
        View{width: Fill height: Fit flow: Right spacing: 3 align: Align{y: 0.5}
            Tip{text: "Add a layer above the selected one; the next tile you click fills it" layer_add := HeadButton{width: 30 padding: 0 text: "+"}}
            Tip{text: "Add an adjustment layer: an effect over everything under it" layer_adjust := HeadButton{width: Fit text: "ADJUST"}}
            Tip{text: "Span the selected layer to the other side; again to part the copies" layer_span := HeadButton{width: Fit text: "SPAN"}}
            View{width: Fill height: 1}
            Tip{text: "Remove the selected layer" layer_remove := HeadButton{width: 22 padding: 0 text: "×"}}
        }
        // Why the stack refused the last ask (the host sets it, and clears
        // it on the next ask the stack takes). Its line is kept while it is
        // empty, so the rows never jump under the hand when it comes or goes.
        layer_status := Label{width: Fill height: Fit{min: 10} text: "" padding: 0 flow: Flow.Right{wrap: true}
            draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7}}}
        layer_list := ReorderList{
            width: Fill height: Fill drag_scrolling: false
            drag_handle: @layer_grip
            draw_indicator +: {color: stage.accent}
            LayerRow := mod.widgets.StageLayerRow{}
            AdjustRow := mod.widgets.StageAdjustRow{}
        }
    }
}

/// One layer as the panel shows it, in display order (top first).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerStackRow {
    pub name: String,
    /// What the layer does: its effect, its clip.
    pub detail: String,
    /// An adjustment layer: the slim row, no picture or blend.
    pub adjust: bool,
    /// The eye: the layer shows (an adjustment: it is switched on).
    pub on: bool,
    /// 0..1; an adjustment layer's strength.
    pub opacity: f64,
    /// Index into NORMAL, ADD, SCREEN, MULTIPLY, ALPHA, MASK.
    pub blend: usize,
    /// The layer is spanned to the other side: a SPAN chip ends its name line.
    pub spanned: bool,
    pub selected: bool,
    /// Why the layer draws nothing, when it failed: the row's tip, its edge red.
    pub failed: Option<String>,
}

/// What the panel asks the host for. Rows are display indices into the rows
/// the host handed over; the panel changes nothing itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum LayerStackAction {
    #[default]
    None,
    /// The row was clicked: make it the selected layer.
    Select(usize),
    /// The row's eye was clicked: show or hide it.
    Toggle(usize),
    /// The row's opacity slider (an adjustment row's strength) moved, to
    /// this value 0..1.
    Opacity { row: usize, value: f64 },
    /// The opacity slider let go: the host keeps the value.
    OpacityEnd(usize),
    /// The row's blend picked, an index as [`LayerStackRow::blend`].
    Blend { row: usize, blend: usize },
    /// The row dragged from one place to another: remove it at `from`, then
    /// insert it at `to` (already adjusted for the removal).
    Move { from: usize, to: usize },
    Add,
    AddAdjust,
    Span,
    Remove,
}

/// What a row's item was last given that costs to give again: the border
/// is a script eval, and a picture bound afresh lets its frame texture go,
/// so the thumbnail blanks until its next frame. The border is given again
/// when the item is a new one (the list made it, took it from its pool, or
/// the row changed kind), the border changes, or the palette is rebuilt;
/// the picture only when the item is a new one or `framed` was cleared,
/// which [`LayerStackViewRef::set_rows`] does only for a new stamp or a
/// new shape of rows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Painted {
    border: u32,
    generation: u64,
    framed: bool,
}

#[derive(Script, ScriptHook, Widget)]
pub struct LayerStackView {
    #[deref]
    view: View,
    #[rust]
    rows: Vec<LayerStackRow>,
    /// How many of the rows, from the top, are the shared overlays.
    #[rust]
    overlays: usize,
    #[rust]
    frames: Vec<Option<(Rc<dyn FrameSource>, FrameBinding)>>,
    /// The host's generation `frames` were read from ([`LayerStackViewRef::set_rows`]).
    #[rust]
    stamp: u64,
    /// Per row; `None` paints it on its next draw.
    #[rust]
    painted: Vec<Option<Painted>>,
}

impl LayerStackView {
    /// The colour of a row's edge: a failed layer's in the colour of a
    /// fault, the selected one's in the accent.
    fn border(palette: &makepad_look::palette::Palette, row: &LayerStackRow) -> u32 {
        if row.failed.is_some() {
            palette.danger
        } else if row.selected {
            palette.accent
        } else {
            palette.line
        }
    }

    /// One row's words and controls from its model: cheap, every draw.
    fn fill_row(cx: &mut Cx, item: &WidgetRef, row: &LayerStackRow, index: usize, overlays: usize) {
        item.view(cx, ids!(cross_line)).set_visible(cx, overlays > 0 && index == overlays);
        item.label(cx, ids!(layer_name)).set_text(cx, &row.name);
        let badge = item.view(cx, ids!(span_badge));
        if badge.visible() != row.spanned {
            badge.set_visible(cx, row.spanned);
        }
        item.label(cx, ids!(layer_detail)).set_text(cx, &row.detail);
        // A failed layer's reason is its tip (its edge is red); any other
        // row's tip says what it does.
        item.tip(cx, ids!(row_tip)).set_text(row.failed.as_deref().unwrap_or(&row.detail));
        let eye = item.check_box(cx, ids!(layer_eye));
        if eye.active(cx) != row.on {
            eye.set_active(cx, row.on, Animate::No);
        }
        // Only a drag holds the slider (an adjustment row's is its
        // strength): its value field is sized away and never drawn, so
        // `has_text_focus` compares an empty area and reads true whenever
        // nothing holds the key focus (at launch, after a press on the
        // slider), which left a fresh row at 0 for a layer at full opacity.
        let slider = item.slider(cx, ids!(layer_opacity));
        if !slider.borrow().is_some_and(|slider| slider.dragging.is_some()) {
            slider.set_value(cx, row.opacity);
        }
        if !row.adjust {
            item.drop_down(cx, ids!(layer_blend)).set_selected_item(cx, row.blend);
        }
    }

    /// One row's border, given only when [`Painted`] says so.
    fn paint_border(cx: &mut Cx, item: &WidgetRef, border: u32) {
        let mut body = item.view(cx, ids!(body));
        let border = Vec4f::from_u32(border);
        script_apply_eval!(cx, body, {
            draw_bg +: {border_color: #(border)}
        });
    }

    /// One row's picture bound, given only when [`Painted`] says so (an
    /// adjustment row has none).
    fn bind_frame(cx: &mut Cx, item: &WidgetRef, row: &LayerStackRow, frame: Option<&(Rc<dyn FrameSource>, FrameBinding)>) {
        if !row.adjust {
            let thumb = item.widget(cx, ids!(layer_thumb)).as_live_frame_view();
            match frame {
                Some((source, binding)) => thumb.set_frame(cx, source.clone(), binding.clone()),
                None => thumb.clear_frame(cx),
            }
        }
    }

    fn handle_list_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let uid = self.widget_uid();
        let list = self.view.reorder_list(cx, ids!(layer_list));
        let (moved, items) = match list.borrow() {
            Some(list) => (list.reordered(actions), list.items_with_actions(actions)),
            None => (None, Vec::new()),
        };
        if let Some((from, to)) = moved {
            cx.widget_action(uid, LayerStackAction::Move { from, to });
        }
        for (row, item) in items {
            let Some(model) = self.rows.get(row) else { continue };
            if item.check_box(cx, ids!(layer_eye)).changed(actions).is_some() {
                cx.widget_action(uid, LayerStackAction::Toggle(row));
            }
            // A content row's opacity, an adjustment row's strength.
            let slider = item.slider(cx, ids!(layer_opacity));
            if let Some(value) = slider.slided(actions) {
                cx.widget_action(uid, LayerStackAction::Opacity { row, value });
            }
            if slider.end_slide(actions).is_some() {
                cx.widget_action(uid, LayerStackAction::OpacityEnd(row));
            }
            if !model.adjust {
                // The picker reports a pick of the blend already in force
                // too; only a change is an ask.
                if let Some(blend) = item.drop_down(cx, ids!(layer_blend)).selected(actions).filter(|blend| *blend != model.blend) {
                    cx.widget_action(uid, LayerStackAction::Blend { row, blend });
                }
            }
            // A click on the row's own face -- its picture, its words -- and
            // not a press the grip, the eye or a control took.
            if item.view(cx, ids!(body)).finger_up(actions).is_some_and(|up| up.is_over && (up.abs - up.abs_start).length() < 4.0) {
                cx.widget_action(uid, LayerStackAction::Select(row));
            }
        }
    }
}

impl LayerStackViewRef {
    /// The rows to show, top first; `overlays` of them from the top are the
    /// shared overlays (the crossfade line goes under them), and `frames`
    /// holds each row's picture by the same index (`None`: no picture, and
    /// always `None` for an adjustment). `stamp` names what the frames were
    /// read from: the host's installed video generation, moved on by every
    /// new installed generation and by any change of the nodes the rows
    /// stand for.
    ///
    /// The same rows, the same `overlays` and the same `stamp` change
    /// nothing: the frames are taken to be the ones already bound, and none
    /// is compared (a source or a binding says nothing of the generation it
    /// came from).
    ///
    /// Binding a picture afresh lets its frame texture go (the thumbnail
    /// blanks until its next frame), so only a new picture binds one: a new
    /// `stamp`, or a new shape of rows (a row added or removed, or one
    /// turned from a content row into an adjustment row or back), binds
    /// every visible row's picture afresh, as a new installed generation
    /// must. The widget cannot tell a reordered row (rows carry no
    /// identity): the stamp can, as it covers the nodes the rows show,
    /// which a reorder moves. Anything else that changes in the rows (a
    /// selection, an opacity or strength, a blend, a name, a detail, a
    /// refusal) repaints the words, the border and the controls only, and
    /// keeps every picture bound; a row that had no picture takes the one
    /// it is handed now.
    pub fn set_rows(&self, cx: &mut Cx, rows: &[LayerStackRow], overlays: usize, frames: Vec<Option<(Rc<dyn FrameSource>, FrameBinding)>>, stamp: u64) {
        if let Some(mut inner) = self.borrow_mut() {
            if inner.rows == rows && inner.overlays == overlays && inner.stamp == stamp {
                return;
            }
            let reshaped = inner.rows.len() != rows.len() || inner.rows.iter().zip(rows).any(|(old, new)| old.adjust != new.adjust);
            inner.rows = rows.to_vec();
            inner.overlays = overlays;
            if reshaped || inner.stamp != stamp || inner.frames.len() != frames.len() {
                inner.frames = frames;
                inner.stamp = stamp;
                inner.painted = vec![None; rows.len()];
            } else {
                let inner = &mut *inner;
                for ((held, new), painted) in inner.frames.iter_mut().zip(frames).zip(inner.painted.iter_mut()) {
                    if held.is_none() && new.is_some() {
                        *held = new;
                        if let Some(painted) = painted {
                            painted.framed = false;
                        }
                    }
                }
            }
            inner.view.redraw(cx);
        }
    }

    /// The line under the buttons: why the stack refused the last ask
    /// ("" clears it, as the host does on the next ask the stack takes).
    /// Setting what is already there does nothing.
    pub fn set_status(&self, cx: &mut Cx, text: &str) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let label = inner.view.label(cx, ids!(layer_status));
        if label.text() == text {
            return;
        }
        label.set_text(cx, text);
        inner.view.redraw(cx);
    }

    /// The panel's asks in `actions`, in order.
    pub fn actions(&self, actions: &Actions) -> Vec<LayerStackAction> {
        actions.filter_widget_actions_cast::<LayerStackAction>(self.widget_uid()).filter(|action| *action != LayerStackAction::None).collect()
    }

    /// The display row a drop at `abs` lands on: the row it goes in above,
    /// `Some(len)` under the last row, `None` outside the list.
    pub fn drop_row(&self, cx: &Cx, abs: DVec2) -> Option<usize> {
        let inner = self.borrow()?;
        let list = inner.view.reorder_list(cx, ids!(layer_list));
        let list = list.borrow()?;
        if !list.area().rect(cx).contains(abs) {
            return None;
        }
        let len = inner.rows.len();
        Some(list.row_at(cx, abs).unwrap_or(len).min(len))
    }

    /// Open the gap a dragged tile will land in, at a row as [`Self::drop_row`]
    /// gives it, with the list's drop line in it; `None` closes it.
    pub fn set_drop_gap(&self, cx: &mut Cx, row: Option<usize>) {
        let Some(list_ref) = self.borrow().map(|inner| inner.view.reorder_list(cx, ids!(layer_list))) else { return };
        let Some(mut list) = list_ref.borrow_mut() else { return };
        list.set_external_gap(cx, row);
    }
}

impl Widget for LayerStackView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // The markup went on again over the rows: their borders with it.
        if matches!(event, Event::ScriptReapply) {
            self.painted.iter_mut().for_each(|painted| *painted = None);
        }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let uid = self.widget_uid();
        for (id, action) in [
            (ids!(layer_add), LayerStackAction::Add),
            (ids!(layer_adjust), LayerStackAction::AddAdjust),
            (ids!(layer_span), LayerStackAction::Span),
            (ids!(layer_remove), LayerStackAction::Remove),
        ] {
            if self.view.button(cx, id).clicked(actions) {
                cx.widget_action(uid, action);
            }
        }
        self.handle_list_actions(cx, actions);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let palette = makepad_look::palette::get(cx);
        let generation = makepad_look::palette::generation(cx);
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.borrow_mut::<ReorderList>() {
                list.set_item_range(cx, 0, self.rows.len());
                while let Some(index) = list.next_visible_item(cx) {
                    let Some(row) = self.rows.get(index) else { continue };
                    let template = if row.adjust { live_id!(AdjustRow) } else { live_id!(LayerRow) };
                    let (item, existed) = list.item_with_existed(cx, index, template);
                    Self::fill_row(cx, &item, row, index, self.overlays);
                    let border = Self::border(&palette, row);
                    let was = self.painted.get(index).copied().flatten().filter(|_| existed);
                    if !was.is_some_and(|was| was.border == border && was.generation == generation) {
                        Self::paint_border(cx, &item, border);
                    }
                    if !was.is_some_and(|was| was.framed) {
                        Self::bind_frame(cx, &item, row, self.frames.get(index).and_then(Option::as_ref));
                    }
                    if let Some(slot) = self.painted.get_mut(index) {
                        *slot = Some(Painted { border, generation, framed: true });
                    }
                    item.draw_all(cx, scope);
                }
            }
        }
        DrawStep::done()
    }
}
