//! The share view and board use the same letterboxed picture in screen layout points.
use super::design::drag::DropHit;
use super::{DesignPlace, SessionId, focus::{FocusState, ViewportTransform}, At, Seat, SeatView};
use crate::model::clean_name;
use makepad_town_core::{event::Id, fold::{Agent, Profile, State}, hex};
use makepad_widgets::*;
use std::{collections::{BTreeMap, HashMap}, sync::Arc};

/// A profile colour of 0 is unset (a new profile writes 0), so the seat's own hue stands in, never black.
pub(crate) fn seat_colour(state: &State, seat: &Seat) -> Vec4f {
    state.profiles.get(seat).filter(|p| p.colour != 0).map_or_else(|| {
        let hue = seat.iter().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(*b as u32)) % 360;
        let channel = |shift: f32| 0.30 + 0.65 * (((hue as f32 / 60. + shift) % 6. - 3.).abs() - 1.).clamp(0., 1.);
        vec4(channel(0.), channel(4.), channel(2.), 1.)
    }, |p| Vec4f::from_u32((p.colour << 8) | 255))
}

pub(crate) fn who(state: &State, seat: &Seat, agent: Option<Id>) -> String {
    actor_label(&state.profiles, &state.agents, seat, agent)
}

pub(super) fn actor_label(profiles: &BTreeMap<Seat, Profile>, agents: &BTreeMap<Id, Agent>, seat: &Seat, agent: Option<Id>) -> String {
    let person = profiles.get(seat).map(|p| clean_name(&p.name)).filter(|s| !s.is_empty())
        .unwrap_or_else(|| hex(&seat[1..5]));
    let Some(id) = agent else { return person };
    if let Some(agent) = agents.get(&id).filter(|agent| &agent.owner == seat) {
        if agents.iter().any(|(other, a)| *other != id && &a.owner == seat) {
            let name = clean_name(&agent.name);
            if !name.is_empty() { return format!("{person}'s {name}") }
        }
    }
    format!("{person}'s AI")
}

#[derive(PartialEq)]
struct Ring { rect: super::Rect, label: String, colour: Vec4f, rank: usize, count: usize }

/// Rebuild on a selection, profile or window change. Drawing borrows these prepared labels.
#[derive(Clone, PartialEq)]
pub struct Selections {
    pub window: usize,
    pub window_size: [f64; 2],
    pub focus: FocusState,
    pub share: Option<SessionId>,
    pub(crate) build_mode: bool,
    pub(crate) can_edit: bool,
    pub(crate) local_selection: Option<(String, super::Rect)>,
    ghost: Option<(super::Rect, String)>,
    pub(crate) caret: Option<DropHit>,
    rings: Arc<[Ring]>,
    pub(super) revision: Option<(usize, u64)>,
}

impl Selections {
    pub fn new(window: usize, window_size: [f64; 2], view: &SeatView, state: &State) -> Self {
        let mut counts = HashMap::new();
        let bounds = |r: super::Rect| [r.x, r.y, r.w, r.h].map(f64::to_bits);
        let mut rings = Vec::new();
        for (seat, selection) in &view.selections {
            let Some(rect) = selection.rect.filter(|r| r.window == window && r.valid()) else { continue };
            let label = who(state, seat, selection.agent);
            let colour = seat_colour(state, seat);
            let rank = counts.entry(bounds(rect)).or_insert(0);
            rings.push(Ring { rect, label, colour, rank: *rank, count: 0 });
            *rank += 1;
        }
        for r in &mut rings { r.count = counts[&bounds(r.rect)]; }
        let ghost = view.design.ghost.as_ref().and_then(|g| {
            let rect = g.rect.filter(|r| r.window == window && r.valid())?;
            Some((rect, format!("{} · {}", who(state, &g.seat, None), g.label)))
        });
        Self { window, window_size, share: None, build_mode: false, can_edit: false, local_selection: None, ghost, caret: None, focus: FocusState::default(), rings: rings.into(), revision: None }
    }

    pub fn with_focus(mut self, focus: FocusState) -> Self {
        self.focus = focus;
        self
    }

    pub(crate) fn with_design(mut self, share: SessionId, view: &SeatView, me: &Seat, build: bool, edit: bool, caret: Option<&DropHit>) -> Self {
        self.share = Some(share);
        self.build_mode = build;
        self.can_edit = edit;
        self.caret = caret.cloned();
        self.local_selection = view.selections.get(me).and_then(|s|
            Some((s.path.clone(), s.rect.filter(|r| r.window == self.window && r.valid())?)));
        self
    }

