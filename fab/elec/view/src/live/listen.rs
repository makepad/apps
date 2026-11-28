//! Listen plays a running simulation's nets through the app's audio output as the Scope monitors them.
//!
//! What is heard is one set for the whole app: [`listen`] adds or takes out a net by its circuit
//! name from any view (a Scope lane's speaker chip), [`listen_jack`] a board's output jack (a part
//! whose data says it is one, `fab_elec_bench::audio`: each channel against its reference; a
//! closed box's Listen toggle, the transport's jack chip, and [`click_jack`]: a click on the jack
//! in the board or the schematic while a run is made), and each view shows [`listening`] /
//! [`listening_jack`] and the plain [`listen_status`]. The set's levels, master and mute live
//! with it, so every view hears and shows the same mix. While any net is heard, one tap of them
//! all is installed in the bench (`SimAction::Listen`, which also keeps the bench running).
//!
//! The app holds one [`ElecAudio`] (a `Cx` global, made on the first [`listen`]): the sample ring
//! the worker's tap fills (`fab_elec_sim::listen`), the one consumer that drains it and the heard
//! set. The platform's audio output callback runs a `Player` that plays live while the run keeps
//! up. Otherwise it loops the last rendered stretch while the next renders, crossfading at each switch.
//!
//! Realtime rules: the consumer takes no lock, waits on nothing and allocates nothing (the
//! player's two stretches are made with it). The UI thread reads a few atomics a frame. The worker
//! waits on its own thread when the ring is ahead (the tap's backpressure); nothing is dropped
//! while it is open.

use crate::instruments::bench::probe_node;
use crate::live::sim::{SimAction, SinkSlot};
use crate::ElecLive;
use fab_elec_bench::audio::{output_jack, output_jacks};
use fab_elec_circuit::{Circuit, NetKind};
use fab_elec_core::ids::{BoardId, CompRef, NetId};
use fab_elec_sim::device_audio::{DevicePlayer, DeviceTap};
use fab_elec_sim::listen::{voice_key, AudioTap, ListenBus, Player, SampleRing, TapMix, TapSpec, TapVoice, LOOP_SECONDS};
use fab_elec_sim::budget::KEEP_BYTES;
use fab_elec_sim::recording::{write_wav, PlayerLinks, RunSlot, Shape, Take, Tape};
use makepad_widgets_core::makepad_platform::thread::Class;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use makepad_widgets_core::*;
use std::collections::HashMap;
use std::fmt;
use makepad_widgets_core::makepad_platform::audio::{AudioDeviceId, AudioDevicesEvent};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// The heard set's tap: its id in the host's sink set, a voice's full scale per volt (a 2.5 V
/// swing round the net's rest is full scale), how far the worker starts ahead of the device (the
/// player then sizes it by measure), and each voice's output coupling corner.
pub const LISTEN_TAP: u64 = 0x4c49_5354_454e_0001;
/// The Listen id of the attached device's speaker tap ([`DeviceTap`]), beside the net tap.
pub const LISTEN_SPEAKER: u64 = 0x4c49_5354_454e_0002;
pub const LISTEN_GAIN: f32 = 0.4;
pub const LISTEN_LATENCY: f64 = 0.06;
pub const LISTEN_DC_HZ: f64 = 5.0;

/// A board's hearing recorder: its id in the host's sink set and its slot on the shelf. The host
/// installs it for a scope with output jacks or voltage probes ([`jack_tap_voices`],
/// [`probe_tap_voices`]), heard or not.
pub const JACK_TAP: u64 = 0x4a41_434b_0000_0001;

/// Seconds of audio the ring holds: room for the worker to run ahead of the device by much more
/// than its latency target, so a slow frame of the solver is absorbed before anyone hears it.
const RING_SECONDS: f64 = 0.5;

/// The sample rate in Hz used to size the Listen ring.
const RING_RATE: u32 = 48_000;

/// The capacity in frames of the ring shared with `SimLive::listen_bus`.
pub const RING_FRAMES: usize = (RING_SECONDS * RING_RATE as f64) as usize;

/// The app's Listen output and the signals it plays from `SimLive::listen_bus`.
pub struct ElecAudio {
    pub ring: Arc<SampleRing>,
    bus: Arc<ListenBus>,
    /// The output callback's calls and frames, and what it reported since it last opened ([`frame`]).
    device: Arc<DeviceCounts>,
    opened: Option<Opened>,
    /// The nets heard, in the order they were switched on; each net's level in dB (kept when it
    /// is no longer heard), the master level in dB and the mute.
    nets: Vec<String>,
    /// The output jacks heard, in the order they were switched on.
    jacks: Vec<CompRef>,
    /// The attached device's speakers heard ([`listen_speaker`]), those its device last ran with
    /// (a speaker seen anew is heard: on by default), and whether the device tap is installed.
    speakers: Vec<CompRef>,
    speakers_seen: Vec<CompRef>,
    speaker_tap: bool,
    levels: HashMap<String, f32>,
    master_db: f32,
    muted: bool,
    /// The installed tap: its nets (its voice order), its shared controls and meters, and a count
    /// that moves at each install or removal, so a view sees a new tap with one compare.
    tap_nets: Vec<String>,
    mix: Option<Arc<TapMix>>,
    generation: u64,
    /// What the installed tap hears (the nets and jacks it was made of) and its jacks' names for
    /// the status ("J1 out · stereo").
    tap_heard: (Vec<String>, Vec<CompRef>),
    tap_jacks: Vec<String>,
    /// The recording's window in seconds ([`record`]) and a shape the full queue refused (sent
    /// again at the next change or status read).
    record_s: f64,
    unsent_shape: Option<Box<Shape>>,
    /// The newest save's state, which its job writes.
    save: Arc<Mutex<SaveState>>,
    /// The kept takes the shelf had let go at the last log ([`frame`]), and the last take whose
    /// render was logged as differing from the kept take it replayed.
    evicted_seen: u64,
    differed_seen: u64,
    differed_take: Option<Take>,
}

/// The newest save of the recording ([`save_recording`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SaveState {
    #[default]
    Idle,
    Saving,
    Saved { path: PathBuf, seconds: f32 },
    Failed(String),
}

impl ElecAudio {
    /// Registers the app's Listen callback on first use. [`use_output`] selects the output
    /// while either ring is open.
    pub fn get(cx: &mut Cx) -> &ElecAudio {
        if !cx.has_global::<ElecAudio>() {
            let audio = ElecAudio::start(cx);
            cx.set_global(audio);
        }
        cx.get_global::<ElecAudio>()
    }

