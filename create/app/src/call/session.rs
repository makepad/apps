//! The UI side of a call. The link outlives its calls, and the device callbacks outlive the link.
use super::{CallAction, CallShown, PickerShown, Talking, CallFlow, CallId, CallScope, CallView, DeviceChanges, DeviceKind, Devices, Effect, Fact, Ignored, Now, Phase, Press, ShareEndReason};
use super::{TranscribeShown, transcribe::{Recognizer, Transcriber, TranscriberConfig, TranscriberEvent}};
use super::media_key::MediaKey;
use super::media_session::{Changes, MediaSession};
use super::signal::MediaSignal;
use crate::{link_worker::MediaPort, transcript::Heard};
use makepad_ai_hub::speech::WakeHook;
use makepad_call::driver::TranscriptTap;
use makepad_call::driver::{
    audio::{audio_ports, InputPort, OutputPort, PortControl},
    codec::platform_codecs, Call, CallConfig, CallEvent, EchoCancelMode, Inlet, Join, PeerId, Spawn, VoiceSource,
};
use makepad_widgets::{log, Cx, CxMediaApi, LiveId, Timer, makepad_platform::audio::{AudioDeviceDesc, AudioDeviceId, AudioDeviceType, AudioInputOptions}};
use makepad_town_core::{event::{Id, TownId}, home_writer::PrivateFiles, key::PublicKey, share::hello::MediaCaps};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    time::Instant,
};

const ECHO_CANCEL_NOTICE: &str = "Echo cancellation is off — use headphones to avoid echo.";

fn platform_echo_cancel() -> EchoCancelMode {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    { EchoCancelMode::OsHardware }
    #[cfg(any(target_os = "linux", windows))]
    { EchoCancelMode::Software }
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux", windows)))]
    { EchoCancelMode::Off }
}

#[path = "share.rs"]
mod share;
#[path = "takeover.rs"]
mod takeover;
pub use share::{ShareChoice, ShareEvent, ShareOffer, ShareRole, ShareRow, ShareShown};

#[derive(Debug, Eq, PartialEq)]
pub struct NotLinked;

#[derive(Debug, Eq, PartialEq)]
pub enum PeerEvent {
    /// Accepted locally, but still waiting to be handed to this link when it ended.
    PeerUndelivered { peer: PublicKey, topic: u8, count: usize },
}

#[derive(Default)]
struct PeerMessages {
    /// One frame owns the topic for undelivered-message accounting (the first for latest-only traffic).
    outgoing: VecDeque<(Option<u8>, Vec<u8>)>,
    incoming: std::collections::BTreeMap<u8, Vec<Vec<u8>>>,
    partial: std::collections::BTreeMap<u8, Vec<u8>>,
    /// Topics this build does not take, each logged once per link.
    ignored: std::collections::BTreeSet<u8>,
    waiting: bool,
}

pub struct CallEnv {
    pub files: PrivateFiles,
    pub spawn: Spawn,
    pub jobs: Spawn,
    pub signal: Arc<dyn Fn() + Send + Sync>,
    /// Where the picked audio devices are saved. Empty: none are read or saved.
    pub devices_path: PathBuf,
    pub voice: VoiceSource,
}

pub trait AudioHost {
    /// Once per controller, before the first device opens.
    fn install(&mut self, mic: MicPort, speaker: OutputPort);
    /// Open exactly these inputs (empty: none), with echo cancellation.
    fn use_inputs(&mut self, ids: &[AudioDeviceId]);
    fn use_outputs(&mut self, ids: &[AudioDeviceId]);
}

impl AudioHost for Cx {
    fn install(&mut self, mut mic: MicPort, mut speaker: OutputPort) {
        self.audio_input(0, move |info, buffer| {
            mic.push_planar(info.sample_rate, buffer.frame_count(), buffer.channel_count(), &buffer.data);
        });
        self.audio_output(0, move |info, buffer| {
            speaker.fill_planar(info.sample_rate, buffer.frame_count(), buffer.channel_count(), &mut buffer.data);
        });
    }

    fn use_inputs(&mut self, ids: &[AudioDeviceId]) {
        self.use_audio_inputs_with_options(ids, AudioInputOptions { echo_cancellation: true });
    }

    fn use_outputs(&mut self, ids: &[AudioDeviceId]) { self.use_audio_outputs(ids); }
}

struct SyntheticHost {
    spawn: Spawn,
    ports: Option<(MicPort, Arc<SyntheticSpeaker>)>,
    input: Option<AudioDeviceId>,
    output: Option<AudioDeviceId>,
}

struct SyntheticSpeaker {
    port: Mutex<OutputPort>,
    open: AtomicBool,
    stop: AtomicBool,
}

impl SyntheticSpeaker {
    fn run(&self) {
        let mut block = [0.0f32; 480];
        let tick = std::time::Duration::from_millis(10);
        let mut deadline = Instant::now();
        loop {
            deadline += tick;
            if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            if self.stop.load(Ordering::Relaxed) { break }
            if self.open.load(Ordering::Relaxed) {
                let mut port = self.port.lock().unwrap_or_else(|err| err.into_inner());
                port.fill_planar(48_000.0, 480, 1, &mut block);
            }
        }
    }
}

impl SyntheticHost {
    const MIC: AudioDeviceId = AudioDeviceId(LiveId::from_str("town_synthetic_microphone"));
    const SPEAKER: AudioDeviceId = AudioDeviceId(LiveId::from_str("town_synthetic_speaker"));

    fn new(spawn: Spawn) -> Self {
        Self { spawn, ports: None, input: None, output: None }
    }

    #[cfg(test)]
    fn received_seq(&self, peer: PeerId, stream: u8) -> Option<u32> {
        let (_, speaker) = self.ports.as_ref()?;
        speaker.port.lock().unwrap_or_else(|err| err.into_inner()).received_seq(peer, stream)
    }

    fn descs() -> [AudioDeviceDesc; 2] {
        [(Self::MIC, AudioDeviceType::Input, "Synthetic microphone"),
         (Self::SPEAKER, AudioDeviceType::Output, "Synthetic speaker")].map(|(device_id, device_type, name)|
            AudioDeviceDesc { device_id, device_type, name: name.into(), is_default: true, has_failed: false, channel_count: 1 })
    }

    fn select(slot: &mut Option<AudioDeviceId>, allowed: AudioDeviceId, ids: &[AudioDeviceId]) {
        match ids {
            [] => *slot = None,
            [id] if *id == allowed => *slot = Some(allowed),
            _ => log!("Town call: refusing a device outside the synthetic host"),
        }
    }
}

impl AudioHost for SyntheticHost {
    fn install(&mut self, mic: MicPort, speaker: OutputPort) {
        let speaker = Arc::new(SyntheticSpeaker {
            port: Mutex::new(speaker),
            open: AtomicBool::new(self.output.is_some()),
            stop: AtomicBool::new(false),
        });
        self.ports = Some((mic, speaker.clone()));
        (self.spawn)("town synthetic speaker", Box::new(move || speaker.run()));
    }

    fn use_inputs(&mut self, ids: &[AudioDeviceId]) { Self::select(&mut self.input, Self::MIC, ids); }
    fn use_outputs(&mut self, ids: &[AudioDeviceId]) {
        Self::select(&mut self.output, Self::SPEAKER, ids);
        if let Some((_, speaker)) = &self.ports {
            speaker.open.store(self.output.is_some(), Ordering::Relaxed);
        }
    }
}

impl Drop for SyntheticHost {
    fn drop(&mut self) {
        if let Some((_, speaker)) = &self.ports { speaker.stop.store(true, Ordering::Relaxed); }
    }
}

/// The platform callback records the first block after reopening, including a silent block.
pub struct MicPort {
    input: InputPort,
    blocks: Arc<AtomicU64>,
}

impl MicPort {
    pub fn push_mono(&mut self, rate: f64, samples: &[f32]) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
        self.input.push_mono(rate, samples);
    }

    pub fn push_planar(&mut self, rate: f64, frames: usize, channels: usize, data: &[f32]) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
        self.input.push_planar(rate, frames, channels, data);
    }

    pub fn push_interleaved(&mut self, rate: f64, channels: usize, data: &[f32]) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
        self.input.push_interleaved(rate, channels, data);
    }
}

struct Link {
    share_parts: Vec<u8>,
    me: PublicKey,
    peer: PublicKey,
    local_media_key: MediaKey,
    peer_media_key: Option<MediaKey>,
    media: MediaSession,
    name: String,
    peer_id: PeerId,
    media_held: Option<u32>,
    caps: MediaCaps,
    epoch: u64,
    out: SyncSender<Vec<u8>>,
    cap: Arc<AtomicU64>,
    incoming: Receiver<Vec<u8>>,
    outgoing: SyncSender<Vec<u8>>,
    pending: VecDeque<Vec<u8>>,
    peer_messages: PeerMessages,
}

impl Link {
    fn new(me: PublicKey, peer: PublicKey, local_media_key: MediaKey, name: String, peer_id: PeerId, epoch: u64, inlet: &Inlet) -> (Self, MediaPort) {
        // The workers retain outbound traffic on backpressure. Signalling has its own queue.
        let (out, datagrams_out) = sync_channel(256);
        let (messages_in, incoming) = sync_channel(16);
        let (outgoing, messages_out) = sync_channel(16);
        let cap = Arc::new(AtomicU64::new(0));
        let port = MediaPort {
            datagrams_in: inlet.sender(), datagrams_out, messages_in, messages_out,
            rate_cap: cap.clone(), sent_bytes: Arc::default(),
        };
        (Self { me, peer, local_media_key, peer_media_key: None, media: MediaSession::default(), name, peer_id, media_held: None, caps: MediaCaps::NONE, epoch, out, cap, incoming, outgoing,
            pending: VecDeque::new(), peer_messages: PeerMessages::default(), share_parts: Vec::new() }, port)
    }

    fn flush(&mut self, sent: &mut u64) {
        while let Some(body) = self.pending.pop_front() {
            match self.outgoing.try_send(body) {
                Ok(()) => *sent += 1,
                Err(TrySendError::Full(body) | TrySendError::Disconnected(body)) => {
                    self.pending.push_front(body);
                    break;
                }
            }
        }
    }

    fn send(&mut self, body: Vec<u8>, sent: &mut u64) {
        self.pending.push_back(body);
        if self.pending.len() == 1 {
            self.flush(sent);
            if !self.pending.is_empty() {
                log!("Town call: signalling is waiting for the link");
            }
        }
    }
}

pub struct CallController {
    echo_cancel: EchoCancelMode,
    sharing: share::Sharing,
    takeover: takeover::Takeover,
    codecs: Arc<dyn makepad_call::driver::codec::Codecs>,
    flow: CallFlow,
    synthetic: Option<SyntheticHost>,
    link_changed: bool,
    timer: Timer,
    talking: Talking,
    last_talking: Instant,
    devices: Devices,
    env: Option<CallEnv>,
    links: BTreeMap<PublicKey, Link>,
    inlet: Inlet,
    next_epoch: u64,
    open_town: Option<(TownId, Vec<PublicKey>)>,
    town_members: BTreeMap<TownId, BTreeSet<PublicKey>>,
    next_peer_id: PeerId,
    id_to_peer: BTreeMap<PeerId, PublicKey>,
    call_generation: u64,
    current: Option<Call>,
    voice_wanted: bool,
    voice_attached: bool,
    stopping: Vec<Call>,
    control: PortControl,
    uninstalled: Option<(MicPort, OutputPort)>,
    blocks: Arc<AtomicU64>,
    unmute_at: u64,
    enumerated_or_requested: bool,
    want_input: bool,
    want_output: bool,
    input: Option<AudioDeviceId>,
    output: Option<AudioDeviceId>,
    facts: Vec<Fact>,
    muted_sessions: Vec<CallId>,
    clock: Instant,
    sent: u64,
    peer_events: Vec<PeerEvent>,
    undelivered_input: usize,
    peer_retry: Timer,
    speech: Speech,
    peer_level: super::level::PeerLevel,
    level_timer: Timer,
    pub consent_home: crate::consent::TestHome,
}

impl Default for CallController {
    fn default() -> Self {
        let (input, output, control) = audio_ports();
        let blocks: Arc<AtomicU64> = Arc::default();
        Self {
            flow: CallFlow::default(), synthetic: None, link_changed: false, timer: Timer::empty(),
            sharing: share::Sharing::default(), takeover: Default::default(), voice_wanted: false, voice_attached: false,
            codecs: platform_codecs(),
            echo_cancel: platform_echo_cancel(),
            talking: Talking::default(), last_talking: Instant::now(), devices: Devices::default(), env: None,
            inlet: Inlet::new(), links: BTreeMap::new(), next_epoch: 0,
            open_town: None, town_members: BTreeMap::new(), next_peer_id: 1, id_to_peer: BTreeMap::new(), call_generation: 0, current: None, stopping: Vec::new(), control,
            uninstalled: Some((MicPort { input, blocks: blocks.clone() }, output)),
            blocks, unmute_at: 0, enumerated_or_requested: false, want_input: false,
            want_output: false, input: None, output: None, facts: Vec::new(), muted_sessions: Vec::new(),
            clock: Instant::now(), sent: 0, speech: Speech::default(),
            peer_events: Vec::new(), undelivered_input: 0, peer_retry: Timer::empty(),
            peer_level: Default::default(), level_timer: Timer::empty(),
            consent_home: Default::default(),
        }
    }
}

