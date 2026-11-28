use super::*;

impl ShareShown {
    fn single_waiting_line(&self) -> Option<&str> {
        let [offer] = self.offers.as_slice() else { panic!("expected one offer") };
        self.waiting_line(offer)
    }
}
use crate::{link_worker::{linked_pair, FromWorker, LinkWorker, ToWorker}, model::{Action, Origin}};
use makepad_call::{driver::codec::{Codecs, Encoder, TestCodecs}, guard::LADDER};
use makepad_town_core::{event::SessionMode, fold::Outcome};
use std::{panic::{catch_unwind, AssertUnwindSafe}, sync::atomic::AtomicUsize, thread, time::Duration};

#[derive(Default)]
struct Reading {
    demand: Option<View>,
    popped: usize,
    hold_picture: bool,
    next_frame: bool,
    parks: usize,
    ended_why: Option<Arc<str>>,
    keys: usize,
    drops: usize,
}

#[test]
fn guest_files_visual_issue_from_watched_picture() {
    use crate::{lists::{Live, Page, Pane, ViewData}, model::tests::{group_town, record_call_event},
        record::capture::BoardSnaps, view::{LocalClock, Workspace}};
    use makepad_town_core::{blob, entity::{field, kind}, event::{Content, CopySource, Member, SessionOp, Value},
        trust::{Gate, LocalPolicy, Withheld}};
    use makepad_widgets::{makepad_draw::cx_draw::CxDraw, *};
    use makepad_platform::studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp};

    let (_homes, mut members) = group_town("guest-issue", 3);
    let session = [7; 16];
    let anchor = record_call_event(&mut members, 1, Content::Session(SessionOp::Open { session,
        mode: SessionMode::Share { title: "Canvas".into() }, to: vec![], town_wide: true }));
    let joined = record_call_event(&mut members, 2, Content::Session(SessionOp::Join { session }));
    let [mut ben, mut opener, mut source] = members.try_into().ok().unwrap();
    let sharer = source.me();
    let project = ben.act(crate::model::Action::AddProject { title: "Design".into() }, Origin::Ui).unwrap();
    let board = ben.act(crate::model::Action::AddBoard { title: "Sketches".into(), project }, Origin::Ui).unwrap();
    let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let ui = cx.with_vm(|vm| {
        let value = script_eval!(vm, { use mod.prelude.widgets.* use mod.widgets.* View{
            live_share := LiveSharePage{} issues_page := IssuesPage{visible: false}
        } });
        WidgetRef::script_from_value(vm, value)
    });
    let (signal, wake) = std::sync::mpsc::channel();
    ben.start_store(cx.task_pool(), move || { let _ = signal.send(()); }).unwrap();
    let mut call = CallController::default();
    call.begin_offer(session, ben.open_id(), "Ana\nCanvas".into(), ShareRole::Viewer { sharer }, (320., 240.));
    call.watch(session);
    let clock = LocalClock::default();
    let mut data = ViewData { page: Page::Live(Live::Share(session)), call: call.shown(),
        workspace: Workspace::read(&ben, false, &clock), ..Default::default() };
    data.tree.set(ben.towns(), ben.town_tree());
    data.sync_live_share(&mut cx, &ui);
    let button = ui.widget(&cx, ids!(live_share.share_issue));
    let snap_button = ui.button(&cx, ids!(live_share.snap_to_board));
    assert!(!button.visible() && !snap_button.visible(), "both guest controls wait for a decoded picture");
    // Neutral, red, green and blue 2x2 blocks, including black and white luma endpoints.
    data.call.share.picture = Some(Arc::new(Picture { width: 4, height: 4, pts_100ns: 1,
        nv12: vec![16,235,63,63, 128,200,63,63, 173,173,32,32, 173,173,32,32, 128,128,102,240, 42,26,240,118] }));
    data.sync_share_issue(&mut cx, &ui);
    assert!(button.visible() && snap_button.visible(), "the first received picture reveals both guest controls");
    let mut links = AppLinks::default();
    let mut snaps = BoardSnaps::default();
    snaps.start(&cx, &mut links, &data.call.share, session, ben.open_id(), board).unwrap();
    while snaps.busy() { snaps.poll(&mut ben); std::thread::yield_now(); }
    let snap_id = loop {
        if let Some((town, id)) = ben.drain_store().board_events.pop() {
            assert_eq!(town, ben.open_id());
            break id
        }
        assert!(ben.clips_pending(), "snap should publish a board image: {:?}", ben.take_recording_problems());
        wake.recv().expect("the snap store wakes the guest");
    };
    let size = dvec2(640., 480.);
    let pass = DrawPass::new(&mut cx);
    pass.set_size(&mut cx, size);
    let mut list = DrawList2d::new(&mut cx);
    let mut draw = |cx: &mut Cx, data: &mut ViewData| {
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
    };
    // This fixture's Cx has an empty handler. Forward each settled focus change to the UI.
    let settle_focus = |cx: &mut Cx, data: &mut ViewData| {
        let prev = cx.keyboard.key_focus();
        cx.handle_actions();
        let focus = cx.keyboard.key_focus();
        if prev != focus {
            ui.handle_event(cx, &Event::KeyFocus(KeyFocusEvent { prev, focus }), &mut Scope::with_data(data));
        }
    };
    draw(&mut cx, &mut data);
    let abs = button.area().clipped_rect(&cx).center();
    let window_id = WindowId(1, 1);
    let primary = MouseButton::PRIMARY;
    cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown { x: abs.x, y: abs.y, time: 1.,
        button_raw_bits: primary.bits(), ..Default::default() }), window_id, DVec2::default());
    let down = Event::MouseDown(MouseDownEvent { abs, window_id, time: 1., button: primary,
        handled: Default::default(), modifiers: Default::default() });
    ui.handle_event(&mut cx, &down, &mut Scope::with_data(&mut data));
    settle_focus(&mut cx, &mut data);
    assert!(cx.has_key_focus(button.area()), "the mouse press focuses the guest control");
    let up = Event::MouseUp(MouseUpEvent { abs, window_id, time: 1.1, button: primary, modifiers: Default::default() });
    let actions = cx.capture_actions(|cx| ui.handle_event(cx, &up, &mut Scope::with_data(&mut data)));
    cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp { x: abs.x, y: abs.y, time: 1.1,
        button_raw_bits: primary.bits(), ..Default::default() }), window_id, DVec2::default());
    assert!(data.share_issue_input(&mut cx, &ui, &actions, &mut links, &mut ben), "the guest's press opens a draft");
    assert_eq!((data.page, data.pane, data.workspace.current), (Page::Issues, Pane::Page, Some(project)), "Issues opens in the current project");
    assert_eq!(data.issue_share(), data.watched_share(), "the draft keeps the original watch");
    assert!(ben.project_issues(project).is_empty(), "the draft opens before any issue is filed");
    let share = ui.widget(&cx, ids!(live_share));
    share.set_visible(&mut cx, false);
    let page = ui.widget(&cx, ids!(issues_page));
    page.set_visible(&mut cx, true);
    data.issues.sync(&mut cx, &ui, Some(project), &mut ben, &clock);
    draw(&mut cx, &mut data);
    cx.extend_actions(actions);
    settle_focus(&mut cx, &mut data);
    let input = page.text_input(&cx, ids!(issue_input));
    assert!(!input.area().is_empty() && cx.has_key_focus(input.area()), "the drawn draft's note owns keyboard focus");

    while !data.issues.link_events(&mut links, &[], true, &mut ben) { std::thread::yield_now(); }
    data.issues.sync(&mut cx, &ui, Some(project), &mut ben, &clock);
    assert!(!page.widget(&cx, ids!(problem)).visible(), "the decoded picture reaches the store worker");
    let receipt = loop {
        if let Some(receipt) = ben.drain_store().issues.pop() { break receipt }
        wake.recv().expect("the store wakes the guest");
    };
    data.issues.stored(&mut ben, receipt);
    let issues = ben.project_issues(project);
    assert_eq!(issues.len(), 1, "one issue is filed in the guest's selected project");
    let issue = issues[0];
    assert_eq!((issue.value(field::W), issue.value(field::H)), (Some(&Value::Int(4)), Some(&Value::Int(4))), "the issue has the received pixel size");
    assert_eq!(issue.value(field::SESSION), Some(&Value::Key(session.to_vec())), "the issue identifies the live share");
    assert_eq!(issue.text(field::LABEL), "Ana Canvas · 320 × 240 pt · as received", "the label carries the window's point size on one line");
    let snap = &ben.state.entities[&snap_id];
    assert_eq!(snap.kind, kind::IMAGE);
    assert_eq!(snap.value(field::BOARD), Some(&Value::Id(board)));
    assert_eq!(snap.value(field::SESSION), issue.value(field::SESSION));
    assert_eq!(snap.value(field::BLOB), issue.value(field::BLOB), "both actions captured the same frame");
    assert_eq!(snap.value(field::POSTER), issue.value(field::POSTER), "both stored the frame's poster");
    assert_eq!(snap.text(field::MEDIA), "image/png");
    let captured = [(issue.id, &[field::LABEL, field::BLOB, field::POSTER][..]),
        (snap.id, &[field::TITLE, field::BLOB, field::POSTER, field::MEDIA][..])];
    let sources = [CopySource::Record(anchor), CopySource::SharePeer { peer: sharer }];
    let gate = Gate::new(&ben.state, LocalPolicy::Members);
    for (id, fields) in captured {
        for field in fields {
            assert_eq!(ben.state.copied_from(id, id, field), sources, "{field} must name the watched sharer and the session anchor");
            assert!(gate.entity_field_value(&id, field).is_ok(), "active members can read the captured {field}");
        }
    }
    for (member, last, role) in [(&mut source, joined, "sharer"), (&mut opener, anchor, "opener")] {
        let device = member.me();
        let seat = member.state.seat_of(&device).unwrap();
        let removal = record_call_event(std::slice::from_mut(member), 0, Content::Member(Member::Remove { seat, device, last }));
        let mut state = ben.state.clone();
        state.receive(member.event(&removal).unwrap());
        assert!(!state.active_member(&device), "the {role} was removed");
        let gate = Gate::new(&state, LocalPolicy::Members);
        for (id, fields) in captured {
            for field in fields {
                assert_eq!(gate.entity_field_value(&id, field), Err(Withheld::External),
                    "removing the {role} alone must withhold the captured {field}");
            }
        }
    }
    let Some(Value::Blob { size, root }) = issue.value(field::BLOB) else { panic!("the issue references its full PNG") };
    let path = blob::data_path(&ben.home.blobs_dir().join(makepad_town_core::hex(&ben.open_id())), &blob::id(*size, root));
    let png = makepad_ai_services::pictures::decode_png(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!((png.width, png.height), (4, 4), "the full PNG preserves the received dimensions");
    for (pixel, rgba) in [(0, [0,0,0,255]), (1, [255; 4]), (2, [255,1,0,255]), (8, [0,255,1,255]), (10, [1,0,255,255])] {
        let got = &png.rgba[pixel * 4..pixel * 4 + 4];
        assert!(got.iter().zip(rgba).all(|(a,b)| a.abs_diff(b) <= 2), "pixel {pixel} has the display colour {rgba:?}, got {got:?}");
    }
    data.page = Page::Live(Live::Share(session));
    page.set_visible(&mut cx, false);
    share.set_visible(&mut cx, true);
    data.sync_live_share(&mut cx, &ui);
    draw(&mut cx, &mut data);
    let face = button.borrow::<Button>().unwrap();
    assert!(face.animator_in_state(&cx, ids!(hover.off)), "the returning control has cleared its hover without a pointer move");
    assert!(face.animator_in_state(&cx, ids!(focus.off)), "the returning control has no mouse focus ring");
    ben.stop_store();
    while !ben.store_stopped() {
        ben.drain_store();
        if !ben.store_stopped() { wake.recv().unwrap(); }
    }
}

struct Held {
    readings: Arc<Mutex<Reading>>,
    encoder: Box<dyn Encoder>,
    pixels: Vec<u8>,
    pending: bool,
    pts: i64,
}

impl Drop for Held {
    fn drop(&mut self) { self.readings.lock().unwrap().drops += 1; }
}

impl EncodedSource for Held {
    fn set_wake(&mut self, _: Arc<dyn Fn() + Send + Sync>) {}
    fn set_kbps(&mut self, kbps: u32) { self.encoder.set_kbps(kbps); }
    fn want(&mut self, width: u32, height: u32, fps: u32) {
        let mut reading = self.readings.lock().unwrap();
        if width == 0 { reading.parks += 1; }
        reading.demand = Some(View { width, height, fps });
    }
    fn key_now(&mut self) { self.readings.lock().unwrap().keys += 1; self.encoder.request_key(); self.pending = true; }
    fn frame_counts(&self) -> Option<(u64, u64)> { Some((self.pts as u64 / 100_000, self.pts as u64 / 100_000)) }
    fn pop(&mut self) -> Option<Packet> {
        {
            let mut reading = self.readings.lock().unwrap();
            reading.popped += 1;
            self.pending |= std::mem::take(&mut reading.next_frame);
            if reading.hold_picture || reading.ended_why.is_some() { return None }
        }
        if !std::mem::take(&mut self.pending) { return None }
        self.pts += 100_000;
        self.encoder.push(&self.pixels, self.pts).unwrap().pop()
    }
}

fn held_source() -> (Box<Held>, Arc<Mutex<Reading>>) {
    let readings: Arc<Mutex<Reading>> = Arc::default();
    let rung = LADDER[3];
    let source = Held { readings: readings.clone(), encoder: TestCodecs.encoder(rung, 198).unwrap(),
        pixels: vec![128; (rung.width * rung.height * 3 / 2) as usize], pending: false, pts: 0 };
    (Box::new(source), readings)
}

#[derive(Default)]
pub(crate) struct Host { installs: usize, share: Option<ShareSpeaker> }
impl AudioHost for Host {
    fn install(&mut self, _: MicPort, _: OutputPort, share: ShareSpeaker) {
        self.installs += 1;
        self.share = Some(share);
    }
    fn use_inputs(&mut self, ids: &[AudioDeviceId]) { assert!(ids.is_empty()); }
    fn use_outputs(&mut self, ids: &[AudioDeviceId]) { assert!(ids.is_empty()); }
}

struct Seat {
    ctl: CallController,
    host: Host,
    worker: LinkWorker,
}

impl Seat {
    fn press(&mut self, p: Press) {
        assert!(self.ctl.call_opening_presses(&mut self.host, CallAction::Press(p)) ||
            self.ctl.call_actions(&mut self.host, CallAction::Press(p)), "{p:?} changed state");
    }
}

struct Pair { seats: [Seat; 2], panics: Arc<AtomicUsize> }

impl Pair {
    fn wait(&mut self, mut ready: impl FnMut(&mut [Seat; 2]) -> bool) {
        loop {
            for s in &mut self.seats {
                while let Some(event) = s.worker.try_recv() {
                    assert!(!matches!(event, FromWorker::Failed(_) | FromWorker::Lost), "link stopped: {event:?}");
                }
                s.ctl.poll(&mut s.host);
                assert!(s.ctl.linked());
                assert!(!s.ctl.current.as_ref().is_some_and(Call::stopping), "a stopping session was kept after poll");
                assert!(!s.ctl.current.as_ref().is_some_and(Call::stopped), "active media worker stopped");
            }
            assert_eq!(self.panics.load(Ordering::Acquire), 0, "a worker panicked");
            if ready(&mut self.seats) { return }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Exercise the panel's normal scoped draw, including the first-presentation acknowledgement.
/// Draw both share views at the same scope and fixed viewport they receive in Town.
/// Returns the text runs the panel itself laid out and the size it reported.
fn panel_drawer() -> impl FnMut(CallShown, bool) -> (usize, Option<crate::call::SharePanelSize>) {
    use makepad_widgets::{makepad_draw::{cx_2d::TextRunSink, cx_draw::CxDraw}, *};
    let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let ui = cx.with_vm(|vm| {
        let value = script_eval!(vm, { use mod.prelude.widgets.* use mod.widgets.* View{flow: Overlay
            live_share := LiveSharePage{}
            share_popout := View{share_view := SharePopoutBody{}}
        } });
        WidgetRef::script_from_value(vm, value)
    });
    let center = ui.widget(&cx, ids!(live_share.share_picture_panel));
    let popped = ui.widget(&cx, ids!(share_popout.popout_picture));
    let pass = DrawPass::new(&mut cx);
    let size = dvec2(320., 600.);
    pass.set_size(&mut cx, size);
    let mut list = DrawList2d::new(&mut cx);
    let mut data = crate::lists::ViewData::default();
    move |shown, detached| {
        data.call = shown;
        data.page = crate::lists::Page::Live(crate::lists::Live::Share(data.call.share.offers[0].session));
        data.share_popout = if detached { crate::lists::SharePopout::Open(data.watched_share().unwrap()) }
            else { crate::lists::SharePopout::Closed };
        let panel = if detached { &popped } else { &center };
        data.sync_live_share(&mut cx, &ui);
        assert_eq!(popped.visible(), detached && data.call.share.watching.is_some());
        assert_eq!(center.visible(), data.call.share.shows_picture(&data.call.share.offers[0]),
            "the center shows the guest's watched picture or the sharer's own view, beside an open window too");
        if let Some(offer) = data.call.share.offers.first().filter(|o| matches!(o.role, ShareRole::Viewer { .. })) {
            for widget in [&center, &popped] {
                let mut node = a11y_tree::Node::default();
                widget.borrow::<crate::call::share_panel::SharePanel>().unwrap().semantics(&cx, &mut node);
                assert_eq!(node.name, format!("{} shared by {}", offer.title, data.call.view.peer_name));
            }
        }
        if let Some(head) = &data.call.share.offers[0].sharer_line {
            assert_eq!(ui.label(&cx, ids!(live_share.status)).text(), head.as_ref());
            assert!(ui.widget(&cx, ids!(live_share.stop_share)).visible());
            assert!(!ui.widget(&cx, ids!(live_share.watch)).visible());
        }
        if let Some(line) = data.call.share.offers[0].stopped_line.as_deref().filter(|_| data.call.share.watching.is_none()) {
            assert_eq!(ui.label(&cx, ids!(live_share.status)).text(), line,
                "the share page reports a stopped picture even when the panel is closed");
        }
        let mut runs = 0;
        let actions = cx.capture_actions(|cx| {
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.text_runs = Some(TextRunSink::default());
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            ui.draw_all(&mut frame, &mut Scope::with_data(&mut data));
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
            runs = frame.text_runs.take().unwrap().runs.iter().filter(|r| r.owner == panel.widget_uid().0).count();
        });
        // Each view of the picture reports its own size; the guest asks for the larger.
        let reported = actions.iter().filter_map(|a| a.as_widget_action()).filter(|w| w.widget_uid == panel.widget_uid())
            .filter_map(|w| w.action.downcast_ref::<crate::call::SharePanelSize>().copied()).collect::<Vec<_>>();
        assert!(reported.len() <= 1, "a single draw reports at most one size per view");
        let reported = reported.first().copied();
        if data.call.share.watching.is_some() {
            let area = panel.area().rect(&cx);
            assert!(area.size.y > size.y * 0.5 && area.size.x > size.x * 0.5,
                "the panel fills the remaining center page: {area:?}");
            if let Some(size) = reported {
                let (w, h) = data.call.share.offers.first().unwrap().window_size;
                assert!((f64::from(size.width) * h - f64::from(size.height) * w).abs() <= w.max(h),
                    "the requested size fits the shared window's aspect to within a pixel");
            }
        }
        (runs, reported)
    }
}

fn linked_controllers(a: &crate::model::Model, b: &crate::model::Model) -> (Pair, Arc<AtomicUsize>) {
    linked_with(a, b, |_, _| {})
}

fn linked_with(a: &crate::model::Model, b: &crate::model::Model, mut intercept: impl FnMut(&Link, &MediaPort)) -> (Pair, Arc<AtomicUsize>) {
    let (workers, _) = linked_pair(a.device_key(), b.device_key());
    let panics = Arc::new(AtomicUsize::new(0));
    let networks = Arc::new(AtomicUsize::new(0));
    let tracked = panics.clone();
    let starts = networks.clone();
    let spawn: Spawn = Arc::new(move |name, job| {
        if name == "call network" { starts.fetch_add(1, Ordering::Release); }
        let p = tracked.clone();
        thread::Builder::new().name(name.into()).spawn(move || {
            if catch_unwind(AssertUnwindSafe(job)).is_err() { p.fetch_add(1, Ordering::Release); }
        }).unwrap();
    });
    let mut workers = workers.into_iter();
    let seats = std::array::from_fn(|i| {
        let model = if i == 0 { a } else { b };
        let (worker, peer) = workers.next().unwrap();
        let mut ctl = CallController::default();
        ctl.codecs = Arc::new(TestCodecs);
        ctl.configure(CallEnv { files: Default::default(), spawn: spawn.clone(), jobs: spawn.clone(), signal: Arc::new(|| {}),
            devices_path: model.home.call_devices_path(), voice: VoiceSource::Feed });
        let mut host = Host::default();
        let port = ctl.link_up(&mut host, model.me(), peer, "Peer").unwrap();
        ctl.set_open_town(Some((a.open_id(), vec![(peer, "Peer".into())])));
        ctl.set_peer_caps(peer, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        intercept(&ctl.links[&peer], &port);
        assert!(worker.try_send(ToWorker::Call(port)).is_ok());
        Seat { ctl, host, worker }
    });
    (Pair { seats, panics }, networks)
}

fn linked_call(a: &crate::model::Model, b: &crate::model::Model) -> (Pair, Arc<AtomicUsize>) {
    let (mut pair, networks) = linked_controllers(a, b);
    pair.seats[0].press(Press::Call);
    pair.wait(|s| s[1].ctl.view().ringing());
    pair.seats[1].press(Press::Answer);
    pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Live && s.ctl.voice_attached));
    (pair, networks)
}

#[test]
fn crossed_calls_and_plain_calls_each_spend_one_session() {
    let (_homes, a, b) = crate::model::tests::shared_town("media-periods");
    for (first, second) in [(&a, &b), (&b, &a)] {
        let (mut pair, networks) = linked_controllers(first, second);
        for (index, glare) in [true, false].into_iter().enumerate() {
            pair.seats[0].press(Press::Call);
            if glare {
                pair.seats[1].press(Press::Call);
            } else {
                pair.wait(|s| s[1].ctl.view().ringing());
                pair.seats[1].press(Press::Answer);
            }
            pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Live && s.ctl.voice_attached));
            pair.seats[0].press(Press::HangUp);
            pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Idle && !s.ctl.has_call_workers()));
            for s in &pair.seats { assert_eq!(s.ctl.links.values().next().unwrap().media.session_last, index as u32 + 1); }
            assert_eq!(networks.load(Ordering::Acquire), 2 * (index + 1));
        }
    }
}

