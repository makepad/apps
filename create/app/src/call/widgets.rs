//! The live call controls and the call entries in chat.

use crate::lists::{paint_peer_badge, ActiveTownCall, SeatPalette, ViewData};
use crate::ui::live::{button, button_events};
use crate::view::{Seat, TownMembers};
use super::{TranscribeShown, CallShown, PickerShown, CallView, DeviceKind, DeviceNotice, DeviceRow, Devices, Phase, Press};
use makepad_widgets::{makepad_platform::audio::AudioDeviceId, tip::TipWidgetRefExt, *};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAction {
    Press(Press),
    OnShare(makepad_town_core::event::SessionId, Press),
    Pick(DeviceKind, AudioDeviceId),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Talking {
    pub you: bool,
    pub peer: bool,
}

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.LiveCallPageBase = #(CallRow::register_widget(vm))
    mod.widgets.ProjectHeaderBase = #(ProjectHeader::register_widget(vm))
    mod.widgets.CallButtonBase = #(CallButton::register_widget(vm))
    mod.widgets.JoinCallButtonBase = #(JoinCallButton::register_widget(vm))
    mod.widgets.CallEntryRowBase = #(CallEntryRow::register_widget(vm))
    mod.widgets.LiveDotBase = #(LiveDot::register_widget(vm))
    let PickerBase = #(DevicePicker::register_widget(vm))
    mod.widgets.CallVoiceRing = #(VoiceRing::register_widget(vm)){
        width: Fill height: Fill show_bg: true
        draw_bg +: {
            fraction: instance(0.0)
            ink: uniform(palette.onpeer)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                if self.fraction > 0.0 {
                    let c = self.rect_size * 0.5
                    sdf.arc_round_caps(c.x, c.y, min(c.x, c.y) - 1.5, -PI, -PI + 2.0 * PI * self.fraction, 2.0)
                    sdf.fill(self.ink)
                }
                return sdf.result
            }
        }
    }

    let Controls = View{width: Fill height: Fit flow: Right spacing: 5 align: Align{y: 0.5}}
    let Control = PanelButton{height: 26 padding: Inset{left: 8 right: 8}}
    let Name = RailText{max_lines: 1 text_overflow: Ellipsis}

    mod.widgets.CallDevicePicker = PickerBase{
        width: "calc(50% - 2.5px)" height: 26 flow: Right spacing: 4 align: Align{y: 0.5}
        padding: Inset{left: 6 right: 16}
        menu: DropDown{
            width: Fill height: Fill margin: 0 padding: 0
            popup_menu_position: #(drop_down::PopupMenuPosition::BelowInput)
            popup_menu: PopupMenu{
                width: Fit
                // The tick follows Devices::chosen even when no listed device is current.
                menu_item: PopupMenuItem{draw_bg +: {mark_color: #0000 mark_color_active: #0000 mark_color_disabled: #0000}}
            }
        }
        glyph := Svg{width: 14 height: 14 animating: false draw_svg.color: theme.color_label_inner a11y: {hidden: true}}
        name := Name{width: Fill draw_text.text_style: theme.font_regular{font_size: 9} a11y: {hidden: true}}
    }
    mod.widgets.ChatRowHead = View{
        width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
    }
    mod.widgets.ChatTime = Kicker{draw_text.text_style: theme.font_code{font_size: 7.5}}
    mod.widgets.CallEntryRow = mod.widgets.CallEntryRowBase{
        width: Fill height: Fit flow: Down spacing: 3 padding: Inset{bottom: 9}
        head := ChatRowHead{
            line := BodyNote{width: Fit}
            when := ChatTime{}
        }
        sent_transcript := BodyNote{width: Fill visible: false}
        controls := Controls{visible: false
            call_back := Control{text: "Call back"}
            transcript_toggle := Control{}
        }
        words := BodyNote{width: Fill visible: false}
    }
    let JoinTip = Tip{visible: false call_join := Control{visible: false text: "Join call"}}
    mod.widgets.CallButton = mod.widgets.CallButtonBase{
        width: Fit height: Fit
        content := View{
            visible: false width: Fit height: Fit flow: Right spacing: 4
            tip := Tip{call_start := Control{text: "Call"}}
            join_tip := JoinTip{}
        }
    }
    mod.widgets.JoinCallButton = mod.widgets.JoinCallButtonBase{
        width: Fit height: Fit
        content := View{visible: false width: Fit height: Fit
            join_tip := JoinTip{}
        }
    }
    mod.widgets.LiveDot = mod.widgets.LiveDotBase{
        width: 6 height: 6
        content := View{
            visible: false width: 6 height: 6
            PresenceDot{width: Fill height: Fill draw_bg +: {color: theme.color_error border_radius: 3}}
            a11y: {role: "status" label: "Your microphone is on"}
        }
    }
}

fn title(view: &CallView) -> String {
    let peer = &view.peer_name;
    match view.phase {
        Phase::Idle => String::new(),
        Phase::Calling => format!("Calling {peer}…"),
        Phase::Ringing => format!("{peer} is calling"),
        Phase::Live => format!("Call with {peer}"),
    }
}

fn duration(secs: u64) -> String {
    if secs < 3600 {
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    }
}

pub(crate) fn length(secs: u64) -> String {
    if secs < 60 { format!("{secs} s") } else { format!("{} min", secs / 60) }
}

type CallViewCache = Option<(std::sync::Arc<CallView>, bool, bool, bool, u64, u64)>;

fn view_changed(last: &mut CallViewCache, data: &ViewData) -> bool {
    let shown = &data.call;
    if last.as_ref().is_some_and(|(view, linked, can_call, can_share, members, active)|
        std::sync::Arc::ptr_eq(view, &shown.view) && *linked == shown.linked && *can_call == shown.can_call
            && *can_share == shown.can_share && *members == data.member_revision && *active == data.active_town_call_revision) {
        return false
    }
    *last = Some((shown.view.clone(), shown.linked, shown.can_call, shown.can_share,
        data.member_revision, data.active_town_call_revision));
    true
}

// The outer widget stays in the tree so it can see the next snapshot even when its
// content has no layout or input. Unscoped embeddings can still drive the Ref's show method.
macro_rules! call_view {
    ($name:ident $(, $scoped:ident)?) => {
        #[derive(Script, ScriptHook, Widget)]
        pub struct $name {
            #[source] source: ScriptObjectRef,
            #[deref] view: View,
            $(#[rust] $scoped: CallViewCache,)?
        }

        impl Widget for $name {
            fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
                $(if let Some(data) = scope.data.get::<ViewData>() {
                    if view_changed(&mut self.$scoped, data) { self.show(cx, data); }
                    if !self.view.child(id!(content)).visible() { return DrawStep::done() }
                })?
                self.view.draw_walk(cx, scope, walk)
            }
            fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
                $(if self.$scoped.is_some() && !self.view.child(id!(content)).visible() { return })?
                if self.view.visible() { button_events(&mut self.view, cx, event, scope); }
            }
        }
    };
}

call_view!(CallEntryRow);
call_view!(CallButton, last_view);
call_view!(JoinCallButton, last_view);
call_view!(ProjectHeader, last_view);
call_view!(LiveDot, last_view);

#[derive(Script, ScriptHook, Widget)]
pub struct CallRow {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] last_view: CallViewCache,
    #[rust] peer_chips: Vec<WidgetRef>,
}

impl Widget for CallRow {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(data) = scope.data.get::<ViewData>() {
            if view_changed(&mut self.last_view, data) { self.show(cx, data); }
            if !self.view.child(id!(content)).visible() { return DrawStep::done() }
        }
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if step.widget_uid() != self.view.widget(cx, ids!(peer_list)).widget_uid() { return Err(step) }
            for chip in &self.peer_chips { chip.draw_all(cx, scope); }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.last_view.is_some() && !self.view.child(id!(content)).visible() { return }
        if self.view.visible() { button_events(&mut self.view, cx, event, scope); }
    }
}

// The same UI-thread level is shared by both layouts. A meter tick never asks
// the app to rebuild its call snapshot or redraw the chat.
#[derive(Script, ScriptHook, Widget)]
struct VoiceRing {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] step: u8,
}

