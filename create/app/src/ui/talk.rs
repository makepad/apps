use crate::lists::{Live, Page, ViewData};
use makepad_widgets::*;
use makepad_town_core::home::Home;
use std::io;

const BAR: f64 = 1.0;
const LEFT_MIN: f64 = 160.0;
const PAGE_MIN: f64 = 320.0;
const CHAT_MIN: f64 = 220.0;
const DEFAULT_WIDTHS: (f64, f64) = (220.0, 279.0);

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.IssuesPage = #(super::issues::page_template(vm))
    let Edge = Sash{
        size: #(BAR) exact_floors: true
        draw_bg +: {color: palette.line color_bg: palette.panel}
    }
    mod.widgets.ShareChatEdge = Tip{
        visible: false text: "Show chat"
        View{
            width: 28 height: Fill flow: Overlay align: Align{x: 0.5 y: 0.04}
            show_chat := PanelButton{
                width: Fill height: Fill padding: 0 text: "‹" a11y: {label: "Show chat"}
                draw_bg +: {color: palette.panel border_radius: 0}
            }
            edge_dot := PresenceDot{visible: false width: 8 height: 8 draw_bg.color: palette.accent}
        }
    }
    mod.widgets.WorkspaceColumns = View{
        width: Fill height: Fill flow: Down
        compact_call := PanelButton{visible: false width: Fill height: 30}
        compact_live := PanelButton{visible: false width: Fill height: 30}
        chat_split := Edge{
            align: SplitterAlign.FromB(#(DEFAULT_WIDTHS.1 + BAR))
            min_vertical: #(LEFT_MIN + BAR + PAGE_MIN)
            max_vertical: #(CHAT_MIN)
            a: View{
                rail_split := Edge{
                    align: SplitterAlign.FromA(#(DEFAULT_WIDTHS.0))
                    min_vertical: #(LEFT_MIN)
                    max_vertical: #(PAGE_MIN)
                    a: View{
                        left_rail := SolidView{new_batch: true
                            width: Fill height: Fill flow: Down
                            padding: Inset{left: 8 right: 8 top: 10 bottom: 10}
                            draw_bg.color: palette.panel
                            navigation := TownRail{tree +: {list +: {
                                Row +: {new_batch: true}
                                TownRow +: {new_batch: true}
                                Join +: {new_batch: true}
                            }}}
                        }
                    }
                    b: View{
                        main_area := View{
                            width: Fill height: Fill flow: Down
                            item_head := ItemHead{}
                            home_page := HomePage{}
                            join_page := JoinPage{}
                            project_page := ProjectPage{}
                            board := TaskBoard{visible: false}
                            issues_page := mod.widgets.IssuesPage{visible: false}
                            canvas := WhiteboardPage{visible: false}
                            docs_page := DocumentsPage{visible: false}
                            publish_page := PublishPage{visible: false}
                            live_call := LiveCallPage{visible: false}
                            live_share := LiveSharePage{visible: false}
                        }
                    }
                }
            }
            b: View{chat_panel := ChatRail{}}
        }
    }
}

fn read_widths(home: &Home) -> (f64, f64) {
    // Even a damaged layout file costs at most one small buffer at startup.
    let result = home.files().read_capped(&home.layout_path(), 1024)
        .and_then(|bytes| String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)));
    match result {
        Ok(text) => {
            let mut fields = text.split_whitespace();
            let mut width = || fields.next()?.parse::<f64>().ok().filter(|w| w.is_finite());
            if let (Some(left), Some(chat), None) = (width(), width(), fields.next()) {
                if text.len() < 1024 { return (left.max(LEFT_MIN), chat.max(CHAT_MIN)); }
            }
            log!("town: malformed layout, using default panel widths");
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => log!("town: reading layout: {e}"),
    }
    DEFAULT_WIDTHS
}

fn write_widths(home: &Home, left: f64, chat: f64) -> io::Result<()> {
    home.files().replace_rebuildable(&home.layout_path(), format!("{left} {chat}\n").into_bytes())
}

impl ViewData {
    pub fn restore_layout(cx: &mut Cx, ui: &WidgetRef, home: &Home) {
        let (left, chat) = read_widths(home);
        ui.splitter(cx, ids!(rail_split)).set_align(cx, SplitterAlign::FromA(left));
        // FromB measures from the far edge to the start of the bar.
        ui.splitter(cx, ids!(chat_split)).set_align(cx, SplitterAlign::FromB(chat + BAR));
    }

    pub fn save_settled_layout(cx: &mut Cx, ui: &WidgetRef, home: &Home, actions: &Actions) {
        let rail = ui.splitter(cx, ids!(rail_split));
        let chat = ui.splitter(cx, ids!(chat_split));
        if rail.settled(actions).is_none() && chat.settled(actions).is_none() { return }
        if let (Some(SplitterAlign::FromA(left)), Some(SplitterAlign::FromB(right))) = (rail.align(), chat.align()) {
            if let Err(e) = write_widths(home, left, right - BAR) {
                log!("town: saving layout: {e}");
            }
        }
    }

