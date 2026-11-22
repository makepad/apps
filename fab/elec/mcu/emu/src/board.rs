//! The rewindable board boundary, independent of any chip.

use crate::machine::Drive;

/// Err: the machine latches an Unmodeled stop with this text.
pub type Res<T = ()> = Result<T, &'static str>;

/// Parts and wiring attached to a machine.
pub trait Board: Clone + Send + 'static {
    /// Outside stimulus, defined by the board.
    type In: Clone + Send + 'static;
    /// Resolves chip pad names and durations once at construction.
    fn bind(&mut self, chip_pins: &[&'static str], tick_hz: u64) -> Result<(), String>;
    /// A bound chip pad's drive changed at `t`.
    fn pad_drive(&mut self, t: u64, pad: u16, d: Drive) -> Res;
    /// Drains changed chip input levels.
    fn pad_inputs(&mut self, out: &mut Vec<(u16, bool)>);
    /// Reports a chip reset; reset drives arrive through `pad_drive`.
    fn chip_reset(&mut self, t: u64) -> Res;
    /// The controller's windows for the I2C list starting at `t`, in board time.
    /// A board that knows its nets' rise times stops when a line cannot make its window.
    fn i2c_timing(&mut self, _t: u64, _scl: u16, _sda: u16, _timing: I2cTiming) -> Res {
        Ok(())
    }
    /// One I2C step on the bus attached to these pads.
    fn i2c(&mut self, t: u64, scl: u16, sda: u16, op: I2cOp) -> Res<I2cAns>;
    /// One SPI transfer at its done tick, `x.t1`.
    fn spi(&mut self, x: &SpiXfer, tx: &dyn Payload, rx: &mut Vec<u8>) -> Res;
    /// One I2S frame on the bus attached to `f`'s pads: the samples the device drives on `din`.
    fn i2s(&mut self, _f: &I2sFrame) -> Res<[i32; 2]> {
        Err("I2S frame with no device on its pads")
    }
    /// The detail behind the board's latest refusal (its expected and actual values), when a part
    /// gave one; read at a halt.
    fn refusal_detail(&self) -> Option<String> { None }
    fn next_event(&self) -> Option<u64>;
    fn event(&mut self, t: u64) -> Res;
    fn input(&mut self, t: u64, ev: Self::In) -> Res;
    fn mark(&mut self);
    fn rewind(&mut self);
    /// The first part of this board's state that differs from `other`'s, a copy of the same
    /// board (same parts and wiring): None when every part's state, the pending records and the
    /// marked copies are the same. For differential tests (two tiers of one machine in lockstep).
    fn same_state(&self, _other: &Self) -> Option<&'static str> { Some("this board does not compare its state") }
    /// A part's journal needs a mark soon.
    fn barrier(&self) -> bool;
    fn nets(&self) -> &[NetInfo];
    /// Drains bus records and part notes, in board time, just before a mark.
    fn take_bus(&mut self, out: &mut Vec<BusEvent>);
    /// Drains the sound its amplifiers played, in board time, just before a mark, as `take_bus`.
    fn take_audio(&mut self, _out: &mut Audio) {}
    /// Hands committed display state to a view at the run's simulated time `t`, in seconds.
    fn publish(&mut self, t: f64);
    /// Hands over the outputs a view reads (a panel's screen feed) to the earlier copy of this
    /// board that a rewind restores (`put_feeds`). None: it has none. A clone carries none.
    fn take_feeds(&mut self) -> Option<Box<dyn std::any::Any + Send>> { None }
    /// Takes over the feeds `take_feeds` handed over from the board this is an earlier copy of.
    /// Each feed sends this copy's whole state at its next publication.
    fn put_feeds(&mut self, _feeds: Box<dyn std::any::Any + Send>) {}
}

/// The controller's data-setup and SCL rising-allowance windows, in board ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I2cTiming {
    pub setup: u64,
    pub allowance: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2cLine {
    Scl,
    Sda,
}

/// The first Write after Start is the address byte, including its R/W bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2cOp {
    Start,
    Write(u8),
    Read { ack: bool },
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2cAns {
    None,
    Ack(bool),
    Byte(u8),
}

/// A completed SPI transfer and its pad assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpiXfer {
    pub t0: u64,
    pub t1: u64,
    pub sck: u16,
    pub d: [Option<u16>; 4],
    pub bit_ticks: u32,
    pub mode: u8,
    pub lsb_first: bool,
    pub cmd: (u8, u16),
    pub addr: (u8, u32),
    pub dummy: u16,
    pub lines: [u8; 3],
    pub tx_bits: u32,
    pub rx_bits: u32,
    pub full_duplex: bool,
}

