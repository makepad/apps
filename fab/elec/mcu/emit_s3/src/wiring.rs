//! Image wiring derived from the circuit's parts and nets.
use fab_elec_mcu_parts::{BoardDesc, PartKind, PinRef};
use fab_elec_mcu_rt as rt;

use crate::{chip, I2cBus, Motion, Panel, SlotOptions, Touch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmpEnable {
    pub gpio: u32,
    pub enable_high: bool,
}

/// ES8311 on the shared I2C bus, with the GPIOs of its I2S bus and amplifier enables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Audio {
    pub addr: u8,
    pub mclk: u32,
    pub sclk: u32,
    pub lrck: u32,
    pub dsdin: u32,
    pub asdout: Option<u32>,
    pub amps: Vec<AmpEnable>,
}

impl Audio {
    // Several amplifiers may have the same enable net and polarity.
    pub(crate) fn amp_enables(&self) -> impl Iterator<Item = &AmpEnable> {
        self.amps.iter().enumerate().filter_map(move |(i, amp)| (!self.amps[..i].contains(amp)).then_some(amp))
    }
}

// TRM trm.txt:37470-37538,38837-38864,38941-38968. With MCLK = 256 * rate,
// PLL_F160M / MCLK gives N + b/a. For the rates below the exact divisors are
// 39+1/16, 32+53/96, 28+152/441, 19+17/32, 14+76/441 and 13+1/48.
// With Z = min(b,a-b), X = a/Z-1 (integer division), Y = a%Z, YN1 = (2*b>a),
// each fits the 9-bit X/Y/Z fields. BCLK = MCLK/8 for two 16-bit slots.
// parts/src/es8311.rs:578-684 accepts converter clock/LRCK = 256, OSR = 16.
// Divide and multiply by 1,2,2,2,4,4 respectively, keeping pre below 4.5 MHz
// and converter clocks below 13 MHz at either supply voltage (facts-audio.md).
// NOT FOUND: explicit 19.2 and 22.05 kHz rows in the TRM feature list.
// At 22,050 and 44,100 Hz the facts-audio.md table rows on lines 43 and 45 give
// ES8311 register 0x02. Lines 104 and 106 give N and the packed TX_CLKM_DIV_CONF.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AudioClock {
    pub hz: u32,
    pub n: u32,
    pub fraction: u32,
    pub codec_pre: u8,
}

impl AudioClock {
    // One extra stereo frame in the last descriptor of each fractional cycle.
    pub(crate) fn descriptor_cycle(self) -> u32 {
        match self.hz % 200 {
            0 => 1, 50 => 4, 100 => 2,
            _ => unreachable!("unsupported 5 ms descriptor fraction"),
        }
    }
}

pub(crate) const AUDIO_CLOCKS: [AudioClock; 6] = [
    AudioClock { hz: 16_000, n: 39, fraction: 0x003c_0001, codec_pre: 0x00 },
    AudioClock { hz: 19_200, n: 32, fraction: 0x0804_142b, codec_pre: 0x28 },
    AudioClock { hz: 22_050, n: 28, fraction: 0x0005_1298, codec_pre: 0x28 },
    AudioClock { hz: 32_000, n: 19, fraction: 0x0804_040f, codec_pre: 0x28 },
    AudioClock { hz: 44_100, n: 14, fraction: 0x0010_7a4c, codec_pre: 0x70 },
    AudioClock { hz: 48_000, n: 13, fraction: 0x00bc_0001, codec_pre: 0x70 },
];
const AUDIO_RATES: [u32; 6] = {
    let [a, b, c, d, e, f] = AUDIO_CLOCKS;
    [a.hz, b.hz, c.hz, d.hz, e.hz, f.hz]
};

