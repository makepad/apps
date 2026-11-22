//! Firmware follows the running lowering, committed document text and installed radio island.
//! Unchanged frames compare three Arc identities and each panel's generation, and drain completed
//! work. A changed input rebuilds one key and compiles on the pool. Each successful image makes
//! one maker for all workers. Each worker gets its own reset machine.
//!
//! The device's EEPROM is kept with the project (`fab_model::ElecMemory`, one
//! `<board>.<part>.eeprom` per device): a run from reset starts with the kept bytes, the nominal
//! run's writes go back (at most once a second while it runs, at once when it stops or the app
//! quits), and the person can erase them. A restart of the run from 0 within a session (a value
//! edit while Listen plays the recording) starts from the session's bytes again: it replays the
//! session, which would otherwise apply its own writes twice. Ensemble members, scrubs and
//! snapshots never write. Per frame: one Arc compare; a published change is compared once.

use crate::elec_screen::Screens;
use fab_elec_bench::{Lowered, SimCmd};
use fab_elec_fw::{compile, maker, new_chip_eeprom, start_with, Built, Chip, FwInput};
use fab_elec_mcu::Diagnostic;
use fab_elec_mcu_cosim::{eeprom, pin_rows, FwFrame, FwState, PinUse};
use fab_elec_mcu_emit_s3 as emit_s3;
use fab_elec_mcu_island::Island;
use fab_elec_simhost::{events::{EventFeed, EventSource}, host::CoSimMaker, rows::CoPinReader, SimHost};
use fab_elec_view::{live::{sim::{FwDiagnostic, FwLive}, PinLiveMode}, snapshot::DocsView, DesignSnapshot, ElecLive};
use fab_model::ElecMemory;
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, ToUIReceiver};
use crate::elec_flash::Flasher;
use crate::elec_net::{FwNet, NetContext};
use crate::elec_radio::RadioPack;
use fab_elec_bench::net::{Fake, NetEndpoint};
use makepad_widgets::{log, Cx, Event};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// At most this often a running chip's changed EEPROM goes to the project.
const MEMORY_EVERY: Duration = Duration::from_secs(1);

/// One compile to run off the UI thread: it compiles the source the spawn is given and hands the
/// result back to the runner, which drops it unless it is still the newest.
pub type FwJob = Box<dyn FnOnce() + Send>;

/// Where compiles run, given each job and the source it compiles: the app's task pool
/// (`FwRunner::default`). A test can keep one job and run it later itself, so a superseded result
/// arrives late on any machine. `Ok(None)`: taken, no handle.
pub type Spawn = Arc<dyn Fn(&mut Cx, &str, FwJob) -> Result<Option<TaskHandle<()>>, String> + Send + Sync>;

/// The app's spawn: the task pool's Visible class, whatever the source. A compile is what the
/// person waits to hear: a run waits for its first image, and an edit's run for the new one. As
/// Background it waited behind every other use of the cores (a first launch's shader pipeline
/// compiles: 0.6 s before it started, with every pool worker idle).
pub fn pool_spawn(cx: &mut Cx, _source: &str, job: FwJob) -> Result<Option<TaskHandle<()>>, String> {
    // The S3's native page units, which its images' machines compile, go to the same pool.
    fab_elec_fw::compile_units_on(cx.task_pool());
    cx.task_pool().submit_named(Class::Visible, "elec firmware compile", job)
        .map(Some).map_err(|error| error.to_string())
}

fn pin_reader() -> CoPinReader {
    Arc::new(|state, row| pin_rows(state, &mut |name, use_, level, duty| {
        let mode = match use_ {
            PinUse::Input => PinLiveMode::Input,
            PinUse::PullUp => PinLiveMode::PullUp,
            PinUse::Output => PinLiveMode::Output,
            PinUse::Pwm => PinLiveMode::Pwm,
            PinUse::Peripheral => PinLiveMode::Peripheral,
        };
        row(name, mode, level, duty);
    }))
}