impl CallController {
    pub fn configure(&mut self, env: CallEnv) {
        if self.env.is_some() {
            return;
        }
        let text = if env.devices_path.as_os_str().is_empty() {
            String::new()
        } else {
            match env.files.read(&env.devices_path)
                .and_then(|bytes| String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))) {
                Ok(text) => text,
                Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
                Err(err) => {
                    log!("Town call: couldn't read the audio devices: {err}");
                    String::new()
                }
            }
        };
        self.devices = Devices::new(&text);
        if matches!(env.voice, VoiceSource::Synthetic { .. }) {
            self.synthetic = Some(SyntheticHost::new(env.spawn.clone()));
            self.devices.update(&SyntheticHost::descs());
            self.enumerated_or_requested = true;
            log!("Town call: synthetic mode for live checks: no microphone or speaker will open; the synthetic speaker plays out received voice");
        }
        self.env = Some(env);
    }

    pub fn configured(&self) -> bool { self.env.is_some() }
    pub fn linked(&self) -> bool { !self.links.is_empty() }
    pub fn inlet(&self) -> Inlet { self.inlet.clone() }
    pub fn set_peer_caps(&mut self, peer: PublicKey, caps: MediaCaps) -> bool {
        let could_share = self.can_share();
        if let Some(link) = self.links.get_mut(&peer) { link.caps = caps; }
        let changed = self.sync_share_members() || self.can_share() != could_share;
        if changed { self.notify_link_change(); }
        changed
    }
    pub fn can_call(&self) -> bool { self.flow.can_call() }

    pub fn can_join_call(&self, town: TownId, scope: CallScope) -> bool { self.flow.can_join_call(town, scope) }

    pub fn set_town_peer_seats(&mut self, seats: BTreeMap<TownId, BTreeMap<PublicKey, Id>>) {
        self.flow.set_town_peer_seats(seats);
    }

    /// Supplies each loaded town's active peers, excluding this seat. A missing town
    /// has unknown membership and keeps its named control Allows.
    pub fn set_town_members(&mut self, members: BTreeMap<TownId, BTreeSet<PublicKey>>) -> bool {
        self.town_members = members;
        self.revoke_departed_control()
    }

    pub fn can_call_seat(&self, town: TownId, seat: Id) -> bool { self.flow.can_call_seat(town, seat) }
    pub fn calling_peers(&self, town: TownId) -> Vec<PublicKey> { self.flow.calling_peers(town) }

    pub fn set_town_peers(&mut self, host: &mut dyn AudioHost, towns: BTreeMap<TownId, BTreeSet<PublicKey>>) -> bool {
        let mut effects = Vec::new();
        let could_share = self.can_share();
        self.flow.set_town_peers(towns, self.now(), &mut effects);
        let mut changed = self.sync_share_members() || self.can_share() != could_share;
        if !effects.is_empty() { changed |= self.apply(host, effects); }
        if changed { self.notify_link_change(); }
        changed
    }

    pub fn open_town(&self) -> Option<(TownId, &[PublicKey])> {
        self.open_town.as_ref().map(|(town, peers)| (*town, peers.as_slice()))
    }
    pub fn ignored(&self) -> Ignored { self.flow.ignored() }
    pub fn voice_peers(&self) -> Vec<PublicKey> { self.flow.voice_peers().collect() }
    pub fn peer_level(&self, peer: &PublicKey) -> f32 { self.peer_level.peer_level(peer) }
    pub fn active_call_town(&self) -> Option<TownId> { self.flow.active_town() }
    pub fn ringing_call_town(&self) -> Option<TownId> { self.flow.ringing_town() }
    pub fn ringing_from(&self) -> Option<PublicKey> { self.flow.ringing_from() }
    pub fn call_generation(&self) -> u64 { self.call_generation }
    pub fn linked_peers(&self) -> impl Iterator<Item = PublicKey> + '_ { self.links.keys().copied() }
    pub fn peer_epochs(&self) -> impl Iterator<Item = (PublicKey, u64)> + '_ {
        self.links.values().map(|link| (link.peer, link.epoch))
    }
    pub(super) fn peer_epoch_of(&self, peer: &PublicKey) -> Option<(PublicKey, u64)> {
        self.links.get(peer).map(|link| (link.peer, link.epoch))
    }

    pub fn set_open_town(&mut self, open: Option<(TownId, Vec<(PublicKey, String)>)>) -> bool {
        let could_share = self.can_share();
        let targets = open.as_ref().map(|(id, peers)| (*id, peers.iter().map(|(p, _)| *p).collect()));
        let mut effects = Vec::new();
        self.flow.set_open_town(open, &mut effects);
        let mut changed = targets != self.open_town || !effects.is_empty();
        self.open_town = targets;
        changed |= self.sync_share_members() || self.can_share() != could_share;
        if changed { self.notify_link_change(); }
        changed
    }

    pub fn wants_input(&self) -> bool { self.want_input }

    pub fn shown(&self) -> CallShown {
        let echo_cancel_off = self.view().phase == Phase::Live && self.echo_cancel == EchoCancelMode::Off
            && self.env.as_ref().is_some_and(|env| env.voice == VoiceSource::Feed);
        CallShown {
            echo_cancel_notice: echo_cancel_off.then(|| ECHO_CANCEL_NOTICE.to_owned()),
            view: Arc::new(self.view().clone()),
            input: PickerShown::read(&self.devices, DeviceKind::Input),
            output: PickerShown::read(&self.devices, DeviceKind::Output),
            linked: self.linked(), can_call: self.can_call(), can_share: self.can_share(), mic_open: self.mic_open(), talking: self.talking,
            transcribe: self.transcription_status(),
            peer_level: self.peer_level.step.clone(),
            transcript_town: self.speech.town.as_ref().map(|(_, name)| name.clone()).unwrap_or_default(),
            share: ShareShown {
                media_held: self.sharing.shown.watching_sharer.and_then(|p| self.links.get(&p)).is_some_and(|l| l.media_held.is_some()),
                ..self.sharing.shown.clone()
            },
        }
    }

    pub fn sync_timer(&mut self, cx: &mut Cx) {
        self.sync_peer_retry(cx);
        self.sync_level_timer(cx);
        if self.view().phase != Phase::Idle && self.timer.is_empty() {
            self.timer = cx.start_interval(1.0);
        } else if self.view().phase == Phase::Idle && !self.timer.is_empty() {
            cx.stop_timer(std::mem::take(&mut self.timer));
        }
    }

    /// Opening actions must go through the consent gate of the originating input event.
    fn opening(&self, action: CallAction) -> bool {
        match action {
            CallAction::Press(Press::Share(_) | Press::Watch | Press::Clip | Press::Snap | Press::FirstCaptureWindow | Press::LetControl(_) | Press::AllowAll(true)) => true,
            CallAction::OnShare(_, Press::Watch | Press::LetControl(_) | Press::AllowAll(true)) => true,
            CallAction::Press(Press::Call | Press::CallSeat(_) | Press::JoinCall(..) | Press::Answer | Press::Unmute | Press::Transcribe) => true,
            CallAction::Pick(DeviceKind::Input, _) => self.wants_input(),
            _ => false,
        }
    }

    fn act(&mut self, host: &mut dyn AudioHost, action: CallAction) -> bool {
        match action {
            CallAction::Press(press) => self.press(host, press),
            CallAction::OnShare(session, press) => self.close_share_action(session, press),
            CallAction::Pick(kind, id) => self.pick(host, kind, id),
        }
    }

    pub fn call_actions(&mut self, host: &mut dyn AudioHost, action: CallAction) -> bool {
        if matches!(action, CallAction::OnShare(_, Press::Watch | Press::LetControl(_) | Press::AllowAll(true)) | CallAction::Press(Press::Share(_) | Press::Watch | Press::Clip | Press::Snap | Press::FirstCaptureWindow | Press::LetControl(_) | Press::AllowAll(true))) { return false }
        (self.synthetic.is_some() || !self.opening(action)) && self.act(host, action)
    }

    pub fn call_opening_presses(&mut self, host: &mut dyn AudioHost, action: CallAction) -> bool {
        if let CallAction::OnShare(session, Press::Watch) = action {
            let changed = self.watch(session);
            if changed { self.settle_media(); }
            return changed
        }
        if let CallAction::Press(press @ (Press::Share(_) | Press::Watch | Press::Clip | Press::Snap | Press::FirstCaptureWindow)) = action { return self.share_press(press) }
        self.synthetic.is_none() && self.opening(action) && self.act(host, action)
    }

    fn host<'a>(&'a mut self, platform: &'a mut dyn AudioHost) -> &'a mut dyn AudioHost {
        match &mut self.synthetic { Some(host) => host, None => platform }
    }

    fn notify_link_change(&mut self) {
        self.link_changed = true;
        if let Some(env) = &self.env { (env.signal)(); }
    }

    pub fn view(&self) -> &CallView { self.flow.view() }
    pub fn devices(&self) -> &Devices { &self.devices }
    pub fn mic_open(&self) -> bool { self.input.is_some() }
    pub fn has_call_workers(&self) -> bool { self.current.is_some() || !self.stopping.is_empty() }
    pub fn messages_sent(&self) -> u64 { self.sent }

    pub fn take_facts(&mut self) -> Vec<Fact> {
        self.record_displayed_share();
        std::mem::take(&mut self.facts)
    }
    pub fn take_muted_sessions(&mut self) -> Vec<CallId> { std::mem::take(&mut self.muted_sessions) }
    pub fn ringing_session(&self) -> Option<CallId> { self.flow.session().filter(|_| self.view().ringing()) }

    fn now(&self) -> Now { Now { mono_us: self.clock.elapsed().as_micros() as u64 } }

    pub fn link_up(&mut self, host: &mut dyn AudioHost, me: PublicKey, peer: PublicKey, peer_name: &str) -> Option<MediaPort> {
        if self.env.is_none() {
            log!("Town call: configure the controller before linking");
            return None;
        }
        if self.links.contains_key(&peer) {
            self.link_down(host, peer);
        }
        let key = match MediaKey::generate() {
            Ok(key) => key,
            Err(err) => {
                log!("Town call: couldn't create a link media key: {err}");
                return None;
            }
        };
        self.next_epoch = self.next_epoch.wrapping_add(1);
        let peer_id = self.allocate_peer_id(peer);
        let (mut link, port) = Link::new(me, peer, key, peer_name.to_owned(), peer_id, self.next_epoch, &self.inlet);
        link.send(link.local_media_key.encode(), &mut self.sent);
        self.flow.set_local_key(me);
        self.flow.set_peer_name(peer, link.name.clone());
        self.links.insert(peer, link);
        self.notify_link_change();
        Some(port)
    }

    pub fn link_down(&mut self, host: &mut dyn AudioHost, peer: PublicKey) -> bool {
        let Some(link) = self.links.remove(&peer) else { return false };
        self.id_to_peer.remove(&link.peer_id);
        if let Some(call) = &mut self.current {
            if link.media_held.is_some() { call.leave(link.peer_id); }
        }
        let interrupted: Vec<_> = if self.current.as_ref().is_some_and(Call::stopping) {
            self.links.iter().filter_map(|(&peer, link)| link.media_held.map(|_| peer)).collect()
        } else { Vec::new() };
        self.retire_if_unused();
        self.sharing.detach_peer(peer, link.peer_id);
        link.report_unsent(&mut self.peer_events);
        self.peer_level.remove_peer(&peer);
        if let Some((_, peers)) = &mut self.open_town { peers.retain(|p| *p != peer); }
        self.share_peer_down(peer);
        let mut effects = Vec::new();
        self.flow.peer_down(peer, self.now(), &mut effects);
        self.apply_with_ended(host, effects, &interrupted);
        self.notify_link_change();
        true
    }

    pub fn link_down_all(&mut self, host: &mut dyn AudioHost) -> bool {
        let peers: Vec<_> = self.linked_peers().collect();
        let mut changed = false;
        for peer in peers { changed |= self.link_down(host, peer); }
        changed
    }

    pub fn shutdown(&mut self, host: &mut dyn AudioHost) {
        self.link_down_all(host);
        self.want_input = false;
        self.want_output = false;
        self.route(host);
    }

    pub fn list_devices(&mut self, host: &mut dyn AudioHost) -> bool {
        if self.env.is_some() && !self.enumerated_or_requested {
            self.enumerated_or_requested = true;
            let host = self.host(host);
            host.use_outputs(&[]);
            host.use_inputs(&[]);
        }
        false // The list arrives in audio_devices.
    }

    pub fn audio_devices(&mut self, host: &mut dyn AudioHost, descs: &[AudioDeviceDesc]) -> bool {
        if self.synthetic.is_some() { return false }
        self.enumerated_or_requested = true;
        let changes = self.devices.update(descs);
        self.route(host);
        changes != DeviceChanges::default()
    }

    pub fn pick(&mut self, host: &mut dyn AudioHost, kind: DeviceKind, id: AudioDeviceId) -> bool {
        let Some((changes, remember)) = self.devices.pick(kind, id) else { return false };
        self.route(host);
        if let Some(env) = self.env.as_ref().filter(|env| !env.devices_path.as_os_str().is_empty()) {
            if let Err(err) = env.files.replace_rebuildable(&env.devices_path, remember.0.into_bytes()) {
                log!("Town call: couldn't save the audio devices: {err}");
            }
        }
        changes != DeviceChanges::default()
    }

    fn install(&mut self, host: &mut dyn AudioHost) {
        if let Some((mic, speaker)) = self.uninstalled.take() {
            self.host(host).install(mic, speaker);
        }
    }

    fn route(&mut self, host: &mut dyn AudioHost) {
        let feed = self.env.as_ref().is_some_and(|env| env.voice == VoiceSource::Feed);
        let input = if self.wants_input() && feed { self.devices.chosen(DeviceKind::Input) } else { None };
        let output = if self.want_output { self.devices.chosen(DeviceKind::Output) } else { None };
        if input.is_some() || output.is_some() {
            self.install(host);
        }
        if input != self.input {
            self.host(host).use_inputs(input.as_slice());
            self.input = input;
            if let Some(call) = &self.current { call.reset_echo_cancel(); }
        }
        if output != self.output {
            self.host(host).use_outputs(output.as_slice());
            self.output = output;
            if let Some(call) = &self.current { call.reset_echo_cancel(); }
        }
    }

    fn attach_voice(&mut self, host: &mut dyn AudioHost) {
        if !self.voice_wanted || self.voice_attached || self.current.is_none() { return }
        let voice = self.env.as_ref().unwrap().voice;
        if voice == VoiceSource::Off { return }
        if self.echo_cancel == EchoCancelMode::Off && voice == VoiceSource::Feed {
            log!("{}", ECHO_CANCEL_NOTICE);
        }
        self.install(host);
        let call = self.current.as_mut().unwrap();
        call.add_voice(voice);
        call.set_muted(!self.want_input);
        let input = call.voice_in();
        let output = call.voice_out().expect("new voice has its output");
        self.voice_attached = true;
        if self.view().transcribing_me { self.transcribe(true); }
        self.control.attach(input, output);
    }

    fn retire_call(&mut self) {
        self.talking = Talking::default();
        for link in self.links.values_mut() { link.media_held = None; }
        if let Some(mut call) = self.current.take() {
            call.stop();
            self.stopping.push(call);
            self.sharing.detach();
            if std::mem::take(&mut self.voice_attached) {
                self.control.detach();
                self.speech.tap = None;
                if self.view().transcribing_me {
                    self.transcribe(false);
                    self.speech.status = TranscribeShown::Starting;
                }
            }
        }
    }

    fn retire_if_unused(&mut self) -> bool {
        if let Some(call) = &self.current {
            if call.stopping() || self.links.values().all(|link| link.media_held.is_none()) {
                self.retire_call();
                return true;
            }
        }
        false
    }

    fn allocate_peer_id(&mut self, peer: PublicKey) -> PeerId {
        let id = self.next_peer_id;
        self.next_peer_id = id.checked_add(1).expect("media peer IDs exhausted");
        self.id_to_peer.insert(id, peer);
        id
    }

    fn start_session(&mut self, peer: PublicKey, n: u32) {
        let link = &self.links[&peer];
        let remote = link.peer_media_key.as_ref().expect("session start needs both keys");
        let call_id = MediaKey::media_id(n, (&link.me, &link.local_media_key), (&peer, remote));
        let join = Join {
            peer: link.peer_id, their_pub: peer.to_vec(), key_id: 0,
            send_secret: link.local_media_key.session_secret(&call_id, &link.me),
            recv_secret: remote.session_secret(&call_id, &peer),
            out: link.out.clone(), stream_cap: Some(link.cap.clone()),
        };
        if self.current.is_none() {
            let env = self.env.as_ref().unwrap();
            self.current = Some(Call::start(CallConfig {
                inlet: Some(self.inlet()), my_pub: link.me.to_vec(), call_id, video: None, voice: VoiceSource::Off,
                echo_cancel: self.echo_cancel, codecs: self.codecs.clone(), spawn: env.spawn.clone(),
                signal: env.signal.clone(), clock: self.clock,
            }));
            self.call_generation += 1;
        }
        self.current.as_mut().unwrap().join_peer(join, call_id, self.flow.is_voice_peer(&peer));
        self.links.get_mut(&peer).unwrap().media_held = Some(n);
    }

    #[cfg(test)]
    fn send_media_to(&mut self, peer: PublicKey, signal: MediaSignal) {
        self.send_to(peer, Vec::try_from(signal).expect("the session machine sends valid numbers"));
    }

    fn send_to(&mut self, peer: PublicKey, body: Vec<u8>) {
        if let Some(link) = self.links.get_mut(&peer) { link.send(body, &mut self.sent); }
    }

    fn retire_peer(&mut self, peer: PublicKey) {
        let old = self.links[&peer].peer_id;
        if let Some(call) = &mut self.current {
            call.leave(old);
        }
        self.sharing.detach_peer(peer, old);
        self.id_to_peer.remove(&old);
        let id = self.allocate_peer_id(peer);
        let link = self.links.get_mut(&peer).unwrap();
        link.media_held = None;
        link.peer_id = id;
    }

    fn apply_media_for(&mut self, peer: PublicKey, changes: Changes) -> bool {
        self.apply_media_changes(vec![(peer, changes)])
    }

    fn apply_media_changes(&mut self, changes: Vec<(PublicKey, Changes)>) -> bool {
        let mut changed = false;
        for (peer, change) in &changes {
            if change.retire {
                self.retire_peer(*peer);
                changed = true;
            }
        }
        let mut interrupted: Vec<_> = if self.current.as_ref().is_some_and(Call::stopping) {
            self.links.iter().filter_map(|(&peer, link)| link.media_held.map(|_| peer)).collect()
        } else { Vec::new() };
        changed |= self.retire_if_unused();
        for (peer, change) in changes {
            if let Some(n) = change.announce_end {
                self.send_to(peer, Vec::try_from(MediaSignal::Media { n, on: false }).unwrap());
            }
            if let Some(n) = change.start { self.start_session(peer, n); changed = true; }
            if let Some(signal) = change.send { self.send_to(peer, Vec::try_from(signal).unwrap()); }
            if change.exhausted {
                let why = "Relink to start another media session.";
                if self.sharing.shown.problem.as_deref() != Some(why) {
                    log!("Town call: {why}");
                    self.sharing.shown.problem = Some(why.into());
                    changed = true;
                }
            }
        }
        self.attach_share();
        if self.current.as_ref().is_some_and(Call::stopping) {
            for (&peer, link) in &self.links {
                if link.media_held.is_some() && !interrupted.contains(&peer) { interrupted.push(peer); }
            }
            self.retire_call();
        }
        if !interrupted.is_empty() {
            let followup = self.media_changes(&interrupted);
            changed |= self.apply_media_changes(followup);
        }
        changed
    }

    fn media_changes(&mut self, ended: &[PublicKey]) -> Vec<(PublicKey, Changes)> {
        let mut changes = Vec::new();
        for (&peer, link) in &mut self.links {
            let own = self.flow.is_voice_peer(&peer)
                || self.sharing.peer_needs_share(&peer);
            let change = link.media.settle(link.me < peer, own, link.media_held.is_some(),
                ended.contains(&peer), link.peer_media_key.is_some());
            if change.retire || change.announce_end.is_some() || change.start.is_some() || change.send.is_some() || change.exhausted {
                changes.push((peer, change));
            }
        }
        changes
    }

    fn settle_media(&mut self) -> bool {
        self.settle_media_ended(&[])
    }

    fn settle_media_ended(&mut self, extra_ended: &[PublicKey]) -> bool {
        let stopped = self.current.as_ref().is_some_and(Call::stopping);
        let mut ended = extra_ended.to_vec();
        if stopped {
            ended.extend(self.links.iter().filter_map(|(&peer, link)| link.media_held.map(|_| peer)));
            self.retire_call();
        }
        let changes = self.media_changes(&ended);
        stopped | self.apply_media_changes(changes)
    }

    fn apply(&mut self, host: &mut dyn AudioHost, effects: Vec<Effect>) -> bool {
        self.apply_with_ended(host, effects, &[])
    }

    fn apply_with_ended(&mut self, host: &mut dyn AudioHost, effects: Vec<Effect>, ended: &[PublicKey]) -> bool {
        let changed = self.apply_effects(host, effects);
        let media_changed = self.settle_media_ended(ended);
        self.attach_voice(host);
        changed || media_changed
    }

    fn apply_effects(&mut self, host: &mut dyn AudioHost, effects: Vec<Effect>) -> bool {
        if self.view().phase != Phase::Idle {
            self.list_devices(host);
        }
        let mut changed = false;
        for effect in effects {
            match effect {
                Effect::StartLocalAudio => self.voice_wanted = true,
                Effect::PeerVoice { peer, on } => {
                    if let Some(link) = self.links.get(&peer).filter(|l| l.media_held.is_some()) {
                        if let Some(call) = &mut self.current { call.set_peer_voice(link.peer_id, on); }
                    }
                    if !on { self.peer_level.remove_peer(&peer); }
                }
                Effect::StopLocalAudio => {
                    self.sharing.close_picker();
                    self.speech.tap = None;
                    self.speech.status = TranscribeShown::Off;
                    self.talking = Talking::default();
                    self.peer_level.clear();
                    self.voice_wanted = false;
                    if let Some(call) = &mut self.current {
                        for link in self.links.values().filter(|l| l.media_held.is_some()) {
                            call.set_peer_voice(link.peer_id, false);
                        }
                    }
                    if std::mem::take(&mut self.voice_attached) {
                        self.control.detach();
                        if let Some(call) = &mut self.current { call.remove_voice(); }
                    }
                }
                Effect::Record(fact) => {
                    // TL1 flushes before Leave and then releases the pinned town. Let a
                    // stopped recognizer deliver its last utterance before exposing that fact.
                    if let Fact::Leave { session, .. } = fact {
                        if let Some(s) = self.speech.stretches.iter_mut().rev().find(|s| s.session == session) {
                            s.leave = Some(fact);
                            continue;
                        }
                    }
                    self.facts.push(fact);
                }
                Effect::Transcribe(on) => self.transcribe(on),
                Effect::Mic(false) => {
                    if let Some(id) = self.flow.session() { self.muted_sessions.push(id); }
                    if let Some(call) = &mut self.current { call.set_muted(true); }
                    self.want_input = false;
                    self.route(host);
                }
                Effect::Mic(true) => {
                    if self.view().unmuting {
                        self.unmute_at = self.blocks.load(Ordering::Relaxed);
                    }
                    self.want_input = true;
                    self.route(host);
                    if let Some(call) = &mut self.current { call.set_muted(false); }
                }
                Effect::Speaker(on) => {
                    self.want_output = on;
                    self.route(host);
                }
                Effect::Ring(on) => {
                    self.install(host);
                    self.control.ring(on);
                }
                Effect::SendTo { peer, body } => self.send_to(peer, body),
                Effect::Changed => changed = true,
            }
        }
        if changed && self.view().phase == Phase::Idle {
            self.sharing.shown.closed_notice = None;
        }
        self.route(host);
        changed
    }

    pub fn press(&mut self, host: &mut dyn AudioHost, press: Press) -> bool {
        let control_end = match press {
            Press::TakeBack if self.local_control() => Some(super::control::EndReason::TakenBack),
            Press::ReleaseControl if !self.local_control() => Some(super::control::EndReason::Released),
            _ => None,
        };
        if let Some(reason) = control_end {
            let changed = self.end_control(reason, true);
            if changed { self.settle_media(); }
            return changed;
        }
        if matches!(press, Press::Share(_) | Press::Watch | Press::CloseWatch | Press::StopShare | Press::StopRecording) {
            return self.share_press(press);
        }
        let mut effects = Vec::new();
        if press == Press::Transcribe && matches!(self.transcription_status(), TranscribeShown::Unavailable(_)) {
            return false;
        }
        self.flow.press(press, self.now(), &mut effects);
        if self.synthetic.is_some() && self.view().unmuting {
            self.flow.mic_flowing(self.now(), &mut effects);
        }
        self.apply(host, effects)
    }

    pub fn poll(&mut self, host: &mut dyn AudioHost) -> bool {
        let mut changed = std::mem::take(&mut self.link_changed);
        changed |= self.poll_transcription(host);
        if self.links.is_empty() && !self.has_call_workers() {
            return changed;
        }
        let peers: Vec<_> = self.linked_peers().collect();
        for peer in peers {
            loop {
                let link = self.links.get_mut(&peer).unwrap();
                let mut body = match link.incoming.try_recv() {
                    Ok(body) => body,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        changed |= self.link_down(host, peer);
                        break;
                    }
                };
                if let Some(key) = MediaKey::take_from(&mut body) {
                    if link.peer_media_key.is_none() {
                        link.peer_media_key = Some(key);
                    } else {
                        log!("Town call: ignoring a second MediaKey on this link");
                    }
                } else if let Some((topic, complete, payload)) = super::signal::peer_payload(&body) {
                    body.drain(..body.len() - payload.len());
                    link.peer_messages.receive(topic, complete, body);
                } else {
                    if let Some((last, payload)) = super::Message::part(&body) {
                        link.share_parts.extend_from_slice(payload);
                        if !last { continue }
                        body = std::mem::take(&mut link.share_parts);
                        if !super::Message::is_share_message(&body) { continue }
                    }
                    if MediaSignal::has_tag(&body) {
                        let link = self.links.get_mut(&peer).unwrap();
                        match MediaSignal::decode(&body) {
                            Some(signal) => match link.media.receive(link.me < link.peer, link.media_held.is_some(), link.peer_media_key.is_some(), signal) {
                                Ok(changes) => changed |= self.apply_media_for(peer, changes),
                                Err(why) => log!("Town call: ignoring {why}"),
                            },
                            None => log!("Town call: refused malformed media signal"),
                        }
                    } else if super::Message::is_share_message(&body) {
                        changed |= self.share_message(peer, &body);
                    } else {
                        let mut effects = Vec::new();
                        self.flow.receive(peer, &body, self.now(), &mut effects);
                        changed |= self.apply_effects(host, effects);
                    }
                }
                changed |= self.settle_media();
                self.attach_voice(host);
            }
        }
        for link in self.links.values_mut() {
            link.flush(&mut self.sent);
            changed |= link.flush_peer(&mut self.sent);
        }
        if self.view().phase != Phase::Idle {
            let mut effects = Vec::new();
            self.flow.tick(self.now(), &mut effects);
            if !effects.is_empty() { changed |= self.apply(host, effects); }
        }
        self.attach_voice(host);
        let watched_peer = self.watched_peer_id();
        if let Some(call) = &mut self.current {
            for event in call.poll() {
                if let CallEvent::Problem { peer, what } = event {
                    log!("Town call: {what}");
                    if peer.is_some() && peer == watched_peer && self.sharing.shown.picture.is_none() {
                        if let Some(offer) = self.sharing.shown.watching.and_then(|s| self.sharing.shown.offer_mut(s)) {
                            offer.decode_problem = Some(Arc::from(what));
                            changed = true;
                        }
                    }
                }
            }
        }
        for call in &mut self.stopping {
            for event in call.poll() {
                if let CallEvent::Problem { what, .. } = event {
                    log!("Town call: {what}");
                }
            }
        }
        if let Some(call) = &self.current {
            if self.last_talking.elapsed().as_millis() >= 100 {
                self.last_talking = Instant::now();
                let mut talking = Talking::default();
                for tile in call.tiles() {
                    if tile.who.is_none() { talking.you = tile.talking; }
                    else if tile.who.and_then(|id| self.id_to_peer.get(&id)).is_some_and(|peer| self.flow.is_voice_peer(peer)) {
                        talking.peer |= tile.talking;
                    }
                }
                changed |= talking != self.talking;
                self.talking = talking;
            }
        }
        self.sample_peer_level();
        self.stopping.retain(|call| !call.stopped());
        changed |= self.poll_share();
        self.control.tick();
        if self.view().unmuting && self.blocks.load(Ordering::Relaxed) != self.unmute_at {
            let mut effects = Vec::new();
            self.flow.mic_flowing(self.now(), &mut effects);
            changed |= self.apply(host, effects);
        }
        changed
    }
}

