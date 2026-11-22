//! A picked WAV or the person's live microphone as a timed input to the simulated board.

use fab_elec_bench::{BoardInput, SimCmd};
use fab_elec_core::CompRef;
use fab_elec_fw::{mic::{MicInput, MicPort, MicStatus}, sound_track, TrackInput};
use fab_elec_mcu_cosim::FwState;
use fab_elec_mcu_parts::{PartKind, SoundTrack, TrackRef};
use fab_elec_sim::recording::{RunContent, MAX_RATE};
use fab_elec_simhost::SimHost;
use fab_elec_view::{firmware::{ComputerMicClick, FirmwareMic}, live::sim::{MicLive, SimAction, NO_RUNNING_BOARD}, ElecLive};
use makepad_widgets::makepad_platform::{audio::AudioDeviceId, remote};
use makepad_widgets::makepad_platform::thread::{to_ui_oneshot, Class, ToUIOneshotReceiver};
use makepad_widgets::*;
use std::{borrow::Cow, path::{Path, PathBuf}, sync::{atomic::{AtomicU32, Ordering}, mpsc::TryRecvError, Arc, OnceLock}};

#[derive(Clone, Debug)]
pub(crate) struct PlayFile {
    pub bed: u64,
    pub file: PathBuf,
    pub call: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct MicAnswer {
    pub call: u64,
    pub result: Result<(), String>,
}

impl PlayFile {
    fn answer(&self, cx: &mut Cx, result: Result<(), String>) {
        if let Some(call) = self.call { cx.action(MicAnswer { call, result }); }
    }
}

// Start copying with the very first buffer. If the negotiated rate needs more storage
// than the recording path normally reserves, the UI supplies the larger ring.
struct DeviceFormat {
    hz: AtomicU32,
    first: Arc<MicPort>,
    larger: OnceLock<Arc<MicPort>>,
}

const FORMAT_CHANGED: u32 = u32::MAX;

struct Capture {
    device: AudioDeviceId,
    format: Arc<DeviceFormat>,
    run: Arc<RunContent>,
    comp: CompRef,
    refdes: String,
    rate: u32,
    source: Option<Arc<MicInput>>,
    attached: bool,
    last_status: Option<(bool, u64, u64, u64)>,
}

struct FinishingCapture {
    source: Arc<MicInput>,
    submitted: bool,
    message: Option<String>,
}

#[derive(Default)]
pub struct MicPlayback {
    shown: Arc<MicLive>,
    default_input: Option<AudioDeviceId>,
    capture: Option<Capture>,
    finishing: Vec<FinishingCapture>,
    frame: Option<NextFrame>,
    // A browser picker supplies bytes directly. A native pick is read on the pool.
    bytes: Option<Arc<[u8]>>,
    // A catalog pick can only read within its saved project, including when the level changes.
    folder: Option<PathBuf>,
    // Play only on the run for which the WAV was requested.
    requested: Option<(PlayFile, u64, Arc<RunContent>)>,
    // Keep the recording across stops when the next program declares the same rate.
    recording_rate: Option<u32>,
    track: Option<SoundTrack>,
    loading: Option<ToUIOneshotReceiver<Result<SoundTrack, String>>>,
    // While an older conversion finishes, only the newest pick, level and rate need work.
    dirty: bool,
}

impl MicPlayback {
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, mut sim: Option<&mut SimHost>, bed: u64, project: Option<&Path>) {
        if let Event::AudioDevices(devices) = event {
            self.default_input = devices.default_input().first().copied();
            if self.capture.as_ref().is_some_and(|c| !devices.descs.iter().any(|d| d.device_id == c.device && !d.has_failed)) {
                self.close_capture(cx, "The computer microphone is unavailable.");
            }
        }
        if matches!(event, Event::Pause | Event::Shutdown) {
            self.close_capture(cx, "Computer mic off.");
        }
        if !matches!(event, Event::Actions(_) | Event::Signal | Event::NextFrame(_) | Event::Draw(_)
            | Event::AudioDevices(_) | Event::Pause | Event::Shutdown) { return; }
        if matches!(event, Event::NextFrame(ne) if self.frame.is_some_and(|f| ne.set.contains(&f))) { self.frame = None; }
        let rate = mic_rate(sim.as_deref());
        if self.shown.rate != rate {
            Arc::make_mut(&mut self.shown).rate = rate;
        }
        if let Ok(rate) = rate {
            if self.recording_rate != Some(rate) {
                self.recording_rate = Some(rate);
                self.invalidate();
            }
        }
        if let Event::Actions(actions) = event {
            for action in actions {
                if let Some(request) = action.downcast_ref::<PlayFile>() {
                    if let Err(why) = self.pick_request(cx, request, sim.as_deref(), bed, project) {
                        request.answer(cx, Err(why.into()));
                    }
                }
                if matches!(action.downcast_ref::<SimAction>(), Some(SimAction::Cmd(
                    SimCmd::Pause | SimCmd::Hold | SimCmd::Stop | SimCmd::Step(_) | SimCmd::StepFw(_)))) {
                    self.close_capture(cx, "Computer mic off. The board paused.");
                }
                if let Some(mic) = action.downcast_ref::<FirmwareMic>() {
                    match *mic {
                        FirmwareMic::Pick => cx.open_select_file_dialog(FileDialog::new()
                            .set_id(live_id!(fab_mic_wav)).set_title("Choose a WAV for the board microphone".into())
                            .add_filter("WAV audio".into(), vec!["wav".into()])),
                        FirmwareMic::Level(db) => {
                            let db = db.clamp(-60, 0);
                            if self.shown.level_db != db {
                                Arc::make_mut(&mut self.shown).level_db = db;
                                if let Some(source) = self.capture.as_ref().and_then(|c| c.source.as_ref()) {
                                    source.set_level_db(f64::from(db)).expect("the slider supplies a finite level");
                                }
                                self.invalidate();
                            }
                        }
                        FirmwareMic::ComputerOn(click) => {
                            if self.capture.is_none() {
                                for old in &mut self.finishing { old.message = None; }
                                match self.open_computer_microphone(cx, click, sim.as_deref()) {
                                    Ok(capture) => {
                                        self.capture = Some(capture);
                                        let shown = Arc::make_mut(&mut self.shown);
                                        shown.computer = true;
                                        shown.capture_status = "Waiting for the computer microphone…".into();
                                    }
                                    Err(why) => Arc::make_mut(&mut self.shown).capture_status = why.into(),
                                }
                            }
                        }
                        FirmwareMic::ComputerOff => self.close_capture(cx, "Computer mic off."),
                        FirmwareMic::Play if self.capture.is_none() => {
                            // If the board went away since the button was drawn, the pane shows
                            // the rate lookup's reason until the board comes back.
                            if let (Ok(_), Some(sim), Some(track)) = (rate, sim.as_deref_mut(), self.track.as_ref()) {
                                let message = play(sim, track).err().unwrap_or_default();
                                Arc::make_mut(&mut self.shown).status = message.to_owned();
                            }
                        }
                        FirmwareMic::Play => {}
                    }
                }
                match action.downcast_ref::<FileDialogAction>() {
                    Some(FileDialogAction::FileSelected { id, paths }) if *id == live_id!(fab_mic_wav) => {
                        if let Some(path) = paths.first() { self.pick(cx, path.clone(), None, None); }
                    }
                    Some(FileDialogAction::FileLoaded { id, files }) if *id == live_id!(fab_mic_wav) => {
                        if let Some(file) = files.first() { self.pick(cx, PathBuf::from(&file.name), Some(file.bytes.clone()), None); }
                    }
                    _ => {}
                }
            }
        }
        if let Some(capture) = &mut self.capture {
            match capture.follow(sim.as_deref_mut(), rate, self.shown.level_db) {
                Ok(Some(status)) => Arc::make_mut(&mut self.shown).capture_status = status,
                Ok(None) => {}
                Err(why) => self.close_capture(cx, &why),
            }
        }
        self.finishing.retain_mut(|ending| {
            if ending.source.input.track.is_closed() {
                if let Some(message) = &ending.message {
                    Arc::make_mut(&mut self.shown).capture_status = capture_status(message, ending.source.status());
                }
                return false;
            }
            if ending.submitted { return true; }
            let source = ending.source.clone();
            match cx.task_pool().submit_named(Class::Background, "finish board microphone", move || source.finish()) {
                Ok(task) => { task.detach(); ending.submitted = true; }
                Err(why) => log!("cannot finish the microphone capture yet: {why}"),
            }
            true
        });
        if (self.capture.is_some() || !self.finishing.is_empty()) && self.frame.is_none() {
            self.frame = Some(cx.new_next_frame());
        }
        self.convert(cx);
        if let Some((request, run, content)) = &self.requested {
            let result = if request.bed != bed || !sim.as_deref().is_some_and(|s|
                s.live_run() == Some(*run) && s.content().is_some_and(|c| Arc::ptr_eq(c, content))) {
                Err("The board run ended before the WAV could play.".to_string())
            } else if self.capture.is_some() {
                Err("The computer microphone is in use.".into())
            } else if let Err(why) = rate {
                Err(why.into())
            } else if self.dirty || self.loading.is_some() {
                // The pool wakes the UI when the conversion finishes.
                return self.publish(cx);
            } else if let (Some(sim), Some(track)) = (sim.as_deref_mut(), &self.track) {
                play(sim, track).map_err(str::to_owned)
            } else {
                Err(self.shown.status.clone())
            };
            request.answer(cx, result);
            self.requested = None;
        }
        self.publish(cx);
    }