pub(crate) fn audio_clock(hz: u32) -> Result<AudioClock, crate::Refusal> {
    AUDIO_CLOCKS.into_iter().find(|clock| clock.hz == hz).ok_or_else(||
        crate::image::refusal(format!("S3 audio cannot clock {hz} Hz. Supported rates in Hz are {AUDIO_RATES:?}")))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wiring {
    pub i2c: Option<I2cBus>,
    pub motion: Option<Motion>,
    pub touch: Option<Touch>,
    pub panel: Option<Panel>,
    pub audio: Option<Audio>,
}

impl Wiring {
    /// The language profile for this wiring: chip::profile() with each present feature's part.
    pub fn profile(&self) -> rt::Profile {
        let mut profile = chip::profile();
        if let Some(panel) = self.panel { profile.display = panel.profile().display; }
        if let Some(motion) = self.motion { profile.motion = motion.profile().motion; }
        if let Some(touch) = self.touch { profile.touch = touch.profile().touch; }
        if let Some(audio) = &self.audio {
            profile.audio = Some(rt::AudioProfile { rates: &AUDIO_RATES, mic: audio.asdout.is_some() });
        }
        profile
    }

    /// SlotOptions with this wiring (sites and marks empty, flash limit as SlotOptions::default()).
    pub fn options(&self) -> SlotOptions {
        SlotOptions { i2c: self.i2c, motion: self.motion, touch: self.touch, panel: self.panel,
            audio: self.audio.clone(), ..SlotOptions::default() }
    }
}

/// Resolve the supported bus parts without substituting board-specific wiring.
pub fn wiring(desc: &BoardDesc) -> Result<Wiring, String> {
    let mut wiring = Wiring { i2c: None, motion: None, touch: None, panel: None, audio: None };
    let mut panel_part = None;
    let mut codec = None;
    let mut amps = Vec::new();
    let mut bus_nets: Option<[usize; 2]> = None;
    for (part, description) in desc.parts.iter().enumerate() {
        match description.kind {
            PartKind::Co5300 { glass } => {
                if panel_part.replace((part, glass)).is_some() {
                    return Err(format!("{}: a second panel (one is supported)", description.refdes));
                }
                continue;
            }
            PartKind::Qmi8658c { addr } => {
                if wiring.motion.replace(Motion { addr }).is_some() {
                    return Err(format!("{}: a second 6-axis IMU (one is supported)", description.refdes));
                }
            }
            PartKind::Cst820 { addr } => {
                if wiring.touch.replace(Touch { addr }).is_some() {
                    return Err(format!("{}: a second touch controller (one is supported)", description.refdes));
                }
            }
            PartKind::Es8311 { addr } => {
                if codec.replace((part, addr)).is_some() {
                    return Err(format!("{}: a second ES8311 codec (one is supported)", description.refdes));
                }
                if !matches!(addr, 0x18 | 0x19) {
                    return Err(format!("{}: ES8311 address {addr:#x} must be 0x18 or 0x19", description.refdes));
                }
            }
            PartKind::Amp { enable_high, .. } => {
                let gpio = audio_pad(desc, part, "EN", pin_net(desc, part, "EN")?)?;
                amps.push(AmpEnable { gpio, enable_high });
                continue;
            }
            PartKind::Tca9554 { .. } => (),
            PartKind::Key | PartKind::Not => continue,
        }

        let sda_net = pin_net(desc, part, "SDA")?;
        let scl_net = pin_net(desc, part, "SCL")?;
        if sda_net == scl_net {
            return Err(format!("{}: SDA and SCL share net {}", description.refdes, desc.nets[sda_net].name));
        }
        let sda = chip_pad(desc, part, "SDA", sda_net)?;
        let scl = chip_pad(desc, part, "SCL", scl_net)?;
        if let Some(first) = bus_nets {
            for (pin, net, expected) in [("SDA", sda_net, first[0]), ("SCL", scl_net, first[1])] {
                if net != expected {
                    return Err(format!("{}: {pin} is on net {}, not shared I2C net {} (one bus is supported)",
                        description.refdes, desc.nets[net].name, desc.nets[expected].name));
                }
            }
        } else {
            bus_nets = Some([sda_net, scl_net]);
            // VOICEBOX_I2C documents every I2C part's 400 kHz maximum.
            wiring.i2c = Some(I2cBus { sda, scl, hz: 400_000 });
        }
    }

    if let Some((part, addr)) = codec {
        let pad = |pin| audio_pad(desc, part, pin, pin_net(desc, part, pin)?);
        // An open pin may be absent or alone on a floating net in the board description.
        let asdout = optional_pin_net(desc, part, "ASDOUT")?
            .filter(|&net| desc.nets[net].pins.len() > 1 || desc.nets[net].pull != fab_elec_mcu_emu::Pull::None)
            .map(|net| audio_pad(desc, part, "ASDOUT", net)).transpose()?;
        wiring.audio = Some(Audio {
            addr, mclk: pad("MCLK")?, sclk: pad("SCLK")?, lrck: pad("LRCK")?,
            dsdin: pad("DSDIN")?, asdout, amps,
        });
    }
    if let Some((part, [x, y, width, height])) = panel_part {
        let cs = chip_pad(desc, part, "CSX", pin_net(desc, part, "CSX")?)?;
        let sck = chip_pad(desc, part, "SCLK", pin_net(desc, part, "SCLK")?)?;
        let mosi = chip_pad(desc, part, "SIO0", pin_net(desc, part, "SIO0")?)?;
        let (expander, reset_bit) = panel_reset(desc, part)?;
        // Every TCA9554 was checked above, including the one driving RESX.
        wiring.panel = Some(Panel {
            width: u32::from(width), height: u32::from(height),
            column_offset: u32::from(x), row_offset: u32::from(y),
            cs, sck, mosi, expander, reset_bit,
            brightness: 0xff, // Lead choice recorded in panel.rs.
            report: false,
        });
    }
    Ok(wiring)
}

fn pin_net(desc: &BoardDesc, part: usize, pin: &str) -> Result<usize, String> {
    optional_pin_net(desc, part, pin)?.ok_or_else(|| format!("{}: {pin} has no net", desc.parts[part].refdes))
}

fn optional_pin_net(desc: &BoardDesc, part: usize, pin: &str) -> Result<Option<usize>, String> {
    let refdes = &desc.parts[part].refdes;
    let mut found: Option<usize> = None;
    for (net, description) in desc.nets.iter().enumerate() {
        for endpoint in &description.pins {
            if matches!(endpoint, PinRef::Part { part: index, pin: name }
                if usize::from(*index) == part && name.as_ref() == pin)
            {
                if let Some(first) = found {
                    return Err(format!("{refdes}: {pin} occurs more than once on nets {} and {}",
                        desc.nets[first].name, description.name));
                }
                found = Some(net);
            }
        }
    }
    Ok(found)
}

fn audio_pad(desc: &BoardDesc, part: usize, pin: &str, net: usize) -> Result<u32, String> {
    let gpio = chip_pad(desc, part, pin, net)?;
    // TRM GPIO numbering, as for pin declarations in image.rs.
    if !matches!(gpio, 0..=21 | 26..=48) {
        return Err(format!("{}: {pin} is on net {}, GPIO{gpio} does not exist on the S3",
            desc.parts[part].refdes, desc.nets[net].name));
    }
    Ok(gpio)
}

fn chip_pad(desc: &BoardDesc, part: usize, pin: &str, net: usize) -> Result<u32, String> {
    let refdes = &desc.parts[part].refdes;
    let net = &desc.nets[net];
    let mut pads = net.pins.iter().filter_map(|endpoint| match endpoint {
        PinRef::Chip(name) => Some(name.as_ref()),
        PinRef::Part { .. } => None,
    });
    let name = pads.next().ok_or_else(|| {
        format!("{refdes}: {pin} is on net {}, which reaches no chip pad", net.name)
    })?;
    if pads.next().is_some() {
        return Err(format!("{refdes}: {pin} is on net {}, which reaches more than one chip pad", net.name));
    }
    name.strip_prefix("GPIO")
        .filter(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| format!("{refdes}: {pin} is on net {}, whose chip pad {name} is not GPIO<n> with a u32 pin number", net.name))
}

fn panel_reset(desc: &BoardDesc, panel: usize) -> Result<(u8, u8), String> {
    let net = &desc.nets[pin_net(desc, panel, "RESX")?];
    let refdes = &desc.parts[panel].refdes;
    let mut output = None;
    for endpoint in &net.pins {
        let PinRef::Part { part, pin } = endpoint else { continue };
        let part = usize::from(*part);
        let description = desc.parts.get(part).ok_or_else(|| {
            format!("{refdes}: RESX on net {} reaches unknown part {part} pin {pin}", net.name)
        })?;
        let PartKind::Tca9554 { addr } = description.kind else { continue };
        if !description.kind.pins().contains(&pin.as_ref()) {
            return Err(format!("{refdes}: RESX on net {} reaches {}'s unknown pin {pin}",
                net.name, description.refdes));
        }
        // The model's PINS establishes which P<k> names are outputs.
        let Some(bit) = pin.strip_prefix('P').and_then(|number| number.parse::<u8>().ok()) else { continue };
        pin_net(desc, part, pin)?;
        if output.replace((addr, bit)).is_some() {
            return Err(format!("{refdes}: RESX on net {} reaches more than one expander output", net.name));
        }
    }
    output.ok_or_else(|| format!("{refdes}: RESX on net {} reaches no expander output", net.name))
}
