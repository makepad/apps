//! The live bench's side of the view contract. The simulation host (`fab-elec-simhost`) writes a
//! [`SimLive`] once a frame (ELEC's `ElecLive` holds it); views read it and register their wiring in
//! it, and ask the bench for everything else with a [`SimAction`], a Cx action the app routes to the
//! host the way `ViewAction` goes to the design host.

use crate::live::segs::SegRows;
use fab_elec_bench::{SimCmd, SimMode, ValueIndex};
use fab_elec_core::ids::{CompRef, NetRef};
use fab_elec_lang::sys_sim::SimSettings;
use fab_elec_sim::{analysis::Sink, cosim::CoState, trace::{SignalId, TraceStore}};
use makepad_widgets_core::*;
use std::cell::Cell;
use std::fmt;
use makepad_widgets_core::makepad_platform::log::LogLevel;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// The wait before playback while the run prepares its circuit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preparing {
    /// This is the first run the host has started for this board.
    pub first: bool,
    /// Wall seconds since the host first saw this run preparing.
    pub wall: f64,
}

/// The view's copy of the transport state; the simulation host owns its source type.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveStatus {
    pub t: f64,
    pub playing: bool,
    pub stepping: bool,
    pub preparing: Option<Preparing>,
    pub rate_asked: f64,
    pub rate_achieved: Option<f64>,
    pub limited_by: Option<LiveLimit>,
    /// Where the stepping time goes while the run goes on, as shares of 1 (None at rest).
    pub split: Option<LiveSplit>,
    pub steady: bool,
    /// The time resolution the newest values were stepped at, seconds: 0 is Exact.
    pub res: f64,
    /// The user's setting: Fast (the default: the resolution follows the finest view), Exact
    /// (resolution 0) or Logic (Fast with the timing cells).
    pub mode: SimMode,
    /// The timing cells the running lowering has (Logic's threshold inverters run as cells).
    pub cells: usize,
    /// The sample rate the run steps on, Hz (0: none, its bandwidth step); with no run, the rate
    /// the next one takes.
    pub sample_rate: f64,
    /// The user's pick (None: the host's choice: the audio rate while a net is heard, else none).
    pub sample_rate_chosen: Option<f64>,
    /// The rate the run's takes are recorded at, Hz; with no run, the rate the next one's are.
    pub recording_rate: f64,
    /// The user's pick (None: the default audio rate).
    pub recording_rate_chosen: Option<f64>,
    /// The default load for the output jacks (None: a line input).
    pub jack_load: Option<fab_elec_bench::audio::AudioLoad>,
    /// The take's length, simulated seconds (None: an open run): the run renders to its end and
    /// rests, and the recording plays it round.
    pub take: Option<f64>,
    /// Continuous is in effect. The run is open and keeps a rolling record.
    pub continuous: bool,
    /// The open run plays directly as it is simulated.
    pub realtime: bool,
    /// Continuous's ring, simulated seconds.
    pub ring: f64,
    /// Where the newest edit heard goes in, while the take renders again from there.
    pub rerender: Option<f64>,
    /// Why the take renders again for a Listen, in plain words, while it does: the Listen could not
    /// play it from the hearing recorder ("voice not recorded in this take (QUIET)", "the tape
    /// stopped short (1.000 of 2.000 s)", "the take is not rendered yet").
    pub rerender_reason: Option<Arc<str>>,
    /// Auto re-record is on: an edit while the recording plays renders the take again from where
    /// it is heard; off, the edit waits for the next record.
    pub auto_rerecord: bool,
    /// With auto re-record off, the edits and presses waiting for the next record.
    pub waiting: u32,
    /// The take plays back: the views follow the instant it plays (`SimLive::heard`), with sound
    /// or by the host's silent clock, moving or held; false while they follow the run's front.
    pub plays_back: bool,
}