impl CallController {
    fn sync_level_timer(&mut self, cx: &mut Cx) {
        if self.view().phase == Phase::Live {
            if self.level_timer.is_empty() {
                self.level_timer = cx.start_interval(1.0 / super::level::HZ as f64);
            }
        } else if !self.level_timer.is_empty() {
            cx.stop_timer(std::mem::take(&mut self.level_timer));
        }
    }

    fn sample_peer_level(&mut self) {
        let flow = &self.flow;
        let call = self.current.as_ref();
        let keys = &self.id_to_peer;
        let now = self.now().mono_us;
        self.peer_level.sample(flow.view().phase, &flow.view().participants, now, || {
            call.into_iter().flat_map(|call| call.tiles()).filter_map(|tile| {
                let peer = keys.get(&tile.who?)?;
                (flow.is_voice_peer(peer) && !flow.is_peer_muted(peer) && !tile.muted).then_some((*peer, tile.level))
            })
        });
    }
}

impl PeerMessages {
    fn enqueue(&mut self, topic: u8, mut body: &[u8]) {
        use super::signal::{encode_peer, PEER_PAYLOAD};
        if body.len() > PEER_PAYLOAD { log!("Splitting a peer message across link frames"); }
        while body.len() > PEER_PAYLOAD {
            let (part, rest) = body.split_at(PEER_PAYLOAD);
            self.outgoing.push_back((None, encode_peer(topic, part, false)));
            body = rest;
        }
        self.outgoing.push_back((Some(topic), encode_peer(topic, body, true)));
    }

    fn receive(&mut self, topic: u8, complete: bool, body: Vec<u8>) {
        if !super::signal::topic::taken(topic) {
            if self.ignored.insert(topic) {
                makepad_widgets::log!("Town: dropping peer topic {topic}, which this build does not take");
            }
            return;
        }
        if !complete {
            self.partial.entry(topic).or_default().extend(body);
            return;
        }
        let message = match self.partial.remove(&topic) {
            Some(mut parts) => { parts.extend(body); parts }
            None => body,
        };
        self.incoming.entry(topic).or_default().push(message);
    }
}

impl Link {
    fn flush_peer(&mut self, sent: &mut u64) -> bool {
        let queue = &mut self.peer_messages.outgoing;
        while self.pending.is_empty() {
            let Some((topic, body)) = queue.pop_front() else { break };
            match self.outgoing.try_send(body) {
                Ok(()) => *sent += 1,
                Err(TrySendError::Full(body) | TrySendError::Disconnected(body)) => {
                    queue.push_front((topic, body));
                    break;
                }
            }
        }
        let waiting = !queue.is_empty();
        if waiting == self.peer_messages.waiting { return false }
        self.peer_messages.waiting = waiting;
        if waiting { log!("Peer messages are waiting for the link"); }
        true
    }

    fn report_unsent(self, events: &mut Vec<PeerEvent>) {
        let mut counts = std::collections::BTreeMap::new();
        for (topic, _) in self.peer_messages.outgoing {
            if let Some(topic) = topic { *counts.entry(topic).or_insert(0) += 1; }
        }
        events.extend(counts.into_iter().map(|(topic, count)| PeerEvent::PeerUndelivered { peer: self.peer, topic, count }));
    }
}

impl crate::tweak::Relay for CallController {
    fn peers(&self) -> Vec<PublicKey> { self.linked_peers().collect() }

    fn send(&mut self, peer: PublicKey, topic: u8, body: Vec<u8>) -> Result<(), NotLinked> {
        self.send_peer_to(peer, topic, body)
    }

    fn send_latest(&mut self, peer: PublicKey, topic: u8, body: Vec<u8>) -> Result<(), NotLinked> {
        self.send_peer_latest_to(peer, topic, body)
    }

    fn take(&mut self, topic: u8) -> Vec<(PublicKey, Vec<u8>)> {
        self.take_peer_with_sender(topic)
    }

    fn take_dropped(&mut self, topic: u8) -> Vec<crate::tweak::PeerUndelivered> {
        self.take_dropped_peer(topic)
    }
}