    /// The one chat.
    pub fn conversation(&self, cx: &mut Cx, ui: &WidgetRef) -> WidgetRef {
        ui.widget(cx, ids!(chat_panel))
    }

    pub fn chat_edge_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        if ui.button(cx, ids!(show_chat)).clicked(actions) && ui.widget(cx, ids!(chat_edge)).visible() {
            self.share_chat_open = true;
            self.show_panes(cx, ui);
        }
    }

    pub fn narrow_tab_actions(&mut self, cx: &Cx, ui: &WidgetRef, actions: &Actions) -> bool {
        if !self.narrow() { return false }
        let mut pressed = false;
        for (path, pane) in PANE_TABS {
            let tab = ui.widget(cx, path);
            if tab.visible() && tab.button(cx, ids!(press)).clicked(actions) {
                self.show_pane(pane);
                pressed = true;
            }
        }
        pressed
    }

    /// Whether the current pane layout shows the open town's chat.
    pub fn chat_visible(&self) -> bool {
        let narrow = self.narrow();
        let embedded = matches!(self.page, Page::Live(Live::Share(id)) if self.watched_share() == Some(id));
        let folded = embedded && !narrow && !self.share_chat_open;
        !(folded || self.page == Page::Join || (narrow && self.pane != Pane::Chat))
    }

    /// Give an embedded share the chat column until the viewer opens chat for this visit.
    pub fn show_panes(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let narrow = self.narrow();
        let panel = ui.view(cx, ids!(tweak_panel));
        let panel_in_use = panel.visible();
        if self.pane == Pane::Edit && !panel_in_use { self.show_pane(Pane::Page); }
        let embedded = matches!(self.page, Page::Live(Live::Share(id)) if self.watched_share() == Some(id));
        if !embedded { self.share_chat_open = false; }
        let folded = embedded && !narrow && !self.share_chat_open;
        // Joining another town shows nothing of the open one, its chat included.
        let joining = self.page == Page::Join;
        if joining && narrow && self.pane == Pane::Chat { self.show_pane(Pane::Page); }
        let (chat, rail) = match (narrow, self.pane) {
            // The edge replaces the folded splitter’s own bar and hit area.
            (false, _) => ((folded || joining).then_some(SplitterSide::A), None),
            (true, Pane::Chat) => (Some(SplitterSide::B), None),
            (true, Pane::Places) => (Some(SplitterSide::A), Some(SplitterSide::A)),
            (true, Pane::Page | Pane::Edit) => (Some(SplitterSide::A), Some(SplitterSide::B)),
        };
        ui.splitter(cx, ids!(rail_split)).set_solo(cx, rail);
        let split = ui.splitter(cx, ids!(chat_split));
        split.set_collapse(cx, if folded { SplitterCollapse::B } else { SplitterCollapse::None });
        split.set_solo(cx, chat);
        ui.widget(cx, ids!(chat_edge)).set_visible(cx, folded);
        let editing = narrow && self.pane == Pane::Edit;
        let slot = ui.view(cx, ids!(tweak_slot));
        ui.view(cx, ids!(workspace_columns)).set_visible(cx, !editing);
        slot.set_visible(cx, !narrow || editing);
        let slot_width = if editing { Size::fill() } else { Size::fit() };
        let panel_width = if editing { Size::fill() } else { Size::Fixed(285.) };
        for (view, width) in [(slot, slot_width), (panel, panel_width)] {
            if view.walk(cx).width != width { view.set_walk(cx, Walk::new(width, Size::fill())); }
        }
        let prompt = ui.widget(cx, ids!(name_prompt));
        let bar = narrow && (prompt.is_empty() || !prompt.visible());
        ui.widget(cx, ids!(narrow_bar)).set_visible(cx, bar);
        ui.widget(cx, ids!(edit_tab)).set_visible(cx, bar && panel_in_use);
        ui.widget(cx, ids!(chat_tab)).set_visible(cx, !joining);
        for (path, shown) in PANE_TABS {
            let tab = ui.widget(cx, path);
            tab.label(cx, ids!(caption)).set_visible(cx, !self.tabs_as_icons);
            tab.icon(cx, ids!(glyph)).set_visible(cx, self.tabs_as_icons);
            // At once, not animated: a tab must never look open after another one was chosen.
            tab.button(cx, ids!(press)).set_open_with(cx, self.pane == shown, Animate::No);
        }
        let hidden = !self.chat_visible();
        if !hidden && self.chat_read.open_at_tail {
            match self.tree.snapshot().town {
                Some(open) => {
                    let newest = self.chat_read.open_unread(open).newest;
                    self.chat_read.mark_open_read(open, newest);
                }
                None => self.chat_read.mark_unscoped_read(self.newest_message()),
            }
        }
        if self.tree.set_unread(&self.chat_read.counts) { ui.widget(cx, ids!(tree)).redraw(cx); }
        let unread = hidden && self.chat_unread();
        ui.widget(cx, ids!(chat_dot)).set_visible(cx, narrow && unread);
        ui.widget(cx, ids!(edge_dot)).set_visible(cx, folded && unread);
        self.show_compact_live(cx, ui);
    }
}