    pub(crate) fn selected_at(&self, tf: &ViewportTransform, point: DVec2) -> Option<&str> {
        if !self.build_mode || !self.can_edit { return None }
        let (path, r) = self.local_selection.as_ref()?;
        let p = tf.screen_to_app(point)?;
        (p.x >= r.x && p.x < r.x + r.w && p.y >= r.y && p.y < r.y + r.h).then_some(path)
    }

    pub fn transform(&self, picture: Rect) -> ViewportTransform {
        let rest = FocusState::default();
        let focus = if self.focus.isolated_rect.is_some_and(|r| r.window != self.window) { &rest } else { &self.focus };
        ViewportTransform::compute(picture, self.window_size, focus)
    }

    pub fn board_transform(&self, view: Rect) -> Option<ViewportTransform> {
        Some(self.transform(picture(view, self.window_size)?))
    }
}

fn picture(view: Rect, [w, h]: [f64; 2]) -> Option<Rect> {
    if ![view.pos.x, view.pos.y, view.size.x, view.size.y, w, h].iter().all(|n| n.is_finite())
        || w <= 0. || h <= 0. { return None }
    let scale = (view.size.x / w).min(view.size.y / h);
    if scale <= 0. || !scale.is_finite() { return None }
    let size = dvec2(w, h) * scale;
    Some(Rect { pos: view.pos + (view.size - size) * 0.5, size })
}

/// Both picture views supply a point already mapped into the app window.
pub fn pick_at(window: usize, point: DVec2, [width, height]: [f64; 2]) -> Option<At> {
    (width.is_finite() && height.is_finite()
        && point.x >= 0. && point.x < width && point.y >= 0. && point.y < height)
        .then_some(At::Point { window, x: point.x, y: point.y })
}

