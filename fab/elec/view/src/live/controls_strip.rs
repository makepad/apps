//! The Controls header sums up the run board's keys and knobs in one row; a press asks its host to
//! bring the controls forward (the shell's Board controls editor).
//! Each frame compares the counts once by visiting the controls in the cached model.
//! A draw paints a few quads, a chevron and two labels.

use crate::instruments::controls_panel::ControlsPanel;
use makepad_widgets_core::*;
use std::fmt::Write as _;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    set_type_default() do #(DrawStripChevron::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: instance(#fff)
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.rotate(0.5 * PI, 4.0, 4.0)
            sdf.move_to(1.0, 7.0)
            sdf.line_to(4.0, 1.0)
            sdf.line_to(7.0, 7.0)
            sdf.close_path()
            sdf.fill(self.color)
            return sdf.result
        }
    }

    mod.widgets.ElecControlsStripBase = #(ElecControlsStrip::register_widget(vm))
    mod.widgets.ElecControlsStrip = set_type_default() do mod.widgets.ElecControlsStripBase {
        width: Fill height: Fit
        panel: mod.widgets.ControlsPanel {}
        hidden: false
        color_head: theme.color_surface_container_high
        color_rule: theme.color_bevel_inset_2
        color_hover: #fff1
        color_title: theme.color_text_meta
        color_text: theme.color_text_meta
        draw_text +: {text_style: theme.font_regular {font_size: 7.5}}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawStripChevron {
    #[deref]
    draw_super: DrawQuad,
}

const HEAD: f64 = 22.0;

/// Sent when a person presses the header: the host shows the controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlsStripAction {
    None,
    Pressed,
}

impl Default for ControlsStripAction {
    fn default() -> Self {
        Self::None
    }
}

/// The run board's controls summed up in a header row.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct ElecControlsStrip {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    /// Counts the controls (never drawn here).
    #[live]
    panel: WidgetRef,
    #[live]
    hidden: bool,
    #[live]
    color_head: Vec4f,
    #[live]
    color_rule: Vec4f,
    #[live]
    color_hover: Vec4f,
    #[live]
    color_title: Vec4f,
    #[live]
    color_text: Vec4f,
    #[live]
    draw_head: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_chevron: DrawStripChevron,
    #[rust]
    area: Area,
    #[rust]
    header: Rect,
    #[rust]
    hover: bool,
    #[rust]
    shown: Option<(usize, usize, bool)>,
    #[rust]
    summary: String,
    #[rust]
    pressed: bool,
}

impl ElecControlsStrip {
    /// The keys and knobs it showed at its last draw (none before it): what its head says.
    pub fn shown_counts(&self) -> Option<(usize, usize, bool)> {
        self.shown
    }

    /// The height it takes: none while hidden or without controls (as it last drew), else its
    /// header. O(1).
    pub fn wanted_height(&self) -> f64 {
        if self.has_header() { HEAD } else { 0.0 }
    }

    /// Hides or shows the strip.
    pub fn set_hidden(&mut self, cx: &mut Cx, hidden: bool) {
        if self.hidden != hidden {
            self.hidden = hidden;
            self.hover = false;
            self.pressed = false;
            cx.widget_tree_mark_dirty(self.uid);
            self.area.redraw(cx);
        }
    }

    fn press(&mut self, cx: &mut Cx) {
        cx.widget_action(self.uid, ControlsStripAction::Pressed);
    }

    /// The keys and knobs shown, and whether the bench's `panel` option arranges them.
    fn counts(&self, cx: &Cx) -> Option<(usize, usize, bool)> {
        let mut panel = self.panel.borrow_mut::<ControlsPanel>()?;
        let (keys, knobs) = panel.counts(cx)?;
        Some((keys, knobs, panel.by_bench(cx)))
    }

    fn has_header(&self) -> bool {
        !self.hidden && !matches!(self.shown, None | Some((0, 0, _)))
    }

    fn summary((keys, knobs, by_bench): (usize, usize, bool)) -> String {
        let mut text = String::from(if by_bench { "arranged by the bench's panel" } else { "generated from the board" });
        for (n, label) in [(keys, "key"), (knobs, "knob")] {
            if n > 0 {
                let plural = if n == 1 { "" } else { "s" };
                let _ = write!(text, " · {n} {label}{plural}");
            }
        }
        text.push_str(" · a press or a turn re-records");
        text
    }