#[test]
fn linked_share_joins_on_display_and_survives_the_call() {
    let (_homes, mut a, mut b) = crate::model::tests::shared_town("share");
    let (mut pair, networks) = linked_call(&a, &b);
    let initial_workers = networks.load(Ordering::Acquire);
    assert_eq!(initial_workers, 2, "one network worker per end");
    let (source, readings) = held_source();
    readings.lock().unwrap().hold_picture = true;
    let window = AppWindow { id: WindowId(7), title: "Drawing".into(), visible: true, size: (640., 360.), dpi: 2. };
    let ctl = &mut pair.seats[0].ctl;
    ctl.start_window(LinkId::TEST, &window, "Drawing".into(), source).unwrap();
    let probe = readings.clone();
    ctl.sharing.locals.get_mut(&ctl.sharing.shown.offers[0].session).unwrap().probe = Some(Box::new(move || (1, probe.lock().unwrap().ended_why.clone())));
    let session = ctl.sharing.shown.offers.first().unwrap().session;
    pair.wait(|s| s[1].ctl.viewer_session() == Some(session) && readings.lock().unwrap().demand.is_some());
    assert_eq!(readings.lock().unwrap().demand, Some(View::default()));
    assert_eq!(readings.lock().unwrap().popped, 0, "no source poll before Watch");
    for (i, seat) in pair.seats.iter_mut().enumerate() {
        let role = if i == 0 { ShareRole::Sharer { link: LinkId::TEST, window: window.id } } else { ShareRole::Viewer { sharer: a.me() } };
        assert_eq!(seat.ctl.take_share_events(), [ShareEvent::Started { session, town: a.open_id(), title: "Drawing".into(), role }]);
        assert!(seat.ctl.take_share_events().is_empty());
    }
    assert!(pair.seats[0].ctl.take_facts().iter().any(|f| matches!(f, Fact::ShareOpen { session: id, title, town }
        if *id == session && title == "Drawing" && *town == a.open_id())));
    assert!(!pair.seats[1].ctl.take_facts().iter().any(|f| matches!(f, Fact::ShareJoin { .. })));
    a.act(Action::SessionOpen { town_wide: false, town: None, session, mode: SessionMode::Share { title: "Drawing".into() },
        to: vec![a.call_seat(&b.me()).unwrap()] }, Origin::Ui).unwrap();
    let viewer = &mut pair.seats[1];
    // Make the offer much older than the first draw without spending that time in the test.
    viewer.ctl.sharing.shown.offer_mut(session).unwrap().offered_at -= Duration::from_secs(90);
    assert!(!viewer.ctl.call_actions(&mut viewer.host, CallAction::Press(Press::Watch)), "an injected Watch cannot subscribe");
    viewer.press(Press::Watch);
    viewer.ctl.share_panel_size(session, crate::call::ShareSurface::Page, 320, 180);
    pair.wait(|_| readings.lock().unwrap().popped > 0);
    let mut draw_own = panel_drawer();
    let (_, own) = draw_own(pair.seats[0].ctl.shown(), false);
    assert!(own.is_some_and(|r| r.session == session && r.width > 0),
        "the sharer's own view reports its page size, so its preview fits the page: {own:?}");
    let mut draw_panel = panel_drawer();
    let viewer = &mut pair.seats[1];
    assert_eq!(viewer.ctl.shown().share.single_waiting_line(), Some("Waiting for Peer's picture…"));
    let (runs, reported) = draw_panel(viewer.ctl.shown(), false);
    assert_eq!(runs, 1, "the waiting panel lays out one line inside the center page");
    assert!(reported.is_some_and(|r| r.session == session && r.width > 0 && r.height > 0),
        "the waiting panel reports its size, so the sharer can send a picture: {reported:?}");
    for detached in [true, false] {
        let (runs, size) = draw_panel(viewer.ctl.shown(), detached);
        assert_eq!(runs, 1, "the waiting line shows in each view of the picture");
        // The window measures on its first draw; the page kept its picture, and its size stays subscribed.
        assert_eq!(size.map(|s| s.session), detached.then_some(session), "each view measures once, when it first draws");
    }
    assert!(viewer.ctl.take_facts().is_empty(), "a subscription without a displayed frame has no Join");
    viewer.press(Press::CloseWatch);
    pair.wait(|_| readings.lock().unwrap().demand == Some(View::default()));
    let viewer = &mut pair.seats[1];
    assert!(viewer.ctl.take_facts().is_empty(), "closing before the first picture records neither Join nor Leave");
    viewer.press(Press::Watch);
    viewer.ctl.share_panel_size(session, crate::call::ShareSurface::Page, 320, 180);
    readings.lock().unwrap().hold_picture = false;
    pair.wait(|s| s[1].ctl.sharing.shown.picture.is_some());
    let viewer = &mut pair.seats[1].ctl;
    let pic = viewer.take_share_picture_update().unwrap().unwrap();
    assert_eq!((pic.width, pic.height), (320, 180));
    assert!(Arc::ptr_eq(&pic, viewer.sharing.shown.picture.as_ref().unwrap()));
    assert!(viewer.shown().share.single_waiting_line().is_none());
    assert!(viewer.take_facts().is_empty(), "decoding a frame outside a draw cannot join the viewer");
    assert_eq!(draw_panel(viewer.shown(), false).0, 0, "the picture replaces the waiting line on its draw");
    assert_eq!(viewer.take_facts(), [Fact::ShareJoin { session, town: a.open_id(), sharer: a.me() }]);
    let first_draw = *viewer.sharing.shown.offers.first().unwrap().first_draw.get().unwrap();
    b.act(Action::SessionJoin { peer: None, town: None, session }, Origin::Ui).unwrap();

    let resized = AppWindow { size: (480., 270.), ..window.clone() };
    let sharer = &mut pair.seats[0].ctl;
    let offer = sharer.sharing.shown.offer(session).unwrap().clone();
    assert!(sharer.resize_share(&offer, resized.size, resized.dpi));
    pair.wait(|s| s[1].ctl.sharing.shown.watched().is_some_and(|o| o.window_size == resized.size));
    let viewer = &mut pair.seats[1].ctl;
    assert_eq!(viewer.sharing.shown.watching, Some(session), "resizing keeps the same watch");
    // Model the decoder losing its picture handle while the UI still has Watch open.
    viewer.current.as_mut().unwrap().watch_share(viewer.links[&a.me()].peer_id, View::default());
    viewer.poll_share();
    assert!(matches!(viewer.take_share_picture_update(), Some(None)), "one redraw clears the old picture");
    viewer.poll_share();
    assert!(viewer.take_share_picture_update().is_none(), "a still-empty panel needs no further redraw");
    assert_eq!(viewer.shown().share.single_waiting_line(), Some("Waiting for Peer's picture…"));
    assert_eq!(draw_panel(viewer.shown(), false).0, 1);
    pair.seats[1].press(Press::CloseWatch);
    pair.wait(|_| readings.lock().unwrap().demand == Some(View::default()));
    // Reopen before layout reports another size. An unchanged panel still needs a new key.
    pair.seats[1].press(Press::Watch);
    pair.wait(|s| s[1].ctl.sharing.shown.picture.is_some());
    assert_eq!(draw_panel(pair.seats[1].ctl.shown(), false).0, 0);
    assert!(pair.seats[1].ctl.take_facts().is_empty(), "recovery does not join twice");
    pair.seats[0].press(Press::HangUp);
    pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Idle));
    assert!(pair.seats.iter().all(|s| s.number() == 1 && s.ctl.current.is_some() && !s.ctl.voice_attached));
    assert!(pair.seats[1].ctl.sharing.shown.picture.is_some());
    pair.flush_signals();
    let sent = pair.seats[0].ctl.messages_sent();
    readings.lock().unwrap().ended_why = Some(format!("encoder stopped:\n\r\u{1b} {}", "窗".repeat(1000)).into());
    let sharer = &mut pair.seats[0].ctl;
    assert!(sharer.poll_share(), "the ended stream changes the sharer's page");
    assert!(!sharer.poll_share(), "a second poll of the same ended stream has no new status");
    assert_eq!(sharer.messages_sent() - sent, 1, "the failure sends exactly one reliable message");
    let shown = sharer.shown();
    let why = shown.share.offers[0].sharer_line.as_deref().unwrap().strip_prefix("Sharing Drawing with Peer. Drawing's picture stopped: ")
        .expect("the sharer sees which picture stopped").to_owned();
    assert!(why.starts_with("encoder stopped: 窗") && why.ends_with('…') && why.len() <= 240,
        "the worker's long reason becomes one bounded line");
    assert!(!why.chars().any(char::is_control), "control characters do not reach the page");
    draw_panel(shown, false);
    let stopped = format!("Peer's picture stopped: {why}");
    pair.wait(|s| s[1].ctl.shown().share.single_waiting_line() == Some(stopped.as_str()));
    let viewer = &mut pair.seats[1];
    assert_eq!(viewer.ctl.viewer_session(), Some(session), "a picture failure leaves the offer open");
    for detached in [false, true] {
        assert_eq!(draw_panel(viewer.ctl.shown(), detached).0, 1, "the active panel replaces its last picture with the reason");
    }
    viewer.press(Press::CloseWatch);
    draw_panel(viewer.ctl.shown(), false);
    viewer.press(Press::Watch);
    assert_eq!(viewer.ctl.shown().share.single_waiting_line(), Some(stopped.as_str()), "reopening the failed picture keeps the reason");
    assert_eq!(pair.seats[0].ctl.messages_sent() - sent, 1, "later polls did not repeat the failure");
    let previous_problem = ShareProblem::new(session, "late report").encode();
    let before_stop = first_draw.elapsed().as_secs();
    pair.seats[0].press(Press::StopShare);
    pair.wait(|s| s.iter().all(|s|
        !s.ctl.has_call_workers() && s.ctl.sharing.shown.offer(session).is_none()
    ));
    assert_eq!(readings.lock().unwrap().drops, 1, "Stop releases the watched source");
    assert!(pair.seats.iter().all(|s| s.ctl.sharing.shown.offers.is_empty() && s.ctl.sharing.shown.picture.is_none()));
    assert_eq!(networks.load(Ordering::Acquire), initial_workers);
    let pair_sharer = a.me();
    for (seat, model) in pair.seats.iter_mut().zip([&mut a, &mut b]) {
        assert_eq!(seat.ctl.take_share_events(), [ShareEvent::Ended { session, reason: ShareEndReason::Stopped }]);
        let secs = seat.ctl.take_facts().into_iter().find_map(|f| match f {
            Fact::Leave { session: id, secs, why: LeaveWhy::Ended } if id == session => secs, _ => None,
        }).expect("the share recorded its own duration");
        if model.me() != pair_sharer {
            assert!((before_stop..=first_draw.elapsed().as_secs()).contains(&u64::from(secs)), "viewer duration starts at display");
        }
        model.act(Action::SessionLeave { session, secs: Some(secs), why: LeaveWhy::Ended }, Origin::Ui).unwrap();
    }
    crate::model::tests::exchange(&mut a, &mut b);
    for model in [&a, &b] {
        let summary = model.state.session_summary(&session, None).unwrap();
        assert_eq!(summary.mode, SessionMode::Share { title: "Drawing".into() });
        assert!(matches!(summary.outcome, Outcome::Answered { secs: Some(_), .. }));
    }

    // Neither ending before Answer closes the unseen share.
    for (side, ending) in [(0, Press::Cancel), (1, Press::Decline)] {
        let (source, _) = held_source();
        pair.seats[0].ctl.start_window(LinkId::TEST, &window, "Preview".into(), source).unwrap();
        let session = pair.seats[0].ctl.sharing.shown.offers.first().unwrap().session;
        pair.wait(|s| s[1].ctl.viewer_session() == Some(session));
        pair.seats[1].press(Press::Watch);
        pair.wait(|s| s[1].ctl.current.is_some());
        assert!(!pair.seats[1].ctl.share_message(a.me(), &previous_problem), "a late failure cannot affect the next offer");
        assert_eq!(pair.seats[1].ctl.shown().share.single_waiting_line(), Some("Waiting for Peer's picture…"),
            "a new share has its normal waiting line");
        assert!(pair.seats[0].ctl.shown().share.offers[0].sharer_line.as_deref().unwrap().contains("hasn't opened it yet"),
            "a new share clears the sharer's stopped line");
        if side == 0 {
            let viewer = &mut pair.seats[1].ctl;
            viewer.take_share_picture_update();
            let problem = ShareProblem::new(session, "encoder could not start").encode();
            assert!(viewer.share_message(a.me(), &problem), "the first report changes an empty waiting panel");
            assert!(matches!(viewer.take_share_picture_update(), Some(None)), "a failure before the first frame redraws the panel");
            assert_eq!(viewer.shown().share.single_waiting_line(), Some("Peer's picture stopped: encoder could not start"));
            assert!(!viewer.share_message(a.me(), &problem), "a duplicate report does not redraw the panel again");
        }
        pair.seats[1].press(Press::CloseWatch);
        pair.seats[0].press(Press::Call);
        pair.wait(|s| s[1].ctl.view().ringing());
        pair.seats[side].press(ending);
        pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Idle && !s.ctl.has_call_workers()));
        assert!(pair.seats.iter().all(|s| s.ctl.sharing.shown.offers.first().unwrap().session == session));
        pair.seats[0].press(Press::StopShare);
        pair.wait(|s| s.iter().all(|s|
            s.ctl.view().phase == Phase::Idle
                && !s.ctl.has_call_workers()
                && s.ctl.sharing.shown.offer(session).is_none()
        ));
        for seat in &mut pair.seats {
            assert!(seat.ctl.sharing.shown.offers.is_empty());
            assert_eq!(seat.ctl.take_share_events().last(), Some(&ShareEvent::Ended { session, reason: ShareEndReason::Stopped }));
        }
        assert!(pair.seats[0].ctl.take_facts().iter().any(|f| matches!(f,
            Fact::Leave { session: id, why: LeaveWhy::Ended, .. } if *id == session)));
        assert!(!pair.seats[1].ctl.take_facts().iter().any(|f| matches!(f,
            Fact::ShareJoin { session: id, .. } | Fact::Leave { session: id, .. } if *id == session)),
            "an unseen share leaves no viewer participation at {ending:?}");
    }

    // Reverse the order of the two media on the same linked computers.
    for ending in [LeaveWhy::Ended, LeaveWhy::Dropped] {
        let previous_workers = networks.load(Ordering::Acquire);
        let (source, readings) = held_source();
        let installs = pair.seats.each_ref().map(|s| s.host.installs);
        pair.seats[0].ctl.start_window(LinkId::TEST, &window, "Second".into(), source).unwrap();
        let session = pair.seats[0].ctl.sharing.shown.offers.first().unwrap().session;
        // The follower starts its session when the keeper's Media arrives, which can be after the offer and the demand.
        pair.wait(|s| s[1].ctl.viewer_session() == Some(session));
        pair.seats[1].press(Press::Watch);
        pair.wait(|s| readings.lock().unwrap().demand == Some(View::default())
            && s.iter().all(|s| s.ctl.current.is_some()) && s[0].number() == s[1].number());
        let shares_only = networks.load(Ordering::Acquire);
        assert_eq!(shares_only, previous_workers + 2, "a fresh share starts one media session on each seat");
        assert_eq!(pair.seats.each_ref().map(|s| s.host.installs), installs);
        assert_eq!(readings.lock().unwrap().popped, 0);
        pair.seats[0].press(Press::Call);
        pair.wait(|s| s[1].ctl.view().ringing());
        pair.seats[1].press(Press::Answer);
        pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Live));
        pair.seats[1].ctl.share_panel_size(session, crate::call::ShareSurface::Page, 320, 180);
        pair.wait(|s| s[1].ctl.sharing.shown.picture.is_some());
        draw_panel(pair.seats[1].ctl.shown(), false);
        assert_eq!(networks.load(Ordering::Acquire), shares_only, "adding voice reused both network workers");
        if ending == LeaveWhy::Ended {
            pair.seats[1].press(Press::HangUp);
            pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Idle));
            assert!(pair.seats[1].ctl.sharing.shown.picture.is_some(), "the viewer keeps the picture after hanging up");
            assert_eq!(networks.load(Ordering::Acquire), shares_only);
            pair.seats[0].press(Press::StopShare);
            pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Idle && !s.ctl.has_call_workers()));
            assert_eq!(readings.lock().unwrap().drops, 1, "explicit Stop ends capture");
        } else {
            for s in &mut pair.seats { s.ctl.link_down_all(&mut s.host); }
        }
        for s in &mut pair.seats {
            assert!(s.ctl.sharing.shown.offers.is_empty() && s.ctl.sharing.shown.picture.is_none());
            let reason = if ending == LeaveWhy::Dropped { ShareEndReason::LinkDropped } else { ShareEndReason::Stopped };
            assert_eq!(s.ctl.take_share_events().last(), Some(&ShareEvent::Ended { session, reason }));
            assert!(s.ctl.take_facts().iter().any(|fact| matches!(fact,
                Fact::Leave { session: id, secs: Some(_), why } if *id == session && *why == ending)));
        }
    }
}

#[test]
fn the_sharer_line_says_who_has_not_opened_it_and_why_nothing_goes_out() {
    let secs = std::time::Duration::from_secs;
    assert_eq!(sharer_line("Fab", "Ben", None, 0, None), "Sharing Fab with Ben. Ben hasn't opened it yet.",
        "nothing is captured before the viewer opens the share, and the row says why");
    assert_eq!(sharer_line("Fab", "Ben", Some(secs(1)), 0, None), "Sharing Fab with Ben.",
        "a picture still on its way is not a stall");
    assert_eq!(sharer_line("Fab", "Ben", Some(NO_PICTURE_AFTER), 0, None),
        "Sharing Fab with Ben. Town asked Fab for a picture when Ben opened it, and none has come back in 3 s.",
        "a watched share with no captured picture says so once the wait is over");
    assert_eq!(sharer_line("Fab", "Ben", Some(secs(60)), 1, None), "Sharing Fab with Ben.",
        "one captured picture is enough: a still app sends no more until it changes");
    assert_eq!(sharer_line("Fab", "Ben", None, 0, Some("window capture ended")),
        "Sharing Fab with Ben. Fab's picture stopped: window capture ended",
        "an error that ended the app's pictures is shown in its own words");
}