/// The two forms of the window: three columns, or one pane at a time with a bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Form { #[default] Wide, Narrow }

/// The pane a Narrow window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane { #[default] Chat, Places, Page, Edit }

const PANE_TABS: [(&[LiveId], Pane); 4] = [
    (ids!(chat_tab), Pane::Chat), (ids!(places_tab), Pane::Places),
    (ids!(page_tab), Pane::Page), (ids!(edit_tab), Pane::Edit),
];

const NARROW_BELOW: f64 = 980.0;
const TABS_AS_ICONS_BELOW: f64 = 560.0;
/// Amp's: a larger form is entered only this far past its minimum.
const HYSTERESIS: f64 = 24.0;

/// The form for a window `width` points wide, given the form it has now.
pub fn form_for_width(width: f64, current: Form) -> Form {
    match current {
        Form::Wide if width < NARROW_BELOW => Form::Narrow,
        Form::Narrow if width >= NARROW_BELOW + HYSTERESIS => Form::Wide,
        form => form,
    }
}

impl ViewData {
    /// Update the tab faces on a resize within one form. A form change needs a full page update.
    pub fn fit_tabs(&mut self, cx: &mut Cx, ui: &WidgetRef, width: f64, form: Form, reapply: bool) -> bool {
        let icons = width < TABS_AS_ICONS_BELOW;
        let tabs_changed = self.tabs_as_icons != icons;
        self.tabs_as_icons = icons;
        if !reapply && self.form == form {
            if tabs_changed { self.show_panes(cx, ui); }
            return false
        }
        if form == Form::Narrow && !self.narrow() { self.show_pane(Pane::Chat); }
        self.form = form;
        true
    }
}

/// The form Town declares to a window manager that parks it.
pub fn compact_forms() -> Vec<makepad_widgets::makepad_platform::ime::CompactForm> {
    vec![makepad_widgets::makepad_platform::ime::CompactForm::flexible("chat", 360.0, 280.0, 320.0)]
}

