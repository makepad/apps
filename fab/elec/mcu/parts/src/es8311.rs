//! ES8311 I2C control and slave serial audio ports.

use fab_elec_mcu_emu::board::{I2cAns, I2cOp, I2sFrame, I2sStandard, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, Pull};

use crate::{Cx, I2cTarget, Model, Stimulus, TargetOp, TrackRead, TrackRef};

struct Register {
    reset: u8,
    writable: u8,
    invalid_bits: &'static str,
}

macro_rules! registers {
    ($($addr:literal => ($reset:literal, $mask:literal)),* $(,)?) => {
        fn register_spec(addr: u8) -> Option<Register> {
            match addr {
                $($addr => Some(Register {
                    reset: $reset,
                    writable: $mask,
                    invalid_bits: concat!("ES8311: register ", stringify!($addr), ", reserved or internal-use bit set"),
                }),)*
                _ => None,
            }
        }
    };
}

fn register(cx: &mut Cx<'_>, addr: u8) -> Res<Register> {
    register_spec(addr).ok_or_else(|| cx.refuse(
        "ES8311: undocumented register (es8311-datasheet.txt:653-1425)",
        format!("ES8311: register {addr:#04x} is undocumented"),
    ))
}

registers! {
    0x00 => (0x1f, 0xff), // es8311-datasheet.txt:654-678 reset, CSM, MSC and sequencer.
    0x01 => (0x00, 0xff), // es8311-datasheet.txt:679-714 clock source, polarity and gates.
    0x02 => (0x00, 0xff), // es8311-datasheet.txt:715-731 input divider, multiplier and doubler.
    0x03 => (0x10, 0x7f), // es8311-datasheet.txt:732-747 ADC speed and oversampling.
    0x04 => (0x10, 0x7f), // es8311-datasheet.txt:748-759 DAC oversampling.
    0x05 => (0x00, 0xff), // es8311-datasheet.txt:764-771 ADC and DAC clock dividers.
    0x06 => (0x03, 0x7f), // es8311-datasheet.txt:772-795 BCLK settings.
    0x07 => (0x00, 0x3f), // es8311-datasheet.txt:796-807 tri-state and high divider nibble.
    0x08 => (0xff, 0xff), // es8311-datasheet.txt:808-818 low LRCK divider byte.
    0x09 => (0x00, 0xff), // es8311-datasheet.txt:819-841 DAC serial port.
    0x0a => (0x00, 0x7f), // es8311-datasheet.txt:842-861 ADC serial port.
    0x0b => (0x00, 0xff), // es8311-datasheet.txt:862-875 power-up stages A and B.
    0x0c => (0x20, 0xff), // es8311-datasheet.txt:876-885 power-up stages B and C.
    0x0d => (0xfc, 0xff), // es8311-datasheet.txt:886-903 analog power and VMID.
    0x0e => (0x6a, 0x7c), // es8311-datasheet.txt:904-915 PGA, modulator and references.
    0x0f => (0x00, 0xff), // es8311-datasheet.txt:920-937 low-power switches.
    0x10 => (0x13, 0xff), // es8311-datasheet.txt:938-955 sync, bias and voltage settings.
    0x11 => (0x7c, 0x00), // es8311-datasheet.txt:956-958 VSEL is internal use.
    0x12 => (0x02, 0x03), // es8311-datasheet.txt:959-964 DAC and output reference enables.
    0x13 => (0x40, 0x10), // es8311-datasheet.txt:969-972 headphone switch.
    0x14 => (0x10, 0x5f), // es8311-datasheet.txt:973-991 microphone select and PGA gain.
    0x15 => (0x00, 0xf1), // es8311-datasheet.txt:992-1013 ADC ramp and DMIC sense.
    0x16 => (0x04, 0x3f), // es8311-datasheet.txt:1014-1035 ADC sync, invert, RAM clear and scale.
    0x17 => (0x00, 0xff), // es8311-datasheet.txt:1036-1047 ADC volume.
    0x18 => (0x00, 0xcf), // es8311-datasheet.txt:1048-1060 ALC, automute and window.
    0x19 => (0x00, 0xff), // es8311-datasheet.txt:1061-1100 ALC levels.
    0x1a => (0x00, 0xff), // es8311-datasheet.txt:1101-1129 automute window and noise gate.
    0x1b => (0x0c, 0xff), // es8311-datasheet.txt:1130-1138 mute gain and HPF coefficient.
    0x1c => (0x4c, 0x7f), // es8311-datasheet.txt:1139-1147 ADC EQ bypass and HPF.
    // es8311-datasheet.txt:1148-1215 ADC EQ coefficients B0, A1, A2, B1 and B2.
    0x1d => (0, 0x3f), 0x1e => (0, 0xff), 0x1f => (0, 0xff), 0x20 => (0, 0xff),
    0x21 => (0, 0x3f), 0x22 => (0, 0xff), 0x23 => (0, 0xff), 0x24 => (0, 0xff),
    0x25 => (0, 0x3f), 0x26 => (0, 0xff), 0x27 => (0, 0xff), 0x28 => (0, 0xff),
    0x29 => (0, 0x3f), 0x2a => (0, 0xff), 0x2b => (0, 0xff), 0x2c => (0, 0xff),
    0x2d => (0, 0x3f), 0x2e => (0, 0xff), 0x2f => (0, 0xff), 0x30 => (0, 0xff),
    0x31 => (0x00, 0xfc), // es8311-datasheet.txt:1216-1235 DAC mute, invert, clear and dither.
    0x32 => (0x00, 0xff), // es8311-datasheet.txt:1236-1247 DAC volume.
    0x33 => (0x00, 0xff), // es8311-datasheet.txt:1248-1250 DAC offset.
    0x34 => (0x00, 0x8f), // es8311-datasheet.txt:1255-1265 DRC and window.
    0x35 => (0x00, 0xff), // es8311-datasheet.txt:1266-1301 DRC levels.
    0x36 => (0x00, 0x00), // es8311-datasheet.txt:1302-1308 gives no field definitions.
    0x37 => (0x08, 0xf8), // es8311-datasheet.txt:1308-1329 DAC ramp and EQ bypass.
    // es8311-datasheet.txt:1330-1369 DAC EQ coefficients B0, B1 and A1.
    0x38 => (0, 0x3f), 0x39 => (0, 0xff), 0x3a => (0, 0xff), 0x3b => (0, 0xff),
    0x3c => (0, 0x3f), 0x3d => (0, 0xff), 0x3e => (0, 0xff), 0x3f => (0, 0xff),
    0x40 => (0, 0x3f), 0x41 => (0, 0xff), 0x42 => (0, 0xff), 0x43 => (0, 0xff),
    0x44 => (0x00, 0xf0), // es8311-datasheet.txt:1370-1385 feedback and internal controls.
    0x45 => (0x00, 0x01), // es8311-datasheet.txt:1386-1398 internal controls and clock pull-ups.
    0xfa => (0x00, 0x01), // es8311-datasheet.txt:1399-1404 register initialization.
    0xfc => (0x00, 0x00), // es8311-datasheet.txt:1405-1416 status flags.
    0xfd => (0x83, 0x00), // es8311-datasheet.txt:1417-1419 chip ID high.
    0xfe => (0x11, 0x00), // es8311-datasheet.txt:1420-1422 chip ID low.
    0xff => (0x00, 0x00), // es8311-datasheet.txt:1423-1425 chip revision.
}