#[test]
fn share_opening_controls_require_local_input_in_both_audio_modes() {
    for voice in [VoiceSource::Feed, VoiceSource::Synthetic { tone_hz: 700. }] {
        let mut ctl = CallController::default();
        let mut host = Host::default();
        ctl.configure(CallEnv { files: Default::default(), signal: Arc::new(|| {}), voice, devices_path: PathBuf::new(),
            spawn: makepad_call::driver::std_spawn(), jobs: makepad_call::driver::std_spawn() });
        let port = ctl.link_up(&mut host, [1; 65], [2; 65], "Peer").unwrap();
        ctl.set_open_town(Some(([9; 32], vec![([2; 65], "Peer".into())])));
        ctl.set_peer_caps([2; 65], MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        for press in [Press::Share(ShareChoice::Menu), Press::Share(ShareChoice::Row(8)), Press::Watch, Press::LetControl([2; 65]), Press::AllowAll(true)] {
            let action = CallAction::Press(press);
            assert!(ctl.opening(action));
            assert!(!ctl.call_actions(&mut host, action), "queued or injected {press:?} is ignored");
        }
        for press in [Press::StopShare, Press::CloseWatch, Press::TakeBack, Press::ReleaseControl, Press::AllowAll(false), Press::RevokeControl([2; 65])] {
            assert!(!ctl.opening(CallAction::Press(press)));
        }
        assert!(ctl.sharing.shown.choices.is_none());
        assert!(ctl.call_opening_presses(&mut host, CallAction::Press(Press::Share(ShareChoice::Menu))));
        assert!(ctl.sharing.shown.choices.is_some());
        let offer = Message::Share { session: [4; 16], title: "Sheet".into(), w: 96., h: 64., ext: super::super::super::ShareOfferExt { town: Some([9; 32]), ..Default::default() } };
        port.messages_in.try_send(offer.encode()).unwrap();
        ctl.poll(&mut host);
        assert!(!ctl.call_actions(&mut host, CallAction::Press(Press::Watch)));
        assert!(ctl.sharing.shown.watching.is_none());
        assert!(ctl.call_opening_presses(&mut host, CallAction::Press(Press::Watch)));
        assert!(ctl.call_actions(&mut host, CallAction::Press(Press::CloseWatch)));
        assert!(ctl.sharing.shown.watching.is_none());
        assert_eq!(host.installs, 0, "sharing and watching never open voice");
        ctl.link_down(&mut host, [2; 65]);
        assert_eq!(ctl.take_share_events().last(), Some(&ShareEvent::Ended { session: [4; 16], reason: ShareEndReason::LinkDropped }));
        assert!(ctl.sharing.shown.offers.is_empty());
        assert!(ctl.take_facts().is_empty(), "a viewer who closed before any picture has no Join or Leave");
        let port = ctl.link_up(&mut host, [1; 65], [2; 65], "Peer").unwrap();
        ctl.set_open_town(Some(([9; 32], vec![([2; 65], "Peer".into())])));
        ctl.set_peer_caps([2; 65], MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        port.messages_in.try_send(offer.encode()).unwrap();
        ctl.poll(&mut host);
        assert_eq!(ctl.viewer_session(), Some([4; 16]), "an idle link accepts a new share");
        assert!(ctl.current.is_none(), "the offer waits for keys before starting media");
    }
}

#[cfg(unix)]
#[test]
fn share_end_preserves_control_notice_across_split_polls_and_clears_earlier_notices() {
    use crate::call::{signal, topic};
    use makepad_call::driver::std_spawn;

    const ANA: PublicKey = [1; 65];
    const BEN: PublicKey = [2; 65];
    const KIT: PublicKey = [3; 65];

    struct DirectSeat<'a> {
        ctl: CallController,
        host: Host,
        port: MediaPort,
        apps: &'a mut AppLinks,
    }

    impl<'a> DirectSeat<'a> {
        fn new(me: PublicKey, peer: PublicKey, name: &str, apps: &'a mut AppLinks) -> Self {
            let mut ctl = CallController::default();
            ctl.codecs = Arc::new(TestCodecs);
            ctl.configure(CallEnv {
                files: Default::default(), devices_path: Default::default(), voice: VoiceSource::Off,
                spawn: std_spawn(), jobs: std_spawn(), signal: Arc::new(|| {}),
            });
            let mut host = Host::default();
            let port = ctl.link_up(&mut host, me, peer, name).unwrap();
            ctl.set_open_town(Some((MESH_TOWN, vec![(peer, name.into())])));
            ctl.set_peer_caps(peer, SHARE_CAPS);
            Self { ctl, host, port, apps }
        }

        fn poll(&mut self) {
            self.ctl.poll(&mut self.host);
            self.ctl.receive_control(self.apps);
            self.ctl.revoke_ended_shares(self.apps);
        }
    }

    fn deliver_one_by_one(from: &MediaPort, to: &mut DirectSeat<'_>) -> Vec<Vec<u8>> {
        let mut delivered = Vec::new();
        for frame in from.messages_out.try_iter() {
            to.port.messages_in.try_send(frame.clone()).unwrap();
            to.poll();
            delivered.push(frame);
        }
        delivered
    }

    fn exchange(a: &mut DirectSeat<'_>, b: &mut DirectSeat<'_>) {
        loop {
            a.poll();
            b.poll();
            let outbound = deliver_one_by_one(&a.port, b);
            let inbound = deliver_one_by_one(&b.port, a);
            if outbound.is_empty() && inbound.is_empty() { break }
        }
    }

    fn open_and_grant(a: &mut DirectSeat<'_>, b: &mut DirectSeat<'_>, link: LinkId, window: WindowId) -> SessionId {
        a.ctl.open_share_window(a.apps, link, window).unwrap();
        let session = a.ctl.only_local_offer().unwrap();
        exchange(a, b);
        assert!(b.ctl.call_opening_presses(&mut b.host, CallAction::OnShare(session, Press::Watch)),
            "the viewer opens the new offer");
        b.ctl.share_panel_size(session, ShareSurface::Page, 320, 180);
        assert!(b.ctl.ask_control(session, true), "the viewer asks for control on this share");
        exchange(a, b);
        assert!(a.ctl.allow_control(a.apps, session, BEN, "Ben"), "the sharer grants the watching viewer control");
        exchange(a, b);
        let grant = a.ctl.shown().share.control.active.expect("the sharer holds a current grant");
        assert_eq!(b.ctl.shown().share.control.active, Some(grant), "the viewer receives that grant before the end");
        session
    }

    fn deliver_end(a: &mut DirectSeat<'_>, b: &mut DirectSeat<'_>, session: SessionId) {
        a.poll();
        let frames = deliver_one_by_one(&a.port, b);
        let ends = frames.iter().filter(|f| matches!(signal::decode(f),
            Ok(Some(Message::ShareOff { session: id, reason: ShareEndReason::Stopped })) if id == session)).count();
        assert_eq!(ends, 1, "the viewer gets exactly one ShareOff for the ended offer");
        assert!(!frames.iter().any(|f| signal::peer_payload(f).is_some_and(|(t, _, _)| t == topic::INPUT)),
            "ending the share sends no redundant control message");
        assert!(b.ctl.sharing.shown.offer(session).is_none(), "the viewer processes ShareOff");
        assert_eq!(b.ctl.inputs_refused(), 0, "separate polls leave no stale input to refuse");
        exchange(a, b);
    }

    let mut rig = crate::app_link::RecordingRig::new();
    rig.app.scan();
    rig.wait(|r| !r.app.apps().is_empty());
    let link = rig.app.link(&rig.app.apps()[0].key.clone());
    rig.live(link);
    let window = rig.app.state(link).unwrap().windows[0].id;
    let mut guest_apps = AppLinks::default();
    let mut a = DirectSeat::new(ANA, BEN, "Ben", &mut rig.app);
    let mut b = DirectSeat::new(BEN, ANA, "Ana", &mut guest_apps);

    let session = open_and_grant(&mut a, &mut b, link, window);
    assert!(a.ctl.press(&mut a.host, Press::StopShare), "StopShare ends the controlled share");
    deliver_end(&mut a, &mut b, session);
    for seat in [&a, &b] {
        assert_eq!(seat.ctl.shown().share.control.notice.as_deref(), Some("Control ended with the share."),
            "both seats keep the notice when stopping the share cuts control short");
    }

    let session = open_and_grant(&mut a, &mut b, link, window);
    assert!(a.ctl.press(&mut a.host, Press::TakeBack), "the sharer takes back control before stopping");
    exchange(&mut a, &mut b);
    assert_eq!(b.ctl.shown().share.control.notice.as_deref(), Some("Ana took back control."),
        "the viewer sees the earlier take-back before the share ends");
    assert!(a.ctl.press(&mut a.host, Press::StopShare), "StopShare ends the previously controlled share");
    deliver_end(&mut a, &mut b, session);
    for seat in [&a, &b] {
        let share = seat.ctl.shown().share;
        assert!(share.control.notice.is_none() && share.control_session.is_none(),
            "the earlier take-back notice and its session clear on both seats");
    }

    let _other_port = a.ctl.link_up(&mut a.host, ANA, KIT, "Kit").unwrap();
    a.ctl.set_open_town(Some((MESH_TOWN, vec![(BEN, "Ben".into()), (KIT, "Kit".into())])));
    a.ctl.set_peer_caps(KIT, SHARE_CAPS);
    let session = open_and_grant(&mut a, &mut b, link, window);
    a.ctl.set_town_peers(&mut a.host, BTreeMap::from([(MESH_TOWN, BTreeSet::from([KIT]))]));
    deliver_end(&mut a, &mut b, session);
    assert!(a.ctl.is_recipient(session, &KIT), "the share survives for the remaining recipient");
    for seat in [&a, &b] {
        assert_eq!(seat.ctl.shown().share.control.notice.as_deref(), Some("Control ended with the share."),
            "revoking the holder's membership preserves the share-end notice on both seats");
    }
    for seat in [&mut a, &mut b] {
        seat.ctl.shutdown(&mut seat.host);
        while seat.ctl.has_call_workers() { seat.poll(); thread::yield_now(); }
    }
}

#[cfg(unix)]
#[test]
fn linked_takeover_binds_input_to_the_live_share_and_its_current_allow() {
    use crate::app_link::{AppInput, Mods, RecordingRig};
    use crate::call::{control::{ControlMessage, FrameMotion, GrantId, PanelInput}, topic};
    use super::super::takeover::Refusal;
    use makepad_widgets::KeyCode;

    fn wait(pair: &mut Pair, app: &mut RecordingRig, mut ready: impl FnMut(&mut [Seat; 2], &RecordingRig) -> bool) {
        let mut viewer_apps = AppLinks::default();
        pair.wait(|seats| {
            let events = app.app.poll();
            seats[0].ctl.share_app_events(&mut app.app, &events);
            seats[0].ctl.receive_control(&mut app.app);
            seats[1].ctl.receive_control(&mut viewer_apps);
            ready(seats, app)
        });
    }

    // A marker follows all earlier reliable input. Waiting for it and the local
    // app's reply barrier also completes when a deliberately bad input was refused.
    fn drain(pair: &mut Pair, app: &mut RecordingRig, link: LinkId) {
        pair.seats[1].ctl.send_peer_to(pair.seats[1].peer(), topic::PROBE, Vec::new()).unwrap();
        wait(pair, app, |s, _| !s[0].ctl.take_peer_with_sender(topic::PROBE).is_empty());
        app.barrier(link);
    }

    fn watch(pair: &mut Pair, app: &mut RecordingRig, link: LinkId, allow_all: bool) -> SessionId {
        let window = app.app.state(link).unwrap().windows[0].id;
        pair.seats[0].ctl.open_share_window(&mut app.app, link, window).unwrap();
        let session = pair.seats[0].ctl.shown().share.offers.first().unwrap().clone().session;
        let line = pair.seats[0].ctl.shown().share.offers[0].sharer_line.clone().unwrap();
        assert!(line.starts_with("Sharing ") && line.ends_with("hasn't opened it yet."),
            "until the viewer subscribes, the sharer's row says nobody has opened the share: {line}");
        pair.wait(|s| s[1].ctl.viewer_session() == Some(session));
        let sharer = &mut pair.seats[0];
        let offer = sharer.ctl.sharing.shown.offer(session).unwrap();
        assert!(!offer.allow_all() && offer.allowed().is_empty(), "each share starts private");
        if allow_all {
            assert!(sharer.ctl.allow_all_control(&mut app.app, session));
            assert!(!app.app.state(link).unwrap().input_allowed, "an unwatched share has no spell");
        }
        pair.seats[1].press(Press::Watch);
        pair.seats[1].ctl.share_panel_size(session, crate::call::ShareSurface::Page, 24, 16);
        wait(pair, app, |s, _| s[0].ctl.sharing.shown.watchers.len() == 1);
        let line = pair.seats[0].ctl.shown().share.offers[0].sharer_line.clone().unwrap();
        assert!(!line.contains("hasn't opened"), "the viewer's subscription clears the not-opened line: {line}");
        session
    }

    fn grant(pair: &mut Pair, app: &mut RecordingRig, session: SessionId) -> GrantId {
        let name = pair.seats[0].ctl.view().peer_name.clone();
        assert!(pair.seats[0].ctl.allow_control(&mut app.app, session, pair.seats[0].peer(), &name));
        let id = pair.seats[0].ctl.sharing.shown.control.active.unwrap();
        wait(pair, app, |s, _| s[1].ctl.sharing.shown.control.active == Some(id));
        id
    }

    let (_homes, a, b) = crate::model::tests::shared_town("takeover");
    let (mut pair, _) = linked_call(&a, &b);
    let mut app = RecordingRig::new();
    app.app.scan();
    app.wait(|r| !r.app.apps().is_empty());
    let link = app.app.link(&app.app.apps()[0].key.clone());
    app.live(link);
    let session = watch(&mut pair, &mut app, link, false);
    let mods = Mods::default();
    let moved = |x| AppInput::PointerMove { x, y: 4. };
    let down = |x| AppInput::PointerDown { x, y: 4., button: 0, mods };
    let up = |x| AppInput::PointerUp { x, y: 4., button: 0, mods };
    let key = || AppInput::KeyDown { key: KeyCode::KeyA, mods };
    let send = |pair: &mut Pair, session, grant, event| {
        pair.seats[1].ctl.send_peer_to(pair.seats[1].peer(), topic::INPUT, ControlMessage::Input { session, grant, event }.encode()).unwrap();
    };
    let seat = &mut pair.seats[0];
    assert!(!seat.ctl.call_actions(&mut seat.host, CallAction::Press(Press::LetControl(b.me()))),
        "an injected or queued Let control cannot grant access");
    assert!(!app.app.state(link).unwrap().input_allowed);
    for event in [moved(2.), key()] { send(&mut pair, session, [0; 16], event); }
    drain(&mut pair, &mut app, link);
    assert_eq!(pair.seats[0].ctl.inputs_refused(), 2);
    assert!(app.input_lines().is_empty(), "pointer and keys before Allow stay out of the app");
    for asking in [true, false, true] {
        assert!(pair.seats[1].ctl.ask_control(session, asking));
        wait(&mut pair, &mut app, |s, _| s[0].ctl.sharing.shown.control.requests.first().map(|r| r.0) == asking.then_some(session));
        assert_eq!(pair.seats[1].ctl.sharing.shown.control.asking, asking.then_some(session));
        assert!(!app.app.state(link).unwrap().input_allowed, "only Allow opens app input");
    }
    let sharer = &mut pair.seats[0];
    for press in [Press::LetControl(b.me()), Press::AllowAll(true)] {
        assert!(!sharer.ctl.call_actions(&mut sharer.host, CallAction::OnShare(session, press)),
            "an injected {press:?} cannot open the watched share even with a pending request");
    }
    let offer = sharer.ctl.sharing.shown.offer(session).unwrap();
    assert!(!offer.allow_all() && offer.allowed().is_empty() && !app.app.state(link).unwrap().input_allowed,
        "refused presses leave the switch, named rights and app input closed");
    let requested = grant(&mut pair, &mut app, session);
    let peer = pair.seats[0].peer();
    assert_eq!(pair.seats[0].ctl.sharing.shown.offer(session).unwrap().allowed().get(&peer),
        Some(&pair.seats[0].ctl.view().peer_name), "Allow records the authenticated peer's name for the chip");
    assert!(pair.seats[0].ctl.sharing.shown.control.requests.is_empty());
    assert!(pair.seats[1].ctl.sharing.shown.control.asking.is_none());
    assert_eq!(pair.seats[1].ctl.sharing.shown.control.active, Some(requested));
    pair.seats[1].press(Press::ReleaseControl);
    wait(&mut pair, &mut app, |s, app| s[0].ctl.sharing.shown.control.active.is_none()
        && !app.app.state(link).unwrap().input_allowed);
    assert_eq!(pair.seats[1].ctl.sharing.shown.control.active, None);
    assert_eq!(pair.seats[1].ctl.sharing.shown.control.allowed, Some(session), "Hand back keeps the right");
    assert!(pair.seats[1].ctl.ask_control(session, true));
    wait(&mut pair, &mut app, |s, _| s[1].ctl.sharing.shown.control.active.is_some());
    let first = pair.seats[1].ctl.sharing.shown.control.active.unwrap();
    assert_ne!(requested, first, "Take control starts a fresh spell without a sharer press");
    send(&mut pair, [0; 16], first, key());
    pair.seats[1].ctl.send_peer_to(pair.seats[1].peer(), topic::INPUT, vec![1, 1, 0]).unwrap();
    drain(&mut pair, &mut app, link);
    assert_eq!(pair.seats[0].ctl.inputs_refused(), 4);
    assert_eq!((pair.seats[0].ctl.inputs_refused_for(Refusal::NoAllow), pair.seats[0].ctl.inputs_refused_for(Refusal::Unreadable)), (3, 1),
        "input before the Allow or for another share is refused as no current Allow; bad bytes as unreadable");
    assert_eq!(app.app.inputs_refused(link), 0, "malformed and foreign messages never reached G11");

    let mut motion = FrameMotion::default();
    let mut emit = |i| pair.seats[1].ctl.panel_input(&i);
    for event in [moved(1.), moved(2.), down(3.), moved(-2.), moved(-4.), up(-5.), key(),
        AppInput::KeyUp { key: KeyCode::KeyA, mods }] {
        motion.push(PanelInput { session, grant: first, event }, &mut emit);
    }
    motion.flush(emit);
    for i in 0..94 {
        pair.seats[1].ctl.panel_input(&PanelInput { session, grant: first, event: AppInput::Text(i.to_string()) });
    }
    drain(&mut pair, &mut app, link);
    assert_eq!(app.app.inputs_delivered(link), 100);
    pair.seats[0].ctl.receive_control(&mut app.app);
    let (viewer, sharer) = (pair.seats[1].ctl.takeover.input.summary(), pair.seats[0].ctl.takeover.input.summary());
    assert!(viewer.starts_with("control input sent 100, ") && viewer.ends_with("refused 0, undelivered 0, last press 3,4"),
        "the viewer counts what it sent and its last press: {viewer}");
    assert!(sharer.starts_with("control input sent 0, received 103, handed 100, applied 100, app refused 0, refused 4 (no current Allow 3, unreadable 1)")
        && sharer.ends_with("last press 3,4"), "the sharer counts every input it received, handed over and its app applied: {sharer}");
    let lines: Vec<_> = app.input_lines().iter().map(|s| makepad_remote::Req::parse(s)).collect();
    assert_eq!(&lines[..6].iter().map(|r| (r.cmd.as_str(), r.get("k").unwrap())).collect::<Vec<_>>(),
        &[("mouse","move"), ("mouse","down"), ("mouse","move"), ("mouse","up"), ("key","down"), ("key","up")]);
    assert_eq!((lines[0].get("x"), lines[1].get("x")), (Some("2"), Some("3")), "press kept its place in the burst");
    assert!(lines.iter().all(|r| r.get("if_user_seq") == Some("0") && r.get("w") == Some("3")));
    for (i, text) in lines[6..].iter().enumerate() { assert_eq!(text.get("t").unwrap(), i.to_string()); }
    assert!(app.app.state(link).unwrap().input_allowed, "a hundred applied inputs keep the Allow");

    pair.seats[0].press(Press::TakeBack);
    pair.seats[0].ctl.revoke_ended_shares(&mut app.app);
    assert!(!app.app.state(link).unwrap().input_allowed, "Take back closes admission on this input");
    wait(&mut pair, &mut app, |s, _| s[1].ctl.sharing.shown.control.active.is_none());
    assert!(pair.seats.iter().all(|s| s.ctl.shown().share.control.notice.unwrap().contains("took back")));
    send(&mut pair, session, first, key());
    drain(&mut pair, &mut app, link);
    assert_eq!(pair.seats[0].ctl.inputs_refused(), 5);

    let second = grant(&mut pair, &mut app, session);
    assert_ne!(second, first, "regrant on the same share creates a fresh capability");
    send(&mut pair, session, first, key());
    pair.seats[1].ctl.send_peer_to(pair.seats[1].peer(), topic::INPUT, ControlMessage::Release { session, grant: first }.encode()).unwrap();
    drain(&mut pair, &mut app, link);
    assert_eq!(pair.seats[0].ctl.inputs_refused(), 7);
    assert!(app.app.state(link).unwrap().input_allowed, "a delayed Release cannot end the new grant");
    send(&mut pair, session, second, AppInput::Text("current".into()));
    drain(&mut pair, &mut app, link);
    assert_eq!(app.app.inputs_delivered(link), 101);
    app.local_input();
    wait(&mut pair, &mut app, |s, _| s.iter().all(|s| s.ctl.sharing.shown.control.active.is_none()));
    assert!(pair.seats.iter().all(|s| s.ctl.shown().share.control.notice.unwrap().starts_with("Paused:")));
    send(&mut pair, session, second, key());
    drain(&mut pair, &mut app, link);
    assert_eq!(pair.seats[0].ctl.inputs_refused(), 8);
    let third = grant(&mut pair, &mut app, session);
    pair.seats[0].press(Press::StopShare);
    wait(&mut pair, &mut app, |s, _| s[1].ctl.viewer_session().is_none());
    for seat in &mut pair.seats {
        assert_eq!(seat.ctl.take_share_events().last(), Some(&ShareEvent::Ended { session, reason: ShareEndReason::Stopped }));
    }
    assert!(pair.seats.iter().all(|s| s.ctl.shown().share.control.notice.as_deref() == Some("Control ended with the share.")));
    send(&mut pair, session, third, key());
    drain(&mut pair, &mut app, link);
    assert_eq!(pair.seats[0].ctl.inputs_refused(), 9);
    let reasons = [Refusal::NoAllow, Refusal::Stale, Refusal::NoShare, Refusal::Unreadable].map(|r| pair.seats[0].ctl.inputs_refused_for(r));
    assert_eq!(reasons, [6, 1, 1, 1], "every refusal has its reason: a revoked or replaced Allow, a late Release, the ended share, bad bytes");
    pair.seats[0].press(Press::HangUp);
    pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Idle));
    let next = watch(&mut pair, &mut app, link, true);
    wait(&mut pair, &mut app, |s, _| s[1].ctl.sharing.shown.control.active.is_some());
    let viewer = &mut pair.seats[1].ctl;
    assert_eq!(viewer.sharing.shown.control.allowed, Some(next), "a watcher outside a call learns its switch-given right");
    assert_eq!((viewer.control_rights_revision(next), viewer.control_rights(next)), (None, None),
        "only the sharer's call supplies tweak rights");
    let grant_id = viewer.sharing.shown.control.active.unwrap();
    viewer.panel_input(&PanelInput { session: next, grant: grant_id, event: AppInput::Text("watch grant".into()) });
    drain(&mut pair, &mut app, link);
    assert_eq!(app.app.inputs_delivered(link), 102, "watching with the switch on grants working app input without asking");
    let sharer = &mut pair.seats[0];
    assert!(sharer.ctl.sharing.shown.offer(next).unwrap().allowed().is_empty(), "the switch adds no chip");
    assert!(sharer.ctl.call_actions(&mut sharer.host, CallAction::OnShare(next, Press::AllowAll(false))));
    wait(&mut pair, &mut app, |s, _| s[1].ctl.sharing.shown.control.active.is_none());
    assert!(pair.seats[1].ctl.sharing.shown.control.notice.as_deref().unwrap().contains("took back"), "switch off ends the spell with TakenBack");
    assert_eq!(pair.seats[1].ctl.sharing.shown.control.allowed, None, "switch off withdraws a switch-only right");
    assert!(pair.seats[1].ctl.ask_control(next, true));
    wait(&mut pair, &mut app, |s, _| s[0].ctl.sharing.shown.control.requests.first().map(|r| r.0) == Some(next));
    let revision = pair.seats[0].ctl.control_rights_revision(next).unwrap();
    let check_rights = |call: &CallController, changes: u64, all, seats: &[PublicKey]| {
        let expected = crate::tweak::ControlRights { all, seats: seats.iter().copied().collect() };
        assert_eq!(call.control_rights(next), Some(expected));
        assert_eq!(call.control_rights_revision(next), Some(revision + changes));
    };
    check_rights(&pair.seats[0].ctl, 0, false, &[]);
    let named = grant(&mut pair, &mut app, next);
    let sharer = &mut pair.seats[0];
    check_rights(&sharer.ctl, 1, false, &[peer]);
    let revoke = {
        use makepad_widgets::*;
        let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, {use mod.prelude.widgets.* use mod.widgets.* View{live_share := LiveSharePage{}}});
            WidgetRef::script_from_value(vm, root)
        });
        let mut data = crate::lists::ViewData::default();
        data.page = crate::lists::Page::Live(crate::lists::Live::Share(next));
        data.call = sharer.ctl.shown();
        data.sync_live_share(&mut cx, &ui);
        let switch = ui.widget(&cx, ids!(live_share.allow_all));
        let chips = ui.flat_list(&cx, ids!(live_share.chips));
        let chip = chips.borrow().unwrap().items.values().next().expect("Allow shows the named chip").widget.clone();
        assert_eq!(chip.label(&cx, ids!(name)).text(), sharer.ctl.view().peer_name);
        let mut activate = |cx: &mut Cx, widget: &WidgetRef, action| {
            let event = Event::A11yAction(a11y_tree::ActionRequest {req: 0, node: a11y_tree::NodeId::widget(widget.widget_uid().0),
                action, arg: a11y_tree::ActionArg::None});
            let actions = cx.capture_actions(|cx| ui.handle_event(cx, &event, &mut Scope::with_data(&mut data)));
            actions.iter().filter_map(|a| a.as_widget_action()).filter(|w| w.widget_uid == widget.widget_uid())
                .find_map(|w| w.action.downcast_ref::<CallAction>().copied()).expect("the control emits its own scoped action")
        };
        let on = activate(&mut cx, &switch, a11y_tree::Actions::TOGGLE);
        assert_eq!(on, CallAction::OnShare(next, Press::AllowAll(true)));
        assert!(crate::call::opening_uids(&cx, &ui).contains(&switch.widget_uid()), "switch on requires its local release");
        assert!(!sharer.ctl.call_actions(&mut sharer.host, on) && !switch.as_check_box().active(&cx),
            "an unconsented switch stays off in the model and on screen");
        let remove = chip.widget(&cx, ids!(remove));
        activate(&mut cx, &remove, a11y_tree::Actions::PRESS)
    };
    assert_eq!(revoke, CallAction::OnShare(next, Press::RevokeControl(peer)), "the × names this share and peer");
    assert!(sharer.ctl.allow_all_control(&mut app.app, next));
    check_rights(&sharer.ctl, 2, true, &[peer]);
    assert!(!sharer.ctl.allow_all_control(&mut app.app, next), "repeating Allow all leaves the right unchanged");
    check_rights(&sharer.ctl, 2, true, &[peer]);
    assert!(sharer.ctl.call_actions(&mut sharer.host, CallAction::OnShare(next, Press::AllowAll(false))));
    check_rights(&sharer.ctl, 3, false, &[peer]);
    assert!(sharer.ctl.sharing.shown.offer(next).unwrap().allowed().contains_key(&peer), "switch off keeps named rights");
    assert_eq!(sharer.ctl.sharing.shown.control.active, Some(named), "a still-authorized spell continues");
    assert!(sharer.ctl.call_actions(&mut sharer.host, revoke));
    check_rights(&sharer.ctl, 4, false, &[]);
    assert!(sharer.ctl.sharing.shown.control.active.is_none() && sharer.ctl.sharing.shown.control.holder.is_none(),
        "revoking the current holder removes the right and ends the grant in the same call");
    wait(&mut pair, &mut app, |s, app| s[1].ctl.sharing.shown.control.active.is_none() && !app.app.state(link).unwrap().input_allowed);
    assert_eq!(pair.seats[1].ctl.sharing.shown.control.allowed, None, "removing the chip withdraws its right");
    assert!(pair.seats[1].ctl.sharing.shown.control.notice.as_deref().unwrap().contains("took back"), "removing the chip ends its spell with TakenBack");
    assert!(pair.seats[1].ctl.ask_control(next, true));
    wait(&mut pair, &mut app, |s, _| s[0].ctl.sharing.shown.control.requests.first().map(|r| r.0) == Some(next));
    assert!(pair.seats[0].ctl.allow_all_control(&mut app.app, next));
    wait(&mut pair, &mut app, |s, _| s[1].ctl.sharing.shown.control.active.is_some());
    for seat in &mut pair.seats {
        seat.ctl.link_down_all(&mut seat.host);
        assert_eq!(seat.ctl.shown().share.control.notice.as_deref(), Some("Control ended when the link disconnected."));
    }
    pair.seats[0].ctl.revoke_ended_shares(&mut app.app);
    assert!(!app.app.state(link).unwrap().input_allowed);
    app.barrier(link);
    assert_eq!(app.input_lines().len(), 102, "only current-grant input reached the app, including the watch-start grant");

    assert_eq!((lines[2].get("x"), lines[3].get("x")), (Some("-4"), Some("-5")),
        "the shared app receives a captured drag and its release at their actual outside points");
}