impl Default for LiveStatus {
    fn default() -> Self {
        Self { t: 0.0, playing: false, stepping: false, preparing: None, rate_asked: 1.0,
            rate_achieved: None, limited_by: None, split: None, steady: false, res: 0.0, mode: SimMode::Fast, cells: 0,
            sample_rate: 0.0, sample_rate_chosen: None, recording_rate: 0.0, recording_rate_chosen: None, jack_load: None, take: None,
            continuous: false, realtime: false, ring: fab_elec_bench::values::RING_SECONDS, rerender: None, rerender_reason: None,
            auto_rerecord: true, waiting: 0, plays_back: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveLimit {
    Paced,
    Emulator,
    /// The scopes' acquisition.
    Scope,
    Solver,
    /// The device's event feed is full and holds its time until the host takes its events.
    Events,
    /// A streamed input (a motion track) does not yet cover what the run reads next; it waits.
    Input,
}

/// The shares of the stepping time: the solver's, the scopes' acquisition's and the emulator's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiveSplit {
    pub solver: f32,
    pub scope: f32,
    pub emulator: f32,
}

/// An LED's last stopped instant, shared by every view without per-item updates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveLed {
    pub part: CompRef,
    pub i: f32,
    pub i_mean: f32,
    pub brightness: f32,
    pub rgb: Option<[f32; 3]>,
}

/// A display panel a board's firmware draws, as the newest publication left it. The host writes it,
/// as it writes `leds` and `glow`; the bench's board pane and the 3D view draw `texture` on the part's
/// active area (the part library's screen face).
///
/// Conventions, shared by every reader and by board input in panel texels: texels are sRGB-encoded
/// and alpha is ignored; memory row 0 is the edge the face's `up` points to, and memory +x runs along
/// `up × dir` (right, seen from outside). `glass` is the part of the memory the active area shows;
/// its aspect matches the active area's (the host reports a mismatch past 1 %).
#[derive(Clone, Debug, PartialEq)]
pub struct LiveScreen {
    /// The panel part, as `glow_parts` names LEDs.
    pub comp: CompRef,
    /// The panel's whole frame memory, `TextureFormat::VecBGRAu8_32`, `size` texels.
    pub texture: Texture,
    /// The frame memory's width and height, in texels.
    pub size: (u32, u32),
    /// The window of the memory the glass shows: x, y, width and height, in texels.
    pub glass: [u32; 4],
    /// The panel shows its memory; false draws it dark.
    pub on: bool,
    /// The panel's brightness, 0..1.
    pub brightness: f32,
    /// The mean emitted colour over `glass`, linear RGB 0..1, times `brightness`; zero while off.
    /// The host computes it as it converts the frame, once per publication: what a view's light
    /// and glow take from the panel without reading the texture.
    pub mean: [f32; 3],
    /// The simulated time of the frame in `texture`, seconds. While a scrub shows another instant,
    /// or the Listen output plays the recording (`listen::frame`), the frame is the run's newest: a
    /// view labels it with this time.
    pub t: f64,
}

/// One firmware compile diagnostic, as the Firmware pane marks it.
#[derive(Clone, Debug, PartialEq)]
pub struct FwDiagnostic {
    /// Byte range in the document's text; None for a diagnostic without a site.
    pub span: Option<std::ops::Range<usize>>,
    /// The bench constant it concerns (`b.firmware`'s params), if any.
    pub param: Option<String>,
    pub message: String,
}

/// Flash to: the boards this PC can flash the running image to, and how a flash goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlashLive {
    pub devices: Vec<FlashDevice>,
    /// The chosen board, by its port's path.
    pub chosen: Option<String>,
    /// In place of an empty list: why there is nothing to choose.
    pub hint: Option<String>,
    pub state: FlashState,
    /// Monitor: the chosen board's own watch stream.
    pub monitor: MonitorState,
    /// The monitor's lines for the pane to take; a new feed is a new stream (the list starts
    /// empty), the same feed across a flash keeps the lines before it.
    pub feed: Option<Arc<MonitorFeed>>,
}

