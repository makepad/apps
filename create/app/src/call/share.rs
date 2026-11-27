//! Window offers on the authenticated link. The first displayed picture joins its timeline.
use super::*;
use crate::app_link::{AppLinkEvent, AppLinks, AppWindow, LinkId, PicturePreview, PictureStream, PictureWant, WindowId};
use crate::record::{Recorder, RecordPress, capture::{CaptureShown, Purpose, RecordingClock, WindowPicker, WindowTarget}};
use makepad_call::driver::{codec::{Packet, Picture}, Content, EncodedSource, VideoCaps, View};
use makepad_town_core::event::{LeaveWhy, SessionId};
use super::super::{signal::ShareProblem, Message};
use crate::call::ShareSurface;
use std::{cell::Cell, collections::{BTreeMap, BTreeSet}, sync::OnceLock, time::Duration};

/// A watched share that has captured nothing for this long says so on the sharer's row.
pub(super) const NO_PICTURE_AFTER: Duration = Duration::from_secs(3);
/// The sharer's own view needs to follow the app, not to match the guest's frame rate.
const PREVIEW_FPS: u32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareRole {
    Sharer { link: LinkId, window: WindowId },
    Viewer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareEvent {
    Started { session: SessionId, title: String, role: ShareRole },
    Ended { session: SessionId, reason: ShareEndReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareChoice {
    Menu,
    /// A picker item is valid only for the snapshot that displayed it.
    Row(u64),
}

#[derive(Clone, Debug)]
pub struct ShareRow {
    pub widget_id: LiveId,
    pub choice: ShareChoice,
    pub label: String,
    pub selectable: bool,
    pub thumbnail: Option<std::rc::Rc<crate::record::capture::WindowThumbnail>>,
}

#[derive(Clone)]
pub struct ShareOffer {
    pub session: SessionId,
    pub title: String,
    pub role: ShareRole,
    pub window_size: (f64, f64),
    rights: Arc<ShareRights>,
    offered_at: Instant,
    waiting_line: String,
    first_draw: Arc<OnceLock<Instant>>,
    pub sharer_line: Option<Arc<str>>,
    pub(crate) stopped_line: Option<Arc<str>>,
    preview_note: Option<Arc<str>>,
}

#[derive(Clone)]
pub struct EndedShare {
    pub session: SessionId,
    pub title: Arc<str>,
    pub role: ShareRole,
    pub notice: Arc<str>,
}

#[derive(Clone, Default)]
pub struct ShareShown {
    pub overlay: Option<(makepad_town_core::event::Id, crate::tweak::overlay::Selections)>,
    pub offers: Arc<Vec<Arc<ShareOffer>>>,
    pub choices: Option<Arc<[ShareRow]>>,
    pub watching: Option<SessionId>,
    pub(super) media_held: bool,
    pub closed_notice: Option<Arc<str>>,
    pub ended: Arc<Vec<EndedShare>>,
    pub picture: Option<Arc<Picture>>,
    pub capture: CaptureShown,
    pub problem: Option<Arc<str>>,
    pub control: crate::call::control::ControlShown,
    pub peer_watching: Option<SessionId>,
    pub control_session: Option<SessionId>,
    /// The preview of the local window whose page is open. The guest picture is separate.
    pub(super) preview: Option<(SessionId, Arc<Picture>)>,
}

impl ShareShown {
    pub fn offer(&self, session: SessionId) -> Option<&Arc<ShareOffer>> {
        self.offers.iter().find(|o| o.session == session)
    }

    fn offer_mut(&mut self, session: SessionId) -> Option<&mut ShareOffer> {
        Arc::make_mut(&mut self.offers).iter_mut().find(|o| o.session == session).map(Arc::make_mut)
    }

    pub fn watched(&self) -> Option<&Arc<ShareOffer>> { self.offer(self.watching?) }

    pub fn own_offers(&self) -> impl Iterator<Item = &Arc<ShareOffer>> {
        self.offers.iter().filter(|o| matches!(o.role, ShareRole::Sharer { .. }))
    }

    pub(crate) fn picture_for(&self, session: SessionId) -> Option<&Arc<Picture>> {
        if self.watching == Some(session) { self.picture.as_ref() }
        else { self.preview.as_ref().filter(|(id, _)| *id == session).map(|(_, pic)| pic) }
    }

    pub fn shows_picture(&self, offer: &ShareOffer) -> bool {
        self.watching == Some(offer.session) || matches!(offer.role, ShareRole::Sharer { .. })
    }

    pub fn waiting_line<'a>(&self, offer: &'a ShareOffer) -> Option<&'a str> {
        if matches!(offer.role, ShareRole::Sharer { .. }) {
            return self.picture_for(offer.session).is_none().then(|| offer.preview_note.as_deref().unwrap_or(&offer.waiting_line));
        }
        if let Some(line) = &offer.stopped_line { Some(line) }
        else if !self.media_held { Some("Connecting securely…") }
        else if self.watching == Some(offer.session) && self.picture.is_none() { Some(&offer.waiting_line) }
        else { None }
    }
}

impl ShareOffer {
    pub fn allow_all(&self) -> bool { self.rights.allow_all }
    pub fn allowed(&self) -> &BTreeMap<PublicKey, String> { &self.rights.allowed }

    /// The center panel acknowledges the first presentation of this offer once.
    pub(crate) fn picture_drawn(&self) { self.first_draw.get_or_init(Instant::now); }
}

#[derive(Default)]
pub(super) struct Sharing {
    pub(super) shown: ShareShown,
    events: Vec<ShareEvent>,
    pub(super) revokes: Vec<LinkId>,
    picker: WindowPicker,
    picker_dirty: bool,
    record_presses: Vec<RecordPress>,
    clock: RecordingClock,
    joined: BTreeSet<SessionId>,
    watch_view: View,
    measured_panel: Option<(SessionId, View)>,
    /// The pop-out window's picture size. It stays in the subscription after the window closes,
    /// until the watch does, so opening and closing the window never resizes the stream.
    window_panel: Option<(SessionId, View)>,
    locals: BTreeMap<SessionId, LocalShare>,
    sending: Option<SessionId>,
    receiving: bool,
    previewing: Option<SessionId>,

    fresh: bool,
    last_report: Option<Instant>,
    logged_keys: [Option<u64>; 4],
    watch_started: Option<Instant>,
}

struct LocalShare {
    rights: Arc<ShareRights>,
    source: OfferSource,
    view: View,
    /// The offered stream's counts and end reason, independent of the session borrowing the stream.
    probe: Option<Box<dyn Fn() -> (u64, Option<Arc<str>>) + Send + Sync>>,
    /// When the viewer last subscribed to this share.
    subscribed_at: Option<Instant>,
    /// The sharer's own view, open while its page shows it, and the page's picture size.
    preview: Option<(PicturePreview, Instant)>,
    preview_size: Option<(u32, u32)>,
    last_preview: Option<Arc<Picture>>,
    line_peer: String,
    // Changes only when the line shown to the sharer can change.
    line_state: Option<(bool, Option<u64>, bool)>,
    /// Why the picture goes out smaller than the window, when the encoder bounds it.
    size_note: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct ShareRights {
    allow_all: bool,
    allowed: BTreeMap<PublicKey, String>,
}

impl ShareRights {
    fn allows(&self, peer: &PublicKey) -> bool { self.allow_all || self.allowed.contains_key(peer) }
}

enum RightChange<'a> {
    All(bool),
    Allow(&'a str),
    Remove(PublicKey),
}

/// The window belongs to the offer. Only its newest session can consume encoded pictures.
struct OfferSource { caps: VideoCaps, cell: Arc<Leased> }
struct Leased { holder: AtomicU64, source: Mutex<Box<dyn EncodedSource>> }
struct Lease { id: u64, cell: Arc<Leased>, needs_key: Cell<bool> }

impl OfferSource {
    fn new(caps: VideoCaps, source: Box<dyn EncodedSource>) -> Self {
        Self { caps, cell: Arc::new(Leased { holder: AtomicU64::new(0), source: Mutex::new(source) }) }
    }

    fn lease(&self) -> Lease {
        // Ask for the held key immediately unless the previous worker is using
        // the source. In that case the new worker asks on its first access.
        let id = self.cell.holder.fetch_add(1, Ordering::AcqRel) + 1;
        let needs_key = if let Ok(mut source) = self.cell.source.try_lock() {
            source.key_now();
            false
        } else { true };
        Lease { id, cell: self.cell.clone(), needs_key: Cell::new(needs_key) }
    }
}

impl Lease {
    fn with_source<T>(&self, f: impl FnOnce(&mut dyn EncodedSource) -> T) -> Option<T> {
        let mut source = self.cell.source.lock().unwrap();
        if self.cell.holder.load(Ordering::Acquire) != self.id { return None }
        if self.needs_key.replace(false) { source.key_now(); }
        Some(f(source.as_mut()))
    }
}

impl EncodedSource for Lease {
    fn pop(&mut self) -> Option<Packet> { self.with_source(|s| s.pop()).flatten() }
    fn frame_counts(&self) -> Option<(u64, u64)> { self.with_source(|s| s.frame_counts()).flatten() }
    fn want(&mut self, w: u32, h: u32, fps: u32) { self.with_source(|s| s.want(w, h, fps)); }
    fn set_kbps(&mut self, kbps: u32) { self.with_source(|s| s.set_kbps(kbps)); }
    fn key_now(&mut self) { self.with_source(|s| s.key_now()); }
    fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) { self.with_source(|s| s.set_wake(wake)); }
}

impl EncodedSource for PictureStream {
    fn pop(&mut self) -> Option<Packet> { PictureStream::pop(self) }
    fn want(&mut self, w: u32, h: u32, fps: u32) { PictureStream::want(self, w, h, fps); }
    fn set_kbps(&mut self, kbps: u32) { PictureStream::set_kbps(self, kbps); }
    fn key_now(&mut self) { PictureStream::key_now(self); }
    fn frame_counts(&self) -> Option<(u64, u64)> {
        let stats = self.stats();
        Some((stats.received, stats.encoded))
    }
    fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) { PictureStream::set_wake(self, wake); }
}