fn run_call_tool(model: &crate::model::Model, seat: &mut Seat, tool: crate::call::ai::CallTool, page: Option<SessionId>)
    -> (makepad_strict_json::Value, &'static str, bool) {
    use crate::call::ai::{self, CallGateContext, CallTool, CallToolPlan};
    use makepad_town_core::trust::{Gate, LocalPolicy};

    let gate = Gate::new(&model.state, LocalPolicy::Members);
    let peer = seat.peer();
    let plan = tool.plan(&gate, &seat.ctl.shown(), page, &model.me()).unwrap();
    let (msg, note, changed) = match plan {
        CallToolPlan::AmbiguousShareStop(msg) => (Some(msg), "multiple shares running", false),
        CallToolPlan::Action(None) if tool == CallTool::State => (None, "call state", false),
        CallToolPlan::Action(action) => {
            let changed = action.is_some_and(|a| seat.ctl.call_actions(&mut seat.host, a));
            (None, if changed { "closed" } else { "nothing to close" }, changed)
        }
    };
    let ctx = CallGateContext { call_peers: &[peer], peers: None, me: model.me(), person_name: model.person_name.as_deref() };
    let out = ai::state(&gate, &seat.ctl.shown(), Instant::now(), &ctx, msg).unwrap();
    (makepad_strict_json::parse(out.as_str().as_bytes()).unwrap(), note, changed)
}

/// The AI's call tools on a linked pair: `call_state` reports the share and its Allow from both
/// seats, and each closing tool ends what its button ends, once.
#[cfg(unix)]
#[test]
fn call_tools_read_and_close_a_linked_share() {
    use crate::app_link::RecordingRig;
    use crate::call::ai::CallTool;
    use makepad_strict_json::Value;

    let (_homes, a, b) = crate::model::tests::shared_town("call-tools");
    let (mut pair, _) = linked_call(&a, &b);
    let mut app = RecordingRig::new();
    app.app.scan();
    app.wait(|r| !r.app.apps().is_empty());
    let link = app.app.link(&app.app.apps()[0].key.clone());
    app.live(link);
    let window = app.app.state(link).unwrap().windows[0].id;
    pair.seats[0].ctl.open_share_window(&mut app.app, link, window).unwrap();
    let session = pair.seats[0].ctl.shown().share.offers.first().unwrap().clone().session;
    pair.wait(|s| s[1].ctl.viewer_session() == Some(session));
    pair.seats[1].press(Press::Watch);
    pair.seats[1].ctl.share_panel_size(session, crate::call::ShareSurface::Page, 24, 16);
    let mut viewer_apps = AppLinks::default();
    let mut wait = |pair: &mut Pair, app: &mut RecordingRig, mut ready: Box<dyn FnMut(&mut [Seat; 2]) -> bool>| pair.wait(|seats| {
        let events = app.app.poll();
        seats[0].ctl.share_app_events(&mut app.app, &events);
        seats[0].ctl.receive_control(&mut app.app);
        seats[1].ctl.receive_control(&mut viewer_apps);
        ready(seats)
    });
    wait(&mut pair, &mut app, Box::new(|s| s[0].ctl.sharing.shown.watchers.len() == 1));
    assert!(pair.seats[0].ctl.allow_control(&mut app.app, session, pair.seats[0].peer(), "Peer"));
    let grant = pair.seats[0].ctl.sharing.shown.control.active.unwrap();
    wait(&mut pair, &mut app, Box::new(move |s| s[1].ctl.sharing.shown.control.active == Some(grant)));

    let share_field = |v: &Value, key| v.get("share").and_then(|s| s.get(key)).and_then(Value::as_str).map(str::to_owned);
    let (sharer, done, changed) = run_call_tool(&a, &mut pair.seats[0], CallTool::State, None);
    assert_eq!((done, changed), ("call state", false), "call_state reads only");
    assert_eq!(sharer.get("phase").and_then(Value::as_str), Some("live"));
    assert_eq!((share_field(&sharer, "state").as_deref(), share_field(&sharer, "controlled_by").as_deref()),
        (Some("sharing"), Some("Peer")), "the sharer sees its share and the peer holding the Allow");
    let (viewer, ..) = run_call_tool(&b, &mut pair.seats[1], CallTool::State, None);
    assert_eq!((share_field(&viewer, "state").as_deref(), share_field(&viewer, "controlled_by").as_deref()),
        (Some("viewing"), Some("you")), "the viewer holding the Allow is told it is in control");

    let (after, done, changed) = run_call_tool(&a, &mut pair.seats[0], CallTool::ControlTakeBack, None);
    assert_eq!((done, changed), ("closed", true), "control_take_back ends the Allow");
    assert!(after.get("share").and_then(|s| s.get("controlled_by")).is_some_and(|v| *v == Value::Null),
        "the answer is call_state after the take-back");
    pair.seats[0].ctl.revoke_ended_shares(&mut app.app);
    assert!(!app.app.state(link).unwrap().input_allowed, "the tool closes admission like Take back");
    wait(&mut pair, &mut app, Box::new(|s| s[1].ctl.sharing.shown.control.active.is_none()));
    assert!(pair.seats.iter().all(|s| s.ctl.shown().share.control.notice.unwrap().contains("took back")),
        "both seats read the button's own notice");
    assert_eq!(run_call_tool(&a, &mut pair.seats[0], CallTool::ControlTakeBack, None).1, "nothing to close", "a second take-back has nothing to end");

    let (_, done, _) = run_call_tool(&a, &mut pair.seats[0], CallTool::ShareStop, None);
    assert_eq!(done, "closed", "share_stop stops the share");
    wait(&mut pair, &mut app, Box::new(|s| s[1].ctl.viewer_session().is_none()));
    for seat in &mut pair.seats {
        assert_eq!(seat.ctl.take_share_events().last(), Some(&ShareEvent::Ended { session, reason: ShareEndReason::Stopped }),
            "share_stop ends the share the way Stop does");
    }
    assert!(pair.seats.iter().all(|s| s.ctl.shown().share.control.notice.is_none()),
        "the take-back's line goes with the share it was about");
    assert_eq!(run_call_tool(&b, &mut pair.seats[1], CallTool::ShareClose, None).1, "nothing to close", "no share left to close on the viewer");

    let (after, done, _) = run_call_tool(&a, &mut pair.seats[0], CallTool::HangUp, None);
    assert_eq!(done, "closed", "call_hang_up ends the call");
    assert_eq!(after.get("phase").and_then(Value::as_str), Some("idle"), "the answer is the idle call");
    pair.wait(|s| s[1].ctl.view().phase == Phase::Idle);
    assert_eq!(run_call_tool(&b, &mut pair.seats[1], CallTool::HangUp, None).1, "nothing to close", "the peer's call ended with it");

    // Exercise the project entry on the idle link with the same discoverable window.
    use makepad_widgets::{makepad_draw::cx_draw::CxDraw, *};
    let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let cx = &mut *pool;
    let project = cx.with_vm(|vm| {
        let value = script_eval!(vm, { use mod.widgets.* ProjectPage{visible: true} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut data = crate::lists::ViewData::default();
    data.tree.set(a.towns(), a.town_tree());
    let size = dvec2(640., 480.);
    let pass = DrawPass::new(cx);
    pass.set_size(cx, size);
    let mut list = DrawList2d::new(cx);
    let mut show = |cx: &mut Cx, data: &mut crate::lists::ViewData, ctl: &CallController| {
        data.call = ctl.shown();
        data.sync_live_rows(cx, &project);
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &event);
        let mut frame = Cx2d::new(&mut draw);
        frame.begin_pass(&pass, None);
        list.begin_always(&mut frame);
        frame.begin_root_turtle(size, Layout::flow_down());
        project.draw_all(&mut frame, &mut Scope::with_data(data));
        frame.end_pass_sized_turtle();
        list.end(&mut frame);
        frame.end_pass(&pass);
    };
    let activate = |cx: &mut Cx, widget: &WidgetRef, emitter: WidgetUid| {
        let request = Event::A11yAction(a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId::widget(widget.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None });
        let actions = cx.capture_actions(|cx| project.handle_event(cx, &request, &mut Scope::empty()));
        let presses: Vec<_> = actions.iter().filter_map(|a| a.as_widget_action())
            .filter_map(|a| Some((a.widget_uid, *a.action.downcast_ref::<CallAction>()?))).collect();
        assert_eq!(presses.len(), 1, "one project press must emit one call action");
        assert_eq!(presses[0].0, emitter, "the gate must see the original button or its picker");
        assert!(crate::call::opening_uids(cx, &project).contains(&emitter), "the local event must recognise the project emitter");
        presses[0].1
    };
    let sharer = &mut pair.seats[0];
    let menu = project.widget(cx, ids!(call_share));
    let choices = project.widget(cx, ids!(choices));
    show(cx, &mut data, &sharer.ctl);
    assert!(menu.visible() && !choices.visible(), "the idle project must offer Share with the list closed");
    let action = activate(cx, &menu, menu.widget_uid());
    assert_eq!(action, CallAction::Press(Press::Share(ShareChoice::Menu)), "project Share must use the call-page action");
    assert!(sharer.ctl.call_opening_presses(&mut sharer.host, action), "a local project press opens the picker");
    sharer.ctl.share_app_events(&mut app.app, &[]);
    show(cx, &mut data, &sharer.ctl);
    let row = data.call.share.choices.as_ref().unwrap().iter().find(|row| row.selectable).unwrap();
    let target = choices.widget(cx, &[id!(list), row.widget_id]);
    let action = activate(cx, &target, choices.widget_uid());
    assert_eq!(action, CallAction::Press(Press::Share(row.choice)), "the displayed row must choose its own window");
    assert!(!sharer.ctl.call_actions(&mut sharer.host, action), "a queued project row cannot start the offer");
    sharer.ctl.share_app_events(&mut app.app, &[]);
    assert!(sharer.ctl.shown().share.offers.is_empty(), "a row without local consent started a share");
    assert!(sharer.ctl.call_opening_presses(&mut sharer.host, action), "the local row must select the advertised window");
    sharer.ctl.share_app_events(&mut app.app, &[]);
    show(cx, &mut data, &sharer.ctl);
    let offer = data.call.share.offers.first().expect("local selection must start the project share").clone();
    assert!(menu.visible() && menu.borrow::<Button>().unwrap().enabled() && !choices.visible(), "an outgoing offer keeps Share available and closes the picker");
    assert!(data.tree.lines.iter().any(|line| line.node == crate::lists::Node::Live(crate::lists::Live::Share(offer.session))
        && line.label == format!("You are sharing {}", offer.title)), "the project offer must appear in the sidebar by name");
    pair.wait(|s| s[1].ctl.viewer_session() == Some(offer.session));
    assert!(pair.seats.iter().all(|s| s.ctl.view().phase == Phase::Idle && !s.ctl.voice_attached && !s.ctl.has_call_workers()), "project sharing must leave voice idle on both seats");
    let sharer = &mut pair.seats[0];
    let menu_action = CallAction::Press(Press::Share(ShareChoice::Menu));
    assert!(sharer.ctl.call_opening_presses(&mut sharer.host, menu_action));
    sharer.ctl.share_app_events(&mut app.app, &[]);
    let choices_now = sharer.ctl.shown().share.choices.unwrap();
    assert!(!choices_now.iter().any(|r| r.selectable && r.label == offer.title), "the picker omits the shared window");
    assert!(sharer.ctl.call_opening_presses(&mut sharer.host, menu_action));
    pair.seats[0].press(Press::StopShare);
    pair.wait(|s| s.iter().all(|s|
        !s.ctl.has_call_workers() && s.ctl.sharing.shown.offer(offer.session).is_none()
    ));
    show(cx, &mut data, &pair.seats[0].ctl);
    assert!(menu.visible() && !choices.visible(), "Stop must restore the project Share button with the picker closed");
    let sharer = &mut pair.seats[0];
    assert!(!sharer.ctl.call_opening_presses(&mut sharer.host, action), "a row from the closed project picker cannot start another offer");
}

#[cfg(unix)]
#[test]
fn restarting_the_shared_app_explains_the_end_on_both_seats() {
    let (_homes, a, b) = crate::model::tests::shared_town("share-restart");
    let (mut pair, _) = linked_call(&a, &b);
    let restart = |pair: &mut Pair| {
        let mut rig = crate::app_link::RecordingRig::new();
        rig.app.scan();
        rig.wait(|r| !r.app.apps().is_empty());
        let link = rig.app.link(&rig.app.apps()[0].key.clone());
        rig.live(link);
        let window = rig.app.state(link).unwrap().windows[0].id;
        pair.seats[0].ctl.open_share_window(&mut rig.app, link, window).unwrap();
        let offer = pair.seats[0].ctl.shown().share.offers.first().unwrap().clone();
        pair.wait(|s| s[1].ctl.viewer_session() == Some(offer.session));

        assert!(pair.seats.iter().all(|seat| !seat.ctl.shown().share.ended.iter().any(|e| e.session == offer.session)), "a new offer has no ended notice of its own");

        let (_, listener, socket) = rig.restart(link);
        assert!(rig.events.iter().any(|e| matches!(e, AppLinkEvent::Restarted { ended, .. } if *ended == link)));
        assert!(pair.seats[0].ctl.share_app_events(&mut rig.app, &rig.events));
        pair.wait(|s| s[1].ctl.viewer_session().is_none());
        assert_eq!(rig.app.linked().count(), 0, "the replacement requires a new Share press");
        drop(listener);
        std::fs::remove_file(socket).unwrap();
        offer
    };
    let check_ended = |pair: &Pair, offer: &ShareOffer| {
        let notice = format!("{} closed or restarted.", offer.title);
        for (seat, role) in pair.seats.iter().zip([offer.role, ShareRole::Viewer { sharer: a.me() }]) {
            let ended = seat.ctl.shown().share.ended.iter().find(|e| e.session == offer.session).cloned().expect("a restarted app leaves an ended share");
            assert_eq!((ended.session, ended.title.as_ref(), ended.role, ended.notice.as_ref()),
                (offer.session, offer.title.as_str(), role, notice.as_str()));
        }
    };
    let offer = restart(&mut pair);
    check_ended(&pair, &offer);
    // Plain words: the Share button beside it is the way to share again.
    let notice = format!("{} closed or restarted.", offer.title);
    for seat in &mut pair.seats {
        seat.ctl.poll(&mut seat.host);
        assert_eq!(seat.ctl.take_share_events().last(), Some(&ShareEvent::Ended {
            session: offer.session, reason: ShareEndReason::AppClosedOrRestarted,
        }));
        assert_eq!(seat.ctl.shown().share.closed_notice.as_deref(), Some(notice.as_str()));
    }
    pair.seats[0].press(Press::HangUp);
    pair.wait(|s| s.iter().all(|seat| seat.ctl.view().phase == Phase::Idle));
    assert!(pair.seats.iter().all(|seat| seat.ctl.shown().share.closed_notice.is_none()));
    check_ended(&pair, &offer);

    let standalone = restart(&mut pair);
    pair.flush_signals();
    assert!(pair.seats.iter().all(|s| s.ctl.view().phase == Phase::Idle), "the second share ran without a call");
    check_ended(&pair, &standalone);
    let (replacement, _) = pair.seats[0].offer("Replacement");
    pair.wait(|s| s[1].ctl.viewer_session() == Some(replacement));
    check_ended(&pair, &standalone);
    assert!(pair.seats.iter().all(|seat| !seat.ctl.shown().share.ended.iter().any(|e| e.session == replacement)), "the replacement has its own status");
    pair.seats[0].press(Press::StopShare);
    pair.wait(|s| s[1].ctl.viewer_session().is_none());
    assert!(pair.seats.iter().all(|seat| !seat.ctl.shown().share.ended.iter().any(|e| e.session == replacement)), "a deliberate stop leaves no restart notice");
}

impl Seat {
    fn offer(&mut self, title: &str) -> (SessionId, Arc<Mutex<Reading>>) {
        let window = AppWindow { id: WindowId(7), title: title.into(), visible: true, size: (320., 180.), dpi: 1. };
        let (source, readings) = held_source();
        self.ctl.start_window(LinkId::TEST, &window, title.into(), source).unwrap();
        (self.ctl.sharing.shown.offers.first().unwrap().session, readings)
    }

    fn peer(&self) -> PublicKey { self.ctl.links.values().next().unwrap().peer }

    fn number(&self) -> u32 { self.ctl.links.values().next().unwrap().media.session_last }

    fn media_id(&self) -> CallId {
        let link = self.ctl.links.values().next().unwrap();
        MediaKey::media_id(self.number(), (&link.me, &link.local_media_key), (&link.peer, link.peer_media_key.as_ref().unwrap()))
    }

    fn watch(&mut self, session: SessionId) {
        assert!(self.ctl.call_opening_presses(&mut self.host, CallAction::OnShare(session, Press::Watch)));
        self.ctl.share_panel_size(session, crate::call::ShareSurface::Page, 320, 180);
    }
}

impl Pair {
    fn picture(&mut self, viewer: usize, session: SessionId) {
        self.wait(|s| s[viewer].ctl.sharing.shown.offer(session).is_some_and(|o| matches!(o.role, ShareRole::Viewer { .. })));
        self.seats[viewer].watch(session);
        self.wait(|s| s[viewer].ctl.sharing.shown.picture.is_some());
        let ctl = &mut self.seats[viewer].ctl;
        assert!(!ctl.take_facts().iter().any(|f| matches!(f, Fact::ShareJoin { .. })));
        ctl.sharing.shown.offer(session).unwrap().picture_drawn();
        assert_eq!(ctl.take_facts().iter().filter(|f| matches!(f, Fact::ShareJoin { session: id, .. } if *id == session)).count(), 1);
        assert!(ctl.take_facts().is_empty());
        assert!(self.seats.iter().all(|s| s.ctl.current.is_some() && (s.ctl.sharing.sending.is_some() || !s.ctl.sharing.receiving_peers.is_empty())));
        assert_eq!(self.seats[0].number(), self.seats[1].number());
    }

    /// Messages queued before these markers have run through both Call-channel handlers.
    fn flush_signals(&mut self) {
        for seat in &mut self.seats { seat.ctl.send_peer_to(seat.peer(), crate::call::topic::PROBE, vec![1]).unwrap(); }
        let mut received = [false; 2];
        self.wait(|seats| {
            for (seen, s) in received.iter_mut().zip(seats) {
                *seen |= !s.ctl.take_peer_with_sender(crate::call::topic::PROBE).is_empty();
            }
            received == [true; 2]
        });
    }
}

#[test]
fn media_share_crosses_hangup_and_can_replace_its_window_before_poll() {
    let (_homes, a, b) = crate::model::tests::shared_town("media-crossing");
    for (first, second) in [(&a, &b), (&b, &a)] {
        let (mut pair, networks) = linked_call(first, second);
        pair.seats[1].press(Press::HangUp);
        let (first_offer, first_source) = pair.seats[0].offer("First");
        pair.picture(1, first_offer);
        assert!(pair.seats.iter().all(|s| s.ctl.view().phase == Phase::Idle));
        let n = pair.seats[0].number();
        pair.seats[0].press(Press::StopShare);
        let (second_offer, second_source) = pair.seats[0].offer("Second");
        pair.picture(1, second_offer);
        assert_eq!(pair.seats[0].number(), n + 1);
        assert!(second_source.lock().unwrap().popped > 0);
        pair.wait(|_| first_source.lock().unwrap().drops == 1);
        let starts = networks.load(Ordering::Acquire);
        let keeper = usize::from(first.me() > second.me());
        let follower = 1 - keeper;
        let n = n + 1;
        let id = pair.seats[0].media_id();
        pair.flush_signals();
        let before = pair.seats.each_ref().map(|s| s.ctl.links.values().next().unwrap().media.clone());
        // Duplicate keys and out-of-role or stale signals cannot change this running session.
        for seat in &mut pair.seats {
            let ctl = &mut seat.ctl;
            ctl.send_to(ctl.links.values().next().unwrap().peer, MediaKey::generate().unwrap().encode());
        }
        for msg in [MediaSignal::Media { n, on: true }, MediaSignal::Media { n: n-1, on: true },
            MediaSignal::Media { n: n-1, on: false }, MediaSignal::Want { on: false, last: n, running: false }] {
            pair.seats[keeper].ctl.send_media_to(pair.seats[keeper].peer(), msg);
        }
        for msg in [MediaSignal::Want { on: false, last: n+1, running: false }, MediaSignal::Media { n: n+1, on: true }] {
            pair.seats[follower].ctl.send_media_to(pair.seats[follower].peer(), msg);
        }
        pair.flush_signals();
        for (seat, state) in pair.seats.iter().zip(before) {
            assert!(seat.ctl.links.values().next().unwrap().media == state, "a refused signal changed the session state");
            assert_eq!(seat.number(), n);
            assert!(seat.media_id() == id);
            assert!(seat.ctl.current.is_some() && seat.ctl.stopping.is_empty());
        }
        pair.seats[1].press(Press::CloseWatch);
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers()));
        pair.seats[1].press(Press::Watch);
        pair.wait(|s| s[1].ctl.sharing.shown.picture.is_some());
        assert_eq!(networks.load(Ordering::Acquire), starts + 2);
        pair.seats[0].press(Press::StopShare);
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers()));
        assert!(pair.seats.iter().all(|s| s.number() == n + 1));
    }
}