/// How the board's watch stream stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MonitorState {
    #[default]
    Off,
    /// Opening the port and restarting the board, so the stream starts at its Hello.
    Starting,
    /// The stream runs.
    On,
    /// The monitor lets go of the port for a flash or a read, and comes back after it.
    Paused,
    /// The stream ended by itself (the board went away, the port failed): why. Its lines stay.
    Ended(String),
}

/// The pane's list keeps this many of the monitor's newest lines; the log file keeps them all.
pub const MONITOR_HELD: usize = 20_000;

/// The monitor's lines on their way to the pane: the host appends them as they arrive and the
/// pane takes what waits, both on the UI thread (the lock is never contended). Lines waiting past
/// [`MONITOR_HELD`] (a pane that is not drawn) go only to the log file, counted, as the pane's list
/// would drop them anyway.
#[derive(Debug, Default)]
pub struct MonitorFeed {
    waiting: Mutex<(VecDeque<(LogLevel, String)>, u64)>,
}

impl MonitorFeed {
    pub fn push(&self, level: LogLevel, line: String) {
        let mut waiting = self.waiting.lock().unwrap();
        if waiting.0.len() == MONITOR_HELD {
            waiting.0.pop_front();
            waiting.1 += 1;
        }
        waiting.0.push_back((level, line));
    }

    /// What waits, and how many lines went only to the file since the last take.
    pub fn take(&self) -> (VecDeque<(LogLevel, String)>, u64) {
        std::mem::take(&mut *self.waiting.lock().unwrap())
    }
}

