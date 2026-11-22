use fab_elec_mcu_emit_s3::{chip, wiring, AmpEnable, Audio, I2cBus, Panel, SlotOptions,
    VOICEBOX_I2C, VOICEBOX_MOTION, VOICEBOX_PANEL, VOICEBOX_TOUCH};
use fab_elec_mcu_emu::Pull;
use fab_elec_mcu_parts::{BoardDesc, NetDesc, PartDesc, PartKind, PinRef};
use fab_elec_mcu_rt::{DisplayProfile, Profile};

fn board() -> BoardDesc {
    let mut desc = BoardDesc {
        seed: 0x5300,
        parts: vec![
            PartDesc { refdes: "LCD1".into(), kind: PartKind::Co5300 { glass: [16, 0, 368, 448] } },
            PartDesc { refdes: "U7".into(), kind: PartKind::Tca9554 { addr: 0x20 } },
            PartDesc { refdes: "IMU1".into(), kind: PartKind::Qmi8658c { addr: 0x6b } },
            PartDesc { refdes: "TP1".into(), kind: PartKind::Cst820 { addr: 0x15 } },
        ],
        nets: [("LCD_CS", "GPIO12", "CSX", Pull::Up),
            ("QSPI_SCL", "GPIO11", "SCLK", Pull::None),
            ("QSPI_SIO0", "GPIO4", "SIO0", Pull::None),
            ("QSPI_SIO1", "GPIO5", "SIO1", Pull::None),
            ("QSPI_SIO2", "GPIO6", "SIO2", Pull::None),
            ("QSPI_SIO3", "GPIO7", "SIO3", Pull::None)]
            .into_iter().map(|(name, chip, pin, pull)| NetDesc { name: name.into(), pull, rise_ps: None,
                pins: vec![PinRef::Chip(chip.into()), PinRef::Part { part: 0, pin: pin.into() }] }).collect(),
    };
    desc.nets.push(NetDesc { name: "LCD_RESET".into(), pull: Pull::None, rise_ps: None,
        pins: vec![PinRef::Part { part: 0, pin: "RESX".into() }, PinRef::Part { part: 1, pin: "P0".into() }] });
    for (name, chip, pin) in [("ESP32_SDA", "GPIO15", "SDA"), ("ESP32_SCL", "GPIO14", "SCL")] {
        desc.nets.push(NetDesc { name: name.into(), pull: Pull::Up, rise_ps: None,
            pins: vec![PinRef::Chip(chip.into()),
                PinRef::Part { part: 1, pin: pin.into() },
                PinRef::Part { part: 2, pin: pin.into() },
                PinRef::Part { part: 3, pin: pin.into() }] });
    }
    for (part, pins) in [(1, &["P1", "P2", "P3", "P4", "P5", "P6", "P7", "INT"][..]),
        (2, &["INT1", "INT2"][..]), (3, &["INT", "RST"][..])]
    {
        for &pin in pins {
            desc.nets.push(NetDesc { name: format!("{part}_{pin}").into(), pull: Pull::Up, rise_ps: None,
                pins: vec![PinRef::Part { part, pin: pin.into() }] });
        }
    }
    desc
}

fn net_mut<'a>(desc: &'a mut BoardDesc, name: &str) -> &'a mut NetDesc {
    desc.nets.iter_mut().find(|net| net.name.as_ref() == name).expect("fixture net exists")
}

fn i2c_fields(bus: I2cBus) -> (u32, u32, u32) { (bus.sda, bus.scl, bus.hz) }

fn audio_board() -> BoardDesc {
    let mut desc = BoardDesc { seed: 0, parts: vec![
        PartDesc { refdes: "CODEC".into(), kind: PartKind::Es8311 { addr: 0x19 } },
    ], nets: Vec::new() };
    for (pin, gpio) in [("SDA", 3), ("SCL", 4), ("MCLK", 5), ("SCLK", 6),
        ("LRCK", 7), ("DSDIN", 8), ("ASDOUT", 9)] {
        desc.nets.push(NetDesc { name: pin.into(), pull: Pull::None, rise_ps: None,
            pins: vec![PinRef::Part { part: 0, pin: pin.into() }, PinRef::Chip(format!("GPIO{gpio}").into())] });
    }
    desc
}

