//! The design's ids onto live values (the ESIM seam). A lowering numbers the nets, component pins
//! and components of its scope; `ValueIndex` keeps that numbering by `NetRef` / `PinRef` /
//! `CompRef`, beside the kernel's `ValuePlan` of the same generation, so a view reads
//! `LiveValues` by design id and a command by `CompRef` reaches the kernel device it tunes.
//!
//! Value lookups are dense tables by board, component and net (no hashing per read): a view resolves its
//! ids once per generation or reads them per frame at the cost of two index loads.

use crate::input::Taper;
use fab_elec_core::ids::{BoardId, CompId, CompRef, NetRef, PinRef};
use fab_elec_mcu_parts::{PartKind, Stimulus, TrackRef};
use fab_elec_sim::cosim::StepTo;
use fab_elec_sim::live::{DeviceInput, LiveCmd, LiveValues};
use std::sync::Arc;

/// Continuous's ring by default, simulated seconds (user, 11-18: a looping buffer that records the signals).
pub const RING_SECONDS: f64 = 10.0;

/// How a run steps (the project's `sim` statement's mode, the transport's Accuracy): Exact resolves
/// every edge and tail; Fast lets the time resolution follow the finest view; Logic is Fast with
/// each threshold inverter whose input is a plain RC run as a timing cell (`Lowered::with_cells`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SimMode {
    Exact,
    #[default]
    Fast,
    Logic,
}

/// A host input to the simulated board's parts, by design id.
#[derive(Clone, Debug, PartialEq)]
pub enum BoardInput {
    /// A finger on a panel, in the panel's frame memory texels (`LiveScreen`'s mapping); a move
    /// is another down at new coordinates, a release is `down: false`.
    Touch { comp: CompRef, x: u16, y: u16, down: bool },
    /// The board's pose, in the IMU's own units and axes: micro-g and milli-degrees per second.
    /// It ends a track the IMU read (the newest input wins), as a track replaces it.
    Motion { comp: CompRef, accel_ug: [i32; 3], gyro_mdps: [i32; 3] },
    /// The IMU reads the board's motion from this track from where the run's time is when it
    /// applies (a Mechanics world writes it; `Stimulus::Track`). The run holds for the track only
    /// once its source is among the run's inputs (`SimHost::set_inputs`), which comes first.
    Track { comp: CompRef, track: TrackRef },
    /// The codec reads mono PCM from this track at the run's time. A new track starts playback
    /// at its own origin, and the ADC reads silence past the end of a closed recording.
    Sound { comp: CompRef, track: TrackRef<i16> },
}

/// A command to a running bench, by design id. The host turns it into the kernel's `LiveCmd`
/// with `ValueIndex::command` (touches through `Touches::command`) and sends it without blocking.
#[derive(Clone, Debug, PartialEq)]
pub enum SimCmd {
    Play,
    Pause,
    /// Advance this many simulated seconds, then pause.
    Step(f64),
    /// The component's tunable value (a resistance, a capacitance, a source level, a pot's
    /// wiper place from 0 to 1).
    SetParam { comp: CompRef, value: f64 },
    /// Simulated seconds per real second; infinite runs as fast as the solver can.
    SetRate(f64),
    /// Exact (every edge and tail resolved), Fast (the time resolution follows the finest view) or
    /// Logic (Fast with the timing cells): the host's setting (`SimHost::set_sim_mode`), which it
    /// turns into `LiveCmd::SetRes` and, for Logic and back, a lowering it hands the run over to.
    SetSimMode(SimMode),
    /// Continuous runs on and records a rolling window. Loop renders a finite take and loops it.
    SetContinuous(bool),
    /// Realtime: the open run heard as it is made, with only the device's cushion; it implies Continuous.
    SetRealtime(bool),
    /// Continuous's ring, simulated seconds: how much of the newest record is kept.
    SetRing(f64),
    /// The user's sample rate, Hz (None: the host's choice; `SimHost::set_sample_rate`): a run at
    /// another rate starts over at it.
    SetSampleRate(Option<f64>),
    /// The user's recording rate, Hz (None: the default audio rate;
    /// `SimHost::set_recording_rate`): a run recording at another rate starts over.
    SetRecordingRate(Option<f64>),
    /// What the board's output jacks drive (None: a line input; `SimHost::set_jack_load`): a run
    /// with jacks starts over with it.
    SetJackLoad(Option<crate::audio::AudioLoad>),
    /// One output jack's load by reference designator (None: it takes the default again).
    SetJackLoadOf(String, Option<crate::audio::AudioLoad>),
    /// The person's take length, simulated seconds, in place of the bench's `stop` (None: the
    /// bench's; `SimHost::set_take_user`): a longer take renders on, a shorter one keeps what is
    /// rendered.
    SetTakeLen(Option<f64>),
    /// Auto re-record (on by default; `SimHost::set_auto_rerecord`): an edit or a press while the
    /// recording plays goes in where it is heard and the take renders again from there. Off, it
    /// goes in at the run's front, the take plays on as it was, and the host counts it as waiting
    /// for the next record (`SimStatus::waiting`).
    SetAutoRerecord(bool),
    /// Runs the firmware to its next statement, handler entry or handler exit, then pauses
    /// (`LiveCmd::CoStep`: unpaced; refused without a device that can step so).
    StepFw(StepTo),
    Board(BoardInput),
    /// The run ends and its state goes (the bench powered off): the next Play starts it from 0.
    Stop,
    /// The transport's Stop: what is heard goes silent and holds where it is, the render stops,
    /// and everything stays (the run, its take, its record); the next Play goes on from there.
    Hold,
    /// The play head to the run's time `t` (`|<`, a scrub let go), moving or held as the transport
    /// is: a take played back with nobody listening moves its clock there (`SimHost`'s silent
    /// playback); with a listener the views seek the player, and the host does nothing.
    Seek(f64),
}