/// The two-slot I2S standards (ESP32-S3 TRM 28.5.1-28.5.3; PDM is not one of them): Philips
/// and PCM shift the data one BCLK after WS, MSB alignment changes both together. MSB first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2sStandard {
    Philips,
    MsbAlignment,
    Pcm,
}

/// One I2S frame (one WS period, two slots) as the controller clocks it, the clock master:
/// it drives `bclk`, `ws` and `mclk` when present. `t` is the board tick of the frame's first
/// BCLK edge. Clocks are exact rationals in Hz, (numerator, denominator), as the controller's
/// dividers make them. `tx` holds the two signed samples it shifts out on `dout`, each
/// `data_bits` wide in a `slot_bits` slot; zero when `dout` is None.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I2sFrame {
    pub t: u64,
    pub mclk: Option<u16>,
    pub bclk: u16,
    pub ws: u16,
    pub dout: Option<u16>,
    pub din: Option<u16>,
    pub mclk_hz: Option<(u64, u64)>,
    pub bclk_hz: (u64, u64),
    pub standard: I2sStandard,
    pub slot_bits: u8,
    pub data_bits: u8,
    pub tx: [i32; 2],
}

/// Transfer bytes, possibly held in several slices.
pub trait Payload {
    fn len(&self) -> usize;
    fn chunks(&self, f: &mut dyn FnMut(&[u8]));
}

/// FNV-1a over all payload chunks, independent of their boundaries.
pub fn fnv1a(payload: &dyn Payload) -> u64 {
    // Fowler/Noll/Vo FNV-1a 64-bit offset basis and prime: isthe.com/chongo/tech/comp/fnv/.
    let mut hash = 0xcbf29ce484222325u64;
    payload.chunks(&mut |bytes| {
        for &byte in bytes {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    });
    hash
}

/// A large buffer that rewinds by journal. Each write logs the old value, including
/// repeated writes to an index. `mark` clears the journal; `Clone` copies the whole
/// buffer for a machine snapshot. The limit is soft: writes past it still happen
/// and remain rewindable. It requests a mark soon, changing speed, never results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undo<T: Copy> {
    data: Vec<T>,
    log: Vec<(u32, T)>,
    limit: usize,
}

impl<T: Copy> Undo<T> {
    pub fn new(data: Vec<T>, limit: usize) -> Self {
        assert!(data.len().saturating_sub(1) <= u32::MAX as usize,
            "Undo indices must fit in u32");
        Self { data, log: Vec::new(), limit }
    }

    pub fn get(&self, i: usize) -> T {
        self.data[i]
    }

    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    pub fn write(&mut self, i: usize, v: T) {
        self.log.push((i as u32, self.data[i]));
        self.data[i] = v;
    }

    pub fn mark(&mut self) {
        self.log.clear();
    }

    pub fn rewind(&mut self) {
        for (i, value) in self.log.drain(..).rev() {
            self.data[i as usize] = value;
        }
    }