    fn start(cx: &mut Cx) -> ElecAudio {
        // the app's bus, or one of its own where no views run (the takes then carry no content)
        let bus = match cx.get_global_ref::<ElecLive>().is_some() {
            true => cx.get_global::<ElecLive>().sim.listen_bus(),
            false => Arc::new(ListenBus::new(RING_FRAMES, Arc::new(RunSlot::default()))),
        };
        // one output plays a bus: a second (never made today) plays a bus of its own
        let (bus, player_links) = match bus.take_player() {
            Some(player) => (bus, player),
            None => {
                log!("elec listen: the app's Listen bus is already played; this output plays one of its own");
                let own = Arc::new(ListenBus::new(RING_FRAMES, bus.runs.clone()));
                let player = own.take_player().expect("a new bus has its player's ends");
                (own, player)
            }
        };
        let ring = bus.ring.clone();
        let speaker = Speaker { ring: bus.device_ring.clone(), mix: bus.device_mix.clone() };
        ring.set_realtime(cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.status.realtime));
        let counts = Arc::new(DeviceCounts::default());
        device(cx, ring.clone(), speaker, player_links, counts.clone());
        ElecAudio {
            ring,
            bus,
            device: counts,
            opened: None,
            nets: Vec::new(),
            jacks: Vec::new(),
            speakers: Vec::new(),
            speakers_seen: Vec::new(),
            speaker_tap: false,
            levels: HashMap::new(),
            master_db: 0.0,
            muted: false,
            tap_nets: Vec::new(),
            mix: None,
            generation: 0,
            tap_heard: (Vec::new(), Vec::new()),
            tap_jacks: Vec::new(),
            record_s: cx.get_global_ref::<RecordWindow>().map_or(LOOP_SECONDS, |w| w.0),
            unsent_shape: None,
            save: Arc::default(),
            evicted_seen: 0,
            differed_seen: 0,
            differed_take: None,
        }
    }

    /// Drops what the player let go (chunks it replaced, shapes it is done with). O(items).
    fn drain(&self) {
        self.bus.links.spent.drain();
    }

    /// Sends a shape to the player, keeping it when the queue is full (one waits at most: a
    /// newer one replaces it).
    fn send_shape(&mut self, shape: Box<Shape>) {
        if let Err(std::sync::mpsc::TrySendError::Full(shape)) = self.bus.links.shapes.try_send(shape) {
            self.unsent_shape = Some(shape);
        }
    }

    /// Opens or closes the ring: open, the tap's backpressure paces the worker on the consumer;
    /// closed, the tap never waits and the consumer plays silence. [`use_output`] selects the
    /// default output while either ring is open. O(1).
    pub fn set_open(&mut self, open: bool) {
        let was = self.output_open();
        self.ring.set_open(open);
        self.output_changed(was);
    }

    /// Opens or closes the device ring (the attached device's speaker heard), as `set_open` the
    /// net ring: the output runs while either is open. O(1).
    fn set_speaker_open(&mut self, open: bool) {
        let was = self.output_open();
        self.bus.device_ring.set_open(open);
        self.output_changed(was);
    }

    /// Whether either ring is open: the output plays.
    fn output_open(&self) -> bool {
        self.ring.is_open() || self.bus.device_ring.is_open()
    }

    fn output_changed(&mut self, was: bool) {
        let open = self.output_open();
        if open != was {
            // the rate is the device's to say again: the next open may be another device
            self.ring.set_device_rate(0);
            self.bus.device_ring.set_device_rate(0);
            self.opened = open.then(|| Opened { at: Instant::now(), first: None, counts_said: false, late_said: false });
        }
    }

    /// The consumer's sample rate, once known (the device's first callback says it).
    pub fn rate(&self) -> Option<u32> {
        Some(self.ring.device_rate()).filter(|&hz| hz > 0)
    }

    /// The installed tap's voices by name (its voice order: the nets heard, then each heard
    /// jack's channels, `J1 L`) and mix, with the count that moves at each install or removal;
    /// empty and None while nothing is heard.
    pub fn tap(&self) -> (&[String], Option<&Arc<TapMix>>, u64) {
        (&self.tap_nets, self.mix.as_ref(), self.generation)
    }

    /// Whether anything is heard: a net or a jack.
    fn hears(&self) -> bool {
        !self.nets.is_empty() || !self.jacks.is_empty()
    }
}

/// Hears `net` (a circuit net by its name on the sheet, such as "LINE") on the speakers from any
/// view, or stops hearing it. The set is the app's: a Scope lane's speaker chip shows and changes
/// the same one. While any net is heard, the output is open and one tap of the set keeps the
/// bench running. O(nets heard).
pub fn listen(cx: &mut Cx, net: &str, on: bool) {
    if !on && cx.get_global_ref::<ElecAudio>().is_none() {
        return;
    }
    ElecAudio::get(cx);
    let audio = cx.get_global::<ElecAudio>();
    match (audio.nets.iter().position(|n| n == net), on) {
        (None, true) => audio.nets.push(net.to_owned()),
        (Some(at), false) => drop(audio.nets.remove(at)),
        _ => return,
    }
    sync(cx);
}

/// Hears a board's output jack (`jack`, a part whose data says it is an audio output,
/// `fab_elec_bench::audio::output_jacks`) on the speakers, or stops hearing it: each of its channels
/// against its reference, from any view. Joins the same heard set as [`listen`]. Below real time
/// a jack switched on is heard in windows of one run of the running bench when it names its
/// length (`run: {stop}`, see [`record`]). O(heard).
pub fn listen_jack(cx: &mut Cx, jack: CompRef, on: bool) {
    if !on && cx.get_global_ref::<ElecAudio>().is_none() {
        return;
    }
    let window = cx.get_global_ref::<ElecLive>().and_then(ElecLive::run_bench).and_then(|bench| bench.out.stop);
    ElecAudio::get(cx);
    let audio = cx.get_global::<ElecAudio>();
    match (audio.jacks.iter().position(|j| *j == jack), on) {
        (None, true) => {
            // the set changes, so its new tap records in these windows
            if let Some(seconds) = window.filter(|s| s.is_finite() && *s > 0.0) {
                audio.record_s = seconds;
            }
            audio.jacks.push(jack);
        }
        (Some(at), false) => drop(audio.jacks.remove(at)),
        _ => return,
    }
    sync(cx);
}