impl VoiceRing {
    fn sync_step(&mut self, cx: &Cx, scope: &mut Scope) -> bool {
        let step = scope.data.get::<crate::lists::ViewData>().map_or(0, |d| d.call.peer_level_step());
        if step == self.step { return false; }
        self.step = step;
        self.view.draw_bg.set_dyn_instance(cx, id!(fraction), &[f32::from(step) / f32::from(super::level::STEPS)]);
        true
    }
}

impl Widget for VoiceRing {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.sync_step(cx, scope);
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Timer(_)) && self.sync_step(cx, scope) {
            self.view.draw_bg.redraw(cx);
        }
    }
}

fn answer(cx: &mut Cx, parent: &WidgetRef, ringing: bool, name: &str) {
    let b = button(cx, parent, ids!(call_answer), ringing, Press::Answer);
    if ringing {
        accessible_button(cx, &b, &format!("Answer the call from {name}"));
    }
    button(cx, parent, ids!(decline), ringing, Press::Decline);
}

fn accessible_button(cx: &mut Cx, b: &ButtonRef, label: &str) {
    let Some(inner) = b.borrow() else { return };
    let mut node = a11y_tree::Node::default();
    inner.semantics(cx, &mut node);
    drop(inner);
    if node.name != label {
        let mut target = b.clone();
        let text = cx.with_vm(|vm| vm.new_string(label));
        script_apply_eval!(cx, target, {a11y_name: #(text)});
    }
}

/// Includes the project and call pages, both chats, instantiated transcript lines, and any person sheet.
/// Each UID belongs to the control that emits the action, including the microphone list.
pub fn opening_uids(cx: &Cx, root: &WidgetRef) -> Vec<WidgetUid> {
    // PortalList is a lookup scope. Name the list in ChatConversation to include its
    // instantiated CallLine rows while retaining the widget tree's cached lookups.
    let mut uids: Vec<_> = root.widgets(cx, &[ids!(call_start), ids!(call_join), ids!(call_back), ids!(call_answer), ids!(call_unmute),
        ids!(call_microphone), ids!(call_transcribe), ids!(call_transcribe_yes), ids!(transcript.list.call_back),
        ids!(compact_live), ids!(transcript_send), ids!(transcript_cancel), ids!(call_share), ids!(watch), ids!(choices), ids!(let_control)])
        .iter().map(|w| w.widget_uid()).collect();
    if let Some(tree) = root.widget(cx, ids!(tree)).borrow::<crate::ui::town_tree::TownTreeList>() {
        uids.extend_from_slice(tree.share_buttons());
    }
    uids
}

impl CallRowRef {
    /// `you` is my own name, whose letter my badge carries beside "You".
    pub fn show(&self, cx: &mut Cx, view: &CallView, devices: &Devices, talking: Talking, you: &str) {
        self.set_visible(cx, view.phase != Phase::Idle);
        CallRow::show_transcription(cx, &self.child(id!(content)), view, &TranscribeShown::Off, "");
        CallRow::show_content(cx, &self.child(id!(content)), view, talking,
            &PickerShown::read(devices, DeviceKind::Input), &PickerShown::read(devices, DeviceKind::Output), None, you);
        if let Some(mut row) = self.borrow_mut() { row.bind_peers(cx, view, None); }
    }
}

impl CallRow {
    fn show(&mut self, cx: &mut Cx, data: &ViewData) {
        let shown = &data.call;
        Self::show_transcription(cx, &self.view.child(id!(content)), &shown.view, &shown.transcribe, &shown.transcript_town);
        Self::show_content(cx, &self.view.child(id!(content)), &shown.view, shown.talking, &shown.input, &shown.output,
            shown.echo_cancel_notice.as_deref(), data.members.you_name());
        Self::show_share(cx, &self.view.child(id!(content)), shown);
        self.bind_peers(cx, &shown.view, Some(data));
    }

    fn bind_peers(&mut self, cx: &mut Cx, view: &CallView, data: Option<&ViewData>) {
        let multi = view.phase == Phase::Live && view.participants.len() >= 2;
        if !multi && self.peer_chips.is_empty() { return }
        let list = self.view.flat_list(cx, ids!(peer_list));
        let Some(mut list) = list.borrow_mut() else { return };
        let before = list.items.len();
        self.peer_chips.clear();
        if multi {
            let palette = data.map(|d| d.seat_palette).filter(|p| *p != SeatPalette::default())
                .unwrap_or_else(|| SeatPalette::from_cx(cx));
            let members = data.filter(|d| view.town.is_none_or(|t| d.tree.snapshot().town == Some(t))).map(|d| &d.members);
            for p in &view.participants {
                let id = LiveId::from_bytes(LiveId::SEED, &p.peer, 0, p.peer.len(), 0);
                let Some(chip) = list.item(cx, id, id!(PeerChip)) else { continue };
                let badge = chip.widget(cx, ids!(badge));
                badge.label(cx, ids!(initial)).set_text(cx, &crate::view::peer_initial(&p.name));
                paint_peer_badge(cx, &badge, members.and_then(|m| m.seat_tone_for_device(&p.peer)), &palette);
                chip.label(cx, ids!(name)).set_text(cx, &p.name);
                chip.widget(cx, ids!(muted)).set_visible(cx, p.muted);
                chip.widget(cx, ids!(transcribing)).set_visible(cx, p.transcribing);
                self.peer_chips.push(chip);
            }
        }
        list.items.retain_visible();
        if list.items.len() != before { cx.widget_tree_mark_dirty(list.widget_uid()); }
    }

    fn show_content(cx: &mut Cx, body: &WidgetRef, view: &CallView, talking: Talking, input: &PickerShown, output: &PickerShown,
        echo_cancel_notice: Option<&str>, you: &str) {
        body.widget(cx, ids!(voice)).set_visible(cx, view.phase != Phase::Idle);
        let live = view.phase == Phase::Live;
        let ringing = view.ringing();
        let calling = view.phase == Phase::Calling;
        body.set_visible(cx, view.phase != Phase::Idle);
        let mic = body.device_picker(cx, ids!(call_microphone));
        let speaker = body.device_picker(cx, ids!(call_speaker));
        let has_choices = |picker: &PickerShown| picker.rows.iter().filter(|row| !row.has_failed).nth(1).is_some();
        let show_mic = live && has_choices(input);
        let show_speaker = (live || ringing) && has_choices(output);
        mic.show(cx, DeviceKind::Input, show_mic, input);
        speaker.show(cx, DeviceKind::Output, show_speaker, output);
        body.widget(cx, ids!(devices)).set_visible(cx, show_mic || show_speaker);
        let multi = live && view.participants.len() >= 2;
        body.widget(cx, ids!(peer_roster)).set_visible(cx, multi);
        body.widget(cx, ids!(people_gap)).set_visible(cx, multi);
        if view.phase == Phase::Idle { return }

        let heading = body.label(cx, ids!(title));
        heading.set_visible(cx, !live);
        if !live { heading.set_text(cx, &title(view)); }
        body.widget(cx, ids!(people)).set_visible(cx, live);
        if live {
            body.label(cx, ids!(you_badge.initial)).set_text(cx, &crate::view::seat_initial(you, 'Y'));
            body.widget(cx, ids!(peer_badge)).set_visible(cx, !multi);
            body.widget(cx, ids!(peer_name)).set_visible(cx, !multi);
            if !multi {
                body.label(cx, ids!(peer_badge.initial)).set_text(cx, &crate::view::peer_initial(&view.peer_name));
                body.label(cx, ids!(peer_name)).set_text(cx, &view.peer_name);
            }
            body.widget(cx, ids!(you_talking)).set_visible(cx, talking.you && !view.muted && !view.unmuting);
            body.widget(cx, ids!(peer_talking)).set_visible(cx, !multi && talking.peer && !view.peer_muted);
            body.label(cx, ids!(duration)).set_text(cx, &duration(view.duration_secs));
        }
        button(cx, body, ids!(cancel), calling, Press::Cancel);
        answer(cx, body, ringing, &view.peer_name);
        button(cx, body, ids!(mute), live && !view.muted && !view.unmuting, Press::Mute);
        let unmute = button(cx, body, ids!(call_unmute), live && (view.muted || view.unmuting), Press::Unmute);
        unmute.set_text(cx, if view.unmuting { "Unmuting…" } else { "Unmute" });
        unmute.set_enabled(cx, live && view.muted && !view.unmuting);
        button(cx, body, ids!(hang_up), live, Press::HangUp);
        let peer_muted = body.label(cx, ids!(peer_muted));
        peer_muted.set_visible(cx, live && !multi && view.peer_muted);
        if live && !multi && view.peer_muted {
            peer_muted.set_text(cx, &format!("{} muted", view.peer_name));
        }

        let mut notices = String::new();
        for (picker, shown) in [(input, live), (output, live || ringing)] {
            if shown {
                if let Some(DeviceNotice::Unplugged { name, using }) = picker.notice.as_ref() {
                    if !notices.is_empty() { notices.push('\n'); }
                    notices.push_str(&format!("Using {using}: {name} was unplugged"));
                }
            }
        }
        if let Some(text) = echo_cancel_notice.filter(|_| live) {
            if !notices.is_empty() { notices.push('\n'); }
            notices.push_str(text);
        }
        let notice = body.label(cx, ids!(notice));
        notice.set_visible(cx, !notices.is_empty());
        notice.set_text(cx, &notices);
    }
}

impl CallRow {
    fn show_transcription(cx: &mut Cx, body: &WidgetRef, view: &CallView, status: &TranscribeShown, town: &str) {
        let live = view.phase == Phase::Live;
        let start = button(cx, body, ids!(call_transcribe), live && !view.transcribing_me, Press::Transcribe);
        let available = !matches!(status, TranscribeShown::Unavailable(_));
        start.set_enabled(cx, live && !view.transcribing_me && available);
        let tip = body.tip(cx, ids!(transcribe_tip));
        tip.set_visible(cx, live && !view.transcribing_me);
        match status {
            TranscribeShown::Unavailable(reason) if live => tip.set_text(reason),
            TranscribeShown::Off if live && !town.is_empty() => {
                tip.set_text(&format!("Everyone in {town} can read your words after the call."));
            }
            _ => tip.set_text(""),
        }
        button(cx, body, ids!(call_stop_transcribing), live && view.transcribing_me, Press::StopTranscribing);
        let prompt = live && view.transcribe_prompt;
        body.widget(cx, ids!(consent)).set_visible(cx, prompt);
        let yes = button(cx, body, ids!(call_transcribe_yes), prompt, Press::Transcribe);
        yes.set_enabled(cx, prompt && available);
        button(cx, body, ids!(call_not_me), prompt, Press::NotMe);
        let mut lines = Vec::new();
        if live {
            match status {
                TranscribeShown::Starting => lines.push(format!("Your words go into {town}'s chat · Starting transcription…")),
                TranscribeShown::On(engine) => lines.push(format!("Your words go into {town}'s chat · {engine}")),
                TranscribeShown::Unavailable(reason) => lines.push(reason.clone()),
                TranscribeShown::Off => {}
            }
            let names: Vec<_> = view.participants.iter().filter(|p| p.transcribing).map(|p| p.name.as_str()).collect();
            let transcribing = match names.as_slice() {
                [] if view.participants.is_empty() && (view.peer_transcribing || prompt) => Some(format!("{} is transcribing", view.peer_name)),
                [] => None,
                [one] => Some(format!("{one} is transcribing")),
                _ => Some(format!("{} are transcribing", names.join(", "))),
            };
            if prompt {
                let question = transcribing.map_or_else(|| "Transcribe you too?".into(), |line| format!("{line}. Transcribe you too?"));
                body.label(cx, ids!(question)).set_text(cx, &question);
            } else if let Some(line) = transcribing {
                lines.push(line);
            }
        }
        let label = body.label(cx, ids!(transcription));
        label.set_visible(cx, !lines.is_empty());
        label.set_text(cx, &lines.join("\n"));
    }
}

impl CallRow {
    fn show_share(cx: &mut Cx, body: &WidgetRef, shown: &CallShown) {
        let share = &shown.share;
        let voice = shown.view.phase != Phase::Idle;
        let share_picker = share.choices.is_some() && !share.capture.picking;
        body.set_visible(cx, voice || share_picker || share.problem.is_some() || share.closed_notice.is_some());
        let menu = show_share_controls(cx, body, shown, shown.view.phase == Phase::Live);
        menu.set_enabled(cx, menu.visible() && shown.can_share);
        let closed = body.label(cx, ids!(closed_notice));
        closed.set_visible(cx, share.closed_notice.is_some());
        if let Some(notice) = &share.closed_notice { closed.set_text(cx, notice); }
    }
}

impl ProjectHeader {
    fn show(&self, cx: &mut Cx, data: &ViewData) {
        let share = show_share_controls(cx, &self.view.child(id!(content)), &data.call, true);
        share.set_enabled(cx, share.visible() && data.call.can_share);
        if share.visible() {
            if data.call.can_share {
                accessible_button(cx, &share, "Share");
            } else {
                let reason = disconnected_label(sole_peer(&data.members));
                accessible_button(cx, &share, &format!("Share: {reason}"));
            }
        }
    }
}

fn show_share_controls(cx: &mut Cx, body: &WidgetRef, shown: &CallShown, available: bool) -> ButtonRef {
    let share = &shown.share;
    let menu = button(cx, body, ids!(call_share), available, Press::Share(super::ShareChoice::Menu));
    let problem = body.label(cx, ids!(share_problem));
    problem.set_visible(cx, share.problem.is_some());
    if let Some(why) = &share.problem { problem.set_text(cx, why); }
    body.widget(cx, ids!(choices)).set_visible(cx, share.choices.is_some() && !share.capture.picking);
    menu
}

fn sole_peer(members: &TownMembers) -> Option<&str> {
    let mut peers = members.seats.iter().filter(|member| member.seat == Seat::Peer);
    let peer = peers.next()?;
    peers.next().is_none().then_some(peer.name.as_str())
}

fn disconnected_label(peer: Option<&str>) -> String {
    match peer {
        Some(name) => format!("{name} isn't connected yet"),
        None => "No one else is connected yet".into(),
    }
}

impl CallButtonRef {
    pub fn show(&self, cx: &mut Cx, peer_name: &str, linked: bool, call_on: bool) {
        self.set_visible(cx, !call_on);
        let name = (linked || !peer_name.is_empty()).then_some(peer_name);
        let body = self.child(id!(content));
        show_join_button(cx, &body, None);
        CallButton::show_content(cx, &body, name, linked, !call_on);
    }
}

fn active_call(data: &ViewData) -> Option<&ActiveTownCall> {
    data.active_town_call.as_ref().filter(|c| data.call.view.phase == Phase::Idle
        && data.tree.snapshot().town.is_none_or(|t| t == c.town))
}

fn show_join_button(cx: &mut Cx, body: &WidgetRef, active: Option<&ActiveTownCall>) {
    let join = body.button(cx, ids!(call_join));
    let tip = body.tip(cx, ids!(join_tip));
    join.set_visible(cx, active.is_some());
    tip.set_visible(cx, active.is_some());
    join.set_enabled(cx, active.is_some_and(|c| c.can_join));
    let Some(c) = active else {
        join.set_action_data(());
        tip.set_text("");
        return
    };
    join.set_action_data(CallAction::Press(Press::JoinCall(c.session, c.town, c.scope)));
    join.set_text(cx, if c.can_join { "Join call" } else { "Call in progress" });
    let label = if !c.can_join { "No one in the call is connected yet".into() }
        else if c.participants.is_empty() { "Join call".into() }
        else { format!("Join call with {}", c.participants.join(", ")) };
    tip.set_text(&label);
    accessible_button(cx, &join, &label);
}

impl JoinCallButton {
    fn show(&self, cx: &mut Cx, data: &ViewData) {
        let body = self.view.child(id!(content));
        let active = active_call(data);
        body.set_visible(cx, active.is_some());
        show_join_button(cx, &body, active);
    }
}

impl CallButton {
    fn show(&self, cx: &mut Cx, data: &ViewData) {
        let shown = &data.call;
        let name = if shown.can_call {
            (!shown.view.peer_name.is_empty()).then_some(shown.view.peer_name.as_str())
        } else {
            sole_peer(&data.members)
        };
        let body = self.view.child(id!(content));
        let active = active_call(data);
        Self::show_content(cx, &body, name, shown.can_call, shown.view.phase == Phase::Idle && active.is_none());
        if active.is_some() { body.set_visible(cx, true); }
        show_join_button(cx, &body, active);
    }

    fn show_content(cx: &mut Cx, body: &WidgetRef, peer_name: Option<&str>, linked: bool, visible: bool) {
        body.set_visible(cx, visible);
        let b = button(cx, body, ids!(call_start), visible, Press::Call);
        b.set_enabled(cx, visible && linked);
        let tip = body.tip(cx, ids!(tip));
        tip.set_visible(cx, visible);
        if visible {
            let label = if linked { format!("Call {}", peer_name.unwrap_or("The other person")) } else { disconnected_label(peer_name) };
            tip.set_text(&label);
            b.set_text(cx, &label);
            accessible_button(cx, &b, &label);
        } else {
            tip.set_text("");
        }
    }
}

impl CallEntryRowRef {
    pub fn show(&self, cx: &mut Cx, call: &crate::view::CallLine) {
        self.label(cx, ids!(line)).set_text(cx, &call.words);
        self.label(cx, ids!(when)).set_text(cx, &call.time);
        let sent = self.label(cx, ids!(sent_transcript));
        sent.set_visible(cx, !call.sent.is_empty());
        if !call.sent.is_empty() { sent.set_text(cx, &call.sent); }
        self.widget(cx, ids!(controls)).set_visible(cx, call.call_back.is_some() || call.count > 0);
        let back = self.button(cx, ids!(call_back));
        back.set_visible(cx, call.call_back.is_some());
        if let Some(callback) = &call.call_back {
            back.set_action_data(CallAction::Press(Press::CallSeat(callback.seat)));
            back.set_enabled(cx, callback.enabled);
            accessible_button(cx, &back, &format!("Call {} back", callback.peer));
        } else {
            back.set_enabled(cx, false);
            back.set_action_data(());
        }
        let toggle = self.button(cx, ids!(transcript_toggle));
        toggle.set_visible(cx, call.count > 0);
        if call.count > 0 {
            toggle.set_text(cx, &format!("Transcript ({})", call.count));
            toggle.set_action_data(call.id);
        }
        let body = self.label(cx, ids!(words));
        body.set_visible(cx, call.transcript.is_some());
        if let Some(text) = &call.transcript { body.set_text(cx, text); }
    }
}

impl LiveDot {
    fn show(&self, cx: &mut Cx, data: &ViewData) {
        self.view.child(id!(content)).set_visible(cx, data.call.mic_open);
    }
}

impl LiveDotRef {
    pub fn show(&self, cx: &mut Cx, mic_open: bool) {
        self.set_visible(cx, mic_open);
        self.child(id!(content)).set_visible(cx, mic_open);
    }
}

// The stock dropdown owns pointer capture, focus, keyboard navigation and the popup.
// The picker supplies a compact face and one accessible combobox with a device-specific name.
#[derive(Script, ScriptHook, Widget)]
struct DevicePicker {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[live] menu: WidgetRef,
    #[rust] kind: Option<DeviceKind>,
    #[rust] rows: Vec<DeviceRow>,
    #[rust] menu_dirty: bool,
    // Keep the IDs under the last drawn popup. A device event can precede a release
    // without a repaint between them.
    #[rust] displayed: Vec<Option<AudioDeviceId>>,
    #[rust] label: String,
}

fn menu_label(row: &DeviceRow) -> String {
    let mut label = String::new();
    if row.current { label.push_str("✓ "); }
    label.push_str(&row.name);
    if row.is_default { label.push_str(" · system default"); }
    if row.has_failed { label.push_str(" · unavailable"); }
    label
}

impl DevicePickerRef {
    fn show(&self, cx: &mut Cx, kind: DeviceKind, shown: bool, devices: &PickerShown) {
        let Some(mut picker) = self.borrow_mut() else { return };
        picker.set_visible(cx, shown);
        let menu = picker.menu.as_drop_down();
        menu.set_visible(cx, shown); // releases capture if this phase hid an open list
        if !shown { return }
        let rows = devices.rows.as_slice();
        let label = devices.label.as_str();
        if picker.kind == Some(kind) && picker.label == label && picker.rows == rows { return }

        picker.kind = Some(kind);
        picker.label = label.into();
        picker.view.label(cx, ids!(name)).set_text(cx, label);
        rows.clone_into(&mut picker.rows);
        picker.menu_dirty = true;
        picker.redraw(cx);
    }
}

impl DevicePicker {
    fn update_menu(&mut self, cx: &mut Cx) {
        if !std::mem::take(&mut self.menu_dirty) { return }
        let menu = self.menu.as_drop_down();
        menu.set_labels(cx, self.rows.iter().map(menu_label).collect());
        menu.set_disabled_items(cx, self.rows.iter().map(|row| row.has_failed).collect());
        menu.set_face_labels(cx, vec![String::new(); self.rows.len()]);
        menu.set_selected_item(cx, self.rows.iter().position(|row| row.current).unwrap_or(0));
        self.displayed.clear();
        self.displayed.extend(self.rows.iter().map(|row| (!row.has_failed).then_some(row.id)));
    }

    fn choice(&self, index: usize) -> Option<CallAction> {
        let id = self.displayed.get(index).copied().flatten()?;
        if self.rows.iter().any(|row| row.id == id && !row.has_failed) {
            Some(CallAction::Pick(self.kind?, id))
        } else {
            None
        }
    }

    fn dispatch<R>(&mut self, cx: &mut Cx, f: impl FnOnce(&mut DropDown, &mut Cx) -> R) -> Option<R> {
        let mut menu = self.menu.borrow_mut::<DropDown>()?;
        let uid = self.widget_uid();
        Some(cx.map_actions(|cx| f(&mut menu, cx), |_, mut actions| {
            actions.retain_mut(|action| {
                let Some(action) = action.downcast_mut::<WidgetAction>() else { return true };
                if let Some(drop_down::DropDownAction::Select(index)) = action.action.downcast_ref() {
                    let Some(pick) = self.choice(*index) else { return false };
                    action.widget_uid = uid;
                    action.action = Box::new(pick);
                }
                true
            });
            actions
        }))
    }
}

impl Widget for DevicePicker {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible() { return DrawStep::done() }
        self.update_menu(cx);
        cx.begin_turtle(walk, Layout::flow_overlay());
        if let Some(mut menu) = self.menu.borrow_mut::<DropDown>() {
            menu.draw_walk(cx, Walk::fill());
        }
        // After padding and the 14-point glyph, leave room for at least an ellipsis and a letter.
        let width = cx.turtle().rect().size.x;
        self.view.widget(cx, ids!(name)).set_visible(cx, width >= 6. + 14. + 4. + 16. + 18.);
        self.view.draw_walk_all(cx, scope, Walk::fill());
        cx.end_turtle();
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.visible() { return }
        if let Event::A11yAction(req) = event {
            // Popup rows have their own accessibility nodes. A row press selects its
            // stable device ID even if two devices have the same label.
            if req.action == a11y_tree::Actions::PRESS {
                let mut index = None;
                if let Some(menu) = self.menu.borrow::<DropDown>() {
                    if menu.owns_the_menu(cx) {
                        menu.children(&mut |id, row| {
                            if row.widget_uid().0 == req.node.uid() { index = Some(id.0 as usize); }
                        });
                    }
                }
                if let Some(index) = index {
                    let result = if let Some(pick) = self.choice(index) {
                        cx.widget_action(self.widget_uid(), pick);
                        if let Some(mut menu) = self.menu.borrow_mut::<DropDown>() { menu.set_closed(cx); }
                        a11y_tree::ActionResult::Handled
                    } else {
                        a11y_tree::ActionResult::Disabled
                    };
                    cx.a11y.record_action(req.req, result);
                    return;
                }
            }
        }
        self.dispatch(cx, |menu, cx| menu.handle_event(cx, event, scope));
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if let Some(menu) = self.menu.borrow::<DropDown>() { menu.semantics_parts(cx, out); }
    }

    fn semantics(&self, cx: &Cx, node: &mut a11y_tree::Node) {
        if let Some(menu) = self.menu.borrow::<DropDown>() { menu.semantics(cx, node); }
        let device = if self.kind == Some(DeviceKind::Input) { "Microphone" } else { "Speaker" };
        node.name = format!("{device}: {}", self.label);
        node.value.clone_from(&self.label);
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope)
        -> a11y_tree::ActionResult {
        if !self.visible() { return a11y_tree::ActionResult::Disabled }
        self.dispatch(cx, |menu, cx| menu.handle_action(cx, request, scope))
            .unwrap_or(a11y_tree::ActionResult::Unhandled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_platform::audio::{AudioDeviceDesc, AudioDeviceType};
    use makepad_script::script;

    #[test]
    fn call_copy_and_duration() {
        for (s, text) in [(0, "0:00"), (59, "0:59"), (60, "1:00"), (3599, "59:59"),
                          (3600, "1:00:00"), (3723, "1:02:03")] {
            assert_eq!(duration(s), text);
        }
        let mut view = CallView::default();
        view.peer_name = "Anna".into();
        for (phase, text) in [(Phase::Idle, ""), (Phase::Calling, "Calling Anna…"),
            (Phase::Ringing, "Anna is calling"), (Phase::Live, "Call with Anna")] {
            view.phase = phase;
            assert_eq!(title(&view), text);
        }
    }

    fn device(n: u64, device_type: AudioDeviceType, name: &str, is_default: bool) -> AudioDeviceDesc {
        AudioDeviceDesc { device_id: AudioDeviceId(LiveId(n)), device_type, name: name.into(),
            is_default, has_failed: false, channel_count: 1 }
    }

    #[test]
    fn multiparty_call_roster_shows_each_peer_and_join_call_switches_button() {
        use crate::call::{CallScope, ParticipantShown};
        use crate::view::{MemberId, MemberSeat, Presence, SeatDevice, SeatTone};
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let ui = cx.with_vm(|vm| {
            let val = script_eval!(vm, { use mod.widgets.* View{flow: Down
                header := CallButton{} home := JoinCallButton{} live_call := LiveCallPage{}
            } });
            WidgetRef::script_from_value(vm, val)
        });
        let mut data = ViewData::default();
        data.seat_palette = SeatPalette::from_cx(cx);
        let view = std::sync::Arc::make_mut(&mut data.call.view);
        view.phase = Phase::Live;
        view.transcribe_prompt = true;
        data.members.seats.push(MemberSeat { id: MemberId::You, seat: Seat::You, name: "okp".into(), tone: None,
            presence: Presence::Here, status: String::new(), live: None, devices: Vec::new() });
        for (i, name) in ["Ana", "Ben", "Kit"].into_iter().enumerate() {
            let key = [i as u8 + 1; 65];
            view.participants.push(ParticipantShown { peer: key, name: name.into(), muted: i == 1, transcribing: i == 2 });
            data.members.seats.push(MemberSeat { id: MemberId::Seat([i as u8; 32]), seat: Seat::Peer, name: name.into(),
                tone: Some(SeatTone(i as u8)), presence: Presence::Away, status: String::new(), live: None,
                devices: vec![SeatDevice { key, name: name.into(), presence: Presence::Away, status: String::new(), is_me: false }] });
        }
        let size = dvec2(279., 480.);
        let pass = DrawPass::new(cx);
        pass.set_size(cx, size);
        let mut list = DrawList2d::new(cx);
        let mut draw = |cx: &mut Cx, data: &mut ViewData| {
            let event = DrawEvent::default();
            let mut draw = cx_draw::CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            ui.draw_all(&mut frame, &mut Scope::with_data(data));
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
        };
        draw(cx, &mut data);
        let page = ui.call_row(cx, ids!(live_call));
        assert_eq!(page.label(cx, ids!(you_badge.initial)).text(), "O", "my call badge beside You takes my name's letter");
        assert!(page.widget(cx, ids!(peer_roster)).visible());
        assert_eq!(page.label(cx, ids!(question)).text(), "Kit is transcribing. Transcribe you too?");
        {
            let row = page.borrow().unwrap();
            assert_eq!(row.peer_chips.len(), 3);
            for (i, chip) in row.peer_chips.iter().enumerate() {
                assert_eq!(chip.label(cx, ids!(name)).text(), ["Ana", "Ben", "Kit"][i]);
                assert_eq!(chip.widget(cx, ids!(muted)).visible(), i == 1);
                assert_eq!(chip.widget(cx, ids!(transcribing)).visible(), i == 2);
                assert_eq!(crate::lists::badge_colors(cx, &chip.widget(cx, ids!(badge))).0, data.seat_palette.seats[i]);
                let rect = chip.area().rect(cx);
                assert!(rect.size.x > 0. && rect.pos.x >= 0. && rect.pos.x + rect.size.x <= size.x + 0.01, "chip {i}: {rect:?}");
            }
        }
        data.call.view = Default::default();
        let mut active = ActiveTownCall { session: [6; 16], town: [7; 32], scope: CallScope::Town,
            participants: vec!["Ana".into(), "Ben".into()], can_join: true };
        let buttons = [ids!(header.call_join), ids!(home.call_join)].map(|p| ui.widget(cx, p));
        for can_join in [true, false, true] {
            active.can_join = can_join;
            data.set_active_town_call(Some(active.clone()));
            draw(cx, &mut data);
            assert!(!ui.widget(cx, ids!(header.call_start)).visible());
            for button in &buttons {
                assert!(button.visible());
                assert_eq!(button.disabled(cx), !can_join);
                assert_eq!(button.text(), if can_join { "Join call" } else { "Call in progress" });
                let mut node = a11y_tree::Node::default();
                button.borrow::<Button>().unwrap().semantics(cx, &mut node);
                assert_eq!(node.name, if can_join { "Join call with Ana, Ben" } else { "No one in the call is connected yet" });
                if can_join {
                    assert_eq!(activate(cx, &ui, button, a11y_tree::Actions::PRESS, a11y_tree::ActionArg::None),
                        [(button.widget_uid(), CallAction::Press(Press::JoinCall(active.session, active.town, active.scope)))]);
                }
            }
            active.session[0] += 1;
        }
        data.set_active_town_call(None);
        draw(cx, &mut data);
        for b in buttons {
            assert!(!b.visible() && b.action_data().as_ref().is_some_and(|a| a.is::<()>()), "a hidden Join forgets the session");
        }
        assert!(ui.widget(cx, ids!(header.call_start)).visible());
    }

    fn activate(cx: &mut Cx, ui: &WidgetRef, target: &WidgetRef, action: a11y_tree::Actions, arg: a11y_tree::ActionArg)
        -> Vec<(WidgetUid, CallAction)> {
        let event = Event::A11yAction(a11y_tree::ActionRequest {
            req: 0, node: a11y_tree::NodeId::widget(target.widget_uid().0), action, arg,
        });
        cx.capture_actions(|cx| ui.handle_event(cx, &event, &mut Scope::empty())).iter().filter_map(|action| {
            let a = action.as_widget_action()?;
            Some((a.widget_uid, *a.action.downcast_ref::<CallAction>()?))
        }).collect()
    }

    fn menu_row(menu: &DropDownRef, index: u64) -> WidgetRef {
        let mut found = None;
        menu.children(&mut |id, w| { if id.0 == index { found = Some(w); } });
        found.expect("row in the open menu")
    }

    fn click_row(cx: &mut Cx, ui: &WidgetRef, row: &WidgetRef) -> Vec<(WidgetUid, CallAction)> {
        let rect = row.area().clipped_rect(cx);
        assert!(rect.size.x > 0. && rect.size.y > 0., "popup row has a hit target");
        let abs = rect.pos + rect.size * 0.5;
        let window_id = WindowId(1, 1);
        let mut found = Vec::new();
        for event in [
            Event::MouseDown(MouseDownEvent { abs, window_id, button: MouseButton::PRIMARY,
                modifiers: Default::default(), handled: Default::default(), time: 1.0 }),
            Event::MouseUp(MouseUpEvent { abs, window_id, button: MouseButton::PRIMARY,
                modifiers: Default::default(), time: 1.1 }),
        ] {
            for action in cx.capture_actions(|cx| ui.handle_event(cx, &event, &mut Scope::empty())) {
                if let Some(w) = action.as_widget_action() {
                    if let Some(call) = w.action.downcast_ref::<CallAction>() { found.push((w.widget_uid, *call)); }
                }
            }
        }
        found
    }

    #[test]
    fn pressing_call_dismisses_its_tooltip_over_the_live_row() {
        use crate::call::{CallFlow, Message, Now};
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let ui = cx.with_vm(|vm| {
            let val = vm.eval(script! {
                use mod.prelude.workspace.*
                use mod.widgets.*
                View{width: Fill height: Fill flow: Down
                    header := CallButton{} live_call := LiveCallPage{} tips := TipLayer{}
                }
            });
            WidgetRef::script_from_value(vm, val)
        });
        let header = ui.call_button(cx, ids!(header));
        let row = ui.call_row(cx, ids!(live_call));
        let tip = header.tip(cx, ids!(tip));
        tip.borrow_mut().unwrap().delay_secs = 0.; // Exercise the real overlay without a wall-clock dwell.
        let layer = ui.widget(cx, ids!(tips));
        let shown_tip = |cx: &Cx| {
            let mut node = a11y_tree::Node::default();
            layer.borrow::<tip::TipLayer>().unwrap().semantics(cx, &mut node);
            node.name
        };
        let mut flow = CallFlow::default();
        let now = Now::default();
        let mut effects = Vec::new();
        flow.set_open_town(Some(([9; 32], vec![([1; 65], "Anna".into())])), &mut effects);
        header.show(cx, &flow.view().peer_name, true, false);
        let mut target = ui_checks::UiCheckTarget::new(cx);
        let size = dvec2(279., 600.);
        target.draw(cx, &ui, size);
        let rect = header.widget(cx, ids!(call_start)).area().clipped_rect(cx);
        assert!(rect.size.x > 0. && rect.size.y > 0.);
        let abs = rect.pos + rect.size * 0.5;
        let window_id = WindowId(1, 1);
        let hover = || Event::MouseMove(MouseMoveEvent {
            abs, window_id, lock_delta: Default::default(), modifiers: Default::default(), handled: Default::default(), time: 1.,
        });
        let dispatch = |cx: &mut Cx, event: Event| {
            let actions = cx.capture_actions(|cx| ui.handle_event(cx, &event, &mut Scope::empty()));
            let mut press = None;
            let mut dismissed = false;
            for action in actions.iter().filter_map(|a| a.as_widget_action()) {
                if let Some(CallAction::Press(p)) = action.action.downcast_ref::<CallAction>() { press = Some(*p); }
                dismissed |= action.widget_uid == tip.widget_uid() && matches!(action.cast(), tip::TipAction::HoverOut);
            }
            ui.handle_event(cx, &Event::Actions(actions), &mut Scope::empty());
            (press, dismissed)
        };
        dispatch(cx, hover());
        assert_eq!(shown_tip(cx), "Call Anna", "the call button raised its tooltip");
        let (_, dismissed) = dispatch(cx, Event::MouseDown(MouseDownEvent {
            abs, window_id, button: MouseButton::PRIMARY, modifiers: Default::default(), handled: Default::default(), time: 1.1,
        }));
        assert!(dismissed, "the claimed press dismissed the call button's own tip");
        let (press, _) = dispatch(cx, Event::MouseUp(MouseUpEvent {
            abs, window_id, button: MouseButton::PRIMARY, modifiers: Default::default(), time: 1.2,
        }));
        assert_eq!(press, Some(Press::Call));
        assert_eq!(shown_tip(cx), "", "press takes the visible tip down immediately");
        // An accessibility press takes the tip down the way a click does.
        dispatch(cx, Event::ClearHover);
        dispatch(cx, hover());
        assert_eq!(shown_tip(cx), "Call Anna");
        let call = header.widget(cx, ids!(call_start));
        let (press, dismissed) = dispatch(cx, Event::A11yAction(a11y_tree::ActionRequest {
            req: 0, node: a11y_tree::NodeId::widget(call.widget_uid().0), action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None,
        }));
        assert_eq!(press, Some(Press::Call));
        assert!(dismissed && shown_tip(cx).is_empty(), "an accessibility press takes the call button's tip down");
        // The other header can start the call while this tip is still raised.
        dispatch(cx, Event::ClearHover);
        dispatch(cx, hover());
        assert_eq!(shown_tip(cx), "Call Anna");
        flow.press(Press::Call, now, &mut effects);
        let answer = Message::Answer { call_id: flow.session().unwrap(), secret: [3; 32], ext: crate::call::AnswerExt { town: Some([9; 32]), ..Default::default() } };
        flow.receive([1; 65], &answer.encode(), now, &mut effects);
        assert_eq!(flow.view().phase, Phase::Live);
        let actions = cx.capture_actions(|cx| header.show(cx, &flow.view().peer_name, true, true));
        ui.handle_event(cx, &Event::Actions(actions), &mut Scope::empty());
        assert_eq!(shown_tip(cx), "", "hiding Call dismisses even without another pointer event");
        row.show(cx, flow.view(), &Devices::default(), Talking::default(), "");
        target.draw(cx, &ui, size);
        dispatch(cx, hover());
        assert!(row.widget(cx, ids!(mute)).visible() && row.widget(cx, ids!(hang_up)).visible());
        assert_eq!(shown_tip(cx), "", "the stationary pointer leaves the live call controls clear");
    }

    #[test]
    fn rail_layout_menus_and_local_input_controls() {
        use a11y_tree::{ActionArg, Actions as A};
        use AudioDeviceType::{Input, Output};
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let ui = cx.with_vm(|vm| {
            let val = vm.eval(script! {
                use mod.prelude.workspace.*
                use mod.widgets.*
                View{width: Fill height: Fill flow: Down
                    live_call := LiveCallPage{} header := CallButton{} card := CallButton{}
                    live_share := LiveSharePage{visible: false}
                    line := CallEntryRow{} dot := LiveDot{}
                    transcript := View{visible: false list := PortalList{CallLine := CallEntryRow{}}}
                }
            });
            WidgetRef::script_from_value(vm, val)
        });
        // Changing the scoped snapshot is sufficient to bind the page's controls.
        let mut scoped = ViewData::default();
        scoped.members.seats.push(crate::view::MemberSeat { id: crate::view::MemberId::Seat([2; 32]), devices: Vec::new(), live: None,
            seat: Seat::Peer, tone: None, name: "Anna".into(), presence: crate::view::Presence::Away, status: String::new() });
        std::sync::Arc::make_mut(&mut scoped.call.view).peer_name = "Anna".into();
        let pane = dvec2(279., 600.);
        let pass = DrawPass::new(cx);
        pass.set_size(cx, pane);
        let mut draw_list = DrawList2d::new(cx);
        for (linked, phase) in [(false, Phase::Idle), (true, Phase::Idle), (true, Phase::Calling), (true, Phase::Ringing),
                                (true, Phase::Live), (false, Phase::Idle)] {
            scoped.call.linked = linked;
            scoped.call.can_call = linked;
            scoped.call.can_share = linked;
            if scoped.call.view.phase != phase {
                std::sync::Arc::make_mut(&mut scoped.call.view).phase = phase;
            }
            scoped.call.mic_open = phase == Phase::Live;
            {
                let event = DrawEvent::default();
                let mut draw = makepad_widgets::makepad_draw::cx_draw::CxDraw::new(cx, &event);
                let mut frame = Cx2d::new(&mut draw);
                frame.begin_pass(&pass, None);
                draw_list.begin_always(&mut frame);
                frame.begin_root_turtle(pane, Layout::flow_down());
                ui.draw_all(&mut frame, &mut Scope::with_data(&mut scoped));
                frame.end_pass_sized_turtle();
                draw_list.end(&mut frame);
                frame.end_pass(&pass);
            }
            assert_eq!(ui.widget(cx, ids!(live_call.content)).visible(), phase != Phase::Idle, "scope drives the call page");
            if phase == Phase::Live {
                let ring = ui.widget(cx, ids!(live_call.content.people.peer_badge.ring));
                let tick = Event::Timer(TimerEvent { time: None, timer_id: 1 });
                scoped.call.peer_level.set(7);
                ui.handle_event(cx, &tick, &mut Scope::with_data(&mut scoped));
                let mut ring = ring.borrow_mut::<VoiceRing>().unwrap();
                assert_eq!(ring.step, 7, "a live meter tick reaches the ring through the row");
                assert!(!ring.sync_step(cx, &mut Scope::with_data(&mut scoped)), "steady speech requests no redraw");
                std::sync::Arc::make_mut(&mut scoped.call.view).peer_muted = true;
                assert!(ring.sync_step(cx, &mut Scope::with_data(&mut scoped)) && ring.step == 0,
                    "mute removes the ring despite the cached level");
            }
            assert!(ui.widget(cx, ids!(live_call.call_record)).is_empty() && ui.widget(cx, ids!(header.call_record)).is_empty(), "Clip in the capture bar is the only recording control");
            let idle = phase == Phase::Idle;
            let call = ui.widget(cx, ids!(header.call_start));
            assert_eq!(ui.widget(cx, ids!(header.content)).visible(), idle, "show Call whenever idle");
            assert_eq!(call.visible(), idle);
            assert_eq!(!call.disabled(cx), linked && idle, "Call needs a linked, idle peer");
            let label = if !idle { "" } else if linked { "Call Anna" } else { "Anna isn't connected yet" };
            assert_eq!(ui.tip(cx, ids!(header.tip)).borrow().unwrap().text, label);
            if idle {
                assert_eq!(call.text(), label);
                let mut node = a11y_tree::Node::default();
                call.borrow::<Button>().unwrap().semantics(cx, &mut node);
                assert_eq!(node.name, label);
            }
            if !linked && idle {
                assert!(activate(cx, &ui, &call, A::PRESS, ActionArg::None).is_empty(), "disabled Call emitted a press");
            }
            assert!(ui.widget(cx, ids!(header.call_share)).is_empty(), "the chat header has no Share control");
            let share = ui.widget(cx, ids!(live_call.call_share));
            assert_eq!(share.visible(), phase == Phase::Live);
            let pressed = activate(cx, &ui, &share, A::PRESS, ActionArg::None);
            if phase == Phase::Live {
                assert_eq!(pressed, [(share.widget_uid(), CallAction::Press(Press::Share(super::super::ShareChoice::Menu)))]);
            } else {
                assert!(pressed.is_empty(), "a hidden Share in {phase:?} emitted {pressed:?}");
            }
            assert_eq!(ui.widget(cx, ids!(dot.content)).visible(), phase == Phase::Live, "scope drives the mic indicator");
        }
        let row = ui.call_row(cx, ids!(live_call));
        let line = ui.call_entry_row(cx, ids!(line));
        let mic = row.widget(cx, ids!(call_microphone));
        let speaker = row.widget(cx, ids!(call_speaker));
        let mute = row.widget(cx, ids!(mute));
        let hang = row.widget(cx, ids!(hang_up));
        let answer = row.widget(cx, ids!(call_answer));
        let mut view = CallView::default();
        view.phase = Phase::Live;
        view.peer_name = "Anna".into();
        view.duration_secs = 42;
        let mut data = Devices::default();
        let mut list = vec![device(1, Input, "Recording interface with a very long microphone name", true),
                            device(2, Input, "Broken", false), device(3, Output, "Speaker", true),
                            device(8, Output, "Failed output", false)];
        list[1].has_failed = true;
        list[3].has_failed = true;
        let devices_row = row.widget(cx, ids!(devices));
        data.update(&list);
        row.show(cx, &view, &data, Talking::default(), "");
        assert!(!mic.visible() && !speaker.visible() && !devices_row.visible(),
                "one healthy device of each kind gives no picker, even with failed devices listed");
        list.push(device(7, Output, "Headphones", false));
        data.update(&list);
        row.show(cx, &view, &data, Talking::default(), "");
        assert!(!mic.visible() && speaker.visible() && devices_row.visible(), "two speakers give a speaker choice");
        list.push(device(6, Input, "Desk microphone", false));
        data.update(&list);
        row.show(cx, &view, &data, Talking { you: true, peer: false }, "");
        assert!(mic.visible() && speaker.visible(), "two healthy microphones and speakers give both choices");
        let mut target = ui_checks::UiCheckTarget::new(cx);
        let size = dvec2(279., 600.);
        target.draw(cx, &ui, size);
        let runs = target.draw(cx, &ui, size);
        for picker in [&mic, &speaker] {
            let glyph = picker.widget(cx, ids!(glyph));
            assert!(glyph.borrow::<Svg>().unwrap().draw_svg.svg_doc.is_some(), "the picker loads its icon resource");
        }
        let first = row.widget(cx, ids!(people)).area().rect(cx);
        let second = row.widget(cx, ids!(controls)).area().rect(cx);
        for (w, above) in [(&mute, first), (&hang, first), (&mic, second), (&speaker, second)] {
            let rect = w.area().rect(cx);
            assert!(rect.size.x > 0. && rect.size.y > 0. && rect.pos.y >= above.pos.y + above.size.y,
                    "control {rect:?} must be below {above:?}");
        }
        for path in [ids!(people), ids!(peer_badge), ids!(peer_name), ids!(duration), ids!(mute),
                     ids!(hang_up), ids!(devices), ids!(call_microphone), ids!(call_speaker)] {
            let rect = row.widget(cx, path).area().rect(cx);
            assert!(rect.pos.x >= 0. && rect.pos.x + rect.size.x <= 279.01, "{path:?} overflows: {rect:?}");
        }
        let name = mic.widget(cx, ids!(name));
        assert!(runs.iter().any(|r| r.owner == name.widget_uid().0 && r.truncated),
            "the name is elided on the face: picker {:?}, label {:?}, shown {}, text {:?}, runs {:?}",
            mic.area().rect(cx), name.area().rect(cx), name.visible(), name.text(),
            runs.iter().filter(|r| r.owner == name.widget_uid().0).collect::<Vec<_>>());
        assert_eq!(row.label(cx, ids!(duration)).text(), "0:42");
        assert!(row.widget(cx, ids!(you_talking)).visible());
        for (w, press) in [(&hang, Press::HangUp), (&mute, Press::Mute)] {
            assert_eq!(activate(cx, &ui, w, A::PRESS, ActionArg::None), [(w.widget_uid(), CallAction::Press(press))]);
        }
        let body = row.child(id!(content));
        view.peer_transcribing = true;
        view.transcribe_prompt = true;
        CallRow::show_transcription(cx, &body, &view, &TranscribeShown::Off, "Workshop");
        let yes = row.widget(cx, ids!(call_transcribe_yes));
        let no = row.widget(cx, ids!(call_not_me));
        assert!(yes.visible() && no.visible(), "the invitation offers both choices");
        assert_eq!(row.label(cx, ids!(question)).text(), "Anna is transcribing. Transcribe you too?");
        assert_eq!(activate(cx, &ui, &yes, A::PRESS, ActionArg::None), [(yes.widget_uid(), CallAction::Press(Press::Transcribe))]);
        assert_eq!(activate(cx, &ui, &no, A::PRESS, ActionArg::None), [(no.widget_uid(), CallAction::Press(Press::NotMe))]);
        view.transcribe_prompt = false;
        view.transcribing_me = true;
        CallRow::show_transcription(cx, &body, &view, &TranscribeShown::On("local Whisper".into()), "Workshop");
        assert_eq!(row.label(cx, ids!(transcription)).text(), "Your words go into Workshop's chat · local Whisper\nAnna is transcribing");
        assert!(row.widget(cx, ids!(call_stop_transcribing)).visible() && !yes.visible());
        view.transcribing_me = false;
        view.peer_transcribing = false;
        CallRow::show_transcription(cx, &body, &view, &TranscribeShown::Unavailable("Install a speech model.".into()), "Workshop");
        assert!(row.button(cx, ids!(call_transcribe)).disabled(cx));
        assert_eq!(row.label(cx, ids!(transcription)).text(), "Install a speech model.");
        view.muted = true;
        view.peer_muted = true;
        row.show(cx, &view, &data, Talking::default(), "");
        assert_eq!(row.label(cx, ids!(peer_muted)).text(), "Anna muted");
        assert!(!mute.visible() && row.widget(cx, ids!(call_unmute)).visible());
        view.unmuting = true;
        row.show(cx, &view, &data, Talking::default(), "");
        let unmute = row.button(cx, ids!(call_unmute));
        assert_eq!(unmute.text(), "Unmuting…");
        assert!(unmute.disabled(cx));

        assert!(activate(cx, &ui, &mic, A::EXPAND, ActionArg::None).is_empty());
        target.draw(cx, &ui, size);
        let new = device(4, Input, "USB", false);
        let usb_id = new.device_id;
        list.insert(0, new);
        data.update(&list);
        row.show(cx, &view, &data, Talking::default(), "");
        target.draw(cx, &ui, size);
        let mut node = a11y_tree::Node::default();
        mic.borrow::<DevicePicker>().unwrap().semantics(cx, &mut node);
        assert!(node.states.contains(a11y_tree::States::EXPANDED), "hot-plug leaves the menu open");
        assert_eq!(node.name, format!("Microphone: {}", data.label(DeviceKind::Input)));
        let menu = mic.borrow::<DevicePicker>().unwrap().menu.as_drop_down();
        assert!(menu.has_labels(&["USB".into(), format!("✓ {} · system default", data.label(DeviceKind::Input)),
                                   "Broken · unavailable".into(), "Desk microphone".into()]), "updated menu names the current, default and failed devices");
        assert!(activate(cx, &ui, &mic, A::SELECT, ActionArg::Text("Broken · unavailable".into())).is_empty());
        assert!(activate(cx, &ui, &menu_row(&menu, 2), A::PRESS, ActionArg::None).is_empty());
        assert!(click_row(cx, &ui, &menu_row(&menu, 2)).is_empty());
        // A hot-plug moved the displayed USB row from zero to one. A click before repaint
        // still selects USB, the row the person actually saw.
        let drawn_usb = menu_row(&menu, 0);
        list.insert(0, device(5, Input, "Another microphone", false));
        data.update(&list);
        row.show(cx, &view, &data, Talking::default(), "");
        assert_eq!(click_row(cx, &ui, &drawn_usb),
                   [(mic.widget_uid(), CallAction::Pick(DeviceKind::Input, usb_id))]);
        list.retain(|d| d.device_id != AudioDeviceId(LiveId(1)));
        list[0].is_default = true;
        data.update(&list);
        row.show(cx, &view, &data, Talking::default(), "");
        assert_eq!(row.label(cx, ids!(notice)).text(),
            "Using Another microphone: Recording interface with a very long microphone name was unplugged");
        target.draw(cx, &ui, dvec2(104., 600.));
        assert!(!name.visible() && mic.widget(cx, ids!(glyph)).visible(), "narrow picker retains just its icon");
        target.draw(cx, &ui, size);
        assert!(name.visible());
        // The real 279-point rail puts nine points of padding on each side.
        target.draw(cx, &ui, dvec2(261., 600.));
        for w in [&mic, &speaker, &mute, &hang] {
            let r = w.area().rect(cx);
            assert!(r.pos.x >= 0. && r.pos.x + r.size.x <= 261.01, "rail padding clips {r:?}");
        }
        list.retain(|d| !matches!(d.name.as_str(), "USB" | "Desk microphone" | "Headphones"));
        data.update(&list);
        row.show(cx, &view, &data, Talking::default(), "");
        assert!(!devices_row.visible() && row.widget(cx, ids!(notice)).visible(),
                "the unplug notice survives when only the replacement device is available");
        view.phase = Phase::Calling;
        row.show(cx, &view, &data, Talking::default(), "");
        assert!(row.widget(cx, ids!(cancel)).visible() && !mic.visible() && !speaker.visible());
        view.phase = Phase::Ringing;
        row.show(cx, &view, &data, Talking::default(), "");
        assert!(answer.visible() && row.widget(cx, ids!(decline)).visible() && !mic.visible()
                && !speaker.visible() && !devices_row.visible(), "one available speaker needs no ringing picker");
        list.push(device(7, Output, "Headphones", false));
        data.update(&list);
        row.show(cx, &view, &data, Talking::default(), "");
        assert!(answer.visible() && !mic.visible() && speaker.visible() && devices_row.visible());
        assert_eq!(activate(cx, &ui, &answer, A::PRESS, ActionArg::None), [(answer.widget_uid(), CallAction::Press(Press::Answer))]);
        view.phase = Phase::Idle;
        row.show(cx, &view, &data, Talking::default(), "");
        assert!(!row.visible());

        let header = ui.call_button(cx, ids!(header));
        header.show(cx, "Anna", false, false);
        let call = header.widget(cx, ids!(call_start));
        assert!(header.visible() && call.visible() && call.disabled(cx));
        assert_eq!(call.text(), "Anna isn't connected yet");
        assert_eq!(header.tip(cx, ids!(tip)).borrow().unwrap().text, call.text());
        assert!(activate(cx, &ui, &call, A::PRESS, ActionArg::None).is_empty(), "disabled Call emitted a press");
        header.show(cx, "Anna", true, false);
        assert!(header.visible() && call.visible() && !call.disabled(cx));
        assert_eq!(header.tip(cx, ids!(tip)).borrow().unwrap().text, "Call Anna");
        assert_eq!(activate(cx, &ui, &call, A::PRESS, ActionArg::None), [(call.widget_uid(), CallAction::Press(Press::Call))]);
        header.show(cx, "Anna", true, true);
        assert!(!header.visible());
        let mut missed = crate::view::CallLine { id: crate::view::TranscriptId([0; 32], [1; 16]),
            words: "Missed call from Ana".into(), time: "09:42".into(),
            call_back: Some(crate::view::CallBack { seat: [9; 32], peer: "Ana".into(), enabled: true }), count: 0, transcript: None, sent: String::new(), jobs: Vec::new() };
        line.show(cx, &missed);
        assert_eq!(line.label(cx, ids!(line)).text(), "Missed call from Ana", "the row uses the session summary");
        assert_eq!(line.label(cx, ids!(head.when)).text(), "09:42", "the row draws the anchor time in its header");
        assert!(line.widget(cx, ids!(call_back)).visible(), "a missed call offers a return call");
        let back = line.widget(cx, ids!(call_back));
        assert_eq!(activate(cx, &ui, &back, A::PRESS, ActionArg::None),
            [(back.widget_uid(), CallAction::Press(Press::CallSeat([9; 32])))], "Call back dispatches to the opener's seat");
        let dot = ui.live_dot(cx, ids!(dot));
        dot.show(cx, true);
        assert!(dot.visible());
        dot.show(cx, false);
        assert!(!dot.visible());

        let item = ui.portal_list(cx, ids!(transcript.list)).item(cx, 0, id!(CallLine));
        let mut expected = [item.widget(cx, ids!(call_back)), call, ui.widget(cx, ids!(card.call_start)), answer, row.widget(cx, ids!(call_unmute)),
            mic, row.widget(cx, ids!(call_transcribe)), row.widget(cx, ids!(call_transcribe_yes)), line.widget(cx, ids!(call_back)),
            row.widget(cx, ids!(call_share)), ui.widget(cx, ids!(header.call_join)), ui.widget(cx, ids!(card.call_join)),
            ui.widget(cx, ids!(live_share.watch)), row.widget(cx, ids!(choices)), ui.widget(cx, ids!(live_share.let_control))].map(|w| w.widget_uid().0);
        expected.sort_unstable();
        let mut actual: Vec<_> = opening_uids(cx, &ui).into_iter().map(|u| u.0).collect();
        actual.sort_unstable();
        assert_eq!(actual, expected, "each opening control across both placements has its own UID");
        missed.call_back = None;
        line.show(cx, &missed);
        assert!(back.action_data().as_ref().is_some_and(|data| data.downcast_ref::<()>().is_some()), "the hidden callback forgets its target");
        assert!(!line.widget(cx, ids!(call_back)).visible(), "a connected callback hides the old button on a reused row");
    }
}