#[test]
fn media_simultaneous_offers_switch_windows_and_call_glare_reuses_a_share() {
    let (_homes, a, b) = crate::model::tests::shared_town("media-offers");
    for (first, second) in [(&a, &b), (&b, &a)] {
        let (mut pair, networks) = linked_controllers(first, second);
        let offers = [pair.seats[0].offer("Left"), pair.seats[1].offer("Right")];
        let keeper = usize::from(first.me() > second.me());
        let follower = 1 - keeper;
        pair.picture(follower, offers[keeper].0);
        pair.picture(keeper, offers[follower].0);
        assert!(offers.iter().all(|(_, source)| source.lock().unwrap().popped > 0), "both directions can stream");
        let window = AppWindow { id: WindowId(8), title: "Another window".into(), visible: true, size: (640., 360.), dpi: 1. };
        let (mut source, second_source) = held_source();
        let rung = LADDER.iter().find(|r| r.width != 320).unwrap();
        source.encoder = TestCodecs.encoder(*rung, 198).unwrap();
        source.pixels.resize((rung.width * rung.height * 3 / 2) as usize, 128);
        let other = pair.seats[keeper].ctl.start_window(LinkId::TEST, &window, window.title.clone(), source).unwrap();
        pair.flush_signals();
        assert_eq!(pair.seats[follower].ctl.sharing.shown.picture.as_ref().unwrap().width, 320);
        assert_eq!(second_source.lock().unwrap().popped, 0, "offering another window does not switch the stream");
        let keeper_model = [first, second][keeper];
        let stop = crate::call::ai::CallTool::ShareStop;
        let (answer, note, changed) = run_call_tool(keeper_model, &mut pair.seats[keeper], stop, None);
        assert_eq!((note, changed), ("multiple shares running", false), "an ambiguous stop returns a static note and stops nothing");
        let first_title = if keeper == 0 { "Left" } else { "Right" };
        let guidance = format!("Two shares are running ({first_title}, {}). Open the one to stop, or stop it on its page.", window.title);
        assert_eq!(answer.get("message").and_then(makepad_strict_json::Value::as_str), Some(guidance.as_str()),
            "the gated answer names both active shares and explains how to select one");
        pair.seats[follower].watch(other);
        assert!(pair.seats[follower].ctl.sharing.shown.picture.is_none(), "switching clears the previous picture immediately");
        pair.wait(|s| s[follower].ctl.sharing.shown.picture.as_ref().is_some_and(|p| p.width == rung.width));
        assert_eq!(pair.seats[keeper].ctl.sharing.shown.own_offers().find(|o| !o.watchers.is_empty()).map(|o| o.session), Some(other));
        assert_eq!(offers[keeper].1.lock().unwrap().demand, Some(View::default()), "the first source is parked");
        assert_eq!(pair.seats[keeper].ctl.sharing.shown.watching, Some(offers[follower].0), "the reverse watch continues");
        let (_, _, changed) = run_call_tool(keeper_model, &mut pair.seats[keeper], stop, Some(offers[keeper].0));
        assert!(changed, "the open page selects just that offer");
        pair.flush_signals();
        assert!(pair.seats[follower].ctl.sharing.shown.offer(offers[keeper].0).is_none());
        assert_eq!(pair.seats[follower].ctl.sharing.shown.watching, Some(other));
        pair.seats[keeper].press(Press::StopShare);
        pair.seats[follower].press(Press::StopShare);
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers()));
        let n = pair.seats[0].number();
        let starts = networks.load(Ordering::Acquire);
        pair.seats[0].press(Press::Call);
        pair.seats[1].press(Press::Call);
        let (offer, _) = pair.seats[0].offer("Glare");
        pair.picture(1, offer);
        pair.wait(|s| s.iter().all(|s| s.ctl.voice_attached));
        pair.seats[0].press(Press::StopShare);
        pair.wait(|s| s.iter().all(|s| s.ctl.sharing.shown.offers.is_empty()));
        assert!(pair.seats.iter().all(|s| s.number() == n + 1 && s.ctl.voice_attached));
        pair.seats[1].press(Press::HangUp);
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers()));
        assert_eq!(networks.load(Ordering::Acquire), starts + 2);

        // A plain offer and Stop, with either key order, cost exactly one number.
        let (offer, _) = pair.seats[0].offer("Alone");
        pair.picture(1, offer);
        pair.seats[0].press(Press::StopShare);
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers()));
        assert!(pair.seats.iter().all(|s| s.number() == n + 2));
        assert_eq!(networks.load(Ordering::Acquire), starts + 4);
    }
}

#[test]
fn media_late_key_holds_the_number_and_watch_survives_attachment_before_layout() {
    let (_homes, a, b) = crate::model::tests::shared_town("media-late");
    for (first, second) in [(&a, &b), (&b, &a)] {
        let mut withheld = None;
        let (mut pair, networks) = linked_with(first, second, |link, port| {
            if link.me > link.peer { withheld = Some(port.messages_out.try_recv().unwrap()); }
        });
        let keeper = usize::from(first.me() > second.me());
        let follower = 1 - keeper;
        let (offer, _) = pair.seats[keeper].offer("Sheet");
        pair.wait(|s| s[follower].ctl.viewer_session() == Some(offer));
        pair.seats[follower].press(Press::Watch);
        pair.flush_signals();
        assert_eq!(pair.seats[follower].ctl.shown().share.single_waiting_line(), Some("Connecting securely…"));
        assert!(pair.seats.iter().all(|s| s.number() == 0 && !s.ctl.has_call_workers()));
        assert_eq!(networks.load(Ordering::Acquire), 0);
        assert!(pair.seats[follower].ctl.take_facts().is_empty());
        let ctl = &mut pair.seats[follower].ctl;
        ctl.send_to(ctl.links.values().next().unwrap().peer, withheld.take().unwrap());
        pair.wait(|s| s.iter().all(|s| s.number() == 1 && (s.ctl.sharing.sending.is_some() || !s.ctl.sharing.receiving_peers.is_empty())));
        let ctl = &mut pair.seats[follower].ctl;
        assert!(ctl.current.as_ref().unwrap().share_picture(ctl.links.values().next().unwrap().peer_id).is_some());
        assert_eq!(ctl.shown().share.single_waiting_line(), Some("Waiting for Peer's picture…"));
        ctl.share_panel_size(offer, crate::call::ShareSurface::Page, 320, 180);
        pair.wait(|s| s[follower].ctl.sharing.shown.picture.is_some());
        let ctl = &mut pair.seats[follower].ctl;
        assert!(ctl.take_facts().is_empty());
        ctl.sharing.shown.offers.first().unwrap().picture_drawn();
        assert!(matches!(ctl.take_facts().as_slice(), [Fact::ShareJoin { session, .. }] if *session == offer));
        assert_eq!(networks.load(Ordering::Acquire), 2);
    }
}

#[test]
fn share_caps_are_the_windows_pixels_bounded_only_by_the_encoder() {
    let size = |c: VideoCaps| (c.width, c.height);
    let (retina, note) = caps((1600., 1000.), 2., None).unwrap();
    assert_eq!((size(retina), note), ((3200, 2000), None), "a Retina window goes out at its own pixels, past the old 1920x1200 cap");
    let (taken, note) = caps((1600., 1000.), 2., Some([4096, 2304])).unwrap();
    assert_eq!((size(taken), note), ((3200, 2000), None), "an encoder that takes the window leaves it whole");
    assert_eq!(size(caps((801., 601.), 1., None).unwrap().0), (800, 600), "encoders take even sizes");
    let (bounded, note) = caps((2560., 1600.), 2., Some([4096, 2304])).unwrap();
    assert_eq!(size(bounded), (3686, 2304), "only the encoder's largest picture bounds the size, keeping the shape");
    assert_eq!(note.as_deref(), Some("The window is 5120x3200 pixels and this computer's encoder takes at most 4096x2304, so it goes out at 3686x2304."),
        "a bound is said, never silent");
}

#[test]
fn media_only_the_current_lease_drives_the_source() {
    let (source, readings) = held_source();
    let offer = OfferSource::new(caps((320., 180.), 1., None).unwrap().0, source);
    let mut old = offer.lease();
    assert_eq!(readings.lock().unwrap().keys, 1);
    let mut current = offer.lease();
    old.key_now();
    old.want(320, 180, 30);
    assert!(old.pop().is_none() && old.frame_counts().is_none());
    assert_eq!(readings.lock().unwrap().demand, None);
    assert_eq!(readings.lock().unwrap().keys, 2);
    assert_eq!(readings.lock().unwrap().popped, 0);
    assert!(current.pop().is_some());
    assert_eq!(current.frame_counts(), Some((1, 1)));
    let busy = offer.cell.source.lock().unwrap();
    let mut next = offer.lease();
    assert_eq!(readings.lock().unwrap().keys, 2, "a busy source defers the lease's key request");
    drop(busy);
    assert!(current.pop().is_none() && next.pop().is_some(), "only the new worker can obtain the held key");
    assert_eq!(readings.lock().unwrap().keys, 3);
    drop(next);
    drop(offer);
    drop(current);
    assert_eq!(readings.lock().unwrap().drops, 0);
    drop(old);
    assert_eq!(readings.lock().unwrap().drops, 1);
}

#[test]
fn media_relink_rotates_both_keys_and_refuses_the_old_windows_datagram() {
    use makepad_call::wire::{stream, Header};
    let (_homes, a, b) = crate::model::tests::shared_town("media-relink");
    for (first, second) in [(&a, &b), (&b, &a)] {
        let (mut pair, _) = linked_controllers(first, second);
        let (capture, packets) = sync_channel(256);
        let sender = &mut pair.seats[0].ctl.links.values_mut().next().unwrap().out;
        let forward = std::mem::replace(sender, capture);
        let (offer, _) = pair.seats[0].offer("First");
        pair.wait(|s| s[1].ctl.viewer_session() == Some(offer));
        pair.seats[1].watch(offer);
        let mut saved = None;
        pair.wait(|s| {
            for packet in packets.try_iter() {
                if saved.is_none() && Header::parse(&packet).is_some_and(|(h, _)| h.stream == stream::SHARE) {
                    saved = Some(packet.clone());
                }
                forward.try_send(packet).unwrap();
            }
            saved.is_some() && s[1].ctl.sharing.shown.picture.is_some()
        });
        pair.seats[0].ctl.links.values_mut().next().unwrap().out = forward;
        let datagram = saved.unwrap();
        let old_id = pair.seats[0].media_id();
        let key_fingerprints = |pair: &Pair| pair.seats.each_ref().map(|s| {
            let link = s.ctl.links.values().next().unwrap();
            makepad_network::digest::sha256_hash(&link.local_media_key.session_secret(&[0; 16], &link.me))
        });
        let first_keys = key_fingerprints(&pair);
        let refuse_old = |pair: &mut Pair| {
            let refused = |c: &Call| c.stats().strays + c.stats().peers.iter()
                .map(|p| p.session.bad_tag + p.session.refused).sum::<u64>();
            let call = pair.seats[1].ctl.current.as_ref().unwrap();
            let before = refused(call);
            call.inlet().try_send((std::net::SocketAddr::from(([192, 0, 2, 1], 47100)), datagram.clone())).unwrap();
            pair.wait(|s| refused(s[1].ctl.current.as_ref().unwrap()) > before);
        };

        pair.seats[0].press(Press::StopShare);
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers()));
        let (second_offer, _) = pair.seats[0].offer("Second");
        pair.picture(1, second_offer);
        assert_eq!(pair.seats[0].number(), 2);
        assert!(pair.seats[1].media_id() != old_id);
        refuse_old(&mut pair);

        let (workers, _) = linked_pair(first.device_key(), second.device_key());
        for (s, (worker, peer)) in pair.seats.iter_mut().zip(workers) {
            let me = s.ctl.links.values().next().unwrap().me;
            let port = s.ctl.link_up(&mut s.host, me, peer, "Peer").unwrap();
            s.ctl.set_open_town(Some((a.open_id(), vec![(peer, "Peer".into())])));
            s.ctl.set_peer_caps(peer, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
            assert_eq!(s.number(), 0);
            assert!(s.ctl.current.is_none() && s.ctl.sharing.shown.offers.is_empty());
            assert!(s.ctl.take_share_events().contains(&ShareEvent::Ended { session: second_offer, reason: ShareEndReason::LinkDropped }));
            assert!(s.ctl.take_facts().iter().any(|f| matches!(f, Fact::Leave { session, why: LeaveWhy::Dropped, .. } if *session == second_offer)));
            assert!(worker.try_send(ToWorker::Call(port)).is_ok());
            s.worker = worker;
        }
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers() && s.ctl.links.values().next().unwrap().peer_media_key.is_some()));
        assert!(key_fingerprints(&pair).iter().zip(first_keys).all(|(new, old)| *new != old));
        let (new_offer, _) = pair.seats[0].offer("Relinked");
        pair.picture(1, new_offer);
        assert!(pair.seats.iter().all(|s| s.number() == 1 && s.media_id() != old_id));
        refuse_old(&mut pair);
    }
}