impl SimCmd {
    /// A live press (`down`) or release of a momentary input (`ValueIndex::momentary`): its press
    /// source at 1 or 0, with no document edit.
    pub fn press(comp: CompRef, down: bool) -> SimCmd {
        SimCmd::SetParam { comp, value: if down { 1.0 } else { 0.0 } }
    }
}

const NONE: u32 = u32::MAX;

/// What a component's `SetParam` value sets in the kernel.
#[derive(Clone, Debug, PartialEq)]
pub enum Tune {
    /// The value as given, on one device's key (a resistor's `value`, a press source's `dc`).
    Device { device: String, key: String },
    /// A pot: the value is the wiper's place (0 at A, 1 at B); its two halves, `a` (A to W) and
    /// `b` (W to B), each take their resistance from it (`Taper::halves`).
    Pot { a: String, b: String, ohms: f64, taper: Taper, end: f64 },
    /// A key of a key matrix in Logic mode, by its name in the logic plan: 1 presses it, 0
    /// releases it, as the firmware machine's host input (`KeyPress`), not a kernel value.
    Key { key: String },
}

impl Tune {
    /// A tune of one device's key.
    pub fn device(device: impl Into<String>, key: impl Into<String>) -> Tune {
        Tune::Device { device: device.into(), key: key.into() }
    }
}

/// The empty numbering's generation, which no lowering has (a design's generations count up from
/// 0): a view that caches a lookup by generation redoes one made before the first lowering.
pub const EMPTY_GENERATION: u64 = u64::MAX;

/// One lowering's numbering of its scope, by design id. The default is the empty numbering, of
/// `EMPTY_GENERATION`.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueIndex {
    /// The generation of the `ValuePlan` and `LiveValues` this numbering belongs to.
    pub generation: u64,
    /// `[board][net]` -> net index.
    net: Vec<Vec<u32>>,
    /// `[board][comp]` -> (component index, first pin index, pin count).
    comp: Vec<Vec<(u32, u32, u16)>>,
    nets: u32,
    comps: u32,
    pins: u32,
    /// By component index: what its value sets, when it has one.
    tune: Vec<Option<Tune>>,
    /// The components a person presses (buttons), in lowering order: `SetParam` 1 presses one, 0
    /// releases it.
    momentary: Vec<CompRef>,
    /// The components a person turns (pots) and where each starts, in lowering order: `SetParam`
    /// with a place from 0 to 1 turns one.
    knobs: Vec<(CompRef, f64)>,
    /// Panels, their glass windows and the first firmware binding's touch controller, if any.
    panels: Vec<(CompRef, [u16; 4], Option<u16>)>,
    /// IMUs and their part indices in the first firmware binding's board.
    motion: Vec<(CompRef, u16)>,
    /// Audio codecs and their part indices in the first firmware binding's board.
    sound: Vec<(CompRef, u16)>,
}

