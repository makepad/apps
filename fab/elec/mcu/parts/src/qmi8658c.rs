//! QMI8658C register-read mode. Citations are lines in QMI8658C Rev 0.6
//! (qmi8658c.txt) unless they name Rev A (qmi8658c-reva.txt, document 13-52-27,
//! the only one to give the soft reset); M comments record the lead's modelling decisions.

use std::sync::Arc;

use fab_elec_mcu_emu::board::{I2cAns, I2cOp, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, Pull};

use crate::{Cx, I2cTarget, Model, MotionTrack, Stimulus, TargetOp, TrackRead, TrackRef};

// UI map: lines 892–905, 933–969, 1010. Other mapped ranges are cited in documented().
const WHO_AM_I: usize = 0x00;
const REVISION_ID: usize = 0x01;
const CTRL1: usize = 0x02;
const CTRL2: usize = 0x03;
const CTRL3: usize = 0x04;
const CTRL4: usize = 0x05;
const CTRL5: usize = 0x06;
const CTRL6: usize = 0x07;
const CTRL7: usize = 0x08;
const CTRL9: usize = 0x0a;
const FIFO_CTRL: usize = 0x14;
const STATUSINT: usize = 0x2d;
const STATUS0: usize = 0x2e;
const STATUS1: usize = 0x2f;
const TIMESTAMP: usize = 0x30;
const TEMP: usize = 0x33;
const AX: usize = 0x35;
const GX: usize = 0x3b;
const RESET: usize = 0x60;
/// dQY_L, which reads 0x80 after a successful power-on or soft reset (Rev A lines 861, 1321–1322, 1749–1752).
const RESET_DONE: usize = 0x4d;

// Res carries static text; retain the offending register without allocation.
static REGISTER_ERRORS: [[u8; b"QMI8658C: register 0x00 reserved, undocumented or invalid access".len()]; 256] = {
    let mut errors = [*b"QMI8658C: register 0x00 reserved, undocumented or invalid access"; 256];
    let hex = b"0123456789ABCDEF";
    let mut reg = 0;
    while reg < 256 {
        errors[reg][b"QMI8658C: register 0x".len()] = hex[reg >> 4];
        errors[reg][b"QMI8658C: register 0x0".len()] = hex[reg & 15];
        reg += 1;
    }
    errors
};

fn register_error(reg: u8) -> &'static str {
    std::str::from_utf8(&REGISTER_ERRORS[usize::from(reg)]).expect("register errors are ASCII")
}

fn documented(reg: u8) -> bool {
    // Lines 892–931, 933–1008, 1010; CTRL8 is reserved (1281–1283).
    matches!(reg, 0x00..=0x08 | 0x0a..=0x17 | 0x2c..=0x46 | 0x49..=0x58 | 0x60)
}

