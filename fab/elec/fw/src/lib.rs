//! The firmware build for a lowered circuit's MCU and the co-simulated device made from an image.
//! The app keeps the compile pool, the keys and the UI state.

pub mod mic;
pub mod pictures;
pub mod radio;
use fab_elec_core::CompRef;
use fab_elec_mcu::convert::{sound, Codec, SoundOptions};
use fab_elec_mcu::Diagnostic;
use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_bench::net::{Body, Fake, NetEndpoint, RuleSpec};
use fab_elec_mcu_cosim::{CoMachine, Config, FwDebug, FwFrame, LogicMachine, NetSetup, Upstream};
use fab_elec_mcu_rt::net::{Condition, Endpoint, Outcome, Payload, Responder, Rule, Rules, Served, Then, UpstreamLog, World};
use std::collections::HashMap;
use fab_elec_mcu_emu::{Machine, Tier};
pub use fab_elec_mcu_rt::ports::PinRole;
use fab_elec_mcu_emit_s3::{BoardTime, Wiring, VOICEBOX};
use fab_elec_mcu_parts::{screen::ScreenReader, MotionSample, Netlist, SoundTrack, Track, TrackSample};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};
use fab_elec_sim::cosim::CoSimDevice;
use fab_elec_sim::input::InputSource;
use fab_elec_simhost::{events::EventFeed, host::CoSimMaker};
use makepad_widgets_core::log;
use makepad_widgets_core::makepad_platform::thread::{Class, TaskPool};
use std::sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc, Mutex};

/// The simulated chips selected by the part data's model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip { Avr, S3 }

impl Chip {
    pub fn from_model(model: &str) -> Option<Chip> {
        if model.eq_ignore_ascii_case("ATMEGA328P") { Some(Self::Avr) }
        else if model.eq_ignore_ascii_case("ESP32-S3") { Some(Self::S3) }
        else { None }
    }

    pub fn name(self) -> &'static str {
        match self { Self::Avr => "ATMEGA328P", Self::S3 => "ESP32-S3" }
    }
}

/// A compile key owns only shared source text, never a rewritten firmware document.
pub struct FwInput {
    pub part: String,
    pub doc: String,
    pub rev: u64,
    pub text: Arc<str>,
    pub f_clk: f64,
    pub params: Vec<(String, f64)>,
    pub chip: Chip,
    /// Circuit-derived wiring for the S3; None for an AVR.
    pub wiring: Option<Wiring>,
    /// The linked radio pack for an S3 (None: none installed, or an AVR). A build with it
    /// starts the voicebox station in the simulator.
    pub island: Option<Arc<fab_elec_mcu_island::Island>>,
    /// The firmware's files (the project's `firmware/<name>/` for `<name>.fw.splash`), by their
    /// names in that folder; an S3 image bundles its pictures.
    pub files: Vec<(String, Arc<[u8]>)>,
    /// The folders the bench's `files` fakes serve: each one's name under the project's
    /// `firmware/`, and its files by their names in it (an S3's twin serves their pictures).
    pub net_files: Vec<(String, Vec<(String, Arc<[u8]>)>)>,
}

impl FwInput {
    pub fn same(&self, other: &Self) -> bool {
        self.part == other.part && self.doc == other.doc && self.rev == other.rev
            && Arc::ptr_eq(&self.text, &other.text) && self.f_clk == other.f_clk
            && self.params == other.params
            && self.chip == other.chip && self.wiring == other.wiring
            && match (&self.island, &other.island) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
            && self.files.len() == other.files.len()
            && self.files.iter().zip(&other.files).all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y))
            && self.net_files.len() == other.net_files.len()
            && self.net_files.iter().zip(&other.net_files).all(|((a, x), (b, y))| a == b && x.len() == y.len()
                && x.iter().zip(y).all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y)))
    }
}

/// The image for the selected chip, with its debug table and compiler notes.
#[derive(Clone, Debug, PartialEq)]
pub enum Built {
    Avr(fab_elec_mcu_emit_avr::Firmware),
    S3(fab_elec_mcu_emit_s3::Firmware),
}

impl Built {
    pub fn image_id(&self) -> u32 {
        match self { Self::Avr(fw) => fw.image_id, Self::S3(fw) => fw.image_id }
    }

    /// The pins the firmware ever uses as analog, by the chip's pin names (Logic mode keeps their
    /// nets analog); every other pin of the part is a logic pin.
    pub fn analog(&self) -> &[String] {
        match self { Self::Avr(fw) => &fw.analog, Self::S3(fw) => &fw.analog }
    }