/// The host's fingers, one per panel. A finger stays down across a new lowering (a value edit
/// keeps the firmware running), so its moves and release go out under the newest generation.
#[derive(Default)]
pub struct Touches {
    fingers: Vec<CompRef>,
}

impl Touches {
    /// Maps frame memory texels into glass pixels, ending a finger that leaves the glass.
    /// Downs and ups keep their order; moves may replace a waiting move on the host. A panel
    /// with no touch target in this lowering takes no input and holds no finger.
    pub fn command(&mut self, index: &ValueIndex, comp: CompRef, x: u16, y: u16, down: bool) -> Option<LiveCmd> {
        let finger = self.fingers.iter().position(|&panel| panel == comp);
        let Some((part, [gx, gy, gw, gh])) = index.touch_target(comp).filter(|&(_, [_, _, w, h])| w > 0 && h > 0) else {
            if let Some(i) = finger { self.fingers.swap_remove(i); }
            return None;
        };
        let (x, y, gx, gy) = (u32::from(x), u32::from(y), u32::from(gx), u32::from(gy));
        let (right, bottom) = (gx + u32::from(gw), gy + u32::from(gh));
        let inside = gx <= x && x < right && gy <= y && y < bottom;
        let down = down && inside;
        if !down && finger.is_none() { return None; }
        let replaceable = down && finger.is_some();
        match (down, finger) {
            (true, None) => self.fingers.push(comp),
            (false, Some(i)) => { self.fingers.swap_remove(i); }
            _ => {}
        }
        let x = (x.clamp(gx, right - 1) - gx) as u16;
        let y = (y.clamp(gy, bottom - 1) - gy) as u16;
        Some(LiveCmd::Input {
            generation: index.generation, key: format!("touch {part}"),
            input: DeviceInput(Arc::new(Stimulus::Touch { part, x, y, down })), replaceable,
        })
    }

    /// Forgets every finger.
    pub fn clear(&mut self) {
        self.fingers.clear();
    }
}

fn slot<T: Clone>(v: &mut Vec<Vec<T>>, a: usize, b: usize, empty: T) -> &mut T {
    if v.len() <= a {
        v.resize(a + 1, Vec::new());
    }
    let row = &mut v[a];
    if row.len() <= b {
        row.resize(b + 1, empty);
    }
    &mut row[b]
}

impl Default for ValueIndex {
    fn default() -> ValueIndex {
        ValueIndex::new(EMPTY_GENERATION)
    }
}

impl ValueIndex {
    pub fn new(generation: u64) -> ValueIndex {
        ValueIndex {
            generation, net: Vec::new(), comp: Vec::new(), nets: 0, comps: 0, pins: 0, tune: Vec::new(),
            momentary: Vec::new(), knobs: Vec::new(), panels: Vec::new(), motion: Vec::new(), sound: Vec::new(),
        }
    }

    /// Installs the input targets of the firmware binding the run uses.
    pub(crate) fn board_inputs(&mut self, bus: &crate::bus::BusSide) {
        let touch = bus.parts.iter().position(|part| matches!(part.kind, PartKind::Cst820 { .. }))
            .map(|i| u16::try_from(i).expect("board part indices fit in u16"));
        for (i, part) in bus.parts.iter().enumerate() {
            match part.kind {
                PartKind::Co5300 { glass } => self.panels.push((part.comp, glass, touch)),
                PartKind::Qmi8658c { .. } => {
                    self.motion.push((part.comp, u16::try_from(i).expect("board part indices fit in u16")));
                }
                PartKind::Es8311 { .. } => {
                    self.sound.push((part.comp, u16::try_from(i).expect("board part indices fit in u16")));
                }
                _ => {}
            }
        }
    }

    /// A panel's touch controller part index and glass window in frame memory texels.
    pub fn touch_target(&self, comp: CompRef) -> Option<(u16, [u16; 4])> {
        let &(_, glass, part) = self.panels.iter().find(|&&(panel, _, _)| panel == comp)?;
        Some((part?, glass))
    }

    /// An IMU's part index in the run's board.
    pub fn motion_target(&self, comp: CompRef) -> Option<u16> {
        self.motion.iter().find_map(|&(imu, part)| (imu == comp).then_some(part))
    }

    /// An audio codec's part index in the run's board.
    pub fn sound_target(&self, comp: CompRef) -> Option<u16> {
        self.sound.iter().find_map(|&(codec, part)| (codec == comp).then_some(part))
    }

    /// Marks a numbered component as one a person presses (a button).
    pub fn add_momentary(&mut self, r: CompRef) {
        if !self.momentary.contains(&r) {
            self.momentary.push(r);
        }
    }