    fn draw_header(&mut self, cx: &mut Cx2d) {
        let head = self.header;
        self.draw_head.color = self.color_head;
        self.draw_head.draw_abs(cx, head);
        self.draw_head.color = self.color_rule;
        self.draw_head.draw_abs(
            cx,
            Rect {
                size: dvec2(head.size.x, 1.0),
                ..head
            },
        );
        if self.hover {
            self.draw_head.color = self.color_hover;
            self.draw_head.draw_abs(cx, head);
        }

        cx.push_clip_rect(head);
        let color = self.color_title;
        self.draw_chevron.draw_vars.set_dyn_instance(
            cx,
            id!(color),
            &[color.x, color.y, color.z, color.w],
        );
        self.draw_chevron.draw_abs(
            cx,
            Rect {
                pos: head.pos + dvec2(8.0, (HEAD - 8.0) * 0.5),
                size: dvec2(8.0, 8.0),
            },
        );

        let title = "CONTROLS";
        let layout = self
            .draw_text
            .layout(cx, 0.0, 0.0, None, false, Align::default(), title);
        let scale = self.draw_text.font_scale;
        let width = f64::from(layout.size_in_lpxs.width * scale);
        let height = f64::from(layout.size_in_lpxs.height * scale);
        let pos = head.pos + dvec2(8.0 + 8.0 + 6.0, (HEAD - height) * 0.5);
        self.draw_text.color = color;
        self.draw_text.draw_abs(cx, pos, title);
        self.draw_text.color = self.color_text;
        self.draw_text
            .draw_abs(cx, pos + dvec2(width + 8.0, 0.0), &self.summary);
        cx.pop_clip_rect();
    }
}

impl WidgetNode for ElecControlsStrip {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }

    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }

    fn area(&self) -> Area {
        self.area
    }

    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }

    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}

    fn find_widgets_from_point(&self, _cx: &Cx, _point: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl Widget for ElecControlsStrip {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let counts = self.counts(cx);
        if self.shown != counts {
            self.shown = counts;
            self.summary = counts.map(Self::summary).unwrap_or_default();
        }
        if !self.has_header() {
            self.header = cx.walk_turtle(Walk {
                height: Size::Fixed(0.0),
                ..walk
            });
            // Keep a redraw area so the first board with controls can reveal the strip.
            cx.add_rect_area(&mut self.area, self.header);
            self.hover = false;
            self.pressed = false;
            return DrawStep::done();
        }

        self.header = cx.walk_turtle(Walk {
            height: Size::Fixed(HEAD),
            ..walk
        });
        cx.add_rect_area(&mut self.area, self.header);
        self.draw_header(cx);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if matches!(event, Event::Signal | Event::NextFrame(_)) && self.counts(cx) != self.shown {
            self.area.redraw(cx);
        }
        if !self.has_header() {
            return;
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                cx.set_cursor(MouseCursor::Hand);
                if !self.hover {
                    self.hover = true;
                    self.area.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) if self.hover => {
                self.hover = false;
                self.area.redraw(cx);
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => self.pressed = true,
            Hit::FingerUp(e) => {
                if std::mem::take(&mut self.pressed) && e.is_over && e.was_tap() {
                    self.press(cx);
                }
            }
            _ => {}
        }
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if self.has_header() {
            let node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                bounds: makepad_platform::a11y::semantic_rect(self.area.clipped_rect(cx)),
                name: "Controls".to_owned(),
                value: self.summary.clone(),
                actions: a11y_tree::Actions::PRESS,
                ..Default::default()
            };
            out.push((live_id!(elec_controls_strip).0, None, node));
        }
    }

    fn semantics_part_area(&self, _key: u64) -> Area {
        self.area
    }

    fn handle_action(
        &mut self,
        cx: &mut Cx,
        request: &a11y_tree::ActionRequest,
        _scope: &mut Scope,
    ) -> a11y_tree::ActionResult {
        use a11y_tree::ActionResult;
        if !self.has_header()
            || cx.a11y.action_part_key(request.node) != Some(live_id!(elec_controls_strip).0)
        {
            return ActionResult::Unhandled;
        }
        if request.action != a11y_tree::Actions::PRESS {
            return ActionResult::Unsupported;
        }
        self.press(cx);
        ActionResult::Handled
    }
}