    /// Each pin the source declares, by the chip's pin name, and its role (`output`, `input` with
    /// its pull, `pwm`, `analog`).
    pub fn pins(&self) -> &[(String, PinRole)] {
        match self { Self::Avr(fw) => &fw.pins, Self::S3(fw) => &fw.pins }
    }

    /// The run's content key for this image on `part`. It hashes every image byte (an S3 flash
    /// is megabytes), so it is computed with the build, off the UI thread.
    pub fn identity(&self, part: &str) -> u128 {
        match self {
            Self::Avr(fw) => firmware_identity(part, &fw.words, u64::from(fw.f_clk)),
            Self::S3(fw) => s3_identity(part, &fw.flash, fw.radio.as_ref().map(|radio| radio.island.as_slice())),
        }
    }
}

/// The single boundary to the firmware compiler. The binding's constants are typed inputs, never
/// source edits; the chip's own pin table names the pins.
pub fn compile(input: &FwInput) -> Result<Built, Vec<Diagnostic>> {
    let consts: Vec<_> = input.params.iter().map(|(name, value)| (name.as_str(), *value)).collect();
    match input.chip {
        Chip::Avr => fab_elec_mcu_emit_avr::firmware(&input.text, input.f_clk, &consts).map(Built::Avr),
        Chip::S3 => {
            let f_clk = input.f_clk;
            // ESP32-S3 TRM pp.529-530, sections 7.2.3-7.2.4.1: the hardware reset CPU clock is
            // XTAL/2, 20 MHz with the documented 40 MHz XTAL. The ROM handoff is assumed below;
            // the image's runtime then moves the CPU to the PLL at emit_s3's CPU_HZ.
            if f_clk != 40_000_000.0 {
                return Err(vec![Diagnostic::new(None, format!("the S3 model runs from a 40 MHz crystal (its ROM hands over on XTAL/2); this part's is {f_clk} Hz"))]);
            }
            let wiring = input.wiring.as_ref().ok_or_else(|| vec![Diagnostic::new(None, "the S3 firmware input has no circuit wiring")])?;
            let handoff = s3_handoff(0);
            let time = BoardTime { rc_slow_hz: handoff.time.rc_slow_hz, wdt_delay_sel: handoff.time.wdt_delay_sel };
            let files: Vec<_> = input.files.iter().map(|(name, bytes)| (name.as_str(), &bytes[..])).collect();
            fab_elec_mcu_emit_s3::firmware(&input.text, &consts, wiring, time, input.island.as_deref(), &files).map(Built::S3)
        }
    }
}

/// The assumed ROM handoff used by the S3 runtime images. These are explicit assumptions,
/// not chip measurements: the `M` fields of `S3Profile` still need hardware measurements.
pub fn s3_handoff(seed: u64) -> S3Profile {
    S3Profile {
        flash_bytes: VOICEBOX.flash_bytes as usize,
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT,
        handoff_cpu: CpuClock::Xtal { div: 2 }, handoff_mapped_pages: 64,
        entry: 0x4200_0408, seed,
        time: TimeProfile {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false, mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000,
            usb_frame_phase_ticks: seed % 480_000,
            input_sync_delay_ticks: 0, handoff_digital_pads_unheld: true,
            handoff_out: [0; 2], handoff_enable: [0; 2], handoff_pin: [0; 45],
            handoff_io_mux: [1 << 12; 45], handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: 8, handoff_usb_int_ena: 0,
        },
    }
}

/// What the co-simulated process runs, for the run's content key: the image's words, its clock
/// and the part it runs on (the logic beside it is the lowering's, keyed with the circuit).
fn firmware_identity(part: &str, words: &[u16], f_clk: u64) -> u128 {
    let mut w = fab_elec_core::canon::Writer::hasher();
    w.str(part);
    w.u64(f_clk);
    w.count(words.len());
    for &word in words {
        w.u16(word);
    }
    u128::from_le_bytes(w.finish().0)
}

/// The EEPROM a new chip of the image's kind holds (None: its machine has none,
/// `Machine::eeprom_bytes`): a run's bytes equal to it need no file. One machine made and
/// dropped; called with the build, on the pool.
pub fn new_chip_eeprom(built: &Built) -> Option<Arc<[u8]>> {
    match built {
        Built::Avr(fw) => AvrMachine::new(&fw.words, u64::from(fw.f_clk)).eeprom_bytes(),
        Built::S3(_) => None,
    }
}