    /// The components a person presses, in lowering order: `SetParam` 1 presses one, 0 releases it.
    pub fn momentary(&self) -> &[CompRef] {
        &self.momentary
    }

    /// Logic mode's keys (`LogicMode::keys`, named by their press sources): a component whose
    /// value sets one of those sources' `dc` presses that key instead. O(components x keys).
    pub fn logic_keys(&mut self, keys: &[String]) {
        for tune in self.tune.iter_mut().flatten() {
            if let Tune::Device { device, key } = tune {
                if key == "dc" && keys.contains(device) {
                    *tune = Tune::Key { key: device.clone() };
                }
            }
        }
    }

    /// Marks a numbered component as one a person turns (a pot), its wiper starting at `pos`.
    pub fn add_knob(&mut self, r: CompRef, pos: f64) {
        if !self.knobs.iter().any(|(k, _)| *k == r) {
            self.knobs.push((r, pos));
        }
    }

    /// The components a person turns and where each starts (0 to 1), in lowering order:
    /// `SetParam` with a place turns one.
    pub fn knobs(&self) -> &[(CompRef, f64)] {
        &self.knobs
    }

    /// Numbers a net (the next net index); a net added twice keeps its first index.
    pub fn add_net(&mut self, r: NetRef) -> usize {
        let next = self.nets;
        let s = slot(&mut self.net, r.board.ix(), r.net.ix(), NONE);
        if *s == NONE {
            *s = next;
            self.nets += 1;
        }
        *s as usize
    }

    /// Numbers a component and its `pins` pins (the next component index and a run of pin
    /// indices), with what its value sets, if anything. Returns (component index, first pin
    /// index); a component added twice keeps its first numbering.
    pub fn add_comp(&mut self, r: CompRef, pins: u16, tune: Option<Tune>) -> (usize, usize) {
        let (next, first) = (self.comps, self.pins);
        let s = slot(&mut self.comp, r.board.ix(), r.comp.ix(), (NONE, NONE, 0));
        if s.0 == NONE {
            *s = (next, first, pins);
            self.comps += 1;
            self.pins += pins as u32;
            self.tune.push(tune);
        }
        (s.0 as usize, s.1 as usize)
    }

