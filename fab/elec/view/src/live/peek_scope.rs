//! A control-free phosphor preview on its own overlay list.

use makepad_widgets_core::*;
use std::time::Duration;
use fab_elec_bench::trigger::{EdgeTrigger, Qualifier, Window};
use crate::instruments::{
    acquire::{LaneSetup, NetProbe},
    axis::si_label,
    bench::{initial_settings, probe_node},
    gesture::{fit_time, fit_values},
    look::InstrumentLook,
    phosphor_view::{PhosphorLook, PhosphorView, Stretch},
    scope::{LabelCache, ScopeSettings},
    scope_host::{volts, ScopeHost},
    simfeed::{SimScopeConfig, SimScopeFeed},
};
use super::peek::{peek_rect, PeekGesture, PeekOpen};
use super::tag::{DrawProbeTag, LaneSide, ProbeTag, TagKind, TagState, TagStyle};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.PeekScopeBase = #(PeekScope::register_widget(vm))
    mod.widgets.PeekScope = set_type_default() do mod.widgets.PeekScopeBase {
        width: 320 height: 160
        phosphor: mod.widgets.PhosphorView {}
        draw_tag: mod.draw.DrawProbeTag {}
        draw_text +: {text_style: theme.font_regular {font_size: 11}}
    }
}

/// One semantic node and one independently owned preview feed.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct PeekScope {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    overlay: DrawList2d,
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_tag: DrawProbeTag,
    #[rust]
    text_cache: LabelCache,
    #[live]
    phosphor: PhosphorView,
    #[rust]
    pane: WidgetUid,
    #[rust]
    open: Option<PeekOpen>,
    #[rust]
    host: ScopeHost<SimScopeFeed>,
    #[rust]
    settings: ScopeSettings,
    #[rust]
    setup: Option<(LaneSetup, EdgeTrigger, Window, Option<f64>, Qualifier)>,
    #[rust]
    measurement_generation: u64,
    #[rust]
    line: String,
    #[rust]
    answer_at: usize,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    start_failed: bool,
    #[rust]
    last_look: Option<PhosphorLook>,
}

impl PeekScope {
    fn stop(&mut self, cx: &mut Cx) {
        self.host.stop();
        if self.open.take().is_some() {
            self.line.clear();
            self.setup = None;
            self.overlay.redraw(cx);
        }
    }

    fn read_answers(&mut self, cx: &mut Cx) {
        let Some(feed) = self.host.feed().filter(|_| self.host.running()) else { return };
        if feed.measurement_generation() == self.measurement_generation { return; }
        let Some((generation, rows, _, _)) = feed.measurement_snapshot() else { return };
        self.measurement_generation = generation;
        let row = |name: &str| rows.iter().find(|row| row.lane == 0 && row.name == name && row.value.is_finite());
        let value = |name| row(name).map(|row| row.value);
        let Some(open) = &self.open else { return };
        self.line = open.label.replace('\n', "  ");
        self.answer_at = self.line.len();
        for name in [if row("freq").is_some() { "freq" } else { "mean" }, "Vpp"] {
            if let Some(row) = row(name) {
                self.line.push_str("  ");
                self.line.push_str(&si_label(row.value, &row.unit));
                if name == "Vpp" { self.line.push_str(" pp"); }
            }
        }
        let window = fit_time(value("freq").filter(|f| *f > 0.0).map(|f| 1.0 / f), self.settings.window());
        self.settings.time_per_div = (window.pre + window.post) / 10.0;
        let extrema = value("min").zip(value("max")).filter(|(min, max)| min <= max)
            .or_else(|| value("mean").map(|mean| {
                let half = value("Vpp").unwrap_or(0.0).max(0.0) * 0.5;
                (mean - half, mean + half)
            }));
        if let Some((min, max)) = extrema {
            let (scale, offset) = fit_values(min, max);
            self.settings.channels[0].volts_per_div = scale;
            self.settings.channels[0].offset_div = offset;
            self.settings.trigger.level = min * 0.5 + max * 0.5;
        }
        self.overlay.redraw(cx);
    }

    fn sync_feed(&mut self, cx: &mut Cx) {
        let Some(plot) = self.phosphor.plot_px().filter(|&(w, h)| w > 0 && h > 0) else { return };
        let Some(open) = &self.open else { return };
        let setup = (LaneSetup { map: self.settings.map(plot), spectrum: None, drawn: true }, self.settings.trigger(), self.settings.window(), self.settings.auto_after(), self.settings.trigger.qualify);
        if !self.host.running() {
            let probe = NetProbe { pos: probe_node(cx, &open.net), neg: None, scale: 1.0 };
            match SimScopeFeed::show(cx, probe, SimScopeConfig {
                plot, volts: volts(&self.settings, plot), trigger: setup.1, qualifier: setup.4, zone: None, window: setup.2,
                auto: setup.3, frame_every: Duration::from_millis(8),
            }) {
                Ok(feed) => { self.host.install(feed); self.start_failed = false; }
                Err(error) => {
                    if !self.start_failed { log!("peek: could not start {}: {error:?}", open.net); }
                    self.start_failed = true;
                    self.line = format!("{}  preview unavailable", open.label.replace('\n', "  "));
                    self.answer_at = self.line.len() - "preview unavailable".len();
                    return;
                }
            }
        }
        if self.setup != Some(setup) {
            let to = setup.0.map;
            let from = self.phosphor.picture_map().unwrap_or(to);
            self.phosphor.set_stretch(Some(Stretch { from, to }));
            self.host.configure_lanes(setup.1, setup.4, 0, setup.2, vec![setup.0], setup.3, None);
            self.setup = Some(setup);
        }
    }
}