    fn publish(&self, cx: &mut Cx) {
        if cx.get_global_ref::<ElecLive>().is_some_and(|live| !Arc::ptr_eq(&live.sim.mic, &self.shown)) {
            cx.get_global::<ElecLive>().sim.mic = self.shown.clone();
            cx.new_next_frame();
        }
    }

    /// The only caller of `audio_input` in Fab. Only the Firmware toggle's on action reaches it.
    fn open_computer_microphone(&self, cx: &mut Cx, click: ComputerMicClick, sim: Option<&SimHost>) -> Result<Capture, &'static str> {
        if std::env::var_os("MAKEPAD_HIDE_WINDOWS").is_some() {
            return Err("The computer mic is off in a hidden instance.");
        }
        if cfg!(test) || remote::requested() || !click.from_person() {
            return Err("The computer mic is off for tests and remote control.");
        }
        if cfg!(target_arch = "wasm32") { return Err("This browser build has no microphone input."); }
        let rate = mic_rate(sim)?;
        let sim = sim.ok_or(NO_RUNNING_BOARD)?;
        if !capture_playing(sim) { return Err("Play the board before turning on the computer mic."); }
        let run = sim.content().ok_or(NO_RUNNING_BOARD)?.clone();
        let (comp, refdes) = codec(sim)?;
        let device = self.default_input.ok_or("No default computer microphone is available.")?;
        let format = Arc::new(DeviceFormat { hz: AtomicU32::new(0), first: MicPort::new(MAX_RATE as u32)?, larger: OnceLock::new() });
        let callback = format.clone();
        let mut writer = None;
        let mut hz = 0;
        let mut moved = false;
        cx.audio_input(0, move |info, buffer| {
            if info.device_id != device || buffer.channel_count() == 0 || buffer.frame_count() == 0 { return; }
            let rate = info.sample_rate.round() as u32;
            if hz == 0 {
                writer = callback.first.writer(rate);
                hz = if writer.is_some() { rate } else { FORMAT_CHANGED };
                callback.hz.store(hz, Ordering::Release);
            }
            if hz != rate || hz == FORMAT_CHANGED {
                hz = FORMAT_CHANGED;
                callback.hz.store(hz, Ordering::Release);
                return;
            }
            if let Some(writer) = &mut writer {
                if !moved {
                    if let Some(next) = callback.larger.get().and_then(|port| port.writer(hz)) {
                        writer.move_to(next);
                        moved = true;
                    }
                }
                writer.push(buffer.channel(0));
            }
        });
        cx.use_audio_inputs(&[device]);
        Ok(Capture { device, format, run, comp, refdes: refdes.to_owned(), rate, source: None, attached: false, last_status: None })
    }

    fn close_capture(&mut self, cx: &mut Cx, why: &str) {
        let Some(capture) = self.capture.take() else { return };
        capture.format.first.stop();
        let shown = Arc::make_mut(&mut self.shown);
        shown.computer = false;
        shown.capture_status = if let Some(source) = capture.source {
            source.stop();
            let status = capture_status(why, source.status());
            self.finishing.push(FinishingCapture { source, submitted: false, message: Some(why.to_owned()) });
            status
        } else {
            capture_status(why, MicStatus { overflows: capture.format.first.overflows(), ..MicStatus::default() })
        };
        cx.use_audio_inputs(&[]);
    }

    fn pick_request(&mut self, cx: &mut Cx, request: &PlayFile, sim: Option<&SimHost>, bed: u64, project: Option<&Path>) -> Result<(), &'static str> {
        if request.bed != bed { return Err("The project changed before the WAV could play."); }
        let folder = project.and_then(Path::parent).ok_or("Save the project before playing a project WAV.")?;
        let folder = if folder.as_os_str().is_empty() { Path::new(".") } else { folder };
        let sim = sim.ok_or(NO_RUNNING_BOARD)?;
        let run = sim.live_run().ok_or(NO_RUNNING_BOARD)?;
        mic_rate(Some(sim))?;
        codec(sim)?;
        if self.capture.is_some() { return Err("The computer microphone is in use."); }
        let content = sim.content().ok_or(NO_RUNNING_BOARD)?.clone();
        self.pick(cx, folder.join(&request.file), None, Some(folder.to_owned()));
        self.requested = Some((request.clone(), run, content));
        Ok(())
    }

    fn pick(&mut self, cx: &mut Cx, path: PathBuf, bytes: Option<Arc<[u8]>>, folder: Option<PathBuf>) {
        if let Some((request, _, _)) = self.requested.take() {
            request.answer(cx, Err("Another WAV was picked before this one played.".into()));
        }
        Arc::make_mut(&mut self.shown).file = Some(path);
        self.bytes = bytes;
        self.folder = folder;
        self.invalidate();
    }

    fn invalidate(&mut self) {
        self.track = None;
        let shown = Arc::make_mut(&mut self.shown);
        shown.ready = false;
        self.dirty = shown.file.is_some();
        shown.status = if self.dirty { "Preparing WAV…".into() } else { String::new() };
    }

    fn convert(&mut self, cx: &mut Cx) {
        if let Some(rx) = &self.loading {
            let result = match rx.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err("WAV conversion stopped.".into()),
            };
            self.loading = None;
            if !self.dirty {
                let shown = Arc::make_mut(&mut self.shown);
                shown.ready = result.is_ok();
                match result {
                    Ok(track) => { self.track = Some(track); shown.status.clear(); }
                    Err(why) => shown.status = why,
                }
            }
        }
        if !self.dirty { return; }
        let (Some(path), Ok(rate)) = (&self.shown.file, self.shown.rate) else { return };
        let (path, bytes, db) = (path.clone(), self.bytes.clone(), self.shown.level_db);
        let folder = self.folder.clone();
        let (tx, rx) = to_ui_oneshot();
        self.dirty = false;
        match cx.task_pool().submit_named(Class::Background, "board microphone WAV", move || {
            let wav = match bytes.as_deref() {
                Some(bytes) => Ok(Cow::Borrowed(bytes)),
                None => read_wav(&path, folder.as_deref()).map(Cow::Owned),
            };
            let result = wav.and_then(|wav| sound_track(&wav, rate, f64::from(db), 0))
                .map_err(|why| format!("Cannot load {}: {why}", path.display()));
            let _ = tx.send(result);
        }) {
            Ok(task) => { task.detach(); self.loading = Some(rx); }
            Err(why) => Arc::make_mut(&mut self.shown).status = format!("Cannot start WAV conversion: {why}"),
        }
    }
}

