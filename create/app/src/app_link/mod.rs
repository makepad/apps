//! Local app discovery, window pictures, consented input and recording. Only the coordinator
//! is UI-owned. Its methods enqueue work and poll bounded result channels.
mod capture;
mod commands;
mod endpoint;
mod pictures;
mod recording;
pub mod report;
mod sound;
#[cfg(test)]
mod tests;
#[cfg(all(test, unix))]
pub(crate) use tests::RecordingRig;
#[path = "../app_link_window.rs"]
mod window;
mod worker;

pub use commands::{AppInput, DesignOp, DesignVerb, Mods};
use makepad_call::driver::{codec::Codecs, Spawn};
pub use pictures::{PicturePreview, PictureProbe, PictureStats, PictureStream, PictureWant};
pub use sound::{AppSound, EncodedSound, SoundStats};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
    time::Instant,
};

#[derive(Clone)]
pub struct AppLinkEnv {
    pub spawn: Spawn,
    pub signal: Arc<dyn Fn() + Send + Sync>,
    pub codecs: Arc<dyn Codecs>,
    pub registry: Option<PathBuf>,
}
#[derive(Clone, Debug)]
pub struct AppKey {
    pid: u32,
    started_ms: u64,
    socket: PathBuf,
    run_dir: Option<String>,
    restart_parent: Option<u32>,
}
impl AppKey {
    pub(crate) fn pid(&self) -> u32 { self.pid }
    pub(crate) fn run_dir(&self) -> Option<&str> { self.run_dir.as_deref() }

    /// The registry's process identity. Restart hints only relate two different instances.
    fn instance(&self) -> (u32, u64, &Path) {
        (self.pid, self.started_ms, &self.socket)
    }
}
impl PartialEq for AppKey {
    fn eq(&self, other: &Self) -> bool {
        self.instance() == other.instance()
    }
}
impl Eq for AppKey {}
impl std::hash::Hash for AppKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.instance().hash(state);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LinkId(u64);
#[cfg(test)]
impl LinkId {
    pub(crate) const TEST: Self = Self(1);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(u64);
#[derive(Clone, Debug)]
pub struct LocalApp {
    pub title: Option<String>,
    pub visible_windows: Vec<(WindowId, String)>,
    pub key: AppKey,
    pub name: String,
    pub pid: u32,
    pub started_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkPhase {
    Connecting,
    Live,
    Ended(String),
}
#[derive(Clone, Debug, PartialEq)]
pub struct AppWindow {
    pub title: String,
    pub visible: bool,
    pub id: WindowId,
    pub size: (f64, f64),
    pub dpi: f64,
}
#[derive(Clone, Debug)]
pub struct LinkedApp {
    pub id: LinkId,
    pub app: LocalApp,
    pub phase: LinkPhase,
    pub windows: Vec<AppWindow>,
    pub input_allowed: bool,
    pub recording: Option<Ticket>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StillSize { Poster, Full }

#[derive(Clone, Debug, PartialEq)]
pub struct PngStill {
    pub png: Arc<[u8]>,
    pub size: [u32; 2],
}

pub(crate) struct IssueStill {
    pub png: Arc<Vec<u8>>,
    pub poster: Arc<Vec<u8>>,
    pub size: [u32; 2],
}

type PngReply = Result<PngStill, String>;
type StillReply = Result<(PathBuf, u32, u32), String>;

#[derive(Debug)]
pub struct Clip {
    pub path: PathBuf,
    pub frames: u64,
    pub secs: f64,
    pub w: u32,
    pub h: u32,
}
#[derive(Debug)]
pub enum AppLinkEvent {
    Apps,
    Linked(LinkId),
    Windows(LinkId),
    Ended {
        id: LinkId,
        why: String,
    },
    Restarted {
        ended: LinkId,
        app: LocalApp,
    },
    LocalInput(LinkId),
    Design {
        id: LinkId,
        ticket: Ticket,
        ok: bool,
        json: String,
    },
    Recorded {
        id: LinkId,
        ticket: Ticket,
        result: Result<Clip, String>,
    },
    PngStill { id: LinkId, ticket: Ticket, result: PngReply },
    Still {
        size: (u32, u32),
        id: LinkId,
        ticket: Ticket,
        result: Result<PathBuf, String>,
    },
}

struct Queue<T> {
    tx: SyncSender<T>,
    pending: VecDeque<T>,
    full: bool,
    published: bool,
}
impl<T> Queue<T> {
    fn new(tx: SyncSender<T>) -> Self {
        Self {
            tx,
            pending: VecDeque::new(),
            full: false,
            published: false,
        }
    }
    fn push(&mut self, value: T) {
        self.pending.push_back(value);
        self.flush();
    }
    fn flush(&mut self) {
        while let Some(item) = self.pending.pop_front() {
            match self.tx.try_send(item) {
                Ok(()) => {
                    self.published = true;
                }
                Err(TrySendError::Full(item)) => {
                    if !self.full {
                        makepad_widgets::log!("App link: channel full; queued for retry");
                        self.full = true;
                    }
                    self.pending.push_front(item);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.pending.clear();
                    break;
                }
            }
        }
        if self.pending.is_empty() {
            self.full = false;
        }
    }
}

#[derive(Default)]
struct Gate {
    // Odd epochs grant control. Every revoke invalidates commands already in the worker's queue.
    epoch: AtomicU64,
    closing: AtomicBool,
    // One atomic orders handing a Down to the app against sealing input at quit.
    presses: AtomicU8,
    refused: AtomicU64,
    delivered: AtomicU64,
    refusal_run: AtomicBool,
}
impl Gate {
    const SEALED: u8 = 1;
    const OWES_RELEASE: u8 = 2;

