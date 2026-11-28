use super::ShareRole;
use super::control::{valid_point, FrameMotion, GrantId, PanelInput};
use crate::app_link::{AppInput, Mods, WindowId};
use crate::board::canvas::AppZoom;
use crate::tweak::{overlay, design::drag::{design_pointer, picture_drop, LiftPress}};
use crate::record::capture::{ThumbnailPicture, WindowThumbnail};
use makepad_call::driver::codec::Picture;
use makepad_town_core::event::{Id, SessionId};
use makepad_widgets::{event::{DigitId, ScrollPhase, TouchState}, makepad_draw::shader::draw_nv12::DrawNv12, *};
use std::sync::{Arc, Weak};

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*
    mod.widgets.SharePanel = #(SharePanel::register_widget(vm)){
        width: Fill height: Fill draw_nv12: mod.draw.DrawNv12{} draw_bg.color: #000
        drag_chip: DragChip{}
        ghost_ink: #(overlay::ghost_shader(vm))
        label: BodyNote.draw_text
        draw_waiting: BodyNote.draw_text
        scroll_bars: ScrollBars{
            scroll_bar_x +: {auto_hide: false}
            scroll_bar_y +: {auto_hide: false}
        }
    }
    mod.widgets.ShareChoices = #(ShareChoices::register_widget(vm)){
        width: Fill height: Fit
        placeholder: mod.draw.DrawColor{color: palette.chrome}
        still_image: mod.draw.DrawQuad{
            image: texture_2d(float)
            pixel: fn() {
                let color = self.image.sample(self.pos)
                return vec4(color.rgb * color.a, color.a)
            }
        }
        live_image: mod.draw.DrawNv12{}
        list := FlatList{width: Fill height: Fit
            WindowChoice := PanelButton{width: Fill height: 62
                align: Align{x: 0 y: 0.5} padding: Inset{left: 110 right: 8}
                label_walk: Walk{width: Fill height: Fit}
                draw_text +: {max_lines: 2 text_overflow: Ellipsis}
            }
            Choice := PanelButton{width: Fill height: 26}
            Empty := BodyNote{width: Fill}
        }
    }
}

/// Which view of a share measured it: the share's page, or its pop-out window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareSurface { Page, Window }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareControl {
    Ask(SessionId, bool),
    Release(SessionId, GrantId),
    Close(SessionId),
}

pub fn opening_uids(cx: &Cx, root: &WidgetRef) -> Vec<WidgetUid> {
    let mut uids = super::widgets::opening_uids(cx, root);
    let switch = root.widget(cx, ids!(live_share.allow_all));
    if !switch.is_empty() { uids.push(switch.widget_uid()); }
    if let Some(page) = root.widget(cx, ids!(live_share)).borrow::<crate::ui::live::LiveSharePage>() {
        uids.extend_from_slice(page.let_control_buttons());
    }
    uids
}

#[derive(Clone, Copy, Debug)]
pub struct SharePanelSize {
    pub session: SessionId,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanelWindow { Local(WindowId), Remote(SessionId) }

/// The pop-out asks for every pixel at the shared aspect: the sharer's caps (its window's own
/// pixels, bounded only by its encoder) decide the size it sends.
const NATIVE_REQUEST: f64 = 16384.;

fn picture_rect(panel: Rect, size: DVec2, one_to_one: bool, pan: DVec2, dpi: f64) -> Rect {
    if size.x <= 0. || size.y <= 0. || panel.size.x <= 0. || panel.size.y <= 0. { return Rect::default() }
    if one_to_one {
        let offset = |space: f64, pan: f64| if space >= 0. { space * 0.5 } else { -pan.clamp(0., -space) };
        let space = panel.size - size;
        let origin = panel.pos + dvec2(offset(space.x, pan.x), offset(space.y, pan.y));
        return Rect { pos: (origin * dpi).round() / dpi, size }
    }
    let fit = size * (panel.size.x / size.x).min(panel.size.y / size.y);
    Rect { pos: panel.pos + (panel.size - fit) * 0.5, size: fit }
}

/// The picture's shape for layout: the shared window's, or 16:9 until its size is known.
fn layout_size(window: DVec2) -> DVec2 {
    if window.x > 0. && window.y > 0. && window.x.is_finite() && window.y.is_finite() { window } else { dvec2(16., 9.) }
}

pub fn window_point(rect: Rect, size: DVec2, abs: DVec2, captured: bool) -> Option<DVec2> {
    if rect.size.x <= 0. || rect.size.y <= 0. { return None }
    let p = (abs - rect.pos) / rect.size * size;
    valid_point((size.x, size.y), p.x, p.y, captured).then_some(p)
}

fn picture_overlay(session: Option<SessionId>, share: &super::ShareShown) -> Option<&(Id, overlay::Selections)> {
    let session = session?;
    share.overlay.as_ref().filter(|(_, s)| s.share.or(share.watching) == Some(session)
        && s.window_size.iter().all(|n| n.is_finite() && *n > 0.))
}

#[derive(Script, ScriptHook, Widget)]
pub struct SharePanel {
    #[uid] uid: WidgetUid,
    #[live(true)] #[visible] visible: bool,
    #[walk] walk: Walk,
    #[live] popout: bool,
    #[find] #[live] controls: WidgetRef,
    #[find] #[live] caption_lights: WidgetRef,
    #[find] #[live] caption_buttons: WidgetRef,
    #[rust] caption: Option<(ResolvedChrome, bool)>,
    #[rust] member_color: Option<bool>,
    #[rust] picture_points: DVec2,
    #[rust] dpi: f64,
    #[redraw] #[live] draw_bg: DrawColor,
    #[live] draw_nv12: DrawNv12,
    #[live] ink: DrawColor,
    #[live] ghost_ink: overlay::DrawDesignGhost,
    #[live] drag_chip: ScriptObjectRef,
    #[rust] lift: Option<LiftPress>,
    #[rust] resize: Option<overlay::ResizePress>,
    #[rust] design_over: Option<Id>,
    #[live] label: DrawText,
    #[live] draw_waiting: DrawText,
    #[live] scroll_bars: ScrollBars,
    #[rust] session: Option<SessionId>,
    #[rust] one_to_one: Option<SessionId>,
    #[rust] pan_drag: Option<(DigitId, DVec2)>,
    #[rust] pub view_rect: Rect,
    #[rust] pub window: Option<PanelWindow>,
    #[rust] pub window_size: DVec2,
    #[rust] accessible_name: String,
    #[rust] last: Weak<Picture>,
    #[rust] texture: Option<(Texture, u32, u32)>,
    #[rust] reported: Option<(SessionId, u32, u32)>,
    #[rust] motion: FrameMotion,
    #[rust] motion_frame: Option<NextFrame>,
    #[rust] buttons: u8,
    #[rust] last_pointer: DVec2,
    #[rust] pointer_mods: Mods,
    #[rust] held_keys: Vec<(KeyCode, Mods)>,
    #[rust] panel_grant: Option<GrantId>,
}

impl super::ShareShown {
    /// Bind the panel and return its scale button's label.
    pub(crate) fn show_panel(&self, cx: &mut Cx, panel: &WidgetRef, session: Option<SessionId>, active: bool, name: &str, peer: &str, member: bool) -> &'static str {
        let visible = active && session.and_then(|s| self.offer(s)).is_some_and(|o| self.shows_picture(o));
        let mut label = "1:1";
        if let Some(mut inner) = panel.borrow_mut::<SharePanel>() {
            if inner.session != session {
                inner.clear_picture(cx);
                inner.session = session;
            }
            if inner.one_to_one.is_some() {
                if self.watching == session && inner.one_to_one == session {
                    label = "Fit";
                } else {
                    inner.set_scale(cx, None);
                }
            }
            if let Some(session) = session.filter(|s| visible && self.watching == Some(*s)) {
                inner.bind_controls(cx, self, session, peer, member);
            }
            if !visible { inner.clear_picture(cx); }
            if inner.accessible_name != name { inner.accessible_name = name.to_owned(); }
        }
        panel.set_visible(cx, visible);
        label
    }