fn read_wav(path: &Path, folder: Option<&Path>) -> Result<Vec<u8>, String> {
    let path = if let Some(folder) = folder {
        let root = folder.canonicalize().map_err(|e| e.to_string())?;
        let resolved = path.canonicalize().map_err(|e| e.to_string())?;
        if !resolved.starts_with(root) { return Err("The WAV is outside the project's folder.".into()); }
        resolved
    } else { path.to_owned() };
    std::fs::read(path).map_err(|e| e.to_string())
}

impl Capture {
    fn follow(&mut self, sim: Option<&mut SimHost>, rate: Result<u32, &'static str>, db: i32) -> Result<Option<String>, String> {
        let sim = sim.filter(|s| s.live_run().is_some() && s.content().is_some_and(|run| Arc::ptr_eq(run, &self.run)))
            .ok_or("Computer mic off. The board run ended.")?;
        if !capture_playing(sim) { return Err("Computer mic off. The board paused.".into()); }
        if rate != Ok(self.rate) { return Err("Computer mic off. The firmware microphone changed.".into()); }
        let hz = self.format.hz.load(Ordering::Acquire);
        if hz == FORMAT_CHANGED { return Err("Computer mic off. The input sample rate changed.".into()); }
        if hz == 0 { return Ok(None); }
        let front = sim.values().ok_or(NO_RUNNING_BOARD)?.t;
        if self.source.is_none() {
            let origin = (front.max(0.0) * 1e12) as u64;
            let source = MicInput::new(self.format.first.clone(), hz, self.rate, f64::from(db), origin, self.refdes.clone())?;
            let reader = Arc::new(TrackInput::new(source.input.track.clone(), self.refdes.clone()));
            if !sim.set_mic_input(source.clone(), reader) { return Err(NO_RUNNING_BOARD.into()); }
            if !Arc::ptr_eq(source.port(), &self.format.first) {
                assert!(self.format.larger.set(source.port().clone()).is_ok(), "the capture grows its ring once");
            }
            self.source = Some(source);
        }
        let source = self.source.as_ref().expect("the microphone source is prepared");
        // As with the IMU, attach the reader only after the first grant. Even a publication
        // that reads ahead of the analog front then finds the track covering its instant.
        if !self.attached && source.input.first_want().is_some_and(|want| front > want) {
            let track = TrackRef(source.input.track.clone());
            if !sim.command(&SimCmd::Board(BoardInput::Sound { comp: self.comp, track })) {
                return Err("The running board could not take the computer microphone.".into());
            }
            self.attached = true;
        }
        let status = source.status();
        let displayed = (self.attached, status.overflows, status.holds, (status.behind_s * 1e6).round() as u64);
        if self.last_status == Some(displayed) { return Ok(None); }
        self.last_status = Some(displayed);
        let label = if self.attached { "Computer mic on" } else { "Starting computer mic" };
        Ok(Some(capture_status(label, status)))
    }
}