type RecognizerFactory = Box<dyn Fn(&TranscriberConfig, WakeHook) -> Box<dyn Recognizer>>;

struct Stretch {
    session: CallId,
    call_generation: u64,
    worker: Transcriber,
    stopping: bool,
    lines: VecDeque<Heard>,
    leave: Option<Fact>,
}

struct Speech {
    sender: Option<SyncSender<Heard>>,
    town: Option<(CallId, String)>,
    status: TranscribeShown,
    tap: Option<TranscriptTap>,
    stretches: Vec<Stretch>,
    recognize: RecognizerFactory,
}

impl Default for Speech {
    fn default() -> Self {
        Self { sender: None, town: None, status: TranscribeShown::Off, tap: None,
            stretches: Vec::new(), recognize: Box::new(Transcriber::hub_recognizer) }
    }
}

impl CallController {
    /// Queue an opaque message for this link. Call signals get the first free slots on poll.
    pub fn send_peer_to(&mut self, peer: PublicKey, topic: u8, body: Vec<u8>) -> Result<(), NotLinked> {
        let link = self.links.get_mut(&peer).ok_or(NotLinked)?;
        let wake = link.peer_messages.outgoing.is_empty();
        link.peer_messages.enqueue(topic, &body);
        if wake {
            if let Some(env) = &self.env { (env.signal)(); }
        }
        Ok(())
    }

    /// For a latest-only topic, mark its first frame as replaceable. Once that frame crosses
    /// the carrier, its remaining fragments must finish before the next whole message.
    /// All producers of the topic must use this method.
    pub fn send_peer_latest_to(&mut self, peer: PublicKey, topic: u8, body: Vec<u8>) -> Result<(), NotLinked> {
        use super::signal::{encode_peer, peer_payload, PEER_PAYLOAD};
        let link = self.links.get_mut(&peer).ok_or(NotLinked)?;
        let queue = &mut link.peer_messages.outgoing;
        let wake = queue.is_empty();
        let mut tail = VecDeque::new();
        if let Some(at) = queue.iter().rposition(|(start, _)| *start == Some(topic)) {
            if body.len() <= PEER_PAYLOAD && peer_payload(&queue[at].1).is_some_and(|(_, end, _)| end) {
                queue[at].1 = encode_peer(topic, &body, true);
                return Ok(());
            }
            tail = queue.split_off(at);
            while let Some((_, frame)) = tail.pop_front() {
                if peer_payload(&frame).is_some_and(|(_, end, _)| end) { break }
            }
        }
        let mut start = Some(topic);
        let mut rest = body.as_slice();
        while rest.len() > PEER_PAYLOAD {
            let (part, next) = rest.split_at(PEER_PAYLOAD);
            queue.push_back((start.take(), encode_peer(topic, part, false)));
            rest = next;
        }
        queue.push_back((start, encode_peer(topic, rest, true)));
        queue.append(&mut tail);
        if wake {
            if let Some(env) = &self.env { (env.signal)(); }
        }
        Ok(())
    }

    /// Drain this topic in arrival order. Each link starts with fresh inboxes.
    pub fn take_peer_with_sender(&mut self, topic: u8) -> Vec<(PublicKey, Vec<u8>)> {
        let mut received = Vec::new();
        for (&peer, link) in &mut self.links {
            if let Some(messages) = link.peer_messages.incoming.remove(&topic) {
                received.extend(messages.into_iter().map(|body| (peer, body)));
            }
        }
        received
    }

    pub fn take_dropped_peer(&mut self, topic: u8) -> Vec<crate::tweak::PeerUndelivered> {
        let mut dropped = Vec::new();
        self.peer_events.retain(|&PeerEvent::PeerUndelivered { peer, topic: lost, count }| {
            if lost != topic { return true }
            dropped.push(crate::tweak::PeerUndelivered { peer, topic, count });
            false
        });
        dropped
    }

    pub fn take_peer_events(&mut self) -> Vec<PeerEvent> {
        std::mem::take(&mut self.peer_events)
    }

    /// Called by the single peer-event dispatcher when the link ends with unsent input traffic.
    pub fn input_undelivered(&mut self, count: usize) {
        self.undelivered_input = self.undelivered_input.saturating_add(count);
        log!("The link ended with {count} undelivered control messages ({} total)", self.undelivered_input);
    }

    fn sync_peer_retry(&mut self, cx: &mut Cx) {
        // A quiet, idle link has no call timer. Retry a full port without spinning the UI.
        let waiting = self.links.values().any(|l| l.peer_messages.waiting);
        if waiting && self.peer_retry.is_empty() {
            self.peer_retry = cx.start_interval(0.02);
        } else if !waiting && !self.peer_retry.is_empty() {
            cx.stop_timer(std::mem::take(&mut self.peer_retry));
        }
    }

    pub fn set_transcript_sender(&mut self, sender: SyncSender<Heard>) {
        self.speech.sender = Some(sender);
    }

    pub fn has_transcript_town(&self, session: CallId) -> bool {
        self.speech.town.as_ref().is_some_and(|(id, _)| *id == session)
    }

    pub fn set_transcript_town(&mut self, session: CallId, name: Option<String>) {
        self.speech.town = name.map(|name| (session, name));
    }

    fn transcription_status(&self) -> TranscribeShown {
        if self.view().phase == Phase::Live && (self.speech.sender.is_none() ||
            self.speech.town.as_ref().map(|(id, _)| *id) != self.flow.session()) {
            return TranscribeShown::Unavailable("Transcription needs a shared town for this call.".into());
        }
        // T2a returns the one microphone tap after its last result. Only offer
        // to start when the press can actually enable capture.
        if let Some(session) = self.flow.session() {
            if self.speech.status == TranscribeShown::Off &&
                self.speech.stretches.iter().any(|s| s.session == session && s.stopping) {
                return TranscribeShown::Unavailable("Finishing your last utterance…".into());
            }
        }
        self.speech.status.clone()
    }

    fn transcribe(&mut self, on: bool) {
        if !on {
            // finish() has already cleared the flow's session on a hang-up.
            for s in &mut self.speech.stretches {
                if !s.stopping { s.worker.stop(); s.stopping = true; }
            }
            self.speech.status = TranscribeShown::Off;
            return;
        }
        self.speech.status = TranscribeShown::Starting;
        if !self.voice_attached { return }
        let session = self.flow.session().expect("only a live call starts transcription");
        let tap = self.speech.tap.take().or_else(|| self.current.as_mut()?.transcript_tap())
            .expect("each call owns its microphone tap between stretches");
        let env = self.env.as_ref().expect("live call has an environment");
        let config = TranscriberConfig::new(self.flow.origin_us);
        let rec = (self.speech.recognize)(&config, env.signal.clone());
        let worker = Transcriber::start(tap, config, rec, &env.spawn, env.signal.clone());
        self.speech.stretches.push(Stretch { session, call_generation: self.call_generation, worker, stopping: false, lines: VecDeque::new(), leave: None });
    }

    fn poll_transcription(&mut self, host: &mut dyn AudioHost) -> bool {
        if self.speech.stretches.is_empty() { return false }
        let active = self.flow.session();
        let sender = self.speech.sender.as_ref().expect("a transcriber has a timeline sender");
        let mut changed = false;
        let mut failed = None;
        let generation = self.voice_attached.then_some(self.call_generation);
        let mut draining = None;
        self.speech.stretches.retain_mut(|s| {
            // A restarted microphone can already capture while the old one's final words drain.
            if draining == Some(s.session) { return true }
            draining = Some(s.session);
            let mine = active == Some(s.session) && generation == Some(s.call_generation);
            for event in s.worker.poll() {
                match event {
                    TranscriberEvent::Line { at_ms, dur_ms, text } => s.lines.push_back(Heard {
                        session: s.session, at_ms: at_ms.try_into().unwrap_or(u32::MAX),
                        dur_ms: dur_ms.try_into().unwrap_or(u32::MAX), text,
                    }),
                    TranscriberEvent::Ready(name) if mine && !s.stopping => {
                        self.speech.status = TranscribeShown::On(name);
                        changed = true;
                    }
                    TranscriberEvent::Failed(why) if mine && !s.stopping => failed = Some(why),
                    _ => {}
                }
            }
            while let Some(line) = s.lines.pop_front() {
                if let Err(err) = sender.try_send(line) {
                    match err {
                        TrySendError::Full(line) => {
                            s.lines.push_front(line);
                            // The app drains TL1 after this poll. Retry on the next UI wake.
                            (self.env.as_ref().unwrap().signal)();
                        }
                        TrySendError::Disconnected(line) => s.lines.push_front(line),
                    }
                    return true;
                }
            }
            let Some(tap) = s.worker.take_stopped_tap() else { return true };
            if mine {
                self.speech.tap = Some(tap);
                changed = true;
                if s.stopping { self.muted_sessions.push(s.session); } // TL1's boundary flush, also on Off
            }
            if let Some(leave) = s.leave.take() { self.facts.push(leave); }
            draining = None;
            false
        });
        if let Some(why) = failed {
            self.press(host, Press::StopTranscribing);
            self.speech.status = TranscribeShown::Unavailable(why);
            changed = true;
        }
        changed
    }
}

