//! A local recording keeps the window and destination selected by the person who started it.
#[path = "capture.rs"]
pub mod capture;

use crate::app_link::{AppLinkEvent, AppLinks, LinkId, Ticket, WindowId, StillSize};
use makepad_town_core::event::{Id, TownId, SessionId};
use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}, time::{Instant, SystemTime, UNIX_EPOCH}};

#[derive(Clone, Copy)]
pub enum RecordPress {
    Start { link: LinkId, window: WindowId },
    Own,
    Snap(capture::WindowTarget),
    Stop,
}

impl RecordPress {
    pub fn opening(self) -> bool { !matches!(self, Self::Stop) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Destination {
    pub town: TownId,
    pub project: Id,
    pub session: Option<SessionId>,
}

pub struct Recording {
    pub link: LinkId,
    pub window: WindowId,
    pub title: String,
    pub started: Instant,
    destination: Destination,
    start: Option<(Ticket, PathBuf)>,
    still: Option<Ticket>,
    poster: Option<PathBuf>,
    stop: Option<Ticket>,
}

pub struct RecordedClip {
    pub destination: Destination,
    pub path: PathBuf,
    pub title: String,
    pub duration_ms: u64,
    pub poster: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
}

struct LocalMovie {
    destination: Destination,
    started: Instant,
    file: Option<makepad_widgets::WindowRecording>,
    stopping: bool,
    stop_sent: bool,
    poster: Option<PathBuf>,
}

#[derive(Default)]
pub struct Recorder {
    pub recording: Option<Recording>,
    pub problem: Option<String>,
    finished: Vec<RecordedClip>,
    local: Option<LocalMovie>,
    snaps: Vec<(capture::WindowTarget, Destination)>,
    grabbing: HashMap<(LinkId, Ticket), (Destination, String)>,
    own_stills: Vec<capture::LocalStill>,
    /// The frame asked for while own stills wait, so one request is outstanding at a time.
    still_frame: Option<makepad_widgets::NextFrame>,
    pending_stills: HashSet<(LinkId, Ticket)>,
    unused_posters: Vec<PathBuf>,
    force_close: bool,
}

impl Recorder {
    pub fn start(&mut self, links: &AppLinks, link: LinkId, window: WindowId, destination: Destination) -> Result<(), String> {
        if self.running().is_some() { return Err("A recording is already in progress.".into()); }
        let title = capture::window_label(links, link, window).ok_or("The selected window is no longer visible.")?;
        self.recording = Some(Recording { link, window, title, started: Instant::now(), destination, start: None, stop: None, still: None, poster: None });
        self.problem = None;
        Ok(())
    }

    pub fn running_town(&self) -> Option<TownId> {
        self.recording.as_ref().map(|r| r.destination.town)
            .or_else(|| self.local.as_ref().map(|r| r.destination.town))
    }

    pub fn running(&self) -> Option<(Instant, &str)> {
        self.recording.as_ref().map(|r| (r.started, r.title.as_str()))
            .or_else(|| self.local.as_ref().map(|r| (r.started, "Town")))
    }

    pub fn capture_press(&mut self, links: &mut AppLinks, press: RecordPress, model: &crate::model::Model) -> Result<(), String> {
        if matches!(press, RecordPress::Stop) { self.stop(links); return Ok(()); }
        let destination = model.recording_destination()?;
        self.problem = None;
        match press {
            RecordPress::Start { link, window } => self.start(links, link, window, destination)?,
            RecordPress::Own => {
                if self.running().is_some() { return Err("A recording is already in progress.".into()); }
                self.local = Some(LocalMovie { destination, started: Instant::now(), file: None,
                    stopping: false, stop_sent: false, poster: None });
            }
            RecordPress::Snap(target) => self.snaps.push((target, destination)),
            RecordPress::Stop => unreachable!(),
        }
        Ok(())
    }

    /// If the storage worker is still opening the home, retain the original local press.
    pub fn folder_ready(&mut self, links: &mut AppLinks, folder: Option<Result<&Path, &str>>) {
        let Some(recording) = self.recording.as_mut().filter(|r| r.start.is_none()) else { return };
        match folder {
            None => {}
            Some(Ok(dir)) => { recording.start = Some((links.record_start(recording.link, recording.window, dir), dir.to_owned())); }
            Some(Err(why)) => {
                self.problem = Some(why.to_owned());
                self.recording = None;
            }
        }
    }

    pub fn stop(&mut self, links: &mut AppLinks) {
        if let Some(local) = &mut self.local { local.stopping = true; return; }
        let Some(r) = self.recording.as_mut() else { return };
        let Some((_, dir)) = &r.start else { self.recording = None; return };
        if r.stop.is_some() { return; }
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = dir.join(format!("poster-{}-{stamp}.jpg", std::process::id()));
        let ticket = links.still(r.link, r.window, &path, StillSize::Poster);
        self.pending_stills.insert((r.link, ticket));
        r.still = Some(ticket);
        r.stop = Some(links.record_stop(r.link));
    }

    /// Closing the window and shutting down can both ask for the same final movie.
    /// Only the first request needs another UI turn to flush the link commands.
    pub fn quit(&mut self, links: &mut AppLinks) -> bool {
        if self.local.as_ref().is_none_or(|r| r.stopping) && self.recording.as_ref().is_none_or(|r| r.stop.is_some()) { return false; }
        self.stop(links);
        true
    }

    /// Keep the store available for the final reply. A second explicit close can leave
    /// an unanswered capture behind. Imports already accepted still run to completion.
    pub fn prepare_close(&mut self, links: &mut AppLinks, model: Option<&mut crate::model::Model>, repeated: bool) -> bool {
        let sent = self.quit(links);
        if repeated && self.settling(model.as_deref()) {
            makepad_widgets::log!("Second close: a recording is still settling");
            self.force_close = true;
        }
        if let Some(model) = model { model.await_capture(self.settling(None)); }
        sent
    }

    /// A requested movie is still on the link, or ready to hand to the store.
    pub fn awaiting_movie(&self) -> bool {
        self.recording.as_ref().is_some_and(|r| r.stop.is_some()) || !self.finished.is_empty()
    }

    pub fn settling(&self, model: Option<&crate::model::Model>) -> bool {
        !self.force_close && (self.awaiting_movie() || self.local.as_ref().is_some_and(|r| r.stopping)
            || !self.snaps.is_empty() || !self.grabbing.is_empty() || !self.own_stills.is_empty())
            || model.is_some_and(crate::model::Model::clips_pending)
    }

    pub fn link_events(&mut self, events: &[AppLinkEvent]) {
        // Both replies can be ready in the same poll, in either order. Never wait for the next poll.
        for event in events {
            let AppLinkEvent::Still { id, ticket, result, .. } = event else { continue };
            if !self.pending_stills.remove(&(*id, *ticket)) { continue; }
            match result {
                Ok(path) => match self.recording.as_mut().filter(|r| r.link == *id && r.still == Some(*ticket)) {
                    Some(r) => r.poster = Some(path.clone()),
                    None => self.unused_posters.push(path.clone()),
                },
                Err(why) => eprintln!("Recording poster unavailable: {why}"),
            }
        }
        for event in events {
            let Some(r) = &self.recording else { break };
            match event {
                AppLinkEvent::Recorded { id, ticket, result } if r.link == *id
                    && (Some(*ticket) == r.start.as_ref().map(|s| s.0) || Some(*ticket) == r.stop) => {
                    match result {
                        Ok(clip) => {
                            let r = self.recording.take().unwrap();
                            if r.still.is_some_and(|t| self.pending_stills.contains(&(*id, t))) {
                                eprintln!("Posting {} without a poster: the grab is still pending.", r.title);
                            }
                            self.finished.push(RecordedClip {
                                destination: r.destination, title: r.title, path: clip.path.clone(),
                                duration_ms: (clip.secs * 1000.).round() as u64, poster: r.poster,
                                width: clip.w, height: clip.h,
                            });
                        }
                        Err(why) => self.failed(why.clone()),
                    }
                }
                AppLinkEvent::Ended { id, why } if r.link == *id => {
                    self.failed(why.clone());
                }
                AppLinkEvent::Restarted { ended, app } if r.link == *ended => {
                    self.failed(format!("{} restarted. Link it again.", app.name));
                }
                _ => {}
            }
        }
    }

    fn failed(&mut self, why: String) {
        if let Some(poster) = self.recording.take().and_then(|r| r.poster) {
            self.unused_posters.push(poster);
        }
        self.problem = Some(why);
    }

    /// Retry scheduling cleanup if the background queue is full.
    pub fn discard_unused_posters(&mut self, mut schedule: impl FnMut(&Path) -> bool) -> bool {
        self.unused_posters.retain(|path| !schedule(path));
        !self.unused_posters.is_empty()
    }

    pub fn take_finished(&mut self) -> Vec<RecordedClip> { std::mem::take(&mut self.finished) }

    /// Consume the same event batch as the share and submit each finished movie to its original project.
    pub fn record_link_events(&mut self, links: &mut AppLinks, events: &[AppLinkEvent], model: &mut crate::model::Model) {
        self.link_events(events);
        self.folder_ready(links, model.recordings_dir());
        let mut problems = model.take_recording_problems();
        for event in events {
            let AppLinkEvent::Still { id, ticket, result, size } = event else { continue; };
            let Some((d, title)) = self.grabbing.remove(&(*id, *ticket)) else { continue; };
            if let Err(why) = result.as_ref().map_err(Clone::clone).and_then(|path|
                model.add_picture(d.town, d.project, d.session, path.clone(), title, size.0, size.1)) { problems.push(why); }
        }
        for clip in self.take_finished() {
            let d = clip.destination;
            if let Err(why) = model.add_clip(d.town, d.project, d.session, clip.path, clip.title.clone(),
                clip.duration_ms, clip.poster, clip.width, clip.height) {
                problems.push(format!("{}: {why}", clip.title));
            }
        }
        if !problems.is_empty() {
            if let Some(why) = self.problem.take() { problems.insert(0, why); }
            self.problem = Some(problems.join(" · "));
        }
        if !self.settling(None) { model.await_capture(false); }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app_link::RecordingRig;

    #[test]
    fn the_capture_chips_and_shortcuts_post_to_the_start_town() {
        use crate::call::{CallAction, CallController, CallEnv, Message, Press, ShareChoice, ShareRole};
        use crate::lists::{Page, ViewData};
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_town_core::{entity::field, fold::Entry};
        use makepad_widgets::{a11y_tree, ids, Button, LiveId, Scope, Widget};
        use std::sync::Arc;

        let mut rig = RecordingRig::new();
        rig.wide_window();
        rig.app.scan();
        rig.wait(|r| !r.app.apps().is_empty());
        let app = rig.app.apps()[0].clone();
        let link = rig.app.link(&app.key);
        rig.live(link);
        let window = rig.app.state(link).unwrap().windows[1].id;
        let (_homes, _, mut model) = crate::model::tests::shared_town("capture-chips");
        let start_town = model.open_id();
        let other = model.towns().into_iter().find(|s| s.id != start_town).unwrap().id;
        let (signal, wake) = std::sync::mpsc::channel();
        model.start_store(crate::store_worker::test_pool(), move || { let _ = signal.send(()); }).unwrap();
        while model.recordings_dir().is_none() { wake.recv().unwrap(); }
        let mut recorder = Recorder::default();
        let mut cx = makepad_widgets::test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let ui = cx.with_vm(|vm| {
            use makepad_widgets::*;
            let root = script_eval!(vm, { use mod.widgets.* View{title_content := WorkCaptionTitle{}} });
            WidgetRef::script_from_value(vm, root)
        });
        let row = ui.widget(&cx, ids!(title_content.recording));
        let mut view = ViewData { page: Page::Tasks, ..Default::default() };
        view.tree.set(model.towns(), model.town_tree());
        view.workspace.current = Some(model.recording_destination().unwrap().project);
        view.show_caption(&mut cx, &ui);
        let mut ctl = CallController::default();
        ctl.configure(CallEnv { files: Default::default(), devices_path: rig.registry.join("devices"), voice: VoiceSource::Synthetic { tone_hz: 500. },
            signal: Arc::new(|| {}), spawn: std_spawn(), jobs: std_spawn() });
        let choose = |ctl: &mut CallController, cx: &mut makepad_widgets::Cx, links: &mut AppLinks, menu| {
            let capture = menu == Press::Clip;
            let menu = CallAction::Press(menu);
            assert!(ctl.opening_from_ui(menu));
            assert!(!ctl.call_actions(cx, menu), "an injected menu press opens nothing");
            assert!(ctl.shown().share.choices.is_none());
            if capture {
                let grant = |test_home| !matches!(crate::consent::decide(Some("test driver".into()), test_home), crate::consent::Consent::Refused(_));
                assert!(!ctl.capture_input(cx, Press::Clip, grant(false)) && ctl.shown().share.choices.is_none());
                assert!(ctl.capture_input(cx, Press::Clip, grant(true)));
            } else { assert!(ctl.call_opening_presses(cx, menu)); }
            ctl.share_app_events(links, &[]);
            let shown = ctl.shown();
            let rows = shown.share.choices.as_ref().unwrap();
            assert_eq!(rows.iter().any(|r| r.label == "Town"), capture, "the local window is offered for capture");
            let row = rows.iter().find(|r| r.label == "App · Details").unwrap();
            let choose = CallAction::Press(Press::Share(row.choice));
            assert!(ctl.opening_from_ui(choose));
            assert!(!ctl.call_actions(cx, choose), "an injected window press cannot capture");
            assert!(ctl.call_opening_presses(cx, choose));
            ctl.share_app_events(links, &[]);
        };
        choose(&mut ctl, &mut cx, &mut rig.app, Press::Clip);
        assert!(ctl.shown().share.offers.is_empty() && !ctl.has_call_workers(), "idle recording needs no call or share");
        let actions = ctl.take_record_presses();
        assert!(matches!(actions.as_slice(), [RecordPress::Start { link: id, window: w }] if (*id, *w) == (link, window)));
        recorder.start(&rig.app, link, window, model.recording_destination().unwrap()).unwrap();
        recorder.record_link_events(&mut rig.app, &[], &mut model);
        let started = recorder.recording.as_ref().unwrap().started;
        assert!(ctl.sync_recording_row(&mut cx, &mut recorder, started));
        let title = "Recording App · Details";
        assert_eq!(ctl.shown().share.capture.line.as_deref(), Some(format!("{title} · 0:00").as_str()));
        ctl.shown().share.capture.show_recording(&mut cx, &ui);
        assert!(row.visible(), "starting the clip shows the caption indicator on Tasks");
        view.page = Page::Home;
        view.show_caption(&mut cx, &ui);
        assert!(row.visible(), "leaving Tasks keeps the indicator visible");
        assert!(!ctl.sync_recording_row(&mut cx, &mut recorder, started + std::time::Duration::from_millis(999)));
        assert!(ctl.sync_recording_row(&mut cx, &mut recorder, started + std::time::Duration::from_secs(65)));
        assert_eq!(ctl.shown().share.capture.line.as_deref(), Some(format!("{title} · 1:05").as_str()));
        ctl.shown().share.capture.show_recording(&mut cx, &ui);
        assert_eq!(row.widget(&cx, ids!(line)).text(), "Recording App · Details · 1:05", "the clock ticks on Home");

        let port = ctl.link_up(&mut *cx, [1; 65], [2; 65], "Peer").unwrap();
        ctl.set_open_town(Some((start_town, vec![([2; 65], "Peer".into())])));
        use makepad_town_core::share::hello::MediaCaps;
        ctl.set_peer_caps([2; 65], MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        port.messages_in.try_send(Message::Ring { call_id: [9; 16], secret: [3; 32], video: false, ext: crate::call::RingExt { town: Some(start_town), ..Default::default() } }.encode()).unwrap();
        ctl.poll(&mut *cx);
        assert!(ctl.call_actions(&mut *cx, CallAction::Press(Press::Answer)));
        choose(&mut ctl, &mut cx, &mut rig.app, Press::Share(ShareChoice::Menu));
        let share = ctl.shown().share.own_offers().next().unwrap().clone();
        assert_eq!(share.title, "App · Details", "the share banner uses the selected window’s title");
        assert_eq!(share.role, ShareRole::Sharer { link, window }, "Share uses the same window list");
        assert!(ctl.take_record_presses().is_empty() && recorder.recording.is_some(), "sharing leaves the recording running");
        let stop = row.widget(&cx, ids!(stop_recording));
        let press = a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId::widget(stop.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None };
        let actions = cx.capture_actions(|cx| { stop.borrow_mut::<Button>().unwrap().handle_action(cx, &press, &mut Scope::empty()); });
        let press = capture::recording_press(&cx, &ui, &actions).expect("the caption Stop emits a capture press");
        assert_eq!(press, Press::StopRecording);
        assert!(ctl.capture_input(&mut cx, press, false));
        assert!(matches!(ctl.take_record_presses().as_slice(), [RecordPress::Stop]));
        model.open_town(other).unwrap();
        recorder.stop(&mut rig.app);
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Recorded { .. }))
            && r.events.iter().any(|e| matches!(e, AppLinkEvent::Still { .. })));
        recorder.record_link_events(&mut rig.app, &std::mem::take(&mut rig.events), &mut model);
        assert!(ctl.sync_recording_row(&mut cx, &mut recorder, Instant::now()));
        ctl.shown().share.capture.show_recording(&mut cx, &ui);
        assert!(!row.visible(), "finishing the clip hides the entire indicator on Home");
        assert!(ctl.shown().share.capture.line.is_none() && ctl.shown().share.own_offers().next().unwrap().clone().session == share.session,
            "Stop clears the clock and leaves the same share running");
        let clip_count = |m: &crate::model::Model| m.state.timeline.iter().filter(|e| matches!(e, Entry::Clip(_))).count();
        while model.clips_pending() {
            model.drain_store();
            if model.clips_pending() { wake.recv().unwrap(); }
        }
        assert_eq!(clip_count(&model), 0, "Stop must not post into the newly selected town");
        model.open_town(start_town).unwrap();
        let posted = model.state.timeline.iter().find_map(|e| match e { Entry::Clip(id) => model.state.entities.get(id), _ => None }).unwrap();
        assert!(posted.value(field::POSTER).is_some(), "the stopped recording reaches the timeline with its poster");

        use makepad_widgets::{Event, KeyCode, KeyEvent, KeyModifiers, WindowWidgetRefExt};
        let key = |code| capture::shortcut(&Event::KeyDown(KeyEvent { key_code: code, modifiers: KeyModifiers {
            logo: cfg!(target_vendor = "apple"), control: !cfg!(target_vendor = "apple"), shift: true, alt: false },
            ..Default::default() })).unwrap();
        let twice = |ctl: &mut CallController, cx: &mut makepad_widgets::Cx, links: &mut AppLinks, code| {
            for _ in 0..2 {
                let press = ctl.capture_key_press(key(code));
                assert!(ctl.capture_input(cx, press, true));
                ctl.share_app_events(links, &[]);
            }
        };
        twice(&mut ctl, &mut cx, &mut rig.app, KeyCode::Key2);
        assert!(matches!(ctl.take_record_presses().as_slice(), [RecordPress::Start { link: id, window: w }] if (*id, *w) == (link, window)));
        recorder.start(&rig.app, link, window, model.recording_destination().unwrap()).unwrap();
        recorder.record_link_events(&mut rig.app, &[], &mut model);
        assert!(!recorder.awaiting_movie(), "running capture has no outstanding stop");
        assert!(recorder.quit(&mut rig.app), "quit stops a running recording");
        assert!(recorder.awaiting_movie(), "closing must wait for the link's Recorded reply");
        let stopping = recorder.recording.as_ref().unwrap();
        let tickets = (stopping.stop.unwrap(), stopping.still.unwrap());
        assert!(!recorder.quit(&mut rig.app), "a second quit has nothing new to dispatch");
        let stopping = recorder.recording.as_ref().unwrap();
        assert_eq!((stopping.stop, stopping.still), (Some(tickets.0), Some(tickets.1)), "both quits await the same movie and poster");
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Recorded { ticket, result: Ok(_), .. } if *ticket == tickets.0)));
        recorder.record_link_events(&mut rig.app, &std::mem::take(&mut rig.events), &mut model);
        assert!(!recorder.awaiting_movie(), "the movie has passed from the link to the store");
        assert!(recorder.settling(Some(&model)), "quit waits until the accepted clip is posted");
        assert_eq!(clip_count(&model), 1, "accepting the movie for import has not published it yet");
        assert!(!recorder.quit(&mut rig.app), "quit while the store is importing does not send a second stop");
        while clip_count(&model) == 1 {
            model.drain_store();
            if clip_count(&model) == 1 { wake.recv().unwrap(); }
        }
        assert!(!recorder.settling(Some(&model)), "the timeline post releases the close");
        assert_eq!(clip_count(&model), 2, "the quit recording adds exactly one more clip");
        ctl.sync_recording_row(&mut cx, &mut recorder, Instant::now());
        twice(&mut ctl, &mut cx, &mut rig.app, KeyCode::Key1);
        let snap = ctl.take_record_presses();
        assert!(matches!(snap[..], [RecordPress::Snap(capture::WindowTarget::Linked(id, w))] if (id, w) == (link, window)));
        recorder.capture_press(&mut rig.app, snap[0], &model).unwrap();
        let host = cx.with_vm(|vm| {
            use makepad_widgets::*;
            let v = script_eval!(vm, { use mod.widgets.* Window{} });
            WidgetRef::script_from_value(vm, v)
        }).as_window();
        recorder.capture_event(&mut cx, &host, &mut rig.app, &mut model);
        let snap_ticket = *recorder.grabbing.keys().next().expect("the full-size grab was dispatched");
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Still { id, ticket, .. } if (*id, *ticket) == snap_ticket)));
        recorder.record_link_events(&mut rig.app, &std::mem::take(&mut rig.events), &mut model);
        while model.clips_pending() {
            model.drain_store();
            if model.clips_pending() { wake.recv().unwrap(); }
        }
        let image = model.state.entities.values().find(|e| e.text(field::MEDIA) == "image/jpeg").expect("Snap posts a JPEG");
        assert_eq!((image.value(field::W), image.value(field::H)), (Some(&makepad_town_core::event::Value::Int(1280)), Some(&makepad_town_core::event::Value::Int(720))), "Snap keeps the full drawable, including device scale");
        assert_eq!(image.value(field::POSTER), image.value(field::BLOB), "the picture is its own poster");
        assert_eq!(image.value(field::DURATION), Some(&makepad_town_core::event::Value::Int(0)));
        rig.app.unlink(link);
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Ended { id, .. } if *id == link)));
        ctl.share_app_events(&mut rig.app, &std::mem::take(&mut rig.events));
        twice(&mut ctl, &mut cx, &mut rig.app, KeyCode::Key1);
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Linked(id) if *id != link)));
        ctl.share_app_events(&mut rig.app, &std::mem::take(&mut rig.events));
        assert!(matches!(ctl.take_record_presses().as_slice(), [RecordPress::Snap(capture::WindowTarget::Linked(id, w))]
            if *id != link && *w == window), "the shortcut remembers the second window across relinking");
        assert!(ctl.capture_input(&mut cx, Press::Clip, true));
        ctl.share_app_events(&mut rig.app, &[]);
        let shown = ctl.shown();
        let town = shown.share.choices.unwrap().iter().find(|r| r.label == "Town").unwrap().choice;
        assert!(ctl.capture_input(&mut cx, Press::Share(town), true));
        ctl.share_app_events(&mut rig.app, &[]);
        let own = ctl.take_record_presses();
        assert!(matches!(own[..], [RecordPress::Own]));
        recorder.capture_press(&mut rig.app, own[0], &model).unwrap();
        recorder.capture_event(&mut cx, &host, &mut rig.app, &mut model);
        assert!(recorder.local.as_ref().unwrap().file.is_some(), "Town invokes the window recorder");
        assert!(host.borrow_mut().unwrap().record_to(&mut cx, rig.registry.join("busy.mp4")).is_err(),
            "a second recorder cannot take over the same window");
        assert!(recorder.quit(&mut rig.app) && recorder.settling(Some(&model)));
        assert!(!recorder.quit(&mut rig.app));
        // This poll sends the stop. No frame was ever presented here, so the encoder then ends
        // with an error; wait for that result rather than spinning on the poll.
        recorder.capture_event(&mut cx, &host, &mut rig.app, &mut model);
        if let Some(local) = &recorder.local {
            assert!(local.stop_sent, "quitting stops the window recorder");
            local.file.as_ref().unwrap().wait();
            recorder.capture_event(&mut cx, &host, &mut rig.app, &mut model);
        }
        assert!(recorder.local.is_none(), "the encoder's result ends the local movie");
        // The next poll drops the poster still that waited on the movie.
        recorder.capture_event(&mut cx, &host, &mut rig.app, &mut model);
        assert!(!recorder.settling(Some(&model)), "the local encoder's terminal result releases the close");
        model.stop_store();
        loop {
            model.drain_store();
            if model.store_stopped() { break }
            wake.recv().unwrap();
        }
        ctl.shutdown(&mut *cx);
    }

    #[test]
    fn an_empty_share_list_says_why() {
        let unreachable = [("Fab".to_string(), "outside the user's sockets".to_string())];
        let reason = capture::empty_reason;
        assert_eq!(reason(false, false, &[], None), "Town's app link is not running, so it cannot list apps.");
        assert_eq!(reason(true, true, &[], None), "Looking for running Makepad apps…", "a scan in flight says so");
        assert_eq!(reason(true, false, &unreachable, None), "No apps to share: Town can't reach Fab: outside the user's sockets.");
        assert_eq!(reason(true, false, &[unreachable[0].clone(), unreachable[0].clone()], None),
            "No apps to share: Town can't reach Fab (and 1 more): outside the user's sockets.");
        assert_eq!(reason(true, false, &[], Some("Fab")), "No apps to share: Fab has no visible window.");
        assert_eq!(reason(true, false, &[], None), "No apps to share: no other Makepad app is running on this computer.");
    }

    #[test]
    fn a_local_press_records_the_listed_window_and_keeps_its_destination() {
        let mut rig = RecordingRig::new();
        rig.wide_window();
        rig.app.scan();
        rig.wait(|r| !r.app.apps().is_empty());
        let app = rig.app.apps()[0].clone();
        let link = rig.app.link(&app.key);
        rig.live(link);
        let window = rig.app.state(link).unwrap().windows[1].id;
        assert!(RecordPress::Start { link, window }.opening(), "starting capture requires a local opening press");
        assert!(!RecordPress::Stop.opening());
        let mut recorder = Recorder::default();
        let dest = Destination { town: [1; 32], project: [2; 32], session: Some([3; 16]) };
        recorder.start(&rig.app, link, window, dest).unwrap();
        let running = recorder.recording.as_ref().unwrap();
        let later = running.started + std::time::Duration::from_millis(65_999);
        assert_eq!(capture::recording_line(&running.title, later.duration_since(running.started).as_secs()), "Recording App · Details · 1:05",
            "the row's clock uses the recording's own start");
        recorder.folder_ready(&mut rig.app, None);
        assert!(recorder.start(&rig.app, link, window, dest).is_err(), "an accepted press owns the recording even while storage opens");
        recorder.folder_ready(&mut rig.app, Some(Ok(&rig.registry)));
        recorder.stop(&mut rig.app);
        recorder.stop(&mut rig.app);
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Recorded { .. }))
            && r.events.iter().any(|e| matches!(e, AppLinkEvent::Still { .. })));
        recorder.link_events(&std::mem::take(&mut rig.events));
        assert!(recorder.problem.is_none(), "the app must finish the clip: {:?}", recorder.problem);
        let clips = recorder.take_finished();
        assert_eq!(clips.len(), 1, "one stop posts one finished clip");
        let clip = &clips[0];
        assert_eq!(clip.destination, dest, "the clip retains its town, project and session");
        assert_eq!(clip.title, "App · Details", "title uses listing order");
        assert_eq!(clip.duration_ms, 2500, "duration comes from the finished movie");
        assert!(clip.path.is_file(), "the fake app wrote the movie");
        assert_eq!((clip.width, clip.height), (24, 16), "the dimensions come from the finished movie");
        let jpeg = std::fs::read(clip.poster.as_ref().expect("both answers available at Stop")).unwrap();
        let decoded = makepad_widgets::ImageBuffer::from_jpg(&jpeg).unwrap();
        assert_eq!((decoded.width, decoded.height), (640, 360), "the 1280×720 drawable keeps its aspect ratio");
        assert!(jpeg.len() <= 512 * 1024, "the poster stays below the automatic transfer budget");
        let small = rig.app.state(link).unwrap().windows[0].id;

        // The same worker handles later stops. Deliver the movie before a successful still
        // this time, then let the late reply arrive while the next recording is active.
        recorder.start(&rig.app, link, small, dest).unwrap();
        recorder.folder_ready(&mut rig.app, Some(Ok(&rig.registry)));
        recorder.stop(&mut rig.app);
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Recorded { .. })));
        let (movies, later): (Vec<_>, _) = std::mem::take(&mut rig.events).into_iter()
            .partition(|e| matches!(e, AppLinkEvent::Recorded { .. }));
        recorder.link_events(&movies);
        assert!(recorder.take_finished()[0].poster.is_none(), "an outstanding grab never delays the movie");
        rig.events = later;
        let (mut model, _) = crate::model::Model::open(&rig.registry.join("home")).unwrap();
        let target = Destination { town: model.open_id(), project: model.project().id, session: None };
        let (signal, wake) = std::sync::mpsc::channel();
        model.start_store(crate::store_worker::test_pool(), move || { let _ = signal.send(()); }).unwrap();
        while model.recordings_dir().is_none() { wake.recv().unwrap(); }
        recorder.start(&rig.app, link, small, target).unwrap();
        recorder.folder_ready(&mut rig.app, model.recordings_dir());
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Still { .. })));
        recorder.link_events(&std::mem::take(&mut rig.events));
        let mut unused = Vec::new();
        recorder.discard_unused_posters(|path| { unused.push(path.to_owned()); true });
        assert_eq!(unused.len(), 1, "the late poster goes to cleanup instead of the next recording");

        rig.refuse_next_still();
        assert!(recorder.prepare_close(&mut rig.app, Some(&mut model), false));
        model.stop_store();
        model.drain_store();
        assert!(recorder.settling(Some(&model)), "the outstanding Recorded reply holds the first close");
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Recorded { .. }))
            && r.events.iter().any(|e| matches!(e, AppLinkEvent::Still { result: Err(_), .. })));
        let movie = rig.events.iter().find_map(|event| match event {
            AppLinkEvent::Recorded { result: Ok(clip), .. } => Some(clip.path.clone()),
            _ => None,
        }).expect("a failed grab still delivers the movie");
        recorder.record_link_events(&mut rig.app, &std::mem::take(&mut rig.events), &mut model);
        assert!(model.clips_pending(), "a movie delivered during close enters the store");
        assert!(!recorder.prepare_close(&mut rig.app, Some(&mut model), true));
        assert!(recorder.settling(Some(&model)), "even a second close drains the accepted import");
        loop {
            model.drain_store();
            if model.store_stopped() { break; }
            wake.recv().unwrap();
        }
        let entity = model.state.timeline.iter().find_map(|entry| match entry {
            makepad_town_core::fold::Entry::Clip(id) => model.state.entities.get(id),
            _ => None,
        }).expect("the movie with a failed grab reached its project even during store shutdown");
        assert!(entity.value(makepad_town_core::entity::field::POSTER).is_none());
        assert!(!movie.exists(), "publication preceded cleanup");
        assert!(!recorder.settling(Some(&model)), "publication releases the close");
        recorder.start(&rig.app, link, small, target).unwrap();
        recorder.link_events(&[AppLinkEvent::Restarted { ended: link, app: app.clone() }]);
        assert!(recorder.recording.is_none());
        assert_eq!(recorder.problem.as_deref(), Some(format!("{} restarted. Link it again.", app.name).as_str()));
        recorder.start(&rig.app, link, small, target).unwrap();
        assert!(recorder.problem.is_none(), "the next recording press clears the previous failure");
        rig.app.unlink(link);
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Ended { .. })));
        let reason = rig.events.iter().find_map(|e| match e {
            AppLinkEvent::Ended { id, why } if *id == link => Some(why.clone()), _ => None,
        }).unwrap();
        recorder.link_events(&rig.events);
        assert!(recorder.recording.is_none() && recorder.problem == Some(reason), "unlink shows the app-link worker's reason");
        let refused = rig.app.still(link, window, &rig.registry.join("unlinked.jpg"), StillSize::Poster);
        rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Still { ticket, result: Err(why), .. }
            if *ticket == refused && why == "app is not linked")));
    }
}