/// The sharer's line: "Sharing <app> with <peer>.", then whether the peer has opened it, and
/// when a watched share has captured nothing for NO_PICTURE_AFTER, that no picture came back.
/// An error that ended the app's picture stream is said in its own words.
pub(super) fn sharer_line(app: &str, peer: &str, watched_for: Option<Duration>, captured: u64, ended_why: Option<&str>) -> String {
    let head = format!("Sharing {app} with {peer}.");
    match (ended_why, watched_for) {
        (Some(why), _) => format!("{head} {app}'s picture stopped: {why}"),
        (None, None) => format!("{head} {peer} hasn't opened it yet."),
        (None, Some(t)) if captured == 0 && t >= NO_PICTURE_AFTER =>
            format!("{head} Town asked {app} for a picture when {peer} opened it, and none has come back in {} s.", t.as_secs()),
        (None, Some(_)) => head,
    }
}

fn share_counters(peer: &makepad_call::driver::PeerStats) -> [u64; 4] {
    [peer.session.share_requests_sent, peer.session.share_requests_received, peer.session.share_keys_sent, peer.share_decode_errors]
}

/// The largest picture of a share: the window's own pixels, bounded only by the largest picture
/// this computer's encoder takes, when it says. A slow link lowers the frame rate, never this
/// size. A bound is never silent: the second value says so, for the log and the sharer's page.
/// The largest picture this computer's share encoder takes, when the platform says.
fn encoder_max() -> Option<[u32; 2]> {
    makepad_video::VideoStreamEncoder::max_size(makepad_video::StreamVideoCodec::H264, false)
}

pub(super) fn caps(size: (f64, f64), dpi: f64, encoder_max: Option<[u32; 2]>) -> Option<(VideoCaps, Option<String>)> {
    let (w, h) = (size.0 * dpi, size.1 * dpi);
    if !w.is_finite() || !h.is_finite() || w <= 0. || h <= 0. { return None }
    let scale = encoder_max.map_or(1., |[mw, mh]| (f64::from(mw) / w).min(f64::from(mh) / h).min(1.));
    let even = |n: f64| (n as u32 & !1).max(2);
    let (width, height) = (even(w * scale), even(h * scale));
    let note = encoder_max.filter(|_| scale < 1.).map(|[mw, mh]| format!(
        "The window is {}x{} pixels and this computer's encoder takes at most {mw}x{mh}, so it goes out at {width}x{height}.",
        even(w), even(h)));
    Some((VideoCaps { width, height, fps: 30, content: Content::Share }, note))
}

