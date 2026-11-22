//! A held net preview. Panes supply hit tests; the gesture owns the press and its timeout.

use makepad_widgets_core::{*, makepad_platform::event::TouchState, widget::WidgetWeakRef};
pub use super::peek_scope::{PeekScope, PeekScopeRef, PeekScopeWidgetRefExt};

/// The internal id of a net carried from a peek.
pub const PEEK_DRAG: LiveId = live_id!(elec_peek);

pub fn peek_drag_item(net: &str) -> DragItem {
    DragItem::String { value: net.into(), internal_id: Some(PEEK_DRAG) }
}

pub fn carries_peek(items: &[DragItem]) -> Option<&str> {
    items.iter().find_map(|item| match item {
        DragItem::String { value, internal_id: Some(id) } if *id == PEEK_DRAG => Some(value.as_str()),
        _ => None,
    })
}

const HOLD: f64 = 0.35;

#[derive(Clone, Debug, PartialEq)]
pub struct PeekOpen {
    pub net: String,
    pub label: String,
    pub pane: WidgetUid,
    pub at: DVec2,
    pub opened: f64,
}

#[derive(Clone, Debug)]
struct Pending { peek: PeekOpen, deadline: f64 }

#[derive(Clone, Debug, Default, PartialEq)]
pub enum PeekUpdate {
    #[default]
    None,
    Opened,
    Closed,
    Drag(String),
}

/// Window-free gesture state. A cancelled hold still owns its press until release.
#[derive(Clone, Debug, Default)]
pub struct PeekState {
    pub open: Option<PeekOpen>,
    pub p_held: bool,
    /// A press opened a peek during this hold of P, so its release does not probe.
    pub p_used: bool,
    pub pointer: Option<DVec2>,
    pub dragging: bool,
    pending: Option<Pending>,
    pressed: Option<WidgetUid>,
}

impl PeekState {
    pub fn press(&mut self, now: f64, at: DVec2, pane: WidgetUid, net: Option<(String, String)>, tag: bool) -> PeekUpdate {
        if self.pressed.is_some() || !(self.p_held || tag) { return PeekUpdate::None; }
        let Some((net, label)) = net else { return PeekUpdate::None };
        self.pressed = Some(pane);
        self.pointer = Some(at);
        let peek = PeekOpen { net, label, pane, at, opened: now };
        if self.p_held {
            self.p_used = true;
            self.open = Some(peek);
            PeekUpdate::Opened
        } else {
            self.pending = Some(Pending { peek, deadline: now + HOLD });
            PeekUpdate::None
        }
    }

    pub fn owns(&self, pane: WidgetUid) -> bool { self.pressed == Some(pane) }

    pub fn timeout(&mut self, now: f64) -> PeekUpdate {
        if !self.pending.as_ref().is_some_and(|press| now >= press.deadline) { return PeekUpdate::None; }
        let mut peek = self.pending.take().unwrap().peek;
        peek.opened = now;
        self.open = Some(peek);
        PeekUpdate::Opened
    }

    pub fn move_to(&mut self, at: DVec2) -> PeekUpdate {
        self.pointer = Some(at);
        if self.pending.as_ref().is_some_and(|press| (at - press.peek.at).length() >= 4.0) {
            self.pending = None;
        }
        if let Some(peek) = &self.open {
            if !self.dragging && (at - peek.at).length() > 8.0 {
                self.dragging = true;
                return PeekUpdate::Drag(peek.net.clone());
            }
        }
        PeekUpdate::None
    }

    pub fn escape(&mut self) -> PeekUpdate {
        self.pending = None;
        self.dragging = false;
        if self.open.take().is_some() { PeekUpdate::Closed } else { PeekUpdate::None }
    }

    pub fn release(&mut self) -> PeekUpdate {
        self.pressed = None;
        self.pointer = None;
        self.escape()
    }

    pub fn leave(&mut self) -> PeekUpdate {
        if self.dragging { PeekUpdate::None } else { self.escape() }
    }
}

/// The one open preview, its one-shot hold timer, and the bench's channel identities.
#[derive(Default)]
pub struct PeekGesture {
    pub state: PeekState,
    pub channels: Vec<String>,
    /// By channel, its probe's hue (`LaneStack::hues`).
    pub hues: Vec<usize>,
    timer: Timer,
    hover: Option<(WidgetUid, DVec2)>,
    pub(super) view: Option<(WidgetUid, WidgetWeakRef)>,
}