/// `maker`'s devices starting with `eeprom` in their non-volatile memory, with the run identity
/// for it, so a recording made with other stored bytes is never taken for this run's. None (a new
/// chip) is `maker` and `identity` as they are. The bytes go into that memory only
/// (`CoSim::set_nonvolatile`), never into the firmware's state; a device that does not take them
/// (it has no such memory, or one of another size) starts as a new chip, said once. Hashes the
/// bytes once; each device made copies them into its memory.
pub fn start_with(maker: &CoSimMaker, identity: u128, eeprom: Option<Arc<[u8]>>) -> (CoSimMaker, u128) {
    let Some(bytes) = eeprom else { return (maker.clone(), identity) };
    let mut w = fab_elec_core::canon::Writer::hasher();
    w.u128(identity);
    w.count(bytes.len());
    w.raw(&bytes);
    let identity = u128::from_le_bytes(w.finish().0);
    let made = maker.clone();
    let warned = AtomicBool::new(false);
    let maker: CoSimMaker = Arc::new(move |lowered, member| {
        let mut device = made(lowered, member)?;
        if !device.dev.set_nonvolatile(&bytes) && !warned.swap(true, Ordering::Relaxed) {
            log!("electronics firmware: the kept EEPROM ({} bytes) does not fit the chip; it starts as a new chip", bytes.len());
        }
        Some(device)
    });
    (maker, identity)
}

/// Replaces the host's panel readers with those of a newly built nominal device.
pub type ScreenSink = Arc<dyn Fn(Vec<(CompRef, ScreenReader)>) + Send + Sync>;

/// A closed recording for the board microphone, starting at `origin_ps` on the run clock.
/// The declared mic rate is in Hz and the input level is in dB relative to the WAV's amplitude.
pub fn sound_track(wav: &[u8], rate: u32, level_db: f64, origin_ps: u64) -> Result<SoundTrack, String> {
    let gain = mic_gain(level_db)?;
    let pcm = sound(wav, &SoundOptions { rate, codec: Codec::Pcm16 })?;
    let track = SoundTrack::new(origin_ps, (rate, 1));
    for bytes in pcm.bytes.chunks_exact(2) {
        let sample = i16::from_le_bytes([bytes[0], bytes[1]]);
        track.push(mic_sample(sample, gain));
    }
    track.close();
    Ok(track)
}

fn mic_gain(level_db: f64) -> Result<f64, String> {
    if !level_db.is_finite() {
        return Err("the microphone input level must be a finite number of dB".into());
    }
    Ok(10.0_f64.powf(level_db / 20.0))
}

fn mic_sample(sample: i16, gain: f64) -> i16 {
    // The float-to-integer cast saturates at the i16 limits.
    (f64::from(sample) * gain).round() as i16
}

/// A track as a live run's streamed input (`fab_elec_sim::input`): the run holds until the
/// track covers what the device would read; a closed track holds it no more. Each time the run
/// asks it to cover an instant, the samples its writer may write grow to cover it (`allowed`):
/// a writer led by that count (a Mechanics world on a led clock) writes what the run reads.
pub struct TrackInput<T = MotionSample> {
    pub track: Arc<Track<T>>,
    /// The part's reference designator, for the run's note.
    pub refdes: String,
    allowed: Arc<AtomicU64>,
    /// The first instant the run asked for, an f64's bits; 0 before the run asked.
    first: AtomicU64,
}

impl<T> TrackInput<T> {
    pub fn new(track: Arc<Track<T>>, refdes: String) -> Self {
        TrackInput { track, refdes, allowed: Arc::new(AtomicU64::new(0)), first: AtomicU64::new(0) }
    }

    /// How many samples the track's writer may have written: enough for every instant the run
    /// asked for. It only grows.
    pub fn allowed(&self) -> Arc<AtomicU64> {
        self.allowed.clone()
    }

    /// The first instant the run asked the track to cover (None: the run has not held for it yet).
    /// Every step the run takes from where it then stood is held for the track: an input applied
    /// once the run is past this instant reads within it.
    pub fn first_want(&self) -> Option<f64> {
        match self.first.load(Ordering::Acquire) {
            0 => None,
            bits => Some(f64::from_bits(bits)),
        }
    }
}