/// A feed is equal only to itself: `FlashLive` compares which stream it carries, not its lines.
impl PartialEq for MonitorFeed {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlashDevice {
    /// The port's path, which a choice names.
    pub path: String,
    pub label: String,
    /// The few words the closed list shows for it (the chip and its port or PC).
    pub face: String,
    /// Why it may not be flashed (its row is greyed); None: it may.
    pub off: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum FlashState {
    #[default]
    Idle,
    /// The step under way, in a few words.
    Flashing(String),
    /// A read of the chosen board (its chip and its whole flash's MD5) under way.
    Reading(String),
    /// A back-up of the chosen board's whole flash under way: how far it is.
    BackingUp(String),
    Done(String),
    Failed(String),
    ReadFailed(String),
    BackupFailed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioLive {
    pub state: RadioState,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadioState {
    Absent,
    Fetching { done: u64, file: String },
    Installing,
    Linking,
    Ready { island_bytes: u64 },
    Failed(String),
    Cancelled,
}

/// The Firmware pane's Network row: the real servers the bench's connections name, and this
/// computer's choice for the project. The twin plays the bench's fake for each connection unless
/// the person turned the real network on, and then only toward the hosts they allowed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkLive {
    pub servers: Vec<NetServerLive>,
    /// The project has a file, so this computer can keep a choice for it (the switch shows).
    pub switchable: bool,
    /// The real network is on for the project.
    pub on: bool,
    /// When it was last turned on, and when the twin last opened a real connection (dates).
    pub since: Option<String>,
    pub last_used: Option<String>,
    /// Why the twin plays the fakes whatever the choice (a hidden window, a plate not saved).
    pub held: Option<String>,
}

/// A connection the bench points at a real server, or at a service of this computer's hub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetServerLive {
    pub c: u8,
    /// The endpoint's name, the key of its token; None: it goes without one.
    pub name: Option<String>,
    /// What the person approves: the server's host, or `hub:<service>`.
    pub host: String,
    pub address: String,
    /// Its host (or hub service) is allowed for the project.
    pub approved: bool,
    pub token: TokenLive,
}

/// The token this computer's settings hold for a named endpoint: never its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenLive {
    /// The endpoint has no name, so no token.
    NoName,
    Missing,
    Saved,
    /// The one saved under its name was given for this other host, so it is not sent.
    OtherHost(String),
    /// The settings could not be read.
    Unreadable(String),
}

/// The Firmware pane's microphone for this app session. The running image supplies the rate.
#[derive(Clone, Debug)]
pub struct MicLive {
    pub file: Option<std::path::PathBuf>,
    pub level_db: i32,
    pub rate: Result<u32, &'static str>,
    pub ready: bool,
    pub status: String,
    pub computer: bool,
    pub capture_status: String,
}

pub const MIC_LEVEL_DB: i32 = -24;
pub const NO_RUNNING_BOARD: &str = "No board is running.";

impl Default for MicLive {
    fn default() -> Self {
        Self { file: None, level_db: MIC_LEVEL_DB, rate: Err(NO_RUNNING_BOARD), ready: false, status: String::new(),
            computer: false, capture_status: String::new() }
    }
}

/// The selected firmware binding and its compile state, replaced only when that state changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FwLive {
    /// Component path of the first firmware binding, the device this run can instantiate.
    pub part: String,
    /// Project-relative name of the bound firmware document.
    pub doc: String,
    /// Revision of the last good image handed to the simulation; kept after a failed compile.
    pub running_rev: Option<u64>,
    /// Revision currently compiling on the pool, if any.
    pub compiling_rev: Option<u64>,
    /// The running image's id (the compiler's hash of source, constants and clock; the device
    /// says it in its Hello), so a view can tell which build runs.
    pub image_id: Option<u32>,
    /// Last failed revision and its diagnostics, located in that revision's text.
    pub failed: Option<(u64, Vec<FwDiagnostic>)>,
    /// Compiles whose result came back after a newer commit: dropped, never attached.
    pub superseded: u64,
    /// Compiler notes belonging to the running image.
    pub notes: Vec<String>,
    /// Other bound component paths, which cannot run beside this run's one device.
    pub not_running: Vec<String>,
    /// The device's speakers: each amplifier part on its bus (`AMP`), by its component and
    /// reference designator. What the device plays on them is heard through the Listen bus's
    /// device ring (`listen::listen_speaker`).
    pub speakers: Vec<(CompRef, String)>,
    /// Why this binding cannot compile: no clock or a missing project document.
    pub problem: Option<String>,
    /// The device's EEPROM as the project keeps it between runs, its size in bytes: what the next
    /// run from reset starts with. None: it starts as a new chip.
    pub eeprom: Option<usize>,
    /// Flash to, while the running image can go to a board (an S3's); None: it cannot.
    pub flash: Option<FlashLive>,
    /// The Wi-Fi libraries, while this binding selects an ESP32-S3.
    pub radio: Option<RadioLive>,
    /// The real network, while the bench points a connection of this binding at a server.
    pub network: Option<NetworkLive>,
}

/// One net's voltage statistics over the take slot's span in simulated seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvSlot {
    pub t0: f64,
    pub t1: f64,
    pub mean: f32,
    pub min: f32,
    pub max: f32,
}

/// A view's latest complete coarse trace. Its slots get a new Arc only when they change.
#[derive(Clone, Debug)]
pub struct NetEnvelope {
    pub id: u64,
    pub net: NetRef,
    pub slots: Arc<Vec<EnvSlot>>,
    /// The take's original grid interval in simulated seconds.
    pub grid: f64,
}

