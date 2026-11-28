use makepad_widgets::{event::{DigitId, TouchState}, *};
use crate::ai_trace::{single_line, Ink, Phase};
use crate::model::{wall_ms, ComposerTarget, MentionCandidate, MentionTarget};
use crate::lists::ViewData;
use crate::view::{ChatRow, ChatRowKey, ClipId, ClipLine, Message, TraceFold};
use makepad_town_core::event::Id;
use makepad_town_core::home::Home;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.MentionPickerBase = #(MentionPicker::register_widget(vm))
    mod.widgets.AgentChatListBase = #(AgentChatList::register_widget(vm))
    let ClipBase = #(ClipRow::register_widget(vm))
    let Clip = ClipBase{
        width: Fill height: Fit flow: Down spacing: 4 padding: Inset{bottom: 9}
        clip_title := BodyNote{draw_text.color: palette.text}
        media := View{
            width: Fill height: Fit flow: Down spacing: 4
            frame := SolidView{
                width: 240 height: 135 flow: Overlay draw_bg.color: palette.panel2
                cover := View{
                    width: Fill height: Fill flow: Overlay
                    poster := Image{width: Fill height: Fill fit: ImageFit.Stretch}
                    View{width: Fill height: Fill align: Align{x: 1 y: 1} padding: 5
                        duration_badge := RoundedView{width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1}
                            draw_bg +: {color: palette.panel border_radius: 2}
                            length := Kicker{draw_text.color: palette.text}
                        }
                    }
                }
                // A picture is shown alone: until it has arrived, its progress is on it.
                progress := View{visible: false width: Fill height: Fill align: Align{x: 0.5 y: 0.5} padding: 8
                    RoundedView{width: Fit height: Fit padding: Inset{left: 6 right: 6 top: 3 bottom: 3}
                        draw_bg +: {color: palette.panel border_radius: 2}
                        progress_line := BodyNote{width: Fit draw_text.color: palette.text}
                    }
                }
                player := VideoPlayer{visible: false}
                picture_border := RectView{visible: false width: Fill height: Fill
                    draw_bg +: {border_color: palette.accent border_size: 1.0}
                }
            }
            details := View{width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
                size := Kicker{}
                state := BodyNote{width: Fit}
            }
            failure := BodyNote{visible: false text: "This clip can't be played"}
            controls := View{width: Fill height: Fit flow: Right{wrap: true} spacing: 6
                get := PanelButton{text: "Get"}
                play := PanelButton{text: "Play"}
                close_clip := PanelButton{text: "Close" a11y: {label: "Close clip player"}}
                pin := PanelButton{visible: false text: "Pin"}
            }
        }
    }
    let Name = RailText{draw_text.text_style: theme.font_bold{font_size: 8.625}}
    let QuoteLine = BodyNote{width: Fill flow: Right selectable: false max_lines: 1 text_overflow: Ellipsis}
    let TraceText = RailText{width: Fill draw_text.text_style: theme.font_regular{font_size: 9}}
    let TraceRows = FlatList{
        width: Fill height: Fit flow: Down spacing: 3
        drag_scrolling: false scroll_bars +: {show_scroll_y: false show_scroll_x: false}
        Sent := Kicker{text: "sent" draw_text.color: palette.faint}
        Receipt := View{
            width: Fill height: Fit flow: Right spacing: 5 align: Align{y: 0.5}
            avatar := AiBadge{width: 17 height: 17}
            caption := TraceText{draw_text.color: palette.yourai a11y: {label_for: @avatar}}
        }
        Work := View{
            width: Fill height: Fit flow: Right spacing: 5
            spinner := SolidView{
                width: 14 height: 14
                draw_bg +: {
                    color: palette.yourai
                    turn: instance(0.0)
                    pixel: fn() {
                        let center = self.rect_size * 0.5
                        let p = self.pos * self.rect_size - center
                        let sweep = fract(self.turn - atan2(p.y, p.x) / TAU)
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.circle(center.x, center.y, 4.8)
                        sdf.stroke(vec4(self.color.xyz, 0.18 + 0.82 * (1.0 - sweep)), 1.0)
                        return sdf.result
                    }
                }
            }
            caption := TraceText{}
        }
        Warning := TraceText{draw_text.color: palette.warn}
        Finished := TraceText{draw_text.color: palette.good}
        Fold := PanelButton{padding: Inset{left: 6 right: 6 top: 3 bottom: 3} width: Fit}
        Output := TraceText{max_lines: 1 text_overflow: Ellipsis}
    }
    let Bubble = RoundedView{
        // Room inside the border a selected line shows.
        width: Fill height: Fit flow: Down spacing: 3
        padding: Inset{left: 7 right: 7 top: 3 bottom: 4}
        draw_bg +: {color: #0000 border_color: palette.accent border_size: 1 border_radius: 6}
        head := ChatRowHead{
            name := Name{selectable: true draw_selection.color: palette.textsel}
            when := ChatTime{selectable: true draw_selection.color: palette.textsel}
        }
        // The line this one answers, behind a thin rule so it never reads as this message's words.
        quote := View{visible: false width: Fill height: 20 flow: Overlay
            quote_jump := PanelButton{
                width: Fill height: Fill text: "" grab_key_focus: false padding: 0 margin: 0
                a11y: {role: "link"}
                draw_bg +: {color: #0000 color_hover: palette.sel color_down: palette.card border_size: 0 border_radius: 3}
            }
            quote_body := View{
                a11y: {hidden: true}
                width: Fill height: Fill flow: Right spacing: 6 align: Align{y: 0.5}
                padding: Inset{left: 2 right: 4 top: 1 bottom: 1}
                SolidView{width: 2 height: 14 draw_bg.color: palette.line}
                quote_text := QuoteLine{}
            }
        }
        message := BodyNote{selectable: true draw_selection.color: palette.textsel draw_text +: {color: palette.text text_style: theme.font_regular{font_size: 10.5}}}
        approval := View{
            visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 4
            allow := PanelButton{}
            deny := PanelButton{text: "Don’t"}
        }
        trace_section := View{visible: false width: Fill height: Fit flow: Down trace_rows := TraceRows{}}
        sent := Kicker{selectable: false visible: false text: "sent" draw_text.color: palette.faint}
        no_ai := View{
            visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
            Kicker{selectable: false text: "No AI here yet ·"}
            chat_add_ai := PanelButton{text: "Add AI"}
        }
    }
    let Message = View{
        width: Fill height: Fit flow: Down spacing: 0
        unread_line := View{
            visible: false width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            padding: Inset{top: 6 bottom: 6}
            PanelLine{}
            Kicker{selectable: false text: "New messages" draw_text.color: palette.accent}
            PanelLine{}
        }
        frame := View{
            width: Fill height: Fit flow: Overlay padding: Inset{bottom: 3}
            row := View{
                width: Fill height: Fit flow: Right spacing: 3 padding: Inset{left: 5}
                author := SeatIcons{
                    you.initial.selectable: false
                    your_ai.initial.selectable: false
                    peer.initial.selectable: false
                    peer_ai.initial.selectable: false
                }
                content := View{width: Fill height: Fit flow: Down spacing: 3
                    bubble := Bubble{show_bg: false}
                    // Why an action from the message's menu could not be done, in place.
                    action_note := BodyNote{visible: false selectable: false draw_text.color: palette.warn}
                }
            }
            mention_bar := SolidView{show_bg: false width: 2 height: Fill draw_bg.color: palette.accent}
        }
    }
    // A plain smiley, drawn: it opens the emoji grid.
    let EmojiButton = Button{
        width: 34 height: 34 margin: 0 padding: 0 text: "" grab_key_focus: false
        a11y: {label: "Emoji"}
        draw_bg +: {
            ink: uniform(palette.muted)
            ink_hover: uniform(palette.text)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let c = self.rect_size * 0.5
                let ink = mix(self.ink, self.ink_hover, max(self.hover, self.down))
                sdf.circle(c.x, c.y, 8.5)
                sdf.stroke(ink, 1.4)
                sdf.circle(c.x - 3.0, c.y - 2.5, 1.25)
                sdf.fill(ink)
                sdf.circle(c.x + 3.0, c.y - 2.5, 1.25)
                sdf.fill(ink)
                sdf.arc_round_caps(c.x, c.y + 0.5, 4.5, -0.95, 0.95, 1.4)
                sdf.fill(ink)
                return sdf.result
            }
        }
    }
    mod.widgets.ChatConversation = View{
        width: Fill height: Fill flow: Down spacing: 8
        chat_empty := BodyNote{text: "Say hello. Everyone in this town reads along."}
        View{width: Fill height: Fill flow: Overlay
            transcript := mod.widgets.AgentChatListBase{
                mention_ink: palette.yourai action_ink: palette.muted status_ink: palette.text warning_ink: palette.warn
                outline_ink: palette.accent line_ink: palette.line
                width: Fill height: Fill
                list := PortalList{
                    // The scroll bar (10 wide) overlays the list's right edge: rows wrap short of it.
                    width: Fill height: Fill auto_tail: true keep_end_on_resize: true padding: Inset{right: 12}
                    selectable: true select_all_entry_first: true triple_click_paragraph: true
                    emit_scroll_actions: true emit_item_actions: true grab_key_focus: true
                    Line := Message{}
                    DayLine := View{
                        width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                        padding: Inset{top: 8 bottom: 4}
                        PanelLine{}
                        day_label := Kicker{selectable: false}
                        PanelLine{}
                    }
                    MemberLine := BodyNote{}
                    ClipLine := Clip{}
                    CallLine := CallEntryRow{
                        controls +: {flow: Right{wrap: true}}
                        summarise := PanelButton{visible: false width: Fit text: "Summarise"}
                        update_tasks := PanelButton{visible: false width: Fit text: "Update tasks"}
                        // One line per job, in one note, so a screen reader reads it with the call.
                        jobs := BodyNote{visible: false width: Fill margin: Inset{left: 12}}
                    }
                    Addressed := Message{
                        frame.row.content.bubble +: {
                            show_bg: true padding: Inset{top: 6 bottom: 6 left: 8 right: 8}
                            draw_bg +: {color: palette.sel border_radius: 4 border_size: 1 border_color: palette.line}
                            sent +: {visible: true}
                        }
                    }
                }
            }
            jump_latest := View{
                visible: false width: Fill height: Fill align: Align{x: 1.0 y: 1.0} padding: Inset{right: 18 bottom: 8}
                jump_latest_btn := PanelButton{text: "New messages ↓"}
            }
        }
        // Under the messages, beside the input it explains: nothing stacks above the messages.
        town_read_only := BodyNote{visible: false}
        transcript_confirmation := RoundedView{
            visible: false width: Fill height: Fit flow: Down spacing: 8 padding: 10
            draw_bg +: {color: palette.sel border_color: palette.line border_size: 1 border_radius: 4}
            transcript_question := BodyNote{width: Fill}
            View{width: Fill height: Fit flow: Right{wrap: true} spacing: 6
                transcript_send := PanelButton{text: "Send"}
                transcript_cancel := PanelButton{text: "Cancel"}
            }
        }
        // Shown while a file is dragged over the window: how a drop will post.
        drop_hint := BodyNote{visible: false width: Fill}
        // Shown only while an awaiting AI approval is scrolled off-screen.
        pending_approval := View{
            visible: false width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
            pending_approval_text := BodyNote{width: Fill flow: Right selectable: false max_lines: 1 text_overflow: Ellipsis draw_text.color: palette.warn}
            pending_approval_show := PanelButton{text: "Show"}
        }
        mention_picker := mod.widgets.MentionPickerBase{
            visible: false width: Fill height: Fit{max: 180}
            list := FlatList{
                width: Fill height: Fit{max: 180} flow: Down spacing: 2
                scroll_bars +: {show_scroll_y: true}
                Space := View{}
                Row := RoundedView{
                    width: Fill height: 28 flow: Overlay show_bg: false
                    draw_bg +: {color: palette.sel border_radius: 4}
                    pick := PanelButton{
                        width: Fill height: Fill text: "" grab_key_focus: false padding: 0 margin: 0
                        draw_bg +: {color: #0000 color_hover: palette.sel color_down: palette.card border_size: 0}
                    }
                    View{
                        a11y: {hidden: true}
                        width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                        padding: Inset{left: 6 right: 8 top: 4 bottom: 4}
                        badge := SeatIcons{}
                        label := RailText{width: Fill max_lines: 1 text_overflow: Ellipsis}
                    }
                }
            }
        }
        // Show who the current draft addresses.
        mention_hint := BodyNote{visible: false width: Fill}
        reply_hint := View{visible: false width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
            reply_line := QuoteLine{}
            stop_reply := PanelButton{text: "×" width: 24 height: 24 padding: 0 a11y: {label: "Stop replying"}}
        }
        // Enter sends, Shift-Enter starts a new line; the field grows up to six lines, then scrolls.
        composer := View{width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 1.0}
            chat_input := PanelInput{
                width: Fill height: Fit{max: FitBound.Abs(124)}
                padding: Inset{left: 14 right: 14 top: 9 bottom: 9}
                is_multiline: true submit_on_enter: true
                empty_text: "Message"
                a11y: {label: "Chat message"}
                draw_text +: {text_style: theme.font_regular{font_size: 10.5}}
                draw_bg +: {border_radius: 17}
            }
            // A pick goes in at the cursor, and the grid stays open for the next one.
            emoji := Popover{
                placement: TopEnd offset: 6.0 trap_focus: true
                draw_panel +: {color: palette.card border_color: palette.line arrow_border_color: palette.line}
                emoji_button := EmojiButton{}
                content +: {
                    emoji_picker := EmojiPicker{
                        draw_text +: {color: palette.text}
                        title_color: palette.muted
                        draw_highlight +: {color: palette.sel}
                    }
                }
            }
        }
    }
    mod.widgets.ChatRail = SolidView{
        width: Fill height: Fill flow: Down padding: 9 spacing: 8 draw_bg.color: palette.panel
        conversation := mod.widgets.ChatConversation{}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MentionPick {
    pub town: Id,
    pub generation: u64,
    pub target: ComposerTarget,
}

#[derive(Default, Debug)]
pub struct MentionPickerState {
    pub open: bool,
    pub town: Option<Id>,
    pub generation: u64,
    pub query_range: std::ops::Range<usize>,
    pub candidates: Vec<MentionCandidate>,
    pub highlighted: usize,
    pub dismissed_at: Option<usize>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct MentionPicker {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] row_height: f64,
    #[rust] scrolled_generation: Option<u64>,
    #[rust] highlighted_pick: Option<WidgetUid>,
}

impl Widget for MentionPicker {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.highlighted_pick = None;
        let mut drawn_picker = None;
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let picker = &data.mention_picker;
            let Some(town) = picker.town.filter(|_| picker.open) else { continue };
            let Some(mut list) = step.borrow_mut::<FlatList>() else { continue };
            let top = cx.turtle().origin_unscrolled().y;
            for (i, candidate) in picker.candidates.iter().enumerate() {
                // All rows have a single label line and equally sized badges. Keep a blank
                // layout area for offscreen rows so SCROLL_INTO_VIEW can address every index.
                let slot = Walk { width: Size::fill(), height: Size::Fixed(self.row_height), ..Walk::default() };
                let y = cx.peek_walk_pos(slot).y;
                if self.row_height > 0. && (y >= top + 180. || y + self.row_height <= top) {
                    if let Some(space) = list.item(cx, LiveId::from_num(id!(space).0, i as u64), id!(Space)) {
                        space.draw_walk_all(cx, &mut Scope::empty(), slot);
                    }
                    continue
                }
                let Some(row) = list.item(cx, LiveId::from_num(id!(row).0, i as u64), id!(Row)) else { continue };
                if let Some(mut view) = row.borrow_mut::<View>() { view.show_bg = i == picker.highlighted; }
                let badge = row.widget(cx, ids!(badge));
                crate::lists::seat_icon(cx, &badge, candidate.seat, &candidate.insert_text, candidate.tone);
                let label = row.label(cx, ids!(label));
                let mut pick = row.button(cx, ids!(pick));
                if i == picker.highlighted {
                    self.highlighted_pick = Some(pick.widget_uid());
                }
                if label.text() != candidate.label {
                    label.set_text(cx, &candidate.label);
                    let name = cx.with_vm(|vm| vm.new_string(&candidate.label));
                    script_apply_eval!(cx, pick, {a11y_name: #(name)});
                }
                pick.set_action_data(MentionPick { town, generation: picker.generation, target: candidate.target });
                row.draw_all(cx, &mut Scope::empty());
                self.row_height = row.area().rect(cx).size.y;
            }
            list.items.retain_visible();
            drawn_picker = Some((picker.generation, picker.highlighted));
        }
        // FlatList::end has now recorded the viewport for the rows drawn this frame.
        if let Some((generation, highlighted)) = drawn_picker.filter(|(generation, _)| Some(*generation) != self.scrolled_generation) {
            if let Some(mut list) = self.view.flat_list(cx, ids!(list)).borrow_mut() {
                self.scrolled_generation = Some(generation);
                let request = a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId(list.widget_uid().0),
                    action: a11y_tree::Actions::SCROLL_INTO_VIEW, arg: a11y_tree::ActionArg::Number(highlighted as f64) };
                list.handle_action(cx, &request, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if let Some(uid) = self.highlighted_pick {
            out.push((uid.0, None, a11y_tree::Node {
                id: a11y_tree::NodeId::widget(uid.0),
                role: a11y_tree::Role::Button,
                states: a11y_tree::States::SELECTED,
                ..Default::default()
            }));
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.view.visible { self.view.handle_event(cx, event, scope); }
    }
}

#[derive(Clone, Copy, Debug)]
struct PressedChatTarget {
    digit: DigitId,
    town: Option<Id>,
    /// Press-time target: `Some(id)` for a message or picture body; `None` for the
    /// "New messages" divider, non-targetable rows, or empty list space.
    target: Option<Id>,
}

#[derive(Clone, Copy)]
struct DrawnStrip {
    town: Id,
    message: Id,
    height: f64,
}

#[derive(Clone, Copy, Debug)]
struct PendingApprovalVisibility;

#[derive(Script, ScriptHook, Widget)]
pub struct AgentChatList {
    #[live]
    mention_ink: Vec4f,
    #[live] action_ink: Vec4f,
    #[live] status_ink: Vec4f,
    #[live] warning_ink: Vec4f,
    /// A selected message's outline, and an addressed one's border.
    #[live] outline_ink: Vec4f,
    #[live] line_ink: Vec4f,
    #[rust] motion: Vec<RowMotion>,
    #[rust] next_frame: NextFrame,
    #[rust] drawn_strip: Option<DrawnStrip>,
    /// Last `(pending_cue_rev, desired_cue)` announced from `draw_walk`, preventing duplicate
    /// actions until reconciled while re-arming immediately on any `sync_pending_approval` change.
    #[rust] pending_announced: Option<(u64, Option<(Option<Id>, Id)>)>,
    #[rust] pressed_target: Option<PressedChatTarget>,
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

const FADE_MS: u64 = 200;

enum RowMotion {
    Spin { mark: WidgetRef, until: u64 },
    Fade { label: WidgetRef, ink: Vec4f, at: u64 },
}

fn turn(now: u64) -> f32 { (now % 1000) as f32 / 1000. }

fn fading(mut ink: Vec4f, at: u64, now: u64) -> Vec4f {
    ink.w *= now.saturating_sub(at).min(FADE_MS) as f32 / FADE_MS as f32;
    ink
}

impl RowMotion {
    fn visible(&self, cx: &Cx) -> bool {
        let widget = match self { Self::Spin { mark, .. } => mark, Self::Fade { label, .. } => label };
        let area = widget.area();
        if !area.is_valid(cx) { return false }
        let size = area.clipped_rect(cx).size;
        size.x > 0. && size.y > 0.
    }

    fn advance(&self, cx: &mut Cx, now: u64) -> bool {
        if !self.visible(cx) { return false }
        match self {
            Self::Spin { mark, until } => {
                if let Some(mut view) = mark.borrow_mut::<View>() {
                    view.draw_bg.set_instance_on_area(cx, id!(turn), &[turn(now.min(*until))]);
                }
                now < *until
            }
            Self::Fade { label, ink, at } => {
                if let Some(mut label) = label.borrow_mut::<Label>() {
                    label.set_text_color(cx, fading(*ink, *at, now));
                }
                now.saturating_sub(*at) < FADE_MS
            }
        }
    }
}

impl AgentChatList {
    fn draw_traces(&mut self, cx: &mut Cx2d, list: &mut FlatList, message: &Message, now: u64) {
        let mut slot = 0;
        let mut item = |cx: &mut Cx, template: LiveId| {
            slot += 1;
            list.item(cx, LiveId::from_num(template.0, slot), template)
        };
        for (index, row) in message.traces.iter().enumerate() {
            let template = match row.phase {
                Phase::Sent => id!(Sent),
                Phase::GotIt => id!(Receipt),
                Phase::Working | Phase::Quiet | Phase::NoSign => id!(Work),
                Phase::NotPickedUp | Phase::Away => id!(Warning),
                Phase::Done => id!(Finished),
            };
            if let Some(head) = item(cx, template) {
                if template == id!(Work) || template == id!(Receipt) {
                    head.label(cx, ids!(caption)).set_text(cx, &row.headline);
                } else { head.set_text(cx, &row.headline); }
                if template == id!(Work) {
                    let mark = head.widget(cx, ids!(spinner));
                    if let Some(mut view) = mark.borrow_mut::<View>() {
                        view.draw_bg.set_dyn_instance(cx, id!(turn), &[row.spin_until.map_or(0., |_| turn(now))]);
                    }
                    if let Some(until) = row.spin_until.filter(|end| now < *end) {
                        self.motion.push(RowMotion::Spin { mark, until });
                    }
                }
                head.draw_all_unscoped(cx);
            }
            if row.fold_count != 0 {
                if let Some(button) = item(cx, id!(Fold)) {
                    let caret = if row.expanded { "▾" } else { "▸" };
                    button.set_text(cx, &format!("{caret} {}", row.fold_label()));
                    button.set_action_data(TraceFold { line: message.id, trace: index, history: row.phase.finished() });
                    button.draw_all_unscoped(cx);
                }
            }
            for line in &row.output {
                let Some(label) = item(cx, id!(Output)) else { continue };
                let ink = match line.ink { Ink::Action => self.action_ink, Ink::Status => self.status_ink, Ink::Warning => self.warning_ink };
                let fade = !row.phase.finished() && now.saturating_sub(line.at) < FADE_MS;
                label.set_text(cx, &single_line(&line.text));
                if let Some(mut inner) = label.borrow_mut::<Label>() {
                    inner.draw_text.color = if fade { fading(ink, line.at, now) } else { ink };
                }
                label.draw_all_unscoped(cx);
                if fade { self.motion.push(RowMotion::Fade { label, ink, at: line.at }); }
            }
        }
        list.items.retain_visible();
    }
}

impl Widget for AgentChatList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else {
                continue;
            };
            let Some(data) = scope.data.get_mut::<crate::lists::ViewData>() else { continue };
            self.motion.clear();
            let now = wall_ms();
            let inks = RowInks { mention: self.mention_ink, outline: self.outline_ink, line: self.line_ink };
            let mut drawn_strip = self.drawn_strip;
            draw_visible_rows(cx, &mut list, data, inks, &mut drawn_strip, |cx, rows, message| {
                self.draw_traces(cx, rows, message, now);
            });
            self.drawn_strip = drawn_strip;
        }
        let (desired_cue, current_cue, cue_rev) = match scope.data.get::<ViewData>() {
            Some(data) => {
                let desired = data.chat_visible()
                    .then(|| data.workspace.connect.first_pending())
                    .flatten()
                    .and_then(|(row_hint, line, _)| {
                        let list = self.view.portal_list(cx, ids!(list));
                        let item_id = data.pending_approval_item_id(row_hint, line)?;
                        let on_screen = item_on_screen(&list, item_id)?;
                        (!on_screen).then_some((data.tree.snapshot().town, line))
                    });
                (desired, data.chat_read.pending_cue_shown, data.chat_read.pending_cue_rev)
            }
            None => (None, None, 0),
        };
        if desired_cue != current_cue && self.pending_announced != Some((cue_rev, desired_cue)) {
            self.pending_announced = Some((cue_rev, desired_cue));
            cx.widget_action(self.widget_uid(), PendingApprovalVisibility);
        } else if desired_cue == current_cue {
            self.pending_announced = None;
        }
        // PortalList can measure rows beyond the viewport. Its final placement and clip
        // determine whether any of these marks were actually drawn on screen.
        self.motion.retain(|motion| motion.visible(cx));
        if !self.motion.is_empty() && !cx.next_frame_is_pending(self.next_frame) { self.next_frame = cx.new_next_frame(); }
        DrawStep::done()
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.next_frame.is_event(event).is_some() {
            let now = wall_ms();
            self.motion.retain(|motion| motion.advance(cx, now));
            if !self.motion.is_empty() { self.next_frame = cx.new_next_frame(); }
        }
        // A fresh text gesture takes the place of the selected message. A context
        // press can keep both, so only an actual change of the text range counts.
        let watch = matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::KeyDown(_) | Event::NextFrame(_))
            && scope.data.get::<ViewData>().is_some_and(|data| data.chat_selected.is_some());
        let scroll_up_key = matches!(
            event,
            Event::KeyDown(k) if matches!(k.key_code, KeyCode::Home | KeyCode::PageUp | KeyCode::ArrowUp)
        );
        let end_key = matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::End);
        let scroll_watch = scroll_up_key
            || end_key
            || matches!(
                event,
                Event::Scroll(_) | Event::TouchUpdate(_) | Event::MouseDown(_) | Event::MouseMove(_) | Event::NextFrame(_)
            );
        let resolve_gestures = matches!(
            event,
            Event::MouseDown(_) | Event::MouseUp(_) | Event::TouchUpdate(_) | Event::LongPress(_)
        );
        let list = (watch || scroll_watch || resolve_gestures)
            .then(|| self.view.portal_list(cx, ids!(list)));
        let before = list.as_ref().filter(|_| watch).and_then(text_range);
        let scroll_before = list.as_ref().filter(|_| scroll_watch).map(|list| {
            let scrollable = list.borrow().is_some_and(|l| l.is_scrollable());
            (scrollable, list.user_scroll_travel(), (list.first_id(), list.scroll_position()))
        });
        if let Event::FingerCancel(e) = event {
            if cx.fingers.press_taken_away(e.digit_id)
                && self.pressed_target.is_some_and(|p| p.digit == e.digit_id)
            {
                self.pressed_target = None;
            }
        }
        if resolve_gestures {
            let captured_before = list.as_ref().and_then(|list| list_captured_digit(cx, list));
            let mut actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
            if let (Some(list), Some(data)) = (list.as_ref(), scope.data.get::<ViewData>()) {
                let pressed = match event {
                    Event::MouseDown(e) => {
                        if e.button.is_primary() {
                            let mouse: DigitId = live_id!(mouse).into();
                            if captured_before.is_none_or(|d| d == mouse)
                                && list_captured_digit(cx, list) == Some(mouse)
                            {
                                self.pressed_target =
                                    Some(resolve_press_target(cx, list, data, mouse, e.abs));
                            }
                        }
                        None
                    }
                    Event::MouseUp(e) if e.button.is_primary() => {
                        let mouse: DigitId = live_id!(mouse).into();
                        if self.pressed_target.is_some_and(|p| p.digit == mouse) {
                            self.pressed_target.take()
                        } else {
                            None
                        }
                    }
                    Event::LongPress(e) => {
                        let digit: DigitId = live_id_num!(touch, e.uid).into();
                        if self.pressed_target.is_some_and(|p| p.digit == digit) {
                            self.pressed_target.take()
                        } else {
                            None
                        }
                    }
                    Event::TouchUpdate(e) => {
                        if let Some(digit) = list_captured_digit(cx, list) {
                            if captured_before.is_none_or(|d| d == digit) {
                                if let Some(t) = e.touches.iter().find(|t| {
                                    t.state == TouchState::Start
                                        && DigitId::from(live_id_num!(touch, t.uid)) == digit
                                }) {
                                    self.pressed_target =
                                        Some(resolve_press_target(cx, list, data, digit, t.abs));
                                }
                            }
                        }
                        let ended = self.pressed_target.is_some_and(|p| {
                            e.touches.iter().any(|t| {
                                matches!(t.state, TouchState::Stop | TouchState::Cancel)
                                    && DigitId::from(live_id_num!(touch, t.uid)) == p.digit
                            })
                        });
                        if ended { self.pressed_target.take() } else { None }
                    }
                    _ => None,
                };
                if !actions.is_empty() {
                    stamp_chat_gestures(cx, list, data, pressed, &mut actions);
                }
            }
            if !actions.is_empty() {
                cx.extend_actions(actions);
            }
        } else {
            self.view.handle_event(cx, event, scope);
        }
        if let Some(list) = list {
            if watch {
                let after = text_range(&list);
                if after.is_some() && after != before {
                    scope.data.get_mut::<ViewData>().unwrap().chat_selected = None;
                    list.redraw(cx);
                }
            }
            if let Some((scrollable, travel_before, pos_before)) = scroll_before {
                let pos_after = (list.first_id(), list.scroll_position());
                let moved_up = pos_after.0 < pos_before.0
                    || (pos_after.0 == pos_before.0 && pos_after.1 > pos_before.1);
                let moved_down = pos_after.0 > pos_before.0
                    || (pos_after.0 == pos_before.0 && pos_after.1 < pos_before.1);
                if scrollable && moved_up && (list.user_scroll_travel() > travel_before || scroll_up_key) {
                    if let Some(data) = scope.data.get_mut::<ViewData>() {
                        data.chat_read.mark_user_scrolled_up();
                    }
                } else if end_key && moved_down && list.is_at_end() {
                    if let Some(data) = scope.data.get_mut::<ViewData>() {
                        if !data.chat_unread() {
                            data.chat_read.mark_at_tail();
                        }
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ClipRow {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] line: Option<Arc<ClipLine>>,
    #[rust] playing: bool,
    #[rust] alias: Option<PlaybackLink>,
}

// A native URL resolver can require a filename extension. Try the real data file
// first. Only an MP4 file signature permits retrying with the fixed .mp4 suffix.
struct PlaybackLink(std::path::PathBuf);

impl PlaybackLink {
    fn mp4(path: &std::path::Path) -> Option<Self> {
        use std::{fs, io::Read};
        let mut head = [0; 8];
        fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
        if &head[4..8] != b"ftyp" { return None }
        let dir = path.parent()?.join("playback");
        fs::create_dir_all(&dir).ok()?;
        let link = dir.join(path.file_name()?).with_extension("mp4");
        // A crash can leave a previous playback link in this exclusively opened home.
        if let Err(e) = fs::remove_file(&link) {
            if e.kind() != std::io::ErrorKind::NotFound { return None }
        }
        fs::hard_link(path, &link).ok()?;
        Some(Self(link))
    }
}

impl Drop for PlaybackLink {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0) { log!("town: removing clip playback link: {e}"); }
    }
}

impl Widget for ClipRow {
    /// The row is not a control; its buttons are, and the list still finds them.
    fn is_interactive(&self) -> bool { false }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if self.playing && matches!(event, Event::VideoDecodingError(_)) {
            let player = self.view.video_player(cx, ids!(player));
            if player.video(cx).last_error().is_some() {
                if self.alias.is_none() {
                    self.alias = self.line.as_ref().and_then(|c| c.media.as_ref())
                        .and_then(|m| m.playable.as_deref()).and_then(PlaybackLink::mp4);
                    if let Some(path) = self.alias.as_ref().and_then(|link| link.0.to_str()) {
                        player.open(cx, path);
                        return
                    }
                }
                self.stop(cx);
                self.view.widget(cx, ids!(failure)).set_visible(cx, true);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ClipRow {
    fn set_can_pin(&self, cx: &mut Cx, can_pin: bool) {
        let button = self.view.button(cx, ids!(pin));
        let clip = self.line.as_ref().filter(|c| can_pin && c.media.is_some());
        button.set_visible(cx, clip.is_some());
        if let Some(clip) = clip { button.set_action_data(ChatCommand::PinClip(clip.id)); }
    }

    fn bind(&mut self, cx: &mut Cx, line: &Arc<ClipLine>) {
        if self.line.as_ref().is_some_and(|old| Arc::ptr_eq(old, line)) { return }
        let old = self.line.replace(line.clone());
        let previous = old.as_ref().and_then(|c| c.media.as_ref());
        let media = line.media.as_ref();
        if old.as_ref().map(|c| c.id) != Some(line.id)
            || previous.map(|m| m.is_picture) != media.map(|m| m.is_picture)
            || previous.and_then(|m| m.playable.as_ref()) != media.and_then(|m| m.playable.as_ref()) {
            self.stop(cx);
            self.view.widget(cx, ids!(failure)).set_visible(cx, false);
        }
        let poster = media.and_then(|m| m.poster.as_deref());
        if previous.and_then(|m| m.poster.as_deref()) != poster {
            let image = self.view.image(cx, ids!(poster));
            image.set_texture(cx, None);
            if let Some(path) = poster { let _ = image.load_image_file_by_path_async(cx, path); }
        }
        // A picture needs no file name; a movie or a removed clip keeps its line.
        let picture = media.is_some_and(|m| m.is_picture);
        let title = self.view.label(cx, ids!(clip_title));
        title.set_visible(cx, !picture);
        title.set_text(cx, &line.title);
        self.view.widget(cx, ids!(media)).set_visible(cx, media.is_some());
        let Some(media) = media else { return };
        if previous.is_none_or(|old| old.height != media.height) {
            self.view.view(cx, ids!(frame)).set_walk(cx, Walk::fixed(240., media.height));
        }
        self.view.widget(cx, ids!(duration_badge)).set_visible(cx, !media.is_picture);
        self.view.label(cx, ids!(length)).set_text(cx, &media.length);
        self.view.widget(cx, ids!(details)).set_visible(cx, !media.is_picture);
        self.view.widget(cx, ids!(progress)).set_visible(cx, media.is_picture && media.playable.is_none());
        self.view.label(cx, ids!(progress_line)).set_text(cx, &media.state);
        self.view.label(cx, ids!(size)).set_text(cx, &media.size);
        self.view.label(cx, ids!(state)).set_text(cx, &media.state);
        self.view.widget(cx, ids!(get)).set_visible(cx, media.get);
        self.controls(cx);
    }

    fn controls(&self, cx: &mut Cx) {
        let playable = self.line.as_ref().and_then(|c| c.media.as_ref()).is_some_and(|m| !m.is_picture && m.playable.is_some());
        self.view.widget(cx, ids!(cover)).set_visible(cx, !self.playing);
        self.view.widget(cx, ids!(player)).set_visible(cx, self.playing);
        self.view.widget(cx, ids!(close_clip)).set_visible(cx, self.playing);
        self.view.widget(cx, ids!(play)).set_visible(cx, playable && !self.playing);
    }

    fn stop(&mut self, cx: &mut Cx) {
        if self.playing { self.view.video_player(cx, ids!(player)).stop(cx); }
        self.playing = false;
        self.alias = None;
        self.controls(cx);
    }
}

#[derive(Default)]
pub struct ClipPlayback {
    active: Option<ClipRowRef>,
}

impl ClipPlayback {
    pub fn close(&mut self, cx: &mut Cx) {
        if let Some(row) = self.active.take() {
            if let Some(mut row) = row.borrow_mut() { row.stop(cx); }
        }
    }

    /// A deletion or a change to the source also stops a row scrolled out of view.
    pub fn reconcile(&mut self, cx: &mut Cx, workspace: &crate::view::Workspace) {
        let Some(active) = &self.active else { return };
        let still_playable = active.borrow().is_some_and(|row|
            row.playing && row.line.as_ref().is_some_and(|old| workspace.still_playable(old)));
        if !still_playable { self.close(cx); }
    }

    fn play(&mut self, cx: &mut Cx, target: ClipRowRef, path: &std::path::Path) {
        self.close(cx);
        let Some(mut row) = target.borrow_mut() else { return };
        let Some(path) = path.to_str() else {
            row.view.widget(cx, ids!(failure)).set_visible(cx, true);
            return
        };
        row.playing = true;
        row.view.widget(cx, ids!(failure)).set_visible(cx, false);
        row.controls(cx);
        row.view.video_player(cx, ids!(player)).open(cx, path);
        drop(row);
        self.active = Some(target);
    }

    /// Use the current snapshot to authorize the action on the row that emitted the click.
    pub fn actions(&mut self, cx: &mut Cx, actions: &Actions, list: &PortalListRef, rows: &[crate::view::ChatRow])
        -> Option<(ClipId, makepad_town_core::event::Id)> {
        for (index, item) in list.items_with_actions(actions) {
            let Some(crate::view::ChatRow::Clip(line)) = rows.get(index) else { continue };
            if item.borrow::<ClipRow>().is_none_or(|r| r.line.as_ref().is_none_or(|c| c.id != line.id)) { continue }
            let Some(media) = &line.media else { continue };
            if media.get && item.button(cx, ids!(get)).clicked(actions) { return Some((line.id, media.blob)) }
            if item.button(cx, ids!(close_clip)).clicked(actions) { self.close(cx); break }
            if !media.is_picture && item.button(cx, ids!(play)).clicked(actions) {
                if let Some(path) = &media.playable { self.play(cx, item.as_clip_row(), path); }
                break
            }
        }
        None
    }
}

/// The person's recent emoji. A missing or damaged file means none.
pub fn load_recent_emoji(home: &Home) -> Vec<String> {
    let Ok(bytes) = home.files().read_capped(&home.recent_emoji_path(), 4096) else { return Vec::new() };
    let Ok(text) = std::str::from_utf8(&bytes) else { return Vec::new() };
    text.lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from).collect()
}

fn save_recent_emoji(home: &Home, recents: &[String]) -> std::io::Result<()> {
    home.files().replace_rebuildable(&home.recent_emoji_path(), format!("{}\n", recents.join("\n")).into_bytes())
}

pub fn mention_hint_text(target: &MentionTarget) -> Option<String> {
    Some(match target {
        MentionTarget::None => return None,
        MentionTarget::YourAi { has_ai: true, label, .. } => {
            let display = label.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Your AI");
            format!("Goes to {display}")
        }
        MentionTarget::YourAi { has_ai: false, .. } => "Waits until you add an AI".into(),
        MentionTarget::PeerAi { owner_name, label, .. } => {
            let display = label.trim();
            let target = if display.is_empty() { format!("{owner_name}'s AI") } else { display.to_owned() };
            format!("Goes to {target} ({owner_name} approves first)")
        }
        MentionTarget::UnboundAi => "Not sent to an AI — pick one from @ or use @ai for yours".into(),
        MentionTarget::People(seats) => match seats.as_slice() {
            [] => return None,
            [one] => format!("Mentions {}", one.name),
            [first, second] => format!("Mentions {} and {}", first.name, second.name),
            [first, second, others @ ..] => {
                let n = others.len();
                format!("Mentions {}, {} and {n} other{}", first.name, second.name, if n == 1 { "" } else { "s" })
            }
        },
    })
}

/// The composer's emoji picker: a pick goes into the draft at its cursor, in place of any
/// selection, and the picker's recents are saved in the person's home folder.
pub fn composer_emoji_actions(cx: &mut Cx, conversation: &WidgetRef, actions: &Actions, home: Option<&Home>) {
    let picker = conversation.emoji_picker(cx, ids!(emoji_picker));
    let Some(emoji) = picker.picked(actions) else { return };
    let input = conversation.text_input(cx, ids!(chat_input));
    let selection = input.selection();
    if let Err(e) = input.replace_range(cx, selection.start().index..selection.end().index, &emoji, UndoGroup::New) {
        log!("town: inserting an emoji: {e:?}");
    }
    if let Some(home) = home {
        if let Err(e) = save_recent_emoji(home, &picker.recents()) { log!("town: saving recent emoji: {e}"); }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageAction { Copy, CopySelection, Reply, Delete, Open }

impl MessageAction {
    fn id(self) -> LiveId {
        match self { Self::Copy => id!(copy), Self::CopySelection => id!(copy_selection), Self::Reply => id!(reply), Self::Delete => id!(delete), Self::Open => id!(open) }
    }

    fn label(self) -> &'static str {
        match self { Self::Copy => "Copy", Self::CopySelection => "Copy selection", Self::Reply => "Reply", Self::Delete => "Delete", Self::Open => "Open" }
    }

    /// What the menu says under the action before it is chosen.
    fn hint(self) -> &'static str {
        match self {
            Self::Delete => "Removed from view for everyone.\nThe hidden text stays in each computer's stored history.",
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuoteJump {
    town: Option<Id>,
    source: Id,
    target: Id,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatCommand {
    Reply(Id),
    Delete(Id),
    OpenPicture(ClipId, PathBuf),
    PinClip(ClipId),
    JumpQuote { source: Id, target: Id },
}

#[derive(Clone, Copy)]
enum ChatTarget<'a> {
    Message(&'a Message),
    Picture(ClipId, Option<&'a Path>),
}

impl<'a> ChatTarget<'a> {
    fn from_row(row: &'a ChatRow) -> Option<Self> {
        match row {
            ChatRow::Message(m) => Some(Self::Message(m)),
            ChatRow::Clip(c) => c.media.as_ref().filter(|m| m.is_picture)
                .map(|m| Self::Picture(c.id, m.playable.as_deref())),
            _ => None,
        }
    }

    fn by_id(rows: &'a [ChatRow], id: Id) -> Option<Self> {
        rows.iter().rev().filter_map(Self::from_row).find(|row| row.id() == id)
    }

    fn id(self) -> Id {
        match self { Self::Message(m) => m.id, Self::Picture(id, _) => id.1 }
    }

    fn command(self, action: MessageAction) -> Option<ChatCommand> {
        match (self, action) {
            (Self::Message(m), MessageAction::Reply) => Some(ChatCommand::Reply(m.id)),
            (Self::Message(m), MessageAction::Delete) => Some(ChatCommand::Delete(m.id)),
            (Self::Picture(id, Some(path)), MessageAction::Open) => Some(ChatCommand::OpenPicture(id, path.to_owned())),
            _ => None,
        }
    }
}

fn message_actions(target: ChatTarget<'_>, has_text: bool, writable: bool) -> impl Iterator<Item = MessageAction> {
    use MessageAction::*;
    match target {
        ChatTarget::Message(m) => [Some(Copy), has_text.then_some(CopySelection), writable.then_some(Reply), m.deletable.then_some(Delete)],
        ChatTarget::Picture(_, openable) => [openable.map(|_| Open), None, None, None],
    }.into_iter().flatten()
}

/// Whether `item_id` intersected `list`'s visible viewport in its last draw.
/// Reads `PortalList`'s retained `drawn_slot` and `scroll_bar.get_scroll_view_visible()`
/// (neither depends on `Cx` or draw-list `redraw_id`).
/// Returns `None` if `list` has not drawn its current range yet.
fn item_on_screen(list: &PortalListRef, item_id: usize) -> Option<bool> {
    let inner = list.borrow()?;
    let viewport_h = inner.scroll_bar.get_scroll_view_visible();
    if viewport_h <= 0.0 || inner.items().is_empty() {
        return None;
    }
    Some(inner.drawn_slot(item_id).is_some_and(|slot| {
        slot.size > 0.0 && slot.start + slot.size > 0.0 && slot.start < viewport_h
    }))
}

/// Maximum row distance for a smooth quote-jump scroll; matches `PortalList`'s `max_items_to_show`
/// so `smooth_scroll_to` never teleports `first_id` with a stale `first_scroll`.
const JUMP_SMOOTH_MAX_ROWS: usize = 20;
/// Top margin (in logical px) left above a jumped-to row when it is not row 0.
const JUMP_TOP_OFFSET: f64 = 24.0;
/// Edge slack (in logical px) matching `PortalList`'s `VIEWPORT_END_TOLERANCE`.
const JUMP_VIEW_SLACK: f64 = 1.0;

/// Whether `item_id` is sufficiently inside `list`'s viewport to skip scrolling on a jump:
/// its full height lies in `[0, viewport_h]` (within `JUMP_VIEW_SLACK`), or—for a row taller
/// than the viewport—its top lies in `[0, JUMP_TOP_OFFSET]` (within `JUMP_VIEW_SLACK`, and
/// strictly above `viewport_h`). Returns `None` if `list` has not drawn its current range yet.
fn item_in_view(list: &PortalListRef, item_id: usize) -> Option<bool> {
    let inner = list.borrow()?;
    let viewport_h = inner.scroll_bar.get_scroll_view_visible();
    if viewport_h <= 0.0 || inner.items().is_empty() {
        return None;
    }
    Some(inner.drawn_slot(item_id).is_some_and(|slot| {
        if slot.size <= 0.0 {
            return false;
        }
        let top_ok = slot.start >= -JUMP_VIEW_SLACK;
        if slot.size <= viewport_h {
            top_ok && slot.start + slot.size <= viewport_h + JUMP_VIEW_SLACK
        } else {
            top_ok && slot.start < viewport_h && slot.start <= JUMP_TOP_OFFSET + JUMP_VIEW_SLACK
        }
    }))
}

fn tail_draw_pending(list: &PortalListRef) -> bool {
    list.borrow().is_some_and(|inner| {
        inner.drawn_slot(inner.first_id()).is_none()
            || inner.range_end().checked_sub(1).is_some_and(|last| inner.drawn_slot(last).is_none())
    })
}

fn button_clicked_for<T: ActionTrait + PartialEq>(
    button: &ButtonRef,
    actions: &Actions,
    expected: &T,
) -> bool {
    if button.is_empty() {
        return false;
    }
    actions.filter_widget_actions(button.widget_uid()).any(|item| {
        matches!(item.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))
            && item.data.as_ref().and_then(|d| d.downcast_ref::<T>()) == Some(expected)
    })
}

fn hit_unread_line(cx: &Cx, list: &PortalListRef, item_id: usize, abs: DVec2) -> bool {
    let Some((template, item)) = list.get_item(item_id) else { return false };
    if template != id!(Line) && template != id!(Addressed) { return false }
    let line = item.widget(cx, ids!(unread_line));
    line.visible() && line.point_hits_area(cx, abs)
}

#[derive(Clone, Copy, Debug)]
struct ResolvedChatTarget {
    town: Option<Id>,
    target: Option<Id>,
}

fn list_captured_digit(cx: &Cx, list: &PortalListRef) -> Option<DigitId> {
    let area = list.area();
    (!area.is_empty()).then(|| cx.fingers.find_digit_for_captured_area(area)).flatten()
}

fn resolve_item_target(
    cx: &Cx,
    list: &PortalListRef,
    data: &ViewData,
    item_id: usize,
    abs: DVec2,
) -> Option<Id> {
    (!hit_unread_line(cx, list, item_id, abs))
        .then(|| data.workspace.rows.get(item_id))
        .flatten()
        .and_then(ChatTarget::from_row)
        .map(ChatTarget::id)
}

fn resolve_press_target(
    cx: &Cx,
    list: &PortalListRef,
    data: &ViewData,
    digit: DigitId,
    abs: DVec2,
) -> PressedChatTarget {
    let target = list.item_at(cx, abs)
        .and_then(|item_id| resolve_item_target(cx, list, data, item_id, abs));
    PressedChatTarget { digit, town: data.tree.snapshot().town, target }
}

fn stamp_chat_gestures(
    cx: &Cx,
    list: &PortalListRef,
    data: &ViewData,
    pressed: Option<PressedChatTarget>,
    actions: &mut [Action],
) {
    let list_uid = list.widget_uid();
    let open_town = data.tree.snapshot().town;
    for action in actions {
        let Some(wa) = action.downcast_mut::<WidgetAction>() else { continue };
        if wa.widget_uid != list_uid { continue }
        let stamp = match wa.action.downcast_ref::<PortalListAction>() {
            Some(&PortalListAction::ItemClicked { item_id, abs, .. }) => match pressed {
                Some(pressed) => ResolvedChatTarget { town: pressed.town, target: pressed.target },
                None => ResolvedChatTarget {
                    town: open_town,
                    target: item_id.and_then(|i| resolve_item_target(cx, list, data, i, abs)),
                },
            },
            Some(&PortalListAction::ItemContextMenu { item_id, abs }) => match pressed {
                Some(pressed) => ResolvedChatTarget { town: pressed.town, target: pressed.target },
                None => ResolvedChatTarget {
                    town: open_town,
                    target: resolve_item_target(cx, list, data, item_id, abs),
                },
            },
            _ => continue,
        };
        wa.data = Some(Arc::new(stamp));
    }
}

fn text_range(list: &PortalListRef) -> Option<((usize, usize), (usize, usize))> {
    list.borrow()?.get_selection_range().filter(|(a, b)| a != b)
}

fn message_text(message: &Message) -> String {
    format!("{} {}\n{}", message.name, message.time, message.text)
}

/// Ascending-consumption slot table for keyed reconciliation.
/// Real timelines have unique keys (staying in `One`, zero inner `Vec` allocations).
/// Synthetic duplicate keys store indices in descending order so `pop()` yields the
/// smallest remaining index `>= min` in amortized O(1).
enum KeySlots {
    Empty,
    One(usize),
    Many(Vec<usize>),
}

impl KeySlots {
    fn push(&mut self, index: usize) {
        match self {
            Self::Empty => *self = Self::One(index),
            Self::One(first) => *self = Self::Many(vec![*first, index]),
            Self::Many(indices) => indices.push(index),
        }
    }

    fn take_at_least(&mut self, min: usize) -> Option<usize> {
        match self {
            Self::Empty => None,
            Self::One(index) => {
                let i = *index;
                *self = Self::Empty;
                (i >= min).then_some(i)
            }
            Self::Many(indices) => {
                while indices.last().is_some_and(|&i| i < min) {
                    indices.pop();
                }
                indices.pop()
            }
        }
    }
}

impl ViewData {
    fn sync_chat_rows_inner(&mut self, cx: &mut Cx, list: &mut PortalList) -> usize {
        let rows = &self.workspace.rows;
        let open_town = self.tree.snapshot().town;
        let old_len = list.range_end();
        let town_switched = open_town.is_some()
            && self.chat_read.rendered_town.is_some_and(|t| Some(t) != open_town);
        if old_len == usize::MAX {
            list.set_item_range(cx, 0, rows.len());
            self.chat_read.rendered_rows.clear();
            self.chat_read.rendered_rows.extend(rows.iter().map(ChatRow::key));
            self.chat_read.rendered_rev = self.workspace.rows_rev;
            self.chat_read.rendered_town = open_town;
            return 0;
        }
        if town_switched || self.chat_read.rendered_rows.len() != old_len {
            let splices = if old_len > 0 {
                list.splice_items(cx, 0, old_len, rows.len());
                1
            } else {
                list.set_item_range(cx, 0, rows.len());
                0
            };
            self.chat_read.rendered_rows.clear();
            self.chat_read.rendered_rows.extend(rows.iter().map(ChatRow::key));
            self.chat_read.rendered_rev = self.workspace.rows_rev;
            self.chat_read.rendered_town = open_town;
            return splices;
        }
        let old = &mut self.chat_read.rendered_rows;
        // 1. Common-prefix fast path.
        let mut prefix = 0;
        while prefix < old.len() && prefix < rows.len() && old[prefix] == rows[prefix].key() {
            prefix += 1;
        }
        if prefix == old.len() && prefix == rows.len() {
            self.chat_read.rendered_rev = self.workspace.rows_rev;
            self.chat_read.rendered_town = open_town;
            return 0;
        }
        // 2. Pure tail-append fast path: zero splice_items / HeightTree::splice.
        if prefix == old.len() {
            list.set_item_range(cx, 0, rows.len());
            old.extend(rows[prefix..].iter().map(ChatRow::key));
            self.chat_read.rendered_rev = self.workspace.rows_rev;
            self.chat_read.rendered_town = open_town;
            return 0;
        }
        // 3. Pure tail-removal fast path: splice so item_press, selection, widgets, and surviving heights stay exact.
        if prefix == rows.len() {
            list.splice_items(cx, prefix, old.len() - prefix, 0);
            old.truncate(prefix);
            self.chat_read.rendered_rev = self.workspace.rows_rev;
            self.chat_read.rendered_town = open_town;
            return 1;
        }
        // 4. Common-suffix trim: single middle retraction or single middle insertion uses 1 splice and 0 key-matching allocs.
        let mut old_end = old.len();
        let mut new_end = rows.len();
        while old_end > prefix && new_end > prefix && old[old_end - 1] == rows[new_end - 1].key() {
            old_end -= 1;
            new_end -= 1;
        }
        if new_end == prefix {
            list.splice_items(cx, prefix, old_end - prefix, 0);
            old.drain(prefix..old_end);
            self.chat_read.rendered_rev = self.workspace.rows_rev;
            self.chat_read.rendered_town = open_town;
            return 1;
        }
        if old_end == prefix {
            list.splice_items(cx, prefix, 0, new_end - prefix);
            old.splice(prefix..prefix, rows[prefix..new_end].iter().map(ChatRow::key));
            self.chat_read.rendered_rev = self.workspace.rows_rev;
            self.chat_read.rendered_town = open_town;
            return 1;
        }
        // 5. Strictly linear keyed middle diff over old[prefix..old_end] vs rows[prefix..new_end].
        // Loop invariant: PortalList holds rows[0..prev_ni] ++ old[prev_oi..old.len()].
        let new_mid: Vec<ChatRowKey> = rows[prefix..new_end].iter().map(ChatRow::key).collect();
        let mut positions: std::collections::HashMap<ChatRowKey, KeySlots> =
            std::collections::HashMap::with_capacity(new_mid.len());
        for (offset, &key) in new_mid.iter().enumerate().rev() {
            positions.entry(key).or_insert(KeySlots::Empty).push(prefix + offset);
        }
        let mut splices = 0usize;
        let mut prev_oi = prefix;
        let mut prev_ni = prefix;
        for oi in prefix..old_end {
            let Some(slots) = positions.get_mut(&old[oi]) else { continue };
            let Some(ni) = slots.take_at_least(prev_ni) else { continue };
            let removed = oi - prev_oi;
            let added = ni - prev_ni;
            if removed > 0 || added > 0 {
                list.splice_items(cx, prev_ni, removed, added);
                splices += 1;
            }
            prev_oi = oi + 1;
            prev_ni = ni + 1;
        }
        let tail_removed = old_end - prev_oi;
        let tail_added = new_end - prev_ni;
        if tail_removed > 0 || (tail_added > 0 && old_end < old.len()) {
            list.splice_items(cx, prev_ni, tail_removed, tail_added);
            splices += 1;
        } else if tail_added > 0 {
            list.set_item_range(cx, 0, rows.len());
        }
        old.splice(prefix..old_end, new_mid);
        self.chat_read.rendered_rev = self.workspace.rows_rev;
        self.chat_read.rendered_town = open_town;
        splices
    }

    pub fn sync_chat_rows(&mut self, cx: &mut Cx, list: &PortalListRef) {
        if let Some(mut inner) = list.borrow_mut() {
            let _ = self.sync_chat_rows_inner(cx, &mut inner);
        }
    }

    #[cfg(test)]
    fn sync_chat_rows_counting(&mut self, cx: &mut Cx, list: &PortalListRef) -> usize {
        list.borrow_mut().map_or(0, |mut inner| self.sync_chat_rows_inner(cx, &mut inner))
    }

    fn message_at(&self, index: usize) -> Option<&Message> {
        match self.workspace.rows.get(index)? { ChatRow::Message(m) => Some(m), _ => None }
    }

    fn selected_target(&self) -> Option<ChatTarget<'_>> {
        ChatTarget::by_id(&self.workspace.rows, self.chat_selected?)
    }

    pub fn retain_chat_selection(&mut self) {
        if self.selected_target().is_none() { self.chat_selected = None; }
    }

    fn copy_text(&self, action: MessageAction, list: &PortalListRef) -> Option<String> {
        if action == MessageAction::Copy { return match self.selected_target()? {
            ChatTarget::Message(m) => Some(message_text(m)), _ => None,
        } }
        let ((first, start), (last, _)) = text_range(list)?;
        let mut text = list.selected_text_with(&|i| self.message_at(i).map(message_text));
        if text.is_empty() { return None }
        // Words copied from one message are just those words. A copy across messages keeps
        // the first speaker's attribution when the drag starts inside their words; a range
        // starting at the end of a row contains none of that row.
        if let Some(m) = self.message_at(first).filter(|_| first != last) {
            let header = format!("{} {}\n", m.name, m.time);
            if start != 0 && start < header.len() + m.text.len() {
                text.insert_str(0, header.get(..start.min(header.len()))?);
            }
        }
        Some(text)
    }

    pub fn chat_actions(&mut self, cx: &mut Cx, list: &PortalListRef, actions: &Actions) -> Option<ChatCommand> {
        if self.chat_visible() {
            for action in actions.iter().filter_map(|action| action.as_widget_action()) {
                if !matches!(action.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) { continue }
                if let Some(ChatCommand::PinClip(id)) = action.data.as_ref().and_then(|d| d.downcast_ref::<ChatCommand>()) {
                    let tree = self.tree.snapshot();
                    if tree.town == Some(id.0) && !tree.projects.is_empty() && self.workspace.writable
                        && self.workspace.rows.iter().any(|row| matches!(row, ChatRow::Clip(c) if c.id == *id && c.media.is_some())) {
                        return Some(ChatCommand::PinClip(*id))
                    }
                }
                let Some(jump) = action.data.as_ref().and_then(|data| data.downcast_ref::<QuoteJump>()) else { continue };
                if jump.town == self.tree.snapshot().town {
                    return Some(ChatCommand::JumpQuote { source: jump.source, target: jump.target })
                }
            }
        }
        let list_uid = list.widget_uid();
        for action in actions {
            let Some(wa) = action.as_widget_action().filter(|w| w.widget_uid == list_uid) else { continue };
            let Some(gesture) = wa.action.downcast_ref::<PortalListAction>() else { continue };
            if !matches!(gesture, PortalListAction::ItemClicked { .. } | PortalListAction::ItemContextMenu { .. }) {
                continue;
            }
            let stamped = wa.data.as_ref().and_then(|d| d.downcast_ref::<ResolvedChatTarget>()).copied();
            if stamped.is_some_and(|stamp| stamp.town != self.tree.snapshot().town) {
                continue;
            }
            self.chat_error = None;
            match *gesture {
                PortalListAction::ItemClicked { item_id, abs, taps } => {
                    let target = match stamped {
                        Some(ResolvedChatTarget { target, .. }) => target.and_then(|id| ChatTarget::by_id(&self.workspace.rows, id)),
                        None => item_id
                            .filter(|&i| !hit_unread_line(cx, list, i, abs))
                            .and_then(|i| self.workspace.rows.get(i))
                            .and_then(ChatTarget::from_row),
                    };
                    if taps >= 2 && matches!(target, Some(ChatTarget::Message(_))) { continue }
                    self.chat_selected = target.map(ChatTarget::id);
                    if self.chat_selected.is_none() || list.has_selection() {
                        if let Some(mut inner) = list.borrow_mut() { inner.clear_selection(cx); }
                    }
                    if taps == 2 {
                        if let Some(command) = self.selected_command(MessageAction::Open, list) {
                            list.redraw(cx);
                            return Some(command)
                        }
                    }
                }
                PortalListAction::ItemContextMenu { item_id, abs } => {
                    let target = match stamped {
                        Some(ResolvedChatTarget { target, .. }) => target.and_then(|id| ChatTarget::by_id(&self.workspace.rows, id)),
                        None => (!hit_unread_line(cx, list, item_id, abs))
                            .then(|| self.workspace.rows.get(item_id))
                            .flatten()
                            .and_then(ChatTarget::from_row),
                    };
                    let Some(target) = target else { continue };
                    self.chat_selected = Some(target.id());
                    if matches!(target, ChatTarget::Picture(..)) && list.has_selection() {
                        if let Some(mut inner) = list.borrow_mut() { inner.clear_selection(cx); }
                    }
                    cx.set_key_focus_from_user(list.area());
                    let rows: Vec<_> = message_actions(target, list.has_selection(), self.workspace.writable)
                        .map(|a| MenuRow { danger: a == MessageAction::Delete, ..MenuRow::new(a.id(), a.label()) }.hint(a.hint())).collect();
                    if !rows.is_empty() {
                        cx.action(MenuAction::Open { owner: id!(chat_message_menu), rows,
                            anchor: Rect { pos: abs, ..Default::default() }, place: MenuPlace::At });
                    }
                }
                _ => continue,
            }
            list.redraw(cx);
        }
        let picked = menu_picked(actions, id!(chat_message_menu))?;
        let target = self.selected_target()?;
        let action = message_actions(target, list.has_selection(), self.workspace.writable).find(|&a| a.id() == picked)?;
        match action {
            MessageAction::Reply | MessageAction::Delete | MessageAction::Open => target.command(action),
            _ => {
                if let Some(text) = self.copy_text(action, list) { cx.copy_to_clipboard(&text); }
                None
            }
        }
    }

    pub(crate) fn pending_approval_item_id(&self, row_hint: usize, line: Id) -> Option<usize> {
        let key = ChatRowKey::Message(line);
        let rendered = &self.chat_read.rendered_rows;
        if rendered.get(row_hint) == Some(&key) {
            Some(row_hint)
        } else {
            rendered.iter().position(|&k| k == key)
        }
    }

    pub fn sync_pending_approval(&mut self, cx: &mut Cx, conversation: &WidgetRef, list: &PortalListRef) {
        let bar = conversation.widget(cx, ids!(pending_approval));
        let show_btn = bar.button(cx, ids!(pending_approval_show));
        let hide = |cx: &mut Cx, chat_read: &mut crate::lists::ChatReadState| {
            chat_read.set_pending_cue_shown(None);
            show_btn.set_action_data(());
            if bar.visible() {
                bar.set_visible(cx, false);
            }
        };
        if !self.chat_visible() {
            hide(cx, &mut self.chat_read);
            return;
        }
        let Some((row_hint, line, request)) = self.workspace.connect.first_pending() else {
            hide(cx, &mut self.chat_read);
            return;
        };
        let Some(item_id) = self.pending_approval_item_id(row_hint, line) else {
            hide(cx, &mut self.chat_read);
            return;
        };
        let open_town = self.tree.snapshot().town;
        let target = (open_town, line);
        let show = match item_on_screen(list, item_id) {
            None | Some(true) => false,
            Some(false) => {
                if self.chat_read.at_live_tail() && tail_draw_pending(list) {
                    self.chat_read.pending_cue_shown == Some(target)
                } else {
                    true
                }
            }
        };
        if !show {
            hide(cx, &mut self.chat_read);
            return;
        }
        self.chat_read.set_pending_cue_shown(Some(target));
        show_btn.set_action_data(target);
        let label = bar.label(cx, ids!(pending_approval_text));
        if label.text() != request.cue {
            label.set_text(cx, &request.cue);
        }
        if !bar.visible() {
            bar.set_visible(cx, true);
        }
    }

    fn clicked_pending_approval(&self, cx: &Cx, conversation: &WidgetRef, actions: &Actions) -> Option<(usize, Id)> {
        if !self.chat_visible() {
            return None;
        }
        let (row_hint, line, _) = self.workspace.connect.first_pending()?;
        let target = (self.tree.snapshot().town, line);
        if self.chat_read.pending_cue_shown != Some(target) {
            return None;
        }
        let show = conversation.button(cx, ids!(pending_approval_show));
        button_clicked_for(&show, actions, &target).then_some((row_hint, line))
    }

    pub fn chat_scroll_actions(&mut self, cx: &mut Cx, conversation: &WidgetRef, list: &PortalListRef, actions: &Actions) -> bool {
        let transcript_uid = conversation.widget(cx, ids!(transcript)).widget_uid();
        let pending_visibility_changed = actions.filter_widget_actions(transcript_uid)
            .any(|item| item.action.downcast_ref::<PendingApprovalVisibility>().is_some());
        let mut unread_changed = false;
        if conversation.button(cx, ids!(jump_latest_btn)).clicked(actions) {
            self.chat_read.mark_at_tail();
            if let Some(open) = self.tree.snapshot().town {
                if let Some(unread) = self.chat_read.counts.get(&open).copied() {
                    let had_divider = self.chat_read.towns.get(&open).and_then(|r| r.divider_before).is_some();
                    self.chat_read.mark_open_read(open, unread.newest);
                    if had_divider { list.redraw(cx); }
                }
            }
            list.smooth_scroll_to_end(cx, 120.0, Some(20));
            unread_changed = true;
        } else if let Some((row_hint, line)) = self.clicked_pending_approval(cx, conversation, actions) {
            if self.pending_approval_item_id(row_hint, line).is_none() {
                self.sync_chat_rows(cx, list);
            }
            if let Some(item_id) = self.pending_approval_item_id(row_hint, line) {
                self.chat_read.mark_user_scrolled_up();
                list.set_tail_range(false);
                list.smooth_scroll_to(cx, item_id, 120.0, Some(20), 24.0);
            }
        } else if list.scrolled(actions) || list.smooth_scroll_reached(actions) || list.reached_end(actions) {
            let scrolling_away = list.is_smooth_scrolling()
                .is_some_and(|target| target + 1 < self.workspace.rows.len());
            if self.workspace.rows.is_empty() || (list.is_at_end() && !scrolling_away) {
                self.chat_read.mark_at_tail();
                if self.chat_visible() {
                    match self.tree.snapshot().town {
                        Some(open) => {
                            if let Some(unread) = self.chat_read.counts.get(&open).copied() {
                                self.chat_read.mark_open_seen(open, unread.newest);
                            }
                        }
                        None => self.chat_read.mark_unscoped_read(self.newest_message()),
                    }
                }
            } else if list.is_smooth_scrolling().is_none() && self.chat_read.user_scrolled_up() {
                self.chat_read.open_at_tail = false;
            }
            unread_changed = true;
        } else if !pending_visibility_changed {
            return false;
        }
        self.sync_jump_latest(cx, conversation);
        self.sync_pending_approval(cx, conversation, list);
        if unread_changed { self.tree.set_unread(&self.chat_read.counts) } else { false }
    }

    /// Reveal the quoted message. A successful jump arms the caller's flash timer.
    pub fn jump_to_chat_message(&mut self, cx: &mut Cx, conversation: &WidgetRef, list: &PortalListRef,
        source: Id, target: Id) -> bool {
        self.chat_error = None;
        let Some(row) = self.workspace.chat_row(&target) else {
            self.chat_flash = None;
            if self.workspace.chat_row(&source).is_some() {
                self.chat_error = Some((source, "That message was removed.".into()));
                list.redraw(cx);
            }
            return false
        };
        if list.borrow().is_none_or(|l| l.range_end() != self.workspace.rows.len()) {
            self.sync_chat_rows(cx, list);
        }
        if item_in_view(list, row) != Some(true) {
            self.chat_read.mark_user_scrolled_up();
            list.set_tail_range(false);
            let smooth = list.borrow().is_some_and(|l| {
                let viewport_h = l.scroll_bar.get_scroll_view_visible();
                !l.items().is_empty()
                    && l.first_id().abs_diff(row) <= JUMP_SMOOTH_MAX_ROWS
                    && !l.drawn_slot(row).is_some_and(|slot| {
                        slot.start >= 0.0 && slot.start < viewport_h + JUMP_VIEW_SLACK
                    })
            });
            if smooth {
                list.smooth_scroll_to(cx, row, 120.0, Some(JUMP_SMOOTH_MAX_ROWS), JUMP_TOP_OFFSET);
            }
            if !smooth || list.is_smooth_scrolling().is_none() {
                list.stop_scrolling();
                list.set_first_id_and_scroll(row, if row > 0 { JUMP_TOP_OFFSET } else { 0.0 });
            }
            self.sync_jump_latest(cx, conversation);
            self.sync_pending_approval(cx, conversation, list);
        } else if list.is_smooth_scrolling().is_some() {
            list.stop_scrolling();
        }
        self.chat_flash = Some(target);
        list.redraw(cx);
        true
    }

    fn selected_command(&self, action: MessageAction, list: &PortalListRef) -> Option<ChatCommand> {
        let target = self.selected_target()?;
        message_actions(target, list.has_selection(), self.workspace.writable).find(|&a| a == action).and_then(|a| target.command(a))
    }

    /// Handle keys while the transcript has focus.
    pub fn chat_key(&mut self, cx: &mut Cx, list: &PortalListRef, event: &Event) -> Option<ChatCommand> {
        if list.area().is_empty() || !cx.has_key_focus(list.area()) { return None }
        match event {
            Event::TextCopy(copy) if copy.response.borrow().is_none() => {
                let action = if list.has_selection() { MessageAction::CopySelection } else { MessageAction::Copy };
                *copy.response.borrow_mut() = self.copy_text(action, list);
            }
            Event::KeyDown(k) if k.key_code == KeyCode::Escape => {
                self.chat_selected = None;
                self.chat_flash = None;
                self.chat_error = None;
                if let Some(mut inner) = list.borrow_mut() { inner.clear_selection(cx); }
                list.redraw(cx);
            }
            Event::KeyDown(k) if matches!(k.key_code, KeyCode::Delete | KeyCode::Backspace) => {
                return self.selected_command(MessageAction::Delete, list)
            }
            Event::KeyDown(k) if matches!(k.key_code, KeyCode::ReturnKey | KeyCode::NumpadEnter) => {
                return self.selected_command(MessageAction::Open, list)
            }
            _ => {}
        }
        None
    }
}

/// The colours a message row takes from its list.
#[derive(Clone, Copy)]
struct RowInks { mention: Vec4f, outline: Vec4f, line: Vec4f }

// A clip or call can contain status labels anywhere, including inside its controls.
// Exclude those labels once when the widget is created, leaving its input handling intact.
fn non_message_item(cx: &mut Cx, list: &mut PortalList, index: usize, template: LiveId) -> WidgetRef {
    fn exclude_labels(cx: &mut Cx, mut widget: WidgetRef) {
        if widget.borrow::<Label>().is_some() {
            script_apply_eval!(cx, widget, {selectable: false});
        } else {
            widget.children(&mut |_, child| exclude_labels(cx, child));
        }
    }
    let (item, existed) = list.item_with_existed(cx, index, template);
    if !existed { exclude_labels(cx, item.clone()); }
    item
}

fn draw_visible_rows(cx: &mut Cx2d, list: &mut PortalList, data: &mut crate::lists::ViewData, inks: RowInks,
    drawn_strip: &mut Option<DrawnStrip>,
    mut draw_traces: impl FnMut(&mut Cx2d, &mut FlatList, &Message)) {
    let open_town = data.tree.snapshot().town;
    let already_synced = data.workspace.rows_rev != 0
        && data.chat_read.rendered_rev == data.workspace.rows_rev
        && data.chat_read.rendered_rows.len() == data.workspace.rows.len()
        && data.chat_read.rendered_town == open_town
        && list.range_end() == data.workspace.rows.len();
    if !already_synced {
        data.sync_chat_rows_inner(cx, list);
    }
    let rows = &data.workspace.rows;
    let mut quote_text = String::new();
    let divider_before = open_town
        .and_then(|t| data.chat_read.towns.get(&t))
        .and_then(|r| r.divider_before);
    // Keep the message header from clipping off-top if `first_id` was scrolled partway past its
    // "New messages" strip and that strip is now hiding.
    if let Some(strip) = *drawn_strip {
        if open_town != Some(strip.town) {
            *drawn_strip = None;
        } else if divider_before != Some(strip.message) {
            let first = list.first_id();
            let scroll = list.first_scroll();
            if scroll < 0.0
                && matches!(rows.get(first), Some(crate::view::ChatRow::Message(m)) if m.id == strip.message)
            {
                list.set_first_id_and_scroll_in_place(first, (scroll + strip.height).min(0.0));
            }
            *drawn_strip = None;
        }
    }
    while let Some(index) = list.next_visible_item(cx) {
        let Some(row) = rows.get(index) else { continue };
        let message = match row {
            crate::view::ChatRow::Day(day) => {
                let item = non_message_item(cx, list, index, id!(DayLine));
                let label = item.label(cx, ids!(day_label));
                if label.text() != day.label { label.set_text(cx, &day.label); }
                item.draw_all(cx, &mut Scope::empty());
                continue
            }
            crate::view::ChatRow::Member(notice) => {
                let item = list.item(cx, index, id!(MemberLine));
                item.set_text(cx, &notice.text);
                item.draw_all(cx, &mut Scope::empty());
                continue
            }
            crate::view::ChatRow::Clip(clip) => {
                let item = non_message_item(cx, list, index, id!(ClipLine));
                if let Some(mut row) = item.borrow_mut::<ClipRow>() {
                    row.bind(cx, clip);
                    row.set_can_pin(cx, data.workspace.writable && !data.tree.snapshot().projects.is_empty());
                }
                let selected = data.chat_selected == Some(clip.id.1) && clip.media.as_ref().is_some_and(|m| m.is_picture);
                item.widget(cx, ids!(frame.picture_border)).set_visible(cx, selected);
                item.draw_all(cx, &mut Scope::empty());
                continue
            }
            crate::view::ChatRow::Call(call) => {
                use crate::call::CallEntryRowWidgetRefExt;
                let item = non_message_item(cx, list, index, id!(CallLine));
                item.as_call_entry_row().show(cx, call);
                let button = item.button(cx, ids!(summarise));
                let available = data.workspace.summarisable.contains(&call.id.1);
                button.set_visible(cx, available);
                if available { button.set_action_data(crate::view::Summarise(call.id)); }
                let button = item.button(cx, ids!(update_tasks));
                let available = data.workspace.updatable.contains(&call.id.1);
                button.set_visible(cx, available);
                if available { button.set_action_data(crate::view::UpdateTasks(call.id)); }
                let jobs = item.label(cx, ids!(jobs));
                jobs.set_visible(cx, !call.jobs.is_empty());
                if !call.jobs.is_empty() {
                    jobs.set_text(cx, &call.jobs.iter().map(|j| j.words.as_str()).collect::<Vec<_>>().join("\n"));
                }
                item.draw_all(cx, &mut Scope::empty());
                continue
            }
            crate::view::ChatRow::Message(message) => message,
        };
        let item = list.item(cx, index, if message.addressed { id!(Addressed) } else { id!(Line) });
        let line = item.widget(cx, ids!(unread_line));
        let show_divider = divider_before == Some(message.id);
        if line.visible() != show_divider { line.set_visible(cx, show_divider); }
        let author = item.widget(cx, ids!(author));
        crate::lists::seat_icon(cx, &author, message.seat, &message.badge, message.tone);
        let request = data.workspace.connect.requests.get(&index);
        item.widget(cx, ids!(approval)).set_visible(cx, request.is_some());
        if let Some(request) = request {
            let allow = item.button(cx, ids!(allow));
            allow.set_text(cx, &request.label);
            allow.set_action_data(request.allow);
            item.button(cx, ids!(deny)).set_action_data(request.deny);
        }
        item.label(cx, ids!(name)).set_text(cx, &message.name);
        item.label(cx, ids!(when)).set_text(cx, &message.time);
        item.widget(cx, ids!(quote)).set_visible(cx, message.quote.is_some());
        let mut quote_jump = item.button(cx, ids!(quote_jump));
        if let Some(quote) = &message.quote {
            quote_text.clear();
            write!(quote_text, "{quote}").unwrap();
            let quote_label = item.label(cx, ids!(quote_text));
            if quote_label.text() != quote_text {
                quote_label.set_text(cx, &quote_text);
                let name = cx.with_vm(|vm| vm.new_string(&format!("Jump to quoted message: {quote_text}")));
                script_apply_eval!(cx, quote_jump, {a11y_name: #(name)});
            }
            quote_jump.set_action_data(QuoteJump { town: open_town, source: message.id, target: quote.id });
        } else {
            quote_jump.set_action_data(());
        }
        let mine = message.seat == crate::view::Seat::You;
        item.widget(cx, ids!(trace_section)).set_visible(cx, !message.traces.is_empty());
        item.widget(cx, ids!(sent)).set_visible(cx, message.traces.is_empty() && message.addressed && !message.unheard && mine);
        item.widget(cx, ids!(no_ai)).set_visible(cx, message.unheard && mine);
        if let Some(&town) = data.town_ids.get(data.town_index) {
            item.button(cx, ids!(chat_add_ai)).set_action_data(crate::ui::home::HomeAction::AddAi(town));
        }
        let body = item.label(cx, ids!(message));
        body.set_text(cx, &message.text);
        let mention = [(0, 2, inks.mention)];
        body.set_inks(cx, if message.mention { &mention } else { &[] });
        // Selection and a quote jump share the outline already reserved inside the bubble.
        let selected = data.chat_selected == Some(message.id);
        let flashed = data.chat_flash == Some(message.id);
        if let Some(mut bubble) = item.view(cx, ids!(bubble)).borrow_mut() {
            bubble.show_bg = selected || flashed || message.addressed;
            let ink = if selected || flashed { inks.outline } else { inks.line };
            bubble.draw_bg.set_dyn_instance(cx, id!(border_color), &[ink.x, ink.y, ink.z, ink.w]);
        }
        if let Some(mut bar) = item.view(cx, ids!(mention_bar)).borrow_mut() {
            bar.show_bg = message.mentions_you;
        }
        let note = item.label(cx, ids!(action_note));
        let error = data.chat_error.as_ref().filter(|(id, _)| *id == message.id).map(|(_, why)| why.as_str());
        if note.visible() != error.is_some() { note.set_visible(cx, error.is_some()); }
        if let Some(why) = error { note.set_text(cx, why); }
        let walk = item.walk(cx);
        while let Some(step) = item.draw_walk(cx, &mut Scope::empty(), walk).step() {
            let Some(mut flat) = step.borrow_mut::<FlatList>() else { continue };
            draw_traces(cx, &mut flat, message);
        }
        if show_divider {
            if let Some(town) = open_town {
                let height = line.area().rect(cx).size.y;
                if height > 0.0 {
                    *drawn_strip = Some(DrawnStrip { town, message: message.id, height });
                }
            }
        }
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use makepad_widgets::{cx_draw::CxDraw, makepad_platform::{event::{LongPressEvent, ScrollEvent, ScrollPhase, TouchPoint, TouchUpdateEvent}, studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp}}, makepad_script::script};

    use crate::view::{MemberNotice, NoticeKey, Quote, Workspace};

    struct ChatFixture {
        cx: test_cx::PooledCx,
        chat: WidgetRef,
        list: PortalListRef,
        pass: DrawPass,
        ink: DrawList2d,
        data: ViewData,
    }

    fn message(id: u8, name: &str, text: &str, deletable: bool) -> ChatRow {
        ChatRow::Message(Message { id: [id; 32], quote: None, seat: crate::view::Seat::Peer, tone: None,
            name: name.into(), badge: name.into(), time: "14:02".into(), text: text.into(), traces: Vec::new(),
            addressed: false, unheard: false, mention: false, mentions_you: false, deletable })
    }

    fn notice(id: u8, text: &str) -> ChatRow {
        ChatRow::Member(MemberNotice { key: NoticeKey::Member([id; 32]), text: text.into() })
    }

    fn message_index(workspace: &Workspace, id: Id) -> usize {
        workspace.rows.iter().position(|row| row.key() == ChatRowKey::Message(id)).expect("message in the transcript")
    }

    impl ChatFixture {
        const SIZE: DVec2 = DVec2 { x: 360., y: 480. };
        const WINDOW: WindowId = WindowId(1, 1);

        fn new(town: Id, rows: Vec<ChatRow>) -> Self {
            let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
            let chat = cx.with_vm(|vm| {
                let value = vm.eval(script! { use mod.widgets.* use mod.prelude.widgets.*
                    View{flow: Overlay conversation := ChatConversation{} menus := MenuLayer{}}
                });
                WidgetRef::script_from_value(vm, value)
            });
            let list = chat.portal_list(&cx, ids!(transcript.list));
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, Self::SIZE);
            let ink = DrawList2d::new(&mut cx);
            let mut data = ViewData::default();
            data.workspace.rows = rows;
            data.workspace.index_chat_rows();
            data.workspace.writable = true;
            data.tree.set(Vec::new(), crate::model::tree::TownTree { town: Some(town), ..Default::default() });
            Self { cx, chat, list, pass, ink, data }
        }

        fn divider(&mut self, id: Id) {
            let town = self.data.tree.snapshot().town.unwrap();
            self.data.chat_read.towns.entry(town).or_default().divider_before = Some(id);
        }

        fn draw(&mut self) {
            let actions = self.draw_actions();
            self.cx.extend_actions(actions);
        }

        fn draw_actions(&mut self) -> ActionsBuf {
            self.cx.capture_actions(|cx| {
                let event = DrawEvent::default();
                let mut draw = CxDraw::new(cx, &event);
                let mut frame = Cx2d::new(&mut draw);
                frame.begin_pass(&self.pass, None);
                self.ink.begin_always(&mut frame);
                frame.begin_root_turtle(Self::SIZE, Layout::flow_down());
                self.chat.draw_all(&mut frame, &mut Scope::with_data(&mut self.data));
                frame.end_pass_sized_turtle();
                self.ink.end(&mut frame);
                frame.end_pass(&self.pass);
            })
        }

        fn draw_and_scroll(&mut self) -> ActionsBuf {
            let actions = self.draw_actions();
            self.scroll_actions(&actions);
            actions
        }

        fn scroll_actions(&mut self, actions: &Actions) -> bool {
            self.data.chat_scroll_actions(&mut self.cx, &self.chat, &self.list, actions)
        }

        fn sync_cue(&mut self) {
            self.data.sync_pending_approval(&mut self.cx, &self.chat, &self.list);
        }

        fn release_show(&mut self, at: DVec2, time: f64) -> ActionsBuf {
            let button = MouseButton::PRIMARY;
            let actions = self.input(Event::MouseUp(MouseUpEvent { abs: at, button, time,
                window_id: Self::WINDOW, modifiers: Default::default() }));
            self.pointer_end(at, button, time);
            actions
        }

        fn settle_scroll(&mut self, mut next: u64, time: f64) {
            for frame in 0..60 {
                if self.list.is_smooth_scrolling().is_none() { break }
                let end = self.cx.next_frame_id();
                let actions = self.input(Event::NextFrame(NextFrameEvent { frame, time: time + frame as f64 / 60.,
                    set: (next..end).map(NextFrame).collect() }));
                next = end;
                self.scroll_actions(&actions);
                self.draw_and_scroll();
            }
            assert!(self.list.is_smooth_scrolling().is_none(), "Show finishes scrolling to the request");
        }

        fn sync(&mut self) -> usize { self.data.sync_chat_rows_counting(&mut self.cx, &self.list) }

        fn anchor(&mut self, index: usize, scroll: f64) {
            self.list.set_tail_range(false);
            self.list.set_first_id_and_scroll(index, scroll);
            self.draw();
        }

        fn child(&self, index: usize, path: &[LiveId]) -> WidgetRef {
            self.list.get_item(index).unwrap().1.widget(&self.cx, path)
        }

        fn center(&self, index: usize, path: &[LiveId]) -> DVec2 {
            self.child(index, path).area().rect(&self.cx).center()
        }

        fn input(&mut self, event: Event) -> ActionsBuf {
            self.cx.capture_actions(|cx| {
                self.data.chat_key(cx, &self.list, &event);
                self.chat.handle_event(cx, &event, &mut Scope::with_data(&mut self.data));
            })
        }

        fn dispatch(&mut self, actions: &Actions) -> ActionsBuf {
            self.cx.capture_actions(|cx| { self.data.chat_actions(cx, &self.list, actions); })
        }

        fn pointer_start(&mut self, abs: DVec2, button: MouseButton, time: f64) {
            self.cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown { x: abs.x, y: abs.y, time,
                button_raw_bits: button.bits(), ..Default::default() }), Self::WINDOW, DVec2::default());
        }

        fn pointer_end(&mut self, abs: DVec2, button: MouseButton, time: f64) {
            self.cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp { x: abs.x, y: abs.y, time,
                button_raw_bits: button.bits(), ..Default::default() }), Self::WINDOW, DVec2::default());
        }

        fn down(&mut self, abs: DVec2, button: MouseButton, time: f64) -> ActionsBuf {
            self.pointer_start(abs, button, time);
            self.input(Event::MouseDown(MouseDownEvent { abs, button, window_id: Self::WINDOW, time,
                modifiers: Default::default(), handled: Default::default() }))
        }

        fn click_quote(&mut self, index: usize, time: f64) -> ActionsBuf {
            let at = self.center(index, ids!(quote_jump));
            let down = self.down(at, MouseButton::PRIMARY, time);
            self.dispatch(&down);
            self.release_show(at, time + 0.1)
        }

        fn up(&mut self, abs: DVec2, button: MouseButton, time: f64) {
            let actions = self.input(Event::MouseUp(MouseUpEvent { abs, button, window_id: Self::WINDOW, time,
                modifiers: Default::default() }));
            self.dispatch(&actions);
            self.pointer_end(abs, button, time);
        }

        fn touch(&mut self, state: TouchState, uid: u64, abs: DVec2, time: f64) -> ActionsBuf {
            if state == TouchState::Start { self.pointer_start(abs, MouseButton::PRIMARY, time); }
            let event = TouchUpdateEvent { time, window_id: Self::WINDOW, modifiers: Default::default(),
                touches: vec![TouchPoint { state, uid, abs, time, force: 1., rotation_angle: 0., radius: dvec2(1., 1.),
                    handled: Default::default(), sweep_lock: Default::default() }] };
            let actions = self.input(Event::TouchUpdate(event.clone()));
            self.cx.fingers.process_touch_update_end(&event.touches);
            if state == TouchState::Stop { self.pointer_end(abs, MouseButton::PRIMARY, time); }
            self.dispatch(&actions)
        }

        fn hold(&mut self, uid: u64, abs: DVec2, time: f64) -> ActionsBuf {
            let actions = self.input(Event::LongPress(LongPressEvent { window_id: Self::WINDOW, abs, uid, time }));
            self.dispatch(&actions)
        }

        fn select_words(&mut self, index: usize, words: &str) {
            let body = self.child(index, ids!(message));
            let rect = body.area().rect(&self.cx);
            let point = |offset| (0..(rect.size.x * 2.) as usize)
                .map(|x| dvec2(rect.pos.x + x as f64 * 0.5, rect.pos.y + 5.))
                .find(|&p| body.selection_point_to_char_index(&self.cx, p) == Some(offset)).unwrap();
            let (start, end) = (point(0), point(words.len()));
            let actions = self.down(start, MouseButton::PRIMARY, 1.);
            self.dispatch(&actions);
            let actions = self.input(Event::MouseMove(MouseMoveEvent { abs: end, lock_delta: Default::default(),
                window_id: Self::WINDOW, time: 1.1, modifiers: Default::default(), handled: Default::default() }));
            self.dispatch(&actions);
            self.up(end, MouseButton::PRIMARY, 1.2);
            assert_eq!(self.list.selected_entries(), Some((index, index)));
            assert_eq!(self.copy().as_deref(), Some(words));
        }

        fn copy(&mut self) -> Option<String> {
            let request = TextClipboardEvent::default();
            self.input(Event::TextCopy(request.clone()));
            request.response.take()
        }
    }

    fn set_quote(row: &mut ChatRow, id: u8, name: &str, text: &str) {
        let ChatRow::Message(reply) = row else { panic!("only messages can quote another line") };
        reply.quote = Some(Quote { id: [id; 32], name: name.into(), text: text.into() });
    }

    #[test]
    fn reply_quote_click_jumps_to_target_and_flashes_without_selecting() {
        let mut rows: Vec<_> = (1..=80).map(|id| message(id, "Ana", "Line", false)).collect();
        rows[4] = message(5, "Ana", &(0..45).map(|i| format!("Tall line {i}")).collect::<Vec<_>>().join("\n"), false);
        set_quote(&mut rows[79], 6, "Ana", "Line");
        let mut f = ChatFixture::new([4; 32], rows);
        f.sync();
        f.anchor(5, 24.);
        let viewport_h = f.list.borrow().unwrap().scroll_bar.get_scroll_view_visible();
        assert!(f.list.drawn_slot(4).unwrap().size > viewport_h, "the predecessor is taller than the viewport");
        let bubble = f.child(5, ids!(bubble));
        // Only the bubble is visible in content. Its parent keeps the full layout precision
        // even when the bubble's own area changes from a layout rect to a painted f32 instance.
        let layout_height = |f: &ChatFixture| f.child(5, ids!(content)).area().rect(&f.cx).size.y;
        let before_layout = layout_height(&f);
        let before_h = bubble.area().rect(&f.cx).size.y as f32;
        assert!(before_h > 0., "the height baseline comes from a drawn target");
        assert!(!bubble.borrow::<View>().unwrap().show_bg, "the target starts without an outline");

        f.list.scroll_to_end(&mut f.cx);
        f.draw_and_scroll();
        assert_eq!(item_on_screen(&f.list, 5), Some(false), "the target is off screen before the jump");
        assert!(f.data.chat_read.at_live_tail(), "the chat follows the tail before the click");
        let actions = f.click_quote(79, 1.);
        assert_eq!(f.data.chat_actions(&mut f.cx, &f.list, &actions),
            Some(ChatCommand::JumpQuote { source: [80; 32], target: [6; 32] }), "the click identifies the reply and its target");
        assert_eq!(f.data.chat_selected, None, "clicking the quote leaves the bubble unselected");
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [80; 32], [6; 32]), "a live target arms the timer");
        assert_eq!(f.data.chat_flash, Some([6; 32]), "the flash belongs to the quoted line");
        assert_eq!(f.data.chat_error, None, "a live target has no inline error");
        assert_eq!(f.list.is_smooth_scrolling(), None, "a far jump snaps without smooth scrolling");
        assert_eq!(f.list.first_id(), 5, "the far jump anchors on the target row");
        assert_eq!(f.list.scroll_position(), JUMP_TOP_OFFSET, "a non-zero far jump leaves 24 px of context above the target");
        assert!(!f.data.chat_read.at_live_tail(), "the jump releases live-tail following");
        f.draw();
        assert_eq!(item_on_screen(&f.list, 5), Some(true), "the jump brings the quoted line into view");
        assert_eq!(item_in_view(&f.list, 5), Some(true), "the far jump brings the target into view past a tall predecessor");
        assert!((f.list.drawn_slot(5).unwrap().start - JUMP_TOP_OFFSET).abs() < 0.5, "the target lands 24 px below the viewport top");
        let bubble = f.child(5, ids!(bubble));
        assert!(bubble.borrow::<View>().unwrap().show_bg, "the jumped-to bubble paints its outline");
        assert_eq!(layout_height(&f), before_layout, "the outline preserves the exact height allocated to the bubble");
        assert_eq!(bubble.area().rect(&f.cx).size.y as f32, before_h, "the outline preserves bubble height at paint precision");

        f.anchor(4, JUMP_TOP_OFFSET);
        let before = (f.list.first_id(), f.list.scroll_position());
        assert_eq!(item_in_view(&f.list, 4), Some(true), "a tall row at the 24 px top margin counts as in view");
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [80; 32], [5; 32]),
            "jumping to an already-shown tall row still arms the flash");
        assert_eq!(f.list.is_smooth_scrolling(), None, "an already-shown tall row does not start a smooth scroll");
        assert_eq!((f.list.first_id(), f.list.scroll_position()), before, "an already-shown tall row preserves the viewport");

        f.list.scroll_to_end(&mut f.cx);
        f.draw_and_scroll();
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [80; 32], [1; 32]), "a far jump to row 0 arms the flash");
        assert_eq!(f.list.is_smooth_scrolling(), None, "a far jump to row 0 snaps without smooth scrolling");
        assert_eq!(f.list.first_id(), 0, "a far jump to row 0 anchors on row 0");
        assert_eq!(f.list.scroll_position(), 0.0, "a far jump to row 0 uses zero top offset");
        f.draw();
        assert_eq!(item_in_view(&f.list, 0), Some(true), "row 0 is fully in view");
        assert!(f.list.drawn_slot(0).unwrap().start.abs() < 0.5, "row 0 lands flush with the viewport top");
    }

    #[test]
    fn reply_quote_click_on_removed_target_shows_inline_note() {
        let mut rows = vec![message(1, "Ana", "Before", false), message(2, "Ben", "Reply to removed", false),
            message(3, "Cam", "Reply to live", false)];
        set_quote(&mut rows[1], 99, "", "Removed message");
        set_quote(&mut rows[2], 1, "Ana", "Before");
        let mut f = ChatFixture::new([4; 32], rows);
        f.data.chat_flash = Some([1; 32]);
        f.draw();
        let scroll = |f: &ChatFixture| (f.list.first_id(), f.list.borrow().unwrap().first_scroll());
        let before = scroll(&f);
        let actions = f.click_quote(1, 1.);
        assert_eq!(f.data.chat_actions(&mut f.cx, &f.list, &actions),
            Some(ChatCommand::JumpQuote { source: [2; 32], target: [99; 32] }), "a removed-message bar still emits its reference");
        assert_eq!(f.data.chat_selected, None, "the removed-message click leaves the reply unselected");
        assert!(!f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [2; 32], [99; 32]), "a missing target does not arm the timer");
        assert_eq!(f.data.chat_flash, None, "the missing target clears the previous flash");
        assert_eq!(f.data.chat_error, Some(([2; 32], "That message was removed.".into())), "the note belongs to Ben's reply");
        assert_eq!(scroll(&f), before, "the missing target preserves the scroll position");
        f.draw();
        let note = f.child(1, ids!(action_note));
        assert!(note.visible(), "Ben's reply shows its inline note");
        assert_eq!(note.text(), "That message was removed.", "Ben's note explains the missing target");

        let actions = f.click_quote(2, 2.);
        f.data.workspace.rows.remove(0);
        let ChatRow::Message(cam) = &mut f.data.workspace.rows[1] else { panic!("Cam's reply survives the retract") };
        cam.quote = None;
        f.data.workspace.index_chat_rows();
        f.sync();
        let before = scroll(&f);
        assert_eq!(f.data.chat_actions(&mut f.cx, &f.list, &actions),
            Some(ChatCommand::JumpQuote { source: [3; 32], target: [1; 32] }), "the captured click keeps its pre-retract message IDs");
        assert!(!f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [3; 32], [1; 32]), "a same-turn retract does not arm the timer");
        assert_eq!(f.data.chat_flash, None, "a retracted target has no flash");
        assert_eq!(f.data.chat_error, Some(([3; 32], "That message was removed.".into())), "the race leaves a note on Cam's reply");
        assert_eq!(scroll(&f), before, "dispatch after the retract preserves the refreshed scroll position");
        f.draw();
        assert!(!f.child(1, ids!(quote)).visible(), "the retract hides Cam's former quote");
        let note = f.child(1, ids!(action_note));
        assert!(note.visible(), "Cam's reply shows its inline note after the retract");
        assert_eq!(note.text(), "That message was removed.", "Cam's note explains the retract");

        assert!(!f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [88; 32], [99; 32]), "a vanished source and target do not arm the timer");
        assert_eq!(f.data.chat_flash, None, "two vanished rows leave no flash");
        assert_eq!(f.data.chat_error, None, "a vanished reply leaves no orphan note");
    }

    #[test]
    fn reply_quote_a11y_press_triggers_jump_and_ignores_stale_town() {
        use crate::lists::{Form, Pane};
        let mut rows = vec![message(1, "Ana", "Before", false), message(2, "Ben", "Reply", false)];
        set_quote(&mut rows[1], 1, "Ana", "Before");
        let mut f = ChatFixture::new([4; 32], rows);
        f.draw();
        let button = f.child(1, ids!(quote_jump));
        let press = Event::A11yAction(a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId::widget(button.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None });
        let actions = f.input(press);
        assert_eq!(f.data.chat_actions(&mut f.cx, &f.list, &actions),
            Some(ChatCommand::JumpQuote { source: [2; 32], target: [1; 32] }), "a screen reader press follows the reply's quote");
        f.data.form = Form::Narrow;
        f.data.pane = Pane::Places;
        assert!(!f.data.chat_visible(), "chat is hidden on another narrow pane");
        assert_eq!(f.data.chat_actions(&mut f.cx, &f.list, &actions), None, "a queued press is ignored while chat is hidden");
        assert_eq!(f.data.chat_error, None, "a hidden press leaves no error");
        f.data.form = Form::Wide;
        f.data.pane = Pane::Chat;
        f.data.tree.set(Vec::new(), crate::model::tree::TownTree { town: Some([5; 32]), ..Default::default() });
        assert_eq!(f.data.chat_actions(&mut f.cx, &f.list, &actions), None, "the new town ignores the previous town's press");
        assert_eq!(f.data.chat_error, None, "the stale press leaves no error in the new town");
    }

    #[test]
    fn reply_quote_jump_handles_partial_visibility_and_smooth_cutoff() {
        let mut f = ChatFixture::new([4; 32], (1..=46).map(|id| message(id, "Ana", "Line", false)).collect());
        f.sync();
        f.anchor(5, 0.);
        let viewport_h = f.list.borrow().unwrap().scroll_bar.get_scroll_view_visible();
        let before = (f.list.first_id(), f.list.scroll_position());
        assert_eq!(item_in_view(&f.list, 6), Some(true), "row 6 starts fully in view");
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [46; 32], [7; 32]), "a fully visible target arms the flash");
        assert_eq!(f.list.is_smooth_scrolling(), None, "a fully visible target does not start scrolling");
        assert_eq!((f.list.first_id(), f.list.scroll_position()), before, "a fully visible target preserves the viewport");
        assert_eq!(f.data.chat_flash, Some([7; 32]), "the visible target receives the flash");

        let slot5 = f.list.drawn_slot(5).unwrap().size;
        f.anchor(5, -(slot5 - 1.0));
        assert_eq!(item_on_screen(&f.list, 5), Some(true), "the top sliver still intersects the viewport");
        assert_eq!(item_in_view(&f.list, 5), Some(false), "one visible pixel at the top is insufficient for a quote jump");
        let next = f.cx.next_frame_id();
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [46; 32], [6; 32]), "jumping to the top sliver arms the flash");
        assert_eq!(f.list.is_smooth_scrolling(), Some(5), "the top sliver smooth-scrolls to row 5");
        f.settle_scroll(next, 1.0);
        assert_eq!(item_in_view(&f.list, 5), Some(true), "the top-clipped target settles fully in view");
        assert!((f.list.drawn_slot(5).unwrap().start - JUMP_TOP_OFFSET).abs() < 0.5, "the top-clipped target settles at 24 px");

        f.anchor(18, viewport_h - 1.0);
        assert_eq!(item_on_screen(&f.list, 18), Some(true), "the bottom sliver still intersects the viewport");
        assert_eq!(item_in_view(&f.list, 18), Some(false), "one visible pixel at the bottom is insufficient for a quote jump");
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [1; 32], [19; 32]), "jumping to the bottom sliver arms the flash");
        assert_eq!(f.list.is_smooth_scrolling(), None, "the bottom sliver snaps without smooth scrolling");
        assert_eq!(f.list.first_id(), 18, "the bottom-sliver jump anchors directly on row 18");
        assert_eq!(f.list.scroll_position(), JUMP_TOP_OFFSET, "the bottom-sliver anchor leaves 24 px of context");
        f.draw();
        assert_eq!(item_in_view(&f.list, 18), Some(true), "the bottom-clipped target is fully visible after the snap");
        assert!((f.list.drawn_slot(18).unwrap().start - JUMP_TOP_OFFSET).abs() < 0.5, "the bottom-clipped target lands at 24 px");

        f.anchor(25, 0.);
        assert_eq!(f.list.first_id(), 25, "the smooth-scroll boundary starts 20 rows from the target");
        let next = f.cx.next_frame_id();
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [46; 32], [6; 32]), "a 20-row jump arms the flash");
        assert_eq!(f.list.is_smooth_scrolling(), Some(5), "a 20-row jump smooth-scrolls to row 5");
        assert_eq!(f.list.first_id(), 25, "a 20-row jump preserves the starting row before scrolling");
        f.settle_scroll(next, 2.0);
        assert_eq!(item_in_view(&f.list, 5), Some(true), "the 20-row jump settles fully in view");
        assert!((f.list.drawn_slot(5).unwrap().start - JUMP_TOP_OFFSET).abs() < 0.5, "the 20-row jump settles at 24 px");

        f.anchor(26, 0.);
        assert_eq!(f.list.first_id(), 26, "the direct-jump boundary starts 21 rows from the target");
        assert!(f.data.jump_to_chat_message(&mut f.cx, &f.chat, &f.list, [46; 32], [6; 32]), "a 21-row jump arms the flash");
        assert_eq!(f.list.is_smooth_scrolling(), None, "a 21-row jump snaps without smooth scrolling");
        assert_eq!(f.list.first_id(), 5, "a 21-row jump anchors directly on row 5");
        assert_eq!(f.list.scroll_position(), JUMP_TOP_OFFSET, "the 21-row jump leaves 24 px of context");
        f.draw();
        assert_eq!(item_in_view(&f.list, 5), Some(true), "the 21-row jump brings the target fully into view");
    }

    fn pending_requests(second: usize) -> ChatFixture {
        let rows = (1..=18).map(|id| message(id, "Peer", "First line\nSecond line\nThird line", false)).collect();
        let mut f = ChatFixture::new([1; 32], rows);
        for (index, name) in [(2, "Ben"), (second, "Cam")] {
            add_request(&mut f, index, name);
        }
        f.sync();
        f.list.scroll_to_end(&mut f.cx);
        f.draw_and_scroll();
        f
    }

    fn add_request(f: &mut ChatFixture, index: usize, name: &str) {
        use crate::view::{AgentControl, ApprovalRow};
        let ChatRow::Message(m) = &mut f.data.workspace.rows[index] else { panic!("expected message") };
        m.name = format!("{name} asked your AI");
        m.mentions_you = true;
        let decide = |allow| AgentControl::Decide { town: [1; 32], agent: [18; 32], line: m.id, allow };
        f.data.workspace.connect.requests.insert(index, ApprovalRow {
            cue: m.name.clone(),
            allow: decide(true), deny: decide(false), label: "Let your AI do it".into(),
        });
    }

    #[test]
    fn offscreen_pending_approval_cue_tracks_viewport_and_show_scrolls_to_row() {
        use crate::lists::{Form, Pane};
        let mut f = pending_requests(15);
        let bar = f.chat.widget(&f.cx, ids!(pending_approval));
        let label = bar.label(&f.cx, ids!(pending_approval_text));
        let show = bar.button(&f.cx, ids!(pending_approval_show));
        assert!(f.list.is_at_end() && f.data.chat_read.at_live_tail());
        assert!(bar.visible() && label.text() == "Ben asked your AI", "the earliest off-screen request names its author once");
        let actions = f.draw_and_scroll();
        assert!(!actions.iter().filter_map(|a| a.as_widget_action())
            .any(|a| a.action.downcast_ref::<PendingApprovalVisibility>().is_some()) && bar.visible(),
            "steady visibility emits no cue action");
        assert!(f.list.is_at_end(), "drawing Show keeps the newest line at the bottom in one draw");

        f.list.set_tail_range(false);
        f.list.set_first_id_and_scroll(2, 0.0);
        f.draw_and_scroll();
        assert!(!bar.visible(), "the inline approval is on screen");
        {
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut f.cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&f.pass, None);
            f.ink.begin_always(&mut frame);
            f.ink.end(&mut frame);
            frame.end_pass(&f.pass);
        }
        assert!(!f.list.get_item(2).unwrap().1.area().is_valid(&f.cx));
        f.sync_cue();
        assert!(!bar.visible(), "scroll visibility survives draw-list invalidation");

        f.list.set_tail_range(true);
        f.list.scroll_to_end(&mut f.cx);
        f.draw_and_scroll();
        assert!(bar.visible());
        f.data.workspace.rows.remove(0);
        let requests = &mut f.data.workspace.connect.requests;
        for i in [2, 15] {
            let request = requests.remove(&i).unwrap();
            requests.insert(i - 1, request);
        }
        f.sync();
        f.sync_cue();
        assert_eq!(f.data.pending_approval_item_id(1, [3; 32]), Some(1));
        assert!(bar.visible() && label.text() == "Ben asked your AI");
        f.draw_and_scroll();
        let next = f.cx.next_frame_id();
        let at = show.area().rect(&f.cx).center();
        f.down(at, MouseButton::PRIMARY, 1.);
        let click = f.release_show(at, 1.1);
        f.scroll_actions(&click);
        assert!(!f.data.chat_read.at_live_tail() && f.data.chat_read.user_scrolled_up() && !f.data.chat_read.open_at_tail);
        assert_eq!(f.list.is_smooth_scrolling(), Some(1), "Show follows Ben through a retraction");
        f.settle_scroll(next, 1.2);
        assert!(!f.list.is_at_end() && !f.data.chat_read.at_live_tail() && f.data.chat_read.user_scrolled_up());
        assert_eq!(item_on_screen(&f.list, 1), Some(true));
        assert!(!bar.visible());
        f.data.workspace.rows.push(message(19, "Peer", "Newest", false));
        f.sync();
        f.draw_and_scroll();
        assert!(!f.list.is_at_end() && f.list.first_id() <= 1, "new chat leaves the reader at the request");
        f.data.workspace.connect.requests.remove(&1);
        f.sync_cue();
        assert!(bar.visible() && label.text() == "Cam asked your AI", "the next off-screen request takes its place");

        f.data.form = Form::Narrow;
        f.data.pane = Pane::Places;
        f.sync_cue();
        assert!(!bar.visible());
        f.data.pane = Pane::Chat;
        f.sync_cue();
        assert!(bar.visible());

        f.data.pane = Pane::Places;
        f.draw_and_scroll();
        assert!(!bar.visible(), "the draw hides Cam's cue when chat is hidden");
        f.data.pane = Pane::Chat;
        f.sync_cue();
        assert!(bar.visible(), "sync can restore the cue between draws");
        f.data.pane = Pane::Places;
        let actions = f.draw_and_scroll();
        assert!(!f.list.scrolled(&actions), "the viewport stayed in place");
        assert!(actions.iter().filter_map(|a| a.as_widget_action())
            .any(|a| a.action.downcast_ref::<PendingApprovalVisibility>().is_some()) && !bar.visible(),
            "the intervening cue change re-arms the hide announcement without a scroll");
        f.data.form = Form::Wide;
        f.data.pane = Pane::Chat;

        f.data.workspace.rows.truncate(2);
        f.data.workspace.connect.requests.clear();
        f.data.chat_read.mark_at_tail();
        f.list.set_tail_range(true);
        f.sync();
        f.list.scroll_to_end(&mut f.cx);
        f.draw_and_scroll();
        f.data.workspace.rows.push(message(20, "Peer", "New request", false));
        add_request(&mut f, 2, "Cam");
        f.sync();
        f.sync_cue();
        assert!(!bar.visible(), "a short chat at the tail does not flash a cue before draw");
        f.draw_and_scroll();
        assert_eq!(item_on_screen(&f.list, 2), Some(true));
        assert!(!bar.visible());
        f.data.workspace.connect.requests.clear();
        f.sync_cue();
        assert!(!bar.visible(), "clearing requests hides the cue");
    }

    fn retract_during_show_click(while_held: bool) {
        let mut f = pending_requests(3);
        f.draw_and_scroll();
        let bar = f.chat.widget(&f.cx, ids!(pending_approval));
        let label = bar.label(&f.cx, ids!(pending_approval_text));
        let show = bar.button(&f.cx, ids!(pending_approval_show));
        let town: Id = [1; 32];
        let ben = (Some(town), [3u8; 32]);
        let cam = (Some(town), [4u8; 32]);
        let bound = || show.action_data().as_deref().and_then(|d| d.downcast_ref::<(Option<Id>, Id)>().copied());
        assert!(f.list.is_at_end() && f.data.chat_read.at_live_tail());
        assert_eq!((item_on_screen(&f.list, 2), item_on_screen(&f.list, 3)), (Some(false), Some(false)));
        assert!(bar.visible() && label.text() == "Ben asked your AI" && bound() == Some(ben));
        let at = show.area().rect(&f.cx).center();
        let down = f.down(at, MouseButton::PRIMARY, 1.);
        assert!(down.filter_widget_actions(show.widget_uid()).any(|a|
            matches!(a.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Pressed(_)))
                && a.data.as_deref().and_then(|d| d.downcast_ref()) == Some(&ben)));
        assert!(!f.scroll_actions(&down));
        let released = (!while_held).then(|| f.release_show(at, 1.1));
        f.data.workspace.rows.remove(2);
        let requests = &mut f.data.workspace.connect.requests;
        requests.remove(&2).unwrap();
        let request = requests.remove(&3).unwrap();
        requests.insert(2, request);
        f.sync();
        f.sync_cue();
        assert!(bar.visible() && label.text() == "Cam asked your AI" && bound() == Some(cam));
        assert_eq!(f.data.chat_read.pending_cue_shown, Some(cam));
        let release = released.unwrap_or_else(|| f.release_show(at, 1.1));
        assert!(button_clicked_for(&show, &release, &ben), "the completed click belongs to Ben");
        assert!(!f.scroll_actions(&release));
        assert!(f.data.chat_read.at_live_tail() && !f.data.chat_read.user_scrolled_up() && f.data.chat_read.open_at_tail);
        assert!(f.list.is_at_end() && f.list.is_smooth_scrolling().is_none() && label.text() == "Cam asked your AI");

        f.draw_and_scroll();
        let next = f.cx.next_frame_id();
        let at = show.area().rect(&f.cx).center();
        f.down(at, MouseButton::PRIMARY, 2.);
        let fresh = f.release_show(at, 2.1);
        assert!(button_clicked_for(&show, &fresh, &cam));
        f.scroll_actions(&fresh);
        assert!(!f.data.chat_read.at_live_tail() && f.data.chat_read.user_scrolled_up() && !f.data.chat_read.open_at_tail);
        assert_eq!(f.list.is_smooth_scrolling(), Some(2), "a fresh click scrolls to Cam");
        f.settle_scroll(next, 2.2);
        assert!(!f.list.is_at_end() && !f.data.chat_read.at_live_tail() && f.data.chat_read.user_scrolled_up());
        assert_eq!(item_on_screen(&f.list, 2), Some(true));
        assert!(!bar.visible() && f.data.chat_read.pending_cue_shown.is_none());
        assert!(show.action_data().as_deref().is_some_and(|d| d.is::<()>()));
        assert!(!f.scroll_actions(&fresh) && f.list.is_smooth_scrolling().is_none(), "the hidden cue ignores a queued click");
    }

    #[test]
    fn stale_show_click_after_retraction_stays_at_tail_and_fresh_click_scrolls_to_next() {
        retract_during_show_click(false);
    }

    #[test]
    fn show_press_retracted_while_held_stays_at_tail_and_fresh_click_scrolls_to_next() {
        retract_during_show_click(true);
    }

    fn assert_menu(actions: &Actions, expected: &[&str]) {
        let menu = menu_actions(actions).find_map(|a| match a {
            MenuAction::Open { owner, rows, .. } if *owner == id!(chat_message_menu) => Some(rows),
            _ => None,
        });
        if expected.is_empty() {
            assert!(menu.is_none(), "the gesture must leave the menu closed");
        } else {
            assert_eq!(menu.expect("the gesture opens a menu").iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn queued_right_click_targets_original_message_across_same_turn_retraction() {
        let mut f = ChatFixture::new([8; 32], vec![message(1, "Ana", "Tea is ready.", true),
            message(2, "Ben", "On my way.", false), message(3, "Cam", "See you soon.", true)]);
        f.divider([2; 32]);
        f.draw();
        let at = f.center(1, ids!(message));
        let queued = f.down(at, MouseButton::SECONDARY, 1.);
        f.data.workspace.rows.remove(0);
        f.data.workspace.rows.push(message(4, "Dan", "Here.", true));
        f.divider([4; 32]);
        f.sync();
        assert_menu(&f.dispatch(&queued), &["Copy", "Reply"]);
        assert_eq!(f.data.chat_selected, Some([2; 32]), "the queued menu still belongs to Ben");
        f.up(at, MouseButton::SECONDARY, 1.1);

        f.data.chat_selected = None;
        f.divider([3; 32]);
        f.draw();
        let at = f.center(1, ids!(unread_line));
        let queued = f.down(at, MouseButton::SECONDARY, 2.);
        f.data.workspace.rows.remove(0);
        f.sync();
        assert_menu(&f.dispatch(&queued), &[]);
        assert!(f.data.chat_selected.is_none(), "a press on the divider stays inert");
        f.up(at, MouseButton::SECONDARY, 2.1);

        f.draw();
        let at = f.center(0, ids!(message));
        let queued = f.down(at, MouseButton::SECONDARY, 3.);
        f.data.workspace.rows.remove(0);
        f.sync();
        assert_menu(&f.dispatch(&queued), &[]);
        assert!(f.data.chat_selected.is_none(), "Dan cannot inherit Cam's menu");
        f.up(at, MouseButton::SECONDARY, 3.1);
    }

    #[test]
    fn touch_hold_targets_original_message_or_cancels_across_retraction() {
        let mut f = ChatFixture::new([8; 32], vec![message(1, "Ana", "Tea is ready.", true),
            message(2, "Ben", "On my way.", false), message(3, "Cam", "See you soon.", true)]);
        f.draw();
        let at = f.center(1, ids!(message));
        f.touch(TouchState::Start, 1, at, 1.);
        f.data.workspace.rows.remove(0);
        f.sync();
        assert_menu(&f.hold(1, at, 1.6), &["Copy", "Reply"]);
        assert_eq!(f.data.chat_selected, Some([2; 32]));
        f.touch(TouchState::Stop, 1, at, 1.7);

        f.data.chat_selected = None;
        f.draw();
        let at = f.center(0, ids!(message));
        f.touch(TouchState::Start, 2, at, 2.);
        f.data.workspace.rows.remove(0);
        f.sync();
        f.draw();
        assert_menu(&f.hold(2, at, 2.6), &[]);
        assert_menu(&f.touch(TouchState::Stop, 2, at, 2.7), &[]);
        assert!(f.data.chat_selected.is_none(), "a retracted press cannot select Cam");
    }

    #[test]
    fn my_own_pills_take_my_names_letter_while_the_label_says_you() {
        use crate::model::{tests::ShareHomes, Action, Model, Origin};
        let homes = ShareHomes::new("chat-own-pill");
        let mut model = Model::open(&homes.0).unwrap().0;
        model.set_person_name("okp").unwrap();
        let line = model.post_chat_draft("my pill says Y for you?", None, None, None).unwrap();
        model.act(Action::AddProject { title: "Ours".into() }, Origin::Ui).unwrap();
        model.act(Action::AddTask { title: "Mine".into(), text: String::new() }, Origin::Ui).unwrap();
        let mut f = ChatFixture::new(model.open_id(), Vec::new());
        // A rename moves the letter along; the label stays "You" throughout.
        for (name, letter) in [("okp", "O"), ("Ren", "R")] {
            model.set_person_name(name).unwrap();
            f.data.workspace = Workspace::read(&model, false, &Default::default());
            f.draw();
            let row = f.list.get_item(message_index(&f.data.workspace, line)).expect("my line was drawn").1;
            assert_eq!(row.label(&f.cx, ids!(name)).text(), "You", "my line keeps the You label");
            assert!(row.widget(&f.cx, ids!(author.you)).visible(), "my line keeps my own pill");
            assert_eq!(row.label(&f.cx, ids!(author.you.initial)).text(), letter, "my pill shows {name}'s letter, never the Y of You");
            let card = f.data.workspace.projects.iter().flat_map(|p| p.columns.iter().flatten())
                .find(|c| c.title == "Mine").expect("my task card");
            assert_eq!(card.author_name.as_ref(), name, "my task card's pill names me too");
        }
    }

    #[test]
    fn day_dividers_coexist_with_unread_strip_retraction_copy_and_pending_cue() {
        use crate::model::{tests::ShareHomes, Model};
        use crate::view::LocalClock;
        let homes = ShareHomes::new("chat-day-selection");
        let mut model = Model::open(&homes.0).unwrap().0;
        let town = model.open_id();
        let clock = LocalClock::default();
        let now = 1_795_867_200_000;
        let yesterday = model.post_chat_draft("Yesterday's note", None, None, None).unwrap();
        let read = |model: &mut Model| {
            for line in &mut model.state.chat {
                line.wall_ms = if line.id == yesterday { now - 86_400_000 } else { now };
            }
            Workspace::read_chat_at(model, false, &clock, &Default::default(), &Default::default(), now, &Default::default())
        };
        let mut f = ChatFixture::new(town, Vec::new());
        f.data.workspace = read(&mut model);
        f.sync();
        let first = model.post_chat_draft("Morning note", None, None, None).unwrap();
        f.data.workspace = read(&mut model);
        assert_eq!(f.sync(), 0, "a new day and its first message append without a splice");
        for i in 0..12 { model.post_chat_draft(&format!("More notes {i}\nSecond line\nThird line"), None, None, None).unwrap(); }
        f.data.workspace = read(&mut model);
        assert_eq!(f.sync(), 0, "later messages on the same day append without a splice");
        f.divider(first);
        f.anchor(0, 0.);
        let old = message_index(&f.data.workspace, yesterday);
        let selected = message_index(&f.data.workspace, first);
        let day = selected - 1;
        let label = f.child(day, ids!(day_label));
        let strip = f.child(selected, ids!(unread_line));
        assert_eq!(label.text(), "Today");
        assert!(strip.visible() && label.area().rect(&f.cx).center().y < strip.area().rect(&f.cx).center().y);
        let at = label.area().rect(&f.cx).center();
        assert_eq!(f.list.item_at(&f.cx, at), Some(day));
        for (time, button) in [(0., MouseButton::PRIMARY), (0.2, MouseButton::SECONDARY)] {
            let actions = f.down(at, button, time);
            assert_menu(&f.dispatch(&actions), &[]);
            f.up(at, button, time + 0.05);
            assert!(f.data.chat_selected.is_none() && !f.list.has_selection());
        }
        let endpoint = |index, end| {
            let body = f.child(index, ids!(message));
            let rect = body.area().rect(&f.cx);
            let offset = if end { body.selection_text_len() } else { 0 };
            (0..(rect.size.x * 2.) as usize).map(|x| dvec2(rect.pos.x + x as f64 * 0.5, rect.center().y))
                .find(|&p| body.selection_point_to_char_index(&f.cx, p) == Some(offset)).unwrap()
        };
        let (start, end) = (endpoint(old, false), endpoint(selected, true));
        let actions = f.down(start, MouseButton::PRIMARY, 0.4);
        f.dispatch(&actions);
        let actions = f.input(Event::MouseMove(MouseMoveEvent { abs: end, time: 0.5, window_id: ChatFixture::WINDOW,
            lock_delta: Default::default(), modifiers: Default::default(), handled: Default::default() }));
        f.dispatch(&actions);
        f.up(end, MouseButton::PRIMARY, 0.6);
        let expected = [old, selected].map(|i| message_text(f.data.message_at(i).unwrap())).join("\n");
        assert_eq!(f.copy(), Some(expected), "dragging across the day and unread lines copies only messages");

        f.anchor(selected, -4.);
        f.select_words(selected, "Morning");
        let scroll = f.list.scroll_position();
        model.delete_message(yesterday).unwrap();
        f.data.workspace = read(&mut model);
        assert_eq!(f.sync(), 1, "the sole message of yesterday and its divider leave together");
        f.draw();
        let selected = message_index(&f.data.workspace, first);
        assert_eq!(selected, 1, "only today's divider remains before the message");
        let midnight = (now / 1000 + clock.next_day_check_at(now / 1000)) * 1000;
        for after_midnight in [false, true] {
            if after_midnight {
                assert!(f.data.workspace.refresh_day_labels(&clock, midnight));
                assert_eq!(f.sync(), 0, "relabeling the day preserves all keyed rows");
                f.draw();
            }
            assert_eq!(f.list.first_id(), selected);
            assert_eq!(f.list.scroll_position(), scroll);
            assert_eq!(f.list.selected_entries(), Some((selected, selected)));
            assert_eq!(f.copy().as_deref(), Some("Morning"));
            assert_eq!(f.data.chat_read.towns[&town].divider_before, Some(first));
        }
        f.anchor(0, 0.);
        assert_eq!(f.child(selected - 1, ids!(day_label)).text(), "Yesterday");

        add_request(&mut f, selected, "Ben");
        f.list.set_tail_range(true);
        f.list.scroll_to_end(&mut f.cx);
        f.draw_and_scroll();
        let bar = f.chat.widget(&f.cx, ids!(pending_approval));
        assert!(bar.visible());
        assert_eq!(item_on_screen(&f.list, selected), Some(false));
        f.draw_and_scroll(); // Lay out the Show button revealed by the preceding scroll actions.
        let next = f.cx.next_frame_id();
        let at = bar.button(&f.cx, ids!(pending_approval_show)).area().rect(&f.cx).center();
        f.down(at, MouseButton::PRIMARY, 2.);
        let click = f.release_show(at, 2.1);
        f.scroll_actions(&click);
        assert_eq!(f.list.is_smooth_scrolling(), Some(selected));
        f.settle_scroll(next, 2.2);
        assert_eq!(item_on_screen(&f.list, selected), Some(true));
        assert!(!bar.visible());
    }

    #[test]
    fn text_selection_and_viewport_survive_middle_retraction_and_strip_move() {
        use crate::model::{tests::ShareHomes, Model};
        let homes = ShareHomes::new("chat-retract");
        let mut model = Model::open(&homes.0).unwrap().0;
        let mut f = ChatFixture::new(model.open_id(), Vec::new());
        let ids: Vec<_> = (0..12).map(|i| model.post_chat_draft(
            &format!("Message {i}\nSecond line\nThird line"), None, None, None).unwrap()).collect();
        let read = |model: &Model| Workspace::read(model, false, &Default::default());
        f.data.workspace = read(&model);
        f.divider(ids[1]);
        f.draw();
        let selected = message_index(&f.data.workspace, ids[2]);
        f.anchor(selected, 0.);
        assert_eq!(f.list.first_id(), selected);
        assert!(!f.list.is_at_end());
        f.select_words(selected, "Message 2");

        model.delete_message(ids[1]).unwrap();
        f.data.workspace = read(&model);
        f.divider(ids[2]);
        f.sync();
        f.draw();
        let selected = message_index(&f.data.workspace, ids[2]);
        assert_eq!(f.list.first_id(), selected);
        assert_eq!(f.list.selected_entries(), Some((selected, selected)));
        assert_eq!(f.copy().as_deref(), Some("Message 2"));
        let strip = f.child(selected, ids!(unread_line));
        assert!(strip.visible() && strip.selection_text_len() == 0);
        let height = strip.area().rect(&f.cx).size.y;
        assert!(height > 0.);
        f.anchor(selected, -height);
        model.delete_message(ids[0]).unwrap();
        let newest = model.post_chat_draft("Message 12", None, None, None).unwrap();
        f.data.workspace = read(&model);
        f.divider(newest);
        f.sync();
        f.draw();
        let selected = message_index(&f.data.workspace, ids[2]);
        assert_eq!(f.list.first_id(), selected);
        assert!(f.list.scroll_position().abs() < 0.5, "the surviving header keeps its position");
        assert!(!f.child(selected, ids!(unread_line)).visible());
        assert_eq!(f.list.selected_entries(), Some((selected, selected)));
        assert_eq!(f.copy().as_deref(), Some("Message 2"));

        model.delete_message(ids[2]).unwrap();
        f.data.workspace = read(&model);
        f.sync();
        f.draw();
        assert!(!f.list.has_selection() && f.list.selected_entries().is_none() && f.copy().is_none());
    }

    #[test]
    fn renaming_duplicate_text_member_notice_preserves_selection_and_anchor() {
        let mut rows = vec![notice(10, "Ana added Peer's computer"), message(1, "Ben", "Selected message B\nLine 2\nLine 3", true),
            notice(20, "Ana added Peer's computer")];
        rows.extend((3..12).map(|i| message(i, "Ana", "Another message\nLine 2\nLine 3", false)));
        let mut f = ChatFixture::new([8; 32], rows);
        f.draw();
        f.anchor(1, -4.);
        f.select_words(1, "Selected message B");
        let uid = f.list.get_item(1).unwrap().1.widget_uid();
        let ChatRow::Member(n) = &mut f.data.workspace.rows[0] else { panic!("expected an admission") };
        n.text = "Zoe added Peer's computer".into();
        assert_eq!(f.sync(), 0, "renaming the admission keeps every row");
        f.draw();
        assert_eq!(f.list.first_id(), 1);
        assert!((f.list.scroll_position() + 4.).abs() < 0.5);
        assert_eq!(f.list.get_item(1).unwrap().1.widget_uid(), uid);
        assert_eq!(f.list.selected_entries(), Some((1, 1)));
        assert_eq!(f.copy().as_deref(), Some("Selected message B"));
        f.anchor(0, 0.);
        assert_eq!(f.list.get_item(0).unwrap().1.text(), "Zoe added Peer's computer");
        assert_eq!(f.list.get_item(2).unwrap().1.text(), "Ana added Peer's computer");
    }

    #[test]
    fn linear_reconciliation_handles_many_notices_and_middle_edits_without_displacing_survivors() {
        let rows = (0..200).flat_map(|i| [notice(i, "Same notice"), message(i, "Ana", &format!("Message {i}"), false)]).collect();
        let mut f = ChatFixture::new([8; 32], rows);
        f.sync();
        f.anchor(301, 0.);
        let uid = f.list.get_item(301).unwrap().1.widget_uid();
        f.select_words(301, "Message 150");
        let rows = &mut f.data.workspace.rows;
        for i in (0..50).rev() { rows.remove(i * 2); }
        rows.insert(10, message(200, "Ben", "Arrived later", false));
        rows.drain(260..270);
        rows.extend([message(201, "Cam", "At the tail", false), message(202, "Dan", "Last message", false)]);
        f.sync();
        f.draw();
        assert_eq!(f.list.borrow().unwrap().range_end(), 343);
        assert_eq!(f.data.chat_read.rendered_rows, f.data.workspace.rows.iter().map(ChatRow::key).collect::<Vec<_>>());
        assert_eq!(f.list.first_id(), 252);
        assert_eq!(f.list.get_item(252).unwrap().1.widget_uid(), uid);
        assert_eq!(f.list.selected_entries(), Some((252, 252)));
        assert_eq!(f.copy().as_deref(), Some("Message 150"));

        let rows = &mut f.data.workspace.rows;
        *rows = (0..1999).map(|i| if i == 1000 { message(203, "Ben", "Survivor", false) } else { notice(99, "Same notice") }).collect();
        f.sync();
        f.anchor(1000, 0.);
        let uid = f.list.get_item(1000).unwrap().1.widget_uid();
        f.data.workspace.rows.insert(0, message(204, "Cam", "First", false));
        f.data.workspace.rows.push(message(205, "Dan", "Last", false));
        assert_eq!(f.sync(), 1, "one insertion splice and a tail append");
        assert_eq!(f.list.borrow().unwrap().range_end(), 2001);
        assert_eq!(f.list.first_id(), 1001);
        assert_eq!(f.list.get_item(1001).unwrap().1.widget_uid(), uid);
    }

    #[test]
    fn town_switch_cancels_held_touch_and_rejects_stale_town_gestures() {
        let (town_a, town_b) = ([0xA0; 32], [0xB0; 32]);
        let mut f = ChatFixture::new(town_a, vec![message(1, "Ana", "First", true), message(2, "Ben", "Second", true)]);
        f.draw();
        let at = f.center(1, ids!(message));
        f.touch(TouchState::Start, 1, at, 1.);
        f.data.chat_read.mark_at_tail();
        f.data.chat_read.reset_rendered_rows();
        f.data.chat_selected = None;
        f.list.stop_scrolling();
        f.list.set_tail_range(true);
        f.list.borrow_mut().unwrap().clear_selection(&mut f.cx);
        f.data.tree.set(Vec::new(), crate::model::tree::TownTree { town: Some(town_b), ..Default::default() });
        f.data.workspace.rows = vec![message(3, "Cam", "Third", true), message(4, "Dan", "Fourth", true)];
        assert_eq!(f.sync(), 1, "switching equal-length towns replaces the entire range");
        f.list.scroll_to_end(&mut f.cx);
        f.draw();
        assert_menu(&f.hold(1, at, 1.6), &[]);
        assert_menu(&f.touch(TouchState::Stop, 1, at, 1.7), &[]);
        assert!(f.data.chat_selected.is_none() && f.chat.menu_layer(&f.cx, ids!(menus)).open_owner().is_none());

        let at = f.center(1, ids!(message));
        let queued = f.down(at, MouseButton::SECONDARY, 2.);
        assert!(f.list.item_actions(&queued).any(|a| matches!(a, PortalListAction::ItemContextMenu { item_id: 1, .. })));
        f.data.tree.set(Vec::new(), crate::model::tree::TownTree { town: Some(town_a), ..Default::default() });
        f.data.chat_error = Some(([3; 32], "keep".into()));
        assert_eq!(f.sync(), 1, "the recorded town detects a switch even with identical keys");
        assert_menu(&f.dispatch(&queued), &[]);
        f.up(at, MouseButton::SECONDARY, 2.1);
        assert!(f.data.chat_selected.is_none());
        assert_eq!(f.data.chat_error, Some(([3; 32], "keep".into())), "a stale gesture leaves this town alone");
    }

    #[test]
    fn divider_press_stays_inert_after_retraction_and_tail_follow_shift() {
        let m1 = || message(2, "Ana", "One line", true);
        let mut f = ChatFixture::new([0xD0; 32], vec![message(1, "Ana", "First", true), m1(),
            message(3, "Ben", "On my way.\nSee you soon.", false), message(4, "Cam", "Last", true)]);
        f.divider([3; 32]);
        f.sync();
        f.anchor(0, 0.);
        for uid in 1..=3 {
            if uid > 1 {
                f.data.workspace.rows.insert(1, m1());
                f.sync();
                f.draw();
            }
            let body = uid == 3;
            let at = f.center(2, if body { ids!(message) } else { ids!(unread_line) });
            let start = uid as f64;
            f.touch(TouchState::Start, uid, at, start);
            f.data.workspace.rows.remove(1);
            f.sync();
            f.draw();
            assert_eq!(f.data.workspace.rows[1].key(), ChatRowKey::Message([3; 32]));
            if !body {
                let strip = f.child(1, ids!(unread_line));
                assert!(strip.visible() && !strip.point_hits_area(&f.cx, at));
                assert_eq!(f.list.item_at(&f.cx, at), Some(1), "the finger now rests on Ben's body");
            }
            let held = uid != 2;
            if held {
                assert_menu(&f.hold(uid, at, start + 0.6), if body { &["Copy", "Reply"] } else { &[] });
            }
            assert_menu(&f.touch(TouchState::Stop, uid, at, start + if held { 0.7 } else { 0.1 }), &[]);
            assert_eq!(f.data.chat_selected, body.then_some([3; 32]), "only the body hold selects Ben");
            assert!(f.chat.menu_layer(&f.cx, ids!(menus)).open_owner().is_none());
        }

        f.data.chat_selected = None;
        let line = |i| message(i, "Ana", &format!("Message {i}\nSecond line\nThird line"), false);
        f.data.workspace.rows = (0..12).map(line).collect();
        f.divider([10; 32]);
        f.sync();
        f.list.set_tail_range(true);
        f.list.scroll_to_end(&mut f.cx);
        f.draw();
        f.draw();
        assert!(f.list.is_at_end());
        let strip = f.child(10, ids!(unread_line));
        let at = strip.area().rect(&f.cx).center();
        assert!(strip.visible() && strip.point_hits_area(&f.cx, at));
        f.touch(TouchState::Start, 4, at, 4.);
        f.data.workspace.rows.extend([line(12), line(13)]);
        assert_eq!(f.sync(), 0);
        f.draw();
        f.draw();
        assert!(f.list.get_item(10).is_none_or(|(_, w)| !w.widget(&f.cx, ids!(unread_line)).point_hits_area(&f.cx, at)));
        assert_menu(&f.hold(4, at, 4.6), &[]);
        assert_menu(&f.touch(TouchState::Stop, 4, at, 4.7), &[]);
        assert!(f.data.chat_selected.is_none() && f.chat.menu_layer(&f.cx, ids!(menus)).open_owner().is_none());
    }

    #[test]
    fn selecting_chat_text_and_messages_shares_copy_and_context_actions() {
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let chat = cx.with_vm(|vm| {
            let value = vm.eval(script! { use mod.widgets.* use mod.prelude.widgets.*
                View{flow: Overlay conversation := ChatConversation{} menus := MenuLayer{}}
            });
            WidgetRef::script_from_value(vm, value)
        });
        let menus = chat.menu_layer(cx, ids!(menus));
        let list = chat.portal_list(cx, ids!(transcript.list));
        let mut data = ViewData::default();
        data.workspace.writable = true;
        for (id, name, time, words) in [(1, "Ana", "14:02", "Tea is ready."), (2, "Ben", "14:03", "On my way.")] {
            let quote = (id == 2).then(|| crate::view::Quote { id: [1; 32], name: "Ana".into(), text: "Tea is ready.".into() });
            data.workspace.rows.push(ChatRow::Message(Message { id: [id; 32], quote, seat: crate::view::Seat::Peer, tone: None,
                name: name.into(), badge: name.into(), time: time.into(), text: words.into(), traces: vec![],
                addressed: false, unheard: false, mention: false, mentions_you: false, deletable: id == 1 }));
        }
        let path = PathBuf::from("picture");
        for (id, arrived) in [(3, true), (4, false)] {
            data.workspace.rows.push(ChatRow::Clip(Arc::new(ClipLine {
                id: ClipId([0; 32], [id; 32]), title: String::new(),
                media: Some(crate::view::ClipMedia { is_picture: true, blob: [id; 32], length: String::new(),
                    size: String::new(), poster: None, height: 60., state: String::new(),
                    get: !arrived, playable: arrived.then(|| path.clone()) }),
            })));
        }
        let pass = DrawPass::new(cx);
        let size = dvec2(360., 480.);
        pass.set_size(cx, size);
        let mut ink = DrawList2d::new(cx);
        let mut draw = |cx: &mut Cx, data: &mut ViewData| {
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            ink.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            chat.draw_all(&mut frame, &mut Scope::with_data(data));
            frame.end_pass_sized_turtle();
            ink.end(&mut frame);
            frame.end_pass(&pass);
        };
        draw(cx, &mut data);
        let ana_index = message_index(&data.workspace, [1; 32]);
        let ben_index = message_index(&data.workspace, [2; 32]);
        let ana = list.get_item(ana_index).expect("Ana's row was drawn").1;
        let ben = list.get_item(ben_index).expect("Ben's row was drawn").1;
        let geometry = |cx: &Cx| [ben.clone(), ben.widget(cx, ids!(row)), ben.widget(cx, ids!(author)),
            ben.widget(cx, ids!(bubble)), ben.widget(cx, ids!(message))].map(|widget| widget.area().rect(cx));
        let before_mark = geometry(cx);
        let bar = ben.view(cx, ids!(mention_bar));
        assert!(!bar.borrow().unwrap().show_bg);
        let ChatRow::Message(m) = &mut data.workspace.rows[ben_index] else { unreachable!() };
        m.mentions_you = true;
        draw(cx, &mut data);
        assert_eq!(geometry(cx), before_mark, "marking Ben leaves the row, author, bubble and text in place");
        assert!(bar.borrow().unwrap().show_bg);
        let mark = bar.area().rect(cx).size;
        assert_eq!(mark.x, 2., "the accent is two pixels wide");
        assert!(before_mark[1].size.y > 0. && (mark.y - before_mark[1].size.y).abs() < 0.5,
            "the accent spans the message row");
        let quote = ben.widget(cx, ids!(quote_text));
        assert!(ben.widget(cx, ids!(quote)).visible() && quote.text() == "Ana: Tea is ready." && quote.selection_text_len() == 0,
            "the reply shows the original line outside the selectable text");
        let body_point = |cx: &Cx, row: &WidgetRef, end: bool| {
            let label = row.widget(cx, ids!(message));
            let r = label.area().rect(cx);
            let offset = if end { label.selection_text_len() } else { 0 };
            (0..(r.size.x * 2.) as usize)
                .map(|x| dvec2(r.pos.x + x as f64 * 0.5, r.center().y))
                .find(|&p| label.selection_point_to_char_index(cx, p) == Some(offset))
                .expect("the drag endpoint must be on the first or last glyph")
        };
        let send = |cx: &mut Cx, data: &mut ViewData, event: Event| {
            let mut actions = cx.capture_actions(|cx| {
                data.chat_key(cx, &list, &event);
                chat.handle_event(cx, &event, &mut Scope::with_data(data));
            });
            let mut command = None;
            let follow = cx.capture_actions(|cx| { command = data.chat_actions(cx, &list, &actions); });
            actions.extend(follow);
            (actions, command)
        };
        let win = WindowId(1, 1);
        let down = |cx: &mut Cx, data: &mut ViewData, abs: DVec2, button: MouseButton, time| {
            cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown { x: abs.x, y: abs.y, time,
                button_raw_bits: button.bits(), ..Default::default() }), win, DVec2::default());
            send(cx, data, Event::MouseDown(MouseDownEvent { abs, button, window_id: win, time,
                modifiers: Default::default(), handled: Default::default() })).0
        };
        let release = |cx: &mut Cx, data: &mut ViewData, abs, button: MouseButton, time| {
            let result = send(cx, data, Event::MouseUp(MouseUpEvent { abs, button, window_id: win, time, modifiers: Default::default() }));
            // Finish the platform's capture and focus cycle on the pooled context.
            cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp { x: abs.x, y: abs.y, time,
                button_raw_bits: button.bits(), ..Default::default() }), win, DVec2::default());
            result
        };
        let up = |cx: &mut Cx, data: &mut ViewData, abs, button, time| release(cx, data, abs, button, time).1;
        let drag = |cx: &mut Cx, data: &mut ViewData, time| {
            let start = body_point(cx, &ana, false);
            let end = body_point(cx, &ben, true);
            down(cx, data, start, MouseButton::PRIMARY, time);
            send(cx, data, Event::MouseMove(MouseMoveEvent { abs: end, lock_delta: Default::default(),
                window_id: win, time: time + 0.1, modifiers: Default::default(), handled: Default::default() }));
            up(cx, data, end, MouseButton::PRIMARY, time + 0.2);
        };
        let copy = |cx: &mut Cx, data: &mut ViewData| {
            let request = TextClipboardEvent::default();
            send(cx, data, Event::TextCopy(request.clone()));
            request.response.take()
        };
        let menu = |cx: &mut Cx, data: &mut ViewData, at, time, id, expected: &[&str]| {
            let actions = down(cx, data, at, MouseButton::SECONDARY, time);
            if expected.is_empty() {
                assert!(!menu_actions(&actions).any(|a| matches!(a, MenuAction::Open { .. })), "this row has no context menu");
            } else {
                assert!(menu_actions(&actions).any(|a| matches!(a, MenuAction::Open { owner, rows, anchor, place: MenuPlace::At }
                    if *owner == id!(chat_message_menu) && anchor.pos == at && rows.iter().map(|r| r.label.as_str()).eq(expected.iter().copied())
                        && rows.iter().all(|r| (r.label == "Delete") == r.hint.starts_with("Removed from view for everyone.")))),
                    "right click must open the available actions at the press point, Delete saying what it does: {expected:?}");
            }
            assert_eq!(data.chat_selected, Some([id; 32]), "right click selects the pressed row");
            up(cx, data, at, MouseButton::SECONDARY, time + 0.1);
            actions
        };

        let both = "Ana 14:02\nTea is ready.\nBen 14:03\nOn my way.";
        drag(cx, &mut data, 0.);
        assert_eq!(copy(cx, &mut data).as_deref(), Some(both), "the accent adds no characters to a selection");
        list.borrow_mut().unwrap().clear_selection(cx);
        let ChatRow::Message(m) = &mut data.workspace.rows[ben_index] else { unreachable!() };
        m.mentions_you = false;
        draw(cx, &mut data);
        drag(cx, &mut data, 1.);
        assert_eq!(list.selected_entries(), Some((ana_index, ben_index)), "a desktop drag must select across both message rows");
        assert_eq!(copy(cx, &mut data).as_deref(), Some(both), "copy must join the selected words with each author and time");
        let at = body_point(cx, &ben, false);
        let unselected = ben.area().rect(cx);
        down(cx, &mut data, at, MouseButton::PRIMARY, 2.);
        up(cx, &mut data, at, MouseButton::PRIMARY, 2.1);
        draw(cx, &mut data);
        assert_eq!(data.chat_selected, Some([2; 32]), "a click without travel must select Ben");
        assert!(text_range(&list).is_none(), "choosing Ben must clear the prior text selection");
        assert_eq!(ben.area().rect(cx), unselected, "selecting a message outlines it in place: nothing opens under it");
        let opened = menu(cx, &mut data, at, 2.5, 2, &["Copy", "Reply"]);
        send(cx, &mut data, Event::Actions(opened));
        send(cx, &mut data, Event::KeyDown(KeyEvent { key_code: KeyCode::KeyR, ..Default::default() }));
        let (_, command) = send(cx, &mut data, Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() }));
        assert_eq!(command, Some(ChatCommand::Reply([2; 32])), "choosing Reply from the menu targets the pressed message");
        assert_eq!(copy(cx, &mut data).as_deref(), Some("Ben 14:03\nOn my way."), "TextCopy must answer with the selected message");
        let delete_key = Event::KeyDown(KeyEvent { key_code: KeyCode::Backspace, ..Default::default() });
        assert_eq!(data.chat_key(cx, &list, &delete_key), None, "Backspace leaves another person's line alone");

        let at = body_point(cx, &ana, false);
        menu(cx, &mut data, at, 3., 1, &["Copy", "Reply", "Delete"]);
        assert_eq!(data.chat_key(cx, &list, &delete_key), Some(ChatCommand::Delete([1; 32])), "Backspace on my own selected line deletes it");
        drag(cx, &mut data, 4.);
        assert!(data.chat_selected.is_none(), "starting a text drag must clear whole-message selection");
        menu(cx, &mut data, at, 5., 1, &["Copy", "Copy selection", "Reply", "Delete"]);
        assert_eq!(copy(cx, &mut data).as_deref(), Some(both), "right click must preserve the text selection");
        send(cx, &mut data, Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() }));
        assert!(data.chat_selected.is_none() && text_range(&list).is_none(), "Escape with list focus must clear both selections");

        for (time, expected) in [(5.6, None), (5.7, Some("Tea")), (5.8, Some("Tea is ready."))] {
            down(cx, &mut data, at, MouseButton::PRIMARY, time);
            up(cx, &mut data, at, MouseButton::PRIMARY, time + 0.03);
            if let Some(expected) = expected {
                assert_eq!(list.borrow().unwrap().get_selected_text(), expected, "repeated clicks select a word then its paragraph");
                assert!(data.chat_selected.is_none(), "a word or paragraph selection supersedes the bubble");
                assert_eq!(copy(cx, &mut data).as_deref(), Some(expected), "words copied from one message carry no author line");
            }
        }
        let touch = |cx: &mut Cx, data: &mut ViewData, state, uid, abs: DVec2, time| {
            // Like a host synthesizing touch from a pointer, give the gesture its own tap count.
            if state == TouchState::Start {
                cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown { x: abs.x, y: abs.y, time,
                    button_raw_bits: MouseButton::PRIMARY.bits(), ..Default::default() }), win, DVec2::default());
            }
            let event = TouchUpdateEvent { time, window_id: win, modifiers: Default::default(),
                touches: vec![TouchPoint { state, uid, abs, time, force: 1., rotation_angle: 0., radius: dvec2(1., 1.),
                    handled: Default::default(), sweep_lock: Default::default() }] };
            let (actions, _) = send(cx, data, Event::TouchUpdate(event.clone()));
            cx.fingers.process_touch_update_end(&event.touches);
            if state == TouchState::Stop {
                cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp { x: abs.x, y: abs.y, time,
                    button_raw_bits: MouseButton::PRIMARY.bits(), ..Default::default() }), win, DVec2::default());
            }
            actions
        };
        touch(cx, &mut data, TouchState::Start, 1, at, 6.);
        touch(cx, &mut data, TouchState::Stop, 1, at, 6.1);
        assert_eq!(data.chat_selected, Some([1; 32]), "a short touch tap selects its message");
        touch(cx, &mut data, TouchState::Start, 2, at, 7.);
        let (open, _) = send(cx, &mut data, Event::LongPress(LongPressEvent { window_id: win, abs: at, uid: 2, time: 7.6 }));
        send(cx, &mut data, Event::Actions(open));
        assert_eq!(menus.open_owner(), Some(id!(chat_message_menu)), "a long press opens the same context menu");
        touch(cx, &mut data, TouchState::Stop, 2, at, 7.7);
        assert!(menus.is_open(), "lifting the long press leaves the menu open for a pick");
        let pick = at + dvec2(16., 16.);
        touch(cx, &mut data, TouchState::Start, 3, pick, 8.);
        let actions = touch(cx, &mut data, TouchState::Stop, 3, pick, 8.1);
        assert_eq!(menu_picked(&actions, id!(chat_message_menu)), Some(id!(copy)), "a tap on Copy picks it through the menu layer");

        let input = chat.text_input(cx, ids!(chat_input));
        let field = input.area().rect(cx).center();
        down(cx, &mut data, field, MouseButton::PRIMARY, 9.);
        up(cx, &mut data, field, MouseButton::PRIMARY, 9.1);
        assert!(cx.has_key_focus(input.area()), "the composer takes its own key focus");
        let request = TextClipboardEvent::default();
        data.chat_key(cx, &list, &Event::TextCopy(request.clone()));
        assert!(request.response.borrow().is_none(), "the transcript leaves the composer's copy request alone");
        send(cx, &mut data, Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() }));
        assert_eq!(data.chat_selected, Some([1; 32]), "Escape in the composer leaves the selected message alone");
        let space = list.area().rect(cx);
        let empty = space.pos + space.size - dvec2(16., 4.);
        down(cx, &mut data, empty, MouseButton::PRIMARY, 10.);
        up(cx, &mut data, empty, MouseButton::PRIMARY, 10.1);
        assert!(data.chat_selected.is_none(), "clicking empty list space clears the bubble");
        let enter = Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() });
        let open = || Some(ChatCommand::OpenPicture(ClipId([0; 32], [3; 32]), path.clone()));
        let arrived = list.get_item(2).unwrap().1;
        let picture = arrived.widget(cx, ids!(frame)).area().rect(cx).center();
        down(cx, &mut data, picture, MouseButton::PRIMARY, 12.);
        assert_eq!(up(cx, &mut data, picture, MouseButton::PRIMARY, 12.03), None, "a single click only selects the picture");
        assert_eq!(data.chat_selected, Some([3; 32]), "a click selects the arrived picture");
        assert_eq!(data.chat_key(cx, &list, &enter), open(), "Return opens the selected picture's full-size file");
        draw(cx, &mut data);
        assert!(arrived.widget(cx, ids!(frame.picture_border)).visible(), "the selected picture has an outline");
        down(cx, &mut data, picture, MouseButton::PRIMARY, 12.1);
        assert_eq!(up(cx, &mut data, picture, MouseButton::PRIMARY, 12.13), open(), "a double-click opens the same picture");
        menu(cx, &mut data, picture, 13., 3, &["Open"]);
        let remote = list.get_item(3).unwrap().1.widget(cx, ids!(frame)).area().rect(cx).center();
        menu(cx, &mut data, remote, 14., 4, &[]);
        assert_eq!(data.chat_key(cx, &list, &enter), None, "Return waits for the picture to arrive");
        send(cx, &mut data, Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() }));
        touch(cx, &mut data, TouchState::Start, 4, at, 15.);
        let travel = at + dvec2(0., 35.);
        touch(cx, &mut data, TouchState::Move, 4, travel, 15.1);
        assert!(cx.fingers.is_touch_drag_claimed(4) && !list.has_selection(), "a touch drag belongs to the scroller");
        touch(cx, &mut data, TouchState::Stop, 4, travel, 15.2);
        assert!(data.chat_selected.is_none(), "lifting a touch scroll must not select a message");

        use crate::lists::TownRead;
        use crate::model::{tests::ShareHomes, Model, TownUnread};
        use crate::view::Workspace;
        list.stop_scrolling();
        list.set_first_id_and_scroll(0, 0.);
        let homes = ShareHomes::new("chat-divider");
        let mut model = Model::open(&homes.0).unwrap().0;
        let town = model.open_id();
        data.tree.set(Vec::new(), model.town_tree());
        data.chat_read.towns.insert(town, TownRead { seen: Some([1; 32]), divider_before: Some([2; 32]) });
        draw(cx, &mut data);
        let strip = ben.widget(cx, ids!(unread_line));
        assert!(strip.visible() && strip.selection_text_len() == 0);
        assert!(!ana.widget(cx, ids!(unread_line)).visible());
        let at = strip.area().rect(cx).center();
        for (time, button) in [(16., MouseButton::PRIMARY), (17., MouseButton::SECONDARY)] {
            let started = down(cx, &mut data, at, button, time);
            let (ended, _) = release(cx, &mut data, at, button, time + 0.1);
            assert!(data.chat_selected.is_none(), "the unread strip has no message action");
            assert!(!menu_actions(&started).chain(menu_actions(&ended)).any(|a| matches!(a, MenuAction::Open { .. })),
                "the unread strip has no context menu");
        }
        drag(cx, &mut data, 18.);
        assert_eq!(list.selected_entries(), Some((ana_index, ben_index)));
        assert_eq!(copy(cx, &mut data).as_deref(), Some(both), "the strip adds no text to a multi-message copy");
        let at = body_point(cx, &ben, false);
        let opened = menu(cx, &mut data, at, 19., 2, &["Copy", "Copy selection", "Reply"]);
        send(cx, &mut data, Event::Actions(opened));
        send(cx, &mut data, Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() }));
        draw(cx, &mut data);
        let picture_area = list.get_item(2).unwrap().1.widget(cx, ids!(frame)).area();
        let picture = picture_area.clipped_rect(cx).center();
        assert_eq!(list.item_at(cx, picture), Some(2), "picture frame {:?} inside {:?}", picture_area.clipped_rect(cx), list.area().rect(cx));
        down(cx, &mut data, picture, MouseButton::PRIMARY, 20.);
        up(cx, &mut data, picture, MouseButton::PRIMARY, 20.1);
        assert_eq!(data.chat_selected, Some([3; 32]), "the picture still occupies its original row");
        let get_area = list.get_item(3).unwrap().1.button(cx, ids!(get)).area();
        let get = get_area.clipped_rect(cx).center();
        assert_eq!(list.item_at(cx, get), Some(3), "Get is visible in the remote picture's row");
        down(cx, &mut data, get, MouseButton::PRIMARY, 21.);
        let (clicks, _) = release(cx, &mut data, get, MouseButton::PRIMARY, 21.1);
        assert_eq!(ClipPlayback::default().actions(cx, &clicks, &list, &data.workspace.rows),
            Some((ClipId([0; 32], [4; 32]), [4; 32])), "Get requests the remote picture from row three");

        let clock = Default::default();
        let messages: Vec<_> = (0..12).map(|i| model.post_chat_draft(
            &format!("Message {i}\nSecond line\nThird line"), None, None, None).unwrap()).collect();
        data.workspace = Workspace::read(&model, false, &clock);
        data.chat_read.towns.get_mut(&town).unwrap().divider_before = Some(messages[2]);
        let message_row = message_index(&data.workspace, messages[2]);
        draw(cx, &mut data);
        list.set_tail_range(false);
        list.set_first_id_and_scroll(message_row, 0.);
        draw(cx, &mut data);
        assert_eq!(list.first_id(), message_row);
        assert!(!list.is_at_end());
        let row = list.get_item(message_row).unwrap().1;
        let body = row.widget(cx, ids!(message));
        let rect = body.area().rect(cx);
        let point = |offset| (0..(rect.size.x * 2.) as usize)
            .map(|x| dvec2(rect.pos.x + x as f64 * 0.5, rect.pos.y + 5.))
            .find(|&p| body.selection_point_to_char_index(cx, p) == Some(offset)).unwrap();
        let (start, end) = (point(0), point("Message 2".len()));
        down(cx, &mut data, start, MouseButton::PRIMARY, 22.);
        send(cx, &mut data, Event::MouseMove(MouseMoveEvent { abs: end, lock_delta: Default::default(),
            window_id: win, time: 22.1, modifiers: Default::default(), handled: Default::default() }));
        up(cx, &mut data, end, MouseButton::PRIMARY, 22.2);
        let selected = copy(cx, &mut data);
        assert_eq!(selected.as_deref(), Some("Message 2"));
        assert_eq!(list.selected_entries(), Some((message_row, message_row)));

        let strip_h = row.widget(cx, ids!(unread_line)).area().rect(cx).size.y;
        assert!(strip_h > 0.);
        list.set_first_id_and_scroll(message_row, -strip_h);
        draw(cx, &mut data);
        assert_eq!(list.first_id(), message_row);
        assert!((list.scroll_position() + strip_h).abs() < 0.5);
        let tail = model.post_chat_draft("Next message", None, None, None).unwrap();
        data.workspace = Workspace::read(&model, false, &clock);
        data.chat_read.towns.get_mut(&town).unwrap().divider_before = Some(tail);
        draw(cx, &mut data);
        assert_eq!(list.first_id(), message_row);
        assert!(list.scroll_position().abs() < 0.5, "moving the strip preserves the top message's header: height {strip_h}, offset {}", list.scroll_position());
        assert!(!row.widget(cx, ids!(unread_line)).visible());
        assert_eq!(list.selected_entries(), Some((message_row, message_row)));
        assert_eq!(copy(cx, &mut data), selected, "moving the divider preserves the selected words");
        list.set_first_id_and_scroll(message_row, -10.);
        data.chat_read.towns.get_mut(&town).unwrap().divider_before = None;
        draw(cx, &mut data);
        assert!((list.scroll_position() + 10.).abs() < 0.5, "clearing a divider after its strip already hid does not shift scroll again");

        let wheel = |cx: &mut Cx, data: &mut ViewData, time| {
            let abs = list.area().rect(cx).center();
            send(cx, data, Event::Scroll(ScrollEvent { window_id: win, scroll: dvec2(0., -200.), abs,
                modifiers: Default::default(), handled_x: Default::default(), handled_y: Default::default(),
                is_mouse: true, time, phase: ScrollPhase::None }));
        };
        data.chat_error = Some(([1; 32], "cannot delete".into()));
        wheel(cx, &mut data, 23.);
        let actions = cx.capture_actions(|cx| draw(cx, &mut data));
        assert!(list.scrolled(&actions));
        data.chat_actions(cx, &list, &actions);
        assert_eq!(data.chat_error, Some(([1; 32], "cannot delete".into())), "scrolling keeps the error visible");
        let at = list.get_item(list.first_id() + 1).unwrap().1.widget(cx, ids!(message)).area().rect(cx).center();
        down(cx, &mut data, at, MouseButton::PRIMARY, 24.);
        up(cx, &mut data, at, MouseButton::PRIMARY, 24.1);
        assert!(data.chat_error.is_none(), "a message click clears the error");

        let mut waits = crate::tools::AgentWaits::default();
        let binding = makepad_town_core::queue::Credential { town, check: [7; 32] };
        waits.traces.pickup(binding, [5; 32], "Your AI", &[tail], 10_000);
        for text in ["one", "two", "three"] {
            waits.traces.output(binding, text.into(), crate::ai_trace::Ink::Action, 10_200);
        }
        let mut folds = Default::default();
        data.workspace = Workspace::read_chat_at(&model, false, &clock, &waits, &Default::default(), 11_000, &folds);
        data.chat_read.mark_open_read(town, Some(tail));
        data.chat_read.mark_at_tail();
        list.set_tail_range(true);
        list.scroll_to_end(cx);
        let conversation = chat.widget(cx, ids!(conversation));
        let jump = conversation.widget(cx, ids!(jump_latest));
        let mut scroll_draw = |cx: &mut Cx, data: &mut ViewData| {
            let actions = cx.capture_actions(|cx| draw(cx, data));
            data.chat_scroll_actions(cx, &conversation, &list, &actions);
        };
        scroll_draw(cx, &mut data);
        assert!(list.is_at_end() && data.chat_read.at_live_tail() && !jump.visible());
        let trace_rows = list.get_item(message_index(&data.workspace, tail)).unwrap().1.flat_list(cx, ids!(trace_rows));
        let fold = trace_rows.borrow().unwrap().items.values().find(|r| r.template == id!(Fold)).unwrap().widget.clone();
        let at = fold.area().rect(cx).center();
        down(cx, &mut data, at, MouseButton::PRIMARY, 25.);
        scroll_draw(cx, &mut data);
        let (actions, _) = release(cx, &mut data, at, MouseButton::PRIMARY, 25.1);
        let key = actions.iter().filter_map(|a| a.as_widget_action())
            .filter(|a| matches!(a.cast::<ButtonAction>(), ButtonAction::Clicked(_)))
            .find_map(|a| a.data.as_ref()?.downcast_ref::<TraceFold>()).copied().expect("the tail disclosure toggles the trace");
        data.workspace.toggle_trace(key, &waits.traces, &mut folds, 11_000, &clock);
        assert!(folds.contains(&key));
        scroll_draw(cx, &mut data);
        assert!(data.chat_read.at_live_tail() && !jump.visible(), "expanding a trace is not a user scroll");
        scroll_draw(cx, &mut data);
        assert!(list.is_at_end() && data.chat_read.at_live_tail() && !jump.visible(), "the tail catches up to the taller trace");
        wheel(cx, &mut data, 27.);
        scroll_draw(cx, &mut data);
        let unread = model.post_chat_draft("One more line", None, None, None).unwrap();
        data.workspace = Workspace::read_chat_at(&model, false, &clock, &waits, &Default::default(), 11_000, &folds);
        data.chat_read.counts.insert(town, TownUnread { count: 1, newest: Some(unread), first_unread: Some(unread), ..Default::default() });
        data.chat_read.towns.get_mut(&town).unwrap().divider_before = Some(unread);
        data.sync_jump_latest(cx, &conversation);
        assert!(!list.is_at_end() && !data.chat_read.open_at_tail && jump.visible());
        assert_eq!(jump.button(cx, ids!(jump_latest_btn)).text(), "1 new ↓");
        scroll_draw(cx, &mut data);
        assert!(!list.is_at_end() && !data.chat_read.at_live_tail() && jump.visible());
        let mut nf_cursor = cx.next_frame_id();
        let pill = jump.button(cx, ids!(jump_latest_btn)).area().rect(cx).center();
        down(cx, &mut data, pill, MouseButton::PRIMARY, 28.);
        let (actions, _) = release(cx, &mut data, pill, MouseButton::PRIMARY, 28.1);
        data.chat_scroll_actions(cx, &conversation, &list, &actions);
        assert!(data.chat_read.at_live_tail() && !jump.visible());
        assert_eq!(data.chat_read.open_unread(town).count, 0);
        assert_eq!(data.chat_read.towns[&town], TownRead { seen: Some(unread), divider_before: None });
        assert!(list.is_smooth_scrolling().is_some(), "the pill starts the scroll to the newest line");
        for frame in 0..40 {
            let end = cx.next_frame_id();
            let event = NextFrameEvent { frame, time: 28.2 + frame as f64 / 60., set: (nf_cursor..end).map(NextFrame).collect() };
            nf_cursor = end;
            let (actions, _) = send(cx, &mut data, Event::NextFrame(event));
            data.chat_scroll_actions(cx, &conversation, &list, &actions);
            scroll_draw(cx, &mut data);
            if list.is_smooth_scrolling().is_none() && list.is_at_end() { break }
        }
        assert!(list.is_smooth_scrolling().is_none() && list.is_at_end() && data.chat_read.at_live_tail());
        assert!(!jump.visible());
        assert!(list.get_item(data.workspace.rows.len() - 1).is_some_and(|(_, w)| !w.widget(cx, ids!(unread_line)).visible()));

    }
}