/// The attached device's speakers while its firmware runs (`FwLive::speakers`): each amplifier
/// part on its bus, by its component and reference designator. O(1).
pub fn running_speakers(cx: &Cx) -> &[(CompRef, String)] {
    cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.firmware.as_deref())
        .filter(|fw| fw.running_rev.is_some()).map_or(&[][..], |fw| &fw.speakers[..])
}

/// Hears the attached device's speaker (`speaker`, one of [`running_speakers`]) on the output, or
/// stops hearing it: what its firmware plays through its codec and amplifier, summed with the
/// heard nets and jacks at the master level and mute. A speaker is heard once its device runs (on
/// by default); one switched off stays off while its device runs. O(speakers).
pub fn listen_speaker(cx: &mut Cx, speaker: CompRef, on: bool) {
    if !on && cx.get_global_ref::<ElecAudio>().is_none() {
        return;
    }
    ElecAudio::get(cx);
    let audio = cx.get_global::<ElecAudio>();
    match (audio.speakers.iter().position(|s| *s == speaker), on) {
        (None, true) => audio.speakers.push(speaker),
        (Some(at), false) => drop(audio.speakers.remove(at)),
        _ => return,
    }
    sync_speaker(cx);
}

/// The run's speed as the attached device's speaker tap measures it, while a speaker is heard (0:
/// no reading yet). Below `KEEPS_UP` its sound plays in stretches. One atomic load.
pub fn speaker_speed(cx: &Cx) -> Option<f32> {
    cx.get_global_ref::<ElecAudio>().filter(|audio| audio.speaker_tap).map(|audio| audio.bus.device_ring.tap_speed())
}

/// Whether the attached device's speaker `speaker` is heard. O(speakers heard).
pub fn listening_speaker(cx: &Cx, speaker: CompRef) -> bool {
    cx.get_global_ref::<ElecAudio>().is_some_and(|audio| audio.speakers.contains(&speaker))
}

/// The device tap follows the heard speakers: installed under its own id while any is heard, the
/// device ring open; taken out and the ring closed when none is. The net tap is not touched. O(1).
fn sync_speaker(cx: &mut Cx) {
    let audio = cx.get_global::<ElecAudio>();
    let want = !audio.speakers.is_empty();
    if want == audio.speaker_tap {
        return;
    }
    audio.speaker_tap = want;
    audio.generation += 1;
    if want {
        audio.set_speaker_open(true);
        let tap = DeviceTap::new(audio.bus.device_ring.clone(), LISTEN_LATENCY);
        cx.action(SimAction::Listen { id: LISTEN_SPEAKER, sink: SinkSlot::new(Box::new(tap)) });
    } else {
        audio.set_speaker_open(false);
        cx.action(SimAction::Unlisten { id: LISTEN_SPEAKER });
    }
    use_output(cx);
}

/// A speaker seen anew (its device started running) is heard, as the board makes its sound
/// without being asked; a speaker gone is no longer heard. O(speakers).
fn speakers_by_default(cx: &mut Cx) {
    let seen = cx.get_global_ref::<ElecAudio>().map_or(&[][..], |audio| &audio.speakers_seen[..]);
    if running_speakers(cx).iter().map(|(comp, _)| comp).eq(seen.iter()) {
        return;
    }
    let running: Vec<CompRef> = running_speakers(cx).iter().map(|(comp, _)| *comp).collect();
    ElecAudio::get(cx);
    let audio = cx.get_global::<ElecAudio>();
    let new: Vec<CompRef> = running.iter().filter(|s| !audio.speakers_seen.contains(s)).copied().collect();
    audio.speakers.retain(|s| running.contains(s));
    audio.speakers.extend(new);
    audio.speakers_seen = running;
    sync_speaker(cx);
}

/// Whether the output jack `jack` is heard. O(jacks heard).
pub fn listening_jack(cx: &Cx, jack: CompRef) -> bool {
    cx.get_global_ref::<ElecAudio>().is_some_and(|audio| audio.jacks.contains(&jack))
}

/// A click on `comp` in a view while a run is made: an output jack is heard, or no longer heard
/// (a plug put in or pulled out). False, doing nothing, when no run is made or `comp` is no
/// output jack. O(its pins).
pub fn click_jack(cx: &mut Cx, comp: CompRef) -> bool {
    let runs = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.runs());
    if !runs || !with_circuit(cx, comp.board, |circuit| output_jack(circuit, comp.comp).is_some()).unwrap_or(false) {
        return false;
    }
    let on = !listening_jack(cx, comp);
    listen_jack(cx, comp, on);
    true
}

/// The output jacks of the board the bench runs on, with their names (`J1`), in component
/// order. O(components of that board).
pub fn running_jacks(cx: &Cx) -> Vec<(CompRef, String)> {
    let Some(live) = cx.get_global_ref::<ElecLive>() else { return Vec::new() };
    let Some(bench) = live.run_bench() else { return Vec::new() };
    let board = bench.out.scope_id();
    let Some(circuit) = bench.out.circuit(live.snap.live()) else { return Vec::new() };
    output_jacks(circuit).into_iter().map(|jack| (CompRef { board, comp: jack.comp }, jack.refdes)).collect()
}

/// `f` on the circuit whose components `board` names: a design unit's, or the running bench's own
/// circuit. None when neither is there. O(units + benches).
fn with_circuit<R>(cx: &Cx, board: BoardId, f: impl FnOnce(&Circuit) -> R) -> Option<R> {
    let live = cx.get_global_ref::<ElecLive>()?;
    let design = live.snap.live();
    if let Some(unit) = design.unit(board) {
        return Some(f(&unit.circuit));
    }
    let bench = live.run_bench().filter(|bench| bench.out.scope_id() == board)?;
    bench.out.circuit(design).map(f)
}

/// Sets the recording's window: a run below real time is heard in windows this long, each looped
/// until the next can play to its end. A heard set records anew from the run's next point. The
/// window is the board's to choose (a bar of its pattern, a note's decay); the default is
/// [`LOOP_SECONDS`]. O(nets heard).
pub fn record(cx: &mut Cx, seconds: f64) {
    if !(seconds.is_finite() && seconds > 0.0) {
        return;
    }
    // before anything is heard the setting waits on its own: it never opens the output
    if !cx.has_global::<ElecAudio>() {
        cx.set_global(RecordWindow(seconds));
        return;
    }
    let audio = cx.get_global::<ElecAudio>();
    if audio.record_s == seconds {
        return;
    }
    audio.record_s = seconds;
    // a new tap of the same set records in the new windows
    audio.tap_heard = (Vec::new(), Vec::new());
    sync(cx);
}