fn add_amp(desc: &mut BoardDesc, name: &str, gpio: &str, enable_high: bool) {
    let part = desc.parts.len() as u16;
    desc.parts.push(PartDesc { refdes: name.into(), kind: PartKind::Amp { enable_high, gain_db: 0, start_ms: 1 } });
    desc.nets.push(NetDesc { name: name.into(), pull: Pull::None, rise_ps: None,
        pins: vec![PinRef::Chip(gpio.into()), PinRef::Part { part, pin: "EN".into() }] });
}

#[test]
fn audio_wiring_reaches_the_profile_and_slot() {
    let mut desc = audio_board();
    add_amp(&mut desc, "PA1", "GPIO10", false);
    add_amp(&mut desc, "PA2", "GPIO11", true);
    let wired = wiring(&desc).unwrap();
    assert_eq!(wired.audio, Some(Audio { addr: 0x19, mclk: 5, sclk: 6, lrck: 7, dsdin: 8, asdout: Some(9),
        amps: vec![AmpEnable { gpio: 10, enable_high: false }, AmpEnable { gpio: 11, enable_high: true }] }));
    assert_eq!(wired.i2c.map(i2c_fields), Some((3, 4, 400_000)));
    assert_eq!(wired.options().audio, wired.audio);
    let profile = wired.profile().audio.unwrap();
    assert!(profile.mic);
    assert_eq!(profile.rates, &[16_000, 19_200, 22_050, 32_000, 44_100, 48_000]);
}

#[test]
fn speaker_image_notes_the_duration_of_queues_it_rounds_up() {
    let wired = wiring(&audio_board()).unwrap();
    let profile = wired.profile();
    for (rate, requested, actual, step) in [
        (22_050, 25, 40, 20), (22_050, 195, 200, 20), (44_100, 25, 30, 10), (44_100, 30, 30, 10),
    ] {
        let source = format!("let tx = speaker({rate}, {requested}, fn() {{}})");
        let program = fab_elec_mcu::compile(&source, &profile, &[], &[]).unwrap();
        let module = fab_elec_mcu_rt::module::load(&program.module, &profile).unwrap();
        let slot = fab_elec_mcu_emit_s3::build_slot(&module, &wired.options()).unwrap();
        assert_eq!(slot.layout.speaker.unwrap().count * 5, actual);
        let time = fab_elec_mcu_emit_s3::BoardTime { rc_slow_hz: (136_000, 1), wdt_delay_sel: 0 };
        let built = fab_elec_mcu_emit_s3::firmware(&source, &[], &wired, time, None, &[]).unwrap();
        if requested == actual {
            assert!(built.notes.is_empty());
        } else {
            assert_eq!(built.notes, [format!(
                "speaker queue {requested} ms at {rate} Hz rounds up to {actual} ms (whole {step} ms steps)")]);
        }
    }
}

#[test]
fn line_out_needs_neither_amp_nor_capture_pad() {
    let mut desc = audio_board();
    desc.parts[0].kind = PartKind::Es8311 { addr: 0x18 };
    net_mut(&mut desc, "ASDOUT").pins.truncate(1);
    assert_eq!(wiring(&desc).unwrap().audio.unwrap().asdout, None);
    desc.nets.retain(|net| net.name.as_ref() != "ASDOUT");
    let wired = wiring(&desc).unwrap();
    let profile = wired.profile();
    let audio = wired.audio.unwrap();
    assert_eq!((audio.addr, audio.asdout), (0x18, None));
    assert!(audio.amps.is_empty());
    assert!(!profile.audio.unwrap().mic);
    fab_elec_mcu::compile("let tx = speaker(19200, 20, fn() {})", &profile, &[], &[]).unwrap();
    let errors = fab_elec_mcu::compile("let rx = mic(19200, 20, fn() {})", &profile, &[], &[]).err().unwrap();
    assert!(errors[0].message.contains("no mic input"));
}