impl Drop for CallController {
    fn drop(&mut self) {
        self.control.ring(false);
        self.retire_call();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use makepad_town_core::event::LeaveWhy;
    use crate::link_worker::{linked_pair, FromWorker, LinkWorker, ToWorker};
    use makepad_call::wire::stream::VOICE;
    use makepad_widgets::{makepad_platform::audio::AudioDeviceType, LiveId};
    use makepad_town_core::key::DeviceKey;
    use std::{panic::{catch_unwind, AssertUnwindSafe}, sync::atomic::AtomicUsize, thread, time::Duration};

    const MIC: AudioDeviceId = AudioDeviceId(LiveId(1));
    const SPEAKER: AudioDeviceId = AudioDeviceId(LiveId(2));
    const MIC2: AudioDeviceId = AudioDeviceId(LiveId(3));
    const SPEAKER2: AudioDeviceId = AudioDeviceId(LiveId(4));

    #[test]
    fn echo_cancel_mode_matches_platform_and_off_shows_honest_notice() {
        let mut ctl = CallController::default();
        let expected = if cfg!(any(target_os = "macos", target_os = "ios")) { EchoCancelMode::OsHardware }
            else if cfg!(any(target_os = "linux", windows)) { EchoCancelMode::Software }
            else { EchoCancelMode::Off };
        assert_eq!(ctl.echo_cancel, expected, "controller selects the platform's cancellation mode");
        ctl.echo_cancel = EchoCancelMode::Off;
        ctl.env = Some(CallEnv {
            files: Default::default(), devices_path: Default::default(), voice: VoiceSource::Feed,
            spawn: Arc::new(|_, _| panic!("the view should need no worker")),
            jobs: Arc::new(|_, _| panic!("the view should need no job")), signal: Arc::new(|| {}),
        });
        assert_eq!(ctl.shown().echo_cancel_notice, None, "no headphone notice while idle");
        // Drive the signalling state machine without applying device or worker effects.
        ctl.flow = CallFlow::new(Box::new(|bytes| bytes.fill(7)));
        let now = Now::default();
        let mut effects = Vec::new();
        ctl.flow.set_open_town(Some(([9; 32], vec![([1; 65], "Peer".into())])), &mut effects);
        ctl.flow.press(Press::Call, now, &mut effects);
        assert_eq!(ctl.view().phase, Phase::Calling, "outbound call entered Calling");
        assert_eq!(ctl.shown().echo_cancel_notice, None, "no headphone notice while calling");
        let answer = super::super::Message::Answer { call_id: [7; 16], secret: [8; 32], ext: super::super::AnswerExt { town: Some([9; 32]), ..Default::default() } };
        ctl.flow.receive([1; 65], &answer.encode(), now, &mut effects);
        assert_eq!(ctl.view().phase, Phase::Live, "peer answer made the call live");
        assert_eq!(ctl.shown().echo_cancel_notice.as_deref(), Some("Echo cancellation is off — use headphones to avoid echo."),
            "an unprotected live microphone shows the headphone notice");
        for mode in [EchoCancelMode::OsHardware, EchoCancelMode::Software] {
            ctl.echo_cancel = mode;
            assert_eq!(ctl.shown().echo_cancel_notice, None, "active {mode:?} cancellation hides the notice");
        }
        ctl.echo_cancel = EchoCancelMode::Off;
        ctl.env.as_mut().unwrap().voice = VoiceSource::Synthetic { tone_hz: 700.0 };
        assert_eq!(ctl.shown().echo_cancel_notice, None, "synthetic capture needs no headphone notice");
    }

    #[derive(Debug, PartialEq)]
    enum Use {
        Install,
        Input(Option<AudioDeviceId>),
        Output(Option<AudioDeviceId>),
    }

    #[derive(Default)]
    struct Host {
        history: Vec<Use>,
        mic: Option<MicPort>,
        speaker: Option<OutputPort>,
        input: Option<AudioDeviceId>,
        output: Option<AudioDeviceId>,
        listed: bool,
        tone: f64,
        phase: f64,
    }

    impl AudioHost for Host {
        fn install(&mut self, mic: MicPort, speaker: OutputPort) {
            assert!(self.mic.is_none() && self.speaker.is_none(), "install both callbacks only once");
            self.mic = Some(mic);
            self.speaker = Some(speaker);
            self.history.push(Use::Install);
        }

        fn use_inputs(&mut self, ids: &[AudioDeviceId]) {
            assert!(ids.len() <= 1, "route at most one microphone");
            self.input = ids.first().copied();
            self.history.push(Use::Input(self.input));
        }

        fn use_outputs(&mut self, ids: &[AudioDeviceId]) {
            assert!(ids.len() <= 1, "route at most one speaker");
            self.output = ids.first().copied();
            self.history.push(Use::Output(self.output));
        }
    }

    impl Host {
        fn deliver_devices(&mut self, controller: &mut CallController, second_pair: bool) {
            if self.listed || self.history.is_empty() { return }
            assert_eq!(&self.history[..2], &[Use::Output(None), Use::Input(None)], "enumerate with both devices closed");
            self.listed = true;
            let mut descs = Vec::new();
            for (id, ty, name, default, channels) in [
                (MIC, AudioDeviceType::Input, "Desk mic", true, 1),
                (SPEAKER, AudioDeviceType::Output, "Desk speaker", true, 2),
                (MIC2, AudioDeviceType::Input, "Headset mic", false, 2),
                (SPEAKER2, AudioDeviceType::Output, "Headset speaker", false, 2),
            ] {
                if default || second_pair {
                    descs.push(AudioDeviceDesc { device_id: id, device_type: ty, name: name.into(),
                        is_default: default, has_failed: false, channel_count: channels });
                }
            }
            assert!(controller.audio_devices(self, &descs), "the first device list changes the picker");
        }

        fn push_block(&mut self) {
            let id = self.input.expect("only an open fake mic can capture");
            let (rate, channels) = if id == MIC2 { (44_100, 2) } else { (48_000, 1) };
            let mut data = Vec::new();
            for _ in 0..rate / 50 {
                let sample = (self.phase.sin() * 0.3) as f32;
                self.phase = (self.phase + std::f64::consts::TAU * self.tone / rate as f64) % std::f64::consts::TAU;
                for _ in 0..channels { data.push(sample); }
            }
            let mic = self.mic.as_mut().expect("install the mic before opening it");
            if channels == 1 {
                mic.push_mono(rate as f64, &data);
            } else {
                mic.push_interleaved(rate as f64, channels, &data);
            }
        }

        fn output_rate(&self) -> usize {
            match self.output.expect("only an open fake speaker can play") {
                SPEAKER2 => 44_100,
                _ => 48_000,
            }
        }

        fn pull(&mut self, ms: usize) -> Vec<f32> {
            let rate = self.output_rate();
            let mut stereo = vec![0.0; 2 * rate * ms / 1000];
            self.speaker.as_mut().expect("install the speaker before opening it")
                .fill_interleaved(rate as f64, 2, &mut stereo);
            stereo.chunks_exact(2).map(|lr| {
                assert_eq!(lr[0], lr[1], "both speaker channels must play the mono mix");
                lr[0]
            }).collect()
        }

        fn seq(&mut self) -> Option<u32> {
            self.speaker.as_mut()?.received_seq(1, VOICE)
        }
    }

    struct Seat {
        worker: LinkWorker,
        controller: CallController,
        host: Host,
    }

    struct Rig {
        drained: u64,
        seats: [Seat; 2],
        panics: Arc<AtomicUsize>,
    }

    impl Rig {
        fn drain_events(&mut self) {
            for (index, seat) in self.seats.iter_mut().enumerate() {
                while let Some(event) = seat.worker.try_recv() {
                    self.drained += 1;
                    match event {
                        FromWorker::Failed(why) => panic!("link worker {index} failed: {why}"),
                        FromWorker::Lost => panic!("link worker {index} lost its peer"),
                        _ => {}
                    }
                }
            }
        }

        fn wait(&mut self, mut ready: impl FnMut(&mut [Seat; 2]) -> bool) {
            loop {
                self.drain_events();
                for (index, seat) in self.seats.iter_mut().enumerate() {
                    seat.controller.poll(&mut seat.host);
                    assert!(seat.controller.linked_peers().next().is_some(), "link worker {index} stopped");
                    assert!(!seat.controller.current.as_ref().is_some_and(Call::stopped), "active call worker {index} stopped");
                    seat.host.deliver_devices(&mut seat.controller, index == 0);
                }
                assert_eq!(self.panics.load(Ordering::Relaxed), 0, "a session job panicked");
                if ready(&mut self.seats) { return }
                thread::sleep(Duration::from_millis(2));
            }
        }

        fn press(&mut self, side: usize, press: Press) {
            let seat = &mut self.seats[side];
            assert!(seat.controller.press(&mut seat.host, press), "{press:?} must change the view");
        }

        fn frames(&mut self, last: &mut [Option<u32>; 2], count: usize) -> u32 {
            let mut max_gap = 0;
            let mut heard = [Vec::new(), Vec::new()];
            for block in 0..count {
                for seat in &mut self.seats { seat.host.push_block(); }
                let mut newest = [None; 2];
                self.wait(|seats| {
                    for (i, seat) in seats.iter_mut().enumerate() { newest[i] = seat.host.seq(); }
                    (0..2).all(|i| newest[i].is_some() && newest[i] != last[i])
                });
                for (i, seat) in self.seats.iter_mut().enumerate() {
                    let seq = newest[i].unwrap();
                    let expected = last[i].map_or(0, |old| old + 1);
                    assert!(seq >= expected, "receiver {i} went backwards at block {block}: {seq} < {expected}");
                    let gap = seq - expected;
                    if i == 1 { max_gap = max_gap.max(gap); }
                    assert!(gap <= u32::from(i == 1 && block == 0 && last[i].is_some()),
                        "receiver {i} skipped {gap} frames at block {block}");
                    last[i] = Some(seq);
                    let samples = seat.host.pull(20);
                    // Let the jitter buffer start before measuring the following 400 ms.
                    if block >= 5 { heard[i].extend(samples); }
                }
            }
            for (i, samples) in heard.iter().enumerate() {
                let host = &self.seats[i].host;
                let rate = host.output_rate() as f64;
                let remote = power(samples, self.seats[1-i].host.tone, rate);
                assert!(remote > 0.005, "seat {i} must play the remote tone at {rate} Hz, power {remote}");
                for hz in [host.tone, 440.0, 480.0] {
                    let unwanted = power(samples, hz, rate);
                    assert!(unwanted < remote / 100.0, "seat {i} played {hz} Hz, power {unwanted}, remote {remote}");
                }
            }
            max_gap
        }
    }

    fn power(samples: &[f32], hz: f64, rate: f64) -> f64 {
        let c = 2.0 * (std::f64::consts::TAU * hz / rate).cos();
        let (mut a, mut b) = (0.0, 0.0);
        for &sample in samples { (a, b) = (f64::from(sample) + c * a - b, a); }
        (a*a + b*b - c*a*b) / (samples.len() as f64).powi(2)
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn opening_controls_follow_capture_intent_and_synthetic_calls_isolate_the_host() {
        use super::super::Message;
        for synthetic in [false, true] {
            let key = DeviceKey::generate().unwrap().public();
            let unique = u64::from_le_bytes(key[1..9].try_into().unwrap());
            let dir = std::env::temp_dir().join(format!("town-call-actions-{unique:x}"));
            fs::create_dir(&dir).unwrap();
            let dir = TempDir(dir);
            let mut host = Host::default();
            let mut ctl = CallController::default();
            assert!(!ctl.link_down(&mut host, key) && host.history.is_empty(), "unlinked teardown touches no host");
            assert!(!ctl.shown().linked && ctl.shown().view.phase == Phase::Idle);
            ctl.configure(CallEnv {
                files: Default::default(),
                voice: if synthetic { VoiceSource::Synthetic { tone_hz: 700.0 } } else { VoiceSource::Feed },
                spawn: makepad_call::driver::std_spawn(), jobs: Arc::new(|_, job| job()), signal: Arc::new(|| {}),
                devices_path: dir.0.join("devices"),
            });
            assert!(!ctl.link_down(&mut host, key) && host.history.is_empty(), "a failure before link proof is a no-op");
            let peer = DeviceKey::generate().unwrap().public();
            let port = ctl.link_up(&mut host, key, peer, "Peer").unwrap();
            ctl.set_open_town(Some(([9; 32], vec![(peer, "Peer".into())])));
            ctl.set_peer_caps(peer, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
            let desc = AudioDeviceDesc { device_type: AudioDeviceType::Input, device_id: MIC,
                name: "Manual mic".into(), is_default: false, has_failed: false, channel_count: 1 };
            ctl.audio_devices(&mut host, &[desc]);
            let shown = ctl.shown();
            assert!(shown.linked && shown.view.peer_name == "Peer");
            let share = CallAction::Press(Press::Share(ShareChoice::Menu));
            let row = CallAction::Press(Press::Share(ShareChoice::Row(1)));
            assert!(ctl.opening_from_ui(share) && !ctl.call_opening_presses(&mut host, row), "idle Share is available but a stale row cannot select a window");
            let pick = CallAction::Pick(DeviceKind::Input, MIC);
            if synthetic {
                assert_eq!(shown.input.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [SyntheticHost::MIC]);
                assert_eq!(shown.output.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [SyntheticHost::SPEAKER]);
                assert!(!ctl.pick(&mut host, DeviceKind::Input, MIC), "real IDs cannot enter the synthetic picker");
                let fake = ctl.synthetic.as_mut().unwrap();
                fake.use_inputs(&[SyntheticHost::MIC]);
                fake.use_inputs(&[MIC]);
                fake.use_outputs(&[SPEAKER]);
                assert_eq!((fake.input, fake.output), (Some(SyntheticHost::MIC), None), "unknown IDs cannot change a synthetic route");
                fake.use_inputs(&[]);
            }
            for press in [Press::Call, Press::CallSeat([9; 32]),
                Press::JoinCall([9; 16], [9; 32], crate::call::CallScope::Seats { caller: [1; 32], callee: [2; 32] }), Press::Answer, Press::Unmute, Press::Transcribe, Press::Clip, Press::Snap, Press::FirstCaptureWindow] {
                assert!(ctl.opening(CallAction::Press(press)), "{press:?} needs the person's input");
            }
            for press in [Press::Decline, Press::Cancel, Press::HangUp, Press::Mute, Press::StopTranscribing, Press::NotMe, Press::StopRecording] {
                assert!(!ctl.opening(CallAction::Press(press)), "{press:?} is a closing control");
            }
            let record = CallAction::Press(Press::Clip);
            assert!(ctl.opening_from_ui(record), "local recording is available before a voice call starts");
            assert!(!ctl.call_actions(&mut host, record) && ctl.shown().share.choices.is_none(),
                "an injected Record must leave the picker closed, including with synthetic audio");
            let call = CallAction::Press(Press::Call);
            assert_eq!(ctl.call_actions(&mut host, call), synthetic, "only synthetic checks may inject Call");
            if !synthetic { assert!(ctl.call_opening_presses(&mut host, call)); }
            assert_eq!(ctl.shown().view.phase, Phase::Calling);
            assert!(ctl.call_actions(&mut host, CallAction::Press(Press::Cancel)));
            port.messages_in.try_send(Message::Ring { call_id: [9; 16], secret: [3; 32], video: false, ext: super::super::RingExt { town: Some([9; 32]), ..Default::default() } }.encode()).unwrap();
            ctl.poll(&mut host);
            assert!(ctl.shown().view.ringing() && !ctl.wants_input());
            let answer = CallAction::Press(Press::Answer);
            assert_eq!(ctl.call_actions(&mut host, answer), synthetic, "only synthetic checks may inject Answer");
            if !synthetic { assert!(ctl.call_opening_presses(&mut host, answer)); }
            let shown = ctl.shown();
            assert_eq!(shown.view.phase, Phase::Live);
            assert!(ctl.opening_from_ui(share) && ctl.opening_from_ui(row));
            assert!(ctl.call_opening_presses(&mut host, share) && ctl.shown().share.choices.is_some(), "the live Share opens its picker");
            assert!(ctl.wants_input() && !shown.mic_open, "live capture is wanted even with no eligible physical route");
            assert!(ctl.opening(pick) && !ctl.opening(CallAction::Pick(DeviceKind::Output, SPEAKER)));
            if !synthetic {
                assert_eq!(shown.input.label, "No microphone");
                assert!(!ctl.call_actions(&mut host, pick), "an injected pick cannot start capture");
                assert!(!host.history.iter().any(|op| matches!(op, Use::Input(Some(_)))), "no transient input open");
                assert!(ctl.call_opening_presses(&mut host, pick), "the local pick may open this microphone");
                assert_eq!(host.input, Some(MIC));
            }
            assert!(ctl.call_actions(&mut host, CallAction::Press(Press::Mute)));
            assert!(!ctl.wants_input() && !ctl.opening(pick) && !ctl.opening(CallAction::Pick(DeviceKind::Output, SPEAKER)));
            let unmute = CallAction::Press(Press::Unmute);
            assert_eq!(ctl.call_actions(&mut host, unmute), synthetic);
            if synthetic { assert!(!ctl.view().unmuting, "synthetic unmute needs no input callback"); }
            assert!(ctl.call_actions(&mut host, CallAction::Press(Press::HangUp)));
            assert_eq!(ctl.shown().view.phase, Phase::Idle);
            assert!(ctl.shown().share.choices.is_none() && !ctl.call_opening_presses(&mut host, row), "a pending picker cannot start a share after hang-up");
            ctl.shutdown(&mut host);
            if synthetic { assert!(host.history.is_empty(), "the passed host never receives any operation"); }
        }
    }

    #[test]
    fn linked_voice_call_switches_devices_without_interrupting_audio() {
        let (_homes, mut caller, mut callee) = crate::model::tests::shared_town("call-speech");
        let keys = [caller.device_key(), callee.device_key()];
        let start = Instant::now();
        let (workers, progress) = linked_pair(keys[0].clone(), keys[1].clone());
        let setup = start.elapsed();
        let unique = u64::from_le_bytes(keys[0].public()[1..9].try_into().unwrap());
        let dir = std::env::temp_dir().join(format!("town-call-{unique:x}"));
        fs::create_dir(&dir).expect("a fresh preference directory");
        let dir = TempDir(dir);
        let paths = [dir.0.join("a"), dir.0.join("b")];
        let panics: Arc<AtomicUsize> = Arc::default();
        let watched = panics.clone();
        let spawn: Spawn = Arc::new(move |name, job| {
            let panics = watched.clone();
            thread::Builder::new().name(name.into()).spawn(move || {
                if catch_unwind(AssertUnwindSafe(job)).is_err() { panics.fetch_add(1, Ordering::Relaxed); }
            }).expect("spawn a watched job");
        });
        let mut workers = workers.into_iter();
        let mut attached = Vec::new();
        let seats = std::array::from_fn(|i| {
            let (worker, peer) = workers.next().unwrap();
            let mut controller = CallController::default();
            controller.configure(CallEnv {
                files: Default::default(),
                spawn: spawn.clone(), jobs: spawn.clone(), signal: Arc::new(|| {}), devices_path: paths[i].clone(), voice: VoiceSource::Feed,
            });
            let mut host = Host { tone: [700.0, 1100.0][i], ..Default::default() };
            let port = controller.link_up(&mut host, keys[i].public(), peer, ["Bob", "Ada"][i]).unwrap();
            controller.set_open_town(Some((caller.open_id(), vec![(peer, ["Bob", "Ada"][i].into())])));
            // A delivery to this port means the worker is past its initial Linked/Path events.
            port.sent_bytes.store(u64::MAX, Ordering::Relaxed);
            attached.push(port.sent_bytes.clone());
            assert!(worker.try_send(ToWorker::Call(port)).is_ok(), "attach the new media port");
            Seat { worker, controller, host }
        });
        let mut rig = Rig { seats, panics, drained: 0 };
        for seat in &rig.seats {
            assert_eq!(seat.controller.view().phase, Phase::Idle, "linking leaves the call idle");
            assert!(seat.host.history.is_empty(), "an idle link touches no audio device");
        }

        rig.wait(|_| attached.iter().all(|n| n.load(Ordering::Relaxed) != u64::MAX));
        let mark = progress.said();
        rig.drain_events();
        let at_mark = rig.drained;
        rig.press(0, Press::Call);
        rig.wait(|s| s[1].controller.view().ringing());
        loop {
            let signals = progress.said();
            rig.drain_events();
            if signals - mark > rig.drained - at_mark { break }
            progress.wait_past(signals);
        }
        assert_eq!(rig.seats[0].controller.view().phase, Phase::Calling, "the caller waits for an answer");
        for seat in &rig.seats {
            assert!(!seat.controller.mic_open() && !seat.host.history.iter().any(|op| matches!(op, Use::Input(Some(_)))),
                "neither seat may open a microphone while ringing");
        }
        assert_eq!(rig.seats[0].host.output, None, "the caller is silent while calling");
        assert_eq!(rig.seats[1].host.output, Some(SPEAKER), "the callee opens its speaker to ring");
        let ring = rig.seats[1].host.pull(100);
        for hz in [440.0, 480.0] {
            assert!(power(&ring, hz, 48_000.0) > 0.002, "the incoming ring must contain {hz} Hz");
        }

        rig.press(1, Press::Answer);
        assert!(rig.seats[1].controller.mic_open(), "the answer press opens the callee's mic");
        assert!(!rig.seats[0].controller.mic_open(), "the caller must process the answer before opening its mic");
        rig.wait(|s| s.iter().all(|seat| seat.controller.view().phase == Phase::Live));
        for seat in &rig.seats {
            assert!(seat.controller.mic_open(), "the live microphone indicator follows the route");
            assert_eq!(seat.host.input, Some(MIC), "each seat opens its default microphone after answer");
            assert_eq!(seat.host.output, Some(SPEAKER), "each seat opens its default speaker");
        }
        let mut last = [None; 2];
        let frames = 25;
        // Voice joins one Want/Media round trip after Live.
        rig.wait(|s| s.iter().all(|seat| seat.controller.voice_attached));
        rig.frames(&mut last, frames);
        let a = &mut rig.seats[0];
        let before = a.host.history.len();
        let sent = a.controller.messages_sent();
        assert!(a.controller.pick(&mut a.host, DeviceKind::Input, MIC2), "the picked microphone changes");
        assert!(a.controller.pick(&mut a.host, DeviceKind::Output, SPEAKER2), "the picked speaker changes");
        assert_eq!(&a.host.history[before..], &[Use::Input(Some(MIC2)), Use::Output(Some(SPEAKER2))],
            "switch each route once without reinstalling the callbacks");
        let gap = rig.frames(&mut last, frames);
        let meters = rig.seats.each_ref().map(|seat| seat.controller.shown());
        rig.wait(|_| meters.iter().all(|m| m.peer_level_step() > 0));
        let speaking = meters.each_ref().map(CallShown::peer_level_step);

        // End B's tone on the wire. Keep rendering the received frames until B's
        // decoded silence actually reaches A's speaker.
        loop {
            let last = rig.seats[0].host.seq();
            rig.seats[1].host.mic.as_mut().unwrap().push_mono(48_000., &[0.; 960]);
            rig.wait(|s| s[0].host.seq() != last);
            if rig.seats[0].host.pull(20).iter().all(|x| *x == 0.0) { break; }
        }
        rig.wait(|_| meters[0].peer_level_step() == 0);
        assert!(meters[1].peer_level_step() > 0, "B still displays the last tone it played from A");
        assert_eq!(rig.seats[0].controller.messages_sent(), sent, "local device switches send no signalling");
        let expected = "input Headset mic\noutput Headset speaker\n";
        assert_eq!(fs::read_to_string(&paths[0]).unwrap(), expected, "the last microphone and speaker picks are saved in order");

        rig.press(0, Press::Mute);
        assert_eq!(rig.seats[0].host.input, None, "mute closes the selected microphone immediately");
        rig.wait(|s| s[1].controller.view().peer_muted);
        let stale = rig.seats[1].controller.current.as_ref().unwrap().tiles().iter()
            .find(|tile| tile.who == Some(rig.seats[1].controller.links.values().next().unwrap().peer_id)).unwrap().level;
        assert!(stale > 0.0, "B has not rendered a new block since A muted");
        assert_eq!(meters[1].peer_level_step(), 0, "the mute signal clears even a previously shown meter");
        eprintln!("peer ring: talk {speaking:?}/16, silence 0, mute 0 at cached RMS {stale:.3}");
        rig.press(0, Press::Unmute);
        assert_eq!(rig.seats[0].host.input, Some(MIC2), "unmute reopens the selected microphone");
        rig.wait(|s| !s[1].controller.view().peer_muted);
        assert!(rig.seats[0].controller.view().unmuting, "polling cannot finish unmute before a microphone block");
        rig.seats[0].host.push_block();
        rig.wait(|s| !s[0].controller.view().unmuting);

        rig.press(0, Press::HangUp);
        assert!(!rig.seats[0].controller.mic_open(), "hang-up closes capture before control returns");
        rig.wait(|s| s.iter().all(|seat| seat.controller.view().phase == Phase::Idle && !seat.controller.has_call_workers()));
        for (i, seat) in rig.seats.iter().enumerate() {
            assert!(seat.host.history.ends_with(&[Use::Input(None), Use::Output(None)]), "seat {i} closed both devices");
            assert_eq!(seat.host.history.iter().filter(|op| **op == Use::Install).count(), 1, "seat {i} installed once");
            assert!(!seat.controller.mic_open(), "seat {i} has no live mic indicator after hang-up");
            let facts = &seat.controller.facts;
            let session = match (i, facts.first()) {
                (0, Some(Fact::Open { session, video: false, .. })) | (1, Some(Fact::Join { session, .. })) => *session,
                other => panic!("seat {i} must record its own open or join: {other:?}"),
            };
            assert_eq!(facts.len(), 2, "each seat records exactly two facts");
            assert!(matches!(facts[1], Fact::Leave { session: id, secs: Some(_), why: LeaveWhy::Ended } if id == session), "seat {i} ends the same session");
        }
        assert_eq!(rig.panics.load(Ordering::Relaxed), 0, "no call or preference job panicked");
        eprintln!("linked voice: link_setup_ms={:.3} frames={frames}+{frames} per direction switch_gap={gap}",
            setup.as_secs_f64() * 1000.0);
        check_linked_transcription(&mut rig, &mut caller, &mut callee);
        // Last: it ends with seat 0's link going down.
        peer_messages_on_idle_link(&mut rig);

    }

    #[test]
    fn peer_epoch_distinguishes_relinks_to_the_same_key() {
        let mut call = CallController::default();
        let mut host = Host::default();
        call.configure(CallEnv {
            files: Default::default(),
            voice: VoiceSource::Off, devices_path: Default::default(), signal: Arc::new(|| {}),
            spawn: Arc::new(|_, _| panic!("linking started a call worker")),
            jobs: Arc::new(|_, _| panic!("linking started an audio job")),
        });
        let (me, peer) = ([1; 65], [2; 65]);
        assert_eq!(call.peer_epoch_of(&peer), None, "an idle controller has no peer");
        let _first = call.link_up(&mut host, me, peer, "Peer").unwrap();
        let (key, first) = call.peer_epoch_of(&peer).unwrap();
        assert_eq!(key, peer, "the active link supplies the peer's key");
        call.poll(&mut host);
        assert_eq!(call.peer_epoch_of(&peer), Some((peer, first)), "polling preserves the active epoch");
        call.link_down(&mut host, peer);
        assert_eq!(call.peer_epoch_of(&peer), None, "unlink clears the peer identity");
        let _second = call.link_up(&mut host, me, peer, "Peer").unwrap();
        let (key, second) = call.peer_epoch_of(&peer).unwrap();
        assert_eq!(key, peer, "the reconnected link belongs to the same peer");
        assert_ne!(second, first, "reconnecting to the same key starts a new epoch");
    }

    #[test]
    fn media_keys_precede_presses_keep_the_first_peer_key_and_rotate_on_relink() {
        let devices = [[1; 65], [2; 65]];
        let mut hosts = [Host::default(), Host::default()];
        let mut controllers = devices.map(|_| {
            let mut ctl = CallController::default();
            ctl.configure(CallEnv {
                files: Default::default(),
                voice: VoiceSource::Off, devices_path: Default::default(), signal: Arc::new(|| {}),
                spawn: Arc::new(|_, _| panic!("the key exchange started a media worker")),
                jobs: Arc::new(|_, _| panic!("the key exchange started a job")),
            });
            ctl
        });
        let mut previous: Option<[[u8; 32]; 2]> = None;
        for caller in 0..2 {
            // The second link_up replaces a live link to the same peer on both seats.
            let ports: [_; 2] = std::array::from_fn(|i| {
                let port = controllers[i].link_up(&mut hosts[i], devices[i], devices[1-i], "Peer").unwrap();
                controllers[i].set_open_town(Some(([9; 32], vec![(devices[1-i], "Peer".into())])));
                assert!(controllers[i].links.get(&devices[1-i]).unwrap().peer_media_key.is_none());
                port
            });
            // Hold both directions. The carrier hasn't drained either key when the person presses Call.
            controllers[caller].press(&mut hosts[caller], Press::Call);
            let first = ports.each_ref().map(|p| p.messages_out.try_recv().unwrap());
            let fingerprints = first.each_ref().map(|body| makepad_network::digest::sha256_hash(body));
            assert_ne!(fingerprints[0], fingerprints[1], "each direction draws its own key");
            if let Some(old) = previous {
                for fingerprint in fingerprints { assert!(!old.contains(&fingerprint), "relink reused a media key"); }
            }
            previous = Some(fingerprints);
            for (i, body) in first.into_iter().enumerate() {
                assert_eq!(body.len(), 34);
                assert_eq!(&body[..2], &[1, 12], "the first Call-channel body is MediaKey");
                ports[1-i].messages_in.try_send(body).unwrap();
            }
            let ring = ports[caller].messages_out.try_recv().unwrap();
            assert!(matches!(super::super::decode(&ring), Ok(Some(super::super::Message::Ring { .. }))));
            for (ctl, host) in controllers.iter_mut().zip(&mut hosts) { ctl.poll(host); }
            for i in 0..2 {
                let link = controllers[i].links.get(&devices[1-i]).unwrap();
                assert!(link.peer_media_key.as_ref() == Some(&controllers[1-i].links.get(&devices[i]).unwrap().local_media_key));
                // Offer a different, well-formed key as a duplicate.
                ports[i].messages_in.try_send(link.local_media_key.encode()).unwrap();
            }
            ports[1-caller].messages_in.try_send(ring).unwrap();
            for (ctl, host) in controllers.iter_mut().zip(&mut hosts) { ctl.poll(host); }
            for i in 0..2 {
                let link = controllers[i].links.get(&devices[1-i]).unwrap();
                assert!(link.peer_media_key.as_ref() == Some(&controllers[1-i].links.get(&devices[i]).unwrap().local_media_key),
                    "a duplicate replaced the first peer key");
                assert!(ports[i].messages_out.try_recv().is_err(), "key exchange sends no reply");
            }
            assert_eq!(controllers[1-caller].view().phase, Phase::Ringing);
        }
    }

    #[test]
    fn a_full_port_gives_its_next_slot_to_the_call() {
        use super::super::{Message, topic::TWEAK};
        let (mut link, port) = Link::new([1; 65], [2; 65], MediaKey::generate().unwrap(), "Peer".into(), 1, 1, &Inlet::new());
        let mut sent = 0;
        // Stop reading the bounded port until both a peer message and a Ring are waiting.
        while link.outgoing.try_send(Vec::new()).is_ok() {}
        let body = vec![5; makepad_town_core::connect::MAX_CALL];
        link.peer_messages.enqueue(TWEAK, &body);
        assert!(link.flush_peer(&mut sent));
        let ring = Message::Ring { call_id: [1; 16], secret: [2; 32], video: false, ext: Default::default() }.encode();
        link.send(ring.clone(), &mut sent);
        port.messages_out.try_recv().unwrap();
        link.flush(&mut sent);
        link.flush_peer(&mut sent);
        assert_eq!(port.messages_out.try_iter().last(), Some(ring), "Ring got the only free slot");
        assert_eq!(sent, 1);

        // The first fragment can cross just before disconnect. Count the logical message once.
        while link.outgoing.try_send(Vec::new()).is_ok() {}
        port.messages_out.try_recv().unwrap();
        link.flush_peer(&mut sent);
        assert_eq!(sent, 2, "only the first peer fragment could cross");
        let mut events = Vec::new();
        link.report_unsent(&mut events);
        assert_eq!(events, [PeerEvent::PeerUndelivered { peer: [2; 65], topic: TWEAK, count: 1 }]);
    }

    fn peer_messages_on_idle_link(rig: &mut Rig) {
        use super::super::topic::TWEAK;
        const MARKER: u8 = 2;
        let peers = rig.seats.each_ref().map(|s| s.controller.linked_peers().next().unwrap());
        let burst: Vec<_> = (0..1000u16).map(|n| n.to_be_bytes().to_vec()).collect();
        for body in &burst {
            rig.seats[0].controller.send_peer_to(peers[0], TWEAK, body.clone()).unwrap();
        }
        rig.seats[0].controller.send_peer_to(peers[0], MARKER, vec![]).unwrap();
        let large: Vec<_> = (0..2 * makepad_town_core::connect::MAX_CALL).map(|n| (n % 251) as u8).collect();
        let reverse = [vec![2], large, vec![], vec![1]];
        for body in &reverse {
            rig.seats[1].controller.send_peer_to(peers[1], TWEAK, body.clone()).unwrap();
        }
        // A topic this build does not take, short and in pieces, sent ahead of the marker.
        const UNKNOWN: u8 = 200;
        for body in [vec![9], vec![9; 2 * makepad_town_core::connect::MAX_CALL]] {
            rig.seats[1].controller.send_peer_to(peers[1], UNKNOWN, body).unwrap();
        }
        rig.seats[1].controller.send_peer_to(peers[1], MARKER, vec![]).unwrap();

        let mut arrived = [false; 2];
        rig.wait(|seats| {
            for (side, seat) in seats.iter_mut().enumerate() {
                assert_eq!(seat.controller.view().phase, Phase::Idle, "peer traffic needs no call");
                if !seat.controller.take_peer_with_sender(MARKER).is_empty() { arrived[side] = true; }
            }
            arrived == [true; 2]
        });
        assert!(rig.seats[0].controller.take_peer_with_sender(TWEAK).into_iter().map(|(_, body)| body).collect::<Vec<_>>() == reverse, "reverse path preserves order and entire multi-frame bodies");
        assert!(rig.seats[0].controller.take_peer_with_sender(UNKNOWN).is_empty(),
            "a topic this build does not take is dropped on arrival, whole or in pieces, never buffered for the link's life");
        rig.press(0, Press::Call);
        rig.wait(|seats| seats[1].controller.view().ringing());
        assert_eq!(rig.seats[1].controller.take_peer_with_sender(TWEAK).into_iter().map(|(_, body)| body).collect::<Vec<_>>(), burst, "Ring arrives before anyone drains the 1000 messages");
        assert!(rig.seats[1].controller.take_peer_with_sender(TWEAK).is_empty(), "a drain delivers each message once");

        rig.press(0, Press::Cancel);
        rig.wait(|seats| seats[1].controller.view().phase == Phase::Idle);
        let seat = &mut rig.seats[0];
        seat.controller.send_peer_to(peers[0], MARKER, vec![]).unwrap();
        seat.controller.send_peer_to(peers[0], TWEAK, vec![7; makepad_town_core::connect::MAX_CALL]).unwrap();
        seat.controller.send_peer_to(peers[0], MARKER, vec![]).unwrap();
        let peer = seat.controller.linked_peers().next().unwrap();
        seat.controller.link_down(&mut seat.host, peer);
        assert_eq!(seat.controller.take_peer_events(), [
            PeerEvent::PeerUndelivered { peer, topic: TWEAK, count: 1 },
            PeerEvent::PeerUndelivered { peer, topic: MARKER, count: 2 },
        ], "report only messages still in the old link's queue, grouped by topic");
        assert!(seat.controller.take_peer_events().is_empty());
        assert_eq!(seat.controller.send_peer_to(peers[0], TWEAK, vec![]), Err(NotLinked));
    }
    struct Recording {
        samples: usize,
        turns: Arc<AtomicUsize>,
        next: u64,
        submit: std::sync::mpsc::Sender<(u64, usize)>,
        events: Receiver<makepad_ai_hub::speech::SttEvent>,
    }

    impl Recognizer for Recording {
        fn feed(&mut self, samples: Vec<f32>) { self.samples += samples.len(); }
        fn end_utterance(&mut self) -> u64 {
            self.next += 1;
            self.submit.send((self.next, std::mem::take(&mut self.samples))).unwrap();
            self.turns.fetch_add(1, Ordering::Release);
            self.next
        }
        fn poll(&mut self) -> Vec<makepad_ai_hub::speech::SttEvent> { self.events.try_iter().collect() }
    }

    #[test]
    fn transcription_pressed_before_attachment_captures_the_first_blocks_and_follows_a_restart() {
        use makepad_ai_hub::speech::{SttEvent, Transcript};
        use std::sync::mpsc::channel;
        for (me, peer) in [(1, 2), (2, 1)] {
            let mut ctl = CallController::default();
            let mut host = Host::default();
            ctl.configure(CallEnv { files: Default::default(), voice: VoiceSource::Feed, devices_path: Default::default(),
                signal: Arc::new(|| {}), spawn: makepad_call::driver::std_spawn(), jobs: makepad_call::driver::std_spawn() });
            let port = ctl.link_up(&mut host, [me; 65], [peer; 65], "Peer").unwrap();
            ctl.set_open_town(Some(([9; 32], vec![([peer; 65], "Peer".into())])));
            let (submit, submitted) = channel();
            let (engines, started) = channel();
            ctl.speech.recognize = Box::new(move |_, _| {
                let (tx, events) = channel();
                engines.send(tx).unwrap();
                Box::new(Recording { samples: 0, turns: Default::default(), next: 0, submit: submit.clone(), events })
            });
            let (sender, _sink) = crate::transcript::channel();
            ctl.set_transcript_sender(sender);
            let session = [9; 16];
            port.messages_in.try_send(super::super::Message::Ring { call_id: session, secret: [3; 32], video: false, ext: super::super::RingExt { town: Some([9; 32]), ..Default::default() } }.encode()).unwrap();
            ctl.poll(&mut host);
            ctl.press(&mut host, Press::Answer);
            ctl.set_transcript_town(session, Some("Workshop".into()));
            assert_eq!(ctl.view().phase, Phase::Live);
            assert!(ctl.call_opening_presses(&mut host, CallAction::Press(Press::Transcribe)));
            assert!(!ctl.voice_attached && started.try_recv().is_err());
            port.messages_in.try_send(MediaKey::generate().unwrap().encode()).unwrap();
            if me > peer {
                port.messages_in.try_send(Vec::try_from(MediaSignal::Media { n: 1, on: true }).unwrap()).unwrap();
            }
            let frame: Vec<_> = (0..320).map(|i| (i as f32 * 0.17).sin() * 0.3).collect();
            for n in 1..=2 {
                ctl.poll(&mut host);
                assert!(ctl.voice_attached && ctl.view().transcribing_me);
                let engine = started.try_recv().expect("each attached microphone has its own recognizer");
                for _ in 0..n { host.mic.as_mut().unwrap().push_mono(16_000., &frame); }
                if n == 1 {
                    let restart = if me < peer { MediaSignal::Want { on: true, last: 1, running: false } }
                        else { MediaSignal::Media { n: 2, on: true } };
                    port.messages_in.try_send(Vec::try_from(restart).unwrap()).unwrap();
                    ctl.poll(&mut host);
                } else { ctl.press(&mut host, Press::StopTranscribing); }
                let (utterance, count) = submitted.recv().unwrap();
                assert_eq!(count, n * frame.len(), "every block from the first attached callback reaches this recognizer");
                engine.send(SttEvent::Final { utterance, secs: 0., transcript: Transcript::from_text("captured") }).unwrap();
            }
            ctl.link_down(&mut host, [peer; 65]);
            while ctl.has_call_workers() || !ctl.speech.stretches.is_empty() { ctl.poll(&mut host); thread::yield_now(); }
        }
    }

    fn check_linked_transcription(rig: &mut Rig, a: &mut crate::model::Model, b: &mut crate::model::Model) {
        use crate::{model::{Action, Origin}, transcript};
        use makepad_ai_hub::{pipe::PipeId, speech::{SttCapabilities, SttEngineInfo, SttEvent, Transcript}};
        use makepad_town_core::event::SessionMode;
        use std::sync::{Condvar, mpsc::channel};
        let starts: [Arc<AtomicUsize>; 2] = Default::default();
        let turns: [Arc<AtomicUsize>; 2] = Default::default();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let why = "No on-device speech recognizer here. Install a speech model in AI Hub.";
        let mut sinks = [transcript::channel(), transcript::channel()];
        for (i, seat) in rig.seats.iter_mut().enumerate() {
            seat.controller.take_facts();
            seat.controller.set_transcript_sender(sinks[i].0.clone());
            let (starts, turns, gate) = (starts[i].clone(), turns[i].clone(), gate.clone());
            seat.controller.speech.recognize = Box::new(move |_, wake| {
                let start = starts.fetch_add(1, Ordering::Relaxed);
                let (submit, jobs) = channel();
                let (tx, events) = channel();
                let gate = gate.clone();
                thread::spawn(move || {
                    if i == 1 && start == 1 { tx.send(SttEvent::Failed(why.into())).unwrap(); wake(); return }
                    tx.send(SttEvent::Ready(SttEngineInfo {
                        engine: format!("local engine {i}"), pipe: PipeId::new("stt.fake"), remote: None,
                        capabilities: SttCapabilities { pcm_input: true, engine_mic: false, offline: true, partial_results: false },
                    })).unwrap();
                    wake();
                    for (id, samples) in jobs {
                        drop(gate.1.wait_while(gate.0.lock().unwrap(), |released| !*released).unwrap());
                        tx.send(SttEvent::Final { utterance: id, secs: 0.0,
                            transcript: Transcript::from_text(format!("seat {i} turn {id}: {samples}")) }).unwrap();
                        wake();
                    }
                });
                Box::new(Recording { samples: 0, turns: turns.clone(), next: 0, submit, events })
            });
        }

        let start_call = |rig: &mut Rig| {
            rig.press(0, Press::Call);
            rig.wait(|s| s[1].controller.view().ringing());
            rig.press(1, Press::Answer);
            rig.wait(|s| s[0].controller.view().phase == Phase::Live);
            let session = rig.seats[0].controller.flow.session().unwrap();
            for seat in &mut rig.seats { seat.controller.set_transcript_town(session, Some("Workshop".into())); }
            session
        };
        let session = start_call(rig);
        let Some(Fact::Open { session: opened, town, .. }) = rig.seats[0].controller.take_facts().pop() else { panic!("A opens the session") };
        assert_eq!(opened, session);
        assert_eq!(rig.seats[1].controller.take_facts(), [Fact::Join { session, from: a.me(), town: a.open_id() }]);
        a.act(Action::SessionOpen { town_wide: false, town: Some(town), session, mode: SessionMode::Call { video: false }, to: vec![a.seat_in_town(town, &b.me()).unwrap()] }, Origin::Ui).unwrap();
        b.act(Action::SessionJoin { peer: Some(a.me()), town: Some(town), session }, Origin::Ui).unwrap();
        let local = |rig: &mut Rig, i| {
            let seat: &mut Seat = &mut rig.seats[i];
            let press = CallAction::Press(Press::Transcribe);
            assert!(!seat.controller.call_actions(&mut seat.host, press), "queued actions cannot consent for seat {i}");
            assert!(seat.controller.call_opening_presses(&mut seat.host, press), "local consent starts seat {i}");
        };
        local(rig, 0);
        rig.wait(|s| s[1].controller.view().transcribe_prompt && matches!(s[0].controller.shown().transcribe, TranscribeShown::On(_)));
        rig.press(1, Press::NotMe);

        let frame: Vec<_> = (0..320).map(|n| (n as f32 * 0.17).sin() * 0.3).collect();
        let speak = |seat: &mut Seat, count| {
            let mic = seat.host.mic.as_mut().unwrap();
            for _ in 0..count { mic.push_mono(16_000., &frame); }
        };
        // Hold all recognition results beyond the tap's 2.56 s. Mute supplies silent
        // frames even if the device delivers a final loud block after it was closed.
        for turn in 1..=3 {
            speak(&mut rig.seats[0], 50);
            speak(&mut rig.seats[1], 2);
            if turn < 3 {
                rig.press(0, Press::Mute);
                speak(&mut rig.seats[0], 35);
                rig.wait(|_| turns[0].load(Ordering::Acquire) == turn);
                rig.press(0, Press::Unmute);
            }
        }
        // Make TL1's input full before releasing the finals, with material for an unrelated session.
        let unrelated = [0; 16];
        let mut filled = 0;
        while sinks[0].0.try_send(Heard { session: unrelated, at_ms: 0, dur_ms: 0, text: "queued".into() }).is_ok() { filled += 1; }
        rig.press(0, Press::StopTranscribing);
        rig.wait(|s| turns[0].load(Ordering::Acquire) == 3 && !s[1].controller.view().peer_transcribing);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        rig.wait(|s| s[0].controller.speech.stretches[0].lines.len() == 3);
        assert!(rig.seats[0].controller.speech.tap.is_none(), "Off still owns the three undelivered finals");
        let a_seat = &mut rig.seats[0];
        assert!(!a_seat.controller.call_opening_presses(&mut a_seat.host, CallAction::Press(Press::Transcribe)),
            "a new stretch cannot announce capture before the microphone tap returns");
        assert_eq!(sinks[0].1.flush(unrelated).unwrap().len(), filled);
        rig.wait(|s| s[0].controller.speech.tap.is_some());
        assert_eq!(rig.seats[0].controller.speech.tap.as_ref().unwrap().dropped(), 0,
            "the slow engine must lose no microphone frames while its three finals are held");
        assert_eq!(starts[1].load(Ordering::Relaxed), 0, "Not me keeps B's recognizer unopened for the entire call");
        let lines = sinks[0].1.flush(session).unwrap();
        assert_eq!(lines.len(), 3, "all three held results reached the same sink exactly once");
        for (n, line) in lines.iter().enumerate() {
            assert_eq!(line.text, format!("seat 0 turn {}: 16000", n + 1), "every unmuted sample belongs to A's turn");
            if n > 0 { assert!(line.at_ms > lines[n - 1].at_ms, "capture times must increase on the ring clock"); }
        }
        a.act(Action::Said { session, lines }, Origin::Ui).unwrap();
        *gate.0.lock().unwrap() = false;
        local(rig, 0);
        rig.wait(|s| s[1].controller.view().peer_transcribing);
        assert!(!rig.seats[1].controller.view().transcribe_prompt, "A's restart cannot prompt B a second time");
        speak(&mut rig.seats[0], 5);
        rig.press(0, Press::HangUp);
        rig.wait(|s| turns[0].load(Ordering::Acquire) == 4 && s[1].controller.view().phase == Phase::Idle);
        assert!(rig.seats[0].controller.take_facts().is_empty(), "Leave waits for the final words even after media ends");
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        rig.wait(|s| !s[0].controller.facts.is_empty());
        let last = sinks[0].1.flush(session).expect("TL1's pre-Leave flush sees the last utterance");
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].text, "seat 0 turn 1: 1600");
        a.act(Action::Said { session, lines: last }, Origin::Ui).unwrap();
        for (i, model) in [ &mut *a, &mut *b ].into_iter().enumerate() {
            let facts = rig.seats[i].controller.take_facts();
            let [Fact::Leave { session: ended, secs, why }] = facts[..] else { panic!("seat {i} needs one leave, got {facts:?}") };
            assert_eq!(ended, session);
            model.act(Action::SessionLeave { session, secs, why }, Origin::Ui).unwrap();
        }
        crate::model::tests::exchange(a, b);
        for m in [&*a, &*b] {
            assert_eq!(m.state.sessions.len(), 1, "both logs fold the same call");
            let lines = m.state.said(&session);
            assert_eq!(lines.len(), 4);
            assert!(lines.iter().all(|l| l.author == a.me()), "only A signed the four utterances");
        }
        eprintln!("linked transcription: 48000 speech samples before release, 0 dropped frames, {filled} sink slots full; fourth line flushed before Leave");

        start_call(rig);
        local(rig, 0);
        rig.wait(|s| s[1].controller.view().transcribe_prompt);
        local(rig, 1);
        rig.wait(|s| s[0].controller.view().peer_transcribing && matches!(s[1].controller.speech.status, TranscribeShown::On(_)));
        speak(&mut rig.seats[1], 10);
        rig.press(1, Press::StopTranscribing);
        rig.wait(|s| s[1].controller.speech.tap.is_some() && !s[0].controller.view().peer_transcribing);
        assert_eq!(rig.seats[1].controller.speech.tap.as_ref().unwrap().dropped(), 0);
        let id = rig.seats[1].controller.flow.session().unwrap();
        assert_eq!(sinks[1].1.flush(id).unwrap()[0].text, "seat 1 turn 1: 3200", "Off finishes B's own words");
        local(rig, 1);
        rig.wait(|s| s[1].controller.speech.status == TranscribeShown::Unavailable(why.into()) && !s[0].controller.view().peer_transcribing);
        assert!(rig.seats[0].controller.view().transcribing_me, "B's unavailable engine leaves A running");
        rig.press(0, Press::HangUp);
        rig.wait(|s| s.iter().all(|seat| seat.controller.speech.stretches.is_empty() && !seat.controller.has_call_workers()));
    }


    #[test]
    fn callee_card_call_gives_bystander_no_ack_or_voice_link() {
        use super::super::{decode, Message};
        let town = [9; 32];
        let keys: [PublicKey; 3] = [[1; 65], [2; 65], [3; 65]];
        let seats = [[1; 32], [2; 32], [3; 32]];
        let names = ["Ana", "Ada", "Ben"];
        let mut controllers: [CallController; 3] = Default::default();
        let mut hosts: [Host; 3] = Default::default();
        let mut ports = BTreeMap::new();
        for (i, ctl) in controllers.iter_mut().enumerate() {
            ctl.codecs = Arc::new(makepad_call::driver::codec::TestCodecs);
            ctl.configure(CallEnv { files: Default::default(), devices_path: Default::default(), signal: Arc::new(|| {}),
                voice: VoiceSource::Synthetic { tone_hz: 440. + 220. * i as f32 },
                spawn: makepad_call::driver::std_spawn(), jobs: makepad_call::driver::std_spawn() });
            let mut linked = Vec::new();
            for j in (0..3).filter(|j| *j != i) {
                ports.insert((i, j), ctl.link_up(&mut hosts[i], keys[i], keys[j], names[j]).unwrap());
                ctl.set_peer_caps(keys[j], MediaCaps(MediaCaps::MULTIPARTY_V1));
                linked.push((keys[j], names[j].into()));
            }
            ctl.set_town_peer_seats(BTreeMap::from([(town, keys.into_iter().zip(seats).collect())]));
            ctl.set_open_town(Some((town, linked)));
        }
        let pump = |controllers: &mut [CallController; 3], hosts: &mut [Host; 3]| {
            for (&(i, j), out) in &ports {
                let dest = &ports[&(j, i)];
                for body in out.messages_out.try_iter() {
                    if j == 2 {
                        assert!(!matches!(decode(&body), Ok(Some(Message::CallJoinAck { .. } | Message::Ring { .. }))),
                            "Ben receives neither a ring nor a join acknowledgement");
                    }
                    dest.messages_in.try_send(body).expect("deliver the signalling frame");
                }
                for body in out.datagrams_out.try_iter() {
                    assert_ne!(j, 2, "neither participant sends voice to Ben");
                    let addr = std::net::SocketAddr::from(([192, 0, 2, i as u8 + 1], 4000));
                    dest.datagrams_in.try_send((addr, body)).expect("deliver authorized media");
                }
            }
            for (ctl, host) in controllers.iter_mut().zip(hosts) { ctl.poll(host); }
            thread::yield_now();
        };
        controllers[0].press(&mut hosts[0], Press::CallSeat(seats[1]));
        while !controllers[1].view().ringing() { pump(&mut controllers, &mut hosts); }
        let call_id = controllers[0].flow.session().unwrap();
        let join = Message::CallJoin { call_id, town, muted: false, transcribing: false };
        for phase in [Phase::Ringing, Phase::Live] {
            let stale = controllers[1].flow.ignored().stale;
            assert_eq!(controllers[1].view().phase, phase, "test both admission paths at Ada");
            ports[&(1, 2)].messages_in.try_send(join.encode()).expect("deliver the same-town join request");
            while controllers[1].flow.ignored().stale == stale { pump(&mut controllers, &mut hosts); }
            if phase == Phase::Ringing { controllers[1].press(&mut hosts[1], Press::Answer); }
        }
        let received = |ctl: &mut CallController, from| {
            let id = ctl.links[&from].peer_id;
            ctl.synthetic.as_ref()?.received_seq(id, VOICE)
        };
        while !(0..2).all(|i| received(&mut controllers[i], keys[1-i]).is_some()) { pump(&mut controllers, &mut hosts); }
        let first = [0, 1].map(|i| received(&mut controllers[i], keys[1-i]).unwrap());
        let peer_lit = |ctl: &CallController| {
            let shown = ctl.shown();
            shown.talking.peer && shown.peer_level_step() > 0
        };
        while !controllers[..2].iter().all(peer_lit)
            && (0..2).any(|i| received(&mut controllers[i], keys[1-i]).unwrap().wrapping_sub(first[i]) < 50)
        {
            pump(&mut controllers, &mut hosts);
        }
        for i in 0..2 {
            assert!(peer_lit(&controllers[i]),
                "{}'s synthetic speaker plays out the peer's voice, so the peer's talking mark and level rise", names[i]);
        }
        assert_eq!(controllers[1].voice_peers(), [keys[0]], "Ada exchanges voice only with Ana");
        assert!(controllers[1].links[&keys[2]].media_held.is_none() && !controllers[2].has_call_workers(),
            "the bystander's link stays free of media");
        controllers[0].press(&mut hosts[0], Press::HangUp);
        while controllers.iter().any(CallController::has_call_workers) { pump(&mut controllers, &mut hosts); }
    }

    #[test]
    fn foreign_town_carrier_gets_no_voice_in_live_call() {
        use super::super::{decode, Message};
        let (town_a, town_b) = ([0xaa; 32], [0xbb; 32]);
        let keys: [PublicKey; 3] = [[0; 65], [1; 65], [2; 65]];
        let names = ["Alice", "Ben", "Eve"];
        let mut controllers: [CallController; 3] = Default::default();
        let mut hosts: [Host; 3] = Default::default();
        let mut ports = BTreeMap::new();
        for (i, ctl) in controllers.iter_mut().enumerate() {
            ctl.codecs = Arc::new(makepad_call::driver::codec::TestCodecs);
            ctl.configure(CallEnv {
                files: Default::default(), devices_path: Default::default(), signal: Arc::new(|| {}),
                voice: VoiceSource::Synthetic { tone_hz: 440. + 220. * i as f32 },
                spawn: makepad_call::driver::std_spawn(), jobs: makepad_call::driver::std_spawn(),
            });
            let mut towns = BTreeMap::<_, BTreeSet<_>>::new();
            let neighbors: &[(usize, TownId)] = match i {
                0 => &[(1, town_a), (2, town_b)],
                1 => &[(0, town_a)],
                _ => &[(0, town_b)],
            };
            for &(j, town) in neighbors {
                let port = ctl.link_up(&mut hosts[i], keys[i], keys[j], names[j]).unwrap();
                ctl.set_peer_caps(keys[j], MediaCaps(MediaCaps::MULTIPARTY_V1));
                ports.insert((i, j), port);
                towns.entry(town).or_default().insert(keys[j]);
            }
            ctl.set_town_peers(&mut hosts[i], towns);
            let target = if i == 0 { 1 } else { 0 };
            ctl.set_open_town(Some((if i == 2 { town_b } else { town_a }, vec![(keys[target], names[target].into())])));
        }

        let until = |controllers: &mut [CallController; 3], hosts: &mut [Host; 3],
                         ready: &mut dyn FnMut(&mut [CallController; 3]) -> bool| {
            let start = Instant::now();
            loop {
                for (&(i, j), out) in &ports {
                    let dest = &ports[&(j, i)];
                    for frame in out.messages_out.try_iter() {
                        if j == 2 {
                            assert!(!matches!(decode(&frame), Ok(Some(Message::CallJoinAck { .. }))),
                                "the foreign carrier must never receive a join acknowledgement");
                        }
                        dest.messages_in.try_send(frame).expect("deliver the signaling frame");
                    }
                    for bytes in out.datagrams_out.try_iter() {
                        assert_ne!(j, 2, "no media datagram can be sent to the foreign carrier");
                        let addr = std::net::SocketAddr::from(([192, 0, 2, i as u8 + 1], 4000));
                        dest.datagrams_in.try_send((addr, bytes)).expect("deliver the voice datagram");
                    }
                }
                for (ctl, host) in controllers.iter_mut().zip(hosts.iter_mut()) { ctl.poll(host); }
                if ready(controllers) { break }
                assert!(start.elapsed() < Duration::from_secs(15), "the authorized voice path must make progress");
                thread::yield_now();
            }
        };
        let received = |ctl: &mut CallController, from| {
            let id = ctl.links[&from].peer_id;
            ctl.synthetic.as_ref()?.received_seq(id, VOICE)
        };
        controllers[0].press(&mut hosts[0], Press::Call);
        until(&mut controllers, &mut hosts, &mut |c| c[1].view().ringing());
        controllers[1].press(&mut hosts[1], Press::Answer);
        until(&mut controllers, &mut hosts, &mut |c| received(&mut c[0], keys[1]).is_some() && received(&mut c[1], keys[0]).is_some());
        let before = [0, 1].map(|i| received(&mut controllers[i], keys[1-i]).unwrap());
        let generations = [0, 1].map(|i| controllers[i].call_generation());
        let call_id = controllers[0].flow.session().unwrap();
        controllers[0].set_open_town(Some((town_b, vec![(keys[2], names[2].into())])));
        for msg in [Message::CallJoinAck { call_id, muted: false, transcribing: false, seats: None },
                    Message::CallJoin { call_id, town: town_a, muted: false, transcribing: false }] {
            ports[&(0, 2)].messages_in.try_send(msg.encode()).expect("deliver Eve's signaling to Alice");
        }
        until(&mut controllers, &mut hosts, &mut |c| (0..2).all(|i|
            received(&mut c[i], keys[1-i]).is_some_and(|seq| seq > before[i])));
        assert_eq!(controllers[0].voice_peers(), [keys[1]], "only Ben has voice in Alice's live town");
        assert!(controllers[0].links[&keys[2]].media_held.is_none(), "the foreign link has no media session");
        assert_eq!([0, 1].map(|i| controllers[i].call_generation()), generations,
            "both directions advanced on their original calls");
        controllers[0].press(&mut hosts[0], Press::HangUp);
        until(&mut controllers, &mut hosts, &mut |c| c.iter().all(|ctl| !ctl.has_call_workers()));
    }

    fn keeper_with_two_links(peers: [PublicKey; 2]) -> (CallController, Host, [MediaPort; 2]) {
        let mut ctl = CallController::default();
        ctl.codecs = Arc::new(makepad_call::driver::codec::TestCodecs);
        ctl.configure(CallEnv {
            files: Default::default(), devices_path: Default::default(), signal: Arc::new(|| {}),
            voice: VoiceSource::Off, spawn: makepad_call::driver::std_spawn(), jobs: makepad_call::driver::std_spawn(),
        });
        let mut host = Host::default();
        let ports = peers.map(|peer| {
            let port = ctl.link_up(&mut host, [1; 65], peer, "Peer").unwrap();
            port.messages_in.try_send(MediaKey::generate().unwrap().encode()).unwrap();
            port
        });
        ctl.poll(&mut host);
        (ctl, host, ports)
    }

    #[test]
    fn link_down_that_stops_the_call_ends_the_other_links_media() {
        use super::super::Message;
        let (b, c) = ([2; 65], [3; 65]);
        let (mut ctl, mut host, [sharer, follower]) = keeper_with_two_links([b, c]);
        let town = [9; 32];
        ctl.set_open_town(Some((town, vec![(b, "Ben".into()), (c, "Cam".into())])));
        ctl.set_peer_caps(b, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        sharer.messages_in.try_send(Message::Share {
            session: [7; 16], title: "Drawing".into(), w: 640., h: 480., ext: super::super::ShareOfferExt { town: Some(town), ..Default::default() },
        }.encode()).unwrap();
        ctl.poll(&mut host);
        assert!(ctl.call_opening_presses(&mut host, CallAction::OnShare([7; 16], Press::Watch)), "the viewer starts the sharer's media");
        let want = |last, running| Vec::try_from(MediaSignal::Want { on: true, last, running }).unwrap();
        let signals = || follower.messages_out.try_iter().filter_map(|body| MediaSignal::decode(&body)).collect::<Vec<_>>();
        follower.messages_in.try_send(want(0, false)).unwrap();
        ctl.poll(&mut host);
        assert_eq!(signals(), [MediaSignal::Media { n: 1, on: true }], "the keeper starts the follower's requested media");
        follower.messages_in.try_send(want(1, true)).unwrap();
        ctl.poll(&mut host);
        assert_eq!(ctl.links[&c].media_held, Some(1), "the follower is running session 1 when the sharer drops");

        ctl.link_down(&mut host, b);
        assert!(ctl.current.is_none(), "losing the only remote share retires the call");
        assert_eq!(signals(), [MediaSignal::Media { n: 1, on: false }], "the keeper tells the surviving follower that session 1 ended");
        ctl.shutdown(&mut host);
        while ctl.has_call_workers() { ctl.poll(&mut host); thread::yield_now(); }
    }

    #[test]
    fn retiring_the_call_clears_the_talking_light() {
        use super::super::{decode, AnswerExt, Message};
        let (b, c) = ([2; 65], [3; 65]);
        let (mut ctl, mut host, [answering, ringing]) = keeper_with_two_links([b, c]);
        let town = [9; 32];
        ctl.set_open_town(Some((town, vec![(b, "Ben".into()), (c, "Cam".into())])));
        ctl.press(&mut host, Press::Call);
        let session = ctl.flow.session().unwrap();
        assert!(ringing.messages_out.try_iter().any(|body|
            matches!(decode(&body), Ok(Some(Message::Ring { call_id, .. })) if call_id == session)),
            "the other peer was rung for this call");
        answering.messages_in.try_send(Message::Answer {
            call_id: session, secret: [0; 32], ext: AnswerExt { town: Some(town), ..Default::default() },
        }.encode()).unwrap();
        ctl.poll(&mut host);
        assert_eq!(ctl.voice_peers(), [b], "only the answering peer is live");
        assert!(ctl.current.is_some(), "the answering peer has started media");
        ctl.talking = Talking { you: false, peer: true };

        ctl.link_down(&mut host, b);
        assert!(ctl.current.is_none(), "dropping the only live peer retires media");
        let shown = ctl.shown();
        assert_eq!(shown.view.phase, Phase::Live, "the call stays live while the other peer is ringing");
        assert_eq!(shown.talking, Talking::default(), "retiring media clears the departed peer's talking light immediately");
        ctl.shutdown(&mut host);
        while ctl.has_call_workers() { ctl.poll(&mut host); thread::yield_now(); }
    }

    #[test]
    fn three_party_voice_mesh_stays_live_when_one_peer_disconnects() {
        struct Mesh {
            controllers: [CallController; 3],
            hosts: [Host; 3],
            ports: BTreeMap<(usize, usize), MediaPort>,
        }

        fn key(i: usize) -> PublicKey { [i as u8 + 1; 65] }
        impl Mesh {
            fn pump(&mut self) {
                for (&(i, j), out) in &self.ports {
                    let incoming = &self.ports[&(j, i)];
                    for body in out.messages_out.try_iter() {
                        incoming.messages_in.try_send(body).expect("the other seat receives the signalling frame");
                    }
                    for body in out.datagrams_out.try_iter() {
                        let from = std::net::SocketAddr::from(([192, 0, 2, i as u8 + 1], 4000));
                        incoming.datagrams_in.try_send((from, body)).expect("the destination inlet accepts the media datagram");
                    }
                }
                for (controller, host) in self.controllers.iter_mut().zip(&mut self.hosts) { controller.poll(host); }
            }

            fn until(&mut self, mut ready: impl FnMut(&mut Self) -> bool) {
                let start = Instant::now();
                loop {
                    self.pump();
                    if ready(self) { break }
                    assert!(start.elapsed() < Duration::from_secs(15), "the voice mesh must make progress");
                    thread::yield_now();
                }
            }

            fn received(&mut self, i: usize, from: usize) -> Option<u32> {
                let ctl = &mut self.controllers[i];
                let peer_id = ctl.links[&key(from)].peer_id;
                ctl.synthetic.as_ref()?.received_seq(peer_id, VOICE)
            }
        }

        let mut m = Mesh { controllers: Default::default(), hosts: Default::default(), ports: BTreeMap::new() };
        for i in 0..3 {
            let ctl = &mut m.controllers[i];
            ctl.codecs = Arc::new(makepad_call::driver::codec::TestCodecs);
            ctl.configure(CallEnv { files: Default::default(), devices_path: Default::default(), signal: Arc::new(|| {}),
                voice: VoiceSource::Synthetic { tone_hz: 440. + 220. * i as f32 },
                spawn: makepad_call::driver::std_spawn(), jobs: makepad_call::driver::std_spawn() });
            let mut peers = Vec::new();
            for j in (0..3).filter(|j| *j != i) {
                let name = format!("Peer {j}");
                let port = ctl.link_up(&mut m.hosts[i], key(i), key(j), &name).unwrap();
                m.ports.insert((i, j), port);
                ctl.set_peer_caps(key(j), MediaCaps(MediaCaps::MULTIPARTY_V1));
                peers.push((key(j), name));
            }
            ctl.set_open_town(Some(([9; 32], peers)));
        }
        m.controllers[0].press(&mut m.hosts[0], Press::Call);
        m.until(|m| m.controllers[1..].iter().all(|ctl| ctl.view().ringing()));
        for i in [1, 2] { m.controllers[i].press(&mut m.hosts[i], Press::Answer); }
        m.until(|m| (0..3).all(|i| (0..3).all(|j| i == j || m.received(i, j).is_some())));
        let generations = [0, 2].map(|i| m.controllers[i].call_generation());
        let removed = [0, 2].map(|i| m.controllers[i].links[&key(1)].peer_id);
        for (index, i) in [0, 2].into_iter().enumerate() {
            m.controllers[i].link_down(&mut m.hosts[i], key(1));
            m.controllers[1].link_down(&mut m.hosts[1], key(i));
            m.ports.remove(&(i, 1));
            m.ports.remove(&(1, i));
            assert!(!m.controllers[i].id_to_peer.contains_key(&removed[index]), "seat {i} discarded the departed media ID");
            assert_eq!(m.controllers[i].voice_peers(), [key(2-i)], "seat {i} keeps its surviving partner");
        }
        let before = [m.received(0, 2).unwrap(), m.received(2, 0).unwrap()];
        m.until(|m| m.received(0, 2).is_some_and(|s| s > before[0]) && m.received(2, 0).is_some_and(|s| s > before[1]));
        assert_eq!([0, 2].map(|i| m.controllers[i].call_generation()), generations, "surviving voice advances on the original workers");
        m.controllers[0].press(&mut m.hosts[0], Press::HangUp);
        m.until(|m| m.controllers.iter().all(|ctl| !ctl.has_call_workers()));
    }
}