/// The recording's window in seconds.
pub fn record_seconds(cx: &Cx) -> f64 {
    match cx.get_global_ref::<ElecAudio>() {
        Some(audio) => audio.record_s,
        None => cx.get_global_ref::<RecordWindow>().map_or(LOOP_SECONDS, |w| w.0),
    }
}

/// The recording's window set before anything was heard.
struct RecordWindow(f64);

/// Saves the recording of what is heard as a WAV file at `path`: the take from its start (the
/// run's start, or where it last started over) to what is rendered now, the voices at their
/// levels and the master as they are now, summed and soft limited as heard (the mute does not
/// silence a save), 32-bit float mono at the run's rate. Writes on a pool thread; the status
/// says while it saves, then where it went or why not. Err (and nothing saved) when nothing of
/// what is heard is recorded yet. O(voices) here.
pub fn save_recording(cx: &mut Cx, path: PathBuf) -> Result<(), String> {
    if cx.get_global_ref::<ElecAudio>().is_none() && !jack_recorded(cx) {
        return Err("nothing is heard".into());
    }
    let jack_keys = jack_channel_keys(cx);
    // the save's state is the output's: made here when only a jack was recorded
    let audio = ElecAudio::get(cx);
    let found = match audio.mix.clone() {
        Some(live) => {
            let tape = audio.bus.tapes.current(LISTEN_TAP).filter(|tape| tape.take.tap == audio.generation);
            tape.ok_or("nothing of what is heard is recorded yet").map(|tape| (live, tape))
        }
        // nothing heard: the board's jacks as they were recorded, at full level (the hearing
        // record's other voices, the bench's probes, left out)
        None => audio.bus.tapes.current(JACK_TAP).ok_or("nothing is heard").map(|tape| (jack_save_mix(&tape, &jack_keys), tape)),
    };
    let (live, tape) = match found {
        Ok(found) => found,
        Err(why) => {
            *audio.save.lock().unwrap_or_else(|e| e.into_inner()) = SaveState::Failed(why.into());
            return Err(why.into());
        }
    };
    // the levels as they are now: a change while it saves does not reach the file
    let mix = TapMix::new(tape.voices);
    for j in 0..tape.voices {
        mix.set_level(j, live.level(j));
    }
    mix.set_master(live.master());
    let state = audio.save.clone();
    *state.lock().unwrap_or_else(|e| e.into_inner()) = SaveState::Saving;
    let done = state.clone();
    let job = move || {
        // what the recorder queued for the tape so far is on it (a pool thread may wait)
        fab_elec_sim::recording::settle_tapes();
        let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
            let mut out = BufWriter::new(std::fs::File::create(&path)?);
            let frames = write_wav(&tape, &mix, &mut out)?;
            out.flush()?;
            Ok(frames)
        });
        *done.lock().unwrap_or_else(|e| e.into_inner()) = match written {
            Ok(frames) => SaveState::Saved { seconds: (frames as f64 / tape.rate) as f32, path },
            Err(e) => SaveState::Failed(format!("{}: {e}", path.display())),
        };
    };
    match cx.task_pool().submit(Class::Background, job) {
        Ok(handle) => {
            // the job writes its result to the state the status reads
            handle.detach();
            Ok(())
        }
        Err(e) => {
            let why = format!("the save could not start: {e:?}");
            *state.lock().unwrap_or_else(|e| e.into_inner()) = SaveState::Failed(why.clone());
            Err(why)
        }
    }
}

/// Where a recording saved from the Scope goes: `recordings` in Fab's per-user folder, named by
/// the time it was saved.
pub fn recording_path() -> PathBuf {
    use makepad_widgets_core::makepad_platform::home::{app_data_dir, makepad_home};
    let dir = app_data_dir("fab").unwrap_or_else(|| makepad_home().join("fab")).join("recordings");
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    dir.join(format!("recording-{secs}.wav"))
}