/// What views read of the live bench, as of the host's last frame.
#[derive(Default)]
pub struct SimLive {
    /// One copy per frame; consumers compare it before changing their read-outs.
    pub status: LiveStatus,
    /// Where the host puts up what each run simulates before the run starts (its content key,
    /// the edit that started it, its clean stretch): the Listen tap's recorder stamps each take
    /// that starts with a run from it. One slot for the views' life; the host writes into it.
    pub runs: Arc<fab_elec_sim::recording::RunSlot>,
    /// The bench's expectations as the run's judge last published them (`SimHost::judged`): the
    /// host's slot, shared from when the host is made. The Bench editor reads it as the run
    /// publishes (a lock held for a copy).
    pub judged: Arc<std::sync::Mutex<fab_elec_bench::judge::Judged>>,
    /// What Listen and the recorders share (the ring, the queues to the player, the shelf of
    /// takes, `runs`), made by whoever needs it first ([`SimLive::listen_bus`]), with no device.
    pub listen: Option<Arc<fab_elec_sim::listen::ListenBus>>,
    /// One Vec per stop (O(LEDs)), or new publication at rest; never rebuilt on running frames.
    pub leds: Arc<Vec<LiveLed>>,
    /// The running lowering's LEDs, in the order of `glow`: one Vec per lowering.
    pub glow_parts: Arc<Vec<CompRef>>,
    /// Each of `glow_parts`' brightness (0..1) over the newest publication's interval, running or
    /// at rest: its mean anode current over its rated current. One Vec per fresh publication
    /// (O(LEDs)), kept on unchanged frames.
    pub glow: Arc<Vec<f32>>,
    /// The app's firmware status, shared until the binding or compile state changes.
    pub firmware: Option<Arc<FwLive>>,
    /// The microphone controls and WAV status, shared across Firmware panes.
    pub mic: Arc<MicLive>,
    /// The row textures of `shared/live-rows.md`, bound as `live_nets`, `live_segs` and `live_parts`
    /// (None before the host's first frame).
    pub nets: Option<Texture>,
    pub segs: Option<Texture>,
    pub parts: Option<Texture>,
    /// The brightest LED light output `parts` holds (a part row's y, 0..1): what a view's glow can
    /// reach without reading the texture.
    pub emission: f32,
    /// Moves whenever a part row's LED light output (y) changes so it can show ([`super::light_moved`]:
    /// a running publication, a stop's or a step's instant, a scrub's instant, a clear) or the rows
    /// renumber, and only then: a view that draws LED light keys on it, so a bench whose LEDs hold
    /// still (or only leak a solver's noise) repaints nothing.
    pub lights: u64,
    /// The mini-trace texture ([`crate::live::mini`]), bound as `live_mini` (None before the first frame).
    pub mini: Option<Texture>,
    /// The firmware pin rows ([`crate::live::pin_row`]), bound as `live_pins`; None while no host
    /// writes them, and the views keep their off texture.
    pub pins: Option<Texture>,
    /// The lowering's display panels, by part. One Vec per lowering or change of the running
    /// machine, never per frame: the textures' contents change in place. A stop keeps the last
    /// frames; a clear or a restart from 0 turns each panel off until the new machine's first
    /// publication.
    pub screens: Arc<Vec<LiveScreen>>,
    /// Moves whenever any screen's pixels, `on`, brightness or `mean` change, and only then. The
    /// host's counter, monotonic across runs and restarts: a view that draws panels keys its repaint
    /// on it, as LED light keys on `lights`.
    pub screens_key: u64,
    /// The co-simulated device's state at the newest publication (`LiveValues::co`; None without
    /// a device or before its first publication): the Firmware pane's state panel reads a
    /// `fab_elec_mcu_cosim::FwState` from it. One Arc per publication, equal while the same Arc,
    /// so a reader compares it before reading.
    pub co: Option<CoState>,
    /// Moves each time the host's emphasis changes (the rows' w): a view drawing the rows draws
    /// again when it moves, as for a publication, so a hover lights its net while paused too.
    pub emphasis: u64,
    /// The `live_time` uniform: the animation clock (wall seconds), not simulated time.
    pub time: f32,
    /// The simulated time the views show, seconds (0 before the first values): the instant heard
    /// while the recording plays, else the latest values'.
    pub t: f64,
    /// While the recording plays, the instant heard (`t`) and whether the sound moves on (false:
    /// paused, stopped, waiting for the render); None otherwise. A time strip follows it.
    pub heard: Option<(f64, bool)>,
    /// The run's one take's span of simulated time, (first, last) (the trace's before it has a
    /// frame; 0, 0 while empty): the stretch a time strip reviews.
    pub record: (f64, f64),
    /// Requested net envelopes in id order. A new Arc only when an envelope changes, appears or leaves.
    pub envelopes: Arc<Vec<NetEnvelope>>,
    /// Simulated seconds per wall second over the last publication (`LiveValues::ratio`; None before
    /// the first values): "sim 1 : N".
    pub ratio: Option<f64>,
    /// While a paused cursor sits inside the record (a scrub): the instant shown, None when live.
    pub scrub: Option<f64>,
    /// The consumer record while a scrub lasts (None otherwise). Signal k < nets is net row k
    /// (`index.net`); pin currents follow at nets + `index.pin`. Read it only while
    /// `trace.generation == index.generation`. Samples at or before `scrub` never change while the scrub
    /// lasts (the points that arrive late land after the head it began at), except that the record's
    /// front trim (`keep_last`) may drop the oldest chunk from a retaken snapshot: a cursor on that chunk
    /// then has no samples (`index_at` is None), which a view shows as such.
    pub trace: Option<Arc<TraceStore>>,
    /// The consumer record as the host holds it now, running, paused or scrubbing (None before the
    /// first point and after a Stop): the Scope tab's lanes read it, as the mini strips fill from
    /// it. The host's Arc, a new one each time its trace worker publishes; never copied. Signal
    /// k < nets is net row k (`index.net`). Read it only while `store.generation == index.generation`.
    pub store: Option<Arc<TraceStore>>,
    /// The current lowering's numbering: `net(NetRef)` is a net's row, `comp(CompRef)` a part's row
    /// (both renumber with each lowering, as for any design edit). Whether a value change has a
    /// simulation effect: `!index.command(&SimCmd::SetParam { .. }).is_empty()`.
    pub index: Arc<ValueIndex>,
    /// Whether the bench plays (the last play or pause sent).
    pub playing: bool,
    /// The newest edit taken in where it was heard ("R7 = 22k"), its time in `status.rerender`.
    pub edit: Option<Arc<str>>,
    /// The output jacks with their own loads, by reference designator in ascending order, as the
    /// host applies them (`SimCmd::SetJackLoadOf`, or the project's `sim` statement); the others
    /// drive `status.jack_load`. A new Arc only when they change.
    pub jack_loads: Arc<Vec<(String, fab_elec_bench::audio::AudioLoad)>>,
    /// Why the run's fast mode is off (a round plan failed, the run stays whole); None while on.
    pub plan_error: Option<Arc<str>>,
    /// Momentary inputs held pressed: while the recording plays, those the take had held at the
    /// instant heard; else those a person holds in any view, including a release still waiting
    /// out MIN_PRESS. A new Arc only when this set changes.
    pub held: Arc<Vec<CompRef>>,
    /// Knobs turned, and to where (0 to 1): while the recording plays, where the take had them at
    /// the instant heard; else where a person turned them in any view. A knob not listed sits
    /// where the lowering starts it (`index.knobs()`). It follows the run's values: a Stop keeps
    /// it (the next run starts the knobs there), a lowering that puts the design's values back
    /// empties it. A new Arc only when a place changes.
    pub turned: Arc<Vec<(CompRef, f64)>>,
    /// The one segment-row allocator. A view registers a geometry's trees when it lays them out
    /// (`trees.set(owner, ..)` returns the rows at once, so the instances built in the same pass carry
    /// them) and removes them when the geometry goes; panes of the same geometry share them. The host
    /// clears freed rows and computes the segment currents of every registered tree.
    pub trees: SegRows,
    /// The nets a view shows strips for: set them on layout with the geometry owner (as for
    /// `trees`), keep the returned slots in its instances, and remove the owner when it goes.
    pub minis: crate::live::mini::MiniSubs,
}