impl<T: TrackSample> InputSource for TrackInput<T> {
    fn horizon(&self) -> Option<f64> {
        (!self.track.is_closed()).then(|| self.track.horizon_s())
    }
    fn period(&self) -> f64 {
        self.track.period_s()
    }
    fn name(&self) -> String {
        format!("the {} track of {}", T::KIND, self.refdes)
    }
    fn ask(&self, want: f64) {
        self.allowed.fetch_max(self.track.len_to_cover(want), Ordering::AcqRel);
        // a want is past the run's reach plus a period, never 0.0, whose bits mean "not yet"
        let _ = self.first.compare_exchange(0, want.to_bits(), Ordering::AcqRel, Ordering::Relaxed);
    }
}

/// Opens the real network for a nominal run from its reset (member 0), given its part's bench
/// connections: the connections that reach their real servers, and the network that carries them.
/// None: every connection plays its bench fake. The app decides, by the person's settings when
/// the run starts; a test that plays the network itself passes its own.
pub type OpenNetwork = Arc<dyn Fn(&[NetEndpoint]) -> Option<(Vec<u8>, Upstream)> + Send + Sync>;

/// What the bench's fakes read from the project, built once per image so every run and fork
/// shares it: each served folder's pictures, and the bodies the rules fakes' sends carry, each read
/// or converted once.
pub struct NetFakes {
    files: Vec<(String, Vec<(String, Arc<[u8]>)>)>,
    served: Vec<(String, Served)>,
    bodies: Mutex<HashMap<Body, Payload>>,
}

impl NetFakes {
    /// Over the folders an image's bench fakes read ([`FwInput::net_files`]).
    pub fn new(files: &[(String, Vec<(String, Arc<[u8]>)>)]) -> NetFakes {
        let served = files.iter().map(|(folder, files)| (folder.clone(), Served(Arc::new(pictures::Pictures::new(files))))).collect();
        NetFakes { files: files.to_vec(), served, bodies: Mutex::new(HashMap::new()) }
    }

    fn responder(&self, fake: &Fake) -> Result<Responder, String> {
        Ok(match fake {
            Fake::Echo => Responder::Echo,
            Fake::Sink => Responder::Sink,
            Fake::Files(folder) => Responder::Service(self.served.iter().find(|(f, _)| f == folder).map(|(_, s)| s.clone())
                .unwrap_or_else(|| Served(Arc::new(pictures::Pictures::new(&[]))))),
            Fake::Rules(rules) => Responder::Rules(Arc::new(Rules(rules.iter().map(|rule| self.rule(rule)).collect::<Result<_, _>>()?))),
        })
    }

    fn rule(&self, rule: &RuleSpec) -> Result<Rule, String> {
        let then = rule.send.iter().map(|send| {
            let unit = send.body.unit();
            Ok(Then {
                after_ns: ns(send.after), binary: send.binary, head: send.head.clone(), body: self.body(&send.body)?,
                block: send.block * unit, every_ns: ns(send.every), unit,
            })
        }).collect::<Result<_, String>>()?;
        Ok(Rule { when: rule.when.clone(), stop: rule.stop, then })
    }

    /// A send's body: its file as it is (the model's own bytes), or a WAV as 16-bit samples.
    fn body(&self, body: &Body) -> Result<Payload, String> {
        let Some(path) = body.file() else { return Ok(Payload::from(&[][..])) };
        if let Some(done) = self.bodies.lock().unwrap_or_else(|e| e.into_inner()).get(body) {
            return Ok(done.clone());
        }
        let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
        let bytes = self.files.iter().find(|(f, _)| f == folder).and_then(|(_, files)| files.iter().find(|(n, _)| n == name))
            .map(|(_, bytes)| bytes.clone()).ok_or_else(|| format!("firmware/{path} is not in the project"))?;
        let done = match body {
            Body::Wav { rate, .. } => sound(&bytes, &SoundOptions { rate: *rate, codec: Codec::Pcm16 })
                .map(|pcm| Payload::from(pcm.bytes)).map_err(|why| format!("firmware/{path}: {why}"))?,
            _ => bytes,
        };
        self.bodies.lock().unwrap_or_else(|e| e.into_inner()).insert(body.clone(), done.clone());
        Ok(done)
    }
}

fn ns(seconds: f64) -> u64 { (seconds * 1e9).round() as u64 }

