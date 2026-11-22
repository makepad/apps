use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;

use fab_elec_mcu_emu::board::{
    fnv1a, Audio, AudioRun, Board, BusEvent, I2cAns, I2cLine, I2cOp, I2cTiming, I2cWhat, I2sFrame, NetInfo, NetKind, Payload, Res, SpiXfer,
};
use fab_elec_mcu_emu::{Drive, Pull};

use crate::screen::ScreenWriter;
use crate::{Amp, BoardDesc, BoardPorts, Co5300, Cst820, Cx, Es8311, Key, Model, Not, Part, PartKind, PartState, PinRef, Qmi8658c, Stimulus, Tca9554};

#[derive(Clone, Debug)]
struct Wire {
    pull: Pull,
    rise_ps: Option<u64>,
    rise_ticks: Option<u64>,
    parts: Vec<(usize, u8)>,
    pads: Vec<u16>,
}

#[derive(Clone, Debug)]
struct Bus {
    scl: u16,
    sda: u16,
    parts: Vec<usize>,
}

#[derive(Clone, Debug)]
struct SpiBus {
    sck: u16,
    targets: Vec<(usize, u16)>,
}

#[derive(Clone, Copy, Debug)]
struct I2sTarget {
    part: usize,
    bclk: u16,
    ws: u16,
    mclk: Option<u16>,
    dac_in: Option<u16>,
    adc_out: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Level {
    high: bool,
    initialized: bool,
    floating_note: bool,
}

#[derive(Debug, Default)]
struct State {
    spi_idle_notes: Vec<bool>,
    i2c_rise_note: bool,
    i2c_unknown_rise_note: bool,
    levels: Vec<Level>,
    pad_drives: Vec<Drive>,
    pad_inputs: Vec<(u16, bool)>,
}

impl Clone for State {
    fn clone(&self) -> Self {
        Self {
            spi_idle_notes: self.spi_idle_notes.clone(),
            i2c_rise_note: self.i2c_rise_note,
            i2c_unknown_rise_note: self.i2c_unknown_rise_note,
            levels: self.levels.clone(),
            pad_drives: self.pad_drives.clone(),
            pad_inputs: self.pad_inputs.clone(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        self.spi_idle_notes.clone_from(&source.spi_idle_notes);
        self.i2c_rise_note = source.i2c_rise_note;
        self.i2c_unknown_rise_note = source.i2c_unknown_rise_note;
        self.levels.clone_from(&source.levels);
        self.pad_drives.clone_from(&source.pad_drives);
        self.pad_inputs.clone_from(&source.pad_inputs);
    }
}

#[derive(Default)]
struct Resolution {
    high: bool,
    low: bool,
    up: bool,
    down: bool,
}

impl Resolution {
    fn pull(&mut self, pull: Pull) {
        match pull {
            Pull::Up => self.up = true,
            Pull::Down => self.down = true,
            Pull::None => (),
        }
    }

    fn drive(&mut self, drive: Drive) {
        match drive {
            Drive::High => self.high = true,
            Drive::Low | Drive::OpenDrain { low: true, .. } => self.low = true,
            Drive::Input(pull) | Drive::OpenDrain { low: false, pull } => self.pull(pull),
        }
    }
}

/// Parts and nets, with a reusable snapshot of all changing state.
#[derive(Clone, Debug)]
pub struct Netlist {
    parts: Vec<Part>,
    saved_parts: Vec<PartState>,
    state: State,
    saved: State,
    saved_bus: Option<usize>,
    refusal: Option<String>,
    wires: Vec<Wire>,
    info: Vec<NetInfo>,
    part_nets: Vec<Vec<Option<u16>>>,
    chip_names: Vec<(Arc<str>, u16)>,
    pad_nets: Vec<Option<u16>>,
    buses: Vec<Bus>,
    spi_buses: Vec<SpiBus>,
    i2s_targets: Vec<I2sTarget>,
    /// (codec, amplifier) part pairs: the amplifier's inputs on the codec's output nets.
    audio_routes: Vec<(usize, usize)>,
    /// The sound played since the last take, and where the last run may go on: its codec part
    /// and last frame's tick; None after a take, a mark or a rewind, so a run never spans one.
    audio: Audio,
    audio_open: Option<(u16, u64)>,
    saved_audio: (usize, usize),
    tick_hz: u64,
    bus: Vec<BusEvent>,
    queue: VecDeque<usize>,
    queued: Vec<bool>,
    bound: bool,
}

impl Netlist {
    pub fn new(desc: BoardDesc) -> Result<Self, String> {
        if desc.parts.len() > usize::from(u16::MAX) + 1
            || desc.nets.len() > usize::from(u16::MAX) + 1
        {
            return Err("board part and net indices must fit in u16".into());
        }
        let parts: Vec<Part> = desc.parts.iter().map(|part| {
            Ok(match part.kind {
                PartKind::Co5300 { glass } => Part::Co5300(
                    Co5300::with_glass(desc.seed, glass).map_err(|why| format!("{}: {why}", part.refdes))?,
                ),
                PartKind::Tca9554 { addr } => Part::Tca9554(
                    Tca9554::new(addr).map_err(|why| format!("{}: {why}", part.refdes))?,
                ),
                PartKind::Qmi8658c { addr } => Part::Qmi8658c(
                    Qmi8658c::new(addr).map_err(|why| format!("{}: {why}", part.refdes))?,
                ),
                PartKind::Cst820 { addr } => Part::Cst820(
                    Cst820::new(addr).map_err(|why| format!("{}: {why}", part.refdes))?,
                ),
                PartKind::Es8311 { addr } => Part::Es8311(
                    Es8311::new(addr).map_err(|why| format!("{}: {why}", part.refdes))?,
                ),
                PartKind::Amp { enable_high, gain_db, start_ms } => Part::Amp(Amp::new(enable_high, gain_db, start_ms)),
                PartKind::Key => Part::Key(Key::default()),
                PartKind::Not => Part::Not(Not::default()),
            })
        }).collect::<Result<_, String>>()?;
        let mut part_nets: Vec<Vec<Option<u16>>> = parts.iter()
            .map(|part| vec![None; part.pins().len()]).collect();
        let mut wires = Vec::with_capacity(desc.nets.len());
        let mut info: Vec<NetInfo> = Vec::with_capacity(desc.nets.len());
        let mut chip_names: Vec<(Arc<str>, u16)> = Vec::new();
        for (net, description) in desc.nets.iter().enumerate() {
            if info.iter().any(|net| net.name == description.name) {
                return Err(format!("duplicate board net name {}", description.name));
            }
            let mut wire = Wire {
                pull: description.pull, rise_ps: description.rise_ps, rise_ticks: None,
                parts: Vec::new(), pads: Vec::new(),
            };
            for pin in &description.pins {
                match pin {
                    PinRef::Chip(name) => {
                        if chip_names.iter().any(|(other, _)| other == name) {
                            return Err(format!("chip pin {name} occurs more than once on the board"));
                        }
                        chip_names.push((name.clone(), net as u16));
                    }
                    PinRef::Part { part, pin } => {
                        let index = usize::from(*part);
                        let model = parts.get(index).ok_or_else(|| {
                            format!("{}: unknown part {part}", description.name)
                        })?;
                        let pin_index = model.pins().iter().position(|&name| name == pin.as_ref())
                            .ok_or_else(|| format!("{}: unknown pin {pin}", desc.parts[index].refdes))?;
                        // M: Rev 0.6 lines 1870–1886 specify INT2 pulses but no width.
                        // Refuse connected INT2 rather than exposing an idle substitute to a peer.
                        if matches!(model, Part::Qmi8658c(_)) && pin.as_ref() == "INT2"
                            && description.pins.len() != 1
                        {
                            return Err("QMI8658C INT2 data-ready pulses not modelled".into());
                        }
                        if part_nets[index][pin_index].replace(net as u16).is_some() {
                            return Err(format!("{} pin {pin} is on more than one net", desc.parts[index].refdes));
                        }
                        wire.parts.push((index, pin_index as u8));
                    }
                }
            }
            wires.push(wire);
            info.push(NetInfo { name: description.name.clone(), kind: NetKind::Level });
        }

        let mut buses: Vec<Bus> = Vec::new();
        for (part, model) in parts.iter().enumerate() {
            let Some(addr) = model.i2c_addr() else { continue };
            let wire = |name| -> Result<u16, String> {
                model.pins().iter().position(|&pin| pin == name)
                    .and_then(|pin| part_nets[part][pin])
                    .ok_or_else(|| format!("{}: I2C pin {name} has no net", desc.parts[part].refdes))
            };
            let scl = wire("SCL")?;
            let sda = wire("SDA")?;
            if scl == sda {
                return Err(format!("{}: SCL and SDA share one net", desc.parts[part].refdes));
            }
            let bus = if let Some(index) = buses.iter().position(|bus| bus.scl == scl && bus.sda == sda) {
                index
            } else {
                if buses.len() > usize::from(u8::MAX) {
                    return Err("board bus indices must fit in u8".into());
                }
                if info[usize::from(scl)].kind != NetKind::Level
                    || info[usize::from(sda)].kind != NetKind::Level
                {
                    return Err("a board net belongs to incompatible I2C buses".into());
                }
                let index = buses.len();
                info[usize::from(scl)].kind = NetKind::I2cScl { bus: index as u8 };
                info[usize::from(sda)].kind = NetKind::I2cSda { bus: index as u8 };
                buses.push(Bus { scl, sda, parts: Vec::new() });
                index
            };
            if buses[bus].parts.iter().any(|&other| parts[other].i2c_addr() == Some(addr)) {
                return Err(format!("{}: duplicate I2C address {addr:#04x} on bus {bus}", desc.parts[part].refdes));
            }
            buses[bus].parts.push(part);
        }

        let mut spi_buses: Vec<SpiBus> = Vec::new();
        for (part, model) in parts.iter().enumerate() {
            if !model.pins().contains(&"CSX") || !model.pins().contains(&"SCLK") {
                continue;
            }
            let wire = |name| -> Result<u16, String> {
                model.pins().iter().position(|&pin| pin == name)
                    .and_then(|pin| part_nets[part][pin])
                    .ok_or_else(|| format!("{}: SPI pin {name} has no net", desc.parts[part].refdes))
            };
            let sck = wire("SCLK")?;
            let cs = wire("CSX")?;
            let bus = if let Some(index) = spi_buses.iter().position(|bus| bus.sck == sck) {
                index
            } else {
                if spi_buses.len() > usize::from(u8::MAX) {
                    return Err("board SPI bus indices must fit in u8".into());
                }
                let index = spi_buses.len();
                spi_buses.push(SpiBus { sck, targets: Vec::new() });
                index
            };
            let mut classify = |net: u16, kind| -> Result<(), String> {
                let previous = &mut info[usize::from(net)].kind;
                if *previous != NetKind::Level && *previous != kind {
                    return Err("a board net belongs to incompatible bus signals".into());
                }
                *previous = kind;
                Ok(())
            };
            classify(sck, NetKind::SpiSck { bus: bus as u8 })?;
            for (line, name) in ["SIO0", "SIO1", "SIO2", "SIO3"].iter().enumerate() {
                classify(wire(name)?, NetKind::SpiData { bus: bus as u8, line: line as u8 })?;
            }
            spi_buses[bus].targets.push((part, cs));
        }

        let mut i2s_targets = Vec::new();
        for (part, model) in parts.iter().enumerate() {
            if !matches!(model, Part::Es8311(_)) { continue; }
            // es8311-datasheet.txt:231-238. A control-only part need not have an audio bus.
            let net = |name| model.pins().iter().position(|&pin| pin == name)
                .and_then(|index| part_nets[part][index]);
            if let (Some(bclk), Some(ws)) = (net("SCLK"), net("LRCK")) {
                if bclk == ws {
                    return Err(format!("{}: I2S bit and frame clocks share one net", desc.parts[part].refdes));
                }
                i2s_targets.push(I2sTarget {
                    part, bclk, ws, mclk: net("MCLK"), dac_in: net("DSDIN"), adc_out: net("ASDOUT"),
                });
            }
        }

        let audio_routes = Self::audio_routes(&desc, &parts, &part_nets, &mut info)?;

        Ok(Self {
            parts,
            saved_parts: Vec::new(),
            state: State {
                spi_idle_notes: vec![false; spi_buses.len()],
                levels: vec![Level::default(); wires.len()],
                ..State::default()
            },
            saved: State::default(),
            saved_bus: None,
            refusal: None,
            queued: vec![false; wires.len()],
            wires,
            info,
            part_nets,
            chip_names,
            pad_nets: Vec::new(),
            buses,
            spi_buses,
            i2s_targets,
            audio_routes,
            audio: Audio::default(),
            audio_open: None,
            saved_audio: (0, 0),
            tick_hz: 0,
            bus: Vec::new(),
            queue: VecDeque::new(),
            bound: false,
        })
    }

    pub fn level(&self, net: u16) -> bool {
        self.state.levels[usize::from(net)].high
    }

    /// Pairs each amplifier with the codec whose OUTP net its INP is on (and OUTN, INN when
    /// wired), and marks every codec output and amplifier input net as audio: such a net joins
    /// only those pins, no chip pad, and carries no level.
    fn audio_routes(desc: &BoardDesc, parts: &[Part], part_nets: &[Vec<Option<u16>>], info: &mut [NetInfo])
        -> Result<Vec<(usize, usize)>, String>
    {
        let pin_net = |part: usize, name: &str| parts[part].pins().iter().position(|&pin| pin == name)
            .and_then(|index| part_nets[part][index]);
        let mut audio_pins = Vec::new();
        for (part, model) in parts.iter().enumerate() {
            let names: &[&str] = match model {
                Part::Es8311(_) => &["OUTP", "OUTN"],
                Part::Amp(_) => &["INP", "INN"],
                _ => continue,
            };
            for name in names {
                if let Some(net) = pin_net(part, name) { audio_pins.push((part, *name, net)); }
            }
        }
        for &(part, name, net) in &audio_pins {
            let others = desc.nets[usize::from(net)].pins.iter().any(|pin| match pin {
                PinRef::Chip(_) => true,
                PinRef::Part { part, pin } => !audio_pins.iter()
                    .any(|&(p, n, _)| p == usize::from(*part) && n == pin.as_ref()),
            });
            if others {
                return Err(format!("{} {name}: an audio net joins only codec outputs and amplifier inputs", desc.parts[part].refdes));
            }
            info[usize::from(net)].kind = NetKind::Audio;
        }
        let mut routes = Vec::new();
        for (amp, model) in parts.iter().enumerate() {
            if !matches!(model, Part::Amp(_)) { continue; }
            let refdes = &desc.parts[amp].refdes;
            let input = pin_net(amp, "INP").ok_or_else(|| format!("{refdes}: amplifier input INP has no net"))?;
            let codec = (0..parts.len()).find(|&c| matches!(parts[c], Part::Es8311(_)) && pin_net(c, "OUTP") == Some(input))
                .ok_or_else(|| format!("{refdes}: amplifier input INP reaches no codec output"))?;
            if pin_net(amp, "INN").is_some() && pin_net(amp, "INN") != pin_net(codec, "OUTN") {
                return Err(format!("{refdes}: amplifier input INN is not on its codec's OUTN"));
            }
            routes.push((codec, amp));
        }
        Ok(routes)
    }

    /// The codec's output for the frame at `t` through its amplifiers (their sum, saturated),
    /// appended to the sound played; a run goes on while frames follow at its rate.
    fn speak(&mut self, codec: usize, t: u64, rate: (u64, u64), sample: i16) {
        let mut amps = self.audio_routes.iter().filter(|&&(c, _)| c == codec).peekable();
        if amps.peek().is_none() { return; }
        let sum: i32 = amps.map(|&(_, amp)| match &self.parts[amp] {
            Part::Amp(amp) => i32::from(amp.speak(t, sample)),
            _ => unreachable!("an audio route ends at an amplifier"),
        }).sum();
        let sample = sum.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let part = codec as u16;
        // the next frame of a run lands within a tick of one period on (frames are rounded up
        // onto the tick grid)
        let follows = |last: u64| t > last && u128::from(t - last) * u128::from(rate.0)
            <= u128::from(self.tick_hz) * u128::from(rate.1) + u128::from(rate.0);
        match (self.audio_open, self.audio.runs.last_mut()) {
            (Some((p, last)), Some(run)) if p == part && run.rate == rate && follows(last) => run.len += 1,
            _ => self.audio.runs.push(AudioRun { part, t0: t, rate, len: 1 }),
        }
        self.audio.samples.push(sample);
        self.audio_open = Some((part, t));
    }

    pub fn net(&self, name: &str) -> Option<u16> {
        self.info.iter().position(|net| net.name.as_ref() == name).map(|index| index as u16)
    }

    /// Read-only model access for board inspection and frame consumers.
    pub fn part(&self, part: u16) -> Option<&Part> {
        self.parts.get(usize::from(part))
    }

    /// Hands a motion sensor its motion track; the bench attaches it once, before the run reads motion.
    pub fn attach_motion_track(&mut self, part: u16, track: Arc<crate::MotionTrack>) -> Result<(), String> {
        match self.parts.get_mut(usize::from(part)) {
            Some(Part::Qmi8658c(sensor)) => {
                sensor.attach_track(track);
                Ok(())
            }
            Some(_) => Err(format!("board part {part} is not a motion sensor")),
            None => Err(format!("unknown board part {part}")),
        }
    }

    /// Make fresh screen readers before moving the board into S3Machine.
    /// A clone carries no screen writers.
    pub fn ports(&mut self) -> BoardPorts {
        let screens = self.parts.iter_mut().enumerate().filter_map(|(index, part)| {
            match part {
                Part::Co5300(panel) => Some((index as u16, panel.attach_screen())),
                _ => None,
            }
        }).collect();
        BoardPorts { screens }
    }

    fn i2c_bus(&self, scl: u16, sda: u16) -> Option<usize> {
        self.pad_nets.get(usize::from(scl)).copied().flatten()
            .zip(self.pad_nets.get(usize::from(sda)).copied().flatten())
            .and_then(|(scl, sda)| self.buses.iter().position(|bus| {
                bus.scl == scl && bus.sda == sda
                    && self.wires[usize::from(scl)].pull == Pull::Up
                    && self.wires[usize::from(sda)].pull == Pull::Up
            }))
    }

    fn queue_net(&mut self, net: u16) {
        let index = usize::from(net);
        if !self.queued[index] {
            self.queued[index] = true;
            self.queue.push_back(index);
        }
    }

    fn queue_part(&mut self, part: usize) {
        for pin in 0..self.part_nets[part].len() {
            if let Some(net) = self.part_nets[part][pin] {
                self.queue_net(net);
            }
        }
    }

    fn resolve(&mut self, t: u64, net: usize) -> Res<bool> {
        if self.info[net].kind == NetKind::Audio { return Ok(false); }
        let mut r = Resolution::default();
        let wire = &self.wires[net];
        r.pull(wire.pull);
        for &(part, pin) in &wire.parts {
            r.drive(self.parts[part].drive(pin));
        }
        for &pad in &wire.pads {
            r.drive(self.state.pad_drives[usize::from(pad)]);
        }
        match (r.high, r.low) {
            (true, true) => return Err("contention on a board net"),
            (true, false) => return Ok(true),
            (false, true) => return Ok(false),
            (false, false) => (),
        }
        match (r.up, r.down) {
            (true, true) => Err("pull-up and pull-down on one board net"),
            (true, false) => Ok(true),
            (false, true) => Ok(false),
            (false, false) => {
                let level = &mut self.state.levels[net];
                if !level.floating_note {
                    self.bus.push(BusEvent::Note {
                        t,
                        what: "a board net with no driver and no pull keeps its last level",
                    });
                    level.floating_note = true;
                }
                Ok(level.high)
            }
        }
    }

    /// Resolves the queue in rounds: a round resolves the nets of the parts that saw a change
    /// in the round before. Without a loop a change reaches the last net within one round per
    /// net, plus bind's first round, so a net that still changes after that is on a zero-delay
    /// loop that oscillates.
    fn settle(&mut self, t: u64, initializing: bool) -> Res {
        let mut round = 0;
        while !self.queue.is_empty() {
            round += 1;
            for _ in 0..self.queue.len() {
                let net = self.queue.pop_front().expect("the round's nets are queued");
                self.queued[net] = false;
                let high = self.resolve(t, net)?;
                let old = self.state.levels[net];
                let changed = old.high != high;
                self.state.levels[net].high = high;
                self.state.levels[net].initialized = true;
                if changed {
                    if round > self.wires.len() + 1 {
                        return Err("a zero-delay loop on the board's nets oscillates");
                    }
                    self.bus.push(BusEvent::Level { t, net: net as u16, high });
                }
                if changed || !old.initialized {
                    for index in 0..self.wires[net].parts.len() {
                        let (part, pin) = self.wires[net].parts[index];
                        let mut cx = Cx { t, bus: &mut self.bus, detail: &mut self.refusal, initializing };
                        self.parts[part].pin(&mut cx, pin, high)?;
                        self.queue_part(part);
                    }
                    for &pad in &self.wires[net].pads {
                        self.state.pad_inputs.push((pad, high));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Board for Netlist {
    type In = Stimulus;

    fn bind(&mut self, chip_pins: &[&'static str], tick_hz: u64) -> Result<(), String> {
        if self.bound {
            return Err("board is already bound".into());
        }
        if tick_hz == 0 {
            return Err("board tick rate must be nonzero".into());
        }
        if chip_pins.len() > usize::from(u16::MAX) + 1 {
            return Err("chip pad indices must fit in u16".into());
        }
        let mut pad_nets = vec![None; chip_pins.len()];
        for (name, net) in &self.chip_names {
            let mut matches = chip_pins.iter().enumerate().filter(|(_, pin)| **pin == name.as_ref());
            let (pad, _) = matches.next().ok_or_else(|| format!("unknown chip pin {name}"))?;
            if matches.next().is_some() {
                return Err(format!("ambiguous chip pin {name}"));
            }
            pad_nets[pad] = Some(*net);
        }
        for part in &mut self.parts {
            part.bind(tick_hz)?;
        }
        self.tick_hz = tick_hz;
        for (wire, info) in self.wires.iter_mut().zip(&self.info) {
            wire.rise_ticks = wire.rise_ps.map(|ps| {
                let ticks = (u128::from(ps) * u128::from(tick_hz)).div_ceil(1_000_000_000_000);
                u64::try_from(ticks).map_err(|_| format!("{}: rise time does not fit in board ticks", info.name))
            }).transpose()?;
        }
        for (pad, net) in pad_nets.iter().enumerate() {
            if let Some(net) = *net {
                self.wires[usize::from(net)].pads.push(pad as u16);
            }
        }
        self.pad_nets = pad_nets;
        self.state.pad_drives = vec![Drive::Input(Pull::None); chip_pins.len()];
        self.bound = true;
        for net in 0..self.wires.len() {
            self.queue_net(net as u16);
        }
        self.settle(0, true).map_err(String::from)
    }

    fn pad_drive(&mut self, t: u64, pad: u16, d: Drive) -> Res {
        let drive = self.state.pad_drives.get_mut(usize::from(pad))
            .ok_or("chip pad outside the bound board pin list")?;
        *drive = d;
        if let Some(net) = self.pad_nets[usize::from(pad)] {
            self.queue_net(net);
        }
        self.settle(t, false)
    }

    fn pad_inputs(&mut self, out: &mut Vec<(u16, bool)>) {
        out.append(&mut self.state.pad_inputs);
    }

    fn chip_reset(&mut self, _t: u64) -> Res {
        Ok(())
    }

    fn i2c_timing(&mut self, t: u64, scl: u16, sda: u16, timing: I2cTiming) -> Res {
        let Some(bus) = self.i2c_bus(scl, sda) else { return Ok(()) };
        let bus = &self.buses[bus];
        let lines = [(bus.scl, I2cLine::Scl, timing.allowance), (bus.sda, I2cLine::Sda, timing.setup)];
        if lines.iter().any(|&(net, _, _)| self.wires[usize::from(net)].rise_ticks.is_none())
            && !self.state.i2c_unknown_rise_note
        {
            self.bus.push(BusEvent::Note {
                t,
                what: "I2C rise time not checked: a bus line's pull-up, pin capacitance or input threshold is unknown",
            });
            self.state.i2c_unknown_rise_note = true;
        }
        for (net, line, window_ticks) in lines {
            let Some(rise_ticks) = self.wires[usize::from(net)].rise_ticks else { continue };
            if !self.state.i2c_rise_note {
                self.bus.push(BusEvent::Note {
                    t,
                    what: "I2C rise check: pin capacitance only (no trace capacitance); the controller's input threshold (targets' thresholds and data-setup times are not in part data)",
                });
                self.state.i2c_rise_note = true;
            }
            if rise_ticks > window_ticks {
                self.bus.push(BusEvent::Rise { t, net, line, rise_ticks, window_ticks });
                return Err("an I2C line rises slower than the controller's window; see the rise record");
            }
        }
        Ok(())
    }

    fn i2c(&mut self, t: u64, scl: u16, sda: u16, op: I2cOp) -> Res<I2cAns> {
        let bus = self.i2c_bus(scl, sda).ok_or("I2C controller on a net without a bus")?;
        let mut ack = false;
        let mut value = 0xff;
        for index in 0..self.buses[bus].parts.len() {
            let part = self.buses[bus].parts[index];
            let mut cx = Cx { t, bus: &mut self.bus, detail: &mut self.refusal, initializing: false };
            match self.parts[part].i2c(&mut cx, op)? {
                I2cAns::Ack(answer) => ack |= answer,
                I2cAns::Byte(byte) => value &= byte,
                I2cAns::None => (),
            }
            self.queue_part(part);
            self.settle(t, false)?;
        }
        let (answer, what) = match op {
            I2cOp::Start => (I2cAns::None, I2cWhat::Start),
            I2cOp::Write(value) => (I2cAns::Ack(ack), I2cWhat::Byte { value, ack }),
            I2cOp::Read { ack } => (I2cAns::Byte(value), I2cWhat::Byte { value, ack }),
            I2cOp::Stop => (I2cAns::None, I2cWhat::Stop),
        };
        self.bus.push(BusEvent::I2c { t, bus: bus as u8, what });
        Ok(answer)
    }

    fn i2s(&mut self, f: &I2sFrame) -> Res<[i32; 2]> {
        let net = |pad: u16| self.pad_nets.get(usize::from(pad)).copied().flatten();
        let clocks = net(f.bclk).zip(net(f.ws));
        let mut targets = self.i2s_targets.iter().filter(|t| clocks == Some((t.bclk, t.ws)));
        let target = *targets.next().ok_or("I2S frame with no device on its pads")?;
        if targets.next().is_some() {
            return Err("I2S frame with multiple devices on its BCLK/WS nets");
        }
        if f.mclk.is_some() != f.mclk_hz.is_some() {
            return Err("ES8311: controller MCLK pad and frequency must be supplied together");
        }
        let master_net = f.mclk.and_then(net);
        if master_net == Some(target.bclk) || master_net == Some(target.ws) {
            return Err("ES8311: separate controller clock outputs share one net");
        }
        if let Some(pad) = f.dout {
            let wire = net(pad);
            if wire.is_none() || wire != target.dac_in {
                return Err("ES8311: controller DOUT does not reach DSDIN");
            }
            if wire == Some(target.bclk) || wire == Some(target.ws) || wire == master_net {
                return Err("ES8311: DSDIN and an active serial clock share one net");
            }
        }
        if let Some(pad) = f.din {
            let wire = net(pad);
            if wire.is_none() || wire != target.adc_out {
                return Err("ES8311: controller DIN does not reach ASDOUT");
            }
            if wire == Some(target.bclk) || wire == Some(target.ws) || wire == master_net
                || (f.dout.is_some() && wire == target.dac_in)
            {
                return Err("ES8311: ASDOUT and an active controller output share one net");
            }
        }
        let mut routed = *f;
        if target.mclk == Some(target.bclk) {
            // A single controller BCLK output can physically feed both codec inputs.
            routed.mclk = Some(f.bclk);
            routed.mclk_hz = Some(f.bclk_hz);
        } else if target.mclk.is_none() || master_net != target.mclk {
            routed.mclk = None;
            routed.mclk_hz = None;
        }
        let mut cx = Cx { t: f.t, bus: &mut self.bus, detail: &mut self.refusal, initializing: false };
        let answer = self.parts[target.part].i2s(&mut cx, &routed)?;
        let out = match &mut self.parts[target.part] {
            Part::Es8311(codec) => codec.take_out(),
            _ => None,
        };
        if let Some(sample) = out {
            let (mut n, mut d) = (u128::from(f.bclk_hz.0), u128::from(f.bclk_hz.1) * 2 * u128::from(f.slot_bits));
            let (mut a, mut b) = (n, d);
            while b != 0 { (a, b) = (b, a % b); }
            (n, d) = (n / a, d / a);
            let rate = u64::try_from(n).ok().zip(u64::try_from(d).ok()).ok_or("I2S frame rate does not fit")?;
            self.speak(target.part, f.t, rate, sample);
        }
        Ok(answer)
    }

    fn spi(&mut self, x: &SpiXfer, tx: &dyn Payload, rx: &mut Vec<u8>) -> Res {
        rx.clear();
        let bus = self.pad_nets.get(usize::from(x.sck)).copied().flatten()
            .and_then(|sck| self.spi_buses.iter().position(|bus| bus.sck == sck))
            .ok_or("SPI transfer with no device on its pads")?;
        let mut selected = None;
        for &(part, cs) in &self.spi_buses[bus].targets {
            if !self.level(cs) && selected.replace(part).is_some() {
                return Err("SPI transfer with two chip selects low");
            }
        }
        let Some(part) = selected else {
            if !self.state.spi_idle_notes[bus] {
                let mut cx = Cx { t: x.t1, bus: &mut self.bus, detail: &mut self.refusal, initializing: false };
                cx.note("SPI transfer with no chip select low goes nowhere");
                self.state.spi_idle_notes[bus] = true;
            }
            return Ok(());
        };
        let len = u32::try_from(tx.len()).map_err(|_| "SPI payload length does not fit in u32")?;
        let mut cx = Cx { t: x.t1, bus: &mut self.bus, detail: &mut self.refusal, initializing: false };
        self.parts[part].spi(&mut cx, x, tx, rx)?;
        let mut head = [0; 16];
        let mut copied = 0;
        tx.chunks(&mut |bytes| {
            let count = bytes.len().min(head.len() - copied);
            head[copied..copied + count].copy_from_slice(&bytes[..count]);
            copied += count;
        });
        self.bus.push(BusEvent::Spi {
            t0: x.t0, t1: x.t1, bus: bus as u8, bit_ticks: x.bit_ticks,
            mode: x.mode, lines: x.lines, head, len, hash: fnv1a(tx),
        });
        self.queue_part(part);
        self.settle(x.t1, false)
    }

    fn next_event(&self) -> Option<u64> {
        self.parts.iter().filter_map(Model::next_event).min()
    }

    fn event(&mut self, t: u64) -> Res {
        for part in 0..self.parts.len() {
            if self.parts[part].next_event() == Some(t) {
                let mut cx = Cx { t, bus: &mut self.bus, detail: &mut self.refusal, initializing: false };
                self.parts[part].event(&mut cx)?;
                self.queue_part(part);
            }
        }
        self.settle(t, false)
    }

    fn input(&mut self, t: u64, ev: Stimulus) -> Res {
        let (Stimulus::Key { part, .. } | Stimulus::Motion { part, .. } | Stimulus::Touch { part, .. }
            | Stimulus::Track { part, .. } | Stimulus::Sound { part, .. }) = ev;
        let part = usize::from(part);
        let model = self.parts.get_mut(part).ok_or("stimulus names an unknown board part")?;
        let mut cx = Cx { t, bus: &mut self.bus, detail: &mut self.refusal, initializing: false };
        model.input(&mut cx, &ev)?;
        self.queue_part(part);
        self.settle(t, false)
    }

    fn refusal_detail(&self) -> Option<String> {
        self.refusal.clone()
    }

    fn mark(&mut self) {
        self.refusal = None;
        self.saved_parts.clear();
        self.saved_parts.extend(self.parts.iter_mut().map(Part::mark));
        self.saved.clone_from(&self.state);
        self.saved_bus = Some(self.bus.len());
        self.saved_audio = (self.audio.runs.len(), self.audio.samples.len());
        self.audio_open = None;
    }

    fn rewind(&mut self) {
        self.refusal = None;
        let bus = self.saved_bus.expect("board rewind requires a mark");
        for (part, saved) in self.parts.iter_mut().zip(&self.saved_parts) {
            part.rewind(saved);
        }
        self.state.clone_from(&self.saved);
        self.bus.truncate(bus);
        self.audio.runs.truncate(self.saved_audio.0);
        self.audio.samples.truncate(self.saved_audio.1);
        self.audio_open = None;
        self.queue.clear();
        self.queued.fill(false);
    }

    fn same_state(&self, other: &Self) -> Option<&'static str> {
        // Both copies have the same wiring, bus topology and net metadata. Skip these tables
        // and their immutable Arc-backed names, which are fixed at construction or bind.
        let Self {
            parts: _, saved_parts: _, state: _, saved: _, saved_bus, refusal, wires: _, info: _,
            part_nets: _, chip_names: _, pad_nets: _, buses: _, spi_buses: _, i2s_targets: _, audio_routes: _,
            audio, audio_open, saved_audio, tick_hz: _, bus, queue, queued, bound,
        } = self;

        macro_rules! compare_parts {
            ($field:ident, $kind:ident) => {{
                if self.$field.len() != other.$field.len() {
                    return Some(concat!("netlist.", stringify!($field), ".len"));
                }
                for (part, peer) in self.$field.iter().zip(&other.$field) {
                    if part != peer {
                        return Some(match part {
                            $kind::Co5300(_) => concat!("netlist.", stringify!($field), "[co5300]"),
                            $kind::Tca9554(_) => concat!("netlist.", stringify!($field), "[tca9554]"),
                            $kind::Qmi8658c(_) => concat!("netlist.", stringify!($field), "[qmi8658c]"),
                            $kind::Cst820(_) => concat!("netlist.", stringify!($field), "[cst820]"),
                            $kind::Es8311(_) => concat!("netlist.", stringify!($field), "[es8311]"),
                            $kind::Amp(_) => concat!("netlist.", stringify!($field), "[amp]"),
                            $kind::Key(_) => concat!("netlist.", stringify!($field), "[key]"),
                            $kind::Not(_) => concat!("netlist.", stringify!($field), "[not]"),
                        });
                    }
                }
            }};
        }
        compare_parts!(parts, Part);
        compare_parts!(saved_parts, PartState);

        macro_rules! compare_states {
            ($($field:ident),+) => {{
                let State { $($field: _),+ } = &self.state;
                $(if self.state.$field != other.state.$field {
                    return Some(concat!("netlist.state.", stringify!($field)));
                })+
                $(if self.saved.$field != other.saved.$field {
                    return Some(concat!("netlist.saved.", stringify!($field)));
                })+
            }};
        }
        compare_states!(spi_idle_notes, i2c_rise_note, i2c_unknown_rise_note, levels, pad_drives, pad_inputs);
        if saved_bus != &other.saved_bus { return Some("netlist.saved_bus"); }
        if refusal != &other.refusal { return Some("netlist.refusal"); }
        if bus != &other.bus { return Some("netlist.bus"); }
        if audio != &other.audio { return Some("netlist.audio"); }
        if audio_open != &other.audio_open { return Some("netlist.audio_open"); }
        if saved_audio != &other.saved_audio { return Some("netlist.saved_audio"); }
        if queue != &other.queue { return Some("netlist.queue"); }
        if queued != &other.queued { return Some("netlist.queued"); }
        if bound != &other.bound { return Some("netlist.bound"); }
        None
    }

    fn barrier(&self) -> bool {
        self.parts.iter().any(Part::barrier)
    }

    fn nets(&self) -> &[NetInfo] {
        &self.info
    }

    fn take_audio(&mut self, out: &mut Audio) {
        out.append(&mut self.audio);
        self.audio_open = None;
    }

    fn take_bus(&mut self, out: &mut Vec<BusEvent>) {
        out.append(&mut self.bus);
    }

    fn take_feeds(&mut self) -> Option<Box<dyn Any + Send>> {
        let feeds: Vec<(u16, ScreenWriter)> = self.parts.iter_mut().enumerate().filter_map(|(index, part)| {
            match part {
                Part::Co5300(panel) => panel.take_screen().map(|writer| (index as u16, writer)),
                _ => None,
            }
        }).collect();
        if feeds.is_empty() { None } else { Some(Box::new(feeds)) }
    }

    fn put_feeds(&mut self, feeds: Box<dyn Any + Send>) {
        let Ok(feeds) = feeds.downcast::<Vec<(u16, ScreenWriter)>>() else { return };
        for (index, writer) in *feeds {
            if let Some(Part::Co5300(panel)) = self.parts.get_mut(usize::from(index)) {
                panel.put_screen(writer);
            }
        }
    }

    fn publish(&mut self, t: f64) {
        for part in &mut self.parts {
            if let Part::Co5300(panel) = part {
                panel.publish(t);
            }
        }
    }
}