    fn hand_over_press(&self) -> bool {
        self.presses.try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
            (state & Self::SEALED == 0).then_some(state | Self::OWES_RELEASE)
        }).is_ok()
    }

    fn owes_release(&self) -> bool {
        self.presses.load(Ordering::Acquire) & Self::OWES_RELEASE != 0
    }

    fn releases_finished(&self) {
        self.presses.fetch_and(!Self::OWES_RELEASE, Ordering::AcqRel);
    }

    fn seal_input(&self) {
        self.presses.fetch_or(Self::SEALED, Ordering::AcqRel);
        self.revoke();
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    fn refuse(&self, why: &str) {
        self.refused.fetch_add(1, Ordering::Relaxed);
        if !self.refusal_run.swap(true, Ordering::Relaxed) {
            makepad_widgets::log!("App link: input refused: {why}");
        }
    }
    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        self.seal_input();
    }
    fn revoke(&self) {
        self.epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| Some((n | 1) + 1))
            .unwrap();
    }
}

enum StillTarget {
    File { path: PathBuf, size: StillSize, reply: SyncSender<StillReply> },
    Png(SyncSender<PngReply>),
    Issue(SyncSender<Result<IssueStill, String>>),
    Thumbnail(SyncSender<ThumbnailReply>),
}

enum Command {
    Wake,
    Allow(u64, Instant),
    Input(u64, WindowId, AppInput),
    Picture(pictures::Subscription),
    Sound(sound::SoundSubscription),
    Design(Ticket, Option<WindowId>, DesignOp),
    Start(Ticket, WindowId, PathBuf),
    Stop(Ticket),
    Still(WindowId, StillTarget),
}
enum Update {
    Windows(Vec<AppWindow>),
    LocalInput(u64),
    Recording(Option<Ticket>),
    Ended { why: String, app_gone: bool },
    Event(AppLinkEvent),
}
struct Link {
    state: LinkedApp,
    restart: Option<endpoint::RestartWatch>,
    gate: Arc<Gate>,
    commands: Queue<Command>,
    updates: Receiver<Update>,
}

pub(crate) type ThumbnailReply = Result<Arc<makepad_widgets::ImageBuffer>, String>;

/// Registry windows need a verified connection for their stills, even before the person
/// selects a window to link. These connections belong only to the open picker.
#[derive(Default)]
pub(crate) struct ThumbnailLinks(HashMap<AppKey, Queue<Command>>);

impl ThumbnailLinks {
    pub(crate) fn flush(&mut self) {
        for commands in self.0.values_mut() { commands.flush(); }
    }
}

type ScanResult = (Vec<LocalApp>, Vec<(String, String)>);

struct Scan {
    request: SyncSender<()>,
    requested: Arc<AtomicU64>,
    result: Receiver<(u64, ScanResult)>,
}