#[derive(Clone, Copy)]
enum Note {
    BusRate,
    ClockSelect,
    AnalogClocks,
    DigitalReset,
    RegisterReset,
    FrameDivider,
    Multiplier,
    AdcOsr,
    PowerStages,
    ReservedReset,
    AdcVolume,
    AdcEq,
    DacVolume,
    DacEq,
    Silence,
    Speaker,
    DacProcessing,
    WireTiming,
    Sequence,
    ManualPower,
    OsrTable,
    DacRate,
    References,
    DacOsr,
    AdcProcessing,
    SoundRate,
    SoundShort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Es8311 {
    control: Control,
    /// The DAC's output for the frame just clocked, at full scale, until the board takes it.
    out: Option<i16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Control {
    target: I2cTarget,
    regs: [u8; 256],
    pointer: Option<u8>,
    expects_pointer: bool,
    reset_flags_known: bool,
    notes: u32,
    tick_hz: u64,
    sequence_since: u64,
    sequence_ready: Option<u64>,
    validated: Option<(Stream, u64)>,
    track: Option<TrackRef<i16>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stream {
    mclk: Option<(u64, u64)>,
    bclk: (u64, u64),
    standard: I2sStandard,
    slot: u8,
    word: u8,
    adc: bool,
    dac: bool,
}

impl Es8311 {
    /// SCL/SDA are the netlist names for CCLK/CDATA; OUTP/OUTN the differential analog output,
    /// whose nets carry sound to an amplifier, not levels. es8311-datasheet.txt:231-243.
    pub const PINS: &'static [&'static str] = &["SCL", "SDA", "MCLK", "SCLK", "LRCK", "DSDIN", "ASDOUT", "OUTP", "OUTN"];

    pub fn new(addr: u8) -> Result<Self, String> {
        // es8311-datasheet.txt:303-307 and es8311-user-guide.txt:969-972. CE is the low address bit.
        if !matches!(addr, 0x18 | 0x19) {
            return Err("ES8311: CE strap requires a seven-bit address of 0x18 or 0x19".into());
        }
        Ok(Self { control: Control::new(addr), out: None })
    }

    pub(super) fn bind(&mut self, tick_hz: u64) -> Result<(), String> {
        if tick_hz == 0 { return Err("ES8311: board clock must be positive".into()); }
        self.control.tick_hz = tick_hz;
        Ok(())
    }

    /// The DAC output of the frame just clocked (None: the frame had no playback path), once.
    pub fn take_out(&mut self) -> Option<i16> {
        self.out.take()
    }

    pub(super) fn mark(&mut self) -> Control {
        self.control.clone()
    }

    pub(super) fn rewind(&mut self, control: &Control) {
        self.out = None;
        self.control.clone_from(control);
    }
}

impl Control {
    fn new(addr: u8) -> Self {
        Self {
            target: I2cTarget::new(addr),
            regs: Self::reset_regs(),
            pointer: None,
            expects_pointer: false,
            reset_flags_known: true,
            notes: 0,
            tick_hz: 0,
            sequence_since: 0,
            sequence_ready: None,
            validated: None,
            track: None,
        }
    }

    fn reset_regs() -> [u8; 256] {
        std::array::from_fn(|addr| register_spec(addr as u8).map_or(0, |r| r.reset))
    }

    fn note(&mut self, cx: &mut Cx<'_>, note: Note, message: &'static str) {
        let bit = 1 << note as u8;
        if self.notes & bit == 0 {
            self.notes |= bit;
            cx.note(message);
        }
    }

    fn describe(&mut self, cx: &mut Cx<'_>, addr: u8) {
        match addr {
            // M: Use the two register tables where the guide's prose differs.
            // es8311-user-guide.txt:1113-1120,1139-1144,1149-1153 and es8311-datasheet.txt:654-660,679-683.
            0x00 | 0x01 => self.note(cx, Note::ClockSelect,
                "ES8311: both tables put CSM_ON at 0x00[7] and MCLK_SEL at 0x01[7]; guide prose puts MCLK_SEL at 0x00[7]"),
            // es8311-datasheet.txt:719-723 and es8311-user-guide.txt:1173-1177,1215-1228,1235-1242.
            0x02 => self.note(cx, Note::Multiplier,
                "ES8311: MULT_PRE code 1 is x2 in both tables but x1 in two guide examples; the third gives two different divider outputs"),
            // M: Use the register tables. es8311-datasheet.txt:732-747, es8311-user-guide.txt:1238-1242,1995-2013.
            0x03 => self.note(cx, Note::AdcOsr,
                "ES8311: ADC_OSR resets to 16 in both tables; guide prose says 32, and its equation gives 32 for the 256x clock example programmed with 16"),
            // es8311-datasheet.txt:804-812 and es8311-user-guide.txt:1319-1323,1332.
            0x08 => self.note(cx, Note::FrameDivider,
                "ES8311: 0x08 holds DIV_LRCK[7:0] in both tables; the guide description calls these bits 11 to 8"),
            // es8311-datasheet.txt:862-885. Preserve the reset byte; validate explicitly programmed codes.
            0x0b | 0x0c => self.note(cx, Note::PowerStages,
                "ES8311: PWRUP_B has four bits but its table lists 0..31; PWRUP_C resets to 32 outside its listed 0..31 range"),
            // M: Model the internal-reference path at normal VMID. The standby prose disagrees
            // with the reference fields. es8311-datasheet.txt:886-903; es8311-user-guide.txt:1391-1405.
            0x0d => self.note(cx, Note::References,
                "ES8311: analog paths use internal references and normal VMID; guide standby prose says set the controls to 1, but the tables enable VREF at 1 and power VMID down at 0"),
            // es8311-datasheet.txt:1148-1153,1160-1165,1182-1184; es8311-user-guide.txt:1720,1732-1749,1760-1767.
            0x1d | 0x21 | 0x27 => self.note(cx, Note::AdcEq,
                "ES8311: ADC EQ tables start at 0x1d, guide prose says 0x1e; 0x21 labels six coefficient bits as eight, and 0x27 labels A2 as B0; only raw defined bits are stored"),
            // es8311-datasheet.txt:1036-1046 and es8311-user-guide.txt:1611-1624.
            0x17 => self.note(cx, Note::AdcVolume,
                "ES8311: ADC_VOLUME uses the tabulated -90.5 dB at 0x01, where the stated step gives -95 dB"),
            // es8311-datasheet.txt:1236-1247 and es8311-user-guide.txt:2114-2126,2155-2156.
            0x32 => self.note(cx, Note::DacVolume,
                "ES8311: DAC_VOLUME 0x01 is -95 dB in the datasheet and -90.5 dB in the guide; DRC gain text names ADC_VOLUME vs DAC_VOLUME; the control port stores the code"),
            // M: Use the reset byte. es8311-datasheet.txt:1308-1329 and es8311-user-guide.txt:2213-2222.
            0x37 => self.note(cx, Note::DacEq,
                "ES8311: DAC_EQBYPASS resets to 1 in both register headers but both field descriptions call 0 the default"),
            _ => (),
        }
        if addr == 0x01 {
            // M: Use the byte header. es8311-datasheet.txt:679,705-714 and es8311-user-guide.txt:1149.
            self.note(cx, Note::AnalogClocks,
                "ES8311: 0x01 resets to 0 in both headers but the datasheet calls both analog clocks on by default");
        }
        if addr == 0x00 {
            // es8311-datasheet.txt:664-678 and es8311-user-guide.txt:1363-1374.
            self.note(cx, Note::DigitalReset,
                "ES8311: reset bits assert at 1 in both register tables; the guide's suggested power-up sequence calls setting them a release");
        }
    }

    fn read(&mut self, cx: &mut Cx<'_>, addr: u8) -> Res<u8> {
        // es8311-datasheet.txt:1405-1416 gives state codes but no live transition rules.
        if addr == 0xfc && !self.reset_flags_known {
            return Err("ES8311: live FLAG value unavailable, CSM transitions and automute status are unspecified");
        }
        self.describe(cx, addr);
        Ok(self.regs[usize::from(addr)])
    }

    fn write(&mut self, cx: &mut Cx<'_>, addr: u8, value: u8) -> Res {
        let reg = register(cx, addr)?;
        match addr {
            // es8311-datasheet.txt:956-958,1302-1308,1405-1425.
            0x11 => return Err("ES8311: 0x11 VSEL is internal use, no public settings"),
            0x36 => return Err("ES8311: 0x36 has no documented writable fields"),
            0xfc..=0xff => return Err("ES8311: flags and chip identification have no documented writable fields"),
            // es8311-datasheet.txt:659-660,780-795,804-818; es8311-user-guide.txt:1283-1301,1319-1323.
            0x00 if value & 0x40 != 0 => return Err("ES8311: master mode unsupported; its divider source is the selected pin in the datasheet and dig_mclk in the guide"),
            // es8311-datasheet.txt:732-747,748-759.
            0x03 if value & 0x3f < 15 || value & 0x7f == 0x4f =>
                return Err("ES8311: ADC_OSR requires at least 15 in single speed and 16 in double speed"),
            0x04 if value & 0x7f < 15 => return Err("ES8311: DAC_OSR below 15 is unused"),
            // es8311-datasheet.txt:393-397,833-841,853-861; es8311-user-guide.txt:988-998,1064,1096.
            0x09 | 0x0a if value & 3 == 2 =>
                return Err("ES8311: SDP format 2 is reserved in both tables; datasheet prose mentions right justification without an encoding"),
            0x09 | 0x0a if (value >> 2) & 7 > 4 => return Err("ES8311: SDP word length code 5..7 is undocumented"),
            // es8311-datasheet.txt:876-885.
            0x0c if value & 0x7f > 31 =>
                return Err("ES8311: PWRUP_C only documents settings 0..31 although the reset byte selects 32"),
            // es8311-datasheet.txt:973-991 and es8311-user-guide.txt:1431-1433,1453-1475.
            0x14 if value & 0x20 != 0 =>
                return Err("ES8311: second microphone select is in the guide but bit 5 is undefined in the datasheet"),
            0x14 if value & 15 > 10 => return Err("ES8311: PGA gain for codes 11..15 NOT FOUND (es8311-datasheet.txt:980-991)"),
            // es8311-datasheet.txt:1160-1162 and es8311-user-guide.txt:1744-1749.
            0x21 if value & 0xc0 != 0 => return Err("ES8311: A1[29:24] has six bits but 0x21 lists bits 7:0; top two bits undefined"),
            // es8311-datasheet.txt:1375-1385.
            0x44 if value & 0x70 == 0x70 => return Err("ES8311: ADCDAT_SEL code 7 is unavailable"),
            _ => (),
        }
        if value & !reg.writable != 0 {
            return Err(reg.invalid_bits);
        }
        // es8311-datasheet.txt:1399-1404. The duration/effect of holding INI_REG high is unspecified.
        if addr != 0xfa && self.regs[0xfa] & 1 != 0 {
            return Err("ES8311: write while INI_REG remains set, retention during register reset is unspecified");
        }
        self.describe(cx, addr);
        if addr == 0xfa && value == 1 {
            // M: Apply the documented reset at the write; no latency is given. es8311-datasheet.txt:1399-1404.
            self.note(cx, Note::RegisterReset,
                "ES8311: INI_REG restores register defaults at the write; no reset latency is specified");
            self.regs = Self::reset_regs();
            self.reset_flags_known = true;
        }
        // es8311-datasheet.txt:654-678,1048-1055,1405-1416.
        if (addr == 0x00 && value & 0x80 != 0) || (addr == 0x18 && value & 0x40 != 0) {
            self.reset_flags_known = false;
        }
        if reg.reset & !reg.writable != 0 {
            // M: Preserve unassigned reset ones on field writes. es8311-datasheet.txt:904-915,969-972.
            self.note(cx, Note::ReservedReset,
                "ES8311: 0x0e and 0x13 have unnamed bits set at reset; field writes preserve their reset levels");
        }
        let stored = value | (reg.reset & !reg.writable);
        // M: A completed power-up lasts until the CSM, reset or sequence control changes. During
        // startup, time from the last relevant setup. es8311-datasheet.txt:654-692,715-731,862-885.
        let changed = stored ^ self.regs[usize::from(addr)];
        if (addr == 0x00 && changed & 0xb8 != 0) || (addr == 0xfa && value == 1) {
            self.sequence_ready = None;
            self.sequence_since = cx.t;
        } else if self.sequence_ready.is_none() && ((addr == 0x01 && changed & 0xf0 != 0)
            || (matches!(addr, 0x02 | 0x0b | 0x0c) && changed != 0)) {
            self.sequence_since = cx.t;
        }
        self.regs[usize::from(addr)] = stored;
        self.validated = None;
        Ok(())
    }
}

impl Model for Es8311 {
    fn pins(&self) -> &'static [&'static str] { Self::PINS }

    fn drive(&self, pin: u8) -> Drive {
        // es8311-datasheet.txt:231-238,1386-1398. Byte transfers use the board bus interface.
        let pull = if matches!(pin, 3 | 4) && self.control.regs[0x45] & 1 == 0 { Pull::Up } else { Pull::None };
        Drive::Input(pull)
    }

    fn pin(&mut self, _cx: &mut Cx<'_>, _pin: u8, _high: bool) -> Res { Ok(()) }
    fn next_event(&self) -> Option<u64> { None }
    fn event(&mut self, _cx: &mut Cx<'_>) -> Res { Ok(()) }
    fn i2c_addr(&self) -> Option<u8> { Some(self.control.target.addr()) }

    fn i2c(&mut self, cx: &mut Cx<'_>, op: I2cOp) -> Res<I2cAns> {
        self.control.i2c(cx, op)
    }

    fn i2s(&mut self, cx: &mut Cx<'_>, frame: &I2sFrame) -> Res<[i32; 2]> {
        let stream = Stream {
            mclk: frame.mclk_hz, bclk: frame.bclk_hz, standard: frame.standard,
            slot: frame.slot_bits, word: frame.data_bits, adc: frame.din.is_some(), dac: frame.dout.is_some(),
        };
        if !matches!(self.control.validated, Some((saved, ready)) if saved == stream && frame.t >= ready) {
            let ready = self.control.validate(cx, stream)?;
            if frame.t < ready {
                return Err("ES8311: power-up sequence completion is not established at this tick");
            }
            // es8311-datasheet.txt:661-663. Manual mode bypasses the automatic sequence.
            if self.control.regs[0x00] & 0x20 == 0 {
                self.control.sequence_ready = Some(ready);
            }
            self.control.validated = Some((stream, ready));
        }
        let adc = if stream.adc { self.control.microphone(cx, frame)? } else { [0; 2] };
        self.out = if stream.dac { Some(self.control.speaker(cx, frame)) } else { None };
        Ok(adc)
    }

    fn spi(&mut self, _cx: &mut Cx<'_>, _x: &SpiXfer, _tx: &dyn Payload, _rx: &mut Vec<u8>) -> Res {
        Err("ES8311: SPI control interface is undocumented")
    }

    fn input(&mut self, _cx: &mut Cx<'_>, ev: &Stimulus) -> Res {
        let Stimulus::Sound { track, .. } = ev else { return Err("ES8311: unsupported board stimulus") };
        self.control.track = Some(track.clone());
        Ok(())
    }
}

impl Control {
    // The ADC reads by run time, with no cursor or filter. Past the last recorded period
    // a closed sound track gives silence. The IMU holds its last motion sample there.
    fn microphone(&mut self, cx: &mut Cx<'_>, frame: &I2sFrame) -> Res<[i32; 2]> {
        let Some(track) = &self.track else {
            self.note(cx, Note::Silence, "ES8311: ADC returns silence without a sound track");
            return Ok([0; 2]);
        };
        let (num, den) = track.0.rate;
        // Two slots per LRCK period (es8311-datasheet.txt:393-401). Compare ratios exactly.
        let frame_den = u128::from(frame.bclk_hz.1) * 2 * u128::from(frame.slot_bits);
        if u128::from(num) * frame_den != u128::from(den) * u128::from(frame.bclk_hz.0) {
            self.note(cx, Note::SoundRate, "ES8311: sound track rate differs from LRCK, ADC returns silence");
            return Ok([0; 2]);
        }
        if self.tick_hz == 0 { return Err("ES8311: sampling the sound track requires a bound board clock"); }
        let sample = match track.0.at_tick(frame.t, self.tick_hz) {
            TrackRead::Sample(x) => x,
            TrackRead::Before => 0,
            TrackRead::Ahead => {
                track.0.count_short();
                self.note(cx, Note::SoundShort, "ES8311: sound track short of the frame instant (a live-run hold bug), ADC returns silence");
                0
            }
        };
        // M: Model the two gain controls. es8311-datasheet.txt:992-1055,1130-1147.
        self.note(cx, Note::AdcProcessing,
            "ES8311: ADC reads silence past the recorded sound. ADC scale, inversion, ramp, automute, HPF, ALC and EQ are not modelled");
        // es8311-datasheet.txt:844-846: SDP_OUT_MUTE silences both slots.
        if self.regs[0x0a] & 0x40 != 0 { return Ok([0; 2]); }
        // es8311-datasheet.txt:980-991,1036-1047: 3 dB per PGA step, 0.5 dB per volume
        // step from 0xBF at unity. Both volume tables explicitly give -90.5 dB for 0x01.
        let volume = self.regs[0x17];
        let volume_db = if volume == 1 { -90.5 } else { (f64::from(volume) - 191.0) * 0.5 };
        let db = 3.0 * f64::from(self.regs[0x14] & 15) + volume_db;
        let word = i32::from(sample) << (frame.data_bits - 16);
        let word = if db == 0.0 { word } else {
            let full_scale = (1u32 << (frame.data_bits - 1)) as f64;
            (f64::from(word) * 10f64.powf(db / 20.0)).round().clamp(-full_scale, full_scale - 1.0) as i32
        };
        // es8311-datasheet.txt:1375-1383: ADCDAT_SEL 0..3 give ADC/ADC, ADC/0, 0/ADC,
        // or 0/0. validate() refuses DAC feedback before a frame reaches here.
        let select = self.regs[0x44] >> 4;
        Ok([if select & 2 == 0 { word } else { 0 }, if select & 1 == 0 { word } else { 0 }])
    }

    /// The mono DAC's output for one frame, at the 16-bit full scale: the selected slot, its
    /// mutes, phase and digital volume. es8311-datasheet.txt:819-826,1216-1247;
    /// es8311-user-guide.txt:2058-2071,2113-2126.
    fn speaker(&mut self, cx: &mut Cx<'_>, frame: &I2sFrame) -> i16 {
        // M: the modulator, its dither and the analog output filter are not modelled: the
        // speaker hears the digital path, rounded to 16 bits.
        self.note(cx, Note::Speaker,
            "ES8311: the speaker hears the DAC's digital path (slot select, mutes, phase, volume); the modulator, its dither and the analog filter are not modelled");
        // es8311-datasheet.txt:1255-1265,1248-1250,1308-1329: DRC, offset, soft ramp, EQ.
        let r = &self.regs;
        if r[0x34] & 0x80 != 0 || r[0x33] != 0 || r[0x37] & 0xf0 != 0 || r[0x37] & 0x08 == 0 {
            self.note(cx, Note::DacProcessing,
                "ES8311: DAC DRC, offset, soft volume ramp and EQ are not modelled; the speaker hears the volume as set");
        }
        let regs = &self.regs;
        // es8311-datasheet.txt:819-826: SDP_IN_SEL picks the left (0) or right (1) slot; the
        // serial port accepts only words of 16 bits or more (`serial_port`).
        let word = frame.tx[usize::from(regs[0x09] >> 7)];
        let x = f64::from(word >> (frame.data_bits - 16));
        // es8311-datasheet.txt:824-826,1221-1226: SDP_IN_MUTE, DAC_DSMMUTE, DAC_DEMMUTE.
        if regs[0x09] & 0x40 != 0 || regs[0x31] & 0x60 != 0 { return 0; }
        // es8311-datasheet.txt:1236-1247: 0x00 is -95.5 dB, 0.5 dB a step, 0xBF 0 dB.
        let db = (f64::from(regs[0x32]) - 191.0) * 0.5;
        let y = (x * 10f64.powf(db / 20.0)).round();
        // es8311-datasheet.txt:1227-1229: DAC_INV turns the phase.
        let y = if regs[0x31] & 0x10 != 0 { -y } else { y };
        y.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    }

    fn i2c(&mut self, cx: &mut Cx<'_>, op: I2cOp) -> Res<I2cAns> {
        // es8311-datasheet.txt:310-326 specifies one byte, with a repeated START for reading.
        match self.target.step(op).map_err(|_| "ES8311: I2C data phase contradicts the selected direction or final NACK")? {
            TargetOp::None => {
                if matches!(op, I2cOp::Stop | I2cOp::Write(_)) {
                    self.pointer = None;
                }
                Ok(I2cAns::None)
            }
            TargetOp::Address { read } => {
                if read && self.pointer.is_none() {
                    return Err("ES8311: read requires a register pointer followed by a repeated START");
                }
                if !read {
                    self.pointer = None;
                }
                self.expects_pointer = !read;
                // M: I2cOp carries no wire frequency. es8311-datasheet.txt:297-302, es8311-user-guide.txt:954-956.
                self.note(cx, Note::BusRate,
                    "ES8311: I2C byte transfers do not check wire frequency; datasheet permits up to 400 kbps, guide says below 400 kbps");
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Write(value) if self.expects_pointer => {
                register(cx, value)?;
                self.pointer = Some(value);
                self.expects_pointer = false;
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Write(value) => {
                let addr = self.pointer.ok_or("ES8311: burst write is undocumented")?;
                self.write(cx, addr, value)?;
                self.pointer = None;
                Ok(I2cAns::Ack(true))
            }
            TargetOp::Read { ack } => {
                if ack { return Err("ES8311: burst read is undocumented, single-byte read ends in NACK"); }
                let addr = self.pointer.take().ok_or("ES8311: no register selected for read")?;
                self.read(cx, addr).map(I2cAns::Byte)
            }
        }
    }

    fn validate(&mut self, cx: &mut Cx<'_>, stream: Stream) -> Res<u64> {
        if !stream.adc && !stream.dac { return Err("ES8311: frame has no serial data path"); }
        // es8311-datasheet.txt:654-678,690-692,886-903; es8311-user-guide.txt:1366-1373.
        let reset = self.regs[0x00];
        let clock = self.regs[0x01];
        let power = self.regs[0x0d];
        if reset & 0x98 != 0x80 { return Err("ES8311: CSM is off or digital/clock manager reset is asserted"); }
        if clock & 0x10 == 0 { return Err("ES8311: BCLK_ON is off"); }
        if power & 0xc7 != 0x06 {
            return Err("ES8311: analog circuits, bias, internal reference and normal VMID operation are required");
        }
        // es8311-datasheet.txt:938-941 names SYNCMODE but gives no synchronized timing rules.
        if self.regs[0x10] & 0x80 != 0 { return Err("ES8311: SYNCMODE timing is undocumented"); }
        // es8311-datasheet.txt:777-779,394-396. I2sFrame has no inverted-edge mode.
        if self.regs[0x06] & 0x20 != 0 { return Err("ES8311: inverted BCLK sampling edge is unsupported"); }
        if stream.adc {
            // es8311-datasheet.txt:673-675,693-697,705-711,801-803,892-895,904-911,973-980.
            if reset & 2 != 0 || clock & 0x0a != 0x0a { return Err("ES8311: ADC in reset or ADC analog/digital clock off"); }
            if self.regs[0x14] & 0x40 != 0 { return Err("ES8311: PDM microphone input is unsupported"); }
            if power & 0x30 != 0 || self.regs[0x0e] & 0x70 != 0 || self.regs[0x14] & 0x10 == 0 {
                return Err("ES8311: analog ADC path requires microphone, PGA, bias, reference and released modulator");
            }
            if self.regs[0x07] & 0x10 != 0 { return Err("ES8311: ASDOUT is tri-stated"); }
            // es8311-datasheet.txt:1375-1383. Mixed or looped-back DAC data needs a feedback path.
            if self.regs[0x44] & 0x40 != 0 { return Err("ES8311: DAC feedback on ASDOUT is unsupported"); }
            Self::serial_port(cx, "ADC", self.regs[0x0a], stream)?;
        }
        if stream.dac {
            // es8311-datasheet.txt:676-678,698-714,896-899,959-964,1370-1374.
            if reset & 1 != 0 || clock & 5 != 5 { return Err("ES8311: DAC in reset or DAC analog/digital clock off"); }
            if power & 8 != 0 || self.regs[0x12] & 3 != 1 {
                return Err("ES8311: DAC or its analog/output reference is powered down");
            }
            if self.regs[0x44] & 0x80 != 0 { return Err("ES8311: ADC-to-DAC loopback is unsupported"); }
            // es8311-datasheet.txt:750-753,1327-1329.
            if self.regs[0x04] == 15 && self.regs[0x37] & 8 == 0 {
                return Err("ES8311: DAC_OSR 15 does not support enabled DAC EQ");
            }
            Self::serial_port(cx, "DAC", self.regs[0x09], stream)?;
        }
        if stream.slot < stream.word {
            return Err(cx.refuse(
                "ES8311: serial word exceeds the frame slot width (SDP 0x09/0x0a)",
                format!("ES8311: {}-bit word does not fit in the frame's {}-bit slot", stream.word, stream.slot),
            ));
        }
        self.check_clocks(cx, stream)?;
        self.ready_tick(cx, stream)
    }

    fn serial_port(cx: &mut Cx<'_>, path: &str, port: u8, stream: Stream) -> Res {
        // es8311-datasheet.txt:827-841,847-861; es8311-user-guide.txt:988-998.
        let (standard, label) = match (port & 3, port & 0x20 != 0) {
            (0, false) => (Some(I2sStandard::Philips), "Philips"),
            (1, false) => (Some(I2sStandard::MsbAlignment), "MsbAlignment"),
            (3, false) => (Some(I2sStandard::Pcm), "Pcm (DSP-A)"),
            (0, true) => (None, "Philips with inverted WS"),
            (1, true) => (None, "MsbAlignment with inverted WS"),
            (3, true) => (None, "DSP-B"),
            _ => return Err("ES8311: reserved serial format"),
        };
        if standard != Some(stream.standard) {
            return Err(cx.refuse(
                "ES8311: serial standard mismatch (SDP 0x09/0x0a)",
                format!("ES8311: {path} standard expects {label}, frame has {:?}", stream.standard),
            ));
        }
        let word = [24, 20, 18, 16, 32][usize::from((port >> 2) & 7)];
        if word != stream.word {
            return Err(cx.refuse(
                "ES8311: serial word length mismatch (SDP 0x09/0x0a)",
                format!("ES8311: {path} expects a {word}-bit word, frame has {} bits", stream.word),
            ));
        }
        Ok(())
    }

    fn ready_tick(&mut self, cx: &mut Cx<'_>, stream: Stream) -> Res<u64> {
        // es8311-datasheet.txt:661-663; es8311-user-guide.txt:1369-1374,1393-1398.
        if self.regs[0x00] & 0x20 != 0 {
            // M: In manual mode use the programmed path enables. No analog settling time is given.
            self.note(cx, Note::ManualPower, "ES8311: manual power-up uses the programmed enables and normal VMID; analog settling is not timed");
            return Ok(self.sequence_since);
        }
        if let Some(ready) = self.sequence_ready { return Ok(ready); }
        // es8311-datasheet.txt:862-885. B spans the two registers; C's reset code has no timing.
        let a = self.regs[0x0b] >> 3;
        let b = ((self.regs[0x0b] & 7) << 1) | (self.regs[0x0c] >> 7);
        let c = self.regs[0x0c] & 0x7f;
        if c > 31 { return Err("ES8311: automatic PWRUP_C reset code 32 has no documented duration"); }
        let fs_num = u128::from(stream.bclk.0);
        let fs_den = u128::from(stream.bclk.1) * 2 * u128::from(stream.slot);
        let hz = u128::from(self.tick_hz);
        if hz == 0 { return Err("ES8311: sequencer timing requires a bound board clock"); }
        // M: Count from the last CSM/input-clock or stage setup, assuming the presented LRCK
        // has run since then. At listed rates a nonzero code uses its stage's published upper bound.
        let (minimum, maxima): (u128, [u128; 3]) = if fs_num == 48_000 * fs_den {
            (21, [232_000, 104_000, 234_000])
        } else if fs_num == 44_100 * fs_den {
            (23, [253_000, 113_000, 254_000])
        } else if fs_num == 8_000 * fs_den {
            (120, [1_392_000, 624_000, 1_401_000])
        } else {
            return Err("ES8311: automatic power-up timing is documented only at 8, 44.1 and 48 kHz; manual SEQ_DIS is available");
        };
        let us: u128 = [a, b, c].into_iter().zip(maxima)
            .map(|(code, maximum)| if code == 0 { minimum } else { maximum }).sum();
        let delay = (hz * us).div_ceil(1_000_000);
        self.note(cx, Note::Sequence,
            "ES8311: power-up assumes continuous LRCK since CSM/clock/stage setup and uses published upper bounds for nonzero stages; completion lasts until CSM/reset/sequence control changes");
        u64::try_from(u128::from(self.sequence_since) + delay).map_err(|_| "ES8311: power-up completion exceeds board time")
    }

    fn check_clocks(&mut self, cx: &mut Cx<'_>, stream: Stream) -> Res {
        let (bn, bd) = stream.bclk;
        if bn == 0 || bd == 0 { return Err("ES8311: BCLK must be a positive rational frequency"); }
        // es8311-datasheet.txt:16,26,283-292,557-568. Each WS period contains two wire slots.
        let frame_bits = 2 * u128::from(stream.slot);
        let fs_den = u128::from(bd) * frame_bits;
        let fs_num = u128::from(bn);
        if fs_num > 26_000_000 * u128::from(bd) { return Err("ES8311: BCLK exceeds 26 MHz"); }
        if !(8_000 * fs_den..=96_000 * fs_den).contains(&fs_num) {
            return Err("ES8311: LRCK outside the documented 8..96 kHz converter range");
        }
        if let Some((n, d)) = stream.mclk {
            if n == 0 || d == 0 { return Err("ES8311: MCLK must be a positive rational frequency"); }
            if u128::from(n) > 49_200_000 * u128::from(d) { return Err("ES8311: MCLK exceeds 49.2 MHz"); }
        }
        // es8311-datasheet.txt:679-692,715-723; es8311-user-guide.txt:1142-1144,1254-1258,1307-1310.
        // Slave BCLK and LRCK are external. Neither master output divider constrains them.
        let source = if self.regs[0x01] & 0x80 != 0 { stream.bclk } else {
            if self.regs[0x01] & 0x20 == 0 { return Err("ES8311: pin MCLK selected but MCLK_ON is off"); }
            stream.mclk.ok_or("ES8311: pin MCLK selected but no controller MCLK reaches the codec")?
        };
        let pre = 1 + u128::from(self.regs[0x02] >> 5);
        let mult = 1u128 << ((self.regs[0x02] >> 3) & 3);
        let sn = u128::from(source.0);
        let pre_den = u128::from(source.1) * pre;
        let double = self.regs[0x03] & 0x40 != 0;
        for (adc, active, div) in [
            (true, stream.adc, self.regs[0x05] >> 4), (false, stream.dac, self.regs[0x05] & 15),
        ] {
            if !active { continue; }
            let path = if adc { "ADC" } else { "DAC" };
            // es8311-datasheet.txt:286-288,732-747,764-771; es8311-user-guide.txt:1190-1214.
            if adc && double && fs_num < 64_000 * fs_den {
                return Err("ES8311: ADC double-speed frame outside the documented normal 64..96 kHz range");
            }
            if adc && !double && fs_num > 48_000 * fs_den {
                return Err("ES8311: ADC single-speed frame outside the documented normal 8..48 kHz range");
            }
            if !adc && fs_num > 48_000 * fs_den {
                // M: Both overviews specify DAC to 96 kHz. Its only specified clock rule is single speed.
                // es8311-datasheet.txt:26,286-288; es8311-user-guide.txt:121-123,1195-1214.
                self.note(cx, Note::DacRate, "ES8311: DAC uses its single-speed clock rules through its specified 96 kHz range; 48 kHz is the stated normal single-speed upper rate");
            }
            let div = u128::from(div) + 1;
            let (minimum, step) = if adc && double { (128, 8) }
                else if adc && self.regs[0x1c] & 0x40 != 0 { (240, 16) } else { (256, 16) };
            let ratio = clock_ratio(source, stream.bclk, frame_bits * mult, pre * div);
            let value = match ratio {
                Ok(r) if r >= minimum && r % step == 0 => r,
                actual => {
                    let actual = actual.map_or_else(|fraction| fraction, |n| n.to_string());
                    return Err(cx.refuse(
                        "ES8311: converter clock/LRCK ratio mismatch (es8311-user-guide.txt:1195-1214)",
                        format!("ES8311: {path} clock/LRCK expected >= {minimum} in multiples of {step}, actual {actual}"),
                    ));
                }
            };
            if adc {
                // es8311-user-guide.txt:1238-1245,1995-2007. The table gives 16 at 256x
                // and 18 at 288x; its prose would give 32 and 36, respectively.
                let osr = u128::from(self.regs[0x03] & 63);
                let from_equation = osr * if double { 4 } else { 8 };
                if value != from_equation {
                    if !double && matches!((value, osr), (256, 16) | (288, 18)) {
                        // M: Accept the explicitly tabulated pairs as well as the guide equation.
                        self.note(cx, Note::OsrTable, "ES8311: ADC clock/OSR uses a worked table pair that conflicts with the guide equation");
                    } else {
                        let alternative = match (double, osr) {
                            (false, 16) => " or 256 from the guide table",
                            (false, 18) => " or 288 from the guide table",
                            _ => "",
                        };
                        return Err(cx.refuse(
                            "ES8311: ADC_OSR clock/LRCK ratio mismatch (0x03)",
                            format!("ES8311: ADC_OSR clock/LRCK expected {from_equation}{alternative}, actual {value}"),
                        ));
                    }
                }
            } else {
                // M: No post-divider DAC multiplier is documented. Bound its programmed
                // oversample rate by its internal clock. es8311-datasheet.txt:748-759,768-771.
                let oversample = 4 * u128::from(self.regs[0x04]);
                if value < oversample {
                    return Err(cx.refuse(
                        "ES8311: DAC_OSR exceeds the internal DAC clock/LRCK ratio (0x04)",
                        format!("ES8311: DAC_OSR requires clock/LRCK >= {oversample} without an undocumented multiplier, actual {value}"),
                    ));
                }
                self.note(cx, Note::DacOsr, "ES8311: DAC oversampling is bounded by the internal DAC clock; no further multiplier is documented");
            }
            // es8311-user-guide.txt:1190-1194. Test the active converter's divided clock.
            if sn * mult > 13_000_000 * pre_den * div {
                return Err("ES8311: converter clock exceeds the 13 MHz common DVDD limit (35 MHz at 3.3 V)");
            }
        }
        // es8311-datasheet.txt:494-500; es8311-user-guide.txt:1164-1165,1193-1194,1207-1217.
        // M: No DVDD input. Accept clocks in the intersection of both documented supply ranges.
        if sn >= 10_000_000 * pre_den { return Err("ES8311: predivider output must be below 10 MHz even at 3.3 V (guide clock examples exceed its limit)"); }
        if sn >= 4_500_000 * pre_den { return Err("ES8311: predivider output requires known 3.3 V DVDD at or above the 1.8 V limit"); }
        if mult >= 4 && sn <= 1_000_000 * pre_den {
            return Err("ES8311: x4/x8 requires predivider above 1 MHz at 3.3 V or above 500 kHz at known 1.8 V");
        }
        // M: Frames contain rational frequencies and decoded words but no edges or supply voltage.
        // es8311-datasheet.txt:494-500,557-581; es8311-user-guide.txt:1164-1165,1193-1194.
        self.note(cx, Note::WireTiming,
            "ES8311: frequency checks use limits common to both DVDD settings; decoded frames do not measure phase, duty cycle, setup or hold");
        Ok(())
    }
}

fn cancel(n: &mut u128, d: &mut u128) {
    let (mut a, mut b) = (*n, *d);
    while b != 0 { (a, b) = (b, a % b); }
    *n /= a;
    *d /= a;
}

/// Exact (source/BCLK) * (two-slot frame bits * multiplier) / (predivider * path divider).
/// Cancel factors before multiplying: even unreduced u64 frequencies fit the two cross products.
fn clock_ratio(source: (u64, u64), bclk: (u64, u64), mut mul: u128, mut div: u128) -> Result<u128, String> {
    let mut n = u128::from(source.0) * u128::from(bclk.1);
    let mut d = u128::from(source.1) * u128::from(bclk.0);
    cancel(&mut n, &mut d);
    cancel(&mut mul, &mut div);
    cancel(&mut n, &mut div);
    cancel(&mut mul, &mut d);
    if d == 1 && div == 1 {
        // Bounded by the maximum input frequency times eight divided by the minimum frame rate.
        return Ok(n * mul);
    }
    match (n.checked_mul(mul), d.checked_mul(div)) {
        (Some(n), Some(d)) => Err(format!("{n}/{d}")),
        _ => Err(format!("({n}*{mul})/({d}*{div})")),
    }
}
