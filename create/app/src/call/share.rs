//! Window offers on the authenticated link. The first displayed picture joins its timeline.
use crate::call::share_codec::{self, share_format, EncoderFormats};
use super::*;
use crate::app_link::{AppLinkEvent, AppLinks, AppSound, AppWindow, EncodedSound, LinkId, PicturePreview, PictureProbe, PictureStream, PictureWant, WindowId};
use crate::record::{Recorder, RecordPress, capture::{CaptureShown, Purpose, RecordingClock, WindowPicker, WindowTarget}};
use makepad_call::driver::{codec::{Packet, Picture, StreamFormat}, Content, EncodedSource, VideoCaps, View};
use makepad_call::driver::share_audio::{ShareSound, ShareSpeakerControl};
use makepad_town_core::event::{LeaveWhy, SessionId};
use super::super::{signal::ShareProblem, Message};
use crate::call::ShareSurface;
use std::{cell::Cell, collections::{BTreeMap, BTreeSet}, sync::OnceLock, time::Duration};

/// A watched share that has captured nothing for this long says so on the sharer's row.
pub(super) const NO_PICTURE_AFTER: Duration = Duration::from_secs(3);
/// The sharer's own view needs to follow the app, not to match the guest's frame rate.
const PREVIEW_FPS: u32 = 15;
const SOUND_FRAME_MS: u32 = 10;
const MAX_SOUND_DELAY_MS: u32 = 300;
const MIN_SOUND_FRAMES: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareRole {
    Sharer { link: LinkId, window: WindowId },
    Viewer { sharer: PublicKey },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareEvent {
    Started { session: SessionId, town: TownId, title: String, role: ShareRole },
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
    pub town: TownId,
    pub sharer_name: String,
    pub watchers: Vec<(PublicKey, String)>,
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
    pub(super) decode_problem: Option<Arc<str>>,
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
    pub watching_sharer: Option<PublicKey>,
    pub watchers: Vec<(PublicKey, String)>,
    pub control_session: Option<SessionId>,
    /// The preview of the local window whose page is open. The guest picture is separate.
    pub(super) preview: Option<(SessionId, Arc<Picture>)>,
}

impl ShareShown {
    pub fn offer(&self, session: SessionId) -> Option<&Arc<ShareOffer>> {
        self.offers.iter().find(|o| o.session == session)
    }

    pub(super) fn offer_mut(&mut self, session: SessionId) -> Option<&mut ShareOffer> {
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
        if self.watching != Some(offer.session) { return None }
        if let Some(line) = &offer.stopped_line { Some(line) }
        else if !self.media_held { Some("Connecting securely…") }
        else if self.picture.is_none() { Some(offer.decode_problem.as_deref().unwrap_or(&offer.waiting_line)) }
        else { None }
    }
}

impl ShareOffer {
    pub fn allow_all(&self) -> bool { self.rights.allow_all }
    pub fn allowed(&self) -> &BTreeMap<PublicKey, String> { &self.rights.allowed }

    /// The center panel acknowledges the first presentation of this offer once.
    pub(crate) fn picture_drawn(&self) { self.first_draw.get_or_init(Instant::now); }
}

pub(super) struct Sharing {
    decode_caps: MediaCaps,
    failed: MediaCaps,
    pub(super) decode_probe: share_codec::DecodeProbe,
    sound_on: bool,
    speaker: ShareSpeakerControl,
    playing: Option<Arc<ShareSound>>,
    // Listener identity and the watcher-clock time of its first received audio.
    sync_sound: Option<(Arc<ShareSound>, u64)>,
    // The watched picture can outlive a listening interval, including while the app is silent.
    sync_last_capture_us: Option<u64>,
    sync_v_us: Option<f64>,
    sync_target: Option<u32>,
    sound_ahead_logged: bool,
    // Close watch clears the offer before the next poll logs the stop.
    playing_title: String,
    encoder: EncoderFormats,
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
    sent_views: BTreeMap<PeerId, (SessionId, View)>,
    sending_sound: Option<SessionId>,
    sent_sound: BTreeMap<PeerId, bool>,
    receiving_peers: BTreeMap<PublicKey, PeerId>,
    previewing: Option<SessionId>,

    fresh: bool,
    last_report: Option<Instant>,
    logged_keys: BTreeMap<PeerId, [Option<u64>; 4]>,
    watch_started: Option<Instant>,
}

impl Sharing {
    pub(super) fn new(speaker: ShareSpeakerControl) -> Self {
        Self {
            decode_caps: share_codec::decode_caps(true, false), failed: MediaCaps::NONE,
            decode_probe: Default::default(), sound_on: true, speaker, playing: None, playing_title: String::new(),
            sync_sound: None, sync_last_capture_us: None, sync_v_us: None, sync_target: None, sound_ahead_logged: false,
            encoder: EncoderFormats::local(),
            shown: Default::default(), events: Vec::new(), revokes: Vec::new(), picker: Default::default(),
            picker_dirty: false, record_presses: Vec::new(), clock: Default::default(), joined: BTreeSet::new(),
            watch_view: View::default(), measured_panel: None, window_panel: None, locals: BTreeMap::new(),
            sending: None, sent_views: BTreeMap::new(), sending_sound: None, sent_sound: BTreeMap::new(),
            receiving_peers: BTreeMap::new(), previewing: None,
            fresh: false, last_report: None, logged_keys: BTreeMap::new(), watch_started: None,
        }
    }

    fn clear_sound_sync(&mut self) {
        self.sync_sound = None;
        self.sync_v_us = None;
        self.sync_target = None;
    }

    fn update_sound_target(&mut self, arrival: Option<(&Arc<ShareSound>, i64)>, now_us: u64, picture_us: Option<u64>) {
        let picture_us = picture_us.filter(|&us| self.sync_last_capture_us.is_none_or(|last| us > last));
        if picture_us.is_some() { self.sync_last_capture_us = picture_us; }
        let Some((sound, base_us)) = arrival else { return };
        let first_us = match &self.sync_sound {
            Some((previous, first)) if Arc::ptr_eq(previous, sound) => *first,
            _ => {
                self.sync_sound = Some((sound.clone(), now_us));
                self.sync_v_us = None;
                self.sync_target = None;
                now_us
            }
        };
        if let Some(capture_us) = picture_us {
            let sample = (i128::from(now_us) - i128::from(capture_us)) as f64;
            self.sync_v_us = Some(self.sync_v_us.map_or(sample, |old| old + (sample - old) / 8.0));
        }
        let target = if let Some(video) = self.sync_v_us {
            // Both delays include the same offset between the app and watcher clocks.
            let delay_ms = (video - base_us as f64) / 1_000.0;
            if delay_ms > f64::from(MAX_SOUND_DELAY_MS) && !self.sound_ahead_logged {
                if let Some(offer) = self.shown.watched() {
                    let ms = delay_ms.ceil() as u64;
                    log!("Share {}: the picture runs {ms} ms behind the sound; the sound waits at most {MAX_SOUND_DELAY_MS} ms, so it plays {} ms ahead of the picture",
                        offer.title, ms - u64::from(MAX_SOUND_DELAY_MS));
                    self.sound_ahead_logged = true;
                }
            }
            ((delay_ms / f64::from(SOUND_FRAME_MS)).ceil() as u32).clamp(MIN_SOUND_FRAMES, MAX_SOUND_DELAY_MS / SOUND_FRAME_MS)
        } else if now_us - first_us >= 500_000 {
            MIN_SOUND_FRAMES
        } else {
            return
        };
        if self.sync_target != Some(target) {
            self.speaker.set_target(target);
            self.sync_target = Some(target);
        }
    }

    pub(super) fn wants_speaker(&self) -> bool { self.playing.is_some() }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Viewer {
    view: View,
    caps: MediaCaps,
    audio: bool,
}

impl Viewer {
    fn wants_sound(&self) -> bool { self.audio && view_open(self.view) }
}

struct LocalShare {
    town: TownId,
    recipients: BTreeSet<PublicKey>,
    viewers: BTreeMap<PublicKey, Viewer>,
    /// The last known decode caps of each recipient that opened a View. The stream's format
    /// follows these seats, not the open Views, so a View that repeats, carries no caps, or
    /// closes and reopens never lifts it; a seat goes when its peer stops receiving the share.
    /// A capless seat stays `None` until it explicitly lists its decoders, even an empty set.
    seats: BTreeMap<PublicKey, Option<MediaCaps>>,
    format: StreamFormat,
    holders: Vec<PublicKey>,
    lost: Vec<StreamFormat>,
    picture: Option<PictureProbe>,
    sound: Option<AppSound>,
    app_name: String,
    dpi: f64,
    stopped_problem: Option<Vec<u8>>,
    rights: Arc<ShareRights>,
    rights_revision: u64,
    source: OfferSource,
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
    line_state: Option<(bool, Option<u64>, bool, StreamFormat)>,
    /// Why the picture goes out smaller than the window, when the encoder bounds it.
    size_note: Option<String>,
}

impl LocalShare {
    fn holder_reason(&self, encoder: EncoderFormats, flow: &CallFlow) -> Option<String> {
        if self.holders.is_empty() { return None }
        let better = encoder.available(&self.lost).next()?;
        let names = self.holders.iter().map(|p| flow.peer_name(p).unwrap_or_default()).collect::<Vec<_>>().join(", ");
        Some(format!("{names} can't decode {}", better.label()))
    }

    fn lost_limit(&self, encoder: EncoderFormats) -> Option<StreamFormat> {
        StreamFormat::ORDER.into_iter().take_while(|&f| f != self.format)
            .find(|&f| self.lost.contains(&f) && encoder.makes(f) && self.seats.values().all(|caps| share_codec::decodes(caps.unwrap_or_default(), f)))
    }
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
    Allow(PublicKey, &'a str),
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

fn share_signal(offer: &ShareOffer, town: TownId, size: (f64, f64)) -> Message {
    Message::Share { session: offer.session, title: offer.title.clone(), w: size.0 as f32, h: size.1 as f32,
        ext: super::super::ShareOfferExt { town: Some(town), ..Default::default() } }
}

fn view_open(view: View) -> bool { view.width != 0 && view.height != 0 && view.fps != 0 }

fn decoder_list(caps: MediaCaps, empty: &str) -> String {
    let formats: Vec<_> = [
        (MediaCaps::H264_DECODE, StreamFormat::H264), (MediaCaps::H264_444_DECODE, StreamFormat::H264_FULL),
        (MediaCaps::HEVC_DECODE, StreamFormat::HEVC), (MediaCaps::HEVC_444_DECODE, StreamFormat::HEVC_FULL),
    ].into_iter().filter(|(bit, _)| caps.supports(*bit)).map(|(_, f)| f.label()).collect();
    if formats.is_empty() { empty.into() } else { formats.join(", ") }
}

fn share_counters(peer: &makepad_call::driver::PeerStats) -> [u64; 4] {
    [peer.session.share_requests_sent, peer.session.share_requests_received, peer.session.share_keys_sent, peer.share_decode_errors]
}

/// The largest picture of a share: the window's own pixels, bounded only by the largest picture
/// this computer's encoder takes, when it says. A slow link lowers the frame rate, never this
/// size. A bound is never silent: the second value says so, for the log and the sharer's page.
/// The largest picture this computer's share encoder takes, when the platform says.
fn encoder_max(format: StreamFormat) -> Option<[u32; 2]> {
    use makepad_video::StreamVideoCodec::{H264, Hevc};
    makepad_video::VideoStreamEncoder::max_size(if format.hevc { Hevc } else { H264 }, format.full_chroma)
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
    fn attach_sound(&mut self, call: &mut Call, send: Option<SessionId>, apps: Option<&mut AppLinks>) {
        let wanted = send.filter(|s| self.locals.get(s).and_then(|l| l.sound.as_ref())
            .is_some_and(|sound| sound.ended_why().is_none()));
        if self.sending_sound == wanted { return }
        if self.sending_sound.is_some() {
            call.remove_share_sound();
            self.sound_off();
        }
        if let (Some(session), Some(apps)) = (wanted, apps) {
            let offer = self.shown.offer(session).unwrap();
            let ShareRole::Sharer { link, .. } = offer.role else { return };
            if let Some(sound) = apps.open_sound(link) {
                call.add_share_sound(Box::new(EncodedSound::new(sound)));
                self.sending_sound = Some(session);
                let n = self.locals[&session].viewers.values().filter(|v| v.wants_sound()).count();
                log!("Share {}: sending the app's sound to {n} watcher(s)", offer.title);
            }
        }
    }

    fn sound_off(&mut self) {
        if let Some(offer) = self.sending_sound.take().and_then(|s| self.shown.offer(s)) {
            log!("Share {}: app sound off", offer.title);
        }
    }

    pub(super) fn detach(&mut self) {
        self.sound_off();
        self.sending = None;
        self.sent_views.clear();
        self.sent_sound.clear();
        self.receiving_peers.clear();
        self.logged_keys.clear();
        self.fresh |= self.shown.picture.take().is_some();
    }

    pub(super) fn detach_peer(&mut self, peer: PublicKey, old_id: PeerId) {
        self.sent_views.remove(&old_id);
        self.sent_sound.remove(&old_id);
        self.logged_keys.remove(&old_id);
        if self.receiving_peers.remove(&peer).is_some() {
            self.fresh |= self.shown.picture.take().is_some();
        }
    }

    pub(super) fn peer_needs_share(&self, peer: &PublicKey) -> bool {
        (self.shown.watching_sharer == Some(*peer) && self.shown.watched().is_some_and(|o| o.stopped_line.is_none()))
            || self.locals.iter().any(|(session, local)| local.viewers.contains_key(peer)
                && self.shown.offer(*session).is_some_and(|o| o.stopped_line.is_none()))
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
            CallAction::Press(Press::Share(_)) => return self.can_share(),
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

    fn sync_share_sound(&mut self, apps: &mut AppLinks) {
        let share = &mut self.sharing;
        for offer in share.shown.own_offers() {
            let ShareRole::Sharer { link, .. } = offer.role else { continue };
            let local = share.locals.get_mut(&offer.session).unwrap();
            if local.picture.is_some() && offer.stopped_line.is_none() && local.viewers.values().any(Viewer::wants_sound) {
                if local.sound.is_none() { local.sound = apps.open_sound(link); }
            } else {
                local.sound = None;
            }
        }
        if let Some(call) = &mut self.current { share.attach_sound(call, share.sending, Some(apps)); }
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
                            changed |= self.resize_share(offer, w.size, w.dpi);
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
        self.sync_share_sound(apps);
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
            ShareRole::Viewer { .. } => None,
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

    fn resize_share(&mut self, offer: &ShareOffer, size: (f64, f64), dpi: f64) -> bool {
        let session = offer.session;
        let Some(local) = self.sharing.locals.get_mut(&session) else { return false };
        let Some((caps, note)) = caps(size, dpi, encoder_max(local.format)) else { return false };
        local.dpi = dpi;
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
                    self.sharing.sent_views.clear();
                    self.sharing.sent_sound.clear();
                    self.attach_share();
                }
            }
        }
        if offer.window_size == size { return false }
        self.sharing.shown.offer_mut(session).unwrap().window_size = size;
        let local = &self.sharing.locals[&session];
        self.broadcast_share(&local.recipients.clone(), share_signal(offer, local.town, size));
        true
    }

    fn open_share_window(&mut self, apps: &mut AppLinks, link: LinkId, window: WindowId) -> Result<(), String> {
        let title = crate::record::capture::window_label(apps, link, window)
            .ok_or("The selected window is no longer visible.")?;
        self.open_named_share_window(apps, link, window, title)
    }

    fn open_named_share_window(&mut self, apps: &mut AppLinks, link: LinkId, window: WindowId, title: String) -> Result<(), String> {
        let app = apps.state(link).ok_or("The selected app is no longer linked.")?;
        let app_name = app.app.name.clone();
        let w = app.windows.iter().find(|w| w.id == window).cloned()
            .ok_or("The selected window is no longer linked.")?;
        let source = apps.open_picture(link, PictureWant { window, max_w: 0, max_h: 0, fps: 0, kbps: 0, format: Some(StreamFormat::H264) })
            .ok_or("The app link could not open a picture stream.")?;
        let probe = source.probe();
        let session = self.start_window(link, &w, title, Box::new(source))?;
        let local = self.sharing.locals.get_mut(&session).unwrap();
        local.picture = Some(probe.clone());
        local.app_name = app_name;
        local.probe = Some(Box::new(move || (probe.stats().received, probe.ended_why())));
        self.settle_format(session);
        Ok(())
    }

    pub(super) fn start_window(&mut self, link: LinkId, window: &AppWindow, title: String, source: Box<dyn EncodedSource>) -> Result<SessionId, String> {
        let town = self.open_town().ok_or("Open a town to share a window.")?.0;
        let recipients = self.share_recipients(town);
        if recipients.is_empty() { return Err("There is nobody linked to share with in this town.".into()) }
        let format = StreamFormat::H264;
        let (caps, size_note) = caps(window.size, window.dpi, encoder_max(format)).ok_or("The selected window has no usable picture size.")?;
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
            rights: Arc::default(), rights_revision: 0,
            source: OfferSource::new(caps, source), town, recipients: recipients.clone(), viewers: BTreeMap::new(), seats: BTreeMap::new(),
            format, holders: Vec::new(), lost: Vec::new(), picture: None, sound: None, app_name: String::new(), dpi: window.dpi,
            probe: None, stopped_problem: None,
            preview: None, preview_size: None, last_preview: None, subscribed_at: None, line_peer: String::new(), line_state: None,
            size_note,
        });
        self.refresh_watchers(session);
        self.begin_offer(session, town, title.clone(), role, window.size);
        self.facts.push(Fact::ShareOpen { session, title: title.clone(), town });
        self.broadcast_share(&recipients, Message::Share { session, title, w, h, ext: super::super::ShareOfferExt { town: Some(town), ..Default::default() } });
        self.settle_media();
        Ok(session)
    }

    fn begin_offer(&mut self, session: SessionId, town: TownId, title: String, role: ShareRole, window_size: (f64, f64)) {
        if self.sharing.shown.offers.is_empty() {
            self.sharing.shown.closed_notice = None;
            if self.sharing.shown.control.active.is_none() {
                self.sharing.shown.control = Default::default();
                self.sharing.shown.control_session = None;
            }
        }
        if matches!(role, ShareRole::Sharer { .. }) { self.sharing.close_picker(); }
        log!("Share offer {role:?}, window {}x{} points", window_size.0, window_size.1);
        self.sharing.last_report = Some(Instant::now());
        self.sharing.events.push(ShareEvent::Started { session, town, title: title.clone(), role });
        let sharer_name = match role {
            ShareRole::Viewer { sharer } => self.peer_display_name(&sharer),
            ShareRole::Sharer { .. } => String::new(),
        };
        let waiting_line = match role {
            ShareRole::Viewer { .. } => format!("Waiting for {sharer_name}'s picture…"),
            ShareRole::Sharer { .. } => format!("Waiting for {title}'s picture…"),
        };
        let rights = self.sharing.locals.get(&session).map(|local| local.rights.clone()).unwrap_or_default();
        Arc::make_mut(&mut self.sharing.shown.offers).push(Arc::new(ShareOffer {
            session, town, title, role, sharer_name, watchers: Vec::new(), rights, window_size, waiting_line, offered_at: Instant::now(), first_draw: Default::default(),
            sharer_line: None, stopped_line: None, preview_note: None, decode_problem: None,
        }));
        if let Some(offer) = self.sharing.shown.offer(session).cloned() { self.update_sharer_line(&offer); }
    }

    /// Recomputes the sharer's line; true when it changed.
    fn update_sharer_line(&mut self, offer: &ShareOffer) -> bool {
        let session = offer.session;
        if offer.stopped_line.is_some() { return false }
        let Some(local) = self.sharing.locals.get_mut(&session) else { return false };
        let peer = local.line_peer.as_str();
        let (captured, why) = local.probe.as_ref().map_or((1, None), |probe| probe());
        let watched_for = local.subscribed_at.map(|t| t.elapsed());
        let waiting = watched_for.filter(|t| captured == 0 && *t >= NO_PICTURE_AFTER).map(|t| t.as_secs());
        let preview_why = local.preview.as_ref().and_then(|(p, _)| p.probe().ended_why());
        let preview_empty = preview_why.is_none() && local.preview.as_ref().is_some_and(|(p, since)|
            since.elapsed() >= NO_PICTURE_AFTER && p.probe().stats().received == 0);
        let format = local.picture.as_ref().and_then(PictureProbe::format).unwrap_or(local.format);
        let status = (watched_for.is_some(), waiting, preview_empty, format);
        if local.line_state == Some(status) && why.is_none() && (preview_empty || offer.preview_note == preview_why) { return false }
        local.line_state = Some(status);
        let problem = why.map(|why| ShareProblem::new(session, &why));
        let mut line = sharer_line(&offer.title, peer, watched_for, captured, problem.as_ref().map(ShareProblem::why));
        if watched_for.is_none() && problem.is_none() && local.recipients.len() > 1 {
            line = format!("Sharing {} with {peer}. Nobody has opened it yet.", offer.title);
        }
        if let Some(size) = &local.size_note { line = format!("{line} {size}"); }
        let note = preview_why.or_else(|| preview_empty.then(|| Arc::<str>::from(format!("{} isn't sending pictures.", offer.title))));
        if let Some(note) = &note { line = format!("{line} {note}"); }
        if watched_for.is_some() && problem.is_none() {
            line.push(' ');
            line.push_str(format.label());
            if format == local.format {
                let encoder = self.sharing.encoder;
                if let Some(reason) = local.holder_reason(encoder, &self.flow) {
                    line.push_str(&format!(": {reason}"));
                } else if let Some(lost) = local.lost_limit(encoder) {
                    line.push_str(&format!(": {} couldn't encode {}", local.app_name, lost.label()));
                }
            }
            line.push('.');
        }
        let offer = self.sharing.shown.offer_mut(session).unwrap();
        offer.preview_note = note;
        offer.sharer_line = Some(line.into());
        if let Some(problem) = problem {
            offer.stopped_line = offer.sharer_line.clone();
            let bytes = problem.encode();
            local.stopped_problem = Some(bytes.clone());
            for peer in local.recipients.clone() { self.send_to(peer, bytes.clone()); }

        }
        true
    }

    pub(super) fn attach_share(&mut self) {
        let Some(call) = &mut self.current else { return };
        let share = &mut self.sharing;
        let live = |id: &SessionId| share.shown.offer(*id).is_some_and(|o| o.stopped_line.is_none());
        let watched = |id: &SessionId| share.locals.get(id).is_some_and(|l| !l.viewers.is_empty());
        let send = share.sending.filter(live).filter(watched)
            .or_else(|| share.locals.keys().copied().find(|s| live(s) && watched(s)))
            .or_else(|| share.sending.filter(live))
            .or_else(|| share.shown.own_offers().find(|o| o.stopped_line.is_none()).map(|o| o.session));
        let receive = share.shown.watched().filter(|o| o.stopped_line.is_none())
            .and(share.shown.watching_sharer)
            .and_then(|p| self.links.get(&p).filter(|l| l.media_held.is_some()).map(|l| (p, l.peer_id)));

        share.attach_sound(call, send, None);
        if send != share.sending {
            if let Some(local) = send.and_then(|s| share.locals.get(&s)) {
                call.add_share(local.source.caps, Box::new(local.source.lease()));
                share.sent_views.clear();
                share.sent_sound.clear();
            }
        }
        if let Some((peer, id)) = receive {
            if share.receiving_peers.get(&peer) != Some(&id) {
                call.set_peer_sharing(id, true);
                call.begin_watch(id);
                call.listen_share(id, share.sound_on);
                if view_open(share.watch_view) { call.watch_share(id, share.watch_view); }
            }
        }
        if let Some(session) = send {
            let local = &share.locals[&session];
            let held = |id: &PeerId| self.id_to_peer.get(id)
                .and_then(|p| self.links.get(p)).is_some_and(|l| l.media_held.is_some());
            share.sent_views.retain(|id, _| held(id));
            share.sent_sound.retain(|id, _| held(id));
            for (peer, link) in &self.links {
                if link.media_held.is_none() { continue }
                let id = link.peer_id;
                let viewer = local.viewers.get(peer);
                let sound = viewer.is_some_and(Viewer::wants_sound);
                if share.sent_sound.get(&id) != Some(&sound) {
                    call.set_peer_sound(id, sound);
                    share.sent_sound.insert(id, sound);
                }
                match viewer.map(|v| v.view).filter(|v| view_open(*v)) {
                    Some(view) if share.sent_views.get(&id) != Some(&(session, view)) => {
                        call.set_peer_view(id, view);
                        share.sent_views.insert(id, (session, view));
                    }
                    None if share.sent_views.remove(&id).is_some() => call.set_peer_view(id, View::default()),
                    _ => {}
                }
            }
        } else if share.sending.is_some() {
            call.remove_share();
            share.sent_views.clear();
            share.sent_sound.clear();
        }
        share.receiving_peers.retain(|&p, id| {
            if receive == Some((p, *id)) { return true }
            call.listen_share(*id, false);
            if self.links.get(&p).is_some_and(|l| l.peer_id == *id && l.media_held.is_some()) {
                call.set_peer_sharing(*id, false);
            }
            false
        });
        if let Some((peer, id)) = receive { share.receiving_peers.insert(peer, id); }
        share.sending = send;
        self.send_views();
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
        // control this end cuts short keeps its line. ShareOff carries that end to each
        // recipient on the ordered link. A second end on the input topic can erase the notice.
        if self.sharing.shown.control_session == Some(session) {
            if !self.end_control(control_end, false) {
                self.sharing.shown.control.notice = None;
                self.sharing.shown.control_session = None;
            }
        }
        if self.sharing.shown.watching == Some(session) { self.close_watch(session); }
        log!("Share ended {:?}, reason {reason:?}", offer.role);
        let since = if matches!(offer.role, ShareRole::Sharer { .. }) {
            if notify { self.broadcast_share(&self.sharing.locals[&session].recipients.clone(), Message::ShareOff { session, reason }); }
            Some(offer.offered_at)
        } else { offer.first_draw.get().copied() };
        if let Some(since) = since {
            let secs = Some(u32::try_from(since.elapsed().as_secs()).unwrap_or(u32::MAX));
            self.facts.push(Fact::Leave { session, secs, why });
        }
        if self.sharing.sending_sound == Some(session) {
            if let Some(call) = &mut self.current { self.sharing.attach_sound(call, None, None); }
        }
        Arc::make_mut(&mut self.sharing.shown.offers).retain(|o| o.session != session);
        if self.sharing.locals.remove(&session).is_some() { self.sharing.picker_dirty = true; }
        self.sharing.joined.remove(&session);
        self.sharing.shown.control.requests.retain(|(id, ..)| *id != session);
        self.takeover.new_watchers.retain(|(id, _)| *id != session);
        self.refresh_watchers(session);
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

    pub fn can_share(&self) -> bool {
        self.open_town.as_ref().is_some_and(|(t, _)| self.flow.town_members(*t)
            .is_some_and(|members| members.iter().any(|p| self.is_share_peer_in_town(*t, p))))
    }

    pub(super) fn share_recipients(&self, town: TownId) -> BTreeSet<PublicKey> {
        self.flow.town_members(town).into_iter().flatten().copied()
            .filter(|p| self.is_share_peer_in_town(town, p)).collect()
    }

    pub(super) fn is_share_peer_in_town(&self, town: TownId, peer: &PublicKey) -> bool {
        self.flow.is_town_member(town, peer) && self.links.get(peer)
            .is_some_and(|l| l.caps.supports(MediaCaps::MULTIPARTY_SHARE_V1))
    }

    pub(super) fn peer_display_name(&self, peer: &PublicKey) -> String {
        self.flow.peer_name(peer).unwrap_or_default().to_owned()
    }

    fn send_share_to(&mut self, peer: PublicKey, message: Message) {
        message.send(|bytes| self.send_to(peer, bytes));
    }

    fn broadcast_share(&mut self, peers: &BTreeSet<PublicKey>, message: Message) {
        message.send(|bytes| { for &peer in peers { self.send_to(peer, bytes.clone()); } });
    }

    fn settle_format(&mut self, session: SessionId) {
        let encoder = self.sharing.encoder;
        let Some(local) = self.sharing.locals.get_mut(&session) else { return };
        let Some(picture) = &local.picture else { return }; // Other sources supply H.264.
        let (format, holders) = share_format(encoder, &local.lost, local.seats.iter().map(|(&p, caps)| (p, caps.unwrap_or_default())));
        if holders != local.holders {
            local.holders = holders;
            local.line_state = None;
        }
        if format == local.format { return }
        local.format = format;
        local.line_state = None;
        picture.set_format(format);
        let offer = self.sharing.shown.offer(session).unwrap().clone();
        let reason = local.holder_reason(encoder, &self.flow);
        if let Some(why) = reason { log!("Share {}: {}, {why}", offer.title, format.label()); }
        else { log!("Share {}: {}", offer.title, format.label()); }
        let dpi = local.dpi;
        self.resize_share(&offer, offer.window_size, dpi);
    }

    fn refresh_watchers(&mut self, session: SessionId) {
        if let Some(local) = self.sharing.locals.get_mut(&session) {
            let viewers: Vec<_> = local.viewers.keys().map(|p|
                (*p, self.flow.peer_name(p).unwrap_or_default().to_owned())).collect();
            let names = if viewers.is_empty() {
                local.recipients.iter().map(|p| self.flow.peer_name(p).unwrap_or_default()).collect::<Vec<_>>()
            } else { viewers.iter().map(|(_, name)| name.as_str()).collect() };
            let name = names.join(", ");
            local.line_peer = if name.is_empty() { "the other person".into() } else { name };
            local.line_state = None;
            if let Some(offer) = self.sharing.shown.offer_mut(session) { offer.watchers = viewers; }
        }
        self.sharing.shown.watchers = self.sharing.shown.own_offers().flat_map(|o| o.watchers.iter().cloned())
            .collect::<BTreeMap<_, _>>().into_iter().collect();
        if let Some(offer) = self.sharing.shown.offer(session).cloned() { self.update_sharer_line(&offer); }
    }

    fn remove_viewer(&mut self, session: SessionId, peer: PublicKey, why: crate::call::control::EndReason) {
        let local = self.sharing.locals.get_mut(&session).unwrap();
        local.viewers.remove(&peer);
        let seat_left = !local.recipients.contains(&peer) && local.seats.remove(&peer).is_some();
        if local.viewers.is_empty() { local.subscribed_at = None; }
        self.sharing.shown.control.requests.retain(|(s, p, _)| (*s, *p) != (session, peer));
        self.takeover.new_watchers.retain(|entry| *entry != (session, peer));
        if self.sharing.shown.control_session == Some(session)
            && self.sharing.shown.control.holder.as_ref().is_some_and(|(p, _)| *p == peer) {
            use crate::call::control::EndReason;
            self.end_control(why, !matches!(why, EndReason::Disconnected | EndReason::ShareEnded));
        }
        if seat_left { self.settle_format(session); }
        self.refresh_watchers(session);
    }

    fn close_unavailable_picker(&mut self) -> bool {
        if self.can_share() || self.sharing.shown.capture.picking || self.sharing.shown.choices.is_none() { return false }
        self.sharing.close_picker();
        true
    }

    pub(super) fn sync_share_members(&mut self) -> bool {
        use crate::call::control::EndReason;
        let offers = self.sharing.shown.offers.clone();
        let mut changed = self.close_unavailable_picker();
        for offer in offers.iter() {
            let session = offer.session;
            if let ShareRole::Viewer { sharer } = offer.role {
                if !self.is_share_peer_in_town(offer.town, &sharer) {
                    changed |= self.end_share(session, ShareEndReason::Stopped, false);
                }
                continue
            }
            let town = self.sharing.locals[&session].town;
            let eligible = self.share_recipients(town);
            let local = self.sharing.locals.get_mut(&session).unwrap();
            if eligible == local.recipients { continue }
            let old = std::mem::replace(&mut local.recipients, eligible.clone());
            let problem = local.stopped_problem.clone();
            for &peer in eligible.difference(&old) {
                self.send_share_to(peer, share_signal(offer, town, offer.window_size));
                if let Some(bytes) = &problem { self.send_to(peer, bytes.clone()); }
            }
            for &peer in old.difference(&eligible) {
                self.send_share_to(peer, Message::ShareOff { session, reason: ShareEndReason::Stopped });
                if !eligible.is_empty() { self.remove_viewer(session, peer, EndReason::ShareEnded); }
            }
            if eligible.is_empty() { self.end_share(session, ShareEndReason::Stopped, false); }
            else { self.refresh_watchers(session); }
            changed = true;
        }
        if changed { self.settle_media(); }
        changed
    }

    pub(super) fn share_peer_down(&mut self, peer: PublicKey) {
        let offers = self.sharing.shown.offers.clone();
        for offer in offers.iter() {
            let session = offer.session;
            match offer.role {
                ShareRole::Viewer { sharer } if sharer == peer => { self.end_share(session, ShareEndReason::LinkDropped, false); }
                ShareRole::Sharer { .. } => {
                    let local = self.sharing.locals.get_mut(&session).unwrap();
                    if !local.recipients.remove(&peer) { continue }
                    if local.recipients.is_empty() { self.end_share(session, ShareEndReason::LinkDropped, false); }
                    else { self.remove_viewer(session, peer, crate::call::control::EndReason::Disconnected); }
                }
                _ => {}
            }
        }
        if self.links.is_empty() {
            self.sharing.shown.ended = Default::default();
            self.sharing.shown.closed_notice = None;
        } else {
            Arc::make_mut(&mut self.sharing.shown.ended).retain(|e| e.role != (ShareRole::Viewer { sharer: peer }));
        }
        self.close_unavailable_picker();
    }

    pub(super) fn share_message(&mut self, sender: PublicKey, bytes: &[u8]) -> bool {
        let viewer = ShareRole::Viewer { sharer: sender };
        if let Ok(problem) = ShareProblem::decode(bytes) {
            if !self.sharing.shown.offer(problem.session).is_some_and(|o| o.role == viewer && o.stopped_line.is_none()) { return false }
            let peer = crate::model::clean_name(&self.peer_display_name(&sender));
            let peer = if peer.is_empty() { "The other person" } else { peer.as_str() };
            self.sharing.shown.offer_mut(problem.session).unwrap().stopped_line = Some(format!("{peer}'s picture stopped: {}", problem.why()).into());
            if self.sharing.shown.watching != Some(problem.session) { return true }
            self.sharing.shown.picture = None;
            self.sharing.watch_view = View::default();
            self.sharing.watch_started = None;
            // The panel's text changed even if the encoder never produced its first picture.
            self.sharing.fresh = true;
            return true
        }
        let Ok(Some(signal)) = super::super::decode(bytes) else { return false };
        match signal {
            Message::Share { session, title, w, h, ext } => {
                let Some(town) = ext.town.filter(|t| self.is_share_peer_in_town(*t, &sender)) else { return false };
                let size = (w.into(), h.into());
                if let Some(offer) = self.sharing.shown.offer(session) {
                    if offer.role != viewer || offer.town != town || offer.title != title || offer.window_size == size { return false }
                    self.sharing.shown.offer_mut(session).unwrap().window_size = size;
                    return true
                }
                self.begin_offer(session, town, title, viewer, size);
                true
            }
            Message::ShareOff { session, reason } if self.sharing.shown.offer(session).is_some_and(|o| o.role == viewer) =>
                self.end_share(session, reason, false),
            Message::View { session, width, height, fps, ext } if self.sharing.locals.get(&session)
                .is_some_and(|l| l.recipients.contains(&sender)) => {
                let view = View { width, height, fps: fps.min(30) };
                if view_open(view) {
                    if self.sharing.shown.offer(session).is_some_and(|o| o.stopped_line.is_some()) { return false }
                    if let Some((&old, _)) = self.sharing.locals.iter().find(|(s,l)| **s != session && l.viewers.contains_key(&sender)) {
                        self.remove_viewer(old, sender, crate::call::control::EndReason::Released);
                    }
                    let local = self.sharing.locals.get_mut(&session).unwrap();
                    // A View without caps is an older watcher's: H.264 4:2:0 only, or the seat's last known caps.
                    let caps = ext.media_caps.or_else(|| local.seats.get(&sender).copied().flatten());
                    if local.seats.insert(sender, caps) != Some(caps) {
                        let title = self.sharing.shown.offer(session).map_or(String::new(), |o| o.title.to_string());
                        let name = self.flow.peer_name(&sender).unwrap_or_default();
                        if let Some(caps) = caps {
                            log!("Share {title}: {name} decodes {}", decoder_list(caps, "nothing"));
                        } else {
                            log!("Share {title}: {name}'s View lists no decoders (an older Town); sending H.264 · 4:2:0");
                        }
                    }
                    let viewer = Viewer { view, caps: caps.unwrap_or_default(), audio: ext.audio.unwrap_or(false) };
                    let previous = local.viewers.insert(sender, viewer);
                    if previous == Some(viewer) { return false }
                    local.subscribed_at.get_or_insert_with(Instant::now);
                    if previous.is_none() { self.watcher_started(session, sender); }
                    self.settle_format(session);
                    self.refresh_watchers(session);
                } else {
                    self.remove_viewer(session, sender, crate::call::control::EndReason::Released);
                }
                self.attach_share();
                true
            }
            _ => false,
        }
    }

    fn viewer_session(&self) -> Option<SessionId> {
        let mut viewers = self.sharing.shown.offers.iter().filter(|o| matches!(o.role, ShareRole::Viewer { .. }));
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

    pub(super) fn watched_peer_id(&self) -> Option<PeerId> {
        let peer = self.sharing.shown.watching_sharer?;
        self.links.get(&peer).filter(|l| l.media_held.is_some()).map(|l| l.peer_id)
    }

    fn subscribe_measured(&mut self, session: SessionId) {
        if !self.sharing.shown.offer(session).is_some_and(|o| o.stopped_line.is_none()) { return }
        let peer_id = self.watched_peer_id();
        let view = [self.sharing.measured_panel, self.sharing.window_panel].into_iter().flatten()
            .filter(|(s, _)| *s == session)
            .fold(View::default(), |a, (_, b)| View { width: a.width.max(b.width), height: a.height.max(b.height), fps: 30 });
        if view.width > 0 && view.height > 0 && view != self.sharing.watch_view {
            self.sharing.watch_view = view;
            if let (Some(call), Some(pid)) = (&mut self.current, peer_id) { call.watch_share(pid, view); }
            self.send_views();
        }
    }

    /// The picture size this guest's subscription asks the sharer for.
    pub fn share_subscription(&self) -> View { self.sharing.watch_view }

    fn clear_decode_problem(&mut self, session: SessionId) -> bool {
        if self.sharing.shown.offer(session).is_some_and(|offer| offer.decode_problem.is_some()) {
            self.sharing.shown.offer_mut(session).unwrap().decode_problem = None;
            return true
        }
        false
    }

    fn close_watch(&mut self, session: SessionId) {
        self.clear_decode_problem(session);
        let peer_id = self.watched_peer_id();
        self.record_displayed_share();
        if self.sharing.shown.control_session == Some(session) {
            self.end_control(crate::call::control::EndReason::Released, true);
        }
        if self.sharing.shown.control.asking == Some(session) {
            self.ask_control(session, false);
            self.sharing.shown.control.asking = None;
        }
        log!("Share Watch closed");
        if let (Some(call), Some(pid)) = (&mut self.current, peer_id) {
            call.listen_share(pid, false);
            call.watch_share(pid, View::default());
        }
        self.send_views();
        self.sharing.clear_sound_sync();
        self.sharing.sync_last_capture_us = None;
        self.sharing.shown.watching = None;
        self.sharing.shown.watching_sharer = None;
        self.sharing.shown.control.allowed = None;
        self.sharing.shown.picture = None;
        self.sharing.shown.overlay = None;
        self.sharing.watch_started = None;
        self.sharing.fresh = true;
        self.sharing.watch_view = View::default();
        self.sharing.window_panel = None;
    }

    pub(super) fn watch(&mut self, session: SessionId) -> bool {
        if self.sharing.shown.watching == Some(session) { return false }
        let Some(offer) = self.sharing.shown.offer(session) else { return false };
        let ShareRole::Viewer { sharer } = offer.role else { return false };
        let stopped = offer.stopped_line.is_some();
        if let Some(previous) = self.sharing.shown.watching { self.close_watch(previous); }
        self.clear_decode_problem(session);
        self.sharing.sound_ahead_logged = false;
        self.sharing.shown.watching = Some(session);
        self.sharing.shown.watching_sharer = Some(sharer);
        self.sharing.fresh = true;
        self.sharing.watch_started = (!stopped).then(Instant::now);
        if !stopped {
            let id = self.watched_peer_id();
            if let (Some(call), Some(pid)) = (&mut self.current, id) {
                call.begin_watch(pid);
                call.listen_share(pid, self.sharing.sound_on);
            }
            self.subscribe_measured(session);
        }
        true
    }

    pub(super) fn share_press(&mut self, press: Press) -> bool {
        let changed = self.handle_share_press(press);
        if changed { self.settle_media(); }
        changed
    }

    fn handle_share_press(&mut self, press: Press) -> bool {
        match press {
            Press::Share(ShareChoice::Menu) if self.can_share() => {
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
            Press::Share(ShareChoice::Row(key)) if self.sharing.shown.choices.is_some() && (self.can_share() || self.sharing.shown.capture.picking) => {
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

    pub fn asking_for_control(&self) -> Option<SessionId> { self.sharing.shown.control.asking }

    pub fn control_rights_revision(&self, session: SessionId) -> Option<u64> {
        Some(self.sharing.locals.get(&session)?.rights_revision)
    }

    pub fn control_rights(&self, session: SessionId) -> Option<crate::tweak::ControlRights> {
        let rights = &self.sharing.locals.get(&session)?.rights;
        Some(crate::tweak::ControlRights { all: rights.allow_all, seats: rights.allowed.keys().copied().collect() })
    }

    pub(super) fn peer_can_control(&self, session: SessionId, peer: &PublicKey) -> bool {
        self.sharing.locals.get(&session).is_some_and(|local| local.rights.allows(peer))
    }

    pub(super) fn is_viewer(&self, session: SessionId, peer: &PublicKey) -> bool {
        self.sharing.locals.get(&session).is_some_and(|l| l.viewers.contains_key(peer))
    }

    pub(super) fn is_recipient(&self, session: SessionId, peer: &PublicKey) -> bool {
        self.sharing.locals.get(&session).is_some_and(|l| l.recipients.contains(peer))
    }

    pub fn allow_control(&mut self, apps: &mut AppLinks, session: SessionId, peer: PublicKey, display_name: &str) -> bool {
        if !self.is_viewer(session, &peer) || (!self.flow.is_voice_peer(&peer)
            && !self.sharing.shown.control.requests.iter().any(|(s, p, _)| (*s,*p) == (session, peer))) { return false }
        let changed = self.change_rights(session, RightChange::Allow(peer, display_name));
        self.grant_control(apps, session, peer) || changed
    }

    pub fn allow_all_control(&mut self, apps: &mut AppLinks, session: SessionId) -> bool {
        let changed = self.change_rights(session, RightChange::All(true));
        if self.sharing.shown.control.active.is_some() { return changed }
        let Some(local) = self.sharing.locals.get(&session) else { return changed };
        let next = self.sharing.shown.control.requests.iter().find(|(s,p,_)| *s == session && local.viewers.contains_key(p))
            .map(|(_,p,_)| *p).or_else(|| local.viewers.keys().next().copied());
        next.is_some_and(|peer| self.grant_control(apps, session, peer)) || changed
    }

    pub(super) fn revoke_departed_control(&mut self) -> bool {
        let mut departed = Vec::new();
        for (&session, local) in &self.sharing.locals {
            let Some(members) = self.town_members.get(&local.town) else { continue };
            for &peer in local.rights.allowed.keys() {
                if !members.contains(&peer) { departed.push((session, peer)); }
            }
        }
        let mut changed = false;
        for (session, peer) in departed {
            changed |= self.change_rights(session, RightChange::Remove(peer));
        }
        changed
    }

    fn change_rights(&mut self, session: SessionId, change: RightChange<'_>) -> bool {
        use crate::call::control::{ControlMessage, EndReason};
        let Some(local) = self.sharing.locals.get_mut(&session) else { return false };
        let previous = local.rights.clone();
        match change {
            RightChange::All(on) if local.rights.allow_all != on => Arc::make_mut(&mut local.rights).allow_all = on,
            RightChange::Allow(peer, name) if !local.rights.allowed.contains_key(&peer) => {
                Arc::make_mut(&mut local.rights).allowed.insert(peer, name.to_owned());
            }
            RightChange::Remove(key) if local.rights.allowed.contains_key(&key) => {
                Arc::make_mut(&mut local.rights).allowed.remove(&key);
            }
            _ => return false,
        }
        local.rights_revision += 1;
        self.sharing.shown.offer_mut(session).unwrap().rights = local.rights.clone();
        let updates: Vec<_> = local.viewers.keys().filter_map(|p| {
            let allowed = local.rights.allows(p);
            (allowed != previous.allows(p)).then_some((*p, allowed))
        }).collect();
        for (peer, allowed) in updates {
            self.control_send(peer, ControlMessage::Rights { session, allowed });
            if !allowed && self.sharing.shown.control_session == Some(session)
                && self.sharing.shown.control.holder.as_ref().is_some_and(|(p, _)| *p == peer) {
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
                if let ShareRole::Viewer { sharer } = offer.role {
                    self.facts.push(Fact::ShareJoin { session: offer.session, sharer, town: offer.town });
                }
            }
        }
    }

    pub(super) fn sync_share_speaker(&mut self, host: &mut dyn AudioHost) -> bool {
        self.sharing.speaker.tick();
        let wanted = self.watched_peer_id().filter(|_| self.sharing.sound_on)
            .and_then(|id| self.current.as_ref()?.share_sound(id));
        let share = &mut self.sharing;
        if wanted.is_none() && share.sync_sound.is_some() { share.clear_sound_sync(); }
        let wanted = wanted.filter(|sound| share.sync_target.is_some()
            && share.sync_sound.as_ref().is_some_and(|(tracked, _)| Arc::ptr_eq(tracked, sound)));
        let was_playing = share.wants_speaker();
        match wanted {
            Some(sound) if !share.playing.as_ref().is_some_and(|old| Arc::ptr_eq(old, sound)) => {
                share.playing_title = share.shown.watched().map(|o| o.title.to_string()).unwrap_or_default();
                share.speaker.play(sound.ring.clone());
                share.playing = Some(sound.clone());
                log!("Share {}: playing the app's sound", share.playing_title);
            }
            None if was_playing => {
                share.speaker.stop();
                share.playing = None;
                log!("Share {}: app sound stopped", share.playing_title);
            }
            _ => return false,
        }
        let playing = share.wants_speaker();
        if playing != was_playing {
            if playing { self.list_devices(host); }
            self.route(host);
        }
        true
    }

    pub(super) fn poll_share(&mut self) -> bool {
        if let Some(caps) = self.sharing.decode_probe.try_take() { self.apply_decode_caps(caps); }
        let peer_id = self.watched_peer_id();
        if self.sharing.shown.offers.is_empty() { return false; }
        self.log_share_progress();
        let mut changed = false;
        let offers = self.sharing.shown.offers.clone();
        for offer in offers.iter().filter(|o| matches!(o.role, ShareRole::Sharer { .. })) {
            let local = self.sharing.locals.get_mut(&offer.session).unwrap();
            if let Some(probe) = &local.picture {
                if probe.lost_count() != local.lost.len() {
                    changed = true;
                    local.lost = probe.lost();
                    local.line_state = None;
                    self.settle_format(offer.session);
                }
            }
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
        let Some(call) = &self.current else { return changed };
        let mut fresh_capture_us = None;
        if self.sharing.shown.offer(session).is_some_and(|o| o.stopped_line.is_none()) {
            let slot = peer_id.and_then(|id| call.share_picture(id));
            let picture = slot.and_then(|s| s.picture.as_ref());
            if slot.is_some_and(|s| s.fresh) || (picture.is_none() && self.sharing.shown.picture.is_some()) {
                fresh_capture_us = slot.filter(|s| s.fresh && s.picture.is_some()).map(|s| s.capture_us);
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
        let arrival = peer_id.filter(|_| self.sharing.sound_on).and_then(|id| call.share_sound(id))
            .filter(|sound| sound.counts().frames > 0).and_then(|sound| sound.arrival_base_us().map(|base| (sound, base)));
        if fresh_capture_us.is_some() || arrival.is_some() {
            self.sharing.update_sound_target(arrival, self.now().mono_us, fresh_capture_us);
        }
        if self.sharing.shown.picture.is_some() { changed |= self.clear_decode_problem(session); }
        self.send_views();
        changed
    }

    fn send_views(&mut self) {
        while let Some((id, view)) = self.current.as_mut().and_then(Call::take_view) {
            let Some(&peer) = self.id_to_peer.get(&id) else { continue };
            let Some(session) = self.sharing.shown.watching else { continue };
            if self.sharing.shown.watching_sharer == Some(peer) {
                self.send_watch_view(peer, session, view);
            }
        }
    }

    fn send_watch_view(&mut self, peer: PublicKey, session: SessionId, view: View) {
        let View { width, height, fps } = view;
        let ext = super::super::ShareWatchExt { media_caps: Some(self.sharing.decode_caps), audio: Some(self.sharing.sound_on) };
        self.send_share_to(peer, Message::View { session, width, height, fps, ext });
    }

    fn apply_decode_caps(&mut self, probed: MediaCaps) {
        let share = &mut self.sharing;
        let caps = MediaCaps(probed.0 & !share.failed.0);
        static LOGGED: std::sync::Once = std::sync::Once::new();
        LOGGED.call_once(|| {
            let list = decoder_list(caps, "none");
            log!("Share: this computer decodes {list}");
        });
        if caps == share.decode_caps { return }
        share.decode_caps = caps;
        let view = share.watch_view;
        if let (Some(peer), Some(session)) = (share.shown.watching_sharer, share.shown.watching) {
            if view_open(view) { self.send_watch_view(peer, session, view); }
        }
    }

    pub(super) fn share_decoder_limit(&mut self, format: StreamFormat, timing_us: Option<(u64, u64)>) -> bool {
        let share = &mut self.sharing;
        let previous = share.failed;
        share.failed.0 |= share_codec::failed_decode_bits(format);
        share.decode_caps.0 &= !share.failed.0;
        let (Some(peer), Some(session)) = (share.shown.watching_sharer, share.shown.watching) else { return false };
        let view = share.watch_view;
        // Every higher format can fall back to the mandatory H.264 4:2:0 stream.
        let has_fallback = format != StreamFormat::H264;
        if share.failed != previous {
            if let Some((decode_us, interval_us)) = timing_us {
                log!("Share: this computer can't keep up with {} (decode {:.1} ms per picture, a picture every {:.1} ms); asking for another format",
                    format.label(), decode_us as f64 / 1000.0, interval_us as f64 / 1000.0);
            } else if let Some(offer) = share.shown.offer(session) {
                let retry = if has_fallback { "; asking for another format" } else { "" };
                log!("Share {}: this computer can't decode {}{retry}", offer.title, format.label());
            }
            if view_open(view) { self.send_watch_view(peer, session, view); }
        }
        if has_fallback { return self.clear_decode_problem(session) }

        // The worker reports its detailed Problem only once. If that was the earlier
        // codec's error, the failed fallback key still needs a reason on the waiting tile.
        let shown = &mut self.sharing.shown;
        if shown.picture.is_none() && shown.offer(session).is_some_and(|o| o.decode_problem.is_none()) {
            shown.offer_mut(session).unwrap().decode_problem = Some(format!("This computer can't decode {}.", format.label()).into());
            return true
        }
        false
    }

    fn log_share_progress(&mut self) {
        let Some(last) = self.sharing.last_report else { return };
        let due = last.elapsed().as_secs() >= 10;
        let Some(stats) = self.current.as_ref().map(Call::stats) else { return };
        for peer in &stats.peers {
        let logged = self.sharing.logged_keys.entry(peer.peer).or_insert([Some(0); 4]);
        let counts = share_counters(peer);
        // Report the first key events promptly. Further keys and errors are totals on the
        // ten-second line, including when every picture is a key or the decoder repeatedly fails.
        for (i, what) in ["key request sent", "key request received", "key sent", "decode error"].iter().enumerate() {
            if logged[i].is_some_and(|baseline| counts[i] > baseline) {
                log!("Share {what}, total {}", counts[i]);
                logged[i] = None;
            }
        }
        if due {
            log!("Share frames: (captured, encoded) {:?}, sent {}, fragments received {}, frames received {}, decoded {}, decode errors {}, keys sent {}, last key paced in {} ms, key requests sent/received {}/{} (sent for {:?}), pacing queue {} bytes, link backlog {}, share kbps {:?}, link estimate {} kbit/s, rtt {:?} ms, loss {:.3}, {}",
                peer.share_source_counts, peer.session.share_frames_sent, peer.share_received.fragments, peer.share_received.frames,
                peer.share_decoded, counts[3], counts[2], peer.session.last_share_key_pacing_us / 1000, counts[0], counts[1], peer.session.share_request_causes, peer.paced_bytes, peer.backlog, stats.fixed_cell_share_kbps,
                peer.estimate_bps / 1000, peer.rtt_us.map(|us| us / 1000), peer.random_loss, self.takeover.input.summary());
        }
        }
        if due {
            if let Some(sound) = &self.sharing.playing {
                let counts = sound.counts();
                let speaker = &self.sharing.speaker;
                log!("Share audio: frames {}, restarts {}, bad {}, concealed {}, resets {}, target {}, level {:.3}",
                    counts.frames, counts.restarts, counts.bad, speaker.concealed(), speaker.resets(),
                    self.sharing.sync_target.unwrap_or(MIN_SOUND_FRAMES), speaker.level());
            }
            self.sharing.last_report = Some(Instant::now());
        }
    }

    /// Only the local window open in the center needs a preview stream.
    pub fn sync_share_preview(&mut self, apps: &mut AppLinks, session: Option<SessionId>) -> bool {
        let session = session.filter(|s| self.sharing.locals.contains_key(s));
        let changed = self.sharing.previewing != session;
        if changed {
            if let Some(old) = self.sharing.previewing.take() {
                if let Some(local) = self.sharing.locals.get_mut(&old) {
                    local.preview = None;
                    local.line_state = None;
                }
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
                    let want = PictureWant { window, max_w: w, max_h: h, fps: PREVIEW_FPS, kbps: 0, format: None };
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