fn same_arc<T>(a: Option<&Arc<T>>, b: Option<&Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn same_bytes(a: Option<&Arc<[u8]>>, b: Option<&Arc<[u8]>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

struct Done {
    seq: u64,
    /// The image and its run identity, both made on the pool.
    result: Result<(Built, u128), Vec<Diagnostic>>,
    /// A new chip's EEPROM for the image (`new_chip_eeprom`), made on the pool beside it.
    blank: Option<Arc<[u8]>>,
    /// What Flash to writes, for an image a board can take (an S3's flash from address zero).
    flash: Option<Arc<[u8]>>,
    /// A radio image's linked island, which Flash to writes at the island's own offset.
    island: Option<Arc<Vec<u8>>>,
}

/// The running image's maker as the runner keeps it: another start EEPROM wraps it again
/// (`start_with`), without compiling or making it again.
struct Image {
    maker: CoSimMaker,
    identity: u128,
    /// The EEPROM a new chip of its kind holds: a run's bytes equal to it need no file.
    blank: Option<Arc<[u8]>>,
    /// What Flash to writes (None: no board takes this image).
    flash: Option<Arc<[u8]>>,
    /// A radio image's linked island, written beside `flash`.
    island: Option<Arc<Vec<u8>>>,
}

/// One compile stream for the host's one co-simulated device; a new key supersedes old results.
/// The firmware's files: the assets in `firmware/<name>/` for the document `<name>.fw.splash`, by
/// their names in that folder, sharing the model's bytes. One range walk of the sorted assets.
fn firmware_files(assets: &fab_model::ElecAssets, doc: &str) -> Vec<(String, Arc<[u8]>)> {
    let file = doc.rsplit('/').next().unwrap_or(doc);
    let name = file.strip_suffix(".fw.splash").unwrap_or(file);
    let folder = format!("firmware/{name}/");
    assets.range(folder.clone()..).take_while(|(path, _)| path.starts_with(&folder))
        .map(|(path, bytes)| (path[folder.len()..].to_string(), bytes.clone())).collect()
}

/// The folders the bench's fakes read (a files fake's, a rules fake's files'): each
/// `firmware/<folder>/` with its assets by their names in it, sharing the model's bytes.
fn fake_files(assets: &fab_model::ElecAssets, nets: &[NetEndpoint]) -> Vec<(String, Vec<(String, Arc<[u8]>)>)> {
    let mut folders: Vec<&str> = nets.iter().flat_map(|e| e.fake.folders()).collect();
    folders.sort_unstable();
    folders.dedup();
    folders.into_iter().map(|folder| {
        let prefix = format!("firmware/{folder}/");
        let files = assets.range(prefix.clone()..).take_while(|(path, _)| path.starts_with(&prefix))
            .map(|(path, bytes)| (path[prefix.len()..].to_string(), bytes.clone())).collect();
        (folder.to_string(), files)
    }).collect()
}

/// A file a bench's rules fake sends that the project lacks, as the run's problem.
fn missing_fake_file(assets: &fab_model::ElecAssets, nets: &[NetEndpoint]) -> Option<String> {
    nets.iter().find_map(|e| match &e.fake {
        Fake::Rules(rules) => rules.iter().flat_map(|rule| &rule.send).filter_map(|send| send.body.file())
            .find(|file| !assets.contains_key(&format!("firmware/{file}")))
            .map(|file| format!("connection {}'s rules send firmware/{file}, which the project does not have", e.c)),
        _ => None,
    })
}

pub struct FwRunner {
    spawn: Spawn,
    lowered: Option<Arc<Lowered>>,
    docs: Option<Arc<DocsView>>,
    /// The linked pack seen by the last rebuild, shared with an S3 input.
    island: Option<Arc<Island>>,
    /// The project's electronics assets seen by the last rebuild: the firmware's files are its
    /// `firmware/<name>/` entries.
    assets: Arc<fab_model::ElecAssets>,
    input: Option<Arc<FwInput>>,
    seq: u64,
    task: Option<TaskHandle<()>>,
    done: ToUIReceiver<Done>,
    screens: Screens,
    retry: bool,
    pins_set: bool,
    live: Option<Arc<FwLive>>,
    /// Whether the nominal run's device reports every statement it begins as an event (for the
    /// code view's stepping and the scope's statement column); applies from the next image made.
    pub(crate) statements: bool,
    /// The feed of the sim host's firmware event log: one log per host, kept across images so
    /// its readers stay registered; a host without the log gets a new one.
    feed: Option<EventFeed<FwFrame>>,
    /// The running image (None: none built for the binding).
    image: Option<Image>,
    /// The bed's device memories (`set_memory`) with the running chip's writes on top.
    memory: Arc<ElecMemory>,
    /// The running device's memory file (`<board>.<part>.eeprom`); None: no device or board.
    key: Option<String>,
    /// The EEPROM the maker's devices start with (None: a new chip's).
    start: Option<Arc<[u8]>>,
    /// The nominal run's last published EEPROM, by identity.
    seen: Option<Arc<[u8]>>,
    /// A run of this session started: `start` holds until a Stop.
    ran: bool,
    /// `memory` holds writes the project was not sent yet; when the last went.
    unsent: bool,
    sent_at: Option<Instant>,
    /// The memories for the project, taken by the host (`take_memory`).
    outbox: Option<Arc<ElecMemory>>,
    /// Flash to: the boards the running image can go to, and a flash under way.
    flasher: Flasher,
    /// Created on the first host event with a Cx. The pack belongs to Fab across bed changes.
    radio: Option<RadioPack>,
    /// The real network the run may reach (`elec_net`), kept across bed changes like the pack.
    net: FwNet,
    /// The key of what the running maker's run reaches (`FwNet::key`; 0: nothing real).
    net_key: u128,
}

impl Default for FwRunner {
    fn default() -> Self {
        Self::with_spawn(Arc::new(pool_spawn))
    }
}

impl FwRunner {
    /// Back to no binding, no image and no status, keeping the spawn and the bed's device
    /// memories (a host restart; another bed brings its own, `set_memory`). Writes not sent yet
    /// stay in them and go with the next change, never to another bed.
    pub fn restart(&mut self) {
        let memory = std::mem::take(&mut self.memory);
        let radio = self.radio.take();
        let net = std::mem::take(&mut self.net);
        *self = Self::with_spawn(self.spawn.clone());
        self.memory = memory;
        self.radio = radio;
        self.net = net;
    }

    /// What a check of a change to firmware document `doc` compiles with before it is committed:
    /// the running binding's input (its chip, clock, constants, wiring and files), whose text the
    /// check replaces with the changed one. Err: why nothing here can build `doc` now, so a
    /// change of it goes in unchecked, as typing does. O(constants + files), `Arc` clones.
    pub fn check_input(&self, doc: &str) -> Result<FwInput, String> {
        if let Some(input) = self.input.as_deref().filter(|input| input.doc == doc) {
            return Ok(FwInput {
                part: input.part.clone(), doc: input.doc.clone(), rev: input.rev, text: input.text.clone(),
                f_clk: input.f_clk, params: input.params.clone(), chip: input.chip, wiring: input.wiring.clone(),
                island: input.island.clone(), files: input.files.clone(), net_files: input.net_files.clone(),
            });
        }
        Err(match self.live.as_deref() {
            Some(live) if live.doc == doc => format!("the bench's run cannot build it: {}", live.problem.as_deref().unwrap_or("its part has no chip to build for")),
            Some(live) => format!("the bench's run builds {} (for {}), not this document", live.doc, live.part),
            None => "no bench run has built firmware yet: it is compiled once a bench that runs it plays".into(),
        })
    }

    pub fn with_spawn(spawn: Spawn) -> Self {
        Self {
            spawn, lowered: None, docs: None, island: None, assets: Arc::default(), input: None, seq: 0, task: None,
            done: ToUIReceiver::default(), screens: Screens::default(), retry: false, pins_set: false, live: None, statements: false, feed: None,
            image: None, memory: Arc::default(), key: None, start: None, seen: None, ran: false, unsent: false,
            sent_at: None, outbox: None, flasher: Flasher::default(), radio: None, net: FwNet::default(), net_key: 0,
        }
    }

    /// The bed's device memories, as the project keeps them: a new bed's, after `restart`. The
    /// old bed's writes went to it before its switch (`ElecHost::sync_model`); any the chip made
    /// after the switch was asked are not kept, and the log says so.
    pub fn set_memory(&mut self, memory: Arc<ElecMemory>) {
        if self.unsent || self.outbox.is_some() {
            log!("electronics: EEPROM writes the chip made after the plate switch was asked are not kept");
        }
        self.memory = memory;
        self.unsent = false;
        self.outbox = None;
    }

    /// Writes wait to go to the project (the once-a-second send, or taken by no one yet).
    pub fn memory_pending(&self) -> bool {
        self.unsent || self.outbox.is_some()
    }

    /// The memories to put in the project, when the running chip changed them.
    pub fn take_memory(&mut self) -> Option<Arc<ElecMemory>> {
        self.outbox.take()
    }

    /// Every write not sent yet goes now (the app quits, or a job that can put another plate on
    /// the bed waits for them).
    pub fn flush_memory(&mut self) {
        if self.unsent {
            self.send_memory();
        }
    }

    fn send_memory(&mut self) {
        self.outbox = Some(self.memory.clone());
        self.unsent = false;
        self.sent_at = Some(Instant::now());
    }

    /// The bytes the project keeps for the running device.
    fn stored(&self) -> Option<Arc<[u8]>> {
        self.key.as_ref().and_then(|key| self.memory.get(key)).cloned()
    }

    /// The person erases the EEPROM kept for `part`: the project drops it, and the run starts
    /// again from reset as a new chip (paused if it was paused; after a Stop, the next run does).
    /// False: `part` is not the running device.
    pub fn erase(&mut self, cx: &mut Cx, sim: &mut SimHost, part: &str) -> bool {
        let Some(key) = self.key.clone().filter(|_| self.input.as_ref().is_some_and(|input| input.part == part)) else { return false };
        if Arc::make_mut(&mut self.memory).remove(&key).is_some() {
            self.send_memory();
        }
        self.start = None;
        self.seen = None;
        if self.ran && !sim.stopped() {
            let playing = sim.status().playing;
            sim.command(&SimCmd::Stop);
            self.ran = false;
            self.make(sim);
            sim.command(&SimCmd::Play);
            if !playing {
                sim.command(&SimCmd::Pause);
            }
        } else {
            self.make(sim);
        }
        if self.show_memory() && cx.has_global::<ElecLive>() {
            cx.get_global::<ElecLive>().sim.firmware = self.live.clone();
        }
        true
    }

    /// Once per event, for Flash to's list timer.
    pub fn flash_event(&mut self, cx: &mut Cx, event: &Event) {
        self.flasher.event(cx, event);
    }

    pub fn radio_event(&mut self, cx: &mut Cx, event: &Event) {
        let radio = self.radio.get_or_insert_with(|| RadioPack::new(cx));
        if radio.event(cx, event) {
            self.publish_radio(cx);
        }
    }

    pub fn radio_get(&mut self, cx: &mut Cx) {
        if self.live.as_ref().is_some_and(|live| live.radio.is_some()) {
            if let Some(radio) = &mut self.radio {
                radio.get(cx);
                self.publish_radio(cx);
            }
        }
    }

    pub fn radio_cancel(&self) {
        if self.live.as_ref().is_some_and(|live| live.radio.is_some()) {
            if let Some(radio) = &self.radio {
                radio.cancel();
            }
        }
    }

    fn publish_radio(&mut self, cx: &mut Cx) {
        if self.show_radio() && cx.has_global::<ElecLive>() {
            cx.get_global::<ElecLive>().sim.firmware = self.live.clone();
        }
    }

    fn show_radio(&mut self) -> bool {
        let Some(live) = self.live.as_mut() else { return false };
        let chip = self.lowered.as_ref().and_then(|lowered| lowered.firmware.first())
            .and_then(|binding| binding.chip.as_deref()).and_then(Chip::from_model);
        let radio = self.radio.as_ref().filter(|_| chip == Some(Chip::S3)).map(RadioPack::live);
        if live.radio.as_ref() == radio { return false }
        Arc::make_mut(live).radio = radio.cloned();
        true
    }

    /// The person chooses the board Flash to writes to.
    pub fn flash_choose(&mut self, cx: &mut Cx, path: &str) {
        self.flasher.choose(path);
        self.publish_flash(cx);
    }

    /// The person flashes the running image to the chosen board.
    pub fn flash(&mut self, cx: &mut Cx) {
        if let Some((flash, island)) = self.image.as_ref().and_then(|image| Some((image.flash.clone()?, image.island.clone()))) {
            self.flasher.flash(cx, &flash, island.as_ref());
            self.publish_flash(cx);
        }
    }

    /// The person reads the chosen board (its chip and whole flash's MD5), nothing written.
    pub fn flash_read(&mut self, cx: &mut Cx) {
        if self.image.as_ref().is_some_and(|image| image.flash.is_some()) {
            self.flasher.read(cx);
            self.publish_flash(cx);
        }
    }

    /// The person backs up the chosen board's whole flash, or cancels the back-up.
    pub fn backup(&mut self, cx: &mut Cx) {
        if self.image.as_ref().is_some_and(|image| image.flash.is_some()) {
            self.flasher.backup(cx);
            self.publish_flash(cx);
        }
    }

    /// The person starts or stops the monitor on the chosen board.
    pub fn monitor(&mut self, cx: &mut Cx) {
        if self.image.as_ref().is_some_and(|image| image.flash.is_some()) {
            self.flasher.monitor(cx);
            self.publish_flash(cx);
        }
    }

    fn publish_flash(&mut self, cx: &mut Cx) {
        if self.show_flash() && cx.has_global::<ElecLive>() {
            cx.get_global::<ElecLive>().sim.firmware = self.live.clone();
        }
    }

    /// The binding's status takes Flash to as the flasher shows it; true when that changed it.
    fn show_flash(&mut self) -> bool {
        let Some(live) = self.live.as_mut() else { return false };
        if live.flash.as_ref() == self.flasher.live() {
            return false;
        }
        Arc::make_mut(live).flash = self.flasher.live().cloned();
        true
    }

    /// Hands the run the image's maker, its devices starting with `start`, with its identity: what
    /// its run would reach on the real network is part of it, so a change of that starts it again.
    fn make(&mut self, sim: &mut SimHost) {
        self.net_key = self.net.key(self.endpoints());
        let Some(image) = &self.image else { return };
        let (mut maker, mut identity) = start_with(&image.maker, image.identity, self.start.clone());
        if self.net_key != 0 {
            let mut w = fab_elec_core::canon::Writer::hasher();
            w.u128(identity);
            w.u128(self.net_key);
            identity = u128::from_le_bytes(w.finish().0);
            // a maker of its own: the host compares makers before identities
            let made = maker.clone();
            maker = Arc::new(move |lowered, member| made(lowered, member));
        }
        sim.set_cosim(Some((maker, identity)));
    }

    /// The bench's connections of an S3 binding, whose image's network ports the twin's host end
    /// answers (none for another chip).
    fn endpoints(&self) -> &[NetEndpoint] {
        let binding = self.lowered.as_ref().and_then(|lowered| lowered.firmware.first());
        binding.filter(|binding| binding.chip.as_deref().and_then(Chip::from_model) == Some(Chip::S3))
            .map_or(&[], |binding| &binding.net[..])
    }

    /// This computer's choice for the project on the bed: a run whose reach changes starts again.
    pub fn set_network(&mut self, cx: &mut Cx, sim: Option<&mut SimHost>, ctx: NetContext) {
        if !self.net.set(ctx) {
            return;
        }
        if let Some(sim) = sim {
            self.follow_network(sim);
        }
        if self.show_network() && cx.has_global::<ElecLive>() {
            cx.get_global::<ElecLive>().sim.firmware = self.live.clone();
        }
    }

    /// When the twin last opened a real connection, once per new time (the project's last use).
    pub fn take_net_used(&mut self) -> Option<i64> {
        self.net.take_used()
    }

    fn follow_network(&mut self, sim: &mut SimHost) {
        if self.net.key(self.endpoints()) != self.net_key {
            self.make(sim);
        }
    }

    /// The binding's status takes the Network row; true when that changed it.
    fn show_network(&mut self) -> bool {
        let network = self.net.live(self.endpoints());
        let Some(live) = self.live.as_mut() else { return false };
        if live.network == network {
            return false;
        }
        Arc::make_mut(live).network = network;
        true
    }

    /// The Firmware pane's word on the kept bytes; true when it changed.
    fn show_memory(&mut self) -> bool {
        let kept = self.stored().map(|bytes| bytes.len());
        match self.live.as_mut() {
            Some(live) if live.eeprom != kept => {
                Arc::make_mut(live).eeprom = kept;
                true
            }
            _ => false,
        }
    }

    /// The running chip's EEPROM: the nominal run's newest bytes go to the project when they
    /// differ from what it keeps (at most once a second while it runs, at once when stopped), and
    /// after a Stop the next run starts with them. One Arc compare on a frame without a new
    /// publication of the bytes; a new one is compared byte for byte, once.
    fn follow_memory(&mut self, cx: &mut Cx, sim: &mut SimHost) -> bool {
        let stopped = sim.stopped();
        if sim.content().is_some() {
            self.ran = true;
        }
        if let Some(key) = &self.key {
            let published = sim.values().and_then(|values| values.co.as_ref()).and_then(|co| co.get::<FwState>())
                .and_then(|state| state.eeprom.as_ref());
            if let Some(bytes) = published.filter(|bytes| !same_bytes(Some(bytes), self.seen.as_ref())) {
                self.seen = Some(bytes.clone());
                let blank = self.image.as_ref().and_then(|image| image.blank.as_ref());
                if self.memory.get(key).or(blank).is_none_or(|held| held[..] != bytes[..]) {
                    let bytes = bytes.clone();
                    Arc::make_mut(&mut self.memory).insert(key.clone(), bytes);
                    self.unsent = true;
                }
            }
        }
        if self.unsent {
            if stopped || self.sent_at.is_none_or(|at| at.elapsed() >= MEMORY_EVERY) {
                self.send_memory();
            } else {
                cx.new_next_frame();
            }
        }
        // a Stop ends the session: the next run is one from reset, with the newest bytes
        if stopped {
            self.ran = false;
            let stored = self.stored();
            if !same_bytes(stored.as_ref(), self.start.as_ref()) {
                self.start = stored;
                self.make(sim);
            }
        }
        self.show_memory()
    }

    /// Called only for a made simulation, after its frame adopts any completed lowering.
    /// Unchanged firmware compares the lowering, documents and island by Arc identity.
    pub fn frame(&mut self, cx: &mut Cx, sim: &mut SimHost, snap: &DesignSnapshot, assets: &Arc<fab_model::ElecAssets>, can_run: bool) -> bool {
        if !self.pins_set {
            sim.set_co_pins(Some(pin_reader()));
            self.pins_set = true;
        }
        // Stop keeps the previous lowering for a replay. A failed choice has no bound firmware.
        let lowered = sim.lowered().filter(|_| can_run);
        let lowering_changed = !same_arc(self.lowered.as_ref(), lowered);
        let docs_changed = !same_arc(self.docs.as_ref(), snap.docs.as_ref());
        let island = self.radio.as_ref().and_then(RadioPack::island);
        let island_changed = !same_arc(self.island.as_ref(), island);
        let assets_changed = !Arc::ptr_eq(&self.assets, assets);
        let mut changed = false;
        if lowering_changed || docs_changed || island_changed || assets_changed {
            self.lowered = lowered.cloned();
            self.docs = snap.docs.clone();
            self.island = island.cloned();
            self.assets = assets.clone();
            changed = self.rebuild(sim, lowering_changed);
            self.follow_network(sim);
            changed |= self.show_network();
            // the device's memory file: its board (the run's scope) and its part
            let board = sim.scope_id().and_then(|id| snap.design.unit(id)).map(|unit| unit.name.as_str());
            let key = board.zip(self.input.as_ref()).and_then(|(board, input)| eeprom::file_name(board, &input.part).ok());
            if key != self.key {
                self.key = key;
                self.seen = None;
            }
        }
        if self.retry {
            changed |= self.submit(cx);
        }
        // Inspect the newest key before accepting even an already-queued compile result.
        while let Ok(done) = self.done.try_recv() {
            if done.seq != self.seq {
                // A newer commit superseded this compile: its image never attaches.
                if let Some(live) = self.live.as_mut() {
                    Arc::make_mut(live).superseded += 1;
                    changed = true;
                }
                continue;
            }
            let (Some(input), true) = (self.input.clone(), self.live.is_some()) else { continue };
            if let Some(task) = self.task.take() {
                task.detach();
            }
            match done.result {
                Ok((built, identity)) => {
                    let image_id = built.image_id();
                    let feed = match &self.feed {
                        Some(feed) if sim.events::<FwFrame>().is_some() => feed.clone(),
                        _ => self.feed.insert(sim.set_events::<FwFrame>(|frame| {
                            if frame.is_image() { EventSource::Image } else { EventSource::Trace }
                        })).clone(),
                    };
                    // within a session the start bytes hold; a new session takes the newest
                    if !self.ran || sim.stopped() {
                        self.start = self.stored();
                    }
                    let (maker, notes) = maker(&input, built, feed, self.statements, self.screens.sink(), Some(self.net.open(cx)));
                    self.image = Some(Image { maker, identity, blank: done.blank, flash: done.flash, island: done.island });
                    self.make(sim);
                    let live = Arc::make_mut(self.live.as_mut().expect("a binding's status"));
                    live.compiling_rev = None;
                    live.running_rev = Some(input.rev);
                    live.image_id = Some(image_id);
                    live.failed = None;
                    live.notes = notes;
                }
                Err(diags) => {
                    let live = Arc::make_mut(self.live.as_mut().expect("a binding's status"));
                    live.compiling_rev = None;
                    live.failed = Some((input.rev, diags.into_iter().map(|diag| FwDiagnostic {
                        span: diag.span, param: diag.param, message: diag.message,
                    }).collect()));
                }
            }
            changed = true;
        }
        changed |= self.follow_memory(cx, sim);
        let image = self.image.as_ref().and_then(|image| image.flash.as_ref()).filter(|_| self.live.is_some());
        self.flasher.frame(cx, image);
        changed |= self.show_flash();
        changed |= self.show_radio();
        if changed {
            cx.get_global::<ElecLive>().sim.firmware = self.live.clone();
        }
        changed |= self.screens.frame(cx);
        // a first run of this lowering waits for its process while one is being built
        let building = self.retry || self.live.as_ref().is_some_and(|live| live.compiling_rev.is_some());
        sim.cosim_building(self.lowered.as_ref(), building);
        if self.retry {
            cx.new_next_frame();
        }
        changed
    }

    fn rebuild(&mut self, sim: &mut SimHost, lowering_changed: bool) -> bool {
        let Some(binding) = self.lowered.as_ref().and_then(|l| l.firmware.first()) else {
            self.image = None;
            if self.live.take().is_some() {
                self.supersede(None);
                sim.set_cosim(None);
                self.screens.clear();
                return true;
            }
            return false;
        };
        let mut live = self.live.as_deref().cloned().unwrap_or_default();
        live.part = binding.part.clone();
        live.doc = binding.doc.clone();
        if lowering_changed {
            live.not_running = self.lowered.as_ref().into_iter().flat_map(|l| l.firmware.iter().skip(1))
                .map(|binding| binding.part.clone()).collect();
            live.speakers = binding.speakers().map(|part| (part.comp, part.path.clone())).collect();
        }
        let text = self.docs.as_ref().and_then(|docs| docs.texts.iter().find(|text| text.name == binding.doc));
        let chip = binding.chip.as_deref().and_then(Chip::from_model);
        let mut wiring = None;
        live.problem = if binding.f_clk.is_none() {
            Some("no clock".into())
        } else if text.is_none() {
            Some(format!("the firmware document `{}` is not in the project", binding.doc))
        } else if binding.chip.is_none() {
            Some("no chip model: the part data names none (`model`)".into())
        } else if chip.is_none() {
            Some(format!("the chip `{}` is not simulated (ATMEGA328P, ESP32-S3)", binding.chip.as_deref().unwrap_or_default()))
        } else if chip == Some(Chip::Avr) && !binding.bus.parts.is_empty() {
            Some("bus parts beside an AVR are not simulated".into())
        } else if chip == Some(Chip::S3) {
            match emit_s3::wiring(&binding.bus.board(0)) {
                Ok(derived) => { wiring = Some(derived); None }
                Err(error) => Some(error),
            }
        } else {
            None
        };
        if live.problem.is_none() {
            live.problem = missing_fake_file(&self.assets, &binding.net);
        }
        let input = if live.problem.is_none() { binding.f_clk.zip(text).zip(chip).map(|((f_clk, text), chip)| Arc::new(FwInput {
            part: binding.part.clone(), doc: binding.doc.clone(), rev: text.rev, text: text.text.clone(),
            f_clk, params: binding.params.clone(), chip, wiring,
            island: if chip == Chip::S3 { self.island.clone() } else { None },
            files: firmware_files(&self.assets, &binding.doc),
            net_files: fake_files(&self.assets, &binding.net),
        })) } else { None };
        let same = match (&self.input, &input) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.supersede(input);
            live.compiling_rev = None;
        }
        if self.live.as_deref() == Some(&live) {
            return false;
        }
        self.live = Some(Arc::new(live));
        true
    }

    fn supersede(&mut self, input: Option<Arc<FwInput>>) {
        // Invalidate queued results even when the binding now lacks a document or clock.
        self.seq += 1;
        if let Some(task) = self.task.take() {
            // A superseded compile's result is dropped by its seq; cancel spares the pool its work.
            task.cancel();
        }
        self.retry = input.is_some();
        self.input = input;
    }

    fn submit(&mut self, cx: &mut Cx) -> bool {
        let Some(input) = self.input.clone() else { return false };
        self.seq += 1;
        let seq = self.seq;
        let rev = input.rev;
        let tx = self.done.sender();
        let source = input.text.clone();
        let job: FwJob = Box::new(move || {
            let result = compile(&input).map(|built| {
                let identity = built.identity(&input.part);
                (built, identity)
            });
            let blank = result.as_ref().ok().and_then(|(built, _)| new_chip_eeprom(built));
            let (flash, island) = match &result {
                Ok((Built::S3(firmware), _)) => (
                    Some(Arc::from(firmware.flash.as_slice())),
                    firmware.radio.as_ref().map(|radio| radio.island.clone()),
                ),
                _ => (None, None),
            };
            let _ = tx.send(Done { seq, result, blank, flash, island });
        });
        match (self.spawn)(cx, &source, job) {
            Ok(task) => {
                self.task = task;
                self.retry = false;
                if let Some(live) = self.live.as_mut() {
                    Arc::make_mut(live).compiling_rev = Some(rev);
                }
                true
            }
            Err(error) => {
                log!("electronics firmware compile: {error}; retrying next frame");
                false
            }
        }
    }
}
