//! Route app-link events once so the share and other consumers see the same batch.
use crate::{App, TranscriptInput};
use makepad_create::{call::{ShareChoice, ShareRole}, lists::{Live, Node, Page, Pane, SharePopout}};
use makepad_create::{app_link::{AppLinkEvent, InputQuit}, call::{CallAction, Press, ShareControl, ShareEvent, SharePanelSize, ShareSurface}, consent};
use makepad_widgets::*;
use makepad_widgets::desktop_style::{self, DesktopStyle};

fn popout_chrome(os: &OsType, hosted: bool, fullscreen: bool, host: Option<DesktopStyle>) -> ResolvedChrome {
    let client = match os {
        OsType::Windows => true,
        OsType::LinuxWindow(p) => hosted || p.custom_window_chrome || fullscreen,
        OsType::LinuxDirect => hosted,
        _ => false,
    };
    if !client { return ResolvedChrome::None }
    match resolve_chrome(WindowChrome::Follow, false, host) {
        ResolvedChrome::Lights => ResolvedChrome::Lights,
        // A bare host gives a captionless window no controls of its own.
        _ => ResolvedChrome::Buttons,
    }
}

impl App {
    fn size_share_window(&mut self, cx: &mut Cx, window: &WindowRef, geometry_changed: bool) {
        let Some(picture) = self.view.call.share.picture.as_ref() else { return };
        let frame = (picture.width, picture.height);
        if !geometry_changed && self.popout_frame_size == Some(frame) { return }
        if crate::window_fit::fit_window_to_pixels(cx, window, dvec2(frame.0 as f64, frame.1 as f64), DVec2::default()) {
            self.popout_frame_size = Some(frame);
        }
    }