#[test]
fn media_ring_crosses_share_and_answer_crosses_watch_or_stop() {
    let (_homes, a, b) = crate::model::tests::shared_town("media-answer");
    for (first, second) in [(&a, &b), (&b, &a)] {
        let (mut pair, _) = linked_controllers(first, second);
        let (offer, _) = pair.seats[0].offer("Watch");
        pair.seats[1].press(Press::Call);
        pair.wait(|s| s[0].ctl.view().ringing() && s[1].ctl.viewer_session() == Some(offer));
        pair.seats[1].watch(offer);
        pair.seats[0].press(Press::Answer);
        pair.wait(|s| s.iter().all(|s| s.ctl.voice_attached) && s[1].ctl.sharing.shown.picture.is_some());
        assert!(pair.seats.iter().all(|s| s.number() == 1));
        pair.seats[0].press(Press::HangUp);
        pair.wait(|s| s.iter().all(|s| s.ctl.view().phase == Phase::Idle));
        pair.seats[0].press(Press::Call);
        pair.wait(|s| s[1].ctl.view().ringing());
        pair.seats[0].press(Press::StopShare);
        pair.seats[1].press(Press::Answer);
        pair.wait(|s| s.iter().all(|s| s.ctl.voice_attached && s.ctl.sharing.shown.offers.is_empty()));
        assert_eq!(pair.seats[0].number(), pair.seats[1].number());
        assert_eq!(pair.seats[0].number(), 2, "the sharer removed its last medium before it heard Answer");
        pair.seats[0].press(Press::HangUp);
        pair.wait(|s| s.iter().all(|s| !s.ctl.has_call_workers()));
    }
}

// Keep the real reliable and datagram carriers for each pair. The voice-isolation
// check observes the outgoing media headers before passing them to the link worker.
const MESH_TOWN: TownId = [0x44; 32];
const NAMES: [&str; 3] = ["Ana", "Ben", "Kit"];
const SHARE_CAPS: MediaCaps = MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1);

struct MeshLink {
    worker: LinkWorker,
    outgoing: std::sync::mpsc::Receiver<Vec<u8>>,
    forward: std::sync::mpsc::SyncSender<Vec<u8>>,
    voice_seq: Option<u64>,
}

pub(crate) struct ShareMesh<H: AudioHost = Host> {
    keys: [Arc<makepad_town_core::key::DeviceKey>; 3],
    pub(crate) ctl: [CallController; 3],
    hosts: [H; 3],
    apps: [AppLinks; 3],
    #[cfg(unix)]
    pub(crate) rig: Option<crate::app_link::RecordingRig>,
    carriers: BTreeMap<(usize, usize), MeshLink>,
    networks: Arc<AtomicUsize>,
}

impl ShareMesh {
    fn new(third: bool) -> Self {
        Self::with_hosts(third, Default::default(), |i| VoiceSource::Synthetic { tone_hz: 440. + 110. * i as f32 })
    }
}

impl<H: AudioHost> ShareMesh<H> {
    fn with_hosts(third: bool, hosts: [H; 3], voice: impl Fn(usize) -> VoiceSource) -> Self {
        let networks = Arc::new(AtomicUsize::new(0));
        let count = networks.clone();
        let run = makepad_call::driver::std_spawn();
        let spawn: Spawn = Arc::new(move |name, work| {
            if name == "call network" { count.fetch_add(1, Ordering::Release); }
            run(name, work);
        });
        let mut mesh = Self {
            keys: std::array::from_fn(|_| Arc::new(makepad_town_core::key::DeviceKey::generate().unwrap())),
            ctl: Default::default(), hosts, apps: Default::default(),
            carriers: Default::default(), networks,
            #[cfg(unix)]
            rig: None,
        };
        for (i, ctl) in mesh.ctl.iter_mut().enumerate() {
            ctl.codecs = Arc::new(TestCodecs);
            ctl.configure(CallEnv { spawn: spawn.clone(), jobs: spawn.clone(), signal: Arc::new(|| {}),
                files: Default::default(), devices_path: Default::default(),
                voice: voice(i) });
        }
        mesh.link(0, 1, true);
        if third { mesh.add_third(); }
        mesh
    }

    pub(crate) fn key(&self, i: usize) -> PublicKey { self.keys[i].public() }

    fn link(&mut self, a: usize, b: usize, caps_first: bool) {
        let (pair, _) = linked_pair(self.keys[a].clone(), self.keys[b].clone());
        for ((worker, peer), (i,j)) in pair.into_iter().zip([(a,b),(b,a)]) {
            let me = self.key(i);
            let mut port = self.ctl[i].link_up(&mut self.hosts[i], me, peer, NAMES[j]).unwrap();
            let (forward, rx) = std::sync::mpsc::sync_channel(2048);
            let outgoing = std::mem::replace(&mut port.datagrams_out, rx);
            assert!(worker.try_send(ToWorker::Call(port)).is_ok(), "seat {i} attaches its carrier to {j}");
            self.carriers.insert((i,j), MeshLink { worker, outgoing, forward, voice_seq: None });
            if caps_first { self.ctl[i].set_peer_caps(peer, SHARE_CAPS); }
            let peers: Vec<_> = self.carriers.keys().filter(|(k,_)| *k == i).map(|(_,j)| (self.key(*j), NAMES[*j].into())).collect();
            self.ctl[i].set_town_peers(&mut self.hosts[i], BTreeMap::from([(MESH_TOWN, peers.iter().map(|(p,_)| *p).collect())]));
            self.ctl[i].set_open_town(Some((MESH_TOWN, peers)));
            if !caps_first { self.ctl[i].set_peer_caps(peer, SHARE_CAPS); }
        }
    }

    fn add_third(&mut self) { self.link(0, 2, false); self.link(1, 2, true); }

    fn disconnect(&mut self, i: usize, j: usize) {
        for (a,b) in [(i,j),(j,i)] {
            let p = self.key(b);
            self.ctl[a].link_down(&mut self.hosts[a], p);
            self.carriers.remove(&(a,b));
        }
    }

    fn pump(&mut self) {
        for link in self.carriers.values_mut() {
            for d in link.outgoing.try_iter() {
                if let Some((header,_)) = makepad_call::wire::Header::parse(&d) {
                    if header.stream == makepad_call::wire::stream::VOICE { link.voice_seq = Some(header.seq); }
                }
                link.forward.try_send(d).expect("the live carrier accepts the observed datagram");
            }
            while let Some(event) = link.worker.try_recv() {
                assert!(!matches!(event, FromWorker::Failed(_) | FromWorker::Lost), "the share mesh carrier stays up: {event:?}");
            }
        }
        for i in 0..3 {
            self.ctl[i].poll(&mut self.hosts[i]);
            #[cfg(unix)]
            if i == 0 {
                if let Some(rig) = &mut self.rig {
                    let events = rig.app.poll();
                    self.ctl[i].share_app_events(&mut rig.app, &events);
                    self.ctl[i].receive_control(&mut rig.app);
                    continue
                }
            }
            self.ctl[i].receive_control(&mut self.apps[i]);
        }
    }

    fn until(&mut self, what: &str, mut ready: impl FnMut(&mut Self) -> bool) {
        let limit = Instant::now() + Duration::from_secs(15);
        loop {
            self.pump();
            if ready(self) { return }
            assert!(Instant::now() < limit, "waiting for {what}");
            thread::yield_now();
        }
    }

    fn offer(&mut self) -> (SessionId, Arc<Mutex<Reading>>) {
        let (source, readings) = held_source();
        let w = AppWindow { id: WindowId(7), title: "Drawing".into(), visible: true, size: (320.,180.), dpi: 1. };
        let session = self.ctl[0].start_window(LinkId::TEST, &w, w.title.clone(), source).unwrap();
        let probe = readings.clone();
        self.ctl[0].sharing.locals.get_mut(&session).unwrap().probe = Some(Box::new(move || (1, probe.lock().unwrap().ended_why.clone())));
        (session, readings)
    }

    pub(crate) fn watch(&mut self, i: usize, s: SessionId) {
        self.until("the town offer to reach the viewer", |m| m.ctl[i].sharing.shown.offer(s).is_some());
        assert!(self.ctl[i].call_opening_presses(&mut self.hosts[i], CallAction::OnShare(s, Press::Watch)), "the viewer opens the selected offer");
        self.ctl[i].share_panel_size(s, ShareSurface::Page, 320, 180);
    }

    pub(crate) fn request_named_control(&mut self, viewer: usize, share: SessionId) {
        let peer = self.key(viewer);
        self.watch(viewer, share);
        assert!(self.ctl[viewer].ask_control(share, true));
        self.until("the viewer and their named control request", |m| {
            m.ctl[0].is_viewer(share, &peer) && m.ctl[0].sharing.shown.control.requests.iter()
                .any(|(s, p, _)| (*s, *p) == (share, peer))
        });
    }

    fn pts(&self, i: usize) -> Option<i64> { self.ctl[i].sharing.shown.picture.as_ref().map(|p| p.pts_100ns) }

    pub(crate) fn barrier(&mut self, from: usize, to: usize) {
        let key = self.key(to);
        self.ctl[from].send_peer_to(key, crate::call::topic::PROBE, vec![1]).unwrap();
        self.until("the reliable marker following the test messages", |m|
            m.ctl[to].take_peer_with_sender(crate::call::topic::PROBE).iter().any(|(p,_)| *p == m.key(from)));
    }
}

#[test]
fn a_named_allow_ends_when_its_seat_leaves_the_town() {
    let mut mesh = ShareMesh::new(true);
    let (share, _) = mesh.offer();
    let (peer, other) = (mesh.key(1), mesh.key(2));
    mesh.request_named_control(1, share);
    assert!(mesh.ctl[0].allow_control(&mut mesh.apps[0], share, peer, NAMES[1]));
    let named = crate::tweak::ControlRights { all: false, seats: [peer].into() };
    let revision = mesh.ctl[0].control_rights_revision(share).unwrap();
    mesh.disconnect(0, 1);
    assert_eq!(mesh.ctl[0].control_rights(share).as_ref(), Some(&named), "losing the carrier preserves a named Allow");
    mesh.link(0, 1, true);
    assert_eq!(mesh.ctl[0].control_rights(share).as_ref(), Some(&named), "relink preserves a named Allow");
    assert_eq!(mesh.ctl[0].control_rights_revision(share), Some(revision));

    mesh.disconnect(0, 1);
    mesh.ctl[0].set_town_members([(MESH_TOWN, [other].into())].into());
    assert_eq!(mesh.ctl[0].control_rights(share), Some(Default::default()), "leaving the town withdraws the offline seat's named Allow");
    assert!(mesh.ctl[0].control_rights_revision(share).unwrap() > revision);
    mesh.link(0, 1, true);
    assert_eq!(mesh.ctl[0].control_rights(share), Some(Default::default()), "a returning member needs a fresh Allow");
}

#[test]
fn an_offline_member_keeps_a_named_allow() {
    let mut mesh = ShareMesh::new(true);
    let (share, _) = mesh.offer();
    let (peer, other) = (mesh.key(1), mesh.key(2));
    mesh.request_named_control(1, share);
    assert!(mesh.ctl[0].allow_control(&mut mesh.apps[0], share, peer, NAMES[1]));
    let named = crate::tweak::ControlRights { all: false, seats: [peer].into() };
    let revision = mesh.ctl[0].control_rights_revision(share).unwrap();

    mesh.disconnect(0, 1);
    mesh.ctl[0].set_town_peers(&mut mesh.hosts[0], [(MESH_TOWN, [other].into())].into());
    for members in [[(MESH_TOWN, [peer, other].into())].into(), BTreeMap::new()] {
        assert!(!mesh.ctl[0].set_town_members(members));
        assert_eq!(mesh.ctl[0].control_rights(share).as_ref(), Some(&named), "an offline member keeps the named Allow");
        assert_eq!(mesh.ctl[0].control_rights_revision(share), Some(revision), "no revocation was announced");
    }
}

impl<H: AudioHost> Drop for ShareMesh<H> {
    fn drop(&mut self) {
        for (ctl, host) in self.ctl.iter_mut().zip(&mut self.hosts) { ctl.shutdown(host); }
        while self.ctl.iter().any(CallController::has_call_workers) {
            for (ctl, host) in self.ctl.iter_mut().zip(&mut self.hosts) { ctl.poll(host); }
            thread::yield_now();
        }
    }
}

#[test]
fn multi_viewer_share_delivers_frames_to_two_viewers_and_survives_one_leaving() {
    let mut m = ShareMesh::new(true);
    let (s, reading) = m.offer();
    m.until("both incoming offers", |m| [1,2].iter().all(|&i| m.ctl[i].sharing.shown.offer(s).is_some()));
    assert_eq!(m.networks.load(Ordering::Acquire), 0, "offering to two recipients starts no media workers");
    for i in [1,2] { m.watch(i,s); }
    m.until("both viewers to decode a picture", |m| m.pts(1).is_some() && m.pts(2).is_some());
    let mut expected = vec![(m.key(1), NAMES[1].to_owned()), (m.key(2), NAMES[2].to_owned())];
    expected.sort();
    assert_eq!(m.ctl[0].sharing.shown.offer(s).unwrap().watchers, expected, "the local offer names both viewers");
    assert_eq!(m.ctl[0].sharing.shown.watchers, expected, "the share summary names both viewers");
    let initial = [1,2].map(|i| m.pts(i).unwrap());
    let parks = reading.lock().unwrap().parks;
    m.ctl[0].settle_media();
    m.ctl[0].poll(&mut m.hosts[0]);
    for i in [1,2] {
        let pid = m.ctl[0].links[&m.key(i)].peer_id;
        assert_eq!(m.ctl[0].sharing.sent_views.get(&pid), Some(&(s, m.ctl[i].share_subscription())), "unchanged settling preserves viewer {i}");
    }
    reading.lock().unwrap().next_frame = true;
    m.until("both viewers to decode the next frame without a new key request", |m| [1,2].iter().all(|&i| m.pts(i) > Some(initial[i-1])));
    assert_eq!(reading.lock().unwrap().parks, parks, "unchanged settling keeps demand open");
    let generations = [0,2].map(|i| m.ctl[i].call_generation());
    assert!(m.ctl[1].call_actions(&mut m.hosts[1], CallAction::OnShare(s, Press::CloseWatch)), "Ben closes Watch");
    m.until("Ben's subscription to close", |m| m.ctl[0].sharing.shown.watchers.len() == 1);
    m.disconnect(0,1);
    let previous = m.pts(2);
    reading.lock().unwrap().next_frame = true;
    m.until("Kit to receive a new picture after Ben disconnects", |m| m.pts(2) > previous);
    assert_eq!([0,2].map(|i| m.ctl[i].call_generation()), generations, "both surviving calls keep their workers");
    assert_eq!(m.ctl[0].sharing.shown.watchers, [(m.key(2), NAMES[2].into())], "Kit remains the only watcher");
}

#[test]
fn late_linker_receives_active_share_offer_and_watches() {
    let mut m = ShareMesh::new(false);
    let (s, reading) = m.offer();
    m.watch(1,s);
    m.until("Ben's first picture", |m| m.pts(1).is_some());
    let old = m.pts(1);
    let generation = m.ctl[0].call_generation();
    m.add_third();
    m.watch(2,s);
    m.until("Kit's first picture and Ben's next picture", |m| m.pts(2).is_some() && m.pts(1) > old);
    let offer = m.ctl[2].sharing.shown.offer(s).unwrap();
    assert_eq!((offer.town, offer.role), (MESH_TOWN, ShareRole::Viewer { sharer: m.key(0) }), "the late offer binds Ana and the original town");
    let ben = m.key(1);
    let kit = m.key(2);
    m.ctl[0].set_town_peers(&mut m.hosts[0], BTreeMap::from([(MESH_TOWN,BTreeSet::from([ben]))]));
    m.until("revocation to remove Kit's offer", |m| m.ctl[2].sharing.shown.offer(s).is_none());
    let local = &m.ctl[0].sharing.locals[&s];
    assert!(!local.recipients.contains(&kit) && !local.viewers.contains_key(&kit), "revocation removes both delivery and viewing rights");
    let previous = m.pts(1);
    reading.lock().unwrap().next_frame = true;
    m.until("Ben to receive another picture after revocation", |m| m.pts(1) > previous);
    assert_eq!(m.ctl[0].call_generation(), generation, "revocation keeps Ben's original network worker");
    reading.lock().unwrap().ended_why = Some("capture failed".into());
    m.ctl[0].poll_share();
    m.until("Ben's terminal reason and retirement of standalone media", |m|
        m.ctl[1].sharing.shown.offer(s).unwrap().stopped_line.is_some() && m.ctl.iter().all(|ctl| !ctl.has_call_workers()));
    let starts = m.networks.load(Ordering::Acquire);
    m.ctl[0].set_town_peers(&mut m.hosts[0], BTreeMap::from([(MESH_TOWN,BTreeSet::from([ben, kit]))]));
    m.until("Kit's replayed terminal reason", |m| m.ctl[2].sharing.shown.offer(s).is_some_and(|o| o.stopped_line.is_some()));
    m.watch(2,s);
    m.barrier(2,0);
    let shown = m.ctl[2].shown().share;
    let line = shown.waiting_line(shown.offer(s).unwrap()).unwrap();
    assert!(line.contains("capture failed"), "the reopened panel explains the stopped picture");
    assert_eq!(Some(line), m.ctl[1].sharing.shown.offer(s).unwrap().stopped_line.as_deref(), "both viewers see the same terminal reason");
    assert_eq!(m.ctl[2].share_subscription(), View::default(), "the stopped panel requests no pictures");
    let view = Message::View { session: s, width: 320, height: 180, fps: 30, ext: Default::default() };
    assert!(!m.ctl[0].share_message(kit, &view.encode()), "an open View in flight cannot restart a stopped offer");
    m.ctl[0].settle_media();
    assert!(m.ctl.iter().all(|ctl| !ctl.has_call_workers()), "the stopped offer needs no workers");
    assert_eq!(m.networks.load(Ordering::Acquire), starts, "no new worker started for the stopped offer");
    assert!(!m.ctl[0].is_viewer(s,&kit) && !m.ctl[0].sharing.shown.offer(s).unwrap().watchers.iter().any(|(p,_)| *p == kit),
        "neither the in-flight View nor reopening a stopped offer registers Kit");
}

#[test]
fn share_only_viewer_gets_video_without_voice_leak() {
    let mut m = ShareMesh::new(false);
    m.ctl[0].press(&mut m.hosts[0], Press::Call);
    m.until("Ben's incoming call", |m| m.ctl[1].view().ringing());
    m.ctl[1].press(&mut m.hosts[1], Press::Answer);
    m.until("Ana and Ben exchanging voice", |m| [(0,1),(1,0)].iter().all(|p| m.carriers[p].voice_seq.is_some()));
    m.add_third();
    let (s, _) = m.offer();
    m.watch(2,s);
    m.until("Kit's first share picture", |m| m.pts(2).is_some());
    let before = [(0,1),(1,0)].map(|p| m.carriers[&p].voice_seq);
    m.until("both voice streams advancing while Kit watches", |m|
        [(0,1),(1,0)].iter().enumerate().all(|(i,p)| m.carriers[p].voice_seq > before[i]));
    assert!(!m.ctl[0].flow.is_voice_peer(&m.key(2)), "the watched link is joined with voice disabled for Kit");
    assert_eq!(m.carriers[&(0,2)].voice_seq, None, "Ana has sent zero voice datagrams on Kit's video link");
    assert_eq!(m.ctl[2].view().phase, Phase::Idle, "Kit's watch never opens a voice call");
}

#[cfg(unix)]
impl ShareMesh {
    fn hevc_window(&mut self, title: Option<&str>) -> SessionId {
        self.ctl[0].sharing.encoder = EncoderFormats { hevc: true, full_chroma: false };
        for ctl in &mut self.ctl[1..] {
            ctl.sharing.decode_caps = MediaCaps(MediaCaps::H264_DECODE | MediaCaps::HEVC_DECODE);
        }
        self.app_window_named(title).0
    }

    fn line(&self, session: SessionId) -> &str {
        self.ctl[0].sharing.shown.offer(session).unwrap().sharer_line.as_deref().unwrap()
    }
}