/// The newest save's state.
pub fn save_state(cx: &Cx) -> SaveState {
    cx.get_global_ref::<ElecAudio>().map_or(SaveState::Idle, |audio| audio.save.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// The recording's save for a status line, while anything is heard: saving, where the newest
/// save went or why not, or why nothing can be saved (the take is not taped). None otherwise.
pub fn save_line(cx: &Cx) -> Option<String> {
    let audio = cx.get_global_ref::<ElecAudio>().filter(|audio| audio.hears() || jack_recorded(cx))?;
    if let Some(why) = audio.bus.tapes.error() {
        return Some(format!("Not taped: {why}"));
    }
    match &*audio.save.lock().unwrap_or_else(|e| e.into_inner()) {
        SaveState::Idle => None,
        SaveState::Saving => Some("Saving the recording".into()),
        SaveState::Saved { path, seconds } => Some(format!("Saved {seconds:.1} s to {}", path.display())),
        SaveState::Failed(why) => Some(format!("Not saved: {why}")),
    }
}

/// Whether the board's output jacks are recorded (the run's scope has one), heard or not: the
/// recording can be saved without Listen. O(1).
pub fn jack_recorded(cx: &Cx) -> bool {
    cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.listen.as_ref()).is_some_and(|bus| bus.tapes.current(JACK_TAP).is_some())
}

/// Whether `net` is heard. O(nets heard).
pub fn listening(cx: &Cx, net: &str) -> bool {
    cx.get_global_ref::<ElecAudio>().is_some_and(|audio| audio.nets.iter().any(|n| n == net))
}

/// A heard net's level in dB (0 until set; kept while it is not heard).
pub fn level_db(cx: &Cx, net: &str) -> f32 {
    cx.get_global_ref::<ElecAudio>().and_then(|audio| audio.levels.get(net).copied()).unwrap_or(0.0)
}

/// Sets a net's level in dB, live in the running tap (only once something was heard: a setting
/// never opens the output).
pub fn set_level_db(cx: &mut Cx, net: &str, db: f32) {
    if !cx.has_global::<ElecAudio>() {
        return;
    }
    let audio = cx.get_global::<ElecAudio>();
    audio.levels.insert(net.to_owned(), db);
    if let (Some(mix), Some(i)) = (&audio.mix, audio.tap_nets.iter().position(|n| n == net)) {
        mix.set_level(i, db_to_gain(db));
    }
}

/// The master level in dB.
pub fn master_db(cx: &Cx) -> f32 {
    cx.get_global_ref::<ElecAudio>().map_or(0.0, |audio| audio.master_db)
}

/// Sets the master level in dB, live in the running tap.
pub fn set_master_db(cx: &mut Cx, db: f32) {
    if !cx.has_global::<ElecAudio>() {
        return;
    }
    let audio = cx.get_global::<ElecAudio>();
    audio.master_db = db;
    if let Some(mix) = &audio.mix {
        mix.set_master(db_to_gain(db));
    }
    audio.bus.device_mix.set_master(db_to_gain(db));
}

/// Whether the mix is muted.
pub fn muted(cx: &Cx) -> bool {
    cx.get_global_ref::<ElecAudio>().is_some_and(|audio| audio.muted)
}

/// Mutes or unmutes the mix, live in the running tap.
pub fn set_muted(cx: &mut Cx, muted: bool) {
    if !cx.has_global::<ElecAudio>() {
        return;
    }
    let audio = cx.get_global::<ElecAudio>();
    audio.muted = muted;
    if let Some(mix) = &audio.mix {
        mix.set_muted(muted);
    }
    audio.bus.device_mix.set_muted(muted);
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The tap follows the heard set: a changed set installs a tap of its voices (under one id, so
/// it replaces the last), an empty one takes it out and closes the output. The tap waits on the
/// worker for the device's first callback to name its rate. O(nets heard).
fn sync(cx: &mut Cx) {
    let audio = cx.get_global::<ElecAudio>();
    audio.drain();
    if let Some(shape) = audio.unsent_shape.take() {
        audio.send_shape(shape);
    }
    if audio.tap_heard.0 == audio.nets && audio.tap_heard.1 == audio.jacks {
        return;
    }
    audio.generation += 1;
    audio.tap_heard = (audio.nets.clone(), audio.jacks.clone());
    if !audio.hears() {
        audio.set_open(false);
        (audio.mix, audio.tap_nets, audio.tap_jacks) = (None, Vec::new(), Vec::new());
        cx.action(SimAction::Unlisten { id: LISTEN_TAP });
        use_output(cx);
        return;
    }
    // the voices: each net against ground, then each jack's channels against its reference
    let (heard_nets, heard_jacks) = (audio.nets.clone(), audio.jacks.clone());
    let mut voices: Vec<(String, TapVoice)> = heard_nets.iter().map(|net| {
        (net.clone(), TapVoice { net: probe_node(cx, net), reference: None, gain: LISTEN_GAIN })
    }).collect();
    let mut jack_names = Vec::new();
    for jack in heard_jacks {
        match jack_voices(cx, jack) {
            Some((name, channels)) => {
                jack_names.push(name);
                voices.extend(channels);
            }
            None => log!("elec listen: the heard jack {jack:?} is no output jack of the design now; it is not heard"),
        }
    }
    let audio = cx.get_global::<ElecAudio>();
    audio.tap_jacks = jack_names;
    audio.set_open(true);
    let nets: Vec<String> = voices.iter().map(|(name, _)| name.clone()).collect();
    let mix = TapMix::new(nets.len());
    for (i, net) in nets.iter().enumerate() {
        mix.set_level(i, db_to_gain(audio.levels.get(net).copied().unwrap_or(0.0)));
    }
    mix.set_master(db_to_gain(audio.master_db));
    mix.set_muted(audio.muted);
    let ring = audio.ring.clone();
    (audio.mix, audio.tap_nets) = (Some(mix.clone()), nets.clone());
    let voices = voices.into_iter().map(|(_, voice)| voice).collect();
    let mut tap = AudioTap::new(TapSpec { voices, latency: LISTEN_LATENCY, dc_block_hz: LISTEN_DC_HZ, mix: mix.clone() }, ring);
    if !audio.ring.realtime() {
        // The shape reaches the player before the recorder's first chunk.
        let (id, window_s) = (audio.generation, audio.record_s);
        audio.send_shape(Box::new(Shape::new(id, window_s, nets.len(), mix)));
        tap = tap.recording(audio.bus.record(id, LISTEN_TAP, window_s));
    }
    cx.action(SimAction::Listen { id: LISTEN_TAP, sink: SinkSlot::new(Box::new(tap)) });
    use_output(cx);
}

/// The system's default output devices (from `Event::AudioDevices`, which comes at startup and
/// at each device change) and the ones this app asked the platform to run. Registering the
/// output callback runs nothing: a device plays only once it is asked for.
#[derive(Default)]
struct OutputDevices {
    default: Vec<AudioDeviceId>,
    used: Vec<AudioDeviceId>,
}

/// Moves an open output to the new default. Call for every `Event::AudioDevices`. O(devices).
pub fn audio_devices(cx: &mut Cx, event: &AudioDevicesEvent) {
    let default = event.default_output();
    if !cx.has_global::<OutputDevices>() {
        cx.set_global(OutputDevices::default());
    }
    let devices = cx.get_global::<OutputDevices>();
    devices.default = default;
    use_output(cx);
}

/// Selects the default output while either ring is open and deselects it after. O(devices).
fn use_output(cx: &mut Cx) {
    let open = cx.get_global_ref::<ElecAudio>().is_some_and(ElecAudio::output_open);
    if !cx.has_global::<OutputDevices>() {
        cx.set_global(OutputDevices::default());
    }
    let devices = cx.get_global::<OutputDevices>();
    let want = if open { devices.default.clone() } else { Vec::new() };
    if want == devices.used {
        return;
    }
    devices.used = want.clone();
    cx.use_audio_outputs(&want);
}

/// A heard jack's name for the status (`J1 out · stereo`) and its voices: each channel against the
/// jack's reference, named by the jack and side (`J1 L`). The channels share the jack's full
/// scale while the output is mono, so a stereo jack is no louder than a mono one. None when the
/// live design has no such output jack. O(components of its board).
fn jack_voices(cx: &Cx, jack: CompRef) -> Option<(String, Vec<(String, TapVoice)>)> {
    with_circuit(cx, jack.board, |circuit| jack_voices_in(circuit, jack))?
}

/// The voices a board's output jacks are recorded by: each jack's channels against its reference,
/// in component order, each the voice a Listen of the jack hears it by (`jack_voices_in`), so a
/// Listen of the jack plays from the hearing record. A net on more than one channel at the same
/// gain (a mono out on both sides of a stereo jack) is recorded once: the same columns for half
/// the work, weighed by its channels when the jacks are saved ([`jack_save_mix`]). `board` is
/// the circuit's. O(components + voices^2).
pub fn jack_tap_voices(circuit: &Circuit, board: BoardId) -> Vec<TapVoice> {
    let mut voices: Vec<TapVoice> = Vec::new();
    let channels = output_jacks(circuit).into_iter().filter_map(|jack| jack_voices_in(circuit, CompRef { board, comp: jack.comp }));
    for (_, voice) in channels.flat_map(|(_, channels)| channels) {
        if !voices.contains(&voice) {
            voices.push(voice);
        }
    }
    voices
}

/// The keys of every output jack channel's voice in the live design, one per channel
/// (`listen::voice_key`): a save of the hearing record with nothing heard mixes the jacks by them
/// ([`jack_save_mix`]). O(components of the design).
fn jack_channel_keys(cx: &Cx) -> Vec<u64> {
    let Some(live) = cx.get_global_ref::<ElecLive>() else { return Vec::new() };
    let mut keys = Vec::new();
    for unit in &live.snap.live().units {
        for jack in output_jacks(&unit.circuit) {
            if let Some((_, voices)) = jack_voices_in(&unit.circuit, CompRef { board: unit.id, comp: jack.comp }) {
                keys.extend(voices.iter().map(|(_, voice)| voice_key(voice, LISTEN_DC_HZ)));
            }
        }
    }
    keys
}

/// The levels a save of the hearing record mixes the board's jacks at with nothing heard: each of
/// its voices at the jack channels it records (`keys`, one per channel: a net on both channels of
/// a stereo jack is one voice at two), the bench's probes it also tapes at none, so the file is
/// the jacks' mix as a Listen of them hears it. O(voices x keys).
fn jack_save_mix(tape: &Tape, keys: &[u64]) -> Arc<TapMix> {
    let mix = TapMix::new(tape.voices);
    for (j, key) in tape.keys.iter().enumerate() {
        mix.set_level(j, keys.iter().filter(|&k| k == key).count() as f32);
    }
    mix
}

/// The voices of `nets` (the bench's voltage probes on `circuit`) as Listen hears each from a
/// Scope lane: the net against ground at [`LISTEN_GAIN`] (a ground net as "0", as `probe_node`
/// names it), so a take recorded with them plays any of them heard later. A net twice is one
/// voice. O(nets).
pub fn probe_tap_voices(circuit: &Circuit, nets: impl IntoIterator<Item = NetId>) -> Vec<TapVoice> {
    let mut voices: Vec<TapVoice> = Vec::new();
    for net in nets {
        let Some(n) = circuit.nets.get(net.ix()) else { continue };
        let name = if n.kind == NetKind::Ground { "0".to_owned() } else { n.name.clone() };
        if !voices.iter().any(|v| v.net == name) {
            voices.push(TapVoice { net: name, reference: None, gain: LISTEN_GAIN });
        }
    }
    voices
}

fn jack_voices_in(circuit: &Circuit, jack: CompRef) -> Option<(String, Vec<(String, TapVoice)>)> {
    let found = output_jack(circuit, jack.comp)?;
    let node = |net: NetId| circuit.nets.get(net.ix()).filter(|n| n.kind != NetKind::Ground).map(|n| n.name.clone());
    let channels: Vec<NetId> = found.channels().collect();
    let gain = LISTEN_GAIN / channels.len() as f32;
    let voices = channels.iter().enumerate().map(|(i, &net)| {
        let side = match (found.stereo(), i) {
            (false, _) => "",
            (true, 0) => " L",
            (true, _) => " R",
        };
        let voice = TapVoice { net: node(net).unwrap_or_else(|| "0".to_owned()), reference: node(found.reference), gain };
        (format!("{}{side}", found.refdes), voice)
    }).collect();
    Some((format!("{} out · {}", found.refdes, if found.stereo() { "stereo" } else { "mono" }), voices))
}

/// What the output plays now, for a status line or a progress bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ListenMode {
    /// The audio device has not said its rate yet.
    Opening,
    /// The run is paused or settled: nothing flows.
    Waiting,
    /// The run keeps real time and is heard as it runs, this many seconds behind it.
    Live { latency_s: f32 },
    /// Below real time, before the recording plays, or before a new take of it plays (the run
    /// started over after an edit, the last take heard meanwhile): its first window renders,
    /// `done_s` of `length_s` so far; `starts_in_s` is the wait until it plays (it starts before
    /// it is whole once the rest renders in time), once the speed is measured.
    Rendering { done_s: f32, length_s: f32, starts_in_s: Option<f32> },
    /// Below real time: the recording plays, a window of `length_s` round while the next renders,
    /// `ahead_s` of it so far.
    Playing { length_s: f32, ahead_s: f32 },
}

/// The output's state: its mode, the run's measured speed (audio seconds per second; 0 before
/// the first measure), the sample rate the run steps on (0: none), the device's rate, the
/// underruns counted and, while a take renders, the edit that started its run ("R7 = 22k").
/// `Display` gives the plain line and [`save_line`] adds the recording's save.
#[derive(Clone, Debug, PartialEq)]
pub struct ListenStatus {
    pub mode: ListenMode,
    pub speed: f32,
    /// The last measured speed was below real time.
    pub behind: bool,
    /// The live sample lead and the device buffer, in milliseconds.
    pub lead_ms: f32,
    pub buffer_ms: f32,
    /// The heard jacks by name (`J1 out · stereo`).
    pub jacks: Vec<String>,
    /// The rate the run is simulated at, Hz (0: none, its bandwidth step).
    pub sim_rate: f64,
    /// The rate its takes are recorded at, Hz: named when it is not the simulated rate.
    pub recording_rate: f64,
    pub rate: Option<u32>,
    pub underruns: u64,
    pub edit: Option<Arc<str>>,
}

/// The output's state while any net is heard, None while none is. A few atomic loads.
pub fn listen_status(cx: &Cx) -> Option<ListenStatus> {
    let audio = cx.get_global_ref::<ElecAudio>().filter(|audio| audio.hears())?;
    audio.drain();
    let st = audio.ring.play_status();
    let mode = if audio.rate().is_none() {
        ListenMode::Opening
    } else if audio.ring.hushed() || !st.rendered && !audio.ring.running() {
        ListenMode::Waiting
    } else if !st.rendered {
        ListenMode::Live { latency_s: st.latency_s }
    } else if st.loop_s > 0.0 && !st.new_take {
        ListenMode::Playing { length_s: st.loop_s, ahead_s: st.ready_s }
    } else {
        let starts_in_s = (st.speed > 0.0).then(|| (st.start_s - st.ready_s).max(0.0) / st.speed);
        ListenMode::Rendering { done_s: st.ready_s, length_s: st.window_s, starts_in_s }
    };
    // the rendering take's run: the edit that started it, if one did
    let edit = matches!(mode, ListenMode::Rendering { .. }).then(|| audio.bus.tapes.current(LISTEN_TAP)).flatten().and_then(|tape| tape.content.as_ref()?.edit.clone());
    let (sim_rate, recording_rate) = cx.get_global_ref::<ElecLive>().map_or((0.0, 0.0), |live| (live.sim.status.sample_rate, live.sim.status.recording_rate));
    Some(ListenStatus { mode, speed: st.speed, behind: st.behind, lead_ms: st.lead_ms, buffer_ms: st.buffer_ms,
        jacks: audio.tap_jacks.clone(), sim_rate, recording_rate, rate: audio.rate(), underruns: audio.ring.underruns(), edit })
}

impl fmt::Display for ListenStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.mode {
            ListenMode::Opening => write!(f, "Opening the audio device")?,
            ListenMode::Waiting => write!(f, "Listening · the run is paused")?,
            ListenMode::Live { latency_s } => write!(f, "Live · {:.0} ms behind the run", latency_s * 1000.0)?,
            ListenMode::Rendering { done_s, length_s, starts_in_s } => {
                match &self.edit {
                    Some(edit) => write!(f, "Re-rendering with {edit} · {done_s:.1} of {length_s:.1} s")?,
                    None => write!(f, "Rendering {done_s:.1} of {length_s:.1} s")?,
                }
                if let Some(wait) = starts_in_s {
                    write!(f, " · plays in {wait:.0} s")?;
                }
            }
            ListenMode::Playing { length_s, ahead_s } if ahead_s >= length_s => write!(f, "Playing the recording · {length_s:.1} s round · next ready")?,
            ListenMode::Playing { length_s, ahead_s } => write!(f, "Playing the recording · {length_s:.1} s round · next {ahead_s:.1} s rendered")?,
        }
        for jack in &self.jacks {
            write!(f, " · {jack}")?;
        }
        if self.sim_rate > 0.0 {
            write!(f, " · run at {}", crate::live::strip::format_sample_rate(self.sim_rate))?;
        }
        if self.recording_rate > 0.0 && self.recording_rate != self.sim_rate {
            write!(f, " · recorded at {}", crate::live::strip::format_sample_rate(self.recording_rate))?;
        }
        if let Some(hz) = self.rate {
            write!(f, " · out {:.1} kHz", f64::from(hz) / 1000.0)?;
        }
        if self.speed > 0.0 && !matches!(self.mode, ListenMode::Waiting | ListenMode::Opening) {
            write!(f, " · x{:.2}", self.speed)?;
        }
        Ok(())
    }
}