    pub(crate) fn share_window_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let window = self.ui.window(cx, ids!(share_popout));
        if let Some(op) = self.popout_window_op(cx, &window, actions) {
            match op {
                CxOsOp::CloseWindow(_) => self.close_watched_window(cx),
                _ => cx.push_unique_platform_op(op),
            }
        } else if self.view.open_ended_share.is_some_and(|s| self.view.share_page() == Some(s))
            && self.ui.button(cx, ids!(live_share.close_ended)).clicked(actions) {
            self.open_page(cx, Page::Home);
        } else if self.ui.button(cx, ids!(live_share.share_scale)).clicked(actions) {
            if self.view.watched_share().is_some_and(|s| self.view.page == Page::Live(Live::Share(s))) {
                let [panel, _] = self.view.share_panels(cx, &self.ui);
                self.view.call.share.toggle_panel_scale(cx, &panel);
                self.view.sync_live_share(cx, &self.ui);
            }
        } else if self.ui.button(cx, ids!(live_share.share_pop_out)).clicked(actions) {
            // The window is one more view of the watched picture; the page keeps showing it.
            let watched = self.view.watched_share().filter(|s| self.view.share_page() == Some(*s));
            match (self.view.share_popout, watched) {
                (SharePopout::Closed, Some(session)) => {
                    self.popout_frame_size = None;
                    self.view.share_popout = SharePopout::Open(session);
                    self.view.sync_live_share(cx, &self.ui);
                    window.show(cx);
                }
                (SharePopout::Open(open), Some(session)) if open == session => window.show(cx),
                (state, _) => log!("Pop out waits: the share window is {state:?}"),
            }
        }
    }

    pub(super) fn popout_window_op(&self, cx: &Cx, window: &WindowRef, actions: &Actions) -> Option<CxOsOp> {
        if !self.view.share_in_window() || !actions.iter().filter_map(|a| a.as_widget_action())
            .any(|w| matches!(w.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))) { return None }
        let id = window.window_id()?;
        let panel = window.widget(cx, ids!(popout_picture));
        let picture = panel.borrow::<makepad_create::call::SharePanel>()?;
        let buttons = picture.window_buttons()?;
        let clicked = |key| buttons.child(key).as_desktop_button().clicked(actions);
        if clicked(id!(min)) { return Some(CxOsOp::MinimizeWindow(id)) }
        if clicked(id!(close)) { return Some(CxOsOp::CloseWindow(id)) }
        let toggle = if buttons.child(id!(max)).visible() { id!(max) } else { id!(restore) };
        if clicked(toggle) {
            let expanded = window.is_maximized(cx) || window.is_fullscreen(cx) || cx.hosted_fullscreen(id);
            return Some(if expanded { CxOsOp::RestoreWindow(id) } else { CxOsOp::MaximizeWindow(id) })
        }
        None
    }

    fn close_watched_window(&mut self, cx: &mut Cx) {
        if self.open_watched_page(cx) { self.close_share_window(cx); }
    }

    // A failed document flush keeps both the current page and the watching window available.
    fn open_watched_page(&mut self, cx: &mut Cx) -> bool {
        let Some(session) = self.view.watched_share() else { return false };
        let share = Live::Share(session);
        if self.view.page != Page::Live(share) || (self.view.narrow() && self.view.pane != Pane::Page) {
            self.open_node(cx, Node::Live(share));
        }
        self.view.page == Page::Live(share) && (!self.view.narrow() || self.view.pane == Pane::Page)
    }

    /// Keep the sharer's preview at its existing size while filing an issue beside the stream.
    pub(crate) fn sync_share_preview(&mut self, cx: &mut Cx) {
        let session = self.view.share_page().filter(|_| !self.view.narrow() || self.view.pane == Pane::Page)
            .or_else(|| self.view.issue_share());
        if self.call.sync_share_preview(&mut self.app_links, session) {
            self.view.call.share = self.call.shown().share;
            self.ui.widget(cx, ids!(live_share.share_picture_panel)).redraw(cx);
        }
    }

    pub(crate) fn close_share_window(&mut self, cx: &mut Cx) {
        if let SharePopout::Open(session) = self.view.share_popout {
            self.view.share_popout = SharePopout::Closing(session);
            self.ui.window(cx, ids!(share_popout)).close(cx);
        }
    }

    pub(crate) fn share_window_event(&mut self, cx: &mut Cx, event: &Event) {
        let Some(session) = self.view.share_popout.session() else { return };
        let window = self.ui.window(cx, ids!(share_popout));
        if self.view.share_popout == SharePopout::Open(session) {
            if matches!(event, Event::Draw(_)) {
                let host = desktop_style::system_style(cx);
                let fullscreen = window.is_fullscreen(cx) || window.window_id().is_some_and(|id| cx.hosted_fullscreen(id));
                let chrome = popout_chrome(cx.os_type(), makepad_platform::hosted_by_window_manager(), fullscreen, host);
                let panel = window.widget(cx, ids!(popout_picture));
                if let Some(mut picture) = panel.borrow_mut::<makepad_create::call::SharePanel>() {
                    picture.set_window_controls(cx, chrome, fullscreen || window.is_maximized(cx));
                };
            }
            let changed = matches!(event, Event::WindowGeomChange(e) if window.window_id() == Some(e.window_id)
                && (e.old_geom.position != e.new_geom.position || e.old_geom.dpi_factor != e.new_geom.dpi_factor
                    || e.old_geom.is_fullscreen != e.new_geom.is_fullscreen || e.old_geom.is_maximized != e.new_geom.is_maximized));
            self.size_share_window(cx, &window, changed);
        }
        match event {
            Event::WindowLostFocus(id) => {
                let [page, popped] = self.view.share_panels(cx, &self.ui);
                if window.window_id() == Some(*id) { self.call.leave_share_panel(&popped); }
                else if self.ui.window(cx, ids!(main_window)).window_id() == Some(*id) { self.call.leave_share_panel(&page); }
            }
            Event::WindowCloseRequested(e) if window.window_id() == Some(e.window_id) && e.accept_close.get() => {
                if !self.closing_store && self.view.share_in_window() && !self.open_watched_page(cx) {
                    e.accept_close.set(false);
                }
            }
            Event::WindowClosed(e) if window.window_id() == Some(e.window_id) => {
                let [_, popped] = self.view.share_panels(cx, &self.ui);
                self.call.leave_share_panel(&popped);
                self.view.share_popout = SharePopout::Closed;
                if !self.closing_store && self.view.watched_share() == Some(session) {
                    self.open_watched_page(cx);
                }
                self.view.sync_live_share(cx, &self.ui);
                self.update_page(cx);
            }
            _ => {}
        }
    }

    /// The share owns the final answer after the Window has classified its ordinary caption.
    pub(crate) fn share_window_drag_query(&self, cx: &Cx, q: &WindowDragQueryEvent) {
        let SharePopout::Open(session) = self.view.share_popout else { return };
        let window = self.ui.window(cx, ids!(share_popout));
        if window.window_id() != Some(q.window_id) { return }
        let share = &self.view.call.share;
        // Presses belong to the picture while the guest controls the app or in select mode.
        let controlled = (share.control_session == Some(session) && share.control.active.is_some())
            || share.overlay.as_ref().is_some_and(|(_, s)| s.picking());
        let panel = window.widget(cx, ids!(popout_picture));
        let caption = !window.is_fullscreen(cx) && !cx.hosted_fullscreen(q.window_id)
            && panel.borrow::<makepad_create::call::SharePanel>().is_some_and(|p| p.caption_at(cx, q.abs, controlled));
        q.response.set(if caption { WindowDragQueryResponse::Caption } else { WindowDragQueryResponse::Client });
    }

    pub(crate) fn capture_test_call_release(&self, event: &Event) -> bool {
        self.model.is_some() && (self.view.page == makepad_create::lists::Page::Tasks || self.call.consent_home.verified())
            && TranscriptInput::from_event(event, false) == Some(TranscriptInput::Choose)
    }

    pub(crate) fn allow_consented_call_press(&self, press: CallAction, uid: WidgetUid, opening: &[WidgetUid],
        grant: impl FnOnce(bool) -> bool) -> bool {
        if matches!(press, CallAction::Press(Press::Watch | Press::LetControl(_) | Press::AllowAll(true))) { return false }
        if matches!(press, CallAction::Press(Press::Answer)) && self.view.page != Page::Live(Live::Call) { return false }
        if matches!(press, CallAction::OnShare(_, Press::Watch | Press::LetControl(_) | Press::AllowAll(true))
            | CallAction::Press(Press::Share(_) | Press::Answer | Press::Call | Press::CallSeat(_) | Press::JoinCall(..))) && !opening.contains(&uid) { return false }
        let opens_right = matches!(press, CallAction::OnShare(_, Press::LetControl(_) | Press::AllowAll(true)));
        if !self.call.opening_from_ui(press) || !grant(!opens_right) { return false }
        !matches!(press, CallAction::OnShare(session, _) if self.view.share_page() != Some(session))
    }

    /// Check provenance only for an opening control in this release's captured actions.
    pub(crate) fn consented_call_presses(&mut self, cx: &mut Cx, event: &Event, actions: &mut ActionsBuf, opening: &[WidgetUid]) {
        if TranscriptInput::from_event(event, false) != Some(TranscriptInput::Choose) { return }
        if self.ui.button(cx, ids!(capture.clip.chip.control)).clicked(actions) { self.capture_input(cx, Press::Clip); }
        if self.ui.button(cx, ids!(capture.snap.chip.control)).clicked(actions) { self.capture_input(cx, Press::Snap); }
        // Open the exact row on the original local release before asking the controller to watch.
        // Other row clicks remain queued for ordinary navigation.
        actions.retain(|action| {
            let Some(w) = action.as_widget_action() else { return true };
            let Some(Node::Live(Live::Share(session))) = Node::clicked(w) else { return true };
            if !opening.contains(&w.widget_uid) { return true }
            self.shell_actions(cx, std::slice::from_ref(action));
            if self.view.page == Page::Live(Live::Share(session)) && self.view.call.share.offer(session)
                .is_some_and(|o| matches!(o.role, ShareRole::Viewer { .. })) {
                self.call_opening_presses(cx, CallAction::OnShare(session, Press::Watch));
            }
            false
        });
        for action in actions {
            let Some(w) = action.as_widget_action() else { continue };
            let Some(press) = w.action.downcast_ref::<CallAction>() else { continue };
            if let CallAction::Press(p @ Press::Share(ShareChoice::Row(_))) = *press {
                if self.call.shown().share.capture.picking { self.capture_input(cx, p); continue; }
            }
            if !self.allow_consented_call_press(*press, w.widget_uid, opening, |allow_test_home| {
                consent::granted(cx, allow_test_home && self.call.consent_home.verified())
            }) { continue }
            match *press {
                CallAction::Press(Press::JoinCall(session, town, scope)) => {
                    if self.model.as_ref().is_some_and(|m| m.open_id() == town && m.call_join_scope(town, session) == Some(scope)) {
                        self.call_opening_presses(cx, *press);
                    }
                }
                CallAction::OnShare(session, Press::LetControl(peer)) => {
                    let Some(model) = &self.model else { continue };
                    let name = model.author_label(&peer, makepad_town_core::event::Actor::Human);
                    if self.call.allow_control(&mut self.app_links, session, peer, &name) { self.show_call(cx); }
                }
                CallAction::OnShare(session, Press::AllowAll(true)) => {
                    if self.call.allow_all_control(&mut self.app_links, session) { self.show_call(cx); }
                }
                _ => self.call_opening_presses(cx, *press),
            }
        }
    }

    pub(crate) fn publish_share_frame(&mut self, cx: &mut Cx) {
        if self.call.receive_control(&mut self.app_links) {
            self.show_call(cx);
        }
        let had_picture = self.view.call.share.picture.is_some();
        if self.call.publish_share_picture(&mut self.view.call.share) {
            if had_picture != self.view.call.share.picture.is_some() { self.view.sync_share_issue(cx, &self.ui); }
            self.view.redraw_share_panels(cx, &self.ui);
        }
    }

    pub fn app_links_quit(&mut self, cx: &mut Cx) {
        if self.recorder.prepare_close(&mut self.app_links, self.model.as_mut(), self.closing_store) {
            cx.new_next_frame();
        }
        let repeat_after_store = self.closing_store && self.model.as_ref().is_none_or(|m| m.store_stopped());
        match self.app_links.quit_input(repeat_after_store) {
            InputQuit::Ready => {}
            InputQuit::Wait => { let _ = cx.new_next_frame(); }
            InputQuit::Force { links_owing_release } => {
                log!("Second close: quitting with input release acknowledgements outstanding on {links_owing_release} app link(s)");
                cx.quit();
            }
        }
    }

    pub fn app_links_settling(&self) -> bool { self.app_links.input_settling() || self.recorder_settling() }

    pub fn take_share_events(&mut self) -> Vec<ShareEvent> { self.call.take_share_events() }

    pub(crate) fn share_panel_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for widget in actions.iter().filter_map(|a| a.as_widget_action()) {
            if let Some(size) = widget.action.downcast_ref::<SharePanelSize>() {
                let [center, popped] = self.view.share_panels(cx, &self.ui);
                let surface = if widget.widget_uid == center.widget_uid() && self.view.share_page() == Some(size.session) {
                    ShareSurface::Page
                } else if widget.widget_uid == popped.widget_uid() && self.view.share_in_window()
                    && self.view.watched_share() == Some(size.session) {
                    ShareSurface::Window
                } else { continue };
                self.call.share_panel_size(size.session, surface, size.width, size.height);
                self.sync_share_preview(cx);
            } else if let Some(control) = widget.action.downcast_ref::<ShareControl>() {
                let [page, popped] = self.view.share_panels(cx, &self.ui);
                let in_window = self.view.share_in_window() && widget.widget_uid == popped.widget_uid();
                let on_page = widget.widget_uid == page.widget_uid() && self.view.share_page() == self.view.watched_share();
                if !in_window && !on_page { continue }
                match *control {
                    ShareControl::Ask(session, asking) => {
                        if self.call.ask_control(session, asking) { self.show_call(cx); }
                    }
                    ShareControl::Release(session, grant) if self.view.call.share.control.active == Some(grant) => {
                        if self.call.call_actions(cx, CallAction::OnShare(session, Press::ReleaseControl)) { self.show_call(cx); }
                    }
                    ShareControl::Close(session) if in_window && self.view.watched_share() == Some(session) => {
                        self.close_watched_window(cx);
                    }
                    _ => {}
                }
            } else if let Some(input) = widget.action.downcast_ref::<makepad_create::call::control::PanelInput>() {
                // Either view of the watched picture can carry control; the grant check decides.
                let [center, popped] = self.view.share_panels(cx, &self.ui);
                if widget.widget_uid == center.widget_uid() || (widget.widget_uid == popped.widget_uid() && self.view.share_in_window()) {
                    self.call.panel_input(input);
                }
            }
        }
    }

    pub(crate) fn app_link_event(&mut self, cx: &mut Cx, event: &Event) -> Vec<AppLinkEvent> {
        if matches!(event, Event::Startup) {
            if let Some(model) = &self.model { self.call.consent_home = consent::TestHome::opened(&model.home.dir); }
        }
        if let Event::Actions(actions) = event {
            if let Some(press) = makepad_create::record::capture::recording_press(cx, &self.ui, actions) { self.capture_input(cx, press); }
        }
        let events = self.app_links.app_link_event(cx, event);
        if self.call.share_app_events(&mut self.app_links, &events) {
            self.show_capture(cx);
            self.sync_live_items(cx);
            self.ui.redraw(cx);
        }
        for press in self.call.take_record_presses() { self.record_press(press); }
        self.record_link_events(cx, &events);
        events
    }
}