impl SimLive {
    /// The app's Listen bus, made at the first ask: its ring holds `listen::RING_FRAMES`, and its
    /// takes are stamped from `runs`. O(1) after the first.
    pub fn listen_bus(&mut self) -> Arc<fab_elec_sim::listen::ListenBus> {
        let runs = &self.runs;
        self.listen.get_or_insert_with(|| Arc::new(fab_elec_sim::listen::ListenBus::new(crate::live::listen::RING_FRAMES, runs.clone()))).clone()
    }

    /// Stop: the run's state leaves every view, as before the first Play. No rows (views draw
    /// their off textures: no wire stripes, rims, heat or LED light), no LED snapshot or glow, no
    /// device state (the Firmware pane goes idle), time 0 and an empty record; `lights` moves
    /// once, so a view drawing LED light draws it off, and no input is held (a switch held in a
    /// view draws open again). `status` is the stopped bench's. The
    /// numbering, the firmware binding, the emphasis and the views' row registrations stay.
    /// O(1) plus dropping the run's Arcs and textures. The display panels keep their last frames
    /// (`screens` and `screens_key` stay).
    pub fn stop(&mut self, status: LiveStatus) {
        self.status = status;
        self.leds = Arc::default();
        self.glow = Arc::default();
        (self.nets, self.segs, self.parts, self.mini, self.pins) = (None, None, None, None, None);
        self.emission = 0.0;
        self.lights += 1;
        self.co = None;
        self.t = 0.0;
        self.record = (0.0, 0.0);
        if !self.envelopes.is_empty() {
            self.envelopes = Arc::default();
        }
        self.ratio = None;
        self.scrub = None;
        self.trace = None;
        self.store = None;
        self.playing = false;
        self.edit = None;
        self.plan_error = None;
        if !self.held.is_empty() {
            self.held = Arc::default();
        }
        // the knobs stay where they were turned: the run's values do, and the next run starts there
    }

