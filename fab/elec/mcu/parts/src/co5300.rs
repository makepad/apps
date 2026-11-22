//! CO5300 write interface and frame RAM (P01–P26, P05a, P10a, P18a, O03).

use fab_elec_mcu_emu::board::{I2cAns, I2cOp, Payload, Res, SpiXfer, Undo};
use fab_elec_mcu_emu::{Drive, Pull};

use crate::screen::{screen_feed, ScreenReader, ScreenWriter};
use crate::{Cx, Model, Stimulus};

const WIDTH: usize = 480;
const HEIGHT: usize = 480;

// Keep the command number in an error without allocating or changing Res's static text.
static CMD_ERRORS: [[u8; b"CO5300 CMD 0x00 is not modelled (P20)".len()]; 256] = {
    let mut errors = [*b"CO5300 CMD 0x00 is not modelled (P20)"; 256];
    let hex = b"0123456789ABCDEF";
    let mut cmd = 0;
    while cmd < 256 {
        errors[cmd][b"CO5300 CMD 0x".len()] = hex[cmd >> 4];
        errors[cmd][b"CO5300 CMD 0x0".len()] = hex[cmd & 15];
        cmd += 1;
    }
    errors
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sleep {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Registers {
    display_on: bool,
    inversion: bool,
    brightness: u8,
    spi_wram: bool,
    ctrl: Option<u8>,
    sleep: Option<Sleep>,
    colmod: Option<u8>,
    columns: [u16; 2],
    rows: [u16; 2],
    addressed: bool,
    next_pixel: Option<usize>,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            display_on: false, // P14.
            inversion: false, // P13.
            brightness: 0, // P12.
            spi_wram: false, // P23: SPI writes to frame RAM disabled.
            ctrl: None, // P24: D-P6, conflicting reset defaults.
            sleep: None, // P08: D-P1, conflicting reset defaults.
            colmod: None, // P08: D-P2, conflicting reset defaults.
            columns: [0, 0x1c5], // P15.
            rows: [0, 0x1c5], // P16.
            addressed: false, // P17: a write needs an explicit location command.
            next_pixel: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timing {
    cycle: u64,
    cs: u64,
    reject_reset: u64,
    reset: u64,
    command: u64,
    sleep: u64,
}

/// Snapshot of registers, pin timing and note flags, never of the frame RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct State {
    r: Registers,
    timing: Option<Timing>,
    hardware_reset: bool,
    resx_low: Option<u64>,
    cs_low: Option<u64>,
    last_transfer: Option<u64>,
    quiet_until: u64,
    sleep_out_allowed: u64,
    sleep_in_allowed: u64,
    sleep_out_sequence_until: u64,
    mode_note: bool,
    row_note: bool,
    order_note: bool,
    colmod_note: bool,
    swreset_note: bool,
}

/// The CMD1 write subset. Extended pages remain locked (P20); reads stop (P04).
#[derive(Debug)]
pub struct Co5300 {
    state: State,
    frame: Undo<u16>,
    glass: [u16; 4],
    // Observation only: the writer's row versions are never journaled or restored.
    screen: Option<ScreenWriter>,
}

impl PartialEq for Co5300 {
    fn eq(&self, other: &Self) -> bool {
        // The screen feed belongs to the viewer. A clone has no feed and rewind keeps the live one.
        let Self { state, frame, glass, screen: _ } = self;
        state == &other.state && glass == &other.glass && frame == &other.frame
    }
}

impl Eq for Co5300 {}

impl Clone for Co5300 {
    fn clone(&self) -> Self {
        Self {
            state: self.state,
            frame: self.frame.clone(),
            glass: self.glass,
            screen: None,
        }
    }

    fn clone_from(&mut self, source: &Self) {
        // A live port's dimensions are fixed. Reattach if copying a different glass.
        if self.glass != source.glass { self.screen = None; }
        self.glass = source.glass;
        self.state = source.state;
        self.frame.clone_from(&source.frame);
        // The live part keeps its writer; copied RAM invalidates all of its row versions.
        if let Some(writer) = &mut self.screen {
            writer.dirty_all();
        }
    }
}

impl Co5300 {
    /// Its model pins, in the order the netlist binds; supplies are implicit.
    pub const PINS: &'static [&'static str] = &["CSX", "SCLK", "SIO0", "SIO1", "SIO2", "SIO3", "RESX"];

    pub fn new(seed: u64) -> Self {
        // P17: SplitMix64 supplies reproducible junk, not a hardware RAM distribution.
        let mut random = seed;
        let data = (0..WIDTH * HEIGHT).map(|_| {
            random = random.wrapping_add(0x9e3779b97f4a7c15);
            let mut word = random;
            word = (word ^ (word >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
            (word ^ (word >> 31)) as u16
        }).collect();
        Self {
            state: State {
                r: Registers::default(),
                timing: None,
                hardware_reset: false,
                resx_low: None,
                cs_low: None,
                last_transfer: None,
                quiet_until: 0,
                sleep_out_allowed: 0,
                sleep_in_allowed: 0,
                sleep_out_sequence_until: 0,
                mode_note: false,
                row_note: false,
                order_note: false,
                colmod_note: false,
                swreset_note: false,
            },
            // A 64-row strip is a soft request for a mark, never a write limit.
            frame: Undo::new(data, WIDTH * 64),
            glass: [0, 0, WIDTH as u16, HEIGHT as u16],
            screen: None,
        }
    }

    /// Board-supplied glass [x, y, width, height] in the fixed 480x480 RAM (P19).
    /// O03 gives no glass dimensions or offset, so the schematic must supply them.
    pub fn with_glass(seed: u64, glass: [u16; 4]) -> Result<Self, String> {
        Self::validate_glass(glass)?;
        let mut panel = Self::new(seed);
        panel.glass = glass;
        Ok(panel)
    }

    pub(crate) fn validate_glass([x, y, w, h]: [u16; 4]) -> Result<(), String> {
        if usize::from(x) >= WIDTH { return Err("CO5300: glass_x must be inside 480-pixel memory width".into()); }
        if usize::from(y) >= HEIGHT { return Err("CO5300: glass_y must be inside 480-pixel memory height".into()); }
        if w == 0 || usize::from(x) + usize::from(w) > WIDTH {
            return Err("CO5300: glass_w must be nonzero and fit after glass_x".into());
        }
        if h == 0 || usize::from(y) + usize::from(h) > HEIGHT {
            return Err("CO5300: glass_h must be nonzero and fit after glass_y".into());
        }
        Ok(())
    }

    /// P03/P06/P09/P10/P10a: convert all durations once, rounding up.
    pub fn bind(&mut self, tick_hz: u64) -> Result<(), String> {
        if tick_hz == 0 {
            return Err("CO5300: tick rate must be nonzero".into());
        }
        if self.state.timing.is_some() {
            return Err("CO5300: already bound".into());
        }
        let ticks = |ns: u128| -> u64 {
            ((ns * u128::from(tick_hz) + 999_999_999) / 1_000_000_000) as u64
        };
        self.state.timing = Some(Timing {
            cycle: ticks(20),
            cs: ticks(10),
            reject_reset: ticks(5_000),
            reset: ticks(10_000),
            command: ticks(5_000_000),
            sleep: ticks(120_000_000),
        });
        Ok(())
    }

    /// P19: 480 by 480 pixels, row-major; unwritten contents are seeded junk (P17).
    pub fn frame(&self) -> &[u16] {
        self.frame.as_slice()
    }

    pub fn display_on(&self) -> bool {
        self.state.r.display_on
    }

    pub fn inversion(&self) -> bool {
        self.state.r.inversion
    }

    pub fn brightness(&self) -> u8 {
        self.state.r.brightness
    }

    /// Make a fresh feed; an earlier reader retains only its last published frame.
    pub fn attach_screen(&mut self) -> ScreenReader {
        let (writer, reader) = screen_feed(WIDTH, HEIGHT, self.glass.map(usize::from));
        self.screen = Some(writer);
        reader
    }

    /// Hands the live feed to an earlier copy; clones carry no writer.
    pub fn take_screen(&mut self) -> Option<ScreenWriter> {
        self.screen.take()
    }

    /// Takes over a feed with the same glass and sends this copy's whole memory next time.
    pub fn put_screen(&mut self, mut writer: ScreenWriter) {
        // A live port's dimensions are fixed. Reattach if restoring a different glass.
        self.screen = (writer.glass() == self.glass.map(usize::from)).then(|| {
            writer.dirty_all();
            writer
        });
    }

    pub(crate) fn publish(&mut self, t: f64) {
        if let Some(writer) = &mut self.screen {
            writer.publish(t, self.frame.as_slice(),
                self.state.r.display_on, self.state.r.brightness, self.state.r.inversion);
        }
    }

    pub(crate) fn mark(&mut self) -> State {
        self.frame.mark();
        self.state
    }

    pub(crate) fn rewind(&mut self, saved: State) {
        self.frame.rewind();
        let display = self.display_state();
        self.state = saved;
        self.display_changed(display);
    }

    fn display_state(&self) -> (bool, u8, bool) {
        (self.display_on(), self.brightness(), self.inversion())
    }

    fn display_changed(&mut self, previous: (bool, u8, bool)) {
        if self.display_state() != previous {
            if let Some(writer) = &mut self.screen { writer.changed(); }
        }
    }

    pub(crate) fn barrier(&self) -> bool {
        self.frame.full()
    }

    fn deadline(t: u64, delay: u64) -> Res<u64> {
        t.checked_add(delay).ok_or("CO5300: timing deadline overflow")
    }

    fn reset(&mut self, t: u64, timing: Timing) -> Res {
        let quiet_until = Self::deadline(t, timing.command)?;
        let sleep_out_allowed = Self::deadline(t, timing.sleep)?;
        let display = self.display_state();
        self.state.r = Registers::default(); // P08/P12–P17/P20: RAM survives; CMD1.
        self.display_changed(display);
        self.state.hardware_reset = true;
        self.state.quiet_until = quiet_until;
        self.state.sleep_out_allowed = sleep_out_allowed; // P06: no SLPOUT for 120 ms.
        self.state.sleep_in_allowed = 0;
        self.state.sleep_out_sequence_until = 0;
        Ok(())
    }

    fn window(&mut self, cx: &mut Cx<'_>, cmd: u8, bytes: [u8; 4]) -> Res {
        let start = u16::from_be_bytes([bytes[0], bytes[1]]);
        let end = u16::from_be_bytes([bytes[2], bytes[3]]);
        if usize::from(end) >= WIDTH {
            return Err("CO5300 window outside 480x480 storage is not modelled (P19/O03)");
        }
        if start > end || start & 1 != 0 || (end - start + 1) & 1 != 0 {
            return Err(if cmd == 0x2a {
                "CO5300 CASET requires start <= end, even start and even width (P15/P18)"
            } else {
                "CO5300 RASET requires start <= end, even start and even height (D-P3/P16/P18)"
            });
        }
        if cmd == 0x2a {
            self.state.r.columns = [start, end];
        } else {
            self.state.r.rows = [start, end];
            if !self.state.row_note {
                cx.note("CO5300 row rule taken from the column rule because the row text is malformed (D-P3/P16)");
                self.state.row_note = true;
            }
        }
        self.state.r.addressed = true;
        self.state.r.next_pixel = None;
        Ok(())
    }

    fn pixels(&mut self, cx: &mut Cx<'_>, cmd: u8, tx: &dyn Payload) -> Res {
        if !self.state.r.spi_wram {
            // P23: the chip drops them. Stop, so an image that never set SPI_WRAM shows why its
            // panel keeps showing old or random RAM.
            return Err("CO5300 RAMWR/RAMWRC with SPI_WRAM clear: SPI writes to frame RAM are disabled (P23)");
        }
        if self.state.r.colmod.is_none() {
            return Err("CO5300 RAMWR/RAMWRC before COLMOD: conflicting reset defaults (D-P2/P08)");
        }
        if !self.state.r.addressed {
            return Err("CO5300 RAMWR/RAMWRC before a location command is undefined (P17)");
        }
        if tx.len() & 1 != 0 {
            return Err("CO5300 partial RGB565 pixel is not documented (P18/O03)");
        }
        let start = if cmd == 0x2c {
            0
        } else {
            self.state.r.next_pixel.ok_or("CO5300 RAMWRC has no preceding pixel position (P17/P18)")?
        };
        let [left, right] = self.state.r.columns;
        let [top, bottom] = self.state.r.rows;
        let width = usize::from(right - left + 1);
        let capacity = width * usize::from(bottom - top + 1);
        let count = tx.len() / 2;
        if count > capacity - start {
            return Err("CO5300 pixels beyond the window have no documented behavior (P18/O03)");
        }
        if !self.state.order_note {
            cx.note("CO5300 pixel order within a window assumed: columns first, then rows, both increasing (P18a; the first board page checks it)");
            self.state.order_note = true;
        }
        let mut position = start;
        let mut high = None;
        tx.chunks(&mut |bytes| {
            for &byte in bytes {
                if let Some(msb) = high.take() {
                    let row = usize::from(top) + position / width;
                    let column = usize::from(left) + position % width;
                    // P05a: big-endian RGB565, including across payload chunk boundaries.
                    self.frame.write(row * WIDTH + column, u16::from_be_bytes([msb, byte]));
                    if let Some(writer) = &mut self.screen { writer.dirty_row(row); }
                    position += 1;
                } else {
                    high = Some(byte);
                }
            }
        });
        self.state.r.next_pixel = Some(position);
        Ok(())
    }

    fn command(&mut self, cx: &mut Cx<'_>, x: &SpiXfer, tx: &dyn Payload, timing: Timing) -> Res {
        let display = self.display_state();
        let cmd = (x.addr.1 >> 8) as u8;
        let parameters = match cmd {
            0x01 | 0x10 | 0x11 | 0x20 | 0x29 => 0,
            0x2a | 0x2b => 4,
            0x3a | 0x51 | 0x53 | 0xc4 => 1,
            0x2c | 0x3c => return self.pixels(cx, cmd, tx),
            _ => return Err(std::str::from_utf8(&CMD_ERRORS[usize::from(cmd)])
                .expect("command errors are ASCII")),
        };
        if tx.len() != parameters {
            return Err("CO5300 command parameter count differs from its documented count (P09–P16/P10a/P23/P24)");
        }
        let mut bytes = [0; 4];
        let mut offset = 0;
        tx.chunks(&mut |chunk| {
            bytes[offset..offset + chunk.len()].copy_from_slice(chunk);
            offset += chunk.len();
        });
        match cmd {
            0x01 => {
                if x.t0 < self.state.sleep_out_sequence_until {
                    return Err("CO5300 SWRESET during the Sleep Out sequence (P09/P10)");
                }
                // D-P1 leaves the sleep state after a reset unknown. P09's 120 ms SLPOUT wait
                // applies only from Sleep Out, so the adverse side assumes Sleep Out.
                let sleep = match self.state.r.sleep {
                    Some(sleep) => sleep,
                    None => {
                        if !self.state.swreset_note {
                            cx.note("CO5300 SWRESET with the sleep state unknown (D-P1) assumes Sleep Out: no SLPOUT for 120 ms (P09)");
                            self.state.swreset_note = true;
                        }
                        Sleep::Out
                    }
                };
                let quiet_until = Self::deadline(x.t1, timing.command)?;
                let sleep_out_allowed = if sleep == Sleep::Out {
                    self.state.sleep_out_allowed.max(Self::deadline(x.t1, timing.sleep)?)
                } else {
                    self.state.sleep_out_allowed
                };
                self.state.r = Registers::default(); // P09/P17/P20: CMD1, RAM retained.
                self.state.quiet_until = quiet_until;
                self.state.sleep_out_allowed = sleep_out_allowed;
            }
            0x10 | 0x11 => {
                let sleep = if cmd == 0x10 { Sleep::In } else { Sleep::Out };
                if self.state.r.sleep == Some(sleep) {
                    // P10/P10a: no state change, but still observe the command's 5 ms wait.
                    self.state.quiet_until = Self::deadline(x.t1, timing.command)?;
                    return Ok(());
                }
                if cmd == 0x10 && x.t0 < self.state.sleep_in_allowed {
                    return Err("CO5300 SLPIN within 120 ms of SLPOUT (P10a)");
                }
                if cmd == 0x11 && x.t0 < self.state.sleep_out_allowed {
                    return Err("CO5300 SLPOUT before the 120 ms reset/SLPIN/SWRESET wait (P06/P09/P10)");
                }
                let quiet_until = Self::deadline(x.t1, timing.command)?;
                let ready = Self::deadline(x.t1, timing.sleep)?;
                self.state.r.sleep = Some(sleep);
                self.state.quiet_until = quiet_until;
                if cmd == 0x10 {
                    self.state.sleep_out_allowed = ready;
                } else {
                    self.state.sleep_in_allowed = ready;
                    self.state.sleep_out_sequence_until = ready;
                }
            }
            0x20 => self.state.r.inversion = false, // P13.
            0x29 => {
                // P24/D-P6: DBV takes effect only with BCTRL set, and its reset default is in conflict.
                if self.state.r.ctrl.is_none_or(|ctrl| ctrl & 0x20 == 0) {
                    return Err("CO5300 DISPON without WRCTRLD's BCTRL set: the luminance is not documented (P24/D-P6)");
                }
                self.state.r.display_on = true; // P14: no invented display-on delay.
            }
            0x2a | 0x2b => self.window(cx, cmd, bytes)?,
            0x3a => {
                if bytes[0] != 0x55 {
                    return Err("CO5300 COLMOD: only inferred RGB565 value 0x55 is modelled (P11)");
                }
                self.state.r.colmod = Some(bytes[0]);
                if !self.state.colmod_note {
                    cx.note("CO5300 COLMOD 0x55 is inferred to select RGB565 (P11)");
                    self.state.colmod_note = true;
                }
            }
            0x51 => self.state.r.brightness = bytes[0], // P12: raw DBV, no interpolation.
            0x53 => {
                if bytes[0] & 0x08 != 0 {
                    return Err("CO5300 WRCTRLD dimming (DD) has no documented transition timing (P24)");
                }
                if self.state.r.display_on && bytes[0] & 0x20 == 0 {
                    return Err("CO5300 WRCTRLD clears BCTRL with the display on: the luminance is not documented (P24)");
                }
                self.state.r.ctrl = Some(bytes[0]);
            }
            0xc4 => {
                if bytes[0] & 0x4c != 0 {
                    return Err("CO5300 SetSPIMode sets a bit the command draws as 0 (P23)");
                }
                self.state.r.spi_wram = bytes[0] & 0x80 != 0;
            }
            _ => unreachable!("the command whitelist was checked"),
        }
        self.display_changed(display);
        Ok(())
    }
}

impl Model for Co5300 {
    fn pins(&self) -> &'static [&'static str] {
        Self::PINS
    }

    fn drive(&self, pin: u8) -> Drive {
        assert!(pin < 7, "CO5300: unknown pin");
        Drive::Input(Pull::None)
    }

    fn pin(&mut self, cx: &mut Cx<'_>, pin: u8, high: bool) -> Res {
        let timing = self.state.timing.ok_or("CO5300: pin change before bind")?;
        match pin {
            0 => {
                if high {
                    if let Some(t) = self.state.last_transfer {
                        if cx.t.checked_sub(t).is_none_or(|elapsed| elapsed < timing.cs) {
                            return Err("CO5300 CSX hold shorter than 10 ns (P03)");
                        }
                    }
                    self.state.cs_low = None;
                    self.state.last_transfer = None;
                } else {
                    self.state.cs_low = Some(cx.t);
                }
            }
            1..=5 => (),
            6 => {
                if !high {
                    self.state.resx_low = Some(cx.t);
                } else if let Some(low) = self.state.resx_low.take() {
                    let width = cx.t.checked_sub(low).ok_or("CO5300 RESX time went backwards")?;
                    if width < timing.reject_reset {
                        return Ok(()); // P06: a rejected pulse leaves state untouched.
                    }
                    if width <= timing.reset {
                        return Err("CO5300 RESX pulse of 5-10 us is a 'Reset Start' with no documented outcome (P06)");
                    }
                    self.reset(cx.t, timing)?;
                }
            }
            _ => return Err("CO5300: unknown pin"),
        }
        Ok(())
    }

    fn next_event(&self) -> Option<u64> {
        None
    }

    fn event(&mut self, _cx: &mut Cx<'_>) -> Res {
        Ok(())
    }

    fn i2c(&mut self, _cx: &mut Cx<'_>, _op: I2cOp) -> Res<I2cAns> {
        Err("CO5300 has no I2C")
    }

    fn spi(&mut self, cx: &mut Cx<'_>, x: &SpiXfer, tx: &dyn Payload, rx: &mut Vec<u8>) -> Res {
        rx.clear();
        if !self.state.hardware_reset {
            return Err("CO5300 command before a hardware reset: the power-on sequence is not modelled (P07)");
        }
        if self.state.resx_low.is_some() {
            return Err("CO5300 command during RESX low has no modelled outcome (P06)");
        }
        let timing = self.state.timing.ok_or("CO5300: SPI before bind")?;
        if x.t0 < self.state.quiet_until {
            return Err("CO5300 command within 5 ms of reset or a sleep command (P06/P09/P10/P10a)");
        }
        if x.t1 < x.t0 || self.state.last_transfer.is_some_and(|t| t > x.t0) {
            return Err("CO5300 SPI transfer time went backwards");
        }
        if x.mode != 0 && x.mode != 3 {
            return Err("CO5300 SPI mode does not sample on rising SCL (P02)");
        }
        if u64::from(x.bit_ticks) < timing.cycle {
            return Err("CO5300 SPI write cycle shorter than 20 ns (P03)");
        }
        if self.state.cs_low.and_then(|t| x.t0.checked_sub(t))
            .is_none_or(|elapsed| elapsed < timing.cs)
        {
            return Err("CO5300 CSX setup shorter than 10 ns (P03)");
        }
        if x.cmd.0 != 8 {
            return Err("CO5300 requires an 8-bit instruction (P01)");
        }
        let lines = match x.cmd.1 {
            0x02 => [1, 1, 1],
            0x32 => [1, 1, 4],
            0x12 => [1, 4, 4],
            0x03 => return Err("CO5300 read turnaround is NOT FOUND (P04/O03)"),
            0x72 | 0x52 => return Err("CO5300 2-wire instructions are not modelled (P05a)"),
            0xff | 0x3b | 0x38 => return Err("CO5300 interface mode changes are not modelled (P01)"),
            _ => return Err("CO5300 SPI instruction is not modelled (P01/P05a)"),
        };
        if x.lines != lines {
            return Err("CO5300 SPI lanes do not match the instruction (P01/P05a)");
        }
        if x.addr.0 != 24 || x.addr.1 & !0x0000ff00 != 0 {
            return Err("CO5300 requires the 24-bit address {0x00, CMD, 0x00} (P01)");
        }
        if x.dummy != 0 {
            return Err("CO5300 write dummy clocks are not documented (P01/P04)");
        }
        if x.lsb_first {
            return Err("CO5300 requires MSB-first serialization (P05a)");
        }
        if x.rx_bits != 0 || x.full_duplex {
            return Err("CO5300 read turnaround is NOT FOUND (P04/O03)");
        }
        if u128::from(x.tx_bits) != tx.len() as u128 * 8 {
            return Err("CO5300 partial-byte or mismatched SPI payload is not modelled (P01/P18)");
        }
        let mut actual = 0usize;
        tx.chunks(&mut |bytes| actual = actual.saturating_add(bytes.len()));
        if actual != tx.len() {
            return Err("CO5300 payload chunks disagree with their declared length");
        }
        self.command(cx, x, tx, timing)?;
        self.state.last_transfer = Some(x.t1);
        if !self.state.mode_note {
            cx.note("CO5300 accepts SPI modes 0 and 3: both sample on rising SCL (P02; no mode table)");
            self.state.mode_note = true;
        }
        Ok(())
    }

    fn input(&mut self, _cx: &mut Cx<'_>, _ev: &Stimulus) -> Res {
        Err("CO5300 takes no stimulus")
    }
}