#[cfg(unix)]
#[test]
fn a_watchers_caps_follow_its_decoder_not_its_os() {
    use makepad_call::driver::codec::StreamFormat as F;
    use makepad_widgets::makepad_platform::log_ring;
    Cx::init_log();
    let mut m = ShareMesh::new(true);
    m.ctl[0].sharing.encoder = EncoderFormats { hevc: true, full_chroma: false };
    let watcher = m.key(1);
    m.ctl[0].flow.set_peer_name(watcher, "Mira".into());
    let (s, _) = m.app_window_named(Some("Decoder negotiation"));
    let h264 = MediaCaps(MediaCaps::H264_DECODE);
    let both = MediaCaps(MediaCaps::H264_DECODE | MediaCaps::HEVC_DECODE);
    let cursor = log_ring::read_since(0, 0).newest;
    m.watch(1, s);
    m.until("Mira's H.264 picture and the line naming Mira as the holder", |m| m.pts(1).is_some()
        && m.line(s).ends_with("H.264 · 4:2:0: Mira can't decode H.265 · 4:2:0."));
    let viewer = &m.ctl[0].sharing.locals[&s].viewers[&watcher];
    assert_eq!(viewer.caps, h264, "the initial View advertises only H.264");
    let geometry = viewer.view;
    let mut opens = m.rig.as_ref().unwrap().video_formats();
    assert_eq!(opens.last(), Some(&F::H264), "the initial watcher receives an H.264 stream");
    m.ctl[1].apply_decode_caps(both);
    opens.push(F::HEVC);
    m.until("the sharer to reopen in H.265 and update its line", |m|
        m.rig.as_ref().unwrap().video_formats() == opens && m.line(s).ends_with("H.265 · 4:2:0."));
    let viewer = &m.ctl[0].sharing.locals[&s].viewers[&watcher];
    assert_eq!(viewer.caps, both, "the re-sent View adds the probed H.265 decoder");
    assert_eq!(viewer.view, geometry, "a decoder update preserves the open View's geometry");
    log!("test mark after Mira's decoder probe");
    m.until("Mira's log mark", |_| log_ring::read_since(cursor, log_ring::CAP).lines.iter()
        .any(|line| line.text.ends_with("test mark after Mira's decoder probe")));
    let lines = log_ring::read_since(cursor, log_ring::CAP).lines;
    let lists: Vec<_> = lines.iter().filter_map(|line| line.text.split_once("Share Decoder negotiation: Mira decodes "))
        .map(|(_, list)| list).collect();
    assert_eq!(lists, ["H.264 · 4:2:0", "H.264 · 4:2:0, H.265 · 4:2:0"],
        "Mira's decoder log shows her initial formats and then her probe result once each");

    let kit = m.key(2);
    m.watch(2, s);
    m.ctl[2].share_decoder_limit(F::HEVC, None);
    m.ctl[2].apply_decode_caps(both);
    assert_eq!(m.ctl[2].sharing.decode_caps, h264, "a late probe cannot restore Kit's failed H.265 decoder");
    m.until("Kit's H.264-only View to reach the sharer", |m|
        m.ctl[0].sharing.locals[&s].viewers.get(&kit).is_some_and(|v| v.caps == h264));
}

#[cfg(unix)]
#[test]
fn linked_share_carries_the_apps_sound_without_a_call() {
    use makepad_widgets::makepad_platform::log_ring;
    Cx::init_log();
    let mut m = ShareMesh::new(false);
    let (s, link) = m.app_window();
    let sharer = m.key(0);
    m.until("the unwatched window offer", |m| m.ctl[1].sharing.shown.offer(s).is_some());
    m.rig.as_mut().unwrap().barrier(link);
    assert_eq!(m.rig.as_ref().unwrap().audio_open_count(), 0, "offering a window leaves app sound closed");

    let watch_started = Instant::now();
    m.watch(1, s);
    m.until("the linked picture and the app's audio channel", |m|
        m.pts(1).is_some() && m.rig.as_ref().unwrap().audio_channels_open() == 1);
    let pid = m.ctl[1].links[&sharer].peer_id;
    m.rig.as_mut().unwrap().barrier(link);
    let send = |m: &ShareMesh, i: u64| m.rig.as_ref().unwrap().send_sound(i * 480, 480, 1_000_000_000 + i * 10_000_000);
    let mut record = 0;
    let mut next_record = Instant::now();
    let sound = loop {
        m.pump();
        if let Some(sound) = m.ctl[1].current.as_ref().and_then(|c| c.share_sound(pid)) {
            if sound.counts().frames > 0 { break sound.clone() }
        }
        assert!(watch_started.elapsed() < Duration::from_secs(15),
            "waiting for app sound: source {:?}, sharer {:?}, watcher {:?}",
            m.ctl[0].sharing.locals[&s].sound.as_ref().map(|s| s.stats()),
            m.ctl[0].current.as_ref().map(Call::stats), m.ctl[1].current.as_ref().map(Call::stats));
        // Frames captured during probing expire. Keep the app sounding at its 10 ms cadence.
        if Instant::now() >= next_record {
            send(&m, record);
            record += 1;
            next_record += Duration::from_millis(10);
        }
        thread::yield_now();
    };
    eprintln!("Town share: first app sound frame {} ms after Watch", watch_started.elapsed().as_millis());
    let first = sound.counts().frames;
    let next_capture_us = 1_000_000 + record * 10_000;
    for i in record..record + 4 { send(&m, i); }
    m.until("more app sound on the same listener", |_| {
        let counts = sound.counts();
        counts.frames > first && counts.last_capture_us >= next_capture_us
    });
    assert_eq!(m.rig.as_ref().unwrap().audio_open_count(), 1, "the share and encoder use one app audio OPEN");
    for ctl in &m.ctl[..2] {
        assert!(!ctl.voice_attached, "neither seat attaches voice to carry app sound");
        assert_eq!(ctl.view().phase, Phase::Idle, "both seats stay idle while app sound streams");
    }

    let off = format!("Share {}: app sound off", m.ctl[0].sharing.shown.offer(s).unwrap().title);
    let cursor = log_ring::read_since(0, 0).newest;
    assert!(m.ctl[1].call_actions(&mut m.hosts[1], CallAction::OnShare(s, Press::CloseWatch)), "the watcher closes the share");
    assert!(m.ctl[1].current.as_ref().and_then(|c| c.share_sound(pid)).is_none(), "Close watch releases the listener");
    m.until("the last watcher to close the app audio channel", |m| m.rig.as_ref().unwrap().audio_channels_open() == 0);
    assert!(log_ring::read_since(cursor, log_ring::CAP).lines.iter().any(|line| line.text.ends_with(&off)),
        "the sharer reports when app sound stops");
}

#[cfg(unix)]
#[test]
fn a_watcher_with_no_call_opens_the_speaker_only_while_sound_plays() {
    #[derive(Default)]
    struct RecordingHost {
        outputs: Vec<Vec<AudioDeviceId>>,
        share: Option<ShareSpeaker>,
    }

    impl AudioHost for RecordingHost {
        fn install(&mut self, _: MicPort, _: OutputPort, share: ShareSpeaker) { self.share = Some(share); }
        fn use_inputs(&mut self, ids: &[AudioDeviceId]) { assert!(ids.is_empty(), "watching never opens a microphone"); }
        fn use_outputs(&mut self, ids: &[AudioDeviceId]) { self.outputs.push(ids.to_vec()); }
    }

    impl RecordingHost {
        fn render_block(&mut self) -> bool {
            let mut buffer = [0.0; 960];
            self.share.as_mut().expect("the output has its share callback").add_planar(48_000.0, 480, 2, &mut buffer);
            buffer.iter().any(|&sample| sample != 0.0)
        }
    }

    Cx::init_log();
    let mut m = ShareMesh::<RecordingHost>::with_hosts(false, Default::default(), |_| VoiceSource::Feed);
    let output = AudioDeviceId(LiveId::from_str("watcher_speaker"));
    m.ctl[1].audio_devices(&mut m.hosts[1], &[AudioDeviceDesc { device_id: output, device_type: AudioDeviceType::Output,
        name: "Watcher speaker".into(), is_default: true, has_failed: false, channel_count: 2 }]);
    let (s, link) = m.app_window();
    m.watch(1, s);
    m.until("the watched app to open its sound channel", |m| m.rig.as_ref().unwrap().audio_channels_open() == 1);
    m.rig.as_mut().unwrap().barrier(link);
    let pid = m.ctl[1].links[&m.key(0)].peer_id;
    let tick = Duration::from_millis(10);
    let mut record = 0;
    let mut next_block = Instant::now();
    let mut send_block = |m: &ShareMesh<RecordingHost>| {
        m.rig.as_ref().unwrap().send_sound(record * 480, 480, 1_000_000_000 + record * 10_000_000);
        record += 1;
    };
    m.until("the watcher's first app sound frame", |m| {
        if m.ctl[1].current.as_ref().and_then(|c| c.share_sound(pid)).is_some_and(|s| s.counts().frames > 0) { return true }
        assert!(m.hosts[1].outputs.iter().all(Vec::is_empty), "the output stays closed until sound arrives");
        if Instant::now() >= next_block {
            send_block(m);
            next_block += tick;
        }
        false
    });
    assert!(m.ctl[1].sharing.wants_speaker(), "the first received sound requests playback");
    assert_eq!(m.hosts[1].outputs.last().map(Vec::as_slice), Some([output].as_slice()), "the first sound opens the chosen output");
    let start = Instant::now();
    loop {
        m.pump();
        assert!(start.elapsed() < Duration::from_secs(1), "the device renders audible app sound within one second");
        if Instant::now() >= next_block {
            send_block(&m);
            next_block += tick;
            if m.hosts[1].render_block() { break }
        }
        thread::yield_now();
    }
    assert!(m.ctl[1].sharing.speaker.level() > 0.0, "the share meter registers the audible device block");
    for ctl in &m.ctl[..2] {
        assert!(!ctl.want_output, "sharing leaves the call's output request off");
        assert_eq!(ctl.view().phase, Phase::Idle, "each seat stays idle during share playback");
    }
    let before_close = m.hosts[1].outputs.len();
    assert!(m.ctl[1].call_actions(&mut m.hosts[1], CallAction::OnShare(s, Press::CloseWatch)), "the watcher closes the share");
    m.pump();
    assert_eq!(&m.hosts[1].outputs[before_close..], &[Vec::<AudioDeviceId>::new()], "Close watch closes the share-only output");
    assert!(!m.ctl[1].sharing.wants_speaker(), "a closed watch releases its speaker");
    assert!((0..3).any(|_| !m.hosts[1].render_block()), "the speaker renders silence within a few blocks of Close watch");
    assert_eq!(m.ctl[1].sharing.speaker.level(), 0.0, "the share meter returns to zero on the silent device block");
}

#[cfg(unix)]
#[test]
fn an_old_watcher_gets_no_sound_and_opens_no_audio() {
    let mut m = ShareMesh::new(false);
    let (sharer, watcher, legacy) = (m.key(0), m.key(1), [3; 65]);
    let port = m.ctl[0].link_up(&mut m.hosts[0], sharer, legacy, "Bo").unwrap();
    m.ctl[0].set_town_peers(&mut m.hosts[0], [(MESH_TOWN, [watcher, legacy].into())].into());
    m.ctl[0].set_peer_caps(legacy, SHARE_CAPS);
    let (s, link) = m.app_window();
    let old_view = Message::View { session: s, width: 320, height: 180, fps: 30, ext: Default::default() };
    port.messages_in.try_send(old_view.encode()).unwrap();
    m.until("the legacy viewer's capless View", |m| m.ctl[0].is_viewer(s, &legacy));
    m.rig.as_mut().unwrap().barrier(link);
    assert_eq!(m.rig.as_ref().unwrap().audio_open_count(), 0, "omitting audio from the View leaves the app channel closed");

    m.ctl[1].sharing.sound_on = false;
    m.watch(1, s);
    m.until("the capable watcher to receive video with sound off", |m| m.pts(1).is_some());
    m.rig.as_mut().unwrap().barrier(link);
    assert_eq!(m.rig.as_ref().unwrap().audio_open_count(), 0, "both silent viewers share video without opening app audio");
    let pid = m.ctl[1].links[&sharer].peer_id;
    assert!(m.ctl[1].current.as_ref().unwrap().share_sound(pid).is_none(), "the muted watcher receives no sound");
}

#[cfg(unix)]
#[test]
fn a_watcher_who_decodes_less_downgrades_the_share_and_its_leaving_upgrades_it() {
    use makepad_call::driver::codec::StreamFormat as F;
    use makepad_widgets::makepad_platform::log_ring;
    Cx::init_log();
    let mut m = ShareMesh::new(false);
    m.link(0, 2, true);
    // The older client only needs a reliable port: its View has no capability extension.
    let (ana, ben, kit) = (m.key(0), m.key(1), m.key(2));
    let legacy = [3; 65];
    let port = m.ctl[0].link_up(&mut m.hosts[0], ana, legacy, "Bo").unwrap();
    m.ctl[0].set_town_peers(&mut m.hosts[0], [(MESH_TOWN, [ben, kit, legacy].into())].into());
    m.ctl[0].set_peer_caps(legacy, SHARE_CAPS);
    let s = m.hevc_window(None);
    m.watch(1, s);
    m.until("Ben's HEVC picture", |m| m.pts(1).is_some() && m.line(s).ends_with("H.265 · 4:2:0."));
    assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC], "one HEVC OPEN serves Ben");

    let old_view = Message::View { session: s, width: 320, height: 180, fps: 30, ext: Default::default() };
    let cursor = log_ring::read_since(0, 0).newest;
    let title = m.ctl[0].sharing.shown.offer(s).unwrap().title.clone();
    let legacy_line = format!("Share {title}: Bo's View lists no decoders (an older Town); sending H.264 · 4:2:0");
    port.messages_in.try_send(old_view.encode()).unwrap();
    m.until("Bo's capless View to lower the stream", |m| m.line(s).ends_with("H.264 · 4:2:0: Bo can't decode H.265 · 4:2:0."));
    let downgrades = || log_ring::read_since(cursor, log_ring::CAP).lines.iter()
        .filter(|line| line.text.ends_with("H.264 · 4:2:0, Bo can't decode H.265 · 4:2:0")).count();
    m.until("the downgrade log naming Bo", |_| downgrades() > 0);
    assert_eq!(downgrades(), 1, "the downgrade log names the limiting watcher once");
    assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC, F::H264], "Bo causes one H.264 reopen");
    let before = m.pts(1);
    m.rig.as_ref().unwrap().present(3);
    m.until("Ben's next picture after the downgrade", |m| m.pts(1) > before);

    // Bo's seat keeps its last known caps: a repeated View, a resized capless View and a
    // closed View that reopens at once all leave the stream at H.264.
    let upgrades = || log_ring::read_since(cursor, log_ring::CAP).lines.iter()
        .filter(|line| line.text.ends_with(": H.265 · 4:2:0")).count();
    let close = Message::View { session: s, width: 0, height: 0, fps: 0, ext: Default::default() };
    let resized = |width, height| Message::View { session: s, width, height, fps: 30, ext: Default::default() };
    for (step, messages, width) in [
        ("a repeated capless View", vec![old_view.encode(), resized(400, 225).encode()], 400),
        ("a closed View reopened at once", vec![close.encode(), resized(640, 360).encode()], 640),
    ] {
        for bytes in messages { port.messages_in.try_send(bytes).unwrap(); }
        m.until(step, |m| m.ctl[0].sharing.locals[&s].viewers.get(&legacy).is_some_and(|v| v.view.width == width));
        // The log ring is ordered: once this mark shows, any format line before it shows too.
        let mark = format!("test mark after {step}");
        log!("{mark}");
        m.until("the mark", |_| log_ring::read_since(cursor, log_ring::CAP).lines.iter().any(|line| line.text.ends_with(&mark)));
        assert_eq!(upgrades(), 0, "{step} never lifts the stream to H.265");
        assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC, F::H264], "{step} reopens nothing");
    }
    let explicit_empty = Message::View { session: s, width: 640, height: 360, fps: 30,
        ext: crate::call::ShareWatchExt { media_caps: Some(MediaCaps::NONE), ..Default::default() } };
    port.messages_in.try_send(explicit_empty.encode()).unwrap();
    let empty_line = format!("Share {title}: Bo decodes nothing");
    m.until("Bo's first explicit decoder list after his older Views", |_| log_ring::read_since(cursor, log_ring::CAP)
        .lines.iter().any(|line| line.text.ends_with(&empty_line)));
    port.messages_in.try_send(close.encode()).unwrap();
    m.until("Bo's closed View", |m| !m.ctl[0].sharing.locals[&s].viewers.contains_key(&legacy));
    assert!(m.line(s).ends_with("H.264 · 4:2:0: Bo can't decode H.265 · 4:2:0."),
        "a closed seat still holds the stream while Bo receives the share: {}", m.line(s));
    assert_eq!(upgrades(), 0, "a closed seat never lifts the stream to H.265");
    let legacy_logs = log_ring::read_since(cursor, log_ring::CAP).lines.iter().filter(|line| line.text.ends_with(&legacy_line)).count();
    assert_eq!(legacy_logs, 1, "Bo's repeated Views log the older Town decoder fallback exactly once");

    m.ctl[0].link_down(&mut m.hosts[0], legacy);
    m.until("HEVC to resume when Bo leaves", |m| m.line(s).ends_with("H.265 · 4:2:0."));
    assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC, F::H264, F::HEVC], "Bo leaving causes one HEVC reopen");
    m.watch(2, s);
    m.until("the new HEVC watcher to decode on the open stream", |m| m.pts(2).is_some());
    assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC, F::H264, F::HEVC], "another capable seat causes no reopen");
}

#[cfg(unix)]
#[test]
fn a_watcher_that_cannot_decode_the_format_falls_back_and_the_share_follows() {
    use makepad_call::{driver::codec::{Decoder, StreamFormat as F}, guard::Rung};
    use makepad_widgets::makepad_platform::log_ring;
    Cx::init_log();
    struct NoHevc;
    impl Codecs for NoHevc {
        fn encoder(&self, rung: Rung, kbps: u32) -> Result<Box<dyn Encoder>, String> { TestCodecs.encoder(rung, kbps) }
        fn key_format(&self, key: &[u8]) -> Option<F> { TestCodecs.key_format(key) }
        fn decoder(&self, key: &[u8]) -> Result<Box<dyn Decoder>, String> {
            if self.key_format(key) == Some(F::HEVC) { return Err("no HEVC decoder".into()) }
            TestCodecs.decoder(key)
        }
    }
    let mut m = ShareMesh::new(false);
    m.ctl[1].codecs = Arc::new(NoHevc);
    let s = m.hevc_window(Some("Decoder fallback"));
    let cursor = log_ring::read_since(0, 0).newest;
    m.watch(1, s);

    // The sharer cannot act on the revised View until its next poll.
    m.until("Ben to withdraw HEVC", |m| !m.ctl[1].sharing.decode_caps.supports(MediaCaps::HEVC_DECODE));
    let waiting = m.ctl[1].shown().share;
    assert!(waiting.picture.is_none(), "the recovery key has not reached the watcher yet");
    assert_eq!(waiting.watched().unwrap().decode_problem, None, "a recoverable HEVC failure stays off the tile");
    assert_eq!(waiting.single_waiting_line(), Some("Waiting for Ana's picture…"), "the tile waits for the fallback key");

    m.until("Ben to decode the recovery key without another input", |m| {
        let shown = &m.ctl[1].sharing.shown;
        assert!(shown.watched().unwrap().decode_problem.is_none(), "the tile stays clear while the fallback key is in flight");
        shown.picture.is_some()
    });
    assert_eq!(m.ctl[1].current.as_ref().unwrap().stats().decoder, "test codec H.264 · 4:2:0",
        "the recovered picture came from the H.264 decoder");
    let viewer = &m.ctl[0].sharing.locals[&s].viewers[&m.key(1)];
    assert_eq!(viewer.caps, MediaCaps(MediaCaps::H264_DECODE), "the re-sent View withdrew only HEVC");
    assert_eq!(viewer.view, m.ctl[1].share_subscription(), "the re-sent View kept its geometry");
    assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC, F::H264], "a caps-only change reopens the capture once");

    let title = &m.ctl[0].sharing.shown.offer(s).unwrap().title;
    let sharer_log = format!("Share {title}: H.264 · 4:2:0, Ben can't decode H.265 · 4:2:0");
    let watcher_log = format!("Share {title}: this computer can't decode H.265 · 4:2:0; asking for another format");
    let counts = || {
        let lines = log_ring::read_since(cursor, log_ring::CAP).lines;
        [&watcher_log, &sharer_log].map(|msg| lines.iter().filter(|l| l.text.ends_with(msg)).count())
    };
    m.until("both seats to log the fallback", |_| counts().iter().all(|&n| n > 0));
    assert_eq!(counts(), [1, 1], "the watcher logs one fallback request and the sharer logs one H.264 switch");
}