// Rates in quarter-Hz preserve 62.5 and 31.25 exactly (lines 1133–1148, 1172–1187).
fn rate(config: u8, gyro: bool) -> Res<u32> {
    match config & 0x0f {
        code @ 0..=8 => Ok(32_000 >> code),
        12 if !gyro => Ok(512),
        13 if !gyro => Ok(84),
        14 if !gyro => Ok(44),
        15 if !gyro => Ok(12),
        _ if gyro => Err("QMI8658C: reserved CTRL3 ODR (lines 1181–1187)"),
        _ => Err("QMI8658C: reserved CTRL2 ODR (lines 1142–1144)"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sampling {
    origin: u64,
    rate: u32,
    // Rational deadlines relative to origin, in ticks * rate * 1000.
    first: u128,
    period: u128,
    stop: Option<u128>,
    published: u128,
}

impl Sampling {
    fn new(origin: u64, tick_hz: u64, rate: u32, gyro: bool) -> Self {
        // M: Assumed typical wake times, 3 ms / 60 ms, plus 3/ODR settling
        // (lines 2110–2119). No output is published before that entire delay.
        let wake_ms = if gyro { 60 } else { 3 };
        Self {
            origin,
            rate,
            first: u128::from(tick_hz) * (wake_ms * u128::from(rate) + 12_000),
            period: u128::from(tick_hz) * 4_000,
            stop: None,
            published: 0,
        }
    }

    fn elapsed(&self, t: u64) -> u128 {
        u128::from(t - self.origin) * u128::from(self.rate) * 1_000
    }

    fn disable(&mut self, t: u64) {
        // M: Use the full adverse 2/ODR shutdown interval (lines 2093–2096),
        // including a sample exactly at its end. Retain the existing phase.
        self.stop = Some(self.elapsed(t) + 2 * self.period);
    }

    fn catch_up(&mut self, t: u64) -> u128 {
        let mut elapsed = self.elapsed(t);
        if let Some(stop) = self.stop {
            elapsed = elapsed.min(stop);
        }
        let count = if elapsed < self.first { 0 } else { 1 + (elapsed - self.first) / self.period };
        let added = count - self.published;
        self.published = count;
        added
    }

    fn stopped(&self, t: u64) -> bool {
        self.stop.is_some_and(|stop| self.elapsed(t) >= stop)
    }

    fn instant_ps(&self, n: u128, tick_hz: u64) -> u128 {
        let e = self.first + (n - 1) * self.period;
        let d = u128::from(self.rate) * 1_000;
        let q = u128::from(self.origin) + e / d;
        let r = e % d;
        let a = q * 1_000_000_000_000;
        let tick_hz = u128::from(tick_hz);
        // This is exactly floor((q + r / d) * 10^12 / tick_hz).
        a / tick_hz + ((a % tick_hz) * d + r * 1_000_000_000_000) / (d * tick_hz)
    }
}

#[derive(Clone, Copy, Debug)]
enum Note {
    Startup,
    SoftReset,
    Pulse,
    StatusInt,
    Start,
    Stop,
    StatusClear,
    Rate,
    Flat,
    Temperature,
    Filter,
    Burst,
    TimestampTie,
    Endian,
    Track,
    Short,
}

/// Small, fully rewindable state; sensor samples never enter the event queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Qmi8658c {
    target: I2cTarget,
    regs: [u8; RESET + 1],
    pointer: Option<u8>,
    command: bool,
    advanced: bool,
    tick_hz: Option<u64>,
    startup: u64,
    pulse_end: u64,
    interrupt_event: Option<u64>,
    int1: bool,
    sensors: [Option<Sampling>; 2],
    accel_ug: [i32; 3],
    gyro_mdps: [i32; 3],
    motion_set: bool,
    track: Option<TrackRef>,
    output_locked: bool,
    last_access: u64,
    notes: u16,
}

impl Qmi8658c {
    /// Its model pins: interrupts 1847–1854, I2C 2567–2583. The address strap is in PartKind.
    pub const PINS: &'static [&'static str] = &["SCL", "SDA", "INT1", "INT2"];

    pub fn new(addr: u8) -> Result<Self, String> {
        // SA0 low/high: lines 2571–2579.
        if !matches!(addr, 0x6a | 0x6b) {
            return Err("QMI8658C: address must be 0x6a or 0x6b (lines 2571–2579)".into());
        }
        Ok(Self {
            target: I2cTarget::new(addr),
            regs: Self::reset_regs(),
            pointer: None,
            command: false,
            advanced: false,
            tick_hz: None,
            startup: 0,
            pulse_end: 0,
            interrupt_event: None,
            int1: false,
            sensors: [None; 2],
            // M: Assumed face-up +Z is +1g; the supplied Figure 7 text
            // (lines 430–447) defines positive axes but does not show the drawing.
            accel_ug: [0, 0, 1_000_000],
            gyro_mdps: [0; 3],
            motion_set: false,
            track: None,
            output_locked: false,
            last_access: 0,
            notes: 0,
        })
    }

    /// All mapped reset values are zero except WHO_AM_I, CTRL1, RESET_DONE and the
    /// conflicting REVISION_ID (lines 892–1010). Never return its placeholder.
    fn reset_regs() -> [u8; RESET + 1] {
        let mut regs = [0; RESET + 1];
        regs[WHO_AM_I] = 0x05; // Lines 892, 1096–1098.
        regs[CTRL1] = 0x20; // Lines 895, 1111–1119: BE=1, AI=0.
        regs[RESET_DONE] = 0x80;
        regs
    }

    /// The soft reset (Rev A 7.4, lines 1736–1752): UI registers to their reset values, sensors
    /// off, then about 200 us startup and at most 15 ms initialization with no access (3.3.1,
    /// lines 402–413; t0, 1656–1657), and INT1's ready pulse as at power-on (1855–1860).
    fn soft_reset(&mut self, cx: &mut Cx<'_>) -> Res {
        let tick_hz = self.tick_hz.ok_or("QMI8658C: access before bind")?;
        self.note(cx, Note::SoftReset, "QMI8658C: Assumed exactly 200 us startup then the 15 ms maximum after a soft reset (Rev A lines 404–409, 1745)");
        let busy = (u128::from(tick_hz) * 15_200).div_ceil(1_000_000);
        let width = (u128::from(tick_hz) * 4).div_ceil(1_000);
        let startup = u128::from(cx.t) + busy;
        self.startup = u64::try_from(startup).map_err(|_| "QMI8658C: soft reset tick overflow")?;
        self.pulse_end = u64::try_from(startup + width).map_err(|_| "QMI8658C: soft reset tick overflow")?;
        self.interrupt_event = Some(self.startup);
        self.regs = Self::reset_regs();
        self.pointer = None;
        self.advanced = false;
        self.int1 = false;
        self.sensors = [None; 2];
        self.output_locked = false;
        Ok(())
    }

    /// Supplies the motion record to read at each sensor's publication instants.
    pub fn attach_track(&mut self, track: Arc<MotionTrack>) {
        self.track = Some(TrackRef(track));
    }

    pub fn bind(&mut self, tick_hz: u64) -> Result<(), String> {
        if tick_hz == 0 {
            return Err("QMI8658C: tick rate must be nonzero".into());
        }
        if self.tick_hz.is_some() {
            return Err("QMI8658C: already bound".into());
        }
        // M: Power-on is tick zero; use the specified 1.75 s startup
        // (lines 2099–2109; typical in Tables 7/8, lines 563, 600).
        let startup = (u128::from(tick_hz) * 7).div_ceil(4);
        // M: Assumed exactly 4 ms for "~4 ms" (lines 1855–1860).
        let width = (u128::from(tick_hz) * 4).div_ceil(1_000);
        self.pulse_end = u64::try_from(startup + width)
            .map_err(|_| "QMI8658C: power-on tick overflow")?;
        self.startup = startup as u64;
        self.tick_hz = Some(tick_hz);
        self.interrupt_event = Some(self.startup);
        Ok(())
    }

    fn note(&mut self, cx: &mut Cx<'_>, note: Note, what: &'static str) {
        let mask = 1 << note as u16;
        if self.notes & mask == 0 {
            cx.note(what);
            self.notes |= mask;
        }
    }

    fn catch_up(&mut self, cx: &mut Cx<'_>) -> Res {
        let tick_hz = self.tick_hz.ok_or("QMI8658C: access before bind")?;
        if cx.t < self.last_access {
            return Err("QMI8658C: time moved backwards without rewind");
        }
        let enabled = self.regs[CTRL7] & 3; // aEN/gEN, lines 1277–1280.
        let rates = [rate(self.regs[CTRL2], false)?, rate(self.regs[CTRL3], true)?];
        let fastest = match enabled {
            1 => Some(0),
            2 => Some(1),
            3 => Some(usize::from(rates[1] > rates[0])),
            _ => None,
        };
        // Resolve both instants before consuming either clock.
        let mut sensors = self.sensors;
        let mut counts = [0; 2];
        let mut tracked = [None; 2];
        let mut short = false;
        for (index, count) in counts.iter_mut().enumerate() {
            if let Some(sensor) = &mut sensors[index] {
                *count = sensor.catch_up(cx.t);
                if *count != 0 && !self.output_locked {
                    if let Some(track) = &self.track {
                        let sample = match track.0.at(sensor.instant_ps(sensor.published, tick_hz)) {
                            TrackRead::Sample(sample) => Some(sample),
                            TrackRead::Before => None,
                            // A live run's hold keeps every read within the track
                            // (`fab_elec_sim::input`): a short read is that hold's bug. The
                            // sensor keeps the last written sample rather than fail the run,
                            // and the track counts the read.
                            TrackRead::Ahead => {
                                short = true;
                                track.0.read_short()
                            }
                        };
                        tracked[index] = sample.map(|sample| if index == 0 { sample.accel_ug } else { sample.gyro_mdps });
                    }
                }
                if sensor.stopped(cx.t) {
                    sensors[index] = None;
                }
            }
        }
        if short {
            self.note(cx, Note::Short, "QMI8658C: motion track short of the sample instant (a live-run hold bug); the sensor keeps its last sample");
        }
        self.last_access = cx.t;
        self.sensors = sensors;
        for (index, &count) in counts.iter().enumerate() {
            // syncSmpl stops output refresh, not the sensors' ODR clocks
            // (lines 1887–1900). Consume elapsed samples even while locked;
            // the next ODR publication after release refreshes the registers.
            if count != 0 && !self.output_locked {
                let (held, scale, unit, base) = if index == 0 {
                    // Lines 545–551, 1123–1129: 16384, 8192, 4096, 2048 LSB/g.
                    (self.accel_ug, 16_384 >> ((self.regs[CTRL2] >> 4) & 3), 1_000_000, AX)
                } else {
                    // Lines 583–594, 1159–1168: 2048 through 16 LSB/dps.
                    (self.gyro_mdps, 2_048 >> ((self.regs[CTRL3] >> 4) & 7), 1_000, GX)
                };
                let values = if let Some(values) = tracked[index] {
                    self.note(cx, Note::Track, "QMI8658C: motion from a track, read at its ODR instants");
                    values
                } else {
                    if !self.motion_set {
                        self.note(cx, Note::Flat, "QMI8658C: Assumed face-up default +1g on Z, zero rotation (Figure 7, lines 430–447)");
                    }
                    held
                };
                for (axis, input) in values.into_iter().enumerate() {
                    // Brief's deterministic quantisation; signed 16-bit data (line 948).
                    let product = i64::from(input) * i64::from(scale);
                    let rounded = product.signum() * ((product.abs() + unit / 2) / unit);
                    let word = rounded.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
                    self.regs[base + axis * 2..base + axis * 2 + 2].copy_from_slice(&word.to_le_bytes());
                }
                self.regs[STATUS0] |= 1 << index; // aDA/gDA, lines 1397–1404.
            }
        }
        if !self.output_locked && counts.iter().any(|&count| count != 0) {
            // M: Constant 25 C; no thermal stimulus. Host scale is 256 LSB/C
            // (lines 847–853), not the internal compensation's 16 LSB/C.
            // Assumed temperature becomes valid with the first published motion
            // sample (lines 838–853); otherwise preserve its reset/last value.
            self.regs[TEMP..TEMP + 2].copy_from_slice(&(25_i16 * 256).to_le_bytes());
            self.note(cx, Note::Temperature, "QMI8658C: Assumed constant 25 C, published with motion data (lines 838–853)");
        }
        if let Some(index) = fastest {
            // TIMESTAMP counts sensor samples (1415–1426), even while
            // syncSmpl prevents refresh of the sensor data bytes (1891–1900).
            if enabled == 3 && rates[0] == rates[1] && counts[index] != 0 {
                // M: Assumed accel wins equal-ODR ties. Lines 1415–1426 do not
                // select a source for equal rates with different wake-up phases.
                // For simultaneous enables its earlier wake advances the counter
                // sooner (the adverse side for wrap); never sum the two streams.
                self.note(cx, Note::TimestampTie, "QMI8658C: Assumed accel clocks TIMESTAMP at equal ODR (lines 1415–1426)");
            }
            // M: Count only the highest-ODR CTRL7-enabled sensor's published
            // samples, not disabled shutdown tails; wrap at 24 bits (1415–1426).
            let old = u32::from_le_bytes([
                self.regs[TIMESTAMP], self.regs[TIMESTAMP + 1], self.regs[TIMESTAMP + 2], 0,
            ]);
            let next = ((u128::from(old) + counts[index]) & 0xff_ffff) as u32;
            self.regs[TIMESTAMP..TIMESTAMP + 3].copy_from_slice(&next.to_le_bytes()[..3]);
        }
        Ok(())
    }

    fn accessible(&mut self, cx: &mut Cx<'_>) -> Res {
        self.note(cx, Note::Startup, "QMI8658C: Assumed 1.75 s power-on startup from tick zero (lines 563, 600, 2099–2109)");
        // M: No documented response during startup (lines 2099–2109) or a soft reset's
        // initialization (Rev A lines 408–409); refuse it.
        if cx.t < self.startup {
            return Err("QMI8658C: accessed during power-on startup or a soft reset");
        }
        Ok(())
    }

    fn current(&mut self, cx: &mut Cx<'_>) -> Res<u8> {
        let reg = self.pointer.ok_or("QMI8658C: register pointer power-on value undocumented; set it before access")?;
        if !documented(reg) {
            if self.advanced {
                // M: No wrap convention in lines 1113–1114 or the map (887–1010).
                self.note(cx, Note::Burst, "QMI8658C: burst reached an undocumented register; wrapping is not specified (lines 887–1010, 1113–1114)");
            }
            return Err(register_error(reg));
        }
        Ok(reg)
    }

    fn advance(&mut self) {
        // M: CTRL1 bit 6 is authoritative (1113–1114), reset 0 repeats the
        // selected register. Address-bit-7 increment (2580–2583) is refused.
        if self.regs[CTRL1] & 0x40 != 0 {
            self.pointer = self.pointer.map(|reg| reg + 1);
            self.advanced = true;
        }
    }

    fn read(&mut self, cx: &mut Cx<'_>) -> Res<u8> {
        let reg = self.current(cx)?;
        let index = usize::from(reg);
        let value = match index {
            // M: Conflicting revision values; wait for a board read (893, 1099).
            REVISION_ID => return Err("QMI8658C: REVISION_ID conflicts in Rev 0.6 (0x68 vs 0x79); awaiting a board read (lines 893, 1099)"),
            RESET => return Err(register_error(reg)), // Write-only, line 1010.
            STATUSINT => {
                // M: Assumed bit 0 is zero because INT2 is not modelled;
                // bit 1 mirrors the actual INT1 level (1376–1383, 1851–1854).
                self.note(cx, Note::StatusInt, "QMI8658C: Assumed STATUSINT bit 0 is zero; bit 1 mirrors INT1 (lines 1376–1383)");
                u8::from(self.int1) << 1
            }
            STATUS0 => {
                if self.regs[CTRL7] & 0x80 != 0 {
                    // CTRL7 bit 7 selects syncSmpl (1259–1262); STATUS0
                    // freezes output until the last data byte (1887–1900).
                    self.output_locked = true;
                } else {
                    // M: Assumed this read clears the flags it returns in syncSmpl=0;
                    // lines 1397–1404 say 'last read' without naming the register.
                    self.note(cx, Note::StatusClear, "QMI8658C: Assumed STATUS0 read clears returned flags in syncSmpl=0 (lines 1397–1404)");
                }
                let flags = self.regs[STATUS0];
                self.regs[STATUS0] = 0;
                flags
            }
            STATUS1 => {
                self.int1 = false; // Reading STATUS1 clears INT1 (1859–1860).
                self.interrupt_event = None;
                self.regs[STATUS1]
            }
            _ => {
                // M: Follow the lead's serial-byte-order decision for I2C too:
                // CTRL1 bit 5 resets to 1 (big endian), 0 selects little endian
                // (1115–1116). The source labels it SPI_BE / 'SPI read data'.
                let base = match index {
                    0x33..=0x46 => Some(0x33), // TEMP, accel, gyro, mag (949–983).
                    0x49..=0x56 => Some(0x49), // AE 16-bit outputs (984–1006).
                    _ => None,
                };
                if let Some(base) = base {
                    self.note(cx, Note::Endian, "QMI8658C: lead decision applies CTRL1 SPI_BE byte order to I2C data (lines 1115–1116)");
                    let index = if self.regs[CTRL1] & 0x20 != 0 { base + ((index - base) ^ 1) } else { index };
                    self.regs[index]
                } else {
                    self.regs[index]
                }
            }
        };
        // Last enabled sensor byte: GZ_H (0x40), or AZ_H (0x3a) when
        // only accel is enabled (map 948–969). Release after returning that
        // byte, including its endian selection (1897–1900).
        let last = if self.regs[CTRL7] & 2 != 0 { 0x40 } else { 0x3a };
        if index == last { self.output_locked = false; }
        self.advance();
        Ok(value)
    }

    fn configuration(accel: u8, enabled: u8) -> Res {
        // M: Enforce both operating-mode constraints, not just valid ODR codes:
        // high accel rates require 6DOF (657–660); low power excludes gyro (1205).
        if enabled & 1 != 0 {
            if (accel & 0x0f) <= 2 && enabled & 2 == 0 {
                return Err("QMI8658C: accel high ODR requires both sensors (lines 657–660)");
            }
            if (accel & 0x0f) >= 12 && enabled & 2 != 0 {
                return Err("QMI8658C: accel low-power mode requires gyro off (line 1205)");
            }
        }
        Ok(())
    }

    fn write(&mut self, cx: &mut Cx<'_>, value: u8) -> Res {
        let reg = self.current(cx)?;
        let index = usize::from(reg);
        match index {
            CTRL1 => {
                if value & 0x1e != 0 { return Err(register_error(reg)); } // 1117.
                if value & 1 != 0 { return Err("QMI8658C: oscillator disable not modelled"); } // 1118–1119.
                // SIM (1111–1112) is stored; SPI transfers themselves are refused.
            }
            CTRL2 | CTRL3 => {
                let gyro = index == CTRL3;
                if value & 0x80 != 0 { return Err("QMI8658C: self test not modelled"); } // 1122, 1158.
                if !gyro && value & 0x40 != 0 { return Err(register_error(reg)); } // 1129.
                rate(value, gyro)?;
                Self::configuration(if gyro { self.regs[CTRL2] } else { value }, self.regs[CTRL7])?;
                // M: The source specifies enable/disable delays (2093–2119),
                // not live ODR rephasing. Refuse that transition rather than invent it.
                if self.sensors[usize::from(gyro)].is_some() && (value ^ self.regs[index]) & 0x0f != 0 {
                    return Err("QMI8658C: ODR change while sampling or stopping not modelled; disable and wait 2/ODR");
                }
            }
            CTRL4 => {
                if value != 0 { return Err("QMI8658C: magnetometer not modelled"); } // 1188–1203.
            }
            CTRL5 => {
                if value & 0x88 != 0 { return Err(register_error(reg)); } // 1215, 1226.
                if value & 0x11 != 0 { // LPF enables, 1222–1236.
                    // M: Accept the documented filter settings (1213–1236),
                    // but omit response as permitted by the brief; expose raw samples.
                    self.note(cx, Note::Filter, "QMI8658C: filter response not modelled; outputs are unfiltered");
                }
            }
            CTRL6 => {
                if value & 0x80 != 0 { return Err("QMI8658C: motion on demand not modelled"); } // 1239–1240.
                if value != 0 { return Err("QMI8658C: AttitudeEngine not modelled"); } // 1237–1251.
            }
            CTRL7 => {
                if value & 0x40 != 0 { return Err("QMI8658C: high-speed internal clock not modelled"); } // 1263–1264.
                if value & 0x20 != 0 { return Err(register_error(reg)); } // 1265.
                if value & 0x10 != 0 { return Err("QMI8658C: gyro snooze not modelled"); } // 1266–1269.
                if value & 8 != 0 { return Err("QMI8658C: AttitudeEngine not modelled"); } // 1270–1274.
                if value & 4 != 0 { return Err("QMI8658C: magnetometer not modelled"); } // 1275–1276.
                Self::configuration(self.regs[CTRL2], value)?;
                let changed = value ^ self.regs[CTRL7];
                // Validate both sensors before changing either.
                for index in 0..2 {
                    if changed & value & (1 << index) != 0 && self.sensors[index].is_some() {
                        return Err("QMI8658C: re-enable during 2/ODR shutdown not modelled (lines 2093–2096)");
                    }
                }
                if value & 0x80 == 0 { self.output_locked = false; }
                for index in 0..2 {
                    if changed & (1 << index) == 0 { continue; }
                    if value & (1 << index) != 0 {
                        let rate = rate(self.regs[CTRL2 + index], index == 1)?;
                        self.sensors[index] = Some(Sampling::new(cx.t, self.tick_hz.expect("bound access"), rate, index == 1));
                        self.note(cx, Note::Start, "QMI8658C: Assumed wake-up 3 ms accel / 60 ms gyro plus 3/ODR settling (lines 2110–2119)");
                        // M: Use nominal rates despite the ±5% tolerance (654;
                        // rate codes 1133–1148, 1172–1187). Rational deadlines
                        // round up to ticks without accumulating period rounding.
                        self.note(cx, Note::Rate, "QMI8658C: Assumed nominal ODR, deadlines rounded up to ticks (line 654)");
                    } else if let Some(sensor) = &mut self.sensors[index] {
                        sensor.disable(cx.t);
                        self.note(cx, Note::Stop, "QMI8658C: shutdown publishes through the full adverse 2/ODR interval (lines 2093–2096)");
                    }
                }
            }
            CTRL9 => return Err("QMI8658C: CTRL9 host commands not modelled"), // 1285–1286.
            0x0b..=0x13 => (), // Calibration storage and watermark (907–927); no CTRL9 execution.
            FIFO_CTRL => {
                if value & 0xf0 != 0 || value & 3 != 0 { return Err("QMI8658C: FIFO not modelled"); }
                // Size bits may be stored while in bypass (1298–1317).
            }
            // Rev A Table 27 (lines 1314–1323): 0xB0 resets from any mode. Its 7.4 text (line
            // 1741) says 0x0B against the table; the handheld's chip took 0xB0 and answered RESET_DONE.
            // M: Assumed the command byte is acknowledged; Rev A does not say.
            RESET => {
                if value != 0xb0 {
                    return Err("QMI8658C: RESET takes 0xB0 (Rev A Table 27, as the chip does; its 7.4 text's 0x0B is not a reset)");
                }
                return self.soft_reset(cx);
            }
            _ => return Err(register_error(reg)),
        }
        self.regs[index] = value;
        self.advance();
        Ok(())
    }
}

impl Model for Qmi8658c {
    fn pins(&self) -> &'static [&'static str] {
        Self::PINS
    }

    fn drive(&self, pin: u8) -> Drive {
        match pin {
            0 | 1 => Drive::Input(Pull::None),
            2 if self.int1 => Drive::High, // INT1 asserted high (1857–1860).
            // M: INT2 pulses lack a width (1870–1886); drive low and have
            // Netlist reject every connection to another pin. INT1 idle is low.
            2 | 3 => Drive::Low,
            _ => panic!("QMI8658C: unknown pin"),
        }
    }

    fn pin(&mut self, cx: &mut Cx<'_>, pin: u8, _high: bool) -> Res {
        if cx.initializing {
            self.note(cx, Note::Startup, "QMI8658C: Assumed 1.75 s power-on startup from tick zero (lines 563, 600, 2099–2109)");
        }
        if pin < 4 { Ok(()) } else { Err("QMI8658C: unknown pin") }
    }

    fn next_event(&self) -> Option<u64> {
        self.interrupt_event
    }

    fn event(&mut self, cx: &mut Cx<'_>) -> Res {
        if self.interrupt_event == Some(cx.t) {
            if cx.t == self.startup {
                self.int1 = true;
                self.interrupt_event = Some(self.pulse_end);
                self.note(cx, Note::Pulse, "QMI8658C: Assumed exactly 4 ms startup INT1 pulse (lines 1855–1860)");
            } else {
                self.int1 = false;
                self.interrupt_event = None;
            }
        }
        Ok(())
    }

    fn i2c_addr(&self) -> Option<u8> {
        Some(self.target.addr())
    }

    fn i2c(&mut self, cx: &mut Cx<'_>, op: I2cOp) -> Res<I2cAns> {
        let selected = self.target.step(op)?;
        if selected == TargetOp::None { return Ok(I2cAns::None); }
        self.catch_up(cx)?;
        self.accessible(cx)?;
        match selected {
            TargetOp::None => Ok(I2cAns::None),
            TargetOp::Address { read } => {
                self.command = !read;
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Write(value) => {
                if self.command {
                    // M: Refuse the incompatible address-bit-7 convention (2580–2583).
                    if value & 0x80 != 0 {
                        return Err("QMI8658C: address bit 7 increment convention (line 2580) conflicts with CTRL1 bit 6; not modelled");
                    }
                    if !documented(value) { return Err(register_error(value)); }
                    self.pointer = Some(value);
                    self.advanced = false;
                    self.command = false;
                } else {
                    self.write(cx, value)?;
                }
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Read { .. } => self.read(cx).map(I2cAns::Byte),
        }
    }

    fn input(&mut self, cx: &mut Cx<'_>, ev: &Stimulus) -> Res {
        if let Stimulus::Track { track, .. } = ev {
            // every sample due through this tick is read from what the sensor had before
            self.catch_up(cx)?;
            self.track = Some(track.clone());
            return Ok(());
        }
        let Stimulus::Motion { accel_ug, gyro_mdps, .. } = *ev else {
            return Err("QMI8658C takes only motion stimulus");
        };
        // Account for every old sample (the track's or the old stimulus's) through this tick before
        // replacement. The newest input wins: a stimulus ends the track the sensor read.
        self.catch_up(cx)?;
        self.track = None;
        self.accel_ug = accel_ug;
        self.gyro_mdps = gyro_mdps;
        self.motion_set = true;
        Ok(())
    }

    fn spi(&mut self, _cx: &mut Cx<'_>, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("QMI8658C: SPI not modelled")
    }
}