/// What an image's connections `0..slots` reach on the bench: each declared one's fake, or its real
/// server when `upstream` names it, with its latency, jitter and rate (the real network keeps its
/// own timing). A connection the bench does not declare reads as not configured. Fails when a
/// fake cannot be played: a rules fake's file missing or unreadable.
pub fn net_world(seed: u64, slots: usize, bench: &[NetEndpoint], upstream: &[u8], fakes: &NetFakes) -> Result<World, String> {
    let mut endpoints = Vec::with_capacity(slots);
    let mut timeline = Vec::new();
    for c in (0..slots).map_while(|c| u8::try_from(c).ok()) {
        let declared = bench.iter().find(|e| e.c == c);
        let responder = match declared {
            Some(_) if upstream.contains(&c) => Responder::Upstream,
            Some(e) => fakes.responder(&e.fake).map_err(|why| format!("connection {c}'s fake: {why}"))?,
            None => {
                timeline.push((0, Condition::Open { c, outcome: Outcome::NotConfigured }));
                Responder::Sink
            }
        };
        if let Some(e) = declared {
            if e.latency > 0.0 || e.jitter > 0.0 {
                timeline.push((0, Condition::Latency { c, ns: ns(e.latency), jitter_ns: ns(e.jitter) }));
            }
            if e.rate > 0.0 {
                timeline.push((0, Condition::Rate { c, bytes_per_s: e.rate.round() as u64 }));
            }
        }
        endpoints.push(Endpoint { responder });
    }
    Ok(World::new(seed, endpoints, timeline))
}

/// The nominal run's device (member 0) reports its events through `feed`, with every statement
/// begun when `statements`. An S3 image with network ports reaches its bench's fakes, and the
/// nominal run the real network `network` opens; ensemble members play the fakes only. Each member-0 device successfully made replaces the set handed to
/// `screens`, including an empty set for an AVR; ensemble members report neither events nor screens.
/// An S3 device's motion sensor takes a motion track as a timed input (`Stimulus::Track`).
pub fn maker(input: &FwInput, built: Built, feed: EventFeed<FwFrame>, statements: bool, screens: ScreenSink, network: Option<OpenNetwork>) -> (CoSimMaker, Vec<String>) {
    match built {
        Built::Avr(firmware) => avr_maker(input, firmware, feed, statements, screens),
        Built::S3(firmware) => s3_maker(input, firmware, feed, statements, screens, network),
    }
}

fn avr_maker(input: &FwInput, firmware: fab_elec_mcu_emit_avr::Firmware, feed: EventFeed<FwFrame>, statements: bool, screens: ScreenSink) -> (CoSimMaker, Vec<String>) {
    let fab_elec_mcu_emit_avr::Firmware { words, f_clk, debug, notes, .. } = firmware;
    let words: Arc<[u16]> = words.into();
    let debug = Arc::new(FwDebug { source_rev: input.rev, ..debug });
    let part = input.part.clone();
    let warned = AtomicBool::new(false);
    let maker: CoSimMaker = Arc::new(move |lowered, member| {
        // The adapter's own tests use 4000 ticks: this bounds replay work, not simulated time.
        let config = Config { stretch: 4000, debug: Some(debug.clone()) };
        let pins = lowered.cosim_pins(&part);
        let chip = AvrMachine::new(&words, u64::from(f_clk));
        let feed = (member == 0).then(|| feed.clone());
        // the logic parts beside the MCU, on its own machine: their digital nets never reach the circuit
        let logic = lowered.firmware.iter().find(|fw| fw.part == part).map(|fw| &fw.logic).filter(|side| !side.parts.is_empty());
        let device = match logic {
            None => CoMachine::new(chip, config, &pins).map(|machine| device(machine, feed, statements, pins)),
            Some(side) => LogicMachine::new(chip, side.plan())
                .and_then(|machine| CoMachine::new(machine, config, &pins))
                .map(|machine| device(machine, feed, statements, pins)),
        };
        match device {
            Ok(device) => {
                if member == 0 { screens(Vec::new()); }
                Some(device)
            }
            Err(error) => {
                if !warned.swap(true, Ordering::Relaxed) {
                    log!("electronics firmware {part}: {error}");
                }
                None
            }
        }
    });
    (maker, notes)
}

/// The S3 run's content key covers the part, chip, slot flash and any linked island bytes.
fn s3_identity(part: &str, flash: &[u8], island: Option<&[u8]>) -> u128 {
    let mut w = fab_elec_core::canon::Writer::hasher();
    w.str(part);
    w.str(Chip::S3.name());
    w.count(flash.len());
    w.raw(flash);
    if let Some(bytes) = island {
        w.count(bytes.len());
        w.raw(bytes);
    }
    u128::from_le_bytes(w.finish().0)
}