/// Once a frame, from the sim host: whether the output plays the run's recording (the run is
/// below real time; value edits then restart the run, so each take renders one content). Logs
/// each kept take the shelf let go for room (a later take of its content renders again) and,
/// once a take, a take whose render differs from the kept take it replays (the run is not a
/// function of its content key: a restart's operating point seeded from the last run may move
/// the low bits). A few atomic loads.
pub fn frame(cx: &mut Cx) -> bool {
    speakers_by_default(cx);
    if !cx.has_global::<ElecAudio>() {
        return false;
    }
    let no_device = cx.get_global_ref::<OutputDevices>().is_none_or(|devices| devices.default.is_empty());
    let status = cx.get_global_ref::<ElecLive>().map(|live| live.sim.status.clone());
    let realtime = status.as_ref().is_some_and(|s| s.realtime);
    let audio = cx.get_global::<ElecAudio>();
    let mode_changed = realtime != audio.ring.realtime();
    if mode_changed {
        audio.ring.set_realtime(realtime);
        // Reinstall the heard voices with or without their playback recorder.
        audio.tap_heard = (Vec::new(), Vec::new());
    }
    // a take of finite length is recorded in one window of its length: the loop is the take
    if let Some(take) = status.as_ref().and_then(|s| s.take) {
        record(cx, take);
    }
    if mode_changed {
        sync(cx);
    }
    // Stop and Pause silence what is heard (a recording's loop too); Play brings it back
    let hushed = cx.get_global_ref::<ElecLive>().is_some_and(|live| !live.sim.playing);
    let audio = cx.get_global::<ElecAudio>();
    audio.ring.set_hushed(hushed);
    // a scrub's release goes on playing the take from there only in Loop
    audio.ring.set_looping(status.is_some_and(|s| s.take.is_some() && !s.continuous));
    say_output(audio, no_device);
    let evicted = audio.bus.tapes.evicted.load(Ordering::Relaxed);
    if evicted > audio.evicted_seen {
        log!("elec listen: let go of {} kept take(s) for room (the kept takes passed {} MiB); a later take of that content renders again", evicted - audio.evicted_seen, KEEP_BYTES >> 20);
        audio.evicted_seen = evicted;
    }
    let differed = audio.bus.tapes.differed.load(Ordering::Relaxed);
    if differed > audio.differed_seen {
        let take = audio.bus.tapes.current(LISTEN_TAP).map(|tape| tape.take);
        if take != audio.differed_take {
            let worst = f32::from_bits(audio.bus.tapes.worst.load(Ordering::Relaxed));
            log!("elec listen: the render of take {take:?} differs from the kept take it replays by up to {:.0} dBFS ({} chunk(s) so far): the restart's operating point is seeded from the last run, so it renders a hit a little differently; the kept samples played", 20.0 * worst.max(1e-12).log10(), differed - audio.differed_seen);
            audio.differed_take = take;
        }
        audio.differed_seen = differed;
    }
    audio.hears() && audio.ring.plays_recording()
}