impl WidgetNode for PeekScope {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _cx: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.draw_bg.area() }
    fn redraw(&mut self, cx: &mut Cx) { self.overlay.redraw(cx); }
}

impl Widget for PeekScope {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let open = cx.global::<PeekGesture>().state.open.as_ref().is_some_and(|open| open.pane == self.pane);
        if !open { self.stop(cx); return; }
        self.phosphor.handle_event(cx, event, scope);
        if matches!(event, Event::Signal) || self.next_frame.is_event(event).is_some() {
            self.read_answers(cx);
            self.sync_feed(cx);
            self.host.note_draw(cx.drawing_id());
            if self.host.present_lanes(|_, _, hits| self.phosphor.push_frame(cx, hits)) { self.overlay.redraw(cx); }
            if self.host.wants_frame() && !cx.next_frame_is_pending(self.next_frame) {
                self.next_frame = cx.new_next_frame();
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, _walk: Walk) -> DrawStep {
        let gesture = cx.global::<PeekGesture>();
        let open = gesture.state.open.as_ref().filter(|open| open.pane == self.pane).cloned();
        let pointer = gesture.state.pointer;
        let dragging = gesture.state.dragging;
        // Empty the list on close too: a retained overlay otherwise keeps its old picture.
        if dragging { self.overlay.begin_overlay_last(cx); } else { self.overlay.begin_popup_overlay(cx); }
        let size = cx.current_pass_size();
        cx.begin_root_turtle(size, Layout::default());
        if let Some(open) = open {
            if self.open.as_ref() != Some(&open) {
                self.host.stop();
                self.settings = initial_settings(&open.label);
                self.line = open.label.replace('\n', "  ");
                self.answer_at = self.line.len();
                self.open = Some(open.clone());
                self.setup = None;
                self.measurement_generation = 0;
                self.start_failed = false;
                self.phosphor.clear(cx);
            }
            let rect = peek_rect(Rect { pos: dvec2(0.0, 0.0), size }, if dragging { pointer.unwrap_or(open.at) } else { open.at });
            let light = base_theme(cx) == BaseTheme::Light;
            let look = InstrumentLook::for_theme(!light);
            let gesture = cx.global::<PeekGesture>();
            let hue = look.channel(gesture.hue_of(&open.net, gesture.channels.len()));
            let bg = look.canvas.canvas;
            self.draw_bg.color = vec4(bg[0], bg[1], bg[2], bg[3]);
            self.draw_bg.draw_abs(cx, rect);
            cx.push_clip_rect(rect);
            self.phosphor.screen = self.draw_bg.color;
            self.phosphor.set_channel(hue);
            let phosphor_look = self.settings.look(light);
            if self.last_look != Some(phosphor_look) {
                self.phosphor.set_look(cx, phosphor_look);
                self.last_look = Some(phosphor_look);
            }
            let plot = Rect { pos: rect.pos + dvec2(6.0, 28.0), size: dvec2((rect.size.x - 12.0).max(0.0), (rect.size.y - 34.0).max(0.0)) };
            self.phosphor.draw_walk_all(cx, scope, Walk::abs_rect(plot));
            self.sync_feed(cx);
            let tag = ProbeTag::from_label(&open.label, &open.net);
            let style = TagStyle { hue: vec4(hue[0], hue[1], hue[2], hue[3]), kind: TagKind::from_unit(self.settings.channels.first().map_or("V", |channel| channel.unit.as_str())),
                side: LaneSide::Sim, state: if dragging { TagState::Carried } else { TagState::Rest } };
            let metrics = tag.measure(cx, &mut self.draw_tag, &self.draw_text);
            self.text_cache.begin_frame(&self.draw_text, base_theme(cx));
            let answers = self.line[self.answer_at..].trim_start();
            let answer = if answers.is_empty() { None } else { Some(self.text_cache.layout(cx, &self.draw_text, answers)) };
            let room = (rect.size.x - 16.0).max(0.0);
            let reserve = answer.as_ref().map_or(0.0, |text| (f64::from(text.size_in_lpxs.width * self.draw_text.font_scale) + 8.0).min(room * 0.5));
            let size = tag.layout(metrics, (room - reserve).max(0.0), style).size;
            let head = Rect { pos: rect.pos + dvec2(8.0, 3.0), size: dvec2(size.x, 22.0) };
            tag.draw(cx, &mut self.draw_tag, &mut self.draw_text, head, style);
            if let Some(answer) = answer {
                let rest = Rect { pos: head.pos + dvec2(head.size.x + 8.0, 4.0), size: dvec2((room - head.size.x - 8.0).max(0.0), 18.0) };
                self.draw_text.color = style.hue;
                cx.push_clip_rect(rest);
                self.draw_text.draw_walk_laidout(cx, Walk::abs_rect(rest), &answer);
                cx.pop_clip_rect();
            }
            cx.pop_clip_rect();
        } else { self.stop(cx); }
        cx.end_pass_sized_turtle();
        self.overlay.end(cx);
        DrawStep::done()
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "peek");
        makepad_platform::a11y::set_string(&mut node.value, &self.line);
        if self.open.is_none() { node.states.insert(a11y_tree::States::HIDDEN); }
    }
}

impl PeekScopeRef {
    /// Draw last in the pane, outside its content clip. Event delivery stays in `peek_target`.
    pub fn draw_peek(&self, cx: &mut Cx2d, scope: &mut Scope, pane: WidgetUid) {
        if let Some(mut peek) = self.borrow_mut() { peek.pane = pane; }
        if cx.global::<PeekGesture>().state.open.as_ref().is_some_and(|open| open.pane == pane) {
            cx.global::<PeekGesture>().view = Some((pane, self.0.downgrade()));
        }
        self.0.draw_walk_all(cx, scope, Walk::default());
    }
}