fn capture_status(label: &str, status: MicStatus) -> String {
    format!("{label} · {} overflows · {} holds · {:.3} ms behind", status.overflows, status.holds, status.behind_s * 1000.0)
}

fn capture_playing(sim: &SimHost) -> bool {
    sim.playing() && sim.values().is_some_and(|v| v.running)
}

fn mic_rate(sim: Option<&SimHost>) -> Result<u32, &'static str> {
    let sim = sim.filter(|sim| sim.live_run().is_some()).ok_or(NO_RUNNING_BOARD)?;
    let debug = sim.values().and_then(|v| v.co.as_ref()).and_then(|co| co.get::<FwState>())
        .and_then(|state| state.debug.as_ref()).ok_or("No firmware is running on the board.")?;
    debug.mic_rate.ok_or("The running firmware has no mic declaration.")
}

fn codec(sim: &SimHost) -> Result<(CompRef, &str), &'static str> {
    sim.lowered().and_then(|l| l.firmware.first())
        .and_then(|binding| binding.bus.parts.iter().find(|p| matches!(p.kind, PartKind::Es8311 { .. })))
        .map(|codec| (codec.comp, codec.path.as_str())).ok_or("The board has no microphone codec.")
}

fn play(sim: &mut SimHost, recording: &SoundTrack) -> Result<(), &'static str> {
    let (comp, _) = codec(sim)?;
    let origin_ps = (sim.input_time().max(0.0) * 1e12) as u64;
    let track = TrackRef(Arc::new(recording.replay_at(origin_ps)));
    if !sim.command(&SimCmd::Board(BoardInput::Sound { comp, track })) {
        return Err("The running board could not take the microphone recording.");
    }
    Ok(())
}