    pub fn full(&self) -> bool {
        self.log.len() >= self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetInfo {
    pub name: std::sync::Arc<str>,
    pub kind: NetKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetKind {
    Level,
    I2cScl { bus: u8 },
    I2cSda { bus: u8 },
    SpiSck { bus: u8 },
    SpiData { bus: u8, line: u8 },
    /// A codec's analog output to an amplifier: it carries sound (`Board::take_audio`), not levels.
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2cWhat {
    Start,
    Byte { value: u8, ack: bool },
    Stop,
}

/// Sound a board's amplifiers played: runs of consecutive frames, their samples back to back in
/// `samples`, each at the speaker's full scale. A run's frames follow at its exact rate from its
/// first frame's board tick; a new run starts where the stream stopped, changed rate or changed
/// amplifier. Buffers are reused: `clear` keeps their storage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Audio {
    pub runs: Vec<AudioRun>,
    pub samples: Vec<i16>,
}

/// One run of [`Audio`]: the amplifier part, the first frame's board tick, the frame rate in Hz
/// as a ratio (numerator, denominator) and the run's sample count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioRun {
    pub part: u16,
    pub t0: u64,
    pub rate: (u64, u64),
    pub len: usize,
}

impl Audio {
    pub fn clear(&mut self) {
        self.runs.clear();
        self.samples.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Appends `other`'s runs after this one's, leaving `other` empty with its storage.
    pub fn append(&mut self, other: &mut Audio) {
        self.runs.append(&mut other.runs);
        self.samples.append(&mut other.samples);
    }

    /// The samples of each run, in order.
    pub fn each(&self) -> impl Iterator<Item = (&AudioRun, &[i16])> {
        self.runs.iter().scan(0, |at, run| {
            let start = *at;
            *at += run.len;
            Some((run, &self.samples[start..*at]))
        })
    }
}

/// Board records in board time, which a chip reset does not restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusEvent {
    Level { t: u64, net: u16, high: bool },
    I2c { t: u64, bus: u8, what: I2cWhat },
    /// The record behind a rise-time stop, with rise and window in board ticks.
    Rise { t: u64, net: u16, line: I2cLine, rise_ticks: u64, window_ticks: u64 },
    Spi {
        t0: u64, t1: u64, bus: u8, bit_ticks: u32, mode: u8, lines: [u8; 3],
        head: [u8; 16], len: u32, hash: u64,
    },
    /// A part's model choice, named the first time it applies.
    Note { t: u64, what: &'static str },
}

/// A machine without parts; bus transfers stop instead of inventing an answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoBoard;

impl Board for NoBoard {
    type In = ();

    fn bind(&mut self, _chip_pins: &[&'static str], _tick_hz: u64) -> Result<(), String> {
        Ok(())
    }

    fn pad_drive(&mut self, _t: u64, _pad: u16, _d: Drive) -> Res {
        Ok(())
    }

    fn pad_inputs(&mut self, _out: &mut Vec<(u16, bool)>) {}

    fn chip_reset(&mut self, _t: u64) -> Res {
        Ok(())
    }

    fn i2c(&mut self, _t: u64, _scl: u16, _sda: u16, _op: I2cOp) -> Res<I2cAns> {
        Err("I2C controller on a net without a bus")
    }

    fn spi(&mut self, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("SPI transfer with no device on its pads")
    }

    fn next_event(&self) -> Option<u64> {
        None
    }

    fn event(&mut self, _t: u64) -> Res {
        Ok(())
    }

    fn input(&mut self, _t: u64, _ev: ()) -> Res {
        Ok(())
    }

    fn mark(&mut self) {}
    fn rewind(&mut self) {}

    fn same_state(&self, _other: &Self) -> Option<&'static str> {
        let Self {} = self;
        None
    }

    fn barrier(&self) -> bool {
        false
    }

    fn nets(&self) -> &[NetInfo] {
        &[]
    }

    fn take_bus(&mut self, _out: &mut Vec<BusEvent>) {}
    fn publish(&mut self, _t: f64) {}
}