pub fn draw_isolate_scrim(cx: &mut Cx2d, tf: &ViewportTransform, selections: &Selections, ink: &mut DrawColor) {
    let Some(rect) = selections.focus.isolated_rect.filter(|r| r.window == selections.window) else { return };
    let previous = ink.color;
    ink.color = Vec4f::from_u32(0x17171aff);
    for band in tf.scrim_bands(rect) {
        if band.size.x > 0. && band.size.y > 0. { ink.draw_abs(cx, band); }
    }
    ink.color = previous;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SizeHandle { Right, Bottom, Corner }

impl SizeHandle {
    pub fn cursor(self) -> makepad_widgets::MouseCursor {
        use makepad_widgets::MouseCursor::*;
        match self { Self::Right => EwResize, Self::Bottom => NsResize, Self::Corner => NwseResize }
    }
}

pub(crate) fn size_handle_rects(tf: &ViewportTransform, r: super::Rect) -> [(SizeHandle, Rect); 3] {
    let end = dvec2(r.x + r.w, r.y + r.h);
    let picture = tf.picture_rect;
    let size = dvec2(8., 8.);
    [(SizeHandle::Right, dvec2(end.x, r.y + r.h * 0.5)),
        (SizeHandle::Bottom, dvec2(r.x + r.w * 0.5, end.y)), (SizeHandle::Corner, end)]
        .map(|(handle, p)| {
            let pos = tf.app_to_screen(p) - size * 0.5;
            let limit = picture.pos + picture.size - size;
            let pos = dvec2(pos.x.clamp(picture.pos.x, limit.x.max(picture.pos.x)),
                pos.y.clamp(picture.pos.y, limit.y.max(picture.pos.y)));
            (handle, Rect { pos, size })
        })
}

pub(crate) fn hit_size_handle(tf: &ViewportTransform, s: &Selections, point: DVec2) -> Option<SizeHandle> {
    if !s.build_mode || !s.can_edit || !tf.picture_rect.contains(point) { return None }
    let (_, r) = s.local_selection.as_ref()?;
    size_handle_rects(tf, *r).into_iter().rev().find(|(_, h)| h.contains(point)).map(|p| p.0)
}

pub(crate) fn resize_from_drag(tf: &ViewportTransform, rect: super::Rect, handle: SizeHandle, start: DVec2, point: DVec2)
    -> (Option<f64>, Option<f64>) {
    let delta = tf.inverse(point) - tf.inverse(start);
    ((handle != SizeHandle::Bottom).then(|| (rect.w + delta.x).round().max(1.)),
        (handle != SizeHandle::Right).then(|| (rect.h + delta.y).round().max(1.)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ResizePhase { Drag, Release, Cancel }

#[derive(Clone, Debug)]
pub(crate) struct Resize {
    pub path: Arc<str>, pub width: Option<f64>, pub height: Option<f64>, pub phase: ResizePhase,
}

pub(crate) struct ResizePress {
    item: Id, pub digit: makepad_widgets::event::DigitId, path: Arc<str>, handle: SizeHandle,
    tf: ViewportTransform, rect: super::Rect, start: DVec2, last: DVec2, moved: bool,
}

impl ResizePress {
    pub fn begin(item: Id, tf: ViewportTransform, s: &Selections, digit: makepad_widgets::event::DigitId, start: DVec2) -> Option<Self> {
        let handle = hit_size_handle(&tf, s, start)?;
        let (path, rect) = s.local_selection.as_ref()?;
        Some(Self { item, digit, path: path.as_str().into(), handle, tf, rect: *rect, start, last: start, moved: false })
    }

    pub fn update(&mut self, cx: &mut makepad_widgets::Cx, uid: makepad_widgets::WidgetUid, point: DVec2, phase: ResizePhase) {
        if phase == ResizePhase::Drag { cx.set_cursor(self.handle.cursor()); }
        let dims = resize_from_drag(&self.tf, self.rect, self.handle, self.start, point);
        let old = resize_from_drag(&self.tf, self.rect, self.handle, self.start, self.last);
        if dims == old && (phase == ResizePhase::Drag || !self.moved) { return }
        self.moved = true;
        self.last = point;
        let resize = Resize { path: self.path.clone(), width: dims.0, height: dims.1, phase };
        cx.widget_action(uid, super::design::drag::DesignPictureEvent::Resize { item: self.item, resize });
    }

    pub fn cancel(&mut self, cx: &mut makepad_widgets::Cx, uid: makepad_widgets::WidgetUid) {
        self.update(cx, uid, self.last, ResizePhase::Cancel);
    }
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawDesignGhost {
    #[deref] pub draw_super: DrawQuad,
    #[live] pub color: Vec4f,
    #[live] pub on_color: Vec4f,
    #[live] pub veil_color: Vec4f,
}

pub(crate) fn ghost_shader(vm: &mut ScriptVm) -> ScriptValue {
    let shader = DrawDesignGhost::script_shader(vm);
    script_eval!(vm, {
        use mod.prelude.workspace.*
        #(shader){
            ..mod.draw.DrawQuad
            color: theme.color_primary
            on_color: theme.color_on_primary
            veil_color: theme.color_drag_target_preview
            pixel: fn() {
                let p = self.pos * self.rect_size
                let edge = min(p, self.rect_size - p)
                if edge.y < 2.0 || edge.x < 2.0 {
                    let along = if edge.y < 2.0 { p.x } else { p.y }
                    if fract(along / 8.0) < 0.625 { return Pal.premul(self.color) }
                    return Pal.premul(self.on_color)
                }
                let a = min(self.veil_color.a, 0.25)
                return vec4(self.veil_color.rgb * a, a)
            }
        }
    })
}

fn draw_ghost(cx: &mut Cx2d, tf: &ViewportTransform, r: super::Rect, text: &str, ghost: &mut DrawDesignGhost, ink: &mut DrawColor, label: &mut DrawText) {
    let pos = tf.app_to_screen(dvec2(r.x, r.y));
    let size = tf.app_to_screen(dvec2(r.x + r.w, r.y + r.h)) - pos;
    let rect = Rect { pos, size };
    if !tf.picture_rect.intersects(rect) { return }
    ghost.draw_abs(cx, rect);
    let text = label.layout(cx, 0., 0., None, false, Align::default(), text);
    let extent = dvec2(text.size_in_lpxs.width as f64, text.size_in_lpxs.height as f64) * label.font_scale as f64 + dvec2(6., 2.);
    let picture = tf.picture_rect;
    let at = dvec2(pos.x.min(picture.pos.x + picture.size.x - extent.x).max(picture.pos.x),
        (pos.y - extent.y).max(picture.pos.y));
    ink.color = ghost.color;
    ink.draw_abs(cx, Rect { pos: at, size: extent });
    label.color = ghost.on_color;
    label.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: at + dvec2(3., 1.), size: extent }), &text);
}

fn draw_insert_caret(cx: &mut Cx2d, tf: &ViewportTransform, hit: &DropHit, ink: &mut DrawColor, ghost: &mut DrawDesignGhost) {
    let r = hit.rect;
    let pos = tf.app_to_screen(dvec2(r.x, r.y));
    let end = tf.app_to_screen(dvec2(r.x + r.w, r.y + r.h));
    if hit.place == DesignPlace::Inside {
        ghost.draw_abs(cx, Rect { pos, size: end - pos });
        return;
    }
    let after = hit.place == DesignPlace::After;
    let rect = if hit.row {
        Rect { pos: dvec2(if after { end.x - 2. } else { pos.x }, pos.y), size: dvec2(2., end.y - pos.y) }
    } else {
        Rect { pos: dvec2(pos.x, if after { end.y - 2. } else { pos.y }), size: dvec2(end.x - pos.x, 2.) }
    };
    ink.color = ghost.on_color;
    ink.draw_abs(cx, Rect { pos: rect.pos - dvec2(1., 1.), size: rect.size + dvec2(2., 2.) });
    ink.color = ghost.color;
    ink.draw_abs(cx, rect);
}

pub fn draw_selections(cx: &mut Cx2d, tf: &ViewportTransform, selections: &Selections, ink: &mut DrawColor, label: &mut DrawText, ghost: &mut DrawDesignGhost) {
    let picture = tf.picture_rect;
    let previous_colours = (ink.color, label.color);
    cx.push_clip_rect(picture);
    for r in selections.rings.iter() {
        let corner = tf.app_to_screen(dvec2(r.rect.x, r.rect.y));
        let size = tf.app_to_screen(dvec2(r.rect.x + r.rect.w, r.rect.y + r.rect.h)) - corner;
        if size.x <= 0. || size.y <= 0. || !picture.intersects(Rect { pos: corner, size }) { continue }
        let band = (size.x.min(size.y) / (2. * r.count as f64)).min(2.);
        let inset = r.rank as f64 * band;
        let pos = corner + dvec2(inset, inset);
        let size = size - dvec2(2. * inset, 2. * inset);
        ink.color = r.colour;
        for (offset, edge) in [
            (DVec2::default(), dvec2(size.x, band)), (dvec2(0., size.y - band), dvec2(size.x, band)),
            (DVec2::default(), dvec2(band, size.y)), (dvec2(size.x - band, 0.), dvec2(band, size.y)),
        ] { ink.draw_abs(cx, Rect { pos: pos + offset, size: edge }); }
        let text = label.layout(cx, 0., 0., None, false, Align::default(), &r.label);
        let extent = dvec2(text.size_in_lpxs.width as f64, text.size_in_lpxs.height as f64) * label.font_scale as f64 + dvec2(6., 2.);
        let x = pos.x.min(picture.pos.x + picture.size.x - extent.x).max(picture.pos.x);
        let bottom = picture.pos.y + picture.size.y;
        let y = (corner.y - extent.y).max(picture.pos.y)
            .min(bottom - extent.y * r.count as f64).max(picture.pos.y) + r.rank as f64 * extent.y;
        let at = dvec2(x, y);
        ink.draw_abs(cx, Rect { pos: at, size: extent });
        label.color = if 0.2126*r.colour.x + 0.7152*r.colour.y + 0.0722*r.colour.z > 0.52 {
            vec4(0., 0., 0., 1.)
        } else { vec4(1., 1., 1., 1.) };
        label.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: at + dvec2(3., 1.), size: extent }), &text);
    }
    if let Some((rect, name)) = &selections.ghost {
        draw_ghost(cx, tf, *rect, name, ghost, ink, label);
    } else if let Some(hit) = &selections.caret {
        draw_insert_caret(cx, tf, hit, ink, ghost);
    }
    if let Some((_, r)) = selections.local_selection.as_ref().filter(|_| selections.build_mode && selections.can_edit) {
        for (_, h) in size_handle_rects(tf, *r) {
            ink.color = ghost.on_color;
            ink.draw_abs(cx, h);
            ink.color = ghost.color;
            ink.draw_abs(cx, Rect { pos: h.pos + dvec2(1., 1.), size: h.size - dvec2(2., 2.) });
        }
    }
    cx.pop_clip_rect();
    (ink.color, label.color) = previous_colours;
}