#[test]
fn codec_is_unique_and_uses_the_shared_i2c_bus() {
    let mut desc = audio_board();
    desc.parts.push(PartDesc { refdes: "CODEC2".into(), kind: PartKind::Es8311 { addr: 0x18 } });
    refuses(&desc, "CODEC2", "second ES8311", "a second codec cannot claim I2S0");
    desc.parts[1].kind = PartKind::Tca9554 { addr: 0x20 };
    net_mut(&mut desc, "SDA").pins.push(PinRef::Part { part: 1, pin: "SDA".into() });
    net_mut(&mut desc, "MCLK").pins.push(PinRef::Part { part: 1, pin: "SCL".into() });
    refuses(&desc, "CODEC2", "shared I2C net", "the codec and expander must share SCL");
}

#[test]
fn audio_pins_require_unambiguous_gpio_pads() {
    for pin in ["MCLK", "SCLK", "LRCK", "DSDIN"] {
        let mut desc = audio_board();
        net_mut(&mut desc, pin).pins.truncate(1);
        refuses(&desc, pin, "no chip pad", "a wired signal needs a pad");
    }
    let mut desc = audio_board();
    net_mut(&mut desc, "MCLK").pins.push(PinRef::Chip("GPIO12".into()));
    refuses(&desc, "MCLK", "more than one chip pad", "one signal cannot choose a pad");
    desc.nets.retain(|net| net.name.as_ref() != "DSDIN");
    net_mut(&mut desc, "MCLK").pins.truncate(2);
    refuses(&desc, "DSDIN", "has no net", "TX data is required");
}

#[test]
fn wired_capture_pin_and_codec_address_are_checked() {
    let mut desc = audio_board();
    net_mut(&mut desc, "ASDOUT").pins[1] = PinRef::Part { part: 0, pin: "OUTP".into() };
    refuses(&desc, "ASDOUT", "no chip pad", "routed capture data needs a pad");
    desc.parts[0].kind = PartKind::Es8311 { addr: 0x20 };
    refuses(&desc, "CODEC", "0x18 or 0x19", "the CE strap selects either codec address");
}

#[test]
fn amp_enable_must_reach_a_real_gpio() {
    for (gpio, reason) in [("RESET", "not GPIO"), ("GPIO24", "does not exist")] {
        let mut desc = audio_board();
        add_amp(&mut desc, "PA", gpio, false);
        refuses(&desc, "PA: EN", reason, "an amp enable needs a GPIO");
        net_mut(&mut desc, "PA").pins.remove(0);
        refuses(&desc, "PA: EN", "no chip pad", "each amp needs its own enable wiring");
    }
}

fn panel_fields(panel: Panel) -> (u32, u32, u32, u32, u32, u32, u32, u8, u8, u8) {
    (panel.width, panel.height, panel.column_offset, panel.row_offset,
        panel.cs, panel.sck, panel.mosi, panel.expander, panel.reset_bit, panel.brightness)
}

fn refuses(desc: &BoardDesc, part: &str, detail: &str, guard: &str) {
    let error = wiring(desc).expect_err(guard);
    assert!(error.contains(part), "{guard}: error must name {part}: {error}");
    assert!(error.contains(detail), "{guard}: error must identify {detail}: {error}");
}