#[cfg(test)]
#[test]
fn popout_controls_follow_the_desktop() {
    use ResolvedChrome::{Buttons, Lights};
    let linux = OsType::LinuxWindow(Default::default());
    for host in [None, Some(DesktopStyle::Macos), Some(DesktopStyle::Windows)] {
        assert_eq!(popout_chrome(&OsType::Macos, true, true, host), ResolvedChrome::None, "AppKit owns the traffic lights");
    }
    assert_eq!(popout_chrome(&linux, true, false, Some(DesktopStyle::Macos)), Lights);
    assert_eq!(popout_chrome(&linux, true, true, Some(DesktopStyle::Windows)), Buttons);
    assert_eq!(popout_chrome(&linux, true, false, Some(DesktopStyle::Omarchy)), Buttons, "a bare WM still has an exit");
    assert_eq!(popout_chrome(&OsType::Windows, false, false, None), Buttons);
    assert_eq!(popout_chrome(&linux, false, false, None), ResolvedChrome::None, "native server decorations supply the controls");
    assert_eq!(popout_chrome(&linux, false, true, None), Buttons, "fullscreen hides server decorations");
    let client = OsType::LinuxWindow(LinuxWindowParams { custom_window_chrome: true });
    assert_eq!(popout_chrome(&client, false, false, None), Buttons);
}