impl Sharing {
    pub(super) fn detach(&mut self) {
        self.sending = None;
        self.receiving = false;
        self.fresh |= self.shown.picture.take().is_some();
    }

    pub(super) fn close_picker(&mut self) {
        if self.shown.capture.picking { return; }
        self.picker.close();
        self.shown.choices = None;
    }

    fn toggle_picker(&mut self, purpose: Purpose) {
        self.shown.problem = None;
        self.picker.toggle(purpose);
        self.shown.capture.picking = self.picker.purpose.is_some_and(|p| p != Purpose::Share);
        self.shown.choices = self.picker.purpose.map(|_| Arc::default());
    }

}

impl CallController {
    pub fn set_share_overlay(&mut self, overlay: Option<(makepad_town_core::event::Id, crate::tweak::overlay::Selections)>) {
        self.sharing.shown.overlay = overlay;
    }

    /// Local capture choices remain available independently of the voice call.
    pub fn opening_from_ui(&self, action: CallAction) -> bool {
        match action {
            CallAction::Press(Press::Clip | Press::Snap) => return true,
            CallAction::Press(Press::Share(ShareChoice::Row(_))) if self.sharing.shown.capture.picking => return true,
            CallAction::Press(Press::Share(_)) => return self.linked(),
            _ => {}
        }
        self.opening(action)
    }

    pub fn take_share_events(&mut self) -> Vec<ShareEvent> { std::mem::take(&mut self.sharing.events) }

    pub fn revoke_ended_shares(&mut self, apps: &mut AppLinks) {
        for id in self.sharing.revokes.drain(..) { apps.revoke_input(id); }
    }

    pub fn share_app_events(&mut self, apps: &mut AppLinks, events: &[AppLinkEvent]) -> bool {
        let changed = self.handle_share_app_events(apps, events);
        if changed { self.settle_media(); }
        changed
    }

    fn handle_share_app_events(&mut self, apps: &mut AppLinks, events: &[AppLinkEvent]) -> bool {
        self.revoke_ended_shares(apps);
        if self.sharing.shown.choices.is_none() && self.sharing.locals.is_empty() { return false }
        let mut changed = false;
        for e in events {
            if let AppLinkEvent::LocalInput(id) = e {
                if self.sharing.shown.control_session.and_then(|s| self.sharer_window(s)).is_some_and(|(link, _)| link == *id)
                    && apps.state(*id).is_some_and(|s| !s.input_allowed) {
                    changed |= self.end_control(crate::call::control::EndReason::LocalInput, true);
                }
            }
            if !matches!(e, AppLinkEvent::Ended { .. } | AppLinkEvent::Restarted { .. } | AppLinkEvent::Windows(_)) { continue }
            let offers = self.sharing.shown.offers.clone();
            for offer in offers.iter() {
                let ShareRole::Sharer { link, window } = offer.role else { continue };
                let reason = match e {
                    AppLinkEvent::Ended { id, why } if *id == link =>
                        Some(if why == "unlinked" { ShareEndReason::Stopped } else { ShareEndReason::AppClosedOrRestarted }),
                    AppLinkEvent::Restarted { ended, .. } if *ended == link => Some(ShareEndReason::AppClosedOrRestarted),
                    AppLinkEvent::Windows(id) if *id == link => {
                        if let Some(w) = apps.state(link).and_then(|l| l.windows.iter().find(|w| w.id == window && w.visible)) {
                            changed |= self.resize_share(offer, w);
                            None
                        } else { Some(ShareEndReason::AppClosedOrRestarted) }
                    }
                    _ => None,
                };
                // The closed notice explains the end on both seats.
                if let Some(reason) = reason {
                    changed |= self.end_share(offer.session, reason, true);
                }
            }
        }
        self.revoke_ended_shares(apps);
        if self.sharing.shown.choices.is_none() { return changed }
        let offers = &self.sharing.shown.offers;
        let locals = &self.sharing.locals;
        let shared = |link, window| offers.iter().find(|o| o.role == (ShareRole::Sharer { link, window }))
            .and_then(|o| locals.get(&o.session)).map(|l| l.last_preview.clone());
        let dirty = std::mem::take(&mut self.sharing.picker_dirty);
        let (updated, picked) = self.sharing.picker.poll_rows(apps, events, &mut self.sharing.shown.choices, dirty, Some(&shared));
        if let Some((intent, target)) = picked {
            self.sharing.shown.capture.picking = false;
            let target = match target {
                Ok(target) => target,
                Err(reason) => {
                    if intent == Purpose::Share { self.sharing.shown.problem = Some(reason.into()); }
                    else { self.sharing.shown.capture.problem = Some(reason.into()); }
                    return true;
                }
            };
            match (intent, target) {
                (Purpose::Share, WindowTarget::Linked(link, window)) => {
                    if let Err(why) = self.open_share_window(apps, link, window) { self.sharing.shown.problem = Some(why.into()); }
                }
                (Purpose::Clip, WindowTarget::Linked(link, window)) => self.sharing.record_presses.push(RecordPress::Start { link, window }),
                (Purpose::Clip, WindowTarget::Town) => self.sharing.record_presses.push(RecordPress::Own),
                (Purpose::Snap, target) => self.sharing.record_presses.push(RecordPress::Snap(target)),
                (Purpose::Share, WindowTarget::Town) => unreachable!("only capture offers the local window"),
            }
        }
        changed || updated
    }

    fn sharer_window(&self, session: SessionId) -> Option<(LinkId, WindowId)> {
        match self.sharing.shown.offer(session)?.role {
            ShareRole::Sharer { link, window } => Some((link, window)),
            ShareRole::Viewer => None,
        }
    }