/// The hand holds the line at `t` (a scrub on the wave view or the bar), or let go (None): the
/// player scratches the recorded take there at the hand's speed and direction, paused too, and
/// on release goes on from there (Loop, playing) or holds there (paused). One atomic store.
pub fn scrub_audio(cx: &mut Cx, t: Option<f64>) {
    if let Some(audio) = cx.get_global_ref::<ElecAudio>() {
        audio.ring.set_scrub(t);
    }
}

/// The take plays on from the run's time `t` (Play from where the person is): the player moves its
/// head there in the recording, playing or held as the transport is, and so does the host's silent
/// playback clock while nobody listens ([`seek_clock`]). One atomic store and one action.
pub fn seek_audio(cx: &mut Cx, t: f64) {
    if let Some(audio) = cx.get_global_ref::<ElecAudio>() {
        audio.ring.seek(t);
    }
    seek_clock(cx, t);
}

/// The host's silent playback clock moves to `t`, playing or held as it is (`SimCmd::Seek`; the
/// host ignores it while the player is heard, which follows its own head).
pub fn seek_clock(cx: &mut Cx, t: f64) {
    cx.action(SimAction::Cmd(fab_elec_bench::SimCmd::Seek(t)));
}

/// Whether the views follow the take as it plays back (with sound or by the host's silent clock,
/// moving or held) rather than the run's front: a scrub let go hands the line back to it.
pub fn plays_back(cx: &Cx) -> bool {
    hears_recording(cx) || cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.status.plays_back)
}