fn run_scan_worker(
    rx: Receiver<()>,
    requested: Arc<AtomicU64>,
    tx: SyncSender<(u64, ScanResult)>,
    wake: Arc<dyn Fn() + Send + Sync>,
    mut discover: impl FnMut() -> ScanResult,
) {
    while rx.recv().is_ok() {
        // Snapshot before reading the registry. A later request queues or shares the next token.
        let generation = requested.load(Ordering::Acquire);
        let found = discover();
        if tx.send((generation, found)).is_err() {
            break;
        }
        wake();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InputQuit {
    Ready,
    Wait,
    Force { links_owing_release: usize },
}

pub struct AppLinks {
    env: Option<AppLinkEnv>,
    apps: Vec<LocalApp>,
    /// Running apps the last scan found but cannot link, with the reason.
    unreachable: Vec<(String, String)>,
    scan_requested: u64,
    scan_completed: u64,
    links: HashMap<LinkId, Link>,
    active: Vec<LinkId>,
    scan: Option<Scan>,
    next: u64,
    events: Vec<AppLinkEvent>,
    stills: Vec<(LinkId, Ticket, Receiver<StillReply>)>,
    png_stills: Vec<(LinkId, Ticket, Receiver<PngReply>)>,
    own: u32,
    quitting_input: bool,
}
impl Default for AppLinks {
    fn default() -> Self {
        Self {
            env: None,
            apps: vec![],
            unreachable: vec![],
            scan_requested: 0,
            scan_completed: 0,
            links: HashMap::new(),
            active: Vec::new(),
            scan: None,
            next: 0,
            events: vec![],
            stills: vec![],
            png_stills: vec![],
            own: std::process::id(),
            quitting_input: false,
        }
    }
}
impl AppLinks {
    pub fn configured(&self) -> bool {
        self.env.is_some()
    }
    pub fn configure(&mut self, env: AppLinkEnv) {
        if self.env.is_none() {
            self.env = Some(env);
        }
    }
    fn serial(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    pub fn scan(&mut self) {
        let Some(env) = &self.env else { return };
        if self.scan.is_none() {
            let (request, rx) = mpsc::sync_channel(1);
            let (tx, result) = mpsc::sync_channel(1);
            let requested = Arc::new(AtomicU64::new(0));
            let worker_requested = requested.clone();
            let (reg, own, wake) = (env.registry.clone(), self.own, env.signal.clone());
            (env.spawn)(
                "app-discovery",
                Box::new(move || {
                    let registry = reg.unwrap_or_else(makepad_remote::discovery::registry_dir);
                    run_scan_worker(rx, worker_requested, tx, wake, || {
                        endpoint::discover_all(&registry, own)
                    });
                }),
            );
            self.scan = Some(Scan { request, requested, result });
        }
        self.scan_requested += 1;
        let scan = self.scan.as_ref().unwrap();
        scan.requested.store(self.scan_requested, Ordering::Release);
        // A pending scan reads the entire registry; concurrent refresh requests coalesce.
        let _ = scan.request.try_send(());
    }
    pub fn apps(&self) -> &[LocalApp] {
        &self.apps
    }
    /// Running apps the last scan found but cannot link: (name, why).
    pub fn unreachable(&self) -> &[(String, String)] {
        &self.unreachable
    }
    /// A scan was asked for and has not answered yet.
    pub fn scanning(&self) -> bool {
        self.scan.is_some() && self.scan_completed < self.scan_requested
    }
    pub fn link(&mut self, app: &AppKey) -> LinkId {
        let id = LinkId(self.serial());
        let entry = self.apps.iter().find(|a| a.key == *app).cloned();
        let (Some(env), Some(entry)) = (&self.env, entry) else {
            self.events.push(AppLinkEvent::Ended {
                id,
                why: "app is not in the local list".into(),
            });
            return id;
        };
        let (gate, commands, updates) = Self::start_worker(env, self.own, id, app);
        let state = LinkedApp {
            id,
            app: entry,
            phase: LinkPhase::Connecting,
            windows: Vec::new(),
            input_allowed: false,
            recording: None,
        };
        let mut restart = endpoint::RestartWatch::default();
        restart.replacement(&state.app, false, &self.apps);
        self.active.push(id);
        self.links.insert(
            id,
            Link {
                state,
                restart: Some(restart),
                gate,
                commands,
                updates,
            },
        );
        id
    }
    fn start_worker(env: &AppLinkEnv, own: u32, id: LinkId, key: &AppKey)
        -> (Arc<Gate>, Queue<Command>, Receiver<Update>) {
        let (cmd, rx) = mpsc::sync_channel(16);
        let (tx, updates) = mpsc::sync_channel(16);
        let gate = Arc::new(Gate::default());
        let args = (env.clone(), own, id, key.clone(), gate.clone());
        (env.spawn)("app-link", Box::new(move || worker::run(args, rx, tx)));
        (gate, Queue::new(cmd), updates)
    }

    pub(crate) fn thumbnail(&mut self, private: &mut ThumbnailLinks, link: Option<LinkId>, key: &AppKey, window: WindowId)
        -> Option<Receiver<ThumbnailReply>> {
        let commands = if let Some(id) = link {
            &mut self.links.get_mut(&id)?.commands
        } else {
            use std::collections::hash_map::Entry;
            match private.0.entry(key.clone()) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    // The picker only needs the still replies. Dropping commands closes this worker.
                    let id = LinkId(self.serial());
                    let (_, commands, _) = Self::start_worker(self.env.as_ref()?, self.own, id, key);
                    e.insert(commands)
                }
            }
        };
        let (tx, rx) = mpsc::sync_channel(1);
        commands.push(Command::Still(window, StillTarget::Thumbnail(tx)));
        Some(rx)
    }

    pub fn unlink(&mut self, id: LinkId) {
        if let Some(l) = self.links.get_mut(&id) {
            l.restart = None;
            l.gate.close();
            l.state.input_allowed = false;
            l.commands.push(Command::Wake);
        }
    }
    pub fn state(&self, id: LinkId) -> Option<&LinkedApp> {
        self.links.get(&id).map(|l| &l.state)
    }
    pub fn linked(&self) -> impl Iterator<Item = &LinkedApp> {
        self.links
            .values()
            .filter(|l| !matches!(l.state.phase, LinkPhase::Ended(_)))
            .map(|l| &l.state)
    }
    fn live(&mut self, id: LinkId) -> Option<&mut Link> {
        self.links
            .get_mut(&id)
            .filter(|l| l.state.phase == LinkPhase::Live && !l.gate.closing.load(Ordering::Acquire))
    }
    pub fn open_picture(&mut self, id: LinkId, want: PictureWant) -> Option<PictureStream> {
        let env = self.env.as_ref()?.clone();
        let link = self.live(id)?;
        if !link.state.windows.iter().any(|w| w.id == want.window) {
            return None;
        }
        let (stream, slot) = PictureStream::new(&env, want);
        link.commands.push(Command::Picture(slot));
        Some(stream)
    }
    /// Subscribes to program audio from the whole app.
    pub fn open_sound(&mut self, id: LinkId) -> Option<AppSound> {
        let env = self.env.as_ref()?.clone();
        let link = self.live(id)?;
        let (sound, slot) = AppSound::new(&env);
        link.commands.push(Command::Sound(slot));
        Some(sound)
    }
    /// The sharer's own view of a shared window; see `PicturePreview`.
    pub fn open_preview(&mut self, id: LinkId, want: PictureWant) -> Option<PicturePreview> {
        let env = self.env.as_ref()?.clone();
        let link = self.live(id)?;
        if !link.state.windows.iter().any(|w| w.id == want.window) {
            return None;
        }
        let (preview, slot) = PicturePreview::new(&env, want);
        link.commands.push(Command::Picture(slot));
        Some(preview)
    }
    pub fn allow_input(&mut self, id: LinkId) {
        if self.quitting_input { return }
        if let Some(l) = self.live(id) {
            if l.gate.epoch() % 2 == 1 {
                return;
            }
            let epoch = l.gate.epoch.fetch_add(1, Ordering::AcqRel) + 1;
            l.state.input_allowed = true;
            l.commands.push(Command::Allow(epoch, Instant::now()));
        }
    }
    pub fn revoke_input(&mut self, id: LinkId) {
        if let Some(l) = self.links.get_mut(&id) {
            l.gate.revoke();
            l.state.input_allowed = false;
            l.commands.push(Command::Wake);
        }
    }
    pub fn inject(&mut self, id: LinkId, window: WindowId, input: AppInput) {
        let Some(l) = self.links.get_mut(&id) else {
            return;
        };
        let epoch = l.gate.epoch();
        if epoch % 2 == 0 || l.gate.closing.load(Ordering::Acquire) {
            l.gate.refuse("no current Allow");
            return;
        }
        l.commands.push(Command::Input(epoch, window, input));
    }
    /// Includes refusals before a request and refusals answered by the app.
    pub fn inputs_refused(&self, id: LinkId) -> u64 {
        self.links
            .get(&id)
            .map_or(0, |l| l.gate.refused.load(Ordering::Relaxed))
    }
    pub fn inputs_delivered(&self, id: LinkId) -> u64 {
        self.links
            .get(&id)
            .map_or(0, |l| l.gate.delivered.load(Ordering::Relaxed))
    }
    pub fn design(&mut self, id: LinkId, window: Option<WindowId>, op: DesignOp) -> Ticket {
        let ticket = Ticket(self.serial());
        if let Err(json) = op.check_window(window) {
            self.events.push(AppLinkEvent::Design { id, ticket, ok: false, json });
            return ticket;
        }
        if let Some(l) = self.live(id) {
            l.commands.push(Command::Design(ticket, window, op));
        } else {
            self.events.push(AppLinkEvent::Design {
                id,
                ticket,
                ok: false,
                json: error_json("app is not linked"),
            });
        }
        ticket
    }
    pub fn record_start(&mut self, id: LinkId, window: WindowId, dir: &Path) -> Ticket {
        let t = Ticket(self.serial());
        if let Some(l) = self.live(id) {
            l.commands.push(Command::Start(t, window, dir.to_owned()));
        } else {
            self.events.push(AppLinkEvent::Recorded {
                id,
                ticket: t,
                result: Err("app is not linked".into()),
            });
        }
        t
    }
    /// Grab a full-size JPEG or a smaller recording poster from this window. Its reply is independent of cap stop.
    pub fn still(&mut self, id: LinkId, window: WindowId, path: &Path, size: StillSize) -> Ticket {
        let ticket = Ticket(self.serial());
        if let Some(link) = self.live(id) {
            let (tx, rx) = mpsc::sync_channel(1);
            link.commands.push(Command::Still(window, StillTarget::File { path: path.to_owned(), size, reply: tx }));
            self.stills.push((id, ticket, rx));
        } else {
            self.events.push(AppLinkEvent::Still { id, ticket, size: (0, 0), result: Err("app is not linked".into()) });
        }
        ticket
    }
    /// Fetch the window at scale one and fit its PNG to the AI tool result budget.
    pub fn png_still(&mut self, id: LinkId, window: WindowId) -> Ticket {
        let ticket = Ticket(self.serial());
        if let Some(link) = self.live(id) {
            let (tx, rx) = mpsc::sync_channel(1);
            link.commands.push(Command::Still(window, StillTarget::Png(tx)));
            self.png_stills.push((id, ticket, rx));
        } else {
            self.events.push(AppLinkEvent::PngStill { id, ticket, result: Err("app is not linked".into()) });
        }
        ticket
    }

    pub(crate) fn issue_still(&mut self, id: LinkId, window: WindowId) -> Result<Receiver<Result<IssueStill, String>>, String> {
        let link = self.live(id).ok_or("The app is no longer linked.")?;
        let (tx, rx) = mpsc::sync_channel(1);
        link.commands.push(Command::Still(window, StillTarget::Issue(tx)));
        Ok(rx)
    }

    pub fn record_stop(&mut self, id: LinkId) -> Ticket {
        let t = Ticket(self.serial());
        if let Some(l) = self.live(id) {
            l.commands.push(Command::Stop(t));
        } else {
            self.events.push(AppLinkEvent::Recorded {
                id,
                ticket: t,
                result: Err("app is not linked".into()),
            });
        }
        t
    }
    pub fn poll(&mut self) -> Vec<AppLinkEvent> {
        self.active.retain(|&id| {
            let l = self.links.get_mut(&id).unwrap();
            l.commands.flush();
            loop {
                match l.updates.try_recv() {
                    Ok(Update::Windows(windows)) => {
                        l.state.windows = windows;
                        self.events.push(if l.state.phase == LinkPhase::Connecting {
                            l.state.phase = LinkPhase::Live;
                            AppLinkEvent::Linked(id)
                        } else {
                            AppLinkEvent::Windows(id)
                        });
                    }
                    Ok(Update::LocalInput(epoch)) => {
                        if l.gate.epoch() == epoch {
                            l.state.input_allowed = false;
                        }
                        self.events.push(AppLinkEvent::LocalInput(id));
                    }
                    Ok(Update::Recording(t)) => l.state.recording = t,
                    Ok(Update::Ended { why, app_gone }) => {
                        if !app_gone {
                            l.restart = None;
                        }
                        l.state.phase = LinkPhase::Ended(why.clone());
                        l.state.recording = None;
                        l.state.input_allowed = false;
                        self.events.push(AppLinkEvent::Ended { id, why });
                    }
                    Ok(Update::Event(e)) => self.events.push(e),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if !matches!(l.state.phase, LinkPhase::Ended(_)) {
                            l.restart = None;
                            l.gate.close();
                            l.state.input_allowed = false;
                            l.state.recording = None;
                            let why = "link worker ended".to_string();
                            l.state.phase = LinkPhase::Ended(why.clone());
                            self.events.push(AppLinkEvent::Ended { id, why });
                        }
                        break;
                    }
                }
            }
            if matches!(l.state.phase, LinkPhase::Ended(_)) {
                l.commands.pending.clear();
                false
            } else {
                true
            }
        });
        self.stills.retain(|(id, ticket, rx)| {
            let result = match rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => Err("app link ended before the picture arrived".into()),
            };
            let size = result.as_ref().map_or((0, 0), |(_, w, h)| (*w, *h));
            self.events.push(AppLinkEvent::Still { id: *id, ticket: *ticket, size, result: result.map(|(path, ..)| path) });
            false
        });
        self.png_stills.retain(|(id, ticket, rx)| {
            let result = match rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => Err("app link ended before the picture arrived".into()),
            };
            self.events.push(AppLinkEvent::PngStill { id: *id, ticket: *ticket, result });
            false
        });
        let mut scan_disconnected = false;
        if let Some(scan) = &self.scan {
            loop {
                let (generation, (apps, unreachable)) = match scan.result.try_recv() {
                    Ok(reply) => reply,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        scan_disconnected = true;
                        break;
                    }
                };
                self.unreachable = unreachable;
                self.scan_completed = self.scan_completed.max(generation);
                self.events.push(AppLinkEvent::Apps);
                for (&id, link) in &mut self.links {
                    let Some(watch) = &mut link.restart else {
                        continue;
                    };
                    let ended = matches!(link.state.phase, LinkPhase::Ended(_));
                    if let Some(app) = watch.replacement(&link.state.app, ended, &apps) {
                        link.restart = None;
                        link.state.phase =
                            LinkPhase::Ended(format!("{} restarted. Link it again.", app.name));
                        self.events.push(AppLinkEvent::Restarted {
                            ended: id,
                            app: app.clone(),
                        });
                    }
                }
                self.apps = apps;
            }
        }
        if scan_disconnected {
            self.scan = None;
        }
        std::mem::take(&mut self.events)
    }
    fn releases_owed(&self) -> impl Iterator<Item = &LinkId> {
        self.active.iter().filter(|&id| self.links[id].gate.owes_release())
    }

    pub fn input_settling(&self) -> bool {
        self.releases_owed().next().is_some()
    }

    /// Seal all input, leaving the links available for other shutdown work.
    pub fn quit_input(&mut self, repeat_after_store: bool) -> InputQuit {
        let first = !self.quitting_input;
        if first {
            self.quitting_input = true;
            for id in &self.active {
                let link = self.links.get_mut(id).unwrap();
                link.gate.seal_input();
                link.state.input_allowed = false;
                link.commands.push(Command::Wake);
            }
        }
        match self.releases_owed().count() {
            0 => InputQuit::Ready,
            links_owing_release if !first && repeat_after_store => InputQuit::Force { links_owing_release },
            _ => InputQuit::Wait,
        }
    }

    pub fn shutdown(&mut self) {
        for l in self.links.values_mut() {
            l.restart = None;
            l.gate.close();
            l.state.input_allowed = false;
            l.commands.push(Command::Wake);
        }
        self.scan = None;
    }
}
impl Drop for AppLinks {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn error_json(why: &str) -> String {
    format!("{{\"err\":{}}}", makepad_remote::json_str(why))
}