#[cfg(test)]
mod clip_tests {
    use super::*;
    use crate::{model::tests::ShareHomes, view::ClipMedia};
    use makepad_widgets::makepad_script::script;

    #[test]
    fn clip_rows_keep_their_box_after_a_bad_poster_and_share_one_player() {
        let files = ShareHomes::new("clip-widget");
        std::fs::create_dir_all(&files.0).unwrap();
        let path = files.0.join("recording");
        std::fs::write(&path, b"neither an image nor a movie").unwrap();
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let chat = cx.with_vm(|vm| {
            let value = vm.eval(script! { use mod.widgets.* ChatConversation{} });
            WidgetRef::script_from_value(vm, value)
        });
        let list = chat.portal_list(cx, ids!(transcript.list));
        let first = list.item(cx, 0, id!(ClipLine));
        let second = list.item(cx, 1, id!(ClipLine));
        let line = |id, playable| Arc::new(ClipLine {
            id: ClipId([1; 32], [id; 32]), title: "Review".into(),
            media: Some(ClipMedia { is_picture: false, blob: [3; 32], length: "1:24".into(), size: "2.1 MB".into(),
                poster: Some(path.clone()), height: 180., state: if playable { "On this computer" } else { "On Ana's computer" }.into(),
                get: !playable, playable: playable.then(|| path.clone()) }),
        });
        first.borrow_mut::<ClipRow>().expect("the clip template builds a row").bind(cx, &line(1, false));
        assert!(first.widget(cx, ids!(get)).visible() && !first.widget(cx, ids!(play)).visible(), "a remote clip offers only Get");
        assert!(!first.image(cx, ids!(poster)).has_texture(), "an invalid poster leaves the plain box");
        let frame = first.widget(cx, ids!(frame)).walk(cx);
        assert_eq!((frame.width, frame.height), (Size::Fixed(240.), Size::Fixed(180.)), "a failed decode keeps the reserved aspect");
        assert_eq!(first.label(cx, ids!(length)).text(), "1:24", "the duration stays on the plain box");
        assert_eq!(first.label(cx, ids!(state)).text(), "On Ana's computer", "poster failure leaves the transfer status intact");

        for local in [false, true] {
            let mut snap = line(3, local);
            Arc::get_mut(&mut snap).unwrap().media.as_mut().unwrap().is_picture = true;
            first.borrow_mut::<ClipRow>().unwrap().bind(cx, &snap);
            assert!(!first.widget(cx, ids!(play)).visible() && !first.widget(cx, ids!(duration_badge)).visible(),
                "a snap has neither playback nor duration, local={local}");
            assert_eq!(first.widget(cx, ids!(get)).visible(), !local, "only a remote snap offers Get");
            assert!(!first.widget(cx, ids!(clip_title)).visible() && !first.widget(cx, ids!(details)).visible(),
                "a snap shows no file name and no size or location line, local={local}");
            assert_eq!(first.widget(cx, ids!(progress)).visible(), !local, "only a snap still on its way shows progress on itself");
            assert_eq!(first.label(cx, ids!(progress_line)).text(), snap.media.as_ref().unwrap().state, "the progress is the transfer status");
        }
        first.borrow_mut::<ClipRow>().unwrap().bind(cx, &line(1, true));
        assert!(first.widget(cx, ids!(duration_badge)).visible(), "a reused movie row shows its duration");
        assert!(first.widget(cx, ids!(clip_title)).visible() && first.widget(cx, ids!(details)).visible()
            && !first.widget(cx, ids!(progress)).visible(), "a movie keeps its name and details under the box");
        second.borrow_mut::<ClipRow>().unwrap().bind(cx, &line(2, true));
        assert!(first.widget(cx, ids!(play)).visible() && !first.widget(cx, ids!(get)).visible(), "verified media replaces Get with Play");
        let mut playback = ClipPlayback::default();
        playback.play(cx, first.as_clip_row(), &path);
        assert!(first.widget(cx, ids!(player)).visible() && !first.widget(cx, ids!(cover)).visible(), "Play replaces the poster inline");
        playback.play(cx, second.as_clip_row(), &path);
        assert!(!first.widget(cx, ids!(player)).visible() && second.widget(cx, ids!(player)).visible(), "a second Play closes the first");
        playback.close(cx);
        assert!(second.widget(cx, ids!(cover)).visible() && !second.widget(cx, ids!(player)).visible(), "close restores the poster");
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let unplayable = std::path::Path::new(std::ffi::OsStr::from_bytes(b"\xff"));
            playback.play(cx, first.as_clip_row(), unplayable);
            assert!(first.widget(cx, ids!(failure)).visible() && first.widget(cx, ids!(cover)).visible() && !first.widget(cx, ids!(player)).visible(),
                "a movie that cannot be opened keeps its box and says it can't be played");
        }
    }
}