    pub fn toggle_panel_scale(&self, cx: &mut Cx, panel: &WidgetRef) {
        let Some(offer) = self.watched() else { return };
        if let Some(mut panel) = panel.borrow_mut::<SharePanel>() {
            let session = (panel.one_to_one != Some(offer.session)).then_some(offer.session);
            panel.set_scale(cx, session);
        }
    }
}

impl super::CallController {
    /// Finish a drag or held keystroke on the old surface before moving the same grant to another window.
    pub fn leave_share_panel(&mut self, widget: &WidgetRef) {
        let Some(mut panel) = widget.borrow_mut::<SharePanel>() else { return };
        let (Some(PanelWindow::Remote(session)), Some(grant)) = (panel.window, panel.panel_grant) else { return };
        panel.release_held(session, grant, |input| self.panel_input(&input));
    }
}

impl SharePanel {
    #[cfg(test)]
    pub(crate) fn ghost_ink(&self) -> &overlay::DrawDesignGhost { &self.ghost_ink }

    fn release_held(&mut self, session: SessionId, grant: GrantId, mut send: impl FnMut(PanelInput)) {
        self.motion.flush(&mut send);
        let mut release = |event| send(PanelInput { session, grant, event });
        for button in 0..3 {
            if self.buttons & (1 << button) != 0 {
                release(AppInput::PointerUp { x: self.last_pointer.x, y: self.last_pointer.y, button, mods: self.pointer_mods });
            }
        }
        self.buttons = 0;
        for (key, mods) in self.held_keys.drain(..) { release(AppInput::KeyUp { key, mods }); }
    }
    fn bind_controls(&mut self, cx: &mut Cx, share: &super::ShareShown, session: SessionId, peer: &str, member: bool) {
        let controls = &self.controls;
        let peer = if peer.is_empty() { "Peer" } else { peer };
        controls.label(cx, ids!(stream_name)).set_text(cx, peer);
        if self.member_color != Some(member) {
            self.member_color = Some(member);
            let mut chip = controls.widget(cx, ids!(chip));
            script_apply_eval!(cx, chip, {draw_bg.color: if #(member) {mod.palette.peer} else {mod.palette.accent}});
        }
        let close = controls.button(cx, ids!(close_popout));
        close.set_visible(cx, self.popout);
        if self.popout { close.set_action_data(ShareControl::Close(session)); }
        let grant = share.control.active.filter(|_| share.control_session == Some(session));
        controls.widget(cx, ids!(controlling)).set_visible(cx, grant.is_some());
        let asking = share.control.asking == Some(session);
        let mut button = controls.button(cx, ids!(request_control));
        let (label, action) = match grant {
            Some(id) => ("Hand back", ShareControl::Release(session, id)),
            None if asking => ("Asking…", ShareControl::Ask(session, false)),
            None if share.control.allowed == Some(session) => ("Take control", ShareControl::Ask(session, true)),
            None => ("Ask for control", ShareControl::Ask(session, true)),
        };
        let was_controlling = matches!(button.action_data().as_deref().and_then(|d| d.downcast_ref::<ShareControl>()),
            Some(ShareControl::Release(..)));
        if was_controlling != grant.is_some() {
            let quiet = grant.is_some();
            script_apply_eval!(cx, button, {draw_text.color: if #(quiet) {mod.palette.muted} else {mod.palette.text}});
        }
        button.set_text(cx, label);
        button.set_action_data(action);
    }

    pub fn over_controls(&self, cx: &Cx, abs: DVec2) -> bool {
        let area = self.controls.area();
        area.is_valid(cx) && area.clipped_rect(cx).contains(abs)
    }

    pub fn set_window_controls(&mut self, cx: &mut Cx, chrome: ResolvedChrome, expanded: bool) {
        if self.caption == Some((chrome, expanded)) { return }
        self.caption = Some((chrome, expanded));
        self.caption_buttons.child(id!(max)).set_visible(cx, !expanded);
        self.caption_buttons.child(id!(restore)).set_visible(cx, expanded);
        self.redraw(cx);
    }

    pub fn window_buttons(&self) -> Option<&WidgetRef> {
        match self.caption?.0 {
            ResolvedChrome::Lights => Some(&self.caption_lights),
            ResolvedChrome::Buttons => Some(&self.caption_buttons),
            _ => None,
        }
    }

    fn over_window_buttons(&self, cx: &Cx, abs: DVec2) -> bool {
        self.window_buttons().is_some_and(|buttons| {
            let area = buttons.area();
            area.is_valid(cx) && area.clipped_rect(cx).contains(abs)
        })
    }

    pub fn caption_at(&self, cx: &Cx, abs: DVec2, controlled: bool) -> bool {
        let over = |area: Area| area.is_valid(cx) && area.clipped_rect(cx).contains(abs);
        if self.over_window_buttons(cx, abs) || self.scroll_bars.bar_areas().into_iter().any(over) { return false }
        if self.over_controls(cx, abs) {
            if over(self.controls.widget(cx, ids!(close_popout)).area())
                || over(self.controls.widget(cx, ids!(request_control)).area()) { return false }
            if controlled { return over(self.controls.widget(cx, ids!(chip)).area()) }
        }
        !controlled
    }

    fn set_scale(&mut self, cx: &mut Cx, session: Option<SessionId>) {
        self.one_to_one = session;
        self.pan_drag = None;
        self.scroll_bars.set_scroll_pos(cx, DVec2::default());
        self.redraw(cx);
    }

    fn pan_to(&mut self, cx: &mut Cx, pan: DVec2) {
        if self.scroll_bars.set_scroll_pos(cx, pan) { self.redraw(cx); }
    }

    fn watch_picture_input(&mut self, cx: &mut Cx, event: &Event, share: &super::ShareShown, pannable: bool) {
        let overlay = picture_overlay(self.session, share);
        if self.resize.is_some() && (overlay.is_none_or(|(_, s)| !s.build_mode || !s.can_edit)
            || matches!(event, Event::WindowLostFocus(_) | Event::DragEnd)) {
            self.resize.take().unwrap().cancel(cx, self.uid);
        }
        if !pannable && overlay.is_none() { return }
        if matches!(event, Event::WindowLostFocus(_) | Event::DragEnd) {
            self.lift = None;
            self.design_over = None;
        }
        if let Some(abs) = design_pointer(event) {
            let target = overlay.and_then(|(item, s)| {
                if !s.can_edit || !self.picture_ready(share) { return None }
                Some((*item, s.window, self.picture_pointer(cx, share, abs, false)?))
            });
            picture_drop(cx, self.uid, self.draw_bg.area(), event, target, &mut self.design_over);
            return
        }
        let tf = overlay.map(|(_, s)| s.transform(self.picture_bounds(self.draw_bg.area().rect(cx), share)));
        let can_hit_handle = |cx: &Cx, abs| self.picture_ready(share)
            && self.draw_bg.area().clipped_rect(cx).contains(abs)
            && !self.over_controls(cx, abs) && !self.over_window_buttons(cx, abs);
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerDown(e) if e.is_primary_hit() => {
                if let Some((item, s)) = overlay.filter(|_| can_hit_handle(cx, e.abs)) {
                    self.resize = overlay::ResizePress::begin(*item, tf.unwrap(), s, e.digit_id, e.abs);
                    if self.resize.is_some() { cx.claim_finger_drag(e.digit_id); return }
                }
                let Some(point) = self.picture_pointer(cx, share, e.abs, false) else { return };
                if let Some((item, s)) = overlay {
                    if let Some(path) = s.selected_at(tf.as_ref().unwrap(), e.abs).filter(|_| self.picture_ready(share)) {
                        self.lift = Some(LiftPress { item: *item, digit: e.digit_id, start: e.abs, path: path.into() });
                        cx.claim_finger_drag(e.digit_id);
                    } else { self.tweak_pick(cx, share, point); }
                } else {
                    cx.claim_finger_drag(e.digit_id);
                    self.pan_drag = Some((e.digit_id, self.scroll_bars.get_scroll_pos()));
                }
            }
            Hit::FingerMove(e) if self.resize.as_ref().is_some_and(|r| r.digit == e.digit_id) => {
                self.resize.as_mut().unwrap().update(cx, self.uid, e.abs, overlay::ResizePhase::Drag);
            }
            Hit::FingerUp(e) if self.resize.as_ref().is_some_and(|r| r.digit == e.digit_id) => {
                let mut resize = self.resize.take().unwrap();
                if e.cancelled || e.is_sweep { resize.cancel(cx, self.uid); }
                else { resize.update(cx, self.uid, e.abs, overlay::ResizePhase::Release); }
            }
            Hit::FingerMove(e) => {
                if let Some(p) = &self.lift {
                    if p.digit == e.digit_id && p.lift(cx, e.abs, &self.drag_chip) { self.lift = None; }
                }
                if let Some((digit, start)) = self.pan_drag {
                    if digit == e.digit_id { self.pan_to(cx, start + (e.abs_start - e.abs)); }
                }
            }
            Hit::FingerUp(e) if self.lift.as_ref().is_some_and(|p| p.digit == e.digit_id) => {
                self.lift = None;
                if !e.cancelled && !e.is_sweep {
                    if let Some(point) = self.picture_pointer(cx, share, e.abs, false) { self.tweak_pick(cx, share, point); }
                }
            }
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) if overlay.is_some() => {
                let s = &overlay.unwrap().1;
                let tf = tf.as_ref().unwrap();
                if can_hit_handle(cx, e.abs) {
                    if let Some(handle) = overlay::hit_size_handle(tf, s, e.abs) {
                        cx.set_cursor(handle.cursor());
                        return;
                    }
                }
                if self.picture_pointer(cx, share, e.abs, false).is_some() && s.selected_at(tf, e.abs).is_some() {
                    cx.set_cursor(MouseCursor::Hand);
                }
            }
            Hit::FingerUp(e) if self.pan_drag.is_some_and(|(digit, _)| digit == e.digit_id) => self.pan_drag = None,
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) if pannable && share.overlay.is_none() => {
                if self.picture_pointer(cx, share, e.abs, false).is_some() { cx.set_cursor(MouseCursor::Hand); }
            }
            _ => {}
        }
    }

    fn picture_bounds(&self, panel: Rect, share: &super::ShareShown) -> Rect {
        let native = self.popout || self.one_to_one.is_some();
        let mut rect = picture_rect(panel, self.picture_points, native, self.scroll_bars.get_scroll_pos(), self.dpi);
        if native && picture_overlay(self.session, share).is_some_and(|(_, s)|
            s.focus.isolated_rect.is_some_and(|r| r.window == s.window) && (s.focus.center || s.focus.zoom > 1.)) {
            // A native-size picture can be larger than its pane. Anchor the focused view to
            // the visible pane's center while keeping its native scale and saved scroll position.
            rect.pos = panel.center() - rect.size * 0.5;
        }
        rect
    }

    fn picture_pointer(&self, cx: &Cx, share: &super::ShareShown, abs: DVec2, captured: bool) -> Option<DVec2> {
        self.window?;
        let area = self.draw_bg.area();
        if !captured && (!area.clipped_rect(cx).contains(abs) || self.over_controls(cx, abs)
            || self.over_window_buttons(cx, abs)) { return None }
        let rect = self.picture_bounds(area.rect(cx), share);
        if let Some((_, selections)) = picture_overlay(self.session, share) {
            let tf = selections.transform(rect);
            let p = if captured { tf.inverse(abs) } else { tf.screen_to_app(abs)? };
            return valid_point((self.window_size.x, self.window_size.y), p.x, p.y, captured).then_some(p)
        }
        window_point(rect, self.window_size, abs, captured)
    }

    fn picture_ready(&self, share: &super::ShareShown) -> bool {
        self.session.and_then(|id| share.offer(id)).is_some_and(|offer| share.picture_for(offer.session).is_some()
            && self.texture.is_some() && self.reported.map(|t| t.0) == Some(offer.session)
            && self.window_size == dvec2(offer.window_size.0, offer.window_size.1))
    }

    fn tweak_pick(&self, cx: &mut Cx, share: &super::ShareShown, point: DVec2) {
        if !self.picture_ready(share) { return }
        let Some((item, rings)) = picture_overlay(self.session, share) else { return };
        if let Some(at) = crate::tweak::overlay::pick_at(rings.window, point, rings.window_size) {
            cx.widget_action(self.uid, crate::board::canvas::AppPick { item: *item, at });
        }
    }

    fn draw_picture(&mut self, cx: &mut Cx2d, share: &super::ShareShown, offer: &super::ShareOffer, visible: Rect) {
        let Some(picture) = share.picture_for(offer.session) else {
            self.release_texture();
            if let Some(line) = share.waiting_line(offer) {
                let waiting = self.view_rect.clip((visible.pos, visible.pos + visible.size));
                cx.push_clip_rect(waiting);
                self.draw_waiting.draw_walk_with_wrap(cx, Walk::abs_rect(waiting), Align { x: 0.5, y: 0.5 }, false, line);
                cx.pop_clip_rect();
            }
            return
        };
        if self.last.as_ptr() != Arc::as_ptr(picture) {
            let (w, h) = (picture.width, picture.height);
            match &mut self.texture {
                Some((texture, tw, th)) if (*tw, *th) == (w, h) => {
                    let mut bytes = texture.take_vec_u8(cx);
                    bytes.clear();
                    bytes.extend_from_slice(&picture.nv12);
                    texture.put_back_vec_u8(cx, bytes, None);
                }
                _ => self.texture = Some((Texture::new_nv12(cx, w, h, picture.nv12.clone()), w, h)),
            }
            self.last = Arc::downgrade(picture);
        }
        if let Some((texture, w, h)) = &self.texture {
            self.draw_nv12.set_nv12(texture, *w, *h, Default::default(), None);
            if let Some((_, rings)) = picture_overlay(Some(offer.session), share) {
                let tf = rings.transform(self.view_rect);
                let (uv, dest) = tf.texture_crop();
                let pixels = dvec2(*w as f64, *h as f64);
                let origin = uv.pos * pixels;
                let extent = uv.size * pixels;
                self.draw_nv12.src = vec4(origin.x as f32, origin.y as f32, extent.x as f32, extent.y as f32);
                cx.push_clip_rect(self.view_rect);
                if dest.size.x > 0. && dest.size.y > 0. { self.draw_nv12.draw_abs(cx, dest); }
                overlay::draw_isolate_scrim(cx, &tf, rings, &mut self.ink);
                overlay::draw_selections(cx, &tf, rings, &mut self.ink, &mut self.label, &mut self.ghost_ink);
                cx.pop_clip_rect();
            } else { self.draw_nv12.draw_abs(cx, self.view_rect); }
            // Only a guest's first picture joins the share; the sharer's own view is not a join.
            if matches!(offer.role, ShareRole::Viewer { .. }) { offer.picture_drawn(); }
        }
    }

    /// The picture this panel drew last, while it still exists.
    pub fn drawn(&self) -> Option<Arc<Picture>> { self.last.upgrade() }

    fn release_texture(&mut self) {
        if self.texture.is_none() { return }
        self.texture = None;
        self.last = Weak::new();
        self.draw_nv12.draw_vars.empty_texture(0);
        self.draw_nv12.draw_vars.empty_texture(1);
    }

    pub(super) fn clear_picture(&mut self, cx: &mut Cx) {
        if self.window.is_none() && self.texture.is_none() { return }
        let area = self.draw_bg.area();
        if !area.is_empty() && cx.has_key_focus(area) { cx.revert_key_focus(); }
        self.release_texture();
        self.lift = None;
        if let Some(mut resize) = self.resize.take() { resize.cancel(cx, self.uid); }
        self.design_over = None;
        self.reported = None;
        self.window = None;
        self.pan_drag = None;
        self.panel_grant = None;
        self.buttons = 0;
        self.held_keys.clear();
        self.motion = FrameMotion::default();
        self.motion_frame = None;
        self.view_rect = Rect::default();
    }
}

impl Widget for SharePanel {
    /// The shared picture is one image to assistive tech and automation, so its bounds can be found.
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Image;
        node.name.clone_from(&self.accessible_name);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible { return DrawStep::done() }
        let share = scope.data.get::<crate::lists::ViewData>().map(|data| &data.call.share);
        let Some((share, offer)) = share.and_then(|s| s.offer(self.session?).filter(|o| s.shows_picture(o)).map(|o| (s, o))) else {
            self.clear_picture(cx);
            return DrawStep::done();
        };
        self.window = Some(match offer.role {
            ShareRole::Sharer { window, .. } => PanelWindow::Local(window),
            ShareRole::Viewer { .. } => PanelWindow::Remote(offer.session),
        });
        self.window_size = dvec2(offer.window_size.0, offer.window_size.1);
        let grant = share.control.active.filter(|_| share.control_session == Some(offer.session));
        if self.panel_grant != grant {
            self.panel_grant = grant;
            self.pan_drag = None;
            self.buttons = 0;
            self.held_keys.clear();
            self.motion = FrameMotion::default();
            self.motion_frame = None;
            let area = self.draw_bg.area();
            if !area.is_empty() && cx.has_key_focus(area) { cx.revert_key_focus(); }
        }
        let shape = layout_size(self.window_size);
        let rect = cx.walk_turtle(walk);
        let visible = rect.clip(cx.turtle_ancestor_clip());
        if visible.size.x <= 0. || visible.size.y <= 0. { return DrawStep::done() }
        let one_to_one = self.popout || self.one_to_one == Some(offer.session);
        self.dpi = cx.current_dpi_factor();
        self.picture_points = if one_to_one {
            share.picture_for(offer.session).map_or(shape, |p| dvec2(p.width as f64, p.height as f64) / self.dpi)
        } else { shape };
        self.view_rect = self.picture_bounds(rect, share);
        self.draw_nv12.sharp = if one_to_one { 2.0 } else { 0.0 };
        self.draw_bg.draw_abs(cx, rect);
        let pixels = if self.popout {
            shape * (NATIVE_REQUEST / shape.x.max(shape.y))
        } else if one_to_one { shape * self.dpi } else { self.view_rect.size * self.dpi };
        let size = (offer.session, pixels.x.round() as u32, pixels.y.round() as u32);
        if size.1 > 0 && size.2 > 0 && self.reported != Some(size) {
            self.reported = Some(size);
            cx.widget_action(self.uid, SharePanelSize { session: size.0, width: size.1, height: size.2 });
        }
        if one_to_one {
            // A fixed viewport clips absolute picture coordinates. Its used size gives the bars their range.
            cx.begin_turtle(Walk::abs_rect(rect), Layout::default());
        }
        self.draw_picture(cx, share, offer, visible);
        if one_to_one {
            cx.turtle_mut().set_used(self.picture_points.x, self.picture_points.y);
            self.scroll_bars.set_area(self.draw_bg.area());
            self.scroll_bars.draw_scroll_bars(cx);
            cx.end_turtle();
        }
        if matches!(offer.role, ShareRole::Viewer { .. }) {
            let picture = self.view_rect.clip((visible.pos, visible.pos + visible.size));
            // Let a small picture's chip extend into the surrounding letterbox.
            let width = 2. * (picture.center().x - visible.pos.x).min(visible.pos.x + visible.size.x - picture.center().x);
            let available = Rect { pos: dvec2(picture.center().x - width * 0.5, picture.pos.y),
                size: dvec2(width, visible.pos.y + visible.size.y - picture.pos.y) };
            let start = cx.align_list_len();
            cx.begin_turtle(Walk::abs_rect(available), Layout::flow_overlay().with_align_x(0.5));
            self.controls.draw_all(cx, scope);
            cx.end_turtle();
            if let Some(buttons) = self.window_buttons() {
                let chip_range = TurtleAlignRange { start, end: cx.align_list_len() };
                let lights = matches!(self.caption, Some((ResolvedChrome::Lights, _)));
                let inset = if lights { dvec2(8., 4.) } else { DVec2::default() };
                let band = Rect { pos: rect.pos + inset, size: rect.size - inset };
                cx.begin_turtle(Walk::abs_rect(band), Layout::flow_overlay().with_align_x(if lights { 0. } else { 1. }));
                buttons.draw_all(cx, scope);
                cx.end_turtle();
                // In a narrow window, keep both the centred chip and the corner buttons reachable.
                let corner = buttons.area().rect(cx);
                let chip = self.controls.area().rect(cx);
                if corner.intersects(chip) {
                    cx.shift_align_range(&chip_range, dvec2(0., corner.pos.y + corner.size.y - chip.pos.y));
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.visible { return }
        if matches!(self.window, Some(PanelWindow::Local(_))) {
            if let Some(share) = scope.data.get::<crate::lists::ViewData>().map(|d| &d.call.share) {
                self.watch_picture_input(cx, event, share, false);
            }
            return
        }
        let Some(PanelWindow::Remote(session)) = self.window else { return };
        if scope.data.get::<crate::lists::ViewData>().and_then(|d| d.call.share.watching) != Some(session) { return }
        let uid = self.uid;
        if let Some(buttons) = self.window_buttons() { buttons.handle_event(cx, event, scope); }
        cx.mutate_actions(|cx| self.controls.handle_event(cx, event, scope), |actions| {
            for action in actions {
                let Some(w) = action.downcast_mut::<WidgetAction>() else { continue };
                if matches!(w.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) {
                    if let Some(control) = w.data.as_ref().and_then(|d| d.downcast_ref::<ShareControl>()) {
                        w.action = Box::new(*control);
                        w.widget_uid = uid;
                    }
                }
            }
        });
        let Some(share) = scope.data.get::<crate::lists::ViewData>().map(|d| &d.call.share) else { return };
        if let Event::Scroll(e) = event {
            let wheel = e.is_mouse || e.phase == ScrollPhase::None;
            let zoom = picture_overlay(self.session, share).filter(|(_, s)| s.focus.isolated_path.is_some())
                .and_then(|(item, _)| AppZoom::wheel(*item, e.scroll, wheel, &e.modifiers));
            if let Some(zoom) = zoom {
                // While a seat drives the app, its wheel scrolls the app, as before isolation.
                if share.control.active.is_none() && self.picture_pointer(cx, share, e.abs, false).is_some()
                    && matches!(event.hits(cx, self.draw_bg.area()), Hit::FingerScroll(_)) {
                    cx.widget_action(self.uid, zoom);
                    e.handled_x.set(true);
                    e.handled_y.set(true);
                    return
                }
            }
        }
        let one_to_one = self.popout || self.one_to_one == Some(session);
        let mut pannable = false;
        if one_to_one {
            // The handles get first hit on their strips, even during takeover.
            self.scroll_bars.handle_main_event(cx, event, &mut Scope::empty(), &mut Vec::new());
            let bars = self.scroll_bars.bar_areas();
            let claimed = event.pointer_claimed_area();
            if !claimed.is_empty() && bars.contains(&claimed) { return }
            let extent = self.scroll_bars.extent().max();
            pannable = extent.x > 0. || extent.y > 0.;
            if let Event::Scroll(wheel) = event {
                if !matches!(event.hits(cx, self.draw_bg.area()), Hit::FingerScroll(_)) { return }
                let over = |bar: Area| bar.is_valid(cx) && bar.clipped_rect(cx).contains(wheel.abs);
                let on_x = extent.x > 0. && over(bars[0]);
                let on_y = extent.y > 0. && over(bars[1]);
                if on_x || on_y || (pannable && share.control.active.is_none() && self.picture_pointer(cx, share, wheel.abs, false).is_some()) {
                    let d = wheel.scroll;
                    let delta = if on_x || (!on_y && wheel.modifiers.shift) { dvec2(d.x + d.y, 0.) }
                        else if on_y { dvec2(0., d.y) } else { d };
                    self.pan_to(cx, self.scroll_bars.get_scroll_pos() + delta);
                    wheel.handled_x.set(true);
                    wheel.handled_y.set(true);
                    return;
                }
            }
        }
        let Some(grant) = share.control.active.filter(|_| share.control_session == Some(session)) else {
            // The pop-out's presses move its window, except while the tweak overlay picks.
            if !self.popout || share.overlay.is_some() { self.watch_picture_input(cx, event, share, pannable); }
            return;
        };
        if Some(grant) != self.panel_grant { return }
        let uid = self.uid;
        if let Event::NextFrame(frame) = event {
            if self.motion_frame.is_some_and(|id| frame.set.contains(&id)) {
                self.motion_frame = None;
                self.motion.flush(|input| cx.widget_action(uid, input));
            }
            return;
        }
        let area = self.draw_bg.area();
        if area.is_empty() { return }
        if cx.has_key_focus(area) {
            let outside = match event {
                Event::MouseDown(e) => self.picture_pointer(cx, share, e.abs, false).is_none(),
                Event::TouchUpdate(e) => e.touches.iter().any(|t| t.state == TouchState::Start
                    && self.picture_pointer(cx, share, t.abs, false).is_none()),
                _ => false,
            };
            if outside { cx.revert_key_focus(); return }
        }
        let mods = |m: KeyModifiers| Mods { shift: m.shift, control: m.control, alt: m.alt, logo: m.logo };
        let button = |d: &DigitDevice| match d.mouse_button().unwrap_or(MouseButton::PRIMARY) {
            MouseButton::PRIMARY => Some(0), MouseButton::SECONDARY => Some(1),
            MouseButton::MIDDLE => Some(2), _ => None,
        };
        let input = match event.hits(cx, area) {
            Hit::FingerDown(e) => {
                let Some(p) = self.picture_pointer(cx, share, e.abs, false) else { return };
                let Some(button) = button(&e.device) else { return };
                self.buttons |= 1 << button;
                self.last_pointer = p;
                self.pointer_mods = mods(e.modifiers);
                cx.set_key_focus_from_user(area);
                AppInput::PointerDown { x: p.x, y: p.y, button, mods: mods(e.modifiers) }
            }
            Hit::FingerUp(e) => {
                let Some(button) = button(&e.device) else { return };
                if self.buttons & (1 << button) == 0 { return }
                let Some(p) = self.picture_pointer(cx, share, e.abs, true) else { return };
                self.buttons &= !(1 << button);
                AppInput::PointerUp { x: p.x, y: p.y, button, mods: mods(e.modifiers) }
            }
            Hit::FingerMove(e) => {
                let Some(p) = self.picture_pointer(cx, share, e.abs, self.buttons != 0) else { return };
                self.last_pointer = p;
                self.pointer_mods = mods(e.modifiers);
                AppInput::PointerMove { x: p.x, y: p.y }
            }
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let Some(p) = self.picture_pointer(cx, share, e.abs, false) else { return };
                // The pop-out holds nothing else: keys follow the pointer there. Docked, a press takes them.
                if self.popout && !cx.has_key_focus(area) { cx.set_key_focus_from_user(area); }
                AppInput::PointerMove { x: p.x, y: p.y }
            }
            Hit::FingerScroll(e) => {
                let Some(p) = self.picture_pointer(cx, share, e.abs, false) else { return };
                if let Event::Scroll(s) = event { s.handled_x.set(true); s.handled_y.set(true); }
                AppInput::Scroll { x: p.x, y: p.y, dx: e.scroll.x, dy: e.scroll.y, mods: mods(e.modifiers) }
            }
            Hit::KeyFocusLost(_) => {
                self.release_held(session, grant, |input| cx.widget_action(uid, input));
                return
            }
            Hit::KeyFocus(_) => { cx.set_key_focus_takes_text(area, true); return }
            Hit::KeyDown(e) => {
                let mods = mods(e.modifiers);
                if !self.held_keys.iter().any(|(key, _)| *key == e.key_code) { self.held_keys.push((e.key_code, mods)); }
                AppInput::KeyDown { key: e.key_code, mods }
            }
            Hit::KeyUp(e) => {
                self.held_keys.retain(|(key, _)| *key != e.key_code);
                AppInput::KeyUp { key: e.key_code, mods: mods(e.modifiers) }
            }
            Hit::TextInput(e) => AppInput::Text(e.input),
            _ => return,
        };
        if self.motion.push(PanelInput { session, grant, event: input }, |i| cx.widget_action(uid, i))
            && self.motion_frame.is_none() {
            self.motion_frame = Some(cx.new_next_frame());
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ShareChoices {
    #[live] placeholder: DrawColor,
    #[live] still_image: DrawQuad,
    #[live] live_image: DrawNv12,
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
}

impl ShareChoices {
    fn draw_thumbnail(&mut self, cx: &mut Cx2d, row: Rect, thumbnail: &WindowThumbnail) {
        let width = 96f64.min((row.size.x - 12.).max(0.));
        let bounds = Rect { pos: row.pos + dvec2(6., (row.size.y - 54.) * 0.5), size: dvec2(width, 54.) };
        self.placeholder.draw_abs(cx, bounds);
        let Some(Ok(picture)) = thumbnail.picture.get() else { return };
        let fit = |w, h| picture_rect(bounds, dvec2(w, h), false, DVec2::default(), 1.);
        match picture {
            ThumbnailPicture::Still(p) => {
                let texture = thumbnail.texture.get_or_init(|| Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                    width: p.width, height: p.height, data: Some(p.data.clone()), updated: TextureUpdated::Full,
                }));
                self.still_image.draw_vars.set_texture(0, texture);
                self.still_image.draw_abs(cx, fit(p.width as f64, p.height as f64));
                self.still_image.draw_vars.empty_texture(0);
            }
            ThumbnailPicture::Shared(p) => {
                let texture = thumbnail.texture.get_or_init(|| Texture::new_nv12(cx, p.width, p.height, p.nv12.clone()));
                self.live_image.set_nv12(texture, p.width, p.height, Default::default(), None);
                self.live_image.draw_abs(cx, fit(p.width as f64, p.height as f64));
                self.live_image.draw_vars.empty_texture(0);
                self.live_image.draw_vars.empty_texture(1);
            }
        }
    }
}

impl Widget for ShareChoices {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(choices) = scope.data.get::<crate::lists::ViewData>().and_then(|d| d.call.share.choices.clone()) else {
            return DrawStep::done();
        };
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<FlatList>() else { continue };
            for row in choices.iter() {
                let template = if row.thumbnail.is_some() { id!(WindowChoice) }
                    else if row.selectable { id!(Choice) } else { id!(Empty) };
                let Some(item) = list.item(cx, row.widget_id, template) else { continue };
                item.set_text(cx, &row.label);
                if row.thumbnail.is_some() { item.set_disabled(cx, !row.selectable); }
                if row.selectable {
                    let button = item.as_button();
                    let press = super::Press::Share(row.choice);
                    if button.action_data().as_deref().and_then(|d| d.downcast_ref::<super::Press>()) != Some(&press) {
                        button.set_action_data(press);
                    }
                }
                item.draw_all(cx, &mut Scope::empty());
                if let Some(preview) = &row.thumbnail { self.draw_thumbnail(cx, item.area().rect(cx), preview); }
            }
            let before = list.items.len();
            list.items.retain_visible();
            if list.items.len() != before { cx.widget_tree_mark_dirty(list.widget_uid()); }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let uid = self.widget_uid();
        cx.mutate_actions(|cx| self.view.handle_event(cx, event, scope), |actions| {
            for a in actions {
                let Some(a) = a.downcast_mut::<WidgetAction>() else { continue };
                if matches!(a.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) {
                    if let Some(p @ super::Press::Share(_)) = a.data.as_ref().and_then(|d| d.downcast_ref::<super::Press>()) {
                        a.action = Box::new(super::CallAction::Press(*p));
                        a.widget_uid = uid;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_uids_includes_all_dynamic_let_control_buttons_and_join_call() {
        use crate::call::{CallAction, CallController, CallEnv, Message, Press, ShareOfferExt};
        use crate::lists::{Live, Page, ViewData};
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_town_core::share::hello::MediaCaps;
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, { use mod.widgets.* View{flow: Down
                project_call := CallButton{} home_join_call := JoinCallButton{} live_share := LiveSharePage{}
            } });
            WidgetRef::script_from_value(vm, root)
        });
        let (session, town, ben, kit) = ([1; 16], [2; 32], [3; 65], [4; 65]);
        let mut ctl = CallController::default();
        ctl.configure(CallEnv { devices_path: Default::default(), files: Default::default(), spawn: std_spawn(), jobs: std_spawn(),
            signal: Arc::new(|| {}), voice: VoiceSource::Off });
        let port = ctl.link_up(cx, ben, kit, "Kit").unwrap();
        ctl.set_open_town(Some((town, vec![(kit, "Kit".into())])));
        ctl.set_peer_caps(kit, MediaCaps(MediaCaps::MULTIPARTY_SHARE_V1));
        port.messages_in.try_send(Message::Share { session, title: "Fab".into(), w: 320., h: 180.,
            ext: ShareOfferExt { town: Some(town), ..Default::default() } }.encode()).unwrap();
        ctl.poll(cx);
        let mut data = ViewData { page: Page::Live(Live::Share(session)), call: ctl.shown(), ..Default::default() };
        let offer = Arc::make_mut(&mut Arc::make_mut(&mut data.call.share.offers)[0]);
        offer.role = ShareRole::Sharer { link: crate::app_link::LinkId::TEST, window: WindowId(1) };
        offer.watchers = vec![(ben, "Ben".into()), (kit, "Kit".into())];
        data.call.share.control.requests = vec![(session, kit, "Kit".into()), (session, ben, "Ben".into())];
        data.sync_live_share(cx, &ui);
        let page = ui.widget(cx, ids!(live_share));
        let mut target = ui_checks::UiCheckTarget::new(cx);
        target.draw(cx, &ui, dvec2(400., 320.));
        assert_eq!(page.label(cx, ids!(share_watchers)).text(), "Watching: Ben, Kit");
        assert_eq!(page.label(cx, ids!(sharer_status)).text(), "Kit asks to control Fab");
        let primary = page.button(cx, ids!(let_control));
        let requests = page.flat_list(cx, ids!(control_requests));
        let row = requests.borrow().unwrap().items.values().next().expect("Ben's request row").widget.clone();
        assert_eq!(row.label(cx, ids!(prompt)).text(), "Ben asks to control Fab");
        let allow = row.button(cx, ids!(allow));
        let mut a11y = a11y_tree::Node::default();
        allow.borrow().unwrap().semantics(cx, &mut a11y);
        assert_eq!(a11y.name, "Allow Ben to control Fab");
        for (button, peer) in [(&primary, kit), (&allow, ben)] {
            assert_eq!(button.text(), "Allow");
            assert!(button.area().rect(cx).size.x > 0., "the Allow control was drawn");
            assert_eq!(button.action_data().as_ref().and_then(|a| a.downcast_ref::<CallAction>()),
                Some(&CallAction::OnShare(session, Press::LetControl(peer))));
        }
        let opening = opening_uids(cx, &ui);
        for uid in [ui.widget(cx, ids!(project_call.call_join)).widget_uid(), ui.widget(cx, ids!(home_join_call.call_join)).widget_uid(),
            primary.widget_uid(), page.widget(cx, ids!(allow_all)).widget_uid(), allow.widget_uid()] {
            assert!(opening.contains(&uid), "opening control {uid:?}");
        }
        data.call.share.control.requests.pop();
        data.call.share.control_session = Some(session);
        data.call.share.control.active = Some([5; 16]);
        data.call.share.control.holder = Some((ben, "Ben".into()));
        data.sync_live_share(cx, &ui);
        assert_eq!(page.label(cx, ids!(sharer_status)).text(), "Ben is controlling Fab · Kit asks to control Fab");
        assert!(!page.widget(cx, ids!(pending_requests)).visible() && requests.borrow().unwrap().items.is_empty());
        assert!(page.borrow::<crate::ui::live::LiveSharePage>().unwrap().let_control_buttons().is_empty());
        assert!(!opening_uids(cx, &ui).contains(&allow.widget_uid()));
        ctl.shutdown(cx);
    }

    #[test]
    fn pointer_maps_the_displayed_window_and_excludes_both_kinds_of_bars() {
        let watched = Some([1; 16]);
        let selections = overlay::Selections::new(0, [800., 400.], &Default::default(), &Default::default());
        let mut share = crate::call::ShareShown::default();
        share.watching = watched;
        share.overlay = Some(([0; 32], selections));
        assert!(picture_overlay(watched, &share).is_some());
        assert!(picture_overlay(Some([2; 16]), &share).is_none(), "a different pop-out uses its own picture coordinates");
        for size in [[0., 400.], [800., -1.], [f64::NAN, 400.], [800., f64::INFINITY]] {
            share.overlay.as_mut().unwrap().1.window_size = size;
            assert!(picture_overlay(watched, &share).is_none(), "invalid window size {size:?}");
        }
        let panel = Rect { pos: dvec2(10., 20.), size: dvec2(400., 300.) };
        for (size, origin, end, bar) in [
            (dvec2(800., 400.), dvec2(10., 70.), dvec2(410., 270.), dvec2(210., 69.)),
            (dvec2(300., 600.), dvec2(135., 20.), dvec2(285., 320.), dvec2(286., 170.)),
        ] {
            let rect = picture_rect(panel, size, false, DVec2::default(), 1.);
            assert_eq!(window_point(rect, size, origin, false), Some(DVec2::default()));
            assert_eq!(window_point(rect, size, end, false), Some(size));
            assert_eq!(window_point(rect, size, panel.center(), false), Some(size * 0.5));
            assert_eq!(window_point(rect, size, bar, false), None);
            let p = window_point(rect, size, bar, true).expect("capture follows a drag over the bar");
            assert!(p.x < 0. || p.y < 0. || p.x > size.x || p.y > size.y, "actual outside point");
            assert!(window_point(rect, size, rect.pos - rect.size * 1.01, true).is_none());
        }
        assert_eq!(window_point(Rect::default(), dvec2(1., 1.), DVec2::default(), false), None);
        let window = dvec2(1600., 1000.);
        let pan = dvec2(300., 200.);
        let rect = picture_rect(panel, window, true, pan, 1.);
        assert_eq!(rect.size, window, "one source pixel occupies one device pixel at 1x");
        // An 800-point app captured at 2x, displayed on a guest at 1.25x.
        let app = dvec2(800., 500.);
        let dpi = 1.25;
        let native = picture_rect(panel, window / dpi, true, pan, dpi);
        assert_eq!(native.size * dpi, window);
        assert_eq!(native.pos * dpi, (native.pos * dpi).round());
        let pointer = native.pos + dvec2(400., 250.) / dpi;
        assert_eq!(window_point(native, app, pointer, false), Some(dvec2(200., 125.)));
        assert_eq!(window_point(rect, window, panel.pos, false), Some(pan));
        assert_eq!(picture_rect(panel, window, true, dvec2(-5., 1400.), 1.).pos, panel.pos - dvec2(0., 700.));
        assert_eq!(picture_rect(panel, window, true, dvec2(1500., -5.), 1.).pos, panel.pos - dvec2(1200., 0.));
        assert_eq!(picture_rect(panel, dvec2(200., 1000.), true, pan, 1.).pos, panel.pos + dvec2(100., -200.));
        assert_eq!(picture_rect(panel, dvec2(1600., 100.), true, pan, 1.).pos, panel.pos + dvec2(-300., 100.));
    }
}