impl PeekGesture {
    /// The hue of `net`'s probe: its Bench channel's. Before the Bench has the lanes (none yet),
    /// `fallback`, the probe's place among its bench's probes; a probe not yet a lane, the hue
    /// it will take.
    pub fn hue_of(&self, net: &str, fallback: usize) -> usize {
        match self.channels.iter().position(|n| n == net) {
            Some(i) => self.hues.get(i).copied().unwrap_or(i),
            None if self.channels.is_empty() => fallback,
            None => crate::instruments::bench::next_hue(&self.hues),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PeekResponse {
    #[default]
    None,
    /// Return from pane input handling, including its old P shortcut and hover cursor.
    Consumed,
    Redraw,
}

/// A 320 by 160 preview, flipped beside the pointer and shrunk only for a small window.
pub fn peek_rect(window: Rect, at: DVec2) -> Rect {
    fn axis(start: f64, extent: f64, at: f64, wanted: f64) -> (f64, f64) {
        let end = start + extent.max(0.0);
        let at = at.clamp(start, end);
        let before = at - start;
        let after = end - at;
        let forward = after >= wanted + 12.0 || before < wanted + 12.0 && after >= before;
        let room = if forward { after } else { before };
        let gap = 12.0_f64.min(room * 0.5);
        let size = wanted.min((room - gap).max(0.0));
        (if forward { at + gap } else { at - gap - size }, size)
    }
    let (x, w) = axis(window.pos.x, window.size.x, at.x, 320.0);
    let (y, h) = axis(window.pos.y, window.size.y, at.y, 160.0);
    Rect { pos: dvec2(x, y), size: dvec2(w, h) }
}

/// Call after `probe_target`. Only peekable presses acquire a hit; plain clicks pass through.
pub fn peek_target(
    cx: &mut Cx,
    event: &Event,
    area: Area,
    pane: WidgetUid,
    mut net_at: impl FnMut(DVec2) -> Option<(String, String)>,
    mut tag_at: impl FnMut(DVec2) -> bool,
) -> PeekResponse {
    let mut redraw = false;
    let mut consumed = false;
    if cx.global::<PeekGesture>().view.as_ref().is_some_and(|(_, view)| view.upgrade().is_none()) {
        let gesture = cx.global::<PeekGesture>();
        gesture.state.release();
        gesture.view = None;
    }
    match event {
        Event::KeyDown(key) if key.key_code == KeyCode::KeyP => {
            let m = key.modifiers;
            if !(m.shift || m.control || m.alt || m.logo) {
                if !key.is_repeat { let state = &mut cx.global::<PeekGesture>().state; state.p_held = true; state.p_used = false; }
                consumed = true;
            }
        }
        Event::KeyUp(key) if key.key_code == KeyCode::KeyP => cx.global::<PeekGesture>().state.p_held = false,
        Event::WindowLostFocus(_) => {
            let gesture = cx.global::<PeekGesture>();
            gesture.state.p_held = false;
            gesture.hover = None;
        }
        Event::MouseMove(e) => {
            let over = area.clipped_rect(cx).contains(e.abs);
            let gesture = cx.global::<PeekGesture>();
            if over {
                gesture.hover = Some((pane, e.abs));
            } else if gesture.hover.is_some_and(|(owner, _)| owner == pane) {
                gesture.hover = None;
            }
        }
        _ => {}
    }
    if matches!(event, Event::KeyDown(_) | Event::KeyUp(_) | Event::MouseMove(_)) {
        let gesture = cx.global::<PeekGesture>();
        if let Some((owner, at)) = gesture.hover {
            if owner == pane {
                let hand = tag_at(at) || gesture.state.p_held && net_at(at).is_some();
                if hand && matches!(event, Event::MouseMove(_)) && !cx.global::<PeekGesture>().state.owns(pane) {
                    if matches!(event.hits(cx, area), Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_)) {
                        cx.set_cursor(MouseCursor::Hand);
                        consumed = true;
                    }
                } else { cx.set_cursor(if hand { MouseCursor::Hand } else { MouseCursor::Default }); }
            }
        }
    }
    let owns = cx.global::<PeekGesture>().state.owns(pane);
    if owns {
        let dismiss = matches!(event, Event::KeyDown(key) if key.key_code == KeyCode::Escape)
            || matches!(event, Event::WindowLostFocus(_) | Event::MouseLeave(_)) || !area.is_valid(cx);
        if dismiss {
            if matches!(event, Event::WindowLostFocus(_) | Event::MouseLeave(_)) || !area.is_valid(cx) {
                let gesture = cx.global::<PeekGesture>();
                gesture.state.release();
                gesture.hover = None;
            } else { cx.global::<PeekGesture>().state.escape(); }
            redraw = true;
            consumed = true;
        } else if cx.global::<PeekGesture>().timer.is_event(event).is_some() {
            let gesture = cx.global::<PeekGesture>();
            gesture.timer = Timer::empty();
            redraw = gesture.state.timeout(Cx::monotonic_now()) == PeekUpdate::Opened;
        }
        match event {
            Event::Drag(e) if carries_peek(&e.items).is_some() => {
                cx.global::<PeekGesture>().state.move_to(e.abs);
                cx.set_cursor(MouseCursor::Grabbing);
                redraw = true;
                consumed = true;
            }
            Event::Drop(e) if carries_peek(&e.items).is_some() => consumed = true,
            Event::DragEnd => {
                cx.global::<PeekGesture>().state.release();
                redraw = true;
                consumed = true;
            }
            _ => {}
        }
    }
    let candidate = match event {
        Event::MouseDown(e) if e.button.is_primary() => Some(e.abs),
        Event::TouchUpdate(e) => e.touches.iter().find(|p| p.state == TouchState::Start).map(|p| p.abs),
        _ => None,
    }.filter(|&at| cx.global::<PeekGesture>().state.pressed.is_none() && area.clipped_rect(cx).contains(at))
        .and_then(|at| {
            let tag = tag_at(at);
            if cx.global::<PeekGesture>().state.p_held || tag { net_at(at).map(|net| (net, tag)) } else { None }
        });
    if owns || candidate.is_some() {
        match event.hits(cx, area) {
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                if let Some((net, tag)) = candidate {
                    let gesture = cx.global::<PeekGesture>();
                    gesture.state.press(Cx::monotonic_now(), e.abs, pane, Some(net), tag);
                    consumed = gesture.state.owns(pane);
                    if gesture.state.pending.is_some() {
                        let timer = cx.start_timeout(HOLD);
                        cx.global::<PeekGesture>().timer = timer;
                    }
                    if consumed { cx.set_key_focus_from_user(area); redraw = true; }
                }
            }
            Hit::FingerMove(e) if owns => {
                let update = cx.global::<PeekGesture>().state.move_to(e.abs);
                if let PeekUpdate::Drag(net) = update {
                    cx.start_dragging(vec![peek_drag_item(&net)]);
                    cx.set_cursor(MouseCursor::Grabbing);
                } else if !e.rect.contains(e.abs) {
                    cx.global::<PeekGesture>().state.leave();
                }
                consumed = true;
                redraw = true;
            }
            Hit::FingerUp(e) if owns => {
                // The internal drag dispatches its Drop after the pointer release.
                if e.cancelled || !cx.global::<PeekGesture>().state.dragging {
                    cx.global::<PeekGesture>().state.release();
                }
                consumed = true;
                redraw = true;
            }
            _ => {}
        }
    }
    if cx.global::<PeekGesture>().state.pending.is_none() {
        let timer = std::mem::take(&mut cx.global::<PeekGesture>().timer);
        if !timer.is_empty() { cx.stop_timer(timer); }
    }
    let view = cx.global::<PeekGesture>().view.as_ref().filter(|(owner, _)| *owner == pane).and_then(|(_, view)| view.upgrade());
    if let Some(view) = view {
        if let Some(mut peek) = view.borrow_mut::<PeekScope>() { peek.handle_event(cx, event, &mut Scope::empty()); }
        if cx.global::<PeekGesture>().state.open.is_none() { cx.global::<PeekGesture>().view = None; }
    }
    if redraw { area.redraw(cx); }
    if consumed { PeekResponse::Consumed } else if redraw { PeekResponse::Redraw } else { PeekResponse::None }
}
