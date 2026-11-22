//! A fixture net list standing in for the explorer and board's probe targets.

use fab_elec_view::live::{
    peek::{peek_target, PeekResponse, PeekScopeWidgetRefExt},
    probe::{carries_probe, probe_key_release, probe_target, ProbeGesture},
};
use makepad_widgets::makepad_platform::{self, a11y_tree};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.ProbeNetsBase = #(ProbeNets::register_widget(vm))
    mod.widgets.ProbeNets = set_type_default() do mod.widgets.ProbeNetsBase {
        width: 220 height: Fill
        bg_dark: #x0b0e14
        bg_light: theme.color_bg_app
        draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: 11}}
        draw_row +: {color: #3d7bd840}
        row_height: 22
        peek: mod.widgets.PeekScope {}
    }
}

#[derive(Clone, Debug, Default)]
pub enum ProbeNetsAction {
    #[default]
    None,
    Probe(String),
}

/// A probe drop target, P shortcut and held-P preview for the fixture's lowered net names.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct ProbeNets {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_bg: DrawColor,
    #[live]
    bg_dark: Vec4f,
    #[live]
    bg_light: Vec4f,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_row: DrawColor,
    #[live(22.0)]
    row_height: f64,
    #[rust]
    nets: Vec<String>,
    #[rust]
    row_rects: Vec<Rect>,
    #[rust]
    hovered: Option<String>,
    #[live]
    peek: WidgetRef,
}

impl ProbeNets {
    /// Store the lowered net names in sorted order, with one row per net.
    pub fn set_nets(&mut self, cx: &mut Cx, mut nets: Vec<String>) {
        nets.sort_unstable();
        nets.dedup();
        self.nets = nets;
        self.row_rects.clear();
        self.hovered = None;
        self.redraw(cx);
    }

    fn net_at(&self, abs: DVec2) -> Option<String> {
        self.row_rects.iter().zip(&self.nets)
            .find_map(|(rect, net)| rect.contains(abs).then(|| net.clone()))
    }
}

impl WidgetNode for ProbeNets {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }

    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }

    fn area(&self) -> Area {
        self.draw_bg.area()
    }

    fn redraw(&mut self, cx: &mut Cx) {
        self.draw_bg.redraw(cx);
    }

    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) {
        visit(id!(peek), self.peek.clone());
    }
}

impl Widget for ProbeNets {
    fn clips_content(&self) -> bool {
        true
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "nets");
    }

    /// One item per drawn row, named by its net: a remote probe drag finds its drop target.
    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (rect, net) in self.row_rects.iter().zip(&self.nets) {
            let node = a11y_tree::Node { role: a11y_tree::Role::ListItem, bounds: makepad_platform::a11y::semantic_rect(*rect), name: net.clone(), ..Default::default() };
            out.push((LiveId::from_str(net).0, None, node));
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let area = self.draw_bg.area();
        if let Some(net) = probe_target(cx, event, area, self.uid, |abs| self.net_at(abs)) {
            cx.widget_action(self.uid, ProbeNetsAction::Probe(net));
        }
        match peek_target(cx, event, area, self.uid, |abs| self.net_at(abs).map(|net| (net.clone(), net)), |_| false) {
            PeekResponse::Consumed => {
                // A held P still follows the row under the pointer, for the probe on its release.
                if let Event::MouseMove(e) = event { self.hovered = self.net_at(e.abs); }
                self.redraw(cx);
                return;
            }
            PeekResponse::Redraw => self.redraw(cx),
            PeekResponse::None => {}
        }
        if matches!(event, Event::Drag(drag) if carries_probe(&drag.items))
            || matches!(event, Event::Drop(_) | Event::DragEnd)
        {
            self.redraw(cx);
        }

        match event.hits(cx, area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let hovered = self.net_at(e.abs);
                if self.hovered != hovered {
                    self.hovered = hovered;
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.hovered.take().is_some() {
                    self.redraw(cx);
                }
            }
            Hit::FingerDown(_) => cx.set_key_focus(area),
            _ => {}
        }

        if let Some(net) = probe_key_release(cx, event, area, self.hovered.as_deref()) {
            cx.widget_action(self.uid, ProbeNetsAction::Probe(net));
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.color = if base_theme(cx) == BaseTheme::Light { self.bg_light } else { self.bg_dark };
        self.draw_bg.draw_abs(cx, rect);
        self.row_rects.clear();
        if self.row_height <= 0.0 {
            self.peek.as_peek_scope().draw_peek(cx, scope, self.uid);
            return DrawStep::done();
        }
        let probe_hovered = cx.global::<ProbeGesture>().hovered().map(str::to_owned);
        let bottom = rect.pos.y + rect.size.y;
        cx.push_clip_rect(rect);
        for (index, net) in self.nets.iter().enumerate() {
            let y = rect.pos.y + index as f64 * self.row_height;
            if y >= bottom {
                break;
            }
            let row = Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, self.row_height) };
            self.row_rects.push(Rect { size: dvec2(row.size.x, row.size.y.min(bottom - y)), ..row });
            if self.hovered.as_deref() == Some(net.as_str()) || probe_hovered.as_deref() == Some(net.as_str()) {
                self.draw_row.draw_abs(cx, row);
            }
            let size = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), net).size_in_lpxs;
            self.draw_text.draw_abs(cx, dvec2(row.pos.x + 10.0, y + (self.row_height - size.height as f64) * 0.5), net);
        }
        cx.pop_clip_rect();
        self.peek.as_peek_scope().draw_peek(cx, scope, self.uid);
        DrawStep::done()
    }
}