    /// Opening the picker and selecting a window both retain the originating input’s consent.
    pub fn capture_input(&mut self, cx: &mut Cx, press: Press, granted: bool) -> bool {
        let action = CallAction::Press(press);
        if press == Press::StopRecording { return self.call_actions(cx, action); }
        granted && self.opening_from_ui(action) && self.call_opening_presses(cx, action)
    }

    pub fn take_record_presses(&mut self) -> Vec<RecordPress> { std::mem::take(&mut self.sharing.record_presses) }

    /// A shortcut selects the first available window only while its own picker is already open.
    pub fn capture_key_press(&self, key: Press) -> Press {
        if key == Press::Clip && self.sharing.shown.capture.line.is_some() { return Press::StopRecording; }
        let purpose = if key == Press::Snap { Purpose::Snap } else { Purpose::Clip };
        if self.sharing.picker.purpose != Some(purpose) { return key; }
        Press::FirstCaptureWindow
    }

    pub fn close_capture_picker(&mut self) -> bool {
        if !self.sharing.shown.capture.picking { return false; }
        self.sharing.shown.capture.picking = false;
        self.sharing.close_picker();
        true
    }

    pub fn sync_recording_row(&mut self, cx: &mut Cx, recorder: &mut Recorder, now: Instant) -> bool {
        let share = &mut self.sharing;
        let mut changed = share.clock.update(cx, recorder.running(), now, &mut share.shown.capture);
        if let Some(why) = recorder.problem.take() {
            share.shown.capture.problem = Some(why.into());
            changed = true;
        }
        changed
    }

    fn resize_share(&mut self, offer: &ShareOffer, window: &AppWindow) -> bool {
        let Some((caps, note)) = caps(window.size, window.dpi, encoder_max()) else { return false };
        let session = offer.session;
        let Some(local) = self.sharing.locals.get_mut(&session) else { return false };
        if local.size_note != note {
            if let Some(note) = &note { log!("Share {}: {note}", offer.title); }
            local.size_note = note;
            local.line_state = None;
            self.update_sharer_line(offer);
        }
        let Some(local) = self.sharing.locals.get_mut(&session) else { return false };
        if local.source.caps != caps {
            local.source.caps = caps;
            if self.sharing.sending == Some(session) {
                if let Some(call) = &mut self.current {
                    call.add_share(caps, Box::new(local.source.lease()));
                }
            }
        }
        if offer.window_size == window.size { return false }
        self.sharing.shown.offer_mut(session).unwrap().window_size = window.size;
        self.send_share(Message::Share { session, title: offer.title.clone(), w: window.size.0 as f32, h: window.size.1 as f32, ext: Default::default() });
        true
    }

    fn open_share_window(&mut self, apps: &mut AppLinks, link: LinkId, window: WindowId) -> Result<(), String> {
        let title = crate::record::capture::window_label(apps, link, window)
            .ok_or("The selected window is no longer visible.")?;
        let w = apps.state(link).and_then(|l| l.windows.iter().find(|w| w.id == window)).cloned()
            .ok_or("The selected window is no longer linked.")?;
        let source = apps.open_picture(link, PictureWant { window, max_w: 0, max_h: 0, fps: 0, kbps: 0 })
            .ok_or("The app link could not open a picture stream.")?;
        let probe = source.probe();
        let session = self.start_window(link, &w, title, Box::new(source))?;
        self.sharing.locals.get_mut(&session).unwrap().probe = Some(Box::new(move || (probe.stats().received, probe.ended_why())));
        Ok(())
    }

    pub(super) fn start_window(&mut self, link: LinkId, window: &AppWindow, title: String, source: Box<dyn EncodedSource>) -> Result<SessionId, String> {
        let peer = self.flow.peer().copied().ok_or("The peer is no longer linked.")?;
        let (caps, size_note) = caps(window.size, window.dpi, encoder_max()).ok_or("The selected window has no usable picture size.")?;
        if let Some(note) = &size_note { log!("Share {title}: {note}"); }
        if self.sharing.shown.own_offers().any(|o| o.role == (ShareRole::Sharer { link, window: window.id })) {
            return Err("This window is already being shared.".into());
        }
        let mut session = [0; 16];
        makepad_network::tls::os_random(&mut session).map_err(|e| format!("Could not start the share: {e}"))?;
        let (w, h) = (window.size.0 as f32, window.size.1 as f32);
        if !w.is_finite() || !h.is_finite() || w <= 0. || h <= 0. { return Err("The selected window has no usable picture size.".into()) }
        let role = ShareRole::Sharer { link, window: window.id };
        self.sharing.locals.insert(session, LocalShare {
            rights: Arc::default(),
            source: OfferSource::new(caps, source), view: View::default(), probe: None,
            preview: None, preview_size: None, last_preview: None, subscribed_at: None, line_peer: String::new(), line_state: None,
            size_note,
        });
        self.begin_offer(session, title.clone(), role, window.size);
        self.facts.push(Fact::ShareOpen { session, title: title.clone(), to: peer });
        self.send_share(Message::Share { session, title, w, h, ext: Default::default() });
        self.settle_media();
        Ok(session)
    }

    fn begin_offer(&mut self, session: SessionId, title: String, role: ShareRole, window_size: (f64, f64)) {
        if self.sharing.shown.offers.is_empty() {
            self.sharing.shown.closed_notice = None;
            if self.sharing.shown.control.active.is_none() {
                self.sharing.shown.control = Default::default();
                self.sharing.shown.control_session = None;
            }
        }
        if role != ShareRole::Viewer { self.sharing.close_picker(); }
        log!("Share offer {role:?}, window {}x{} points", window_size.0, window_size.1);
        self.sharing.last_report = Some(Instant::now());
        self.sharing.logged_keys = self.current.as_ref().and_then(|c| c.stats().peers.iter().find(|p| p.peer == PEER))
            .map_or([0; 4], share_counters).map(Some);
        self.sharing.events.push(ShareEvent::Started { session, title: title.clone(), role });
        let waiting_line = if role == ShareRole::Viewer {
            format!("Waiting for {}'s picture…", self.view().peer_name)
        } else { format!("Waiting for {title}'s picture…") };
        let rights = self.sharing.locals.get(&session).map(|local| local.rights.clone()).unwrap_or_default();
        Arc::make_mut(&mut self.sharing.shown.offers).push(Arc::new(ShareOffer {
            session, title, role, rights, window_size, waiting_line, offered_at: Instant::now(), first_draw: Default::default(),
            sharer_line: None, stopped_line: None, preview_note: None,
        }));
        if let Some(offer) = self.sharing.shown.offer(session).cloned() { self.update_sharer_line(&offer); }
    }

