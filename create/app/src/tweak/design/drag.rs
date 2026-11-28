//! Each share has one pointer query and one outstanding provisional edit for the local seat.
use crate::tweak::{app, parse_json, DesignNotice, DesignPlace, DesignReplyKind, DesignRequest, Notice,
    Rect as AppRect, Request, RequestId, Session, SessionId};
use makepad_town_core::event::Id;
use makepad_widgets::{event::DigitId, *};
use std::{collections::{BTreeMap, VecDeque}, sync::Arc, time::{Duration, Instant}};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DragPayload { TreeNode(String), PaletteItem(String), CanvasLift(String) }

impl DragPayload {
    fn from_item(item: &DragItem) -> Option<Self> {
        let DragItem::String { value, internal_id: Some(tag) } = item else { return None };
        if *tag == live_id!(town_design_node) { Some(Self::TreeNode(value.clone())) }
        else if *tag == live_id!(town_design_palette) { Some(Self::PaletteItem(value.clone())) }
        else if *tag == live_id!(town_design_canvas) { Some(Self::CanvasLift(value.clone())) }
        else { None }
    }

    pub(crate) fn accepts(items: &[DragItem]) -> bool {
        items.iter().any(|i| matches!(i, DragItem::String { internal_id: Some(tag), .. }
            if [live_id!(town_design_node), live_id!(town_design_palette), live_id!(town_design_canvas)].contains(tag)))
    }

    fn exclude(&self) -> Option<String> {
        match self { Self::TreeNode(p) | Self::CanvasLift(p) => Some(p.clone()), Self::PaletteItem(_) => None }
    }