#[test]
fn circuit_wiring_drives_the_image_and_profile() {
    let desc = board();
    let derived = wiring(&desc).expect("the handheld's shared bus and reset wiring resolves");
    assert_eq!(derived.i2c.map(i2c_fields), Some(i2c_fields(VOICEBOX_I2C)),
        "I2C pads and the documented clock match the handheld");
    assert_eq!(derived.motion.map(|motion| motion.addr), Some(VOICEBOX_MOTION.addr),
        "the IMU address comes from its part");
    assert_eq!(derived.touch.map(|touch| touch.addr), Some(VOICEBOX_TOUCH.addr),
        "the touch address comes from its part");
    assert_eq!(derived.panel.map(panel_fields), Some(panel_fields(VOICEBOX_PANEL)),
        "every panel field matches the handheld, including offsets and expander reset");

    let expected_profile = Profile { display: Some(DisplayProfile { width: 368, height: 448 }),
        motion: VOICEBOX_MOTION.profile().motion, touch: true, ..chip::profile() };
    assert_eq!(derived.profile(), expected_profile,
        "display, motion and touch coexist without replacing the chip profile");
    let options = derived.options();
    assert_eq!(options.i2c.map(i2c_fields), derived.i2c.map(i2c_fields), "slot options retain the I2C bus");
    assert_eq!(options.motion.map(|motion| motion.addr), derived.motion.map(|motion| motion.addr),
        "slot options retain the IMU");
    assert_eq!(options.touch.map(|touch| touch.addr), derived.touch.map(|touch| touch.addr),
        "slot options retain touch");
    assert_eq!(options.panel.map(panel_fields), derived.panel.map(panel_fields), "slot options retain every panel field");
    assert!(options.sites.is_empty() && options.marks.is_empty(), "wiring adds no debug sites or marks");
    assert_eq!(options.flash_limit_bytes, SlotOptions::default().flash_limit_bytes,
        "wiring preserves the default flash limit");
    assert!(options.radio.is_none(), "bus parts do not enable the radio");

    let mut changed = desc.clone();
    changed.parts[0].kind = PartKind::Co5300 { glass: [8, 12, 320, 400] };
    changed.parts[1].kind = PartKind::Tca9554 { addr: 0x21 };
    changed.parts[2].kind = PartKind::Qmi8658c { addr: 0x6a };
    changed.parts[3].kind = PartKind::Cst820 { addr: 0x16 };
    for (net, chip) in [("ESP32_SDA", "GPIO17"), ("ESP32_SCL", "GPIO18"),
        ("LCD_CS", "GPIO10"), ("QSPI_SCL", "GPIO9"), ("QSPI_SIO0", "GPIO8")]
    {
        net_mut(&mut changed, net).pins[0] = PinRef::Chip(chip.into());
    }
    net_mut(&mut changed, "LCD_RESET").pins[1] = PinRef::Part { part: 1, pin: "P7".into() };
    net_mut(&mut changed, "1_P7").pins[0] = PinRef::Part { part: 1, pin: "P0".into() };
    let remapped = wiring(&changed).expect("another valid circuit resolves without handheld defaults");
    assert_eq!(remapped.i2c.map(i2c_fields), Some((17, 18, 400_000)), "I2C GPIOs follow changed nets");
    assert_eq!(remapped.motion.map(|motion| motion.addr), Some(0x6a), "IMU address is not hard-coded");
    assert_eq!(remapped.touch.map(|touch| touch.addr), Some(0x16), "touch address is not hard-coded");
    assert_eq!(remapped.panel.map(panel_fields), Some((320, 400, 8, 12, 10, 9, 8, 0x21, 7, 0xff)),
        "glass, SPI pins, expander address and reset output follow the circuit");
    assert_eq!(remapped.profile().display, Some(DisplayProfile { width: 320, height: 400 }),
        "the language display dimensions follow changed glass");

    for (refdes, kind, detail) in [
        ("IMU2", PartKind::Qmi8658c { addr: 0x6a }, "second 6-axis IMU"),
        ("TP2", PartKind::Cst820 { addr: 0x16 }, "second touch controller"),
        ("LCD2", PartKind::Co5300 { glass: [0, 0, 320, 400] }, "second panel"),
    ] {
        let mut changed = desc.clone();
        changed.parts.push(PartDesc { refdes: refdes.into(), kind });
        refuses(&changed, refdes, detail, "a second instance of a single-instance feature is refused");
    }

    for (pin, net, chip) in [("SDA", "ESP32_SDA", "GPIO17"), ("SCL", "ESP32_SCL", "GPIO18")] {
        let mut changed = desc.clone();
        net_mut(&mut changed, net).pins.retain(|endpoint| !matches!(endpoint, PinRef::Part { part: 2, .. }));
        changed.nets.push(NetDesc { name: "OTHER_BUS".into(), pull: Pull::Up, rise_ps: None,
            pins: vec![PinRef::Chip(chip.into()), PinRef::Part { part: 2, pin: pin.into() }] });
        refuses(&changed, "IMU1", pin, "every I2C part must share both bus nets");
    }

    let mut changed = desc.clone();
    net_mut(&mut changed, "ESP32_SDA").pins.retain(|endpoint| !matches!(endpoint, PinRef::Chip(_)));
    refuses(&changed, "U7", "SDA", "a bus net without a chip pad has no fallback GPIO");

    let mut changed = desc.clone();
    net_mut(&mut changed, "LCD_RESET").pins.truncate(1);
    refuses(&changed, "LCD1", "RESX", "panel reset must reach an expander output");

    let mut changed = desc.clone();
    changed.parts.remove(1);
    for net in &mut changed.nets {
        net.pins.retain(|endpoint| !matches!(endpoint, PinRef::Part { part: 1, .. }));
        for endpoint in &mut net.pins {
            if let PinRef::Part { part, .. } = endpoint {
                if *part > 1 { *part -= 1; }
            }
        }
    }
    refuses(&changed, "LCD1", "RESX", "a panel without an expander cannot borrow another I2C part");

    let mut changed = desc.clone();
    net_mut(&mut changed, "LCD_CS").pins.push(PinRef::Chip("GPIO10".into()));
    refuses(&changed, "LCD1", "CSX", "a panel signal cannot choose between two chip pads");

    let mut changed = desc.clone();
    net_mut(&mut changed, "ESP32_SDA").pins.retain(|endpoint| !matches!(endpoint, PinRef::Part { part: 2, .. }));
    refuses(&changed, "IMU1", "SDA", "a missing I2C pin is refused");

    let mut changed = desc.clone();
    changed.nets.push(NetDesc { name: "DUPLICATE_SDA".into(), pull: Pull::Up, rise_ps: None,
        pins: vec![PinRef::Part { part: 2, pin: "SDA".into() }] });
    refuses(&changed, "IMU1", "SDA", "a part pin on multiple nets is ambiguous");

    let mut changed = desc.clone();
    net_mut(&mut changed, "ESP32_SCL").pins.retain(|endpoint| !matches!(endpoint, PinRef::Part { .. }));
    for part in 1..=3 {
        net_mut(&mut changed, "ESP32_SDA").pins.push(PinRef::Part { part, pin: "SCL".into() });
    }
    refuses(&changed, "U7", "SDA and SCL", "SDA and SCL cannot be shorted onto one net");

    for chip in ["SDA15", "GPIO", "GPIO+15", "GPIO4294967296"] {
        let mut changed = desc.clone();
        net_mut(&mut changed, "ESP32_SDA").pins[0] = PinRef::Chip(chip.into());
        refuses(&changed, "U7", "SDA", "chip pads require a GPIO prefix and a representable decimal pin number");
    }

    let mut changed = desc.clone();
    net_mut(&mut changed, "1_P1").pins.clear();
    net_mut(&mut changed, "LCD_RESET").pins.push(PinRef::Part { part: 1, pin: "P1".into() });
    refuses(&changed, "LCD1", "RESX", "reset must select exactly one output, even within one expander");

    let empty = BoardDesc { seed: desc.seed, parts: vec![
        PartDesc { refdes: "KEY1".into(), kind: PartKind::Key },
        PartDesc { refdes: "NOT1".into(), kind: PartKind::Not },
    ], nets: Vec::new() };
    let absent = wiring(&empty).expect("a board without bus parts needs no bus wiring");
    assert!(absent.i2c.is_none() && absent.motion.is_none() && absent.touch.is_none() && absent.panel.is_none() && absent.audio.is_none(),
        "non-bus parts do not synthesize any image feature");
    assert!(chip::profile().audio.is_none(), "the bare chip has no codec");
    assert_eq!(absent.profile(), chip::profile(), "no bus parts leave the chip profile unchanged");
    let options = absent.options();
    assert!(options.i2c.is_none() && options.motion.is_none() && options.touch.is_none() && options.panel.is_none() && options.audio.is_none(),
        "empty wiring stays absent in slot options");
}