    /// Whether any of the run's state is published: what [`SimLive::stop`] clears.
    pub fn runs(&self) -> bool {
        !self.leds.is_empty() || !self.glow.is_empty() || self.nets.is_some() || self.segs.is_some()
            || self.parts.is_some() || self.mini.is_some() || self.pins.is_some() || self.emission != 0.0
            || self.co.is_some() || self.t != 0.0 || self.record != (0.0, 0.0) || self.ratio.is_some()
            || !self.envelopes.is_empty() || self.scrub.is_some() || self.trace.is_some() || self.store.is_some()
            || self.playing || !self.held.is_empty()
    }
}

/// A run setting the person picked (the transport's accuracy and sample-rate chips), written into
/// the project's system document as its `sim` statement (`fab_elec_lang::sys_sim`), which the
/// simulation host follows: one undoable text edit. False without a project (a standalone scope):
/// the caller sends the setting to its host as a command instead. A document that cannot take it
/// (it does not parse, or has two statements) logs why and changes nothing. O(system text).
pub fn write_run_settings(cx: &mut Cx, change: impl FnOnce(&mut SimSettings)) -> bool {
    let Some(docs) = cx.get_global_ref::<crate::ElecLive>().and_then(|live| live.snap.docs.clone()) else { return false };
    let Some(text) = docs.system.and_then(|doc| docs.texts.iter().find(|t| t.doc == doc)) else { return false };
    let mut settings = docs.sim.clone();
    change(&mut settings);
    match fab_elec_lang::sys_sim::edit(text.doc, &text.text, &settings) {
        Ok(edits) if edits.is_empty() => {}
        Ok(edits) => cx.action(crate::ViewAction::Text {
            doc: text.doc, base_rev: text.rev, edits, group: None, label: "run settings".into(), tag: crate::snapshot::next_tag(),
        }),
        Err(diag) => log!("electronics: the run settings are not written: {}", diag.message),
    }
    true
}