#[cfg(test)]
mod composer_tests {
    use super::*;
    use makepad_widgets::{cx_draw::CxDraw, makepad_draw::text::selection::Cursor, makepad_platform::studio::StudioToApp, makepad_script::script};

    #[test]
    fn mention_hint_follows_mention_target() {
        use crate::model::MentionedSeat;

        for (target, expected) in [
            (MentionTarget::None, None),
            (MentionTarget::YourAi { has_ai: true, agent: Some([1; 32]), label: Some("Your AI · Claude Code".into()) }, Some("Goes to Your AI · Claude Code")),
            (MentionTarget::YourAi { has_ai: true, agent: Some([1; 32]), label: None }, Some("Goes to Your AI")),
            (MentionTarget::YourAi { has_ai: true, agent: Some([1; 32]), label: Some("Your AI".into()) }, Some("Goes to Your AI")),
            (MentionTarget::YourAi { has_ai: false, agent: None, label: None }, Some("Waits until you add an AI")),
            (MentionTarget::PeerAi { agent: [1; 32], label: "Ben's AI · Fab Bot".into(), owner_name: "Ben".into() },
                Some("Goes to Ben's AI · Fab Bot (Ben approves first)")),
            (MentionTarget::PeerAi { agent: [1; 32], label: "Ben's AI · Claude Code".into(), owner_name: "Ben".into() },
                Some("Goes to Ben's AI · Claude Code (Ben approves first)")),
            (MentionTarget::PeerAi { agent: [1; 32], label: String::new(), owner_name: "Ben".into() },
                Some("Goes to Ben's AI (Ben approves first)")),
            (MentionTarget::UnboundAi,
                Some("Not sent to an AI — pick one from @ or use @ai for yours")),
        ] {
            assert_eq!(mention_hint_text(&target).as_deref(), expected, "hint for {target:?}");
        }

        let seats: Vec<_> = ["Ben", "Kit", "Rik", "Lu"].into_iter().enumerate()
            .map(|(i, name)| MentionedSeat { seat: [i as u8; 32], name: name.into() }).collect();
        for (count, expected) in [
            None, Some("Mentions Ben"), Some("Mentions Ben and Kit"),
            Some("Mentions Ben, Kit and 1 other"), Some("Mentions Ben, Kit and 2 others"),
        ].into_iter().enumerate() {
            let target = MentionTarget::People(seats[..count].to_vec());
            assert_eq!(mention_hint_text(&target).as_deref(), expected, "hint for {count} mentioned seats");
        }
    }