    /// Recomputes the sharer's line; true when it changed.
    fn update_sharer_line(&mut self, offer: &ShareOffer) -> bool {
        let session = offer.session;
        if offer.stopped_line.is_some() { return false }
        let Some(local) = self.sharing.locals.get_mut(&session) else { return false };
        let peer = &self.flow.view().peer_name;
        let peer = if peer.is_empty() { "the other person" } else { peer.as_str() };
        let (captured, why) = local.probe.as_ref().map_or((1, None), |probe| probe());
        let watched_for = local.subscribed_at.map(|t| t.elapsed());
        let waiting = watched_for.filter(|t| captured == 0 && *t >= NO_PICTURE_AFTER).map(|t| t.as_secs());
        let preview_empty = local.preview.as_ref().is_some_and(|(p, since)|
            since.elapsed() >= NO_PICTURE_AFTER && p.probe().stats().received == 0);
        let status = (watched_for.is_some(), waiting, preview_empty);
        if local.line_state == Some(status) && local.line_peer == peer && why.is_none() { return false }
        local.line_state = Some(status);
        if local.line_peer != peer { peer.clone_into(&mut local.line_peer); }
        let problem = why.map(|why| ShareProblem::new(session, &why));
        let mut line = sharer_line(&offer.title, peer, watched_for, captured, problem.as_ref().map(ShareProblem::why));
        if let Some(size) = &local.size_note { line = format!("{line} {size}"); }
        let note = preview_empty.then(|| Arc::<str>::from(format!("{} isn't sending pictures.", offer.title)));
        if let Some(note) = &note { line = format!("{line} {note}"); }
        let offer = self.sharing.shown.offer_mut(session).unwrap();
        offer.preview_note = note;
        offer.sharer_line = Some(line.into());
        if let Some(problem) = problem {
            offer.stopped_line = offer.sharer_line.clone();
            self.send_active(problem.encode());
        }
        true
    }

    pub(super) fn attach_share(&mut self) {
        let Some(call) = &mut self.current else { return };
        let share = &mut self.sharing;
        let send = share.shown.peer_watching.or(share.sending.filter(|s| share.locals.contains_key(s)))
            .or_else(|| share.shown.own_offers().next().map(|o| o.session));
        let receive = share.shown.offers.iter().any(|o| o.role == ShareRole::Viewer);
        // Attach remaining media first: removing the last medium retires the call.
        if send != share.sending {
            if let Some(local) = send.and_then(|s| share.locals.get(&s)) {
                call.add_share(local.source.caps, Box::new(local.source.lease()));
                call.set_peer_view(PEER, local.view);
            }
        }
        if receive && !share.receiving {
            call.set_peer_sharing(PEER, true);
            if share.shown.watching.is_some() {
                call.begin_watch(PEER);
                if share.watch_view.width > 0 { call.watch_share(PEER, share.watch_view); }
            }
        }
        if send.is_none() && share.sending.is_some() { call.remove_share(); }
        if !receive && share.receiving {
            call.set_peer_sharing(PEER, false);
            while call.take_view().is_some() {}
        }
        share.sending = send;
        share.receiving = receive;

    }

    pub(super) fn end_share(&mut self, session: SessionId, reason: ShareEndReason, notify: bool) -> bool {
        let Some(offer) = self.sharing.shown.offer(session).cloned() else { return false };
        self.record_displayed_share();
        use crate::call::control::EndReason;
        let (why, control_end) = match reason {
            ShareEndReason::LinkDropped => (LeaveWhy::Dropped, EndReason::Disconnected),
            ShareEndReason::Stopped | ShareEndReason::AppClosedOrRestarted =>
                (LeaveWhy::Ended, EndReason::ShareEnded),
        };
        // A notice from control that ended earlier in this share goes with the share; only the
        // control this end cuts short keeps its line.
        if self.sharing.shown.control_session == Some(session) {
            if !self.end_control(control_end, notify || (reason != ShareEndReason::LinkDropped && self.local_control())) {
                self.sharing.shown.control.notice = None;
                self.sharing.shown.control_session = None;
            }
        }
        if self.sharing.shown.watching == Some(session) { self.close_watch(session); }
        log!("Share ended {:?}, reason {reason:?}", offer.role);
        let since = if matches!(offer.role, ShareRole::Sharer { .. }) {
            if notify { self.send_share(Message::ShareOff { session, reason }); }
            Some(offer.offered_at)
        } else { offer.first_draw.get().copied() };
        if let Some(since) = since {
            let secs = Some(u32::try_from(since.elapsed().as_secs()).unwrap_or(u32::MAX));
            self.facts.push(Fact::Leave { session, secs, why });
        }
        Arc::make_mut(&mut self.sharing.shown.offers).retain(|o| o.session != session);
        if self.sharing.locals.remove(&session).is_some() { self.sharing.picker_dirty = true; }
        self.sharing.joined.remove(&session);
        if self.sharing.shown.control.requested == Some(session) { self.sharing.shown.control.requested = None; }
        if self.sharing.shown.peer_watching == Some(session) {
            self.sharing.shown.peer_watching = None;
            self.sharing.shown.control.peer_watching = false;
        }
        if self.sharing.previewing == Some(session) {
            self.sharing.previewing = None;
            self.sharing.shown.preview = None;
        }
        if reason == ShareEndReason::AppClosedOrRestarted {
            let notice: Arc<str> = format!("{} closed or restarted.", offer.title).into();
            self.sharing.shown.closed_notice = Some(notice.clone());
            Arc::make_mut(&mut self.sharing.shown.ended).push(EndedShare {
                session, title: Arc::from(offer.title.as_str()), role: offer.role, notice,
            });
        }
        self.attach_share();
        self.sharing.events.push(ShareEvent::Ended { session, reason });
        true
    }

