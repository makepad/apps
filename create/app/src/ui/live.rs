//! Live calls and shared apps in the sidebar and center.
use crate::call::{CallAction, CallShown, Phase, Press, ShareRole};
use crate::lists::{open_label, Live, Node, ViewData};
use crate::ui::town_tree::LiveRow;
use makepad_widgets::*;
use makepad_town_core::event::SessionId;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    let CallControls = View{width: Fill height: Fit flow: Right spacing: 5 align: Align{y: 0.5}}
    let CallControl = PanelButton{height: 26 padding: Inset{left: 8 right: 8}}
    let Name = RailText{max_lines: 1 text_overflow: Ellipsis}
    let Mark = PresenceDot{visible: false draw_bg.color: theme.color_success}
    mod.widgets.LiveCallPage = mod.widgets.LiveCallPageBase{
        width: Fill height: Fill flow: Down
        padding: Inset{left: 12 right: 12 top: 10 bottom: 10}
        content := View{
            visible: false width: Fill height: Fit flow: Down spacing: 4
            voice := View{width: Fill height: Fit flow: Down spacing: 4
                title := Name{width: Fill}
                people := CallControls{
                    SeatBadge{}
                    Name{text: "You"}
                    you_talking := Mark{a11y: {label: "You are talking"}}
                    peer_badge := PeerBadge{flow: Overlay
                        ring := CallVoiceRing{a11y: {hidden: true}}
                    }
                    peer_name := Name{width: Fill}
                    peer_talking := Mark{a11y: {label: "is talking"}}
                    duration := Kicker{}
                }
                controls := CallControls{flow: Right{wrap: true}
                    cancel := CallControl{text: "Cancel" a11y: {label: "Cancel the call"}}
                    call_answer := CallControl{text: "Answer"}
                    decline := CallControl{text: "Decline" a11y: {label: "Decline the call"}}
                    mute := CallControl{text: "Mute" a11y: {label: "Mute your microphone"}}
                    call_unmute := CallControl{text: "Unmute" a11y: {label: "Unmute your microphone"}}
                    hang_up := CallControl{text: "Hang up"}
                    call_share := CallControl{visible: false enabled: false text: "Share"}
                    transcribe_tip := Tip{call_transcribe := CallControl{text: "Transcribe"}}
                    call_stop_transcribing := CallControl{text: "Stop transcribing"}
                    peer_muted := Kicker{width: Fill max_lines: 1 text_overflow: Ellipsis}
                }
                devices := CallControls{flow: Right{wrap: true}
                    call_microphone := CallDevicePicker{glyph +: {draw_svg.svg: crate_resource("self://resources/icons/mic.svg")}}
                    call_speaker := CallDevicePicker{glyph +: {draw_svg.svg: crate_resource("self://resources/icons/speaker.svg")}}
                }
                transcription := BodyNote{visible: false width: Fill a11y: {live: @Polite}}
                consent := View{visible: false width: Fill height: Fit flow: Down
                    question := BodyNote{width: Fill a11y: {live: @Polite}}
                    CallControls{flow: Right{wrap: true}
                        call_transcribe_yes := CallControl{text: "Yes"}
                        call_not_me := CallControl{text: "Not me"}
                    }
                }
                notice := BodyNote{visible: false a11y: {live: @Polite}}
            }
            closed_notice := BodyNote{visible: false width: Fill a11y: {live: @Polite}}
            choices := ShareChoices{visible: false}
            share_problem := BodyNote{visible: false width: Fill}
        }
    }

    let Control = CallControl{visible: false}
    let Picture = SharePanel{visible: false width: Fill height: Fill
        draw_waiting +: {max_lines: 1 text_overflow: Ellipsis}
        controls: View{
            new_batch: true width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 2
            chip := RoundedView{
                width: Fit height: 22 flow: Right align: Align{y: 0.5} spacing: 4 padding: Inset{left: 6 right: 5}
                draw_bg +: {color: palette.accent border_radius: 4}
                PresenceDot{draw_bg.color: palette.onpeer a11y: {label: "Live stream"}}
                stream_name := RailText{draw_text.color: palette.onpeer width: Fit{max: 100} max_lines: 1 text_overflow: Ellipsis}
                controlling := RailText{visible: false text: "· controlling" draw_text.color: palette.onpeer}
                close_popout := PanelButton{width: 20 height: 20 padding: 0 text: "×" a11y: {label: "Close shared window"}}
            }
            request_control := PanelButton{height: 22 padding: Inset{left: 6 right: 6} text: "Ask for control"}
        }
    }
    let ShareView = #(LiveSharePage::register_widget(vm)){
        width: Fill height: Fill flow: Down spacing: 8
        padding: Inset{left: 12 right: 12 top: 10 bottom: 10}
    }
    mod.widgets.LiveSharePage = ShareView{
        status := ProjectHeading{width: Fill text_overflow: Ellipsis}
        close_ended := PanelButton{visible: false text: "Close"}
        embedded := View{width: Fill height: Fill flow: Down spacing: 8
            View{width: Fill height: Fit flow: Right{wrap: true} spacing: 5
                stop_share := Control{text: "Stop"}
                watch := Control{text: "Watch"}
                close_watch := Control{text: "Close"}
                share_tweak := Control{text: "Tweak"}
                share_pop_out := Control{text: "Pop out"}
                share_scale := Control{text: "1:1"}
                share_issue := Control{text: "Share visual issue"}
                let_control := Control{}
                take_back := Control{text: "Take back"}
            }
            share_rights := View{visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
                allow_all := Toggle{text: "Allow others to control"}
                chips := FlatList{width: Fill height: Fit flow: Right{wrap: true} spacing: 4
                    scroll_bars +: {show_scroll_y: false}
                    Chip := RoundedView{width: Fit height: 22 flow: Right align: Align{y: 0.5}
                        padding: Inset{left: 6} draw_bg +: {color: palette.panel2 border_radius: 4}
                        name := RailText{width: Fit{max: 120} max_lines: 1 text_overflow: Ellipsis}
                        remove := PanelButton{width: 22 height: 22 padding: 0 text: "×"}
                    }
                }
            }
            sharer_status := BodyNote{visible: false width: Fill a11y: {live: @Polite}}
            share_picture_panel := Picture{}
        }
    }
    mod.widgets.SharePopoutBody = ShareView{
        padding: 0 spacing: 0
        popout_picture := Picture{popout: true
            caption_lights: View{new_batch: true width: Fit height: Fit flow: Right
                close := DesktopButton{width: 20 height: 20 draw_bg.button_type: DesktopButtonType.MacClose a11y: {label: "Close window"}}
                min := DesktopButton{width: 20 height: 20 draw_bg.button_type: DesktopButtonType.MacMin a11y: {label: "Minimise window"}}
                max := DesktopButton{width: 20 height: 20 draw_bg.button_type: DesktopButtonType.MacMax a11y: {label: "Maximise or restore window"}}
            }
            caption_buttons: View{new_batch: true width: Fit height: Fit flow: Right
                min := DesktopButton{width: 46 height: 29 draw_bg.button_type: DesktopButtonType.WindowsMin a11y: {label: "Minimise window"}}
                max := DesktopButton{width: 46 height: 29 draw_bg.button_type: DesktopButtonType.WindowsMax a11y: {label: "Maximise window"}}
                restore := DesktopButton{visible: false width: 46 height: 29 draw_bg.button_type: DesktopButtonType.WindowsMaxToggled a11y: {label: "Restore window"}}
                close := DesktopButton{width: 46 height: 29 draw_bg.button_type: DesktopButtonType.WindowsClose a11y: {label: "Close window"}}
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct LiveSharePage {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] chips: Vec<WidgetRef>,
    #[rust] switch_key: Option<(KeyCode, SessionId)>,
}

impl LiveSharePage {
    fn bind_chips(&mut self, cx: &mut Cx, offer: Option<&crate::call::ShareOffer>) {
        if self.chips.is_empty() && offer.is_none_or(|o| o.allowed().is_empty()) { return }
        let list = self.view.flat_list(cx, ids!(chips));
        let Some(mut list) = list.borrow_mut() else { return };
        self.chips.clear();
        if let Some(offer) = offer {
            for (peer, name) in offer.allowed() {
                let id = LiveId::from_bytes(LiveId::SEED, peer, 0, peer.len(), 0);
                let Some(chip) = list.item(cx, id, id!(Chip)) else { continue };
                let label = chip.label(cx, ids!(name));
                let mut remove = chip.button(cx, ids!(remove));
                if label.text() != *name || remove.action_data().is_none() {
                    label.set_text(cx, name);
                    let accessible = cx.with_vm(|vm| vm.new_string(&format!("Revoke control for {name}")));
                    script_apply_eval!(cx, remove, {a11y_name: #(accessible)});
                }
                remove.set_action_data(CallAction::OnShare(offer.session, Press::RevokeControl(*peer)));
                self.chips.push(chip);
            }
        }
        list.items.retain_visible();
        list.redraw(cx);
    }
}

impl Widget for LiveSharePage {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if step.borrow::<FlatList>().is_none() { return Err(step) }
            for chip in &self.chips { chip.draw_all(cx, scope); }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.view.visible() { return }
        let change = button_events(&mut self.view, cx, event, scope);
        if let Some((uid, session, on)) = change {
            let switch = self.view.check_box(cx, ids!(allow_all));
            if uid != switch.widget_uid() { return }
            let current = scope.data.get::<ViewData>().and_then(|d| d.call.share.offer(session)).is_some_and(|o| o.allow_all());
            // The controller owns this value. A refused opening must leave the switch off.
            switch.set_active(cx, current, Animate::No);
            if current == on { return }
            if let Event::KeyDown(key) = event {
                if on {
                    // The Toggle changes on key down. The local consent gate runs on release.
                    self.switch_key = cx.injected_by().is_none().then_some((key.key_code, session));
                    return
                }
            }
            cx.widget_action(uid, CallAction::OnShare(session, Press::AllowAll(on)));
        } else if let Event::KeyUp(key) = event {
            if let Some((down, session)) = self.switch_key.take() {
                let switch = self.view.check_box(cx, ids!(allow_all));
                if down == key.key_code && cx.has_key_focus(switch.area()) {
                    cx.widget_action(switch.widget_uid(), CallAction::OnShare(session, Press::AllowAll(true)));
                }
            }
        } else if matches!(event, Event::KeyFocus(_) | Event::WindowLostFocus(_)) {
            self.switch_key = None;
        }
    }
}

impl ViewData {
    pub fn share_page(&self) -> Option<SessionId> {
        match self.page { crate::lists::Page::Live(Live::Share(s)) => Some(s), _ => None }
    }

    pub fn has_live(&self, live: Live) -> bool {
        match live {
            Live::Call => self.call.view.phase != Phase::Idle,
            Live::Share(session) => self.call.share.ended.iter().any(|e| e.session == session)
                || self.call.share.offer(session).is_some(),
        }
    }

    pub fn sync_live_rows(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let call = LiveRow::call(&self.call.view);
        let shares = self.call.share.offers.iter().map(|o|
            LiveRow::share(o.session, o.role, &self.call.view.peer_name, &o.title));
        let ended = self.call.share.ended.iter()
            .map(|e| LiveRow { item: Live::Share(e.session), label: e.notice.trim_end_matches('.').to_owned() });
        let rows: Vec<_> = call.into_iter().chain(shares).chain(ended).collect();
        if self.tree.live == rows { return }
        if self.tree.set_live(rows) {
            ui.widget(cx, ids!(tree)).redraw(cx);
        } else if let Some(start) = self.tree.live_start {
            let list = ui.portal_list(cx, ids!(tree.list));
            for (i, live) in self.tree.live.iter().enumerate() {
                if let Some((_, row)) = list.get_item(start + i) {
                    let open = row.button(cx, ids!(open));
                    if open.action_data().as_deref().and_then(|d| d.downcast_ref::<Node>()) == Some(&Node::Live(live.item)) {
                        open_label(cx, &row, ids!(name), &live.label);
                    }
                }
            }
        }
        self.show_compact_live(cx, ui);
    }

    pub(super) fn show_compact_live(&self, cx: &mut Cx, ui: &WidgetRef) {
        for (path, call) in [(ids!(compact_call), true), (ids!(compact_live), false)] {
            let button = ui.button(cx, path);
            let row = self.tree.live.iter().find(|r| (r.item == Live::Call) == call);
            button.set_visible(cx, self.narrow() && row.is_some());
            if let Some(row) = row.filter(|_| self.narrow()) {
                button.set_text(cx, &row.label);
                button.set_action_data(Node::Live(row.item));
            }
        }
    }

    pub fn watched_share(&self) -> Option<makepad_town_core::event::SessionId> {
        self.call.share.watching
    }

    pub fn share_in_window(&self) -> bool {
        self.share_popout.session().is_some_and(|s| Some(s) == self.watched_share())
    }

    /// The share's page picture and its pop-out window's picture: two views of one stream.
    pub fn share_panels(&self, cx: &Cx, ui: &WidgetRef) -> [WidgetRef; 2] {
        [ui.widget(cx, ids!(live_share.share_picture_panel)), ui.widget(cx, ids!(share_popout.popout_picture))]
    }

    pub fn redraw_share_panels(&self, cx: &mut Cx, ui: &WidgetRef) {
        for panel in self.share_panels(cx, ui) { panel.redraw(cx); }
    }

    pub fn sync_share_issue(&self, cx: &mut Cx, ui: &WidgetRef) {
        let share = &self.call.share;
        let visible = self.share_page().and_then(|s| share.offer(s)).is_some_and(|o|
            matches!(o.role, ShareRole::Sharer { .. }) || share.picture_for(o.session).is_some());
        let button = ui.button(cx, ids!(live_share.share_issue));
        button.set_visible(cx, visible);
        button.set_enabled(cx, self.workspace.writable && !self.tree.snapshot().projects.is_empty());
    }

    /// Bind the share surfaces on a call snapshot change. Picture-only updates redraw the panel.
    pub fn sync_live_share(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.sync_share_issue(cx, ui);
        let shown = &self.call;
        let session = self.share_page();
        let offer = session.and_then(|s| shown.share.offer(s));
        let row = offer.map(|o| LiveRow::share(o.session, o.role, &shown.view.peer_name, &o.title));
        let page = ui.widget(cx, ids!(live_share));
        let popout = ui.widget(cx, ids!(share_popout));
        // The pop-out is one more view of the watched picture; the page keeps showing it.
        let detached = self.share_in_window();
        let ended = shown.share.ended.iter().find(|e| Some(e.session) == session);
        popout.widget(cx, ids!(share_view)).set_visible(cx, detached);
        let viewer_title = |o: &crate::call::ShareOffer| format!("{} shared by {}", o.title, shown.view.peer_name);
        let title = offer.filter(|o| o.role == ShareRole::Viewer).map(|o| viewer_title(o));
        page.widget(cx, ids!(embedded)).set_visible(cx, ended.is_none());
        page.button(cx, ids!(close_ended)).set_visible(cx, ended.is_some());
        let status = page.label(cx, ids!(status));
        let stopped = offer.and_then(|o| o.stopped_line.as_deref()).filter(|_| title.is_some() && shown.share.watching != session);
        if let Some(mut label) = status.borrow_mut() { label.max_lines = usize::from(stopped.is_some()); }
        if let Some(ended) = ended {
            status.set_text(cx, &ended.notice);
        } else if let Some(row) = &row {
            let head = offer.and_then(|o| o.sharer_line.as_deref()).unwrap_or(&row.label);
            status.set_text(cx, stopped.unwrap_or(head));
        }
        show_controls(cx, &page, shown, session);
        let watching_here = session.is_some() && self.watched_share() == session;
        // A closing window is reopened once its close is acknowledged.
        let closing = matches!(self.share_popout, crate::lists::SharePopout::Closing(_));
        page.button(cx, ids!(share_tweak)).set_visible(cx, ended.is_none() && offer.is_some());
        page.button(cx, ids!(share_pop_out)).set_visible(cx, watching_here && !closing);
        let scale = page.button(cx, ids!(share_scale));
        scale.set_visible(cx, watching_here);
        let name = title.as_deref().unwrap_or("Shared app picture");
        let watched = shown.share.watched();
        if let Some(offer) = watched.filter(|_| detached) {
            ui.window(cx, ids!(share_popout)).set_title(cx, &offer.title);
        }
        let pop_name = watched.map(|o| viewer_title(o));
        let center = page.widget(cx, ids!(share_picture_panel));
        let was_visible = center.visible();
        let member = self.share_peer.and_then(|key| self.members.seats.iter()
            .find(|s| s.id == crate::view::MemberId::Computer(key)));
        let peer = member.map_or(shown.view.peer_name.as_str(), |s| s.name.as_str());
        let scale_label = shown.share.show_panel(cx, &center, session, true, name, peer, member.is_some());
        scale.set_text(cx, scale_label);
        shown.share.show_panel(cx, &popout.widget(cx, ids!(popout_picture)), self.watched_share(), detached, pop_name.as_deref().unwrap_or(name), peer, member.is_some());
        if center.visible() != was_visible { self.show_panes(cx, ui); }
    }
}

fn show_controls(cx: &mut Cx, page: &WidgetRef, shown: &CallShown, session: Option<SessionId>) {
    let share = &shown.share;
    let offer = session.and_then(|s| share.offer(s)).map(AsRef::as_ref);
    let role = offer.map(|o| o.role);
    let local = matches!(role, Some(ShareRole::Sharer { .. }));
    page.widget(cx, ids!(share_rights)).set_visible(cx, local);
    if let Some(mut live_page) = page.borrow_mut::<LiveSharePage>() { live_page.bind_chips(cx, offer); }
    if let Some(offer) = offer.filter(|_| local) {
        let switch = page.check_box(cx, ids!(allow_all));
        if switch.active(cx) != offer.allow_all() { switch.set_active(cx, offer.allow_all(), Animate::No); }
        switch.set_action_data(offer.session);
    }
    let viewer = role == Some(ShareRole::Viewer);
    share_button(cx, page, session, ids!(stop_share), local, Press::StopShare);
    share_button(cx, page, session, ids!(watch), viewer && share.watching != session, Press::Watch);
    share_button(cx, page, session, ids!(close_watch), viewer && share.watching == session, Press::CloseWatch);

    let active = share.control.active.is_some() && share.control_session == session;
    let requested = local && session.is_some() && share.control.requested == session;
    let can_grant = local && (shown.view.phase == Phase::Live || requested)
        && share.peer_watching == session && share.control.active.is_none();
    let grant = share_button(cx, page, session, ids!(let_control), can_grant, Press::LetControl);
    if can_grant {
        if requested { grant.set_text(cx, "Allow"); }
        else { grant.set_text(cx, &format!("Let {} control", shown.view.peer_name)); }
    }
    share_button(cx, page, session, ids!(take_back), local && active, Press::TakeBack);
    let label = page.label(cx, ids!(sharer_status));
    let notice = share.control.notice.as_deref().filter(|_| share.control_session == session);
    // Both seats keep how control ended on the page; the overlay only holds the button.
    label.set_visible(cx, (local && (requested || active)) || notice.is_some());
    match offer {
        Some(offer) if local && requested => label.set_text(cx, &format!("{} asks to control {}", shown.view.peer_name, offer.title)),
        Some(offer) if local && active => label.set_text(cx, &format!("{} is controlling {}", shown.view.peer_name, offer.title)),
        _ => if let Some(text) = notice { label.set_text(cx, text) },
    }
}

fn share_button(cx: &mut Cx, page: &WidgetRef, session: Option<SessionId>, path: &[LiveId], visible: bool, press: Press) -> ButtonRef {
    let button = visible_button(cx, page, path, visible);
    if let Some(session) = session.filter(|_| visible) {
        button.set_action_data(CallAction::OnShare(session, press));
    }
    button
}

pub(crate) fn button(cx: &mut Cx, parent: &WidgetRef, path: &[LiveId], visible: bool, press: Press) -> ButtonRef {
    let b = visible_button(cx, parent, path, visible);
    if b.action_data().is_none() { b.set_action_data(press); }
    b
}

fn visible_button(cx: &mut Cx, parent: &WidgetRef, path: &[LiveId], visible: bool) -> ButtonRef {
    let b = parent.button(cx, path);
    if b.visible() != visible {
        b.set_visible(cx, visible);
        b.set_enabled(cx, visible);
    }
    b
}

// Keep the button's own UID on the typed action. The app's local-input gate sees it
// in the same event that produced the click, including accessibility activation.
pub(crate) fn button_events(view: &mut View, cx: &mut Cx, event: &Event, scope: &mut Scope) -> Option<(WidgetUid, SessionId, bool)> {
    let mut switch = None;
    cx.mutate_actions(|cx| view.handle_event(cx, event, scope), |actions| {
        for action in actions {
            let Some(action) = action.downcast_mut::<WidgetAction>() else { continue };
            if let Some(CheckBoxAction::Change(on)) = action.action.downcast_ref::<CheckBoxAction>() {
                if let Some(session) = action.data.as_ref().and_then(|data| data.downcast_ref::<SessionId>()) {
                    switch = Some((action.widget_uid, *session, *on));
                }
            } else if matches!(action.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) {
                if let Some(scoped) = action.data.as_ref().and_then(|data| data.downcast_ref::<CallAction>()) {
                    action.action = Box::new(*scoped);
                } else if let Some(press) = action.data.as_ref().and_then(|data| data.downcast_ref::<Press>()) {
                    action.action = Box::new(CallAction::Press(*press));
                }
            }
        }
    });
    switch
}