    /// Counts, which a `ValuePlan` of the same generation matches.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.nets as usize, self.pins as usize, self.comps as usize)
    }

    pub fn net(&self, r: NetRef) -> Option<usize> {
        let i = *self.net.get(r.board.ix())?.get(r.net.ix())?;
        (i != NONE).then_some(i as usize)
    }

    pub fn comp(&self, r: CompRef) -> Option<usize> {
        let (i, _, _) = *self.comp.get(r.board.ix())?.get(r.comp.ix())?;
        (i != NONE).then_some(i as usize)
    }

    /// The component at a component index (a scan: for reads like `elec.sim.leds`, not per frame).
    pub fn comp_at(&self, index: usize) -> Option<CompRef> {
        self.comp.iter().enumerate().find_map(|(board, comps)| {
            let comp = comps.iter().position(|&(i, _, _)| i != NONE && i as usize == index)?;
            Some(CompRef { board: BoardId(board as u16), comp: CompId(comp as u32) })
        })
    }

    pub fn pin(&self, r: PinRef) -> Option<usize> {
        let (i, first, n) = *self.comp.get(r.board.ix())?.get(r.comp.ix())?;
        (i != NONE && r.pin.0 < n).then_some(first as usize + r.pin.ix())
    }

    /// A net's voltage in `lv`, if `lv` is of this generation and the net is in the scope.
    pub fn v(&self, lv: &LiveValues, r: NetRef) -> Option<f32> {
        self.read(lv, &lv.net_v, self.net(r))
    }

    /// The current into a component pin (positive flows into the pin).
    pub fn i(&self, lv: &LiveValues, r: PinRef) -> Option<f32> {
        self.read(lv, &lv.branch_i, self.pin(r))
    }

    /// A component's dissipation.
    pub fn p(&self, lv: &LiveValues, r: CompRef) -> Option<f32> {
        self.read(lv, &lv.part_p, self.comp(r))
    }

    fn read(&self, lv: &LiveValues, v: &[f32], i: Option<usize>) -> Option<f32> {
        if lv.generation != self.generation {
            return None;
        }
        v.get(i?).copied()
    }

    /// The kernel commands for a bench command, in order: one for most, both halves of a pot
    /// for its `SetParam`. Empty for a `SetParam` on a component with no tunable value in this
    /// lowering (the caller reports it: the value has no simulation effect) and for the host's
    /// own run mode, ring, accuracy, sample rate, recording rate and jack load. Board motion and
    /// sound need an IMU and a codec respectively. Board touches go through `Touches::command`,
    /// which keeps the finger, and give nothing here.
    pub fn command(&self, cmd: &SimCmd) -> Vec<LiveCmd> {
        let one = match cmd {
            SimCmd::Play => LiveCmd::Play,
            SimCmd::Pause => LiveCmd::Pause,
            SimCmd::Step(dt) => LiveCmd::Step(*dt),
            SimCmd::SetRate(r) => LiveCmd::SetRate(*r),
            SimCmd::SetContinuous(_) | SimCmd::SetRealtime(_) | SimCmd::SetRing(_) | SimCmd::SetSimMode(_) | SimCmd::SetSampleRate(_)
            | SimCmd::SetRecordingRate(_) | SimCmd::SetJackLoad(_) | SimCmd::SetJackLoadOf(_, _) | SimCmd::SetTakeLen(_) | SimCmd::SetAutoRerecord(_) | SimCmd::Seek(_) => return Vec::new(),
            SimCmd::StepFw(to) => LiveCmd::CoStep(*to),
            SimCmd::Stop => LiveCmd::Stop,
            SimCmd::Hold => LiveCmd::Pause,
            SimCmd::Board(BoardInput::Touch { .. }) => return Vec::new(),
            SimCmd::Board(BoardInput::Motion { comp, accel_ug, gyro_mdps }) => {
                let Some(part) = self.motion_target(*comp) else { return Vec::new() };
                LiveCmd::Input {
                    generation: self.generation, key: format!("motion {part}"),
                    input: DeviceInput(Arc::new(Stimulus::Motion { part, accel_ug: *accel_ug, gyro_mdps: *gyro_mdps })),
                    replaceable: true,
                }
            }
            SimCmd::Board(BoardInput::Track { comp, track }) => {
                let Some(part) = self.motion_target(*comp) else { return Vec::new() };
                LiveCmd::Input {
                    generation: self.generation, key: format!("track {part}"),
                    input: DeviceInput(Arc::new(Stimulus::Track { part, track: track.clone() })),
                    replaceable: false,
                }
            }
            SimCmd::Board(BoardInput::Sound { comp, track }) => {
                let Some(part) = self.sound_target(*comp) else { return Vec::new() };
                LiveCmd::Input {
                    generation: self.generation, key: format!("sound {part}"),
                    input: DeviceInput(Arc::new(Stimulus::Sound { part, track: track.clone() })),
                    replaceable: false,
                }
            }
            SimCmd::SetParam { comp, value } => {
                let Some(Some(tune)) = self.comp(*comp).and_then(|i| self.tune.get(i)) else { return Vec::new() };
                let generation = self.generation;
                return match tune {
                    Tune::Device { device, key } => vec![
                        LiveCmd::SetParam { generation, device: device.clone(), key: key.clone(), value: *value },
                    ],
                    Tune::Key { key } => vec![LiveCmd::Input {
                        generation, key: format!("key {key}"),
                        input: DeviceInput(Arc::new(fab_elec_mcu_cosim::KeyPress { key: key.clone(), down: *value > 0.5 })),
                        replaceable: false,
                    }],
                    Tune::Pot { a, b, ohms, taper, end } => {
                        let (ra, rb) = taper.halves(*ohms, *value, *end);
                        [(a, ra), (b, rb)].into_iter().map(|(device, value)| LiveCmd::SetParam {
                            generation, device: device.clone(), key: "value".to_owned(), value,
                        }).collect()
                    }
                };
            }
        };
        vec![one]
    }

    /// The subscription (`LiveCmd::Subscribe`) for these pins and components: the visible ones,
    /// whose currents and dissipation get interval means. Ids not in this lowering are skipped.
    pub fn subscribe(&self, pins: &[PinRef], parts: &[CompRef]) -> LiveCmd {
        LiveCmd::Subscribe {
            generation: self.generation,
            pins: pins.iter().filter_map(|&p| self.pin(p)).map(|i| i as u32).collect(),
            parts: parts.iter().filter_map(|&c| self.comp(c)).map(|i| i as u32).collect(),
        }
    }
}