    pub fn dismiss_ended_share(&mut self, session: SessionId) {
        Arc::make_mut(&mut self.sharing.shown.ended).retain(|e| e.session != session);
    }

    fn send_share(&mut self, message: Message) {
        message.send(|bytes| self.send_active(bytes));
    }

    pub(super) fn share_message(&mut self, bytes: &[u8]) -> bool {
        if let Ok(problem) = ShareProblem::decode(bytes) {
            if !self.sharing.shown.offer(problem.session).is_some_and(|o| o.role == ShareRole::Viewer && o.stopped_line.is_none()) { return false }
            let peer = crate::model::clean_name(&self.view().peer_name);
            let peer = if peer.is_empty() { "The other person" } else { peer.as_str() };
            self.sharing.shown.offer_mut(problem.session).unwrap().stopped_line = Some(format!("{peer}'s picture stopped: {}", problem.why()).into());
            if self.sharing.shown.watching != Some(problem.session) { return true }
            self.sharing.shown.picture = None;
            // The panel's text changed even if the encoder never produced its first picture.
            self.sharing.fresh = true;
            return true
        }
        let Ok(Some(signal)) = super::super::decode(bytes) else { return false };
        match signal {
            Message::Share { session, title, w, h, .. } => {
                let size = (w.into(), h.into());
                if let Some(offer) = self.sharing.shown.offer(session) {
                    if offer.role != ShareRole::Viewer || offer.title != title || offer.window_size == size { return false }
                    self.sharing.shown.offer_mut(session).unwrap().window_size = size;
                    return true
                }
                self.begin_offer(session, title, ShareRole::Viewer, size);
                true
            }
            Message::ShareOff { session, reason } if self.sharing.shown.offer(session).is_some_and(|o| o.role == ShareRole::Viewer) =>
                self.end_share(session, reason, false),
            Message::View { session, width, height, fps, .. } if self.sharing.locals.contains_key(&session) => {
                // The window's pixels bound the picture (the share's caps), never a fixed size.
                let view = View { width, height, fps: fps.min(30) };
                let watching = view.width != 0 && view.height != 0 && view.fps != 0;
                let previous = self.sharing.shown.peer_watching;
                if !watching && previous != Some(session) { return false }
                if let Some(old) = previous.filter(|s| !watching || *s != session) {
                    if self.sharing.shown.control.requested == Some(old) { self.sharing.shown.control.requested = None; }
                    if self.sharing.shown.control_session == Some(old) {
                        self.end_control(crate::call::control::EndReason::Released, true);
                    }
                    let local = self.sharing.locals.get_mut(&old).unwrap();
                    local.subscribed_at = None;
                    local.view = View::default();
                    if let Some(offer) = self.sharing.shown.offer(old).cloned() { self.update_sharer_line(&offer); }
                }
                self.sharing.shown.peer_watching = watching.then_some(session);
                self.sharing.shown.control.peer_watching = watching;
                if watching && previous != Some(session) { self.watcher_started(session); }
                let local = self.sharing.locals.get_mut(&session).unwrap();
                if watching { local.subscribed_at.get_or_insert_with(Instant::now); }
                if local.view != view { log!("Share subscription received {view:?}"); }
                local.view = view;
                if self.sharing.sending == Some(session) {
                    if let Some(call) = &mut self.current { call.set_peer_view(PEER, view); }
                } else { self.attach_share(); }
                let offer = self.sharing.shown.offer(session).unwrap().clone();
                self.update_sharer_line(&offer) | (previous != self.sharing.shown.peer_watching)
            }
            _ => false,
        }
    }

    fn viewer_session(&self) -> Option<SessionId> {
        let mut viewers = self.sharing.shown.offers.iter().filter(|o| o.role == ShareRole::Viewer);
        let session = viewers.next()?.session;
        viewers.next().is_none().then_some(session)
    }

    /// A view of a share measured its picture. The guest asks for the larger of its page's and its
    /// pop-out window's sizes; a sharer's own page sizes its preview.
    pub fn share_panel_size(&mut self, session: SessionId, surface: ShareSurface, width: u32, height: u32) {
        if let Some(local) = self.sharing.locals.get_mut(&session) {
            if surface == ShareSurface::Page && width >= 2 && height >= 2 { local.preview_size = Some((width, height)); }
            return
        }
        if self.sharing.shown.watching != Some(session) || width == 0 || height == 0 { return }
        let measured = Some((session, View { width, height, fps: 30 }));
        match surface {
            ShareSurface::Page => self.sharing.measured_panel = measured,
            ShareSurface::Window => self.sharing.window_panel = measured,
        }
        self.subscribe_measured(session);
    }

    fn subscribe_measured(&mut self, session: SessionId) {
        let view = [self.sharing.measured_panel, self.sharing.window_panel].into_iter().flatten()
            .filter(|(s, _)| *s == session)
            .fold(View::default(), |a, (_, b)| View { width: a.width.max(b.width), height: a.height.max(b.height), fps: 30 });
        if view.width > 0 && view.height > 0 && view != self.sharing.watch_view {
            self.sharing.watch_view = view;
            if let Some(call) = &mut self.current { call.watch_share(PEER, view); }
            self.send_views(session);
        }
    }

    /// The picture size this guest's subscription asks the sharer for.
    pub fn share_subscription(&self) -> View { self.sharing.watch_view }

    fn close_watch(&mut self, session: SessionId) {
        self.record_displayed_share();
        if self.sharing.shown.control_session == Some(session) {
            self.end_control(crate::call::control::EndReason::Released, true);
        }
        if self.sharing.shown.control.asking == Some(session) {
            self.ask_control(session, false);
            self.sharing.shown.control.asking = None;
        }
        log!("Share Watch closed");
        self.sharing.shown.watching = None;
        self.sharing.shown.control.allowed = None;
        self.sharing.shown.picture = None;
        self.sharing.shown.overlay = None;
        self.sharing.watch_started = None;
        self.sharing.fresh = true;
        self.sharing.watch_view = View::default();
        self.sharing.window_panel = None;
        if let Some(call) = &mut self.current { call.watch_share(PEER, View::default()); }
        self.send_views(session);
    }

