use makepad_widgets::*;
use crate::ai_trace::{single_line, Ink, Phase};
use crate::model::{wall_ms, MentionTarget};
use crate::lists::ViewData;
use crate::view::{ChatRow, ClipId, ClipLine, Message, TraceFold};
use makepad_town_core::event::Id;
use makepad_town_core::home::Home;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

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
            controls := View{width: Fill height: Fit flow: Right
                get := PanelButton{text: "Get"}
                play := PanelButton{text: "Play"}
                close_clip := PanelButton{text: "Close" a11y: {label: "Close clip player"}}
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
        quote := View{visible: false width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
            SolidView{width: 2 height: 14 draw_bg.color: palette.line}
            quote_text := QuoteLine{}
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
        width: Fill height: Fit flow: Right spacing: 3 padding: Inset{bottom: 3}
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
        transcript := mod.widgets.AgentChatListBase{
            mention_ink: palette.yourai action_ink: palette.muted status_ink: palette.text warning_ink: palette.warn
            outline_ink: palette.accent line_ink: palette.line
            width: Fill height: Fill
            list := PortalList{
                // The scroll bar (10 wide) overlays the list's right edge: rows wrap short of it.
                width: Fill height: Fill auto_tail: true padding: Inset{right: 12}
                selectable: true select_all_entry_first: true triple_click_paragraph: true
                emit_item_actions: true grab_key_focus: true
                Line := Message{}
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
                    content.bubble +: {
                        show_bg: true padding: Inset{top: 6 bottom: 6 left: 8 right: 8}
                        draw_bg +: {color: palette.sel border_radius: 4 border_size: 1 border_color: palette.line}
                        sent +: {visible: true}
                    }
                }
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
            draw_visible_rows(cx, &mut list, data, inks, |cx, rows, message| {
                self.draw_traces(cx, rows, message, now);
            });
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
        let list = watch.then(|| self.view.portal_list(cx, ids!(list)));
        let before = list.as_ref().and_then(text_range);
        self.view.handle_event(cx, event, scope);
        if let Some(list) = list {
            let after = text_range(&list);
            if after.is_some() && after != before {
                scope.data.get_mut::<ViewData>().unwrap().chat_selected = None;
                list.redraw(cx);
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
        MentionTarget::YourAi { has_ai: true, .. } => "Goes to your AI".into(),
        MentionTarget::YourAi { has_ai: false, .. } => "Waits until you add an AI".into(),
        MentionTarget::PeerAi { owner_name, .. } => format!("Goes to {owner_name}'s AI ({owner_name} approves first)"),
        MentionTarget::AmbiguousAi { name, .. } => format!("Several AIs are named {name} — use @ai for yours"),
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

#[derive(Debug, PartialEq, Eq)]
pub enum ChatCommand {
    Reply(Id),
    Delete(Id),
    OpenPicture(ClipId, PathBuf),
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

fn text_range(list: &PortalListRef) -> Option<((usize, usize), (usize, usize))> {
    list.borrow()?.get_selection_range().filter(|(a, b)| a != b)
}

fn message_text(message: &Message) -> String {
    format!("{} {}\n{}", message.name, message.time, message.text)
}

impl ViewData {
    fn message_at(&self, index: usize) -> Option<&Message> {
        match self.workspace.rows.get(index)? { ChatRow::Message(m) => Some(m), _ => None }
    }

    fn selected_target(&self) -> Option<ChatTarget<'_>> {
        let id = self.chat_selected?;
        self.workspace.rows.iter().filter_map(ChatTarget::from_row).find(|row| row.id() == id)
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
        for gesture in list.item_actions(actions) {
            self.chat_error = None;
            match gesture {
                PortalListAction::ItemClicked { item_id, taps, .. } => {
                    let target = item_id.and_then(|i| self.workspace.rows.get(i)).and_then(ChatTarget::from_row);
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
                    let Some(target) = self.workspace.rows.get(item_id).and_then(ChatTarget::from_row) else { continue };
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

fn draw_visible_rows(cx: &mut Cx2d, list: &mut PortalList, data: &crate::lists::ViewData, inks: RowInks,
    mut draw_traces: impl FnMut(&mut Cx2d, &mut FlatList, &Message)) {
    let rows = &data.workspace.rows;
    let mut quote_text = String::new();
    list.set_item_range(cx, 0, rows.len());
    while let Some(index) = list.next_visible_item(cx) {
        let Some(row) = rows.get(index) else { continue };
        let message = match row {
            crate::view::ChatRow::Member(text) => {
                let item = list.item(cx, index, id!(MemberLine));
                item.set_text(cx, text);
                item.draw_all(cx, &mut Scope::empty());
                continue
            }
            crate::view::ChatRow::Clip(clip) => {
                let item = non_message_item(cx, list, index, id!(ClipLine));
                if let Some(mut row) = item.borrow_mut::<ClipRow>() { row.bind(cx, clip); }
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
        let author = item.widget(cx, ids!(author));
        crate::lists::seat_icon(cx, &author, message.seat, &message.name);
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
        if let Some(quote) = &message.quote {
            quote_text.clear();
            write!(quote_text, "{quote}").unwrap();
            item.label(cx, ids!(quote_text)).set_text(cx, &quote_text);
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
        // Selecting a message outlines it in place: nothing opens under it, so no line moves.
        let selected = data.chat_selected == Some(message.id);
        if let Some(mut bubble) = item.view(cx, ids!(bubble)).borrow_mut() {
            bubble.show_bg = selected || message.addressed;
            let ink = if selected { inks.outline } else { inks.line };
            bubble.draw_bg.set_dyn_instance(cx, id!(border_color), &[ink.x, ink.y, ink.z, ink.w]);
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
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use makepad_widgets::{cx_draw::CxDraw, makepad_platform::{event::{LongPressEvent, TouchPoint, TouchState, TouchUpdateEvent}, studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp}}, makepad_script::script};

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
            data.workspace.rows.push(ChatRow::Message(Message { id: [id; 32], quote, seat: crate::view::Seat::Peer,
                name: name.into(), time: time.into(), text: words.into(), traces: vec![],
                addressed: false, unheard: false, mention: false, deletable: id == 1 }));
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
        let ana = list.get_item(0).expect("Ana's row was drawn").1;
        let ben = list.get_item(1).expect("Ben's row was drawn").1;
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
        let up = |cx: &mut Cx, data: &mut ViewData, abs, button: MouseButton, time| {
            let (_, command) = send(cx, data, Event::MouseUp(MouseUpEvent { abs, button, window_id: win, time, modifiers: Default::default() }));
            // Finish the platform's capture and focus cycle on the pooled context.
            cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp { x: abs.x, y: abs.y, time,
                button_raw_bits: button.bits(), ..Default::default() }), win, DVec2::default());
            command
        };
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

        drag(cx, &mut data, 1.);
        assert_eq!(list.selected_entries(), Some((0, 1)), "a desktop drag must select across both message rows");
        let both = "Ana 14:02\nTea is ready.\nBen 14:03\nOn my way.";
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
            (MentionTarget::YourAi { has_ai: true, name: Some("Claude Code".into()) }, Some("Goes to your AI")),
            (MentionTarget::YourAi { has_ai: false, name: None }, Some("Waits until you add an AI")),
            (MentionTarget::PeerAi { agent: [1; 32], agent_name: "Fab Bot".into(), owner_name: "Ben".into() },
                Some("Goes to Ben's AI (Ben approves first)")),
            (MentionTarget::AmbiguousAi { name: "Claude Code".into(), count: 2 },
                Some("Several AIs are named Claude Code — use @ai for yours")),
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