#[test]
fn overlay_maps_only_the_letterboxed_picture() {
    use makepad_town_core::fold::{Agent, Profile};
    use super::{Rect as AppRect, Selection};
    let view = Rect { pos: dvec2(40., 70.), size: dvec2(300., 300.) };
    let size = [200., 100.];
    assert_eq!(pick_at(7, dvec2(50., 25.), size), Some(At::Point { window: 7, x: 50., y: 25. }),
        "the share panel supplies window points directly");
    for (point, size) in [(dvec2(200., 3.), size), (dvec2(4., 100.), size), (dvec2(f64::NAN, 0.), size),
        (dvec2(0., 0.), [f64::INFINITY, 100.]), (dvec2(0., 0.), [200., 0.])] {
        assert!(pick_at(7, point, size).is_none(), "invalid window geometry or an outside point cannot pick");
    }
    let transform = Selections::new(7, size, &SeatView::default(), &State::default()).board_transform(view).unwrap();
    let pick = |x, y| pick_at(7, transform.screen_to_app(dvec2(x, y))?, size);
    assert_eq!(pick(115., 182.5), Some(At::Point { window: 7, x: 50., y: 25. }), "offset, 1.5 scale and letterbox use app layout points");
    assert_eq!(pick(40., 145.), Some(At::Point { window: 7, x: 0., y: 0. }), "the picture starts below the upper bar");
    for (x,y) in [(40., 144.), (340., 200.), (50., 295.), (39., 180.)] {
        assert!(pick(x,y).is_none(), "press ({x}, {y}) outside the picture produces no pick");
    }
    let mut seats = SeatView::default();
    let mut state = State::default();
    let rect = AppRect { window: 7, x: 5., y: 5., w: 50., h: 20. };
    for (n, name, colour, window, agent) in [(1, "Owner", 0x446688, 7, None),
        (2, "Peer", 0x88aa44, 7, Some([4; 32])), (3, "Guest", 0x668844, 8, None)] {
        let seat = [n; 65];
        state.profiles.insert(seat, Profile { writer: [0; 32], name: name.into(), colour });
        seats.selections.insert(seat, Selection { path: "ui.panel".into(), rect: Some(AppRect { window, ..rect }), agent });
    }
    let snapshot = Selections::new(7, [200., 100.], &seats, &state);
    let labels: Vec<_> = snapshot.rings.iter().map(|r| (r.label.as_str(), r.rank, r.count)).collect();
    assert_eq!(labels, [("Owner", 0, 2), ("Peer's AI", 1, 2)], "coincident selections each get a ring and label in their own window");
    assert_eq!(snapshot.rings[1].colour, Vec4f::from_u32(0x88aa44ff), "an AI uses its person's profile colour");
    state.profiles.get_mut(&[2; 65]).unwrap().colour = 0;
    let unset = seat_colour(&state, &[2; 65]);
    assert!(unset.x.max(unset.y).max(unset.z) >= 0.9 && unset.w == 1.,
        "an unset profile colour (0) takes the seat's hue, never black ink on the dark panel: {unset:?}");
    state.profiles.get_mut(&[2; 65]).unwrap().colour = 0x88aa44;

    let ana = [2; 65];
    let first = [4; 32];
    let second = [5; 32];
    state.profiles.get_mut(&ana).unwrap().name = "Ana".into();
    state.agents.insert(first, Agent { owner: ana, name: "  Aster\n  ".into(), kind: "session".into(), custom_name: None });
    assert_eq!(who(&state, &ana, Some(first)), "Ana's AI", "a person's only AI uses the generic label");
    state.agents.insert(second, Agent { owner: ana, name: "Fe\nrn".into(), kind: "session".into(), custom_name: None });
    let snapshot = Selections::new(7, size, &seats, &state);
    assert_eq!(snapshot.rings[1].label, "Ana's Aster", "the selection ring names the first of two AIs");
    assert_eq!(who(&state, &ana, Some(second)), "Ana's Fern", "a second AI has its own cleaned name");
    assert_eq!(who(&state, &ana, Some([6; 32])), "Ana's AI", "an unreplicated AI has a generic label");
    assert_eq!(who(&state, &ana, None), "Ana", "a person keeps their own label when they have several AIs");
}