    pub(super) fn watch(&mut self, session: SessionId) -> bool {
        if self.sharing.shown.watching == Some(session) ||
            !self.sharing.shown.offer(session).is_some_and(|o| o.role == ShareRole::Viewer) { return false }
        if let Some(previous) = self.sharing.shown.watching { self.close_watch(previous); }
        log!("Share Watch opened, requesting first key");
        self.sharing.shown.watching = Some(session);
        self.sharing.watch_started = Some(Instant::now());
        if let Some(call) = &mut self.current { call.begin_watch(PEER); }
        self.subscribe_measured(session);
        true
    }

    pub(super) fn share_press(&mut self, press: Press) -> bool {
        let changed = self.handle_share_press(press);
        if changed { self.settle_media(); }
        changed
    }

    fn handle_share_press(&mut self, press: Press) -> bool {
        match press {
            Press::Share(ShareChoice::Menu) if self.linked() => {
                self.sharing.toggle_picker(Purpose::Share);
            }
            Press::Clip | Press::Snap => {
                self.sharing.shown.capture.problem = None;
                if press == Press::Clip && self.sharing.shown.capture.line.is_some() { return false; }
                self.sharing.toggle_picker(if press == Press::Clip { Purpose::Clip } else { Purpose::Snap });
            }
            Press::FirstCaptureWindow if self.sharing.shown.capture.picking => {
                return self.sharing.picker.choose_first();
            }
            Press::StopRecording if self.sharing.shown.capture.line.is_some() => {
                self.sharing.shown.capture.problem = None;
                self.sharing.record_presses.push(RecordPress::Stop);
            }
            Press::Share(ShareChoice::Row(key)) if self.sharing.shown.choices.is_some() => {
                let listed = self.sharing.shown.choices.as_ref().is_some_and(|rows|
                    rows.iter().any(|r| r.choice == ShareChoice::Row(key) && r.selectable));
                if !listed || !self.sharing.picker.choose(key) {
                    let why = Some("The selected window is no longer in the list.".into());
                    if self.sharing.shown.capture.picking { self.sharing.shown.capture.problem = why; }
                    else { self.sharing.shown.problem = why; }
                }
                return true;
            }
            Press::Watch => return self.viewer_session().is_some_and(|s| self.watch(s)),
            Press::CloseWatch => {
                let Some(session) = self.sharing.shown.watching else { return false };
                self.close_watch(session);
            }
            Press::StopShare => {
                let Some(session) = self.only_local_offer() else { return false };
                return self.end_share(session, ShareEndReason::Stopped, true)
            }
            _ => return false,
        }
        true
    }

    pub(super) fn close_share_action(&mut self, session: SessionId, press: Press) -> bool {
        let changed = match press {
            Press::StopShare if self.sharing.locals.contains_key(&session) =>
                self.end_share(session, ShareEndReason::Stopped, true),
            Press::CloseWatch if self.sharing.shown.watching == Some(session) => { self.close_watch(session); true }
            Press::AllowAll(false) => self.change_rights(session, RightChange::All(false)),
            Press::RevokeControl(peer) => self.change_rights(session, RightChange::Remove(peer)),
            Press::TakeBack if self.sharing.shown.control_session == Some(session) && self.local_control() =>
                self.end_control(crate::call::control::EndReason::TakenBack, true),
            Press::ReleaseControl if self.sharing.shown.control_session == Some(session) && !self.local_control() =>
                self.end_control(crate::call::control::EndReason::Released, true),
            _ => false,
        };
        if changed { self.settle_media(); }
        changed
    }

    pub(super) fn peer_can_control(&self, session: SessionId) -> bool {
        self.peer_epoch().zip(self.sharing.locals.get(&session))
            .is_some_and(|((peer, _), local)| local.rights.allows(&peer))
    }

    /// Opening actions arrive here only from the consented local share-page press.
    pub fn allow_control(&mut self, apps: &mut AppLinks, session: SessionId, display_name: &str) -> bool {
        if self.sharing.shown.peer_watching != Some(session)
            || (self.view().phase != Phase::Live && self.sharing.shown.control.requested != Some(session)) { return false }
        let changed = self.change_rights(session, RightChange::Allow(display_name));
        self.grant_control(apps, session) || changed
    }

    pub fn allow_all_control(&mut self, apps: &mut AppLinks, session: SessionId) -> bool {
        if !self.change_rights(session, RightChange::All(true)) { return false }
        self.grant_control(apps, session);
        true
    }

    fn change_rights(&mut self, session: SessionId, change: RightChange<'_>) -> bool {
        use crate::call::control::{ControlMessage, EndReason};
        let Some((peer, _)) = self.peer_epoch() else { return false };
        let Some(local) = self.sharing.locals.get_mut(&session) else { return false };
        let was_allowed = local.rights.allows(&peer);
        match change {
            RightChange::All(on) if local.rights.allow_all != on => Arc::make_mut(&mut local.rights).allow_all = on,
            RightChange::Allow(name) if !local.rights.allowed.contains_key(&peer) => {
                Arc::make_mut(&mut local.rights).allowed.insert(peer, name.to_owned());
            }
            RightChange::Remove(key) if local.rights.allowed.contains_key(&key) => {
                Arc::make_mut(&mut local.rights).allowed.remove(&key);
            }
            _ => return false,
        }
        let allowed = local.rights.allows(&peer);
        self.sharing.shown.offer_mut(session).unwrap().rights = local.rights.clone();
        if was_allowed != allowed {
            self.control_send(ControlMessage::Rights { session, allowed });
            if !allowed && self.sharing.shown.control_session == Some(session) {
                self.end_control(EndReason::TakenBack, true);
            }
        }
        true
    }

    fn only_local_offer(&self) -> Option<SessionId> {
        let mut offers = self.sharing.shown.own_offers();
        let session = offers.next()?.session;
        offers.next().is_none().then_some(session)
    }

    pub(super) fn record_displayed_share(&mut self) {
        if let Some(offer) = self.sharing.shown.watched() {
            if offer.first_draw.get().is_some() && self.sharing.joined.insert(offer.session) {
                log!("Share first picture drawn");
                self.facts.push(Fact::ShareJoin { session: offer.session });
            }
        }
    }