    #[test]
    fn an_emoji_pick_goes_in_at_the_cursor_and_leads_the_saved_recents() {
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let chat = cx.with_vm(|vm| {
            let value = vm.eval(script! { use mod.widgets.* use mod.prelude.widgets.*
                View{flow: Overlay conversation := ChatConversation{}}
            });
            WidgetRef::script_from_value(vm, value)
        });
        let conversation = chat.widget(cx, ids!(conversation));
        let dir = crate::model::tests::ShareHomes::new("emoji");
        let home = Arc::new(Home::open(&dir.0).unwrap());
        let (writer, release) = crate::model::tests::held_writer(&home);
        save_recent_emoji(&home, &["😀".into(), "👍".into()]).unwrap();
        let picker = conversation.emoji_picker(cx, ids!(emoji_picker));
        picker.set_recents(cx, &load_recent_emoji(&home));
        assert_eq!(picker.recents(), ["😀", "👍"], "saved recents load back in their order");

        let mut data = ViewData::default();
        let pass = DrawPass::new(cx);
        let size = dvec2(360., 480.);
        pass.set_size(cx, size);
        let mut ink = DrawList2d::new(cx);
        // The grid draws on the overlay, as it does in a window.
        let overlay = cx.with_vm(|vm| Overlay::script_new(vm));
        let mut draw = |cx: &mut Cx, data: &mut ViewData| {
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            ink.begin_always(&mut frame);
            overlay.begin(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            chat.draw_all(&mut frame, &mut Scope::with_data(data));
            frame.end_pass_sized_turtle();
            overlay.end(&mut frame);
            ink.end(&mut frame);
            frame.end_pass(&pass);
        };
        // A platform event cycle applies the key focus asked for since the last one.
        let settle = |cx: &mut Cx| {
            cx.dispatch_studio_msg(StudioToApp::KeyUp(KeyEvent { key_code: KeyCode::Shift, ..Default::default() }),
                WindowId(1, 1), DVec2::default());
        };
        let press = |cx: &mut Cx, data: &mut ViewData, key_code| {
            let event = Event::KeyDown(KeyEvent { key_code, ..Default::default() });
            let actions = cx.capture_actions(|cx| chat.handle_event(cx, &event, &mut Scope::with_data(data)));
            composer_emoji_actions(cx, &conversation, &actions, Some(&home));
        };

        let input = conversation.text_input(cx, ids!(chat_input));
        input.set_text(cx, "ab");
        input.set_cursor(cx, Cursor { index: 1, prefer_next_row: false }, false);
        draw(cx, &mut data);
        cx.set_key_focus(input.area());
        settle(cx);
        let popover = conversation.popover(cx, ids!(emoji));
        popover.open(cx);
        draw(cx, &mut data);
        settle(cx);
        // The grid starts on the first recent: Right moves to the second and Return picks it.
        press(cx, &mut data, KeyCode::ArrowRight);
        press(cx, &mut data, KeyCode::ReturnKey);
        assert_eq!(input.text(), "a👍b", "the pick goes in at the draft's cursor");
        assert!(popover.is_open(), "the grid stays open for another pick");
        assert_eq!(picker.recents(), ["👍", "😀"], "the pick leads the recents, once");
        assert_eq!(load_recent_emoji(&home), ["👍", "😀"], "and the recents are saved for the next start");

        // The draft drew again with the pick; Escape still hands its keys back to it.
        draw(cx, &mut data);
        press(cx, &mut data, KeyCode::Escape);
        settle(cx);
        assert!(!popover.is_open(), "Escape closes the grid");
        assert!(cx.has_key_focus(input.area()), "the draft has the keys again after the grid closes");
        assert!(!home.recent_emoji_path().exists(), "the picker read queued recents while the writer was held");
        drop(release);
        drop((writer, home));
        let reopened = Home::open(&dir.0).unwrap();
        assert_eq!(load_recent_emoji(&reopened), ["👍", "😀"], "the selected order survives reopening");
    }
}