#[cfg(unix)]
#[test]
fn a_watcher_that_cannot_keep_up_steps_the_share_down() {
    use makepad_call::driver::codec::StreamFormat as F;
    use makepad_widgets::makepad_platform::log_ring;
    // The fake application's present advances its clock by a second. Send its real
    // encoded packets on a 1 ms capture clock so the two-millisecond decoder falls behind.
    struct FastWindow(Lease);
    impl EncodedSource for FastWindow {
        fn pop(&mut self) -> Option<Packet> {
            let mut packet = self.0.pop()?;
            packet.pts_100ns /= 1_000;
            Some(packet)
        }
        fn want(&mut self, w: u32, h: u32, fps: u32) { self.0.want(w, h, fps); }
        fn set_kbps(&mut self, kbps: u32) { self.0.set_kbps(kbps); }
        fn key_now(&mut self) { self.0.key_now(); }
        fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) { self.0.set_wake(wake); }
        fn frame_counts(&self) -> Option<(u64, u64)> { self.0.frame_counts() }
    }

    Cx::init_log();
    let mut m = ShareMesh::new(false);
    m.ctl[1].codecs = Arc::new(TestCodecs::with_decode_delay(Duration::from_millis(2)));
    let s = m.hevc_window(Some("Slow decoder"));
    let source = &mut m.ctl[0].sharing.locals.get_mut(&s).unwrap().source;
    *source = OfferSource::new(source.caps, Box::new(FastWindow(source.lease())));
    let cursor = log_ring::read_since(0, 0).newest;
    m.watch(1, s);
    m.until("the first H.265 picture", |m| m.pts(1).is_some() && m.line(s).ends_with("H.265 · 4:2:0."));
    let geometry = m.ctl[1].share_subscription();
    let check_tile = |m: &ShareMesh| {
        let shown = &m.ctl[1].shown().share;
        assert!(shown.picture.is_some(), "the tile keeps displaying pictures during renegotiation");
        assert_eq!(shown.watched().unwrap().decode_problem, None, "a slow decoder shows no error");
        assert_eq!(shown.single_waiting_line(), None, "a slow decoder shows no waiting line");
    };
    for _ in 0..80 {
        let before = m.pts(1);
        m.rig.as_ref().unwrap().present(3);
        m.until("the next picture from the slow stream", |m| { check_tile(m); m.pts(1) > before });
        if !m.ctl[1].sharing.decode_caps.supports(MediaCaps::HEVC_DECODE) { break }
    }
    m.until("the watcher to decode the reopened H.264 share", |m| {
        check_tile(m);
        m.ctl[1].current.as_ref().unwrap().stats().decoder == "test codec H.264 · 4:2:0"
            && m.line(s).ends_with("H.264 · 4:2:0: Ben can't decode H.265 · 4:2:0.")
    });
    let viewer = &m.ctl[0].sharing.locals[&s].viewers[&m.key(1)];
    assert_eq!(viewer.caps, MediaCaps(MediaCaps::H264_DECODE), "the View withdraws the slow H.265 decoder");
    assert_eq!(viewer.view, geometry, "the slowdown preserves the View geometry");
    assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC, F::H264], "the slowdown reopens the app exactly once");
    let reports = log_ring::read_since(cursor, log_ring::CAP).lines.into_iter().filter(|l|
        l.text.contains("Share: this computer can't keep up with H.265 · 4:2:0 (decode ")
        && l.text.ends_with(" ms); asking for another format")).count();
    assert_eq!(reports, 1, "the watcher logs its measured slowdown exactly once");
}

#[test]
fn a_decoder_loss_before_layout_is_remembered_by_the_next_view() {
    use makepad_call::driver::codec::StreamFormat as F;
    let mut m = ShareMesh::new(false);
    let (s, _) = m.offer();
    let remaining = MediaCaps(MediaCaps::H264_DECODE | MediaCaps::HEVC_DECODE);
    m.ctl[1].sharing.decode_caps = MediaCaps(remaining.0 | MediaCaps::HEVC_444_DECODE);
    m.until("Ben to receive the offer", |m| m.ctl[1].sharing.shown.offer(s).is_some());
    let watcher = &mut m.ctl[1];
    watcher.call_opening_presses(&mut m.hosts[1], CallAction::OnShare(s, Press::Watch));
    assert_eq!(watcher.share_subscription(), View::default(), "the watch has no View until its panel is measured");
    let sent = watcher.messages_sent();
    watcher.share_decoder_limit(F::HEVC_FULL, None);
    assert_eq!(watcher.sharing.decode_caps, remaining, "a closed View remembers the failed decoder");
    assert_eq!(watcher.messages_sent(), sent, "the decoder loss sends no View before layout");
    watcher.share_panel_size(s, ShareSurface::Page, 320, 180);
    let ben = m.key(1);
    m.until("the sharer to receive the first measured View", |m| m.ctl[0].is_viewer(s, &ben));
    assert_eq!(m.ctl[0].sharing.locals[&s].viewers[&ben].caps, remaining, "the first View advertises the reduced caps");
}

#[test]
fn a_failed_baseline_key_has_a_reason_even_without_a_second_problem_event() {
    let mut watcher = CallController::default();
    let session = [1; 16];
    watcher.begin_offer(session, MESH_TOWN, "Canvas".into(), ShareRole::Viewer { sharer: [2; 65] }, (24., 16.));
    watcher.watch(session);
    watcher.share_decoder_limit(StreamFormat::H264, None);
    assert_eq!(watcher.sharing.shown.watched().unwrap().decode_problem.as_deref(), Some("This computer can't decode H.264 · 4:2:0."),
        "a failed fallback key explains why no picture can be shown");
    watcher.sharing.shown.offer_mut(session).unwrap().decode_problem = Some("decoder unavailable".into());
    watcher.share_decoder_limit(StreamFormat::H264, None);
    assert_eq!(watcher.sharing.shown.watched().unwrap().decode_problem.as_deref(), Some("decoder unavailable"),
        "the decoder's own terminal reason takes precedence");
}

#[cfg(unix)]
#[test]
fn an_app_that_cannot_encode_the_format_falls_back_without_stopping() {
    use makepad_call::driver::codec::StreamFormat as F;
    let mut m = ShareMesh::new(false);
    let s = m.hevc_window(None);
    m.rig.as_ref().unwrap().refuse_format(F::HEVC);
    m.watch(1, s);
    m.until("the fallback picture and the app's reason", |m| m.pts(1).is_some() &&
        m.line(s).ends_with("H.264 · 4:2:0: app couldn't encode H.265 · 4:2:0."));
    assert_eq!(m.rig.as_ref().unwrap().video_formats(), [F::HEVC, F::H264], "the refused format is never retried");
    assert!(m.ctl[0].sharing.shown.offer(s).unwrap().stopped_line.is_none(), "recovering from the refusal keeps the offer live");
}

#[cfg(unix)]
#[test]
fn decoder_open_failure_surfaces_in_preview_and_watched_share_ui() {
    use makepad_call::{driver::codec::Decoder, guard::Rung};
    const WHY: &str = "low-latency H.264 decode requires GStreamer's avdec_h264 element (install gstreamer1.0-libav)";
    struct MissingDecoder;
    impl Codecs for MissingDecoder {
        fn encoder(&self, rung: Rung, kbps: u32) -> Result<Box<dyn Encoder>, String> { TestCodecs.encoder(rung, kbps) }
        fn decoder(&self, _: &[u8]) -> Result<Box<dyn Decoder>, String> { Err(WHY.into()) }
    }

    let mut m = ShareMesh::new(false);
    m.ctl[1].codecs = Arc::new(MissingDecoder);
    let (session, _) = m.app_window();
    let rig = m.rig.as_mut().unwrap();
    rig.set_preview_codecs(Arc::new(MissingDecoder));
    m.ctl[0].share_panel_size(session, ShareSurface::Page, 320, 180);
    m.ctl[0].sync_share_preview(&mut rig.app, Some(session));
    let probe = m.ctl[0].sharing.locals[&session].preview.as_ref().expect("the page opened a preview").0.probe();
    rig.wait(|_| probe.ended_why().is_some());
    m.ctl[0].poll_share();
    let preview_note = format!("Preview couldn't decode: {WHY}");
    let sharer = m.ctl[0].shown().share;
    assert_eq!(sharer.single_waiting_line(), Some(preview_note.as_str()), "the preview panel shows the decoder-open error");
    let offer = sharer.offer(session).unwrap();
    assert!(offer.sharer_line.as_deref().unwrap().contains(&preview_note), "the sharer's status shows the same error");
    assert!(offer.stopped_line.is_none(), "a local preview failure leaves the encoded share live");
    m.ctl[0].sync_share_preview(&mut rig.app, None);
    m.ctl[0].poll_share();
    assert!(!m.ctl[0].shown().share.offer(session).unwrap().sharer_line.as_deref().unwrap().contains(&preview_note),
        "closing the preview clears its error from the sharer's status");

    while m.ctl[1].sharing.shown.offer(session).is_none() { m.pump(); thread::yield_now(); }
    assert!(m.ctl[1].call_opening_presses(&mut m.hosts[1], CallAction::OnShare(session, Press::Watch)), "the viewer opens the share");
    m.ctl[1].share_panel_size(session, ShareSurface::Page, 320, 180);
    while m.ctl[1].sharing.shown.watched().unwrap().decode_problem.is_none() { m.pump(); thread::yield_now(); }
    let viewer = m.ctl[1].shown().share;
    let message = format!("this person's video couldn't be decoded: {WHY}");
    assert_eq!(viewer.single_waiting_line(), Some(message.as_str()), "the watched panel shows the decoder's exact error");
    assert!(viewer.watched().unwrap().stopped_line.is_none(), "the viewer can recover on a later key frame");
    m.ctl[1].share_press(Press::CloseWatch);
    let closed = m.ctl[1].shown().share;
    assert!(closed.watched().is_none() && closed.offer(session).unwrap().decode_problem.is_none(),
        "closing Watch clears the error on the surviving offer");
}

#[cfg(unix)]
impl CallController {
    pub(crate) fn linked_tweak_test_mesh() -> ShareMesh { ShareMesh::new(true) }
}

#[cfg(unix)]
impl<H: AudioHost> ShareMesh<H> {
    pub(crate) fn app_window(&mut self) -> (SessionId, LinkId) {
        self.app_window_named(None)
    }

    fn app_window_named(&mut self, title: Option<&str>) -> (SessionId, LinkId) {
        let mut rig = crate::app_link::RecordingRig::new();
        rig.app.scan();
        rig.wait(|r| !r.app.apps().is_empty());
        let link = rig.app.link(&rig.app.apps()[0].key.clone());
        rig.live(link);
        let window = rig.app.state(link).unwrap().windows[0].id;
        match title {
            Some(title) => self.ctl[0].open_named_share_window(&mut rig.app, link, window, title.into()),
            None => self.ctl[0].open_share_window(&mut rig.app, link, window),
        }.unwrap();
        self.rig = Some(rig);
        (self.ctl[0].only_local_offer().unwrap(), link)
    }

    pub(crate) fn grant_to(&mut self, s: SessionId, i: usize) -> crate::call::control::GrantId {
        let peer = self.key(i);
        assert!(self.ctl[0].allow_control(&mut self.rig.as_mut().unwrap().app, s, peer, NAMES[i]), "the sharer grants the selected viewer control");
        assert_eq!(self.ctl[0].sharing.shown.control.holder, Some((peer, NAMES[i].into())), "the grant names the selected viewer");
        let grant = self.ctl[0].sharing.shown.control.active.unwrap();
        self.until("the selected viewer to receive its grant", |m| m.ctl[i].sharing.shown.control.active == Some(grant));
        grant
    }
}

#[cfg(unix)]
#[test]
fn step4_local_share_excludes_step3_peer_and_binds_let_control_on_live_share_page() {
    use crate::{call::ShareOfferExt, lists::{Live, Page, ViewData}};
    use makepad_widgets::*;

    let mut m = ShareMesh::new(true);
    let (ana, ben, kit) = (m.key(0), m.key(1), m.key(2));
    for i in [0, 1] { m.ctl[i].set_peer_caps(kit, MediaCaps(MediaCaps::MULTIPARTY_V1)); }
    let (session, link) = m.app_window();
    let app = &m.rig.as_ref().unwrap().app;
    let window = app.state(link).unwrap().windows[0].id;
    let title = crate::record::capture::window_label(app, link, window).expect("the shared window has a title");
    assert_eq!(m.ctl[0].take_facts(), [Fact::ShareOpen { session, title: title.clone(), town: MESH_TOWN }],
        "starting a local share records its session, window title and town");

    m.barrier(0, 1);
    let offer = m.ctl[1].sharing.shown.offer(session).expect("Ben receives the window offer before the next reliable message");
    assert_eq!((offer.town, offer.role, &offer.title), (MESH_TOWN, ShareRole::Viewer { sharer: ana }, &title),
        "Ben sees Ana's window in the shared town");
    m.barrier(0, 2);
    assert!(m.ctl[2].sharing.shown.offers.is_empty(), "Kit receives no offer over the voice-only link");

    for (id, town) in [([31; 16], None), ([32; 16], Some(MESH_TOWN))] {
        m.ctl[2].send_to(ana, Message::Share { session: id, title: "Drawing".into(), w: 320., h: 180.,
            ext: ShareOfferExt { town, ..Default::default() } }.encode());
    }
    m.barrier(2, 0);
    assert_eq!(m.ctl[0].sharing.shown.offers.iter().map(|o| o.session).collect::<Vec<_>>(), [session],
        "neither the townless nor the town-tagged offer from Kit reaches Ana");

    m.watch(1, session);
    assert!(m.ctl[1].ask_control(session, true), "Ben can request control after opening Watch");
    m.until("Ben's watch and named control request to reach Ana", |m| {
        let shown = &m.ctl[0].sharing.shown;
        shown.offer(session).is_some_and(|o| o.watchers.iter().any(|(p, _)| *p == ben))
            && shown.control.requests.iter().any(|(s, p, name)| *s == session && *p == ben && name == "Ben")
    });

    let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let ui = cx.with_vm(|vm| {
        let root = script_eval!(vm, { use mod.prelude.widgets.* use mod.widgets.* View{live_share := LiveSharePage{}} });
        WidgetRef::script_from_value(vm, root)
    });
    let mut data = ViewData { call: m.ctl[0].shown(), page: Page::Live(Live::Share(session)), ..Default::default() };
    data.sync_live_share(&mut cx, &ui);
    let allow = ui.button(&cx, ids!(live_share.let_control));
    assert!(allow.visible(), "the sharer's page shows the pending control request");
    assert_eq!(allow.text(), "Allow", "the pending request uses the Allow button");
    let press = CallAction::OnShare(session, Press::LetControl(ben));
    assert_eq!(allow.action_data().as_ref().and_then(|a| a.downcast_ref::<CallAction>()), Some(&press),
        "Allow binds both the requested session and Ben's key");
    assert!(!m.ctl[0].call_actions(&mut m.hosts[0], press), "ordinary actions cannot grant control");
    m.grant_to(session, 1);
}

#[cfg(unix)]
#[test]
fn takeover_grant_is_mutually_exclusive_and_drops_non_holder_input() {
    use crate::{app_link::{AppInput, Mods}, call::{control::{ControlMessage, PanelInput}, topic}};
    use super::super::takeover::Refusal;
    use makepad_widgets::KeyCode;
    let mut m = ShareMesh::new(true);
    let (s, link) = m.app_window();
    let (ana,ben,kit) = (m.key(0),m.key(1),m.key(2));
    for i in [1,2] {
        m.watch(i,s);
        assert!(m.ctl[i].ask_control(s,true), "viewer {i} requests control");
        m.until("the request and measured view", |m| m.ctl[0].is_viewer(s,&m.key(i))
            && m.ctl[0].sharing.shown.control.requests.iter().any(|(_,p,_)| *p == m.key(i)));
    }
    assert_eq!(m.ctl[0].sharing.shown.control.requests, [(s,ben,"Ben".into()),(s,kit,"Kit".into())], "requests preserve arrival order");
    let first = m.grant_to(s,1);
    let mods = Mods::default();
    let key = AppInput::KeyDown { key: KeyCode::KeyA, mods };
    let button = AppInput::PointerDown { x: 3., y: 4., button: 0, mods };
    for event in [key.clone(), button] { m.ctl[1].panel_input(&PanelInput { session: s, grant: first, event }); }
    m.until("the shared app to receive Ben's held key and button", |m| m.rig.as_ref().unwrap().input_lines().len() == 2);
    for message in [ControlMessage::Input { session: s, grant: first, event: key.clone() },
        ControlMessage::Release { session: s, grant: first }] {
        m.ctl[2].send_peer_to(ana, topic::INPUT, message.encode()).unwrap();
    }
    m.barrier(2,0);
    m.rig.as_mut().unwrap().barrier(link);
    assert_eq!(m.ctl[0].inputs_refused_for(Refusal::NoAllow), 1, "the holder's grant ID cannot authorize another sender's input");
    assert_eq!(m.ctl[0].inputs_refused_for(Refusal::Stale), 1, "another sender cannot release the holder's grant");
    assert_eq!(m.rig.as_ref().unwrap().input_lines().len(), 2, "Kit's ungranted input never reaches the app");
    let second = m.grant_to(s,2);
    assert_ne!(first, second, "handoff creates a fresh grant ID");
    m.until("Ben to receive TakenBack", |m| m.ctl[1].sharing.shown.control.notice.as_deref().is_some_and(|n| n.contains("took back")));
    m.ctl[1].send_peer_to(ana, topic::INPUT, ControlMessage::Input {session:s, grant:first, event:key}.encode()).unwrap();
    m.ctl[2].panel_input(&PanelInput {session:s, grant:second, event:AppInput::Text("new holder".into())});
    m.barrier(1,0);
    m.barrier(2,0);
    let rig = m.rig.as_mut().unwrap();
    rig.barrier(link);
    let lines: Vec<_> = rig.input_lines().iter().map(|line| makepad_remote::Req::parse(line)).collect();
    assert_eq!(lines.len(), 5, "only Ben's two inputs, two releases and Kit's text reach the app");
    assert!(lines[2..4].iter().all(|r| r.get("k") == Some("up"))
        && lines[2..4].iter().any(|r| r.cmd == "mouse") && lines[2..4].iter().any(|r| r.cmd == "key"),
        "the old holder's key and button are released before the new holder's input");
    assert_eq!(lines[4].get("t"), Some("new holder"), "Kit's input arrives after the old holder's releases");
    m.disconnect(0,2);
    let rig = m.rig.as_mut().unwrap();
    m.ctl[0].revoke_ended_shares(&mut rig.app);
    assert!(!rig.app.state(link).unwrap().input_allowed, "disconnect withdraws app input permission");
    assert_eq!(m.ctl[0].sharing.shown.control.notice.as_deref(), Some("Control ended when the link disconnected."), "the sharer sees why control ended");
    assert_eq!(m.ctl[0].sharing.shown.offer(s).unwrap().watchers, [(ben,"Ben".into())], "Ben's watch survives Kit's disconnect");
}

#[cfg(unix)]
#[test]
fn allow_all_second_watcher_does_not_steal_active_grant() {
    let mut m = ShareMesh::new(true);
    let (s, _) = m.app_window();
    assert!(m.ctl[0].allow_all_control(&mut m.rig.as_mut().unwrap().app, s), "local input enables all viewers' rights");
    m.watch(1,s);
    m.until("Ben's automatic grant", |m| m.ctl[1].sharing.shown.control.active.is_some());
    let grant = m.ctl[1].sharing.shown.control.active;
    m.watch(2,s);
    m.until("Kit to receive her right", |m| m.ctl[2].sharing.shown.control.allowed == Some(s));
    assert!(m.ctl[2].ask_control(s,true), "Kit can request the occupied control");
    m.until("Kit's pending request", |m| !m.ctl[0].sharing.shown.control.requests.is_empty());
    assert_eq!(m.ctl[0].sharing.shown.control.holder, Some((m.key(1),"Ben".into())), "the second watcher cannot take Ben's grant");
    assert_eq!(m.ctl[1].sharing.shown.control.active, grant, "Ben retains his uninterrupted grant");
    assert!(m.ctl[2].sharing.shown.control.active.is_none(), "Kit's right alone does not open input");
    assert_eq!(m.ctl[0].sharing.shown.control.requests, [(s,m.key(2),"Kit".into())], "Kit's request waits for explicit handoff");
}

#[test]
fn spoofed_control_from_non_sharer_is_ignored() {
    use crate::call::{control::{ControlMessage, EndReason}, topic};
    let mut m = ShareMesh::new(true);
    let (s, _) = m.offer();
    m.watch(1,s);
    m.until("Ben's first picture from Ana", |m| m.pts(1).is_some());
    let ben = m.key(1);
    let grant = [7;16];
    for msg in [ControlMessage::Control {session:s, grant, ended:None},
        ControlMessage::Control {session:s, grant, ended:Some(EndReason::TakenBack)},
        ControlMessage::Rights {session:s, allowed:true}] {
        m.ctl[2].send_peer_to(ben,topic::INPUT,msg.encode()).unwrap();
    }
    m.ctl[2].send_to(ben, ShareProblem::new(s,"capture stopped").encode());
    m.ctl[2].send_share_to(ben,Message::ShareOff { session:s, reason:ShareEndReason::Stopped });
    m.barrier(2,1);
    let shown = &m.ctl[1].sharing.shown;
    assert!(shown.control.active.is_none() && shown.control.allowed.is_none(), "only the authenticated sharer can set control or rights");
    assert!(shown.offer(s).is_some_and(|o| o.stopped_line.is_none()), "only the authenticated sharer can stop the offer or its picture");
    assert_eq!(shown.watching_sharer, Some(m.key(0)), "Ben is still watching Ana");
}