    pub(super) fn poll_share(&mut self) -> bool {
        if self.sharing.shown.offers.is_empty() { return false; }
        self.log_share_progress();
        let mut changed = false;
        let offers = self.sharing.shown.offers.clone();
        for offer in offers.iter().filter(|o| o.role != ShareRole::Viewer) {
            changed |= self.update_sharer_line(offer);
        }
        if let Some(session) = self.sharing.previewing {
            if let Some(local) = self.sharing.locals.get_mut(&session) {
                if let Some(picture) = local.preview.as_ref().and_then(|(p, _)| p.take()) {
                    local.last_preview = Some(picture.clone());
                    self.sharing.shown.preview = Some((session, picture));
                    self.sharing.fresh = true;
                }
            }
        }
        let Some(session) = self.sharing.shown.watching else { return changed };
        let Some(call) = &mut self.current else { return changed };
        if self.sharing.shown.offer(session).is_some_and(|o| o.stopped_line.is_none()) {
            let slot = call.share_picture(PEER);
            let picture = slot.and_then(|s| s.picture.as_ref());
            if slot.is_some_and(|s| s.fresh) || (picture.is_none() && self.sharing.shown.picture.is_some()) {
                let size = picture.map(|p| (p.width, p.height));
                if size != self.sharing.shown.picture.as_ref().map(|p| (p.width, p.height)) {
                    if let (Some((w, h)), Some(watch)) = (size, self.sharing.watch_started.take()) {
                        log!("Share first decoded picture {w}x{h}, {} ms after Watch", watch.elapsed().as_millis());
                    } else { log!("Share decoded picture size {size:?}"); }
                }
                self.sharing.shown.picture = picture.cloned();
                self.sharing.fresh = true;
            }
        }
        self.send_views(session);
        changed
    }

    fn send_views(&mut self, session: SessionId) {
        while let Some((_, view)) = self.current.as_mut().and_then(Call::take_view) {
            log!("Share subscription sent {view:?}");
            let View { width, height, fps } = view;
            self.send_share(Message::View { session, width, height, fps, ext: Default::default() });
        }
    }

    fn log_share_progress(&mut self) {
        let Some(last) = self.sharing.last_report else { return };
        let due = last.elapsed().as_secs() >= 10;
        let Some(stats) = self.current.as_ref().map(Call::stats) else { return };
        let Some(peer) = stats.peers.iter().find(|p| p.peer == PEER) else {
            if due {
                log!("Share awaiting media peer");
                self.sharing.last_report = Some(Instant::now());
            }
            return;
        };
        let counts = share_counters(peer);
        // Report the first key events promptly. Further keys and errors are totals on the
        // ten-second line, including when every picture is a key or the decoder repeatedly fails.
        for (i, what) in ["key request sent", "key request received", "key sent", "decode error"].iter().enumerate() {
            if self.sharing.logged_keys[i].is_some_and(|baseline| counts[i] > baseline) {
                log!("Share {what}, total {}", counts[i]);
                self.sharing.logged_keys[i] = None;
            }
        }
        if due {
            log!("Share frames: (captured, encoded) {:?}, sent {}, fragments received {}, frames received {}, decoded {}, decode errors {}, keys sent {}, last key paced in {} ms, key requests sent/received {}/{} (sent for {:?}), pacing queue {} bytes, link backlog {}, share kbps {:?}, link estimate {} kbit/s, rtt {:?} ms, loss {:.3}, {}",
                peer.share_source_counts, peer.session.share_frames_sent, peer.share_received.fragments, peer.share_received.frames,
                peer.share_decoded, counts[3], counts[2], peer.session.last_share_key_pacing_us / 1000, counts[0], counts[1], peer.session.share_request_causes, peer.paced_bytes, peer.backlog, stats.fixed_cell_share_kbps,
                peer.estimate_bps / 1000, peer.rtt_us.map(|us| us / 1000), peer.random_loss, self.takeover.input.summary());
            self.sharing.last_report = Some(Instant::now());
        }
    }

    /// Only the local window open in the center needs a preview stream.
    pub fn sync_share_preview(&mut self, apps: &mut AppLinks, session: Option<SessionId>) -> bool {
        let session = session.filter(|s| self.sharing.locals.contains_key(s));
        let changed = self.sharing.previewing != session;
        if changed {
            if let Some(old) = self.sharing.previewing.take() {
                if let Some(local) = self.sharing.locals.get_mut(&old) { local.preview = None; }
                if let Some(offer) = self.sharing.shown.offer_mut(old) { offer.preview_note = None; }
            }
            self.sharing.shown.preview = None;
            self.sharing.previewing = session;
        }
        if let Some((session, (link, window))) = session.and_then(|s| Some((s, self.sharer_window(s)?))) {
            let local = self.sharing.locals.get_mut(&session).unwrap();
            if let Some((w, h)) = local.preview_size {
                if let Some((preview, _)) = &local.preview { preview.want(w, h, PREVIEW_FPS); }
                else {
                    let want = PictureWant { window, max_w: w, max_h: h, fps: PREVIEW_FPS, kbps: 0 };
                    local.preview = apps.open_preview(link, want).map(|p| (p, Instant::now()));
                }
            }
        }
        changed
    }

    pub fn publish_share_picture(&mut self, shown: &mut ShareShown) -> bool {
        if !std::mem::take(&mut self.sharing.fresh) { return false }
        if shown.watching == self.sharing.shown.watching { shown.picture.clone_from(&self.sharing.shown.picture); }
        shown.preview.clone_from(&self.sharing.shown.preview);
        true
    }

    /// A new decoded picture or the loss of the previous handle, once per change.
    pub fn take_share_picture_update(&mut self) -> Option<Option<Arc<Picture>>> {
        std::mem::take(&mut self.sharing.fresh).then(|| self.sharing.shown.picture.clone())
    }

    pub fn take_fresh_share_picture(&mut self) -> Option<Arc<Picture>> {
        self.take_share_picture_update().flatten()
    }

}

#[cfg(test)]
#[path = "share_tests.rs"]
mod tests;