    fn edit(&self, hit: &DropHit, preview: bool) -> DesignRequest {
        let target = hit.target.clone();
        let place = hit.place;
        match self {
            Self::PaletteItem(ty) if preview => DesignRequest::GhostInsert { target, place, ty: ty.clone() },
            Self::PaletteItem(ty) => DesignRequest::Insert { path: target, place, ty: ty.clone(), name: None, props: vec![] },
            Self::CanvasLift(p) | Self::TreeNode(p) if preview => DesignRequest::GhostMove { source: p.clone(), target, place },
            Self::CanvasLift(p) | Self::TreeNode(p) => DesignRequest::Move { path: p.clone(), target, place },
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum DesignPictureEvent {
    Hover { item: Id, window: usize, point: DVec2, items: Arc<Vec<DragItem>> },
    Drop { item: Id, window: usize, point: DVec2, items: Arc<Vec<DragItem>> },
    Leave { item: Id },
    Resize { item: Id, resize: crate::tweak::overlay::Resize },
}

impl DesignPictureEvent {
    pub fn item(&self) -> Id {
        match self { Self::Hover { item, .. } | Self::Drop { item, .. } | Self::Leave { item } | Self::Resize { item, .. } => *item }
    }
}

/// Both picture widgets use screen points for the lift threshold and the built-in chip.
pub(crate) struct LiftPress { pub item: Id, pub digit: DigitId, pub start: DVec2, pub path: String }

impl LiftPress {
    pub fn lift(&self, cx: &mut Cx, point: DVec2, chip: &ScriptObjectRef) -> bool {
        if cx.has_internal_drag() { return true }
        if (point - self.start).length() <= 6. { return false }
        let visual = cx.with_vm(|vm| WidgetRef::script_from_value(vm, chip.as_object().into()));
        visual.label(cx, ids!(label)).set_text(cx, &self.path);
        let item = DragItem::String { value: self.path.clone(), internal_id: Some(live_id!(town_design_canvas)) };
        cx.start_dragging_with(vec![item], visual, dvec2(14., 10.));
        true
    }
}

pub(crate) fn design_pointer(event: &Event) -> Option<DVec2> {
    match event {
        Event::Drag(e) if DragPayload::accepts(&e.items) => Some(e.abs),
        Event::Drop(e) if DragPayload::accepts(&e.items) => Some(e.abs),
        _ => None,
    }
}

/// Only claim the event inside an editable picture. A transition into a letterbox bar
/// or another board card leaves the previous target, even within the same widget area.
pub(crate) fn picture_drop(cx: &mut Cx, uid: WidgetUid, area: Area, event: &Event,
    target: Option<(Id, usize, DVec2)>, over: &mut Option<Id>) {
    if over.is_some() && *over != target.map(|t| t.0) {
        cx.widget_action(uid, DesignPictureEvent::Leave { item: over.take().unwrap() });
    }
    let Some((item, window, point)) = target else { return };
    match event.drag_hits(cx, area) {
        DragHit::Drag(d) if d.state != DragState::Out => {
            cx.set_cursor(MouseCursor::Hand);
            let palette = d.items.iter().any(|i| matches!(i, DragItem::String { internal_id: Some(id), .. } if *id == live_id!(town_design_palette)));
            *d.response.lock().unwrap_or_else(|e| e.into_inner()) = if palette { DragResponse::Copy } else { DragResponse::Move };
            *over = Some(item);
            cx.widget_action(uid, DesignPictureEvent::Hover { item, window, point, items: d.items });
        }
        DragHit::Drop(d) => {
            *over = None;
            cx.widget_action(uid, DesignPictureEvent::Drop { item, window, point, items: d.items });
        }
        _ => {
            if let Some(item) = over.take() { cx.widget_action(uid, DesignPictureEvent::Leave { item }); }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DropHit {
    pub target: String,
    container: String,
    pub place: DesignPlace,
    pub rect: AppRect,
    pub row: bool,
}

impl DropHit {
    fn read(window: usize, json: &str) -> Option<Self> {
        let v = parse_json(json).ok()?;
        let place = match v.get("place")?.as_str()? {
            "before" => DesignPlace::Before, "inside" => DesignPlace::Inside, "after" => DesignPlace::After, _ => return None,
        };
        let rect = app::read_rect(window, v.get("target_rect")?.as_arr()?).ok()?;
        Some(Self { target: v.get("target_path")?.as_str()?.into(), container: v.get("container_path")?.as_str()?.into(), place, rect, row: v.get("flow")?.as_str()? == "row" })
    }

    fn same_place(&self, other: &Self) -> bool {
        self.target == other.target && self.place == other.place
    }
}

struct ActiveDrag {
    kind: DragPayload,
    pointer: DVec2,
    window: usize,
    queried: Option<DVec2>,
    hit: Option<DropHit>,
    dwell: Timer,
    settled: bool,
    dropped: bool,
}

impl ActiveDrag {
    fn new(items: &[DragItem], window: usize, pointer: DVec2, dropped: bool) -> Option<Self> {
        Some(Self { kind: items.iter().find_map(DragPayload::from_item)?, window, pointer, queried: None,
            hit: None, dwell: Timer::empty(), settled: false, dropped })
    }

    fn stop_dwell(&mut self, cx: &mut Cx) {
        if self.dwell.0 != 0 { cx.stop_timer(std::mem::take(&mut self.dwell)); }
    }
}

enum GhostRequest { Preview(u64, DropHit), Cancel, Commit }

#[derive(Default)]
struct ShareDrag {
    active: Option<ActiveDrag>,
    // A released drop can still be waiting for an RTT when the person begins another drag.
    // Keep each release and only the latest point of the still-moving gesture.
    waiting: VecDeque<ActiveDrag>,
    generation: u64,
    query: Option<(RequestId, u64, usize)>,
    provisional: Option<(RequestId, GhostRequest)>,
    last_preview: Option<Instant>,
    installed: Option<(u64, DropHit)>,
}

impl ShareDrag {
    fn clear(&mut self, cx: &mut Cx) {
        if let Some(mut active) = self.active.take() { active.stop_dwell(cx); }
    }

    fn advance(&mut self, cx: &mut Cx, mut send: impl FnMut(DesignRequest) -> Option<RequestId>) {
        loop {
            self.advance_one(cx, &mut send);
            if self.active.is_some() { return }
            let Some(next) = self.waiting.pop_front() else { return };
            self.generation = self.generation.wrapping_add(1);
            self.active = Some(next);
        }
    }

    fn advance_one(&mut self, cx: &mut Cx, send: &mut impl FnMut(DesignRequest) -> Option<RequestId>) {
        if let Some(a) = &mut self.active {
            if self.query.is_none() && a.queried != Some(a.pointer) {
                let req = DesignRequest::DropTarget { window: a.window, x: a.pointer.x, y: a.pointer.y, exclude: a.kind.exclude() };
                a.queried = Some(a.pointer);
                self.query = send(req).map(|id| (id, self.generation, a.window));
                if self.query.is_none() { self.clear(cx); }
            }
        }
        if self.provisional.is_some() { return }
        if self.installed.as_ref().is_some_and(|(g, _)| *g != self.generation) {
            self.installed = None;
            self.provisional = send(DesignRequest::GhostCancel).map(|id| (id, GhostRequest::Cancel));
            return
        }
        let current = self.installed.as_ref();
        let Some(a) = &self.active else {
            if self.installed.take().is_some() {
                self.provisional = send(DesignRequest::GhostCancel).map(|id| (id, GhostRequest::Cancel));
            }
            return
        };
        if a.dropped && self.query.is_some() { return }
        let Some(hit) = &a.hit else {
            if self.installed.take().is_some() { self.provisional = send(DesignRequest::GhostCancel).map(|id| (id, GhostRequest::Cancel)); }
            if a.dropped { self.clear(cx); }
            return
        };
        let matches = current.is_some_and(|(_, old)| old.same_place(hit));
        if a.dropped {
            // A direct edit retracts any earlier provisional layout in the owner's serial pipeline.
            if matches { self.provisional = send(DesignRequest::GhostCommit).map(|id| (id, GhostRequest::Commit)); }
            else { send(a.kind.edit(hit, false)); }
            self.installed = None;
            self.clear(cx);
        } else if a.settled && !matches {
            self.provisional = send(a.kind.edit(hit, true)).map(|id| (id, GhostRequest::Preview(self.generation, hit.clone())));
            self.last_preview = Some(Instant::now());
        }
    }
}

#[derive(Default)]
pub(crate) struct DragController { shares: BTreeMap<SessionId, ShareDrag> }

impl DragController {
    fn send<L: Copy + Eq, T: Copy + Eq>(s: &mut Session<L,T>, r: DesignRequest) -> Option<RequestId> {
        match s.request(None, Request::Design(r)) {
            Ok(id) => Some(id),
            Err(why) => { log!("Town drag: {why}"); None }
        }
    }

    pub fn picture<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, s: &mut Session<L,T>, event: &DesignPictureEvent) {
        let dropped = matches!(event, DesignPictureEvent::Drop { .. });
        let (window, point, items) = match event {
            DesignPictureEvent::Hover { window, point, items, .. } | DesignPictureEvent::Drop { window, point, items, .. } =>
                (*window, *point, items),
            DesignPictureEvent::Resize { .. } => return,
            DesignPictureEvent::Leave { .. } => { self.cancel(cx, s, false); return }
        };
        let drag = self.shares.entry(s.share).or_default();
        if drag.active.as_ref().is_some_and(|a| a.dropped) {
            if let Some(next) = drag.waiting.back_mut().filter(|a| !a.dropped) {
                next.pointer = point;
                next.dropped = dropped;
            } else if let Some(next) = ActiveDrag::new(items, window, point, dropped) { drag.waiting.push_back(next); }
            return
        }
        if drag.active.is_none() {
            drag.active = ActiveDrag::new(items, window, point, false);
            drag.generation = drag.generation.wrapping_add(1);
        }
        let Some(a) = &mut drag.active else { return };
        // The provisional widget occupies precisely the offered space. Moving within it keeps
        // that offer, even if a new pick would now hit the provisional widget itself.
        let on_ghost = drag.installed.as_ref().is_some_and(|(g, hit)| *g == drag.generation && a.hit.as_ref().is_some_and(|h| h.same_place(hit)))
            && s.view.design.ghost.as_ref().filter(|g| g.seat == s.me).and_then(|g| g.rect)
                .is_some_and(|r| r.window == window && point.x >= r.x && point.x < r.x + r.w && point.y >= r.y && point.y < r.y + r.h);
        if !on_ghost { a.pointer = point; }
        a.dropped = dropped;
        if dropped { a.stop_dwell(cx); }
        drag.advance(cx, |r| Self::send(s, r));
    }

    pub fn caret(&self, share: SessionId) -> Option<&DropHit> {
        self.shares.get(&share)?.active.as_ref()?.hit.as_ref()
    }

    pub fn cancel<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, s: &mut Session<L,T>, force: bool) {
        let Some(drag) = self.shares.get_mut(&s.share) else { return };
        if force { drag.waiting.clear(); }
        else {
            if drag.waiting.back().is_some_and(|a| !a.dropped) { drag.waiting.pop_back(); }
            if drag.active.as_ref().is_some_and(|a| a.dropped) { return }
        }
        drag.clear(cx);
        drag.advance(cx, |r| Self::send(s, r));
    }

    pub fn event<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, s: &mut Session<L,T>, event: &Event) {
        let Some(drag) = self.shares.get_mut(&s.share) else { return };
        match event {
            Event::DragEnd => self.cancel(cx, s, false),
            Event::WindowLostFocus(_) => self.cancel(cx, s, true),
            Event::KeyDown(k) if k.key_code == KeyCode::Escape => self.cancel(cx, s, true),
            Event::Timer(_) => {
                let Some(a) = &mut drag.active else { return };
                if a.dwell.0 != 0 && a.dwell.is_event(event).is_some() {
                    a.dwell = Timer::empty();
                    a.settled = true;
                    drag.advance(cx, |r| Self::send(s, r));
                }
            }
            _ => {}
        }
    }

    pub fn notice<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, s: &mut Session<L,T>, req: RequestId, notice: &Notice) -> bool {
        let Some(drag) = self.shares.get_mut(&s.share) else { return false };
        if let Some((_, generation, window)) = drag.query.filter(|q| q.0 == req) {
            let hit = match notice {
                Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::DropTarget, json }) => DropHit::read(window, json),
                Notice::Refused(_) => None,
                _ => return false,
            };
            drag.query = None;
            if let Some(a) = drag.active.as_mut().filter(|_| generation == drag.generation) {
                let changed = match (&a.hit, &hit) {
                    (Some(old), Some(new)) => !old.same_place(new), (None, None) => false, _ => true,
                };
                if changed {
                    a.stop_dwell(cx);
                    let entered = a.hit.as_ref().zip(hit.as_ref()).is_some_and(|(old, new)|
                        old.container != new.container && new.place == DesignPlace::Inside);
                    a.settled = entered && drag.last_preview.is_none_or(|t| t.elapsed() >= Duration::from_millis(120));
                    if !a.settled && hit.is_some() && !a.dropped { a.dwell = cx.start_timeout(0.12); }
                }
                a.hit = hit;
            }
            drag.advance(cx, |r| Self::send(s, r));
            return !matches!(notice, Notice::Refused(_))
        }
        if drag.provisional.as_ref().is_some_and(|p| p.0 == req) {
            let cleanup = matches!(drag.provisional, Some((_, GhostRequest::Cancel)));
            match notice {
                Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::State, .. }) => {
                    if let GhostRequest::Preview(generation, hit) = drag.provisional.take().unwrap().1 {
                        drag.installed = Some((generation, hit));
                    }
                }
                Notice::Design(DesignNotice::Mutated { .. }) if matches!(drag.provisional, Some((_, GhostRequest::Commit))) => {
                    drag.provisional = None;
                    drag.advance(cx, |r| Self::send(s, r));
                    return false
                }
                Notice::Refused(_) => {
                    drag.provisional = None;
                    if let Some(a) = &mut drag.active { a.settled = false; }
                }
                _ => return false,
            }
            drag.advance(cx, |r| Self::send(s, r));
            return match notice {
                Notice::Refused(why) => {
                    if cleanup { log!("Town drag cancel: {why}"); }
                    cleanup
                }
                _ => true,
            }
        }
        false
    }

    /// Another seat's mutation may have retracted our preview since its last acknowledgement.
    pub fn sync_status<L, T>(&mut self, s: &Session<L,T>) {
        if s.view.design.ghost.as_ref().is_none_or(|g| g.seat != s.me) {
            if let Some(drag) = self.shares.get_mut(&s.share) { drag.installed = None; }
        }
    }

    pub fn retire(&mut self, cx: &mut Cx, share: SessionId) {
        if let Some(mut d) = self.shares.remove(&share) { d.clear(cx); }
    }

    #[cfg(test)]
    pub fn timer(&self, share: SessionId) -> Timer { self.shares[&share].active.as_ref().unwrap().dwell }
}