/// The form to answer a parking with: the chat form when it fits, else none.
pub fn parked_form(parked: &makepad_widgets::makepad_platform::ime::HostedParked) -> Option<usize> {
    parked.scale.and_then(|_| makepad_widgets::makepad_platform::ime::pick_compact_form(&compact_forms(), parked.max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{Message, Seat};
    use makepad_widgets::cx_draw::CxDraw;
    use makepad_widgets::makepad_script::script;

    #[test]
    fn rail_widths_round_trip_and_clamp() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("makepad-work-rail-layout-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let home = std::sync::Arc::new(Home::open(&dir).unwrap());
        let (writer, release) = crate::model::tests::held_writer(&home);
        let path = home.layout_path();
        assert_eq!(path, home.endpoint_path().with_file_name("layout"), "layout belongs beside the local endpoint");
        assert_eq!(read_widths(&home), DEFAULT_WIDTHS, "a missing layout uses both defaults");

        write_widths(&home, 193.5, 305.25).unwrap();
        assert_eq!(read_widths(&home), (193.5, 305.25), "both pane widths round-trip through the layout file");
        assert!(!path.exists(), "the widths were read while persistence was held");
        drop(release);
        drop((writer, home));
        let home = Home::open(&dir).unwrap();
        assert_eq!(read_widths(&home), (193.5, 305.25), "the queued widths survive reopening");
        std::fs::write(&path, "180 NaN").unwrap();
        assert_eq!(read_widths(&home), DEFAULT_WIDTHS, "a malformed width resets both panes to their defaults");
        std::fs::write(&path, "-1 37").unwrap();
        assert_eq!(read_widths(&home), (160., 220.), "reading clamps each pane to its own minimum");
        drop(home);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn draw(cx: &mut Cx, ui: &WidgetRef, data: &mut ViewData, size: DVec2) {
        let pass = DrawPass::new(cx);
        pass.set_size(cx, size);
        let mut list = DrawList2d::new(cx);
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &event);
        let mut frame = Cx2d::new(&mut draw);
        frame.begin_pass(&pass, None);
        list.begin_always(&mut frame);
        frame.begin_root_turtle(size, Layout::flow_down());
        ui.draw_all(&mut frame, &mut Scope::with_data(data));
        frame.end_pass_sized_turtle();
        list.end(&mut frame);
        frame.end_pass(&pass);
    }

    fn rows(data: &mut ViewData, line: makepad_town_core::event::Id) {
        use crate::ai_trace::{Ink, Traces};
        for (seat, name, text, addressed) in [(Seat::Peer, "Ben", "Hello", false),
            (Seat::You, "You", "ai hi this working?", true), (Seat::Peer, "Ben asked their AI", "ai hi this working?", true)] {
            data.workspace.rows.push(crate::view::ChatRow::Message(Message {
                id: [data.workspace.rows.len() as u8 + 10; 32], quote: None, traces: vec![], seat, name: name.into(), time: "13:04".into(), text: text.into(),
                addressed, unheard: false, mention: addressed, deletable: false,
            }));
        }
        let binding = makepad_town_core::queue::Credential { town: [1; 32], check: [2; 32] };
        let mut traces = Traces::default();
        traces.pickup(binding, [3; 32], "Your AI", &[line], 10_000);
        for text in ["one", "two", "three"] {
            traces.output(binding, text.into(), Ink::Action, 10_200);
        }
        data.workspace.rows.push(crate::view::ChatRow::Message(Message {
            id: line, quote: None, traces: vec![crate::view::TraceRow::read(&traces.rows(&line)[0], 11_000, &Default::default(), false)],
            seat: Seat::You, name: "You".into(), time: "13:04".into(), text: "make slides".into(),
            addressed: true, unheard: false, mention: false, deletable: false,
        }));
    }

    #[test]
    fn the_one_chat_draws_rows_traces_and_notices_between_the_restored_widths() {
        use crate::model::{NoticeAction, NoticeDetails, TownNotice};
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
        });
        let cx = &mut *pool;
        let ui = cx.with_vm(|vm| {
            let value = vm.eval(script! {
                use mod.widgets.*
                WorkspaceColumns{}
            });
            WidgetRef::script_from_value(vm, value)
        });
        let answers = ui.widgets(cx, &[ids!(call_answer)]);
        assert_eq!(answers.iter().count(), 1, "the chat leaves the call controls to the center page");
        assert!(answers.contains(&ui.widget(cx, ids!(main_area.live_call.call_answer))));
        let panels = ui.widgets(cx, &[ids!(share_picture_panel)]);
        assert_eq!(panels.iter().count(), 1, "the chat leaves the shared picture to the center page");
        assert!(panels.contains(&ui.widget(cx, ids!(main_area.live_share.share_picture_panel))));
        let rail = ui.splitter(cx, ids!(rail_split));
        let chat = ui.splitter(cx, ids!(chat_split));
        assert!(rail.borrow().is_some() && chat.borrow().is_some(), "the workspace has both splitters");
        rail.set_align(cx, SplitterAlign::FromA(193.));
        chat.set_align(cx, SplitterAlign::FromB(305. + BAR));
        let mut data = ViewData::default();
        let line = [7; 32];
        rows(&mut data, line);
        let size = dvec2(1000., 600.);
        assert!(ui.widget(cx, ids!(talk_page)).borrow::<View>().is_none(), "there is no second chat form");
        data.show_panes(cx, &ui);
        let form = data.conversation(cx, &ui);
        assert!(form.visible(), "the one chat is shown");
        // The notice is on the town's home page, not in the chat.
        for path in [ids!(town_notice), ids!(call_start)] {
            assert!(form.widget(cx, path).is_empty(), "no notice or Call button stacks above the chat's messages");
        }
        let town = [1; 32];
        let peer = [4; 65];
        let action = NoticeAction::Offer { town, peer, new: false };
        let notice = TownNotice {
            line: "Your town with Ada used an old format and was set aside.".into(),
            offer: Some(("Share this town with Ada".into(), action.clone())),
            details: Some(NoticeDetails { town, peer: Some(peer), label: "Show the reason", text: "archive".into() }),
            ..TownNotice::default()
        };
        let home = ui.widget(cx, ids!(home_page));
        TownNotice::display(cx, &home, Some(&notice));
        let card = home.widget(cx, ids!(town_notice));
        assert_eq!(card.label(cx, ids!(town_notice_text)).text(), notice.line);
        assert!(!card.button(cx, ids!(town_notice_ok)).visible(), "a notice without a second choice shows no second button");
        let offer = card.widget(cx, ids!(town_notice_share));
        let press = a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId(offer.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None };
        let clicks = cx.capture_actions(|cx| {
            offer.borrow_mut::<Button>().unwrap().handle_action(cx, &press, &mut Scope::empty());
        });
        assert_eq!(TownNotice::clicked(&clicks), Some(&action), "the click carries the displayed town and key");
        let fold = card.fold_header(cx, ids!(fold));
        assert!(!fold.is_open(cx), "the archive starts behind Details");
        fold.set_is_open(cx, true, Animate::No);
        assert_eq!(card.label(cx, ids!(explanation)).text(), "archive");
        TownNotice::display(cx, &home, None);
        assert!(!card.visible() && !fold.is_open(cx), "a resolved notice closes its disclosure");

        let (_homes, mut ada, mut bob) = crate::model::tests::shared_town("rail-badge");
        let shared = ada.open_id();
        let towns = bob.towns();
        let own = towns.iter().find(|t| t.id != shared).unwrap().id;
        bob.open_town(own).unwrap();
        data.chat_read.seed_loaded(&bob);
        for draft in ["The draft is up", "Ready for review", "@Bob please check"] {
            ada.post_chat_draft(draft, None, None, None).unwrap();
        }
        crate::model::tests::exchange(&mut ada, &mut bob);
        data.town_ids = towns.iter().map(|t| t.id).collect();
        data.tree.set(towns, bob.town_tree());
        data.chat_read.sync_counts(&bob, &data.town_ids, false);
        data.show_panes(cx, &ui);
        let view = std::sync::Arc::make_mut(&mut data.call.view);
        view.phase = crate::call::Phase::Live;
        view.peer_name = "Ada".into();
        data.sync_live_rows(cx, &ui);
        draw(cx, &ui, &mut data, size);
        let tree = ui.portal_list(cx, ids!(tree.list));
        let row = |node| {
            let index = data.tree.lines.iter().position(|line| line.node == node).unwrap();
            tree.get_item(index).unwrap().1
        };
        let town = row(crate::lists::Node::Town(own));
        let call = row(crate::lists::Node::Live(Live::Call));
        let shared_badge = row(crate::lists::Node::Town(shared)).view(cx, ids!(unread_badge));
        assert!(shared_badge.visible(), "the background town shows its unread badge");
        assert_eq!(shared_badge.label(cx, ids!(count)).text(), "3", "the badge shows the three synced messages");
        assert!(!town.view(cx, ids!(unread_badge)).visible(), "the read town hides its badge");
        let recording = |row: &WidgetRef| row.borrow::<View>().unwrap().draw_list_id().unwrap();
        let (town_list, call_list) = (recording(&town), recording(&call));
        cx.new_draw_event = Default::default();
        std::sync::Arc::make_mut(&mut data.call.view).duration_secs = 1;
        data.sync_live_rows(cx, &ui);
        assert_eq!(call.label(cx, ids!(name)).text(), "Call with Ada · 0:01");
        assert!(cx.new_draw_event.draw_list_will_redraw(cx, call_list));
        assert!(!cx.new_draw_event.draw_list_will_redraw(cx, town_list), "a clock tick leaves the town's drawing intact");
        let rail = rail.borrow().unwrap();
        let chat = chat.borrow().unwrap();
        assert_eq!(rail.area_a().rect(cx).size.x, 193., "left rail restores its width");
        assert_eq!(chat.area_b().rect(cx).size.x, 305., "chat restores its width");
        assert_eq!(rail.area_b().rect(cx).size.x, size.x - 193. - 305. - 2. * BAR, "the page gets the rest");
        for split in [&*rail, &*chat] {
            assert!(split.area().is_valid(cx), "a visible divider has a drawn bar");
            assert!(split.bar_grab_rect(cx).size.x > BAR, "a visible divider has a widened hit area");
        }

        let list = form.portal_list(cx, ids!(transcript.list));
        for (i, row) in data.workspace.rows.iter().enumerate() {
            let crate::view::ChatRow::Message(message) = row else { continue };
            let (_, row) = list.get_item(i).expect("the chat draws the transcript");
            if message.seat == Seat::Peer {
                assert!(row.widget(cx, ids!(author.peer)).visible());
                assert_eq!(row.label(cx, ids!(name)).text(), if message.addressed { "Ben asked their AI" } else { "Ben" });
                assert_eq!(row.label(cx, ids!(author.peer.initial)).text(), "B");
            }
            assert_eq!(row.label(cx, ids!(message)).text(), message.text);
            assert_eq!(row.label(cx, ids!(when)).text(), message.time);
            assert_eq!(row.widget(cx, ids!(sent)).visible(), message.traces.is_empty() && message.addressed && message.seat == Seat::You);
            if message.id == line {
                let rows = row.flat_list(cx, ids!(trace_rows));
                let rows = rows.borrow().expect("the bubble has a trace list");
                let output: Vec<_> = rows.items.values().filter(|r| r.template == id!(Output)).map(|r| r.widget.text()).collect();
                assert!(output.len() == 2 && output.contains(&"two".into()) && output.contains(&"three".into()), "the chat draws the two newest lines as labels");
                let fold = rows.items.values().find(|r| r.template == id!(Fold)).expect("a fold control for earlier output");
                assert!(fold.widget.text().contains("1 earlier"), "the visible disclosure counts its earlier lines");
                assert_eq!(fold.widget.action_data().as_ref().and_then(|d| d.downcast_ref::<crate::view::TraceFold>()).map(|k| k.line),
                    Some(line), "a recycled row's button identifies the displayed line");
            }
        }
    }

    #[test]
    fn the_form_follows_the_width_with_hysteresis_and_a_parked_window_gets_the_chat_form() {
        use makepad_widgets::makepad_platform::ime::HostedParked;
        assert_eq!(form_for_width(979., Form::Wide), Form::Narrow, "under 980 a wide window folds");
        assert_eq!(form_for_width(980., Form::Wide), Form::Wide, "980 is still wide");
        assert_eq!(form_for_width(1003., Form::Narrow), Form::Narrow, "a narrow window stays narrow until 24 pt past 980");
        assert_eq!(form_for_width(1004., Form::Narrow), Form::Wide, "1004 unfolds it");
        assert_eq!(form_for_width(979., Form::Narrow), Form::Narrow);
        assert_eq!(parked_form(&HostedParked { scale: Some(0.3), max: [400., 700.] }), Some(0), "a slot that fits gets the chat form");
        assert_eq!(parked_form(&HostedParked { scale: Some(0.3), max: [200., 700.] }), None, "a slot too narrow gets none");
        assert_eq!(parked_form(&HostedParked { scale: None, max: [400., 700.] }), None, "unparked answers none");
    }

    #[test]
    fn narrow_shows_one_pane_with_a_bar_and_wide_restores_the_columns() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
            crate::tweak::panel::script_mod(vm);
        });
        let cx = &mut *pool;
        let ui = cx.with_vm(|vm| {
            let value = vm.eval(script! {
                use mod.prelude.workspace.*
                use mod.widgets.*
                View{
                    width: Fill height: Fill flow: Down
                    narrow_bar := NarrowBar{}
                    workspace := View{width: Fill height: Fill flow: Right
                        workspace_columns := WorkspaceColumns{}
                        chat_edge := ShareChatEdge{}
                        tweak_slot := View{width: Fit height: Fill
                            tweak_panel := AppTweakPanel{width: 285 height: Fill visible: false}
                        }
                    }
                }
            });
            WidgetRef::script_from_value(vm, value)
        });
        let rail = ui.splitter(cx, ids!(rail_split));
        let chat = ui.splitter(cx, ids!(chat_split));
        rail.set_align(cx, SplitterAlign::FromA(193.));
        chat.set_align(cx, SplitterAlign::FromB(305. + BAR));
        let mut data = ViewData::default();
        rows(&mut data, [7; 32]);
        let width_of = |cx: &Cx, area: Area| if area.is_valid(cx) { area.rect(cx).size.x } else { 0. };
        // The widths of the chat, Places and page panes.
        let panes = |cx: &Cx| {
            let (rail, chat) = (rail.borrow().unwrap(), chat.borrow().unwrap());
            [width_of(cx, chat.area_b()), width_of(cx, rail.area_a()), width_of(cx, rail.area_b())]
        };

        data.form = Form::Narrow;
        let panel = ui.view(cx, ids!(tweak_panel));
        let slot = ui.view(cx, ids!(tweak_slot));
        let columns = ui.view(cx, ids!(workspace_columns));
        let edit = ui.view(cx, ids!(edit_tab));
        let edit_button = edit.button(cx, ids!(press));
        panel.set_visible(cx, true);
        data.show_panes(cx, &ui);
        assert!(edit.visible(), "an active share offers Edit in the narrow bar");
        let press = a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId::widget(edit_button.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None };
        let actions = cx.capture_actions(|cx| {
            edit_button.borrow_mut().unwrap().handle_action(cx, &press, &mut Scope::empty());
        });
        assert!(data.narrow_tab_actions(cx, &ui, &actions), "pressing Edit selects a narrow pane");
        data.show_panes(cx, &ui);
        assert!(data.pane == Pane::Edit && edit_button.open(), "the Edit tab opens its pane");
        draw(cx, &ui, &mut data, dvec2(600., 600.));
        assert!(!columns.visible() && !ui.widget(cx, ids!(chat_edge)).visible(), "Edit hides the columns and the chat edge");
        assert!(slot.visible(), "Edit shows the tweak slot");
        assert_eq!(panel.area().rect(cx), ui.widget(cx, ids!(workspace)).area().rect(cx), "the editor fills the entire narrow workspace");
        assert_eq!(width_of(cx, panel.area()), 600., "the narrow editor spans the window width");
        panel.set_visible(cx, false);
        data.show_panes(cx, &ui);
        assert_eq!(data.pane, Pane::Page, "ending a share while editing returns to the page");
        assert!(!edit.visible() && ui.button(cx, ids!(page_tab.press)).open(), "ending the share hides Edit and selects the page tab");
        draw(cx, &ui, &mut data, dvec2(600., 600.));
        assert!(columns.visible() && !slot.visible(), "ending the edit restores the columns and hides the tweak slot");
        assert_eq!(panes(cx), [0., 0., 600.], "the page fills the workspace after the share ends");
        panel.set_visible(cx, true);
        data.show_panes(cx, &ui);
        let name = "Walk A is sharing Demo plate · Fab with a page name that cannot fit the tab";
        let page_caption = ui.label(cx, ids!(page_tab.caption));
        page_caption.set_text(cx, name);
        for (w, icons) in [(600., false), (500., true), (600., false), (320., true)] {
            let size = dvec2(w, 600.);
            let form = form_for_width(w, data.form);
            assert!(!data.fit_tabs(cx, &ui, w, form, false), "resizing to {w} stays in the narrow form without rebuilding the page");
            for ((path, _), label) in PANE_TABS.into_iter().zip(["Chat", "Places", "Page", "Edit the shared app"]) {
                let tab = ui.widget(cx, path);
                assert_eq!(tab.label(cx, ids!(caption)).visible(), !icons, "{label} caption visibility at {w}");
                assert_eq!(tab.icon(cx, ids!(glyph)).visible(), icons, "{label} icon visibility at {w}");
                let button = tab.button(cx, ids!(press));
                let button = button.borrow().unwrap();
                let mut node = a11y_tree::Node::default();
                button.semantics(cx, &mut node);
                makepad_widgets::semantics::apply_overrides(&*button, None, cx, &mut node);
                assert_eq!(node.name.as_str(), label, "{label} keeps its accessible name at {w}");
            }
            assert_eq!(page_caption.text(), name, "resizing preserves the full page name");
            data.show_pane(Pane::Chat);
            for (pane, tab, expect) in [(Pane::Chat, ids!(chat_tab), [w, 0., 0.]), (Pane::Places, ids!(places_tab), [0., w, 0.]), (Pane::Page, ids!(page_tab), [0., 0., w])] {
                data.show_pane(pane);
                data.show_panes(cx, &ui);
                assert!(ui.widget(cx, ids!(narrow_bar)).visible(), "the bar shows when narrow");
                assert!(ui.widget(cx, tab).button(cx, ids!(press)).open(), "the {pane:?} segment is the open one");
                assert!(columns.visible() && !slot.visible() && edit.visible(), "{pane:?} hides the active panel but keeps its tab available");
                draw(cx, &ui, &mut data, size);
                assert_eq!(panes(cx), expect, "narrow at {w} shows only {pane:?}, at full width");
                for path in [ids!(narrow_bar), ids!(workspace), ids!(chat_tab), ids!(places_tab), ids!(page_tab), ids!(edit_tab)] {
                    let rect = ui.widget(cx, path).area().rect(cx);
                    assert!(rect.pos.x >= 0. && rect.pos.x + rect.size.x <= w + 0.5, "{path:?} stays inside {w} pt: {rect:?}");
                }
                if !icons && pane == Pane::Page {
                    let mut node = a11y_tree::Node::default();
                    page_caption.borrow().unwrap().semantics(cx, &mut node);
                    let lines = node.text.unwrap().lines;
                    assert_eq!(lines.len(), 1, "the long page caption occupies a single line");
                    assert!(f64::from(lines[0].rect.w) <= width_of(cx, page_caption.area()), "the truncated line fits the page tab");
                }
                if pane == Pane::Chat {
                    let input = ui.widget(cx, ids!(chat_input)).area().rect(cx);
                    assert!(input.size.y > 0. && input.pos.y + input.size.y <= 600.5 && input.pos.y + input.size.y > 600. - 120.,
                        "the chat input stays at the bottom of the Chat pane: {input:?}");
                }
            }
        }
        data.form = Form::Wide;
        data.show_panes(cx, &ui);
        draw(cx, &ui, &mut data, dvec2(1280., 600.));
        assert!(!ui.widget(cx, ids!(narrow_bar)).visible(), "the bar hides when wide");
        assert!(columns.visible() && slot.visible() && !edit.visible(), "wide shows the editor beside the columns and hides its tab");
        assert_eq!(width_of(cx, panel.area()), 285., "the wide editor returns to its fixed width");
        assert_eq!(panes(cx), [305., 193., 1280. - 285. - 193. - 305. - 2. * BAR], "wide has the three columns at their restored widths");
        panel.set_visible(cx, false);
        data.page = Page::Join;
        data.show_panes(cx, &ui);
        draw(cx, &ui, &mut data, dvec2(1280., 600.));
        assert_eq!(panes(cx)[0], 0., "joining another town shows none of the open town's chat");
        data.page = Page::Home;
        data.show_panes(cx, &ui);

        // A line arriving while another pane is shown puts a dot on Chat until Chat opens.
        data.form = Form::Narrow;
        data.show_pane(Pane::Chat);
        data.show_pane(Pane::Places);
        assert!(!data.chat_unread(), "nothing new yet");
        let peer_message = |id| crate::view::ChatRow::Message(Message {
            id, quote: None, traces: vec![], seat: Seat::Peer, name: "Kit".into(), time: "13:05".into(), text: "new".into(),
            addressed: false, unheard: false, mention: false, deletable: false,
        });
        data.workspace.rows.push(peer_message([99; 32]));
        data.show_panes(cx, &ui);
        assert!(ui.widget(cx, ids!(chat_dot)).visible(), "the new line shows the dot");
        data.show_pane(Pane::Chat);
        data.show_panes(cx, &ui);
        assert!(!ui.widget(cx, ids!(chat_dot)).visible(), "opening Chat clears it");

        use crate::call::{CallAction, CallController, CallEnv, Message as Signal, Press, SharePanelSize};
        use crate::lists::SharePopout;
        use makepad_call::driver::{std_spawn, VoiceSource};
        let mut call = CallController::default();
        call.configure(CallEnv { files: Default::default(), signal: std::sync::Arc::new(|| {}), voice: VoiceSource::Synthetic { tone_hz: 700. },
            devices_path: Default::default(), spawn: std_spawn(), jobs: std_spawn() });
        let port = call.link_up(cx, [1; 65], [2; 65], "Kit").unwrap();
        let session = [5; 16];
        port.messages_in.try_send(Signal::Share { session, title: "Drawing".into(), w: 1600., h: 999., ext: Default::default() }.encode()).unwrap();
        call.poll(cx);
        let share_page = Page::Live(Live::Share(session));
        let edge = ui.widget(cx, ids!(chat_edge));
        let button = edge.button(cx, ids!(show_chat));
        let dot = edge.widget(cx, ids!(edge_dot));
        let picture = ui.widget(cx, ids!(live_share.share_picture_panel));
        let size = dvec2(1120., 800.);
        data.form = Form::Wide;
        data.page = share_page;
        ui.widget(cx, ids!(home_page)).set_visible(cx, false);
        ui.widget(cx, ids!(project_page)).set_visible(cx, false);
        ui.widget(cx, ids!(live_share)).set_visible(cx, true);
        let sync = |cx: &mut Cx, data: &mut ViewData, call: &CallController| {
            data.call = call.shown();
            data.sync_live_share(cx, &ui);
        };
        sync(cx, &mut data, &call);
        data.show_panes(cx, &ui);
        assert!(!edge.visible(), "an unwatched offer keeps chat alongside the page");
        let layout = |cx: &mut Cx, data: &mut ViewData| {
            let actions = cx.capture_actions(|cx| draw(cx, &ui, data, size));
            actions.iter().filter_map(|a| a.as_widget_action())
                .filter(|w| w.widget_uid == picture.widget_uid())
                .find_map(|w| w.action.downcast_ref::<SharePanelSize>().copied())
        };
        layout(cx, &mut data);
        let with_chat = panes(cx);
        assert!(call.call_opening_presses(cx, CallAction::Press(Press::Watch)), "the viewer starts watching");
        sync(cx, &mut data, &call);
        let folded = layout(cx, &mut data).expect("the wider picture reports its fitted size");
        let strip = edge.area().rect(cx);
        assert_eq!(strip.size, dvec2(28., size.y), "the edge spans the height of the columns");
        assert_eq!(chat.collapse(), SplitterCollapse::B, "watching folds the chat side of the splitter");
        assert_eq!(panes(cx), [0., with_chat[1], with_chat[2] + 305. + BAR - strip.size.x],
            "the page gains the chat and divider widths less the edge");
        assert!(edge.visible() && !dot.visible(), "the folded edge starts with chat read");

        data.workspace.rows.push(peer_message([100; 32]));
        data.show_panes(cx, &ui);
        assert!(dot.visible(), "an incoming peer message marks the edge unread");

        data.form = Form::Narrow;
        data.show_panes(cx, &ui);
        assert!(!edge.visible() && chat.collapse() == SplitterCollapse::None, "narrow uses its selected pane without a fold");
        data.form = Form::Wide;
        data.show_panes(cx, &ui);
        assert!(edge.visible(), "widening folds again while the viewer has not opened chat");
        let press = a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId::widget(button.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None };
        let actions = cx.capture_actions(|cx| {
            button.borrow_mut().unwrap().handle_action(cx, &press, &mut Scope::empty());
        });
        data.chat_edge_actions(cx, &ui, &actions);
        let reopened = layout(cx, &mut data).expect("opening chat reports the smaller picture");
        assert_eq!(panes(cx), with_chat, "Show chat restores the 305-point column and original page width");
        assert_eq!(data.page, share_page, "Show chat leaves the viewer on the share");
        assert!(!edge.visible() && !data.chat_unread(), "opening chat hides the edge and marks the message read");
        assert!(folded.width > reopened.width && folded.height > reopened.height, "the subscription grows with the picture");
        for report in [folded, reopened] {
            assert_eq!(report.session, session, "the active share reports both fitted sizes");
            assert_eq!(report.height, (f64::from(report.width) * 999. / 1600.).round() as u32,
                "both reported pictures keep the shared window's aspect");
        }
        for form in [Form::Narrow, Form::Wide] {
            data.form = form;
            data.show_panes(cx, &ui);
            assert!(!edge.visible(), "the decision to show chat survives a form switch");
        }
        data.page = Page::Home;
        data.show_panes(cx, &ui);
        data.page = share_page;
        data.show_panes(cx, &ui);
        assert!(edge.visible(), "a new visit to the share folds chat again");
        data.page = Page::Home;
        data.show_panes(cx, &ui);
        layout(cx, &mut data);
        assert_eq!(panes(cx), with_chat, "leaving a folded share restores all columns");
        assert!(matches!(chat.align(), Some(SplitterAlign::FromB(w)) if w == 305. + BAR),
            "folding and unfolding preserve the saved FromB align");
        assert_eq!(chat.collapse(), SplitterCollapse::None, "leaving the page clears the splitter fold");
        data.page = share_page;
        data.show_panes(cx, &ui);
        data.share_popout = SharePopout::Open(session);
        data.sync_live_share(cx, &ui);
        // The page keeps its picture beside the pop-out (both views show the watch), so its fold stays.
        assert!(edge.visible() && chat.collapse() == SplitterCollapse::B, "popping out keeps the page's picture and its chat fold");
        data.share_popout = SharePopout::Closed;
        data.sync_live_share(cx, &ui);
        assert!(edge.visible(), "closing the pop-out leaves the page's fold as it was");
        assert!(call.call_actions(cx, CallAction::Press(Press::CloseWatch)), "the viewer closes the picture");
        sync(cx, &mut data, &call);
        assert!(!edge.visible() && chat.collapse() == SplitterCollapse::None, "closing the watch restores chat");
        call.link_down(cx, [2; 65]);
    }
}