/// Whether what is heard comes from the recording now (the player follows the line there): a
/// scrub's release then hands the line back to the instant heard. A few atomic loads.
pub fn hears_recording(cx: &Cx) -> bool {
    cx.get_global_ref::<ElecAudio>().is_some_and(|audio| audio.hears() && audio.ring.plays_recording())
}

/// The device's callback: the player's samples on every channel. Per buffer: O(frames x
/// channels), no lock, no allocation, no wait.
fn device(cx: &mut Cx, ring: Arc<SampleRing>, speaker: Speaker, links: PlayerLinks, counts: Arc<DeviceCounts>) {
    let mut player = Player::new(Some(links));
    let mut device_player = DevicePlayer::new();
    let mut scratch = vec![0.0f32; SPEAKER_SCRATCH];
    cx.audio_output(0, move |info, out| {
        let rate = info.sample_rate.round().max(0.0) as u32;
        if rate != ring.device_rate() {
            ring.set_device_rate(rate);
        }
        if rate != speaker.ring.device_rate() {
            speaker.ring.set_device_rate(rate);
        }
        let (channels, frames) = (out.channel_count(), out.frame_count());
        counts.calls.fetch_add(1, Ordering::Relaxed);
        counts.frames.fetch_add(frames as u64, Ordering::Relaxed);
        if channels == 0 || frames == 0 {
            return;
        }
        player.fill(&ring, &mut out.channel_mut(0)[..frames], info.sample_rate);
        device_player.mix_into(&speaker.ring, &speaker.mix, &mut out.channel_mut(0)[..frames], &mut scratch, info.sample_rate);
        for c in 1..channels {
            for i in 0..frames {
                let v = out.channel(0)[i];
                out.channel_mut(c)[i] = v;
            }
        }
    });
}

/// An attached process's speaker as the output hears it: the Listen bus's device ring and its
/// level and mute ([`ListenBus::device_ring`]).
struct Speaker {
    ring: Arc<SampleRing>,
    mix: Arc<TapMix>,
}

/// The output's scratch for the speaker's sound, made at start: a device buffer larger than this
/// is summed in pieces.
const SPEAKER_SCRATCH: usize = 4096;

/// The device callback's calls and the frames it was asked for, counted on the callback (two
/// relaxed adds a buffer).
#[derive(Default)]
struct DeviceCounts {
    calls: AtomicU64,
    frames: AtomicU64,
}

/// The output's open, for its log: when it was asked for, the device's first callback (the
/// counts then) and what was said since.
struct Opened {
    at: Instant,
    first: Option<FirstCall>,
    counts_said: bool,
    late_said: bool,
}

/// The device's first callback after an open: when the UI saw it, the counts then, and the ring's
/// underruns a second later (the device's start may starve the ring once).
struct FirstCall {
    at: Instant,
    calls: u64,
    frames: u64,
    consumed: usize,
    underruns: u64,
    underruns_1s: Option<u64>,
}

/// Seconds after the device's first callback at which its counts are logged once, and after the
/// ask at which a device that has not called back is.
const OPEN_PROOF_S: f64 = 2.0;

/// Logs the output's open: the device's rate once its first callback says it,
/// then, `OPEN_PROOF_S` later, the callbacks, frames and samples the ring gave and its underruns;
/// or, once, that no callback has run `OPEN_PROOF_S` after the ask (`no_device`: the system names
/// no default output). O(1).
fn say_output(audio: &mut ElecAudio, no_device: bool) {
    let Some(opened) = audio.opened.as_mut() else { return };
    let rate = audio.ring.device_rate();
    let counts = (audio.device.calls.load(Ordering::Relaxed), audio.device.frames.load(Ordering::Relaxed));
    if opened.first.is_none() && rate > 0 {
        let at = Instant::now();
        log!("[listen] output open: {rate} Hz, {:.0} ms after it was asked for", (at - opened.at).as_secs_f64() * 1000.0);
        opened.first = Some(FirstCall { at, calls: counts.0, frames: counts.1, consumed: audio.ring.consumed(), underruns: audio.ring.underruns(), underruns_1s: None });
    }
    match opened.first.as_mut() {
        Some(first) => {
            let s = first.at.elapsed().as_secs_f64();
            if s >= 1.0 && first.underruns_1s.is_none() {
                first.underruns_1s = Some(audio.ring.underruns());
            }
            if s >= OPEN_PROOF_S && !opened.counts_said {
                opened.counts_said = true;
                let consumed = audio.ring.consumed().wrapping_sub(first.consumed);
                let underruns = audio.ring.underruns() - first.underruns;
                let late = audio.ring.underruns() - first.underruns_1s.unwrap_or(first.underruns);
                log!("[listen] output: {} callbacks, {} frames in {s:.1} s at {rate} Hz; the ring gave {consumed} samples; {underruns} underruns, {late} after the first second", counts.0 - first.calls, counts.1 - first.frames);
            }
        }
        None => {
            let s = opened.at.elapsed().as_secs_f64();
            if s >= OPEN_PROOF_S && !opened.late_said {
                opened.late_said = true;
                let why = if no_device { ": the system names no default output" } else { "" };
                log!("[listen] output asked for, but its callback has not run in {s:.1} s{why}: nothing plays yet");
            }
        }
    }
}