fn s3_maker(input: &FwInput, firmware: fab_elec_mcu_emit_s3::Firmware, feed: EventFeed<FwFrame>, statements: bool, screens: ScreenSink, network: Option<OpenNetwork>) -> (CoSimMaker, Vec<String>) {
    let fab_elec_mcu_emit_s3::Firmware { flash, radio, debug, mut notes, net_slots, .. } = firmware;
    let (flash, table) = match radio {
        Some(radio) => (Arc::new(radio.flash(&flash)), Some(radio.table())),
        None => (Arc::new(flash), None),
    };
    let debug = Arc::new(FwDebug { source_rev: input.rev, ..debug });
    let part = input.part.clone();
    let warned = AtomicBool::new(false);
    let fakes = NetFakes::new(&input.net_files);
    notes.push("the S3's ROM handoff is assumed, not measured: CPU on XTAL/2, 4 MiB of flash mapped, Direct Boot entry".into());
    let maker: CoSimMaker = Arc::new(move |lowered, member| {
        let pins = lowered.cosim_pins(&part);
        let device = (|| -> Result<CoSimDevice, String> {
            let binding = lowered.firmware.iter().find(|fw| fw.part == part)
                .ok_or_else(|| "the S3 part has no firmware binding".to_string())?;
            let mut netlist = Netlist::new(binding.bus.board(member as u64))?;
            let readers = (member == 0).then(|| {
                // BusSide::board maps its parts in order, so a port index names the same part here.
                netlist.ports().screens.into_iter()
                    .map(|(index, reader)| (binding.bus.parts[usize::from(index)].comp, reader)).collect()
            });
            let mut machine = s3_machine(flash.clone(), member as u64, netlist)?;
            if let Some(table) = &table {
                machine = machine.with_foreign(table.clone());
            }
            // The AVR's replay bound, 4000 CPU cycles, at the S3 handoff's 20 MHz
            // on the S3 model's 480 MHz tick grid.
            let config = Config { stretch: 96_000, debug: Some(debug.clone()) };
            let mut machine = CoMachine::new(machine, config, &pins)?;
            if !net_slots.is_empty() {
                // a run from reset: its own log, shared by its copies
                let (upstream, network) = match (&network, member) {
                    (Some(open), 0) => open(&binding.net).map_or((Vec::new(), None), |(upstream, network)| (upstream, Some(network))),
                    _ => (Vec::new(), None),
                };
                let world = net_world(member as u64, net_slots.len(), &binding.net, &upstream, &fakes)?;
                let setup = NetSetup { world, slots: net_slots.clone(), log: Arc::new(Mutex::new(UpstreamLog::new())) };
                machine = machine.with_net(setup, network);
            }
            let feed = (member == 0).then(|| feed.clone());
            let device = device(machine, feed, statements, pins);
            if let Some(readers) = readers { screens(readers); }
            Ok(device)
        })();
        match device {
            Ok(device) => Some(device),
            Err(error) => {
                if !warned.swap(true, Ordering::Relaxed) {
                    log!("electronics firmware {part}: {error}");
                }
                None
            }
        }
    });
    (maker, notes)
}

/// The S3 of a co-simulation member, as the app runs it: its native page units on (they compile
/// off the sim thread, where `compile_units_on` puts them, and the decoded lane runs each page
/// until its unit is there).
pub fn s3_machine(flash: Arc<Vec<u8>>, member: u64, netlist: Netlist) -> Result<S3Machine<Netlist>, String> {
    let mut machine = S3Machine::with_board(flash, &s3_handoff(member), netlist)?;
    machine.set_tier(Tier::Native);
    Ok(machine)
}

/// The S3's native page units compile on `pool`, as background work: never on the sim or the UI
/// thread. For the process, from its first call.
pub fn compile_units_on(pool: TaskPool) {
    if fab_elec_mcu_emu::jit::compiles_on_spawn() { return; }
    fab_elec_mcu_emu::jit::compile_on(Arc::new(move |job| match pool.submit_named(Class::Background, "mcu unit compile", job) {
        Ok(task) => task.detach(),
        Err(error) => log!("electronics firmware: an S3 page unit compile was refused ({error}); the page stays decoded"),
    }));
}

/// The co-simulated device of a machine; the nominal run's (`feed`) reports its events.
fn device<M: Machine>(mut machine: CoMachine<M>, feed: Option<EventFeed<FwFrame>>, statements: bool, pins: Vec<fab_elec_sim::cosim::CoSimPin>) -> CoSimDevice {
    if let Some(feed) = feed {
        machine.set_events(Some(Box::new(move |void, batch| feed.offer_voiding(void, batch))), statements);
    }
    CoSimDevice { dev: Box::new(machine), pins }
}