/// What a view asks of the live bench.
#[derive(Debug)]
pub enum SimAction {
    /// A bench command by design id: play, pause, step, rate, and a value site's `SetParam`, sent on
    /// every pointer sample (the host sends it on at once).
    Cmd(SimCmd),
    /// A scope became visible: its acquisition sink receives every accepted point from now on.
    /// `column` is its finest pixel column, simulated seconds (0 for a sink that draws none): the
    /// run's time resolution follows the finest open view.
    ShowScope { id: u64, sink: SinkSlot, column: f64 },
    /// The scope also wants the ensemble: one member sink per member, from the feed.
    Ensemble { id: u64, feed: FeedSlot },
    /// The scope is hidden: drops its nominal sink and every member sink of this id.
    HideScope { id: u64 },
    /// The hovered or probed nets and parts (the rows' emphasis).
    Emphasis { nets: Vec<NetRef>, parts: Vec<CompRef> },
    /// The co-simulated device's pin hovered, by its pin table name (`CoSimPin::name`, which
    /// `FwPin::pin` names): its pin row and the net its pad is on are emphasised; None clears.
    EmphasisDevicePin(Option<String>),
    /// A gesture starts (a knob press, a drag): the trace as it stands is kept as the ghost.
    BeginGesture,
    /// The ghost is dropped.
    ClearGhost,
    /// Listen: the tap (`fab_elec_sim::listen::AudioTap`) receives every accepted point from now
    /// on, and while any tap is installed the worker runs unpaced: the tap's ring paces it, so the
    /// audio device's clock is the master and the two clocks never drift apart.
    Listen { id: u64, sink: SinkSlot },
    /// The tap of this id is removed; the worker goes back to the bench's rate.
    Unlisten { id: u64 },
    /// The signals a view draws over the whole run (its probes and lanes), by their rows in the
    /// record (`SimLive::store`) of the numbering it has: the record keeps every signal any view
    /// names whole over the run and the rest for the live front only (empty withdraws the ask).
    Probes { id: u64, rows: Vec<usize> },
    /// Request one net's coarse trace over the take. None withdraws this id's request.
    Envelope { id: u64, net: Option<NetRef> },
    /// The spilled logic chunks a view shows in detail (`LogicTrace::spilled_in`), by signal and
    /// first time: the bench loads them back into the record while any view asks (empty withdraws
    /// the ask).
    LogicDetail { id: u64, chunks: Vec<(SignalId, f64)> },
}

/// A sink carried by an action. Actions are delivered by reference, so the host takes it out once.
pub struct SinkSlot(Cell<Option<Box<dyn Sink + Send>>>);

impl SinkSlot {
    pub fn new(sink: Box<dyn Sink + Send>) -> SinkSlot {
        SinkSlot(Cell::new(Some(sink)))
    }

    /// No sink: a view that reads the record and only wants the run going.
    pub fn none() -> SinkSlot {
        SinkSlot(Cell::new(None))
    }

    /// The sink, the first time.
    pub fn take(&self) -> Option<Box<dyn Sink + Send>> {
        self.0.take()
    }
}

impl fmt::Debug for SinkSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SinkSlot")
    }
}

/// Hands out one acquisition sink per ensemble member. The host calls it once per member it starts.
pub trait SinkFeed {
    fn member(&mut self, member: u32) -> Box<dyn Sink + Send>;
}

/// A feed carried by an action. Actions are delivered by reference, so the host takes it out once.
pub struct FeedSlot(Cell<Option<Box<dyn SinkFeed + Send>>>);

impl FeedSlot {
    pub fn new(feed: Box<dyn SinkFeed + Send>) -> FeedSlot {
        FeedSlot(Cell::new(Some(feed)))
    }

    /// The feed, the first time.
    pub fn take(&self) -> Option<Box<dyn SinkFeed + Send>> {
        self.0.take()
    }
}

impl fmt::Debug for FeedSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FeedSlot")
    }
}
