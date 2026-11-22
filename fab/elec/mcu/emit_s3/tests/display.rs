//! The same module through the reference framebuffer and the real S3 panel path.
use std::sync::Arc;

use fab_elec_mcu::compile;
use fab_elec_mcu_emit_s3::{build_image, flash_regions, Board, BoardTime, Image, Panel, SlotOptions, SLOT_BASE, VOICEBOX, VOICEBOX_I2C, VOICEBOX_PANEL};
use fab_elec_mcu_flash::tether::picture_frames;
use fab_elec_mcu_netgw::Link;
use fab_elec_mcu_emit_s3::runtime::PANEL_PROBES;
use fab_elec_mcu_emu::{board::{BusEvent, I2cWhat}, Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, Part, PartDesc, PartKind, PinRef};
use fab_elec_mcu_rt::{bundle, module, sched::{Device, DrawRefusal, Fault, Hal, Role}, serial, watch};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};
use makepad_zune_png::{PngEncoder, makepad_zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions}};

fn profile() -> S3Profile {
    // Same explicit handoff/model inputs as runtime.rs; these are not probe measurements.
    S3Profile {
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT,
        flash_bytes: VOICEBOX.flash_bytes as usize, handoff_cpu: CpuClock::Xtal { div: 2 },
        handoff_mapped_pages: 64, entry: 0x4200_0408, seed: 0x5300,
        time: TimeProfile {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false, mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000,
            usb_frame_phase_ticks: 0, input_sync_delay_ticks: 0,
            handoff_digital_pads_unheld: true, handoff_out: [0; 2], handoff_enable: [0; 2],
            handoff_pin: [0; 45], handoff_io_mux: [1 << 12; 45], handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: 8, handoff_usb_int_ena: 0,
        },
    }
}

fn board() -> BoardDesc {
    let mut desc = BoardDesc {
        seed: 0x5300,
        // The glass is the panel profile's 368x448 window at column 16, row 0.
        parts: vec![PartDesc { refdes: "U1".into(), kind: PartKind::Co5300 { glass: [16, 0, 368, 448] } },
            PartDesc { refdes: "U7".into(), kind: PartKind::Tca9554 { addr: 0x20 } }],
        nets: [("LCD_CS", "GPIO12", "CSX", Pull::Up),
            ("QSPI_SCL", "GPIO11", "SCLK", Pull::None),
            ("QSPI_SIO0", "GPIO4", "SIO0", Pull::None),
            ("QSPI_SIO1", "GPIO5", "SIO1", Pull::None),
            ("QSPI_SIO2", "GPIO6", "SIO2", Pull::None),
            ("QSPI_SIO3", "GPIO7", "SIO3", Pull::None)]
            .into_iter().map(|(name, chip, pin, pull)| NetDesc { rise_ps: None, name: name.into(), pull,
                pins: vec![PinRef::Chip(chip.into()), PinRef::Part { part: 0, pin: pin.into() }] }).collect(),
    };
    // R01–R05: EXIO0 drives RESX, E05 supplies its power-on pull-up.
    desc.nets.push(NetDesc { rise_ps: None, name: "LCD_RESET".into(), pull: Pull::None,
        pins: vec![PinRef::Part { part: 0, pin: "RESX".into() }, PinRef::Part { part: 1, pin: "P0".into() }] });
    for (name, chip, pin) in [("ESP32_SDA", "GPIO15", "SDA"), ("ESP32_SCL", "GPIO14", "SCL")] {
        desc.nets.push(NetDesc { rise_ps: None, name: name.into(), pull: Pull::Up,
            pins: vec![PinRef::Chip(chip.into()), PinRef::Part { part: 1, pin: pin.into() }] });
    }
    for pin in ["P1", "P2", "P3", "P4", "P5", "P6", "P7", "INT"] {
        desc.nets.push(NetDesc { rise_ps: None, name: pin.into(), pull: Pull::Up, pins: vec![PinRef::Part { part: 1, pin: pin.into() }] });
    }
    desc
}

fn png(width: usize, height: usize, rgb: &[[u8; 3]]) -> Vec<u8> {
    let bytes: Vec<u8> = rgb.iter().flatten().copied().collect();
    let mut encoded = Vec::new();
    PngEncoder::new(&bytes, EncoderOptions::new(width, height, ColorSpace::RGB, BitDepth::Eight))
        .encode(&mut encoded).expect("both image assets encode as PNG");
    encoded
}

#[derive(Default)]
struct Reference { events: Vec<watch::Event> }

impl Hal for Reference {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("display program has no GPIO declarations"); }
    fn serial(&mut self, _: u64, _: u32, _: serial::SerialSetting) { panic!("display program has no serial"); }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("display program has no pin writes"); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("display program has no PWM"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("display program has no pin reads"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) {
        let Fault::Draw { site, refusal, .. } = fault else { panic!("unexpected reference fault: {fault:?}"); };
        self.events.push(watch::Event::Fault { code: watch::FAULT_DRAW, site: site as u16, arg: refusal as i32 });
    }
}

#[derive(Default)]
struct Observed {
    bus: Vec<BusEvent>,
    commands: Vec<(u64, u8)>,
    reader: watch::Reader,
    frames: Vec<watch::Frame>,
}

impl Observed {
    /// Returns how many watch bytes left on USB.
    fn take(&mut self, m: &mut S3Machine<Netlist>) -> usize {
        m.take_bus(&mut self.bus);
        let mut out = Vec::new(); m.take_out(&mut out);
        let sent = out.len();
        for byte in out {
            assert_eq!(byte.link, 0, "watch bytes use USB link zero");
            if let Some(frame) = self.reader.push(byte.byte) {
                self.frames.push(frame.expect("every firmware watch frame decodes with a valid CRC"));
            }
        }
        m.mark();
        sent
    }

    fn until_pc(&mut self, m: &mut S3Machine<Netlist>, image: &Image, pc: u32) {
        let mut marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32); marks.insert(pc);
        let deadline = m.now() + 2 * m.tick_hz();
        loop {
            let ran = m.run(&Until { tick: deadline, marks: &marks, stops: &[pc],
                stops_after: m.now(), on_drive: false });
            self.take(m);
            match ran {
                Ran::Pc => break,
                Ran::Barrier => (),
                other => panic!("present must reach {pc:#x} without a model error: {other:?}, {:?}", m.status()),
            }
        }
        assert_eq!(m.status(), Status::Running, "present returns without a peripheral or timing halt");
        assert_eq!(m.pc(), pc, "present stops at the requested handler boundary");
    }

    fn run_setup(&mut self, m: &mut S3Machine<Netlist>, image: &Image) {
        let returned = image.slot.symbols.setup_call + 3;
        let mut stops = image.slot.symbols.panel_commands.clone(); stops.push(returned);
        stops.sort_unstable(); stops.dedup();
        let mut marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32);
        for &pc in &stops { marks.insert(pc); }
        // Run the actual boot record, slot and board. In particular, an Err
        // from a panel timing check becomes a machine halt and fails here.
        loop {
            let ran = m.run(&Until { tick: 2 * m.tick_hz(), marks: &marks, stops: &stops,
                stops_after: m.now(), on_drive: false });
            match ran {
                Ran::Pc if m.pc() == returned => break,
                Ran::Pc => {
                    // BusEvent::Spi records payload bytes, not CMD/ADDR. Pair
                    // each bus transfer with the actual SPI_ADDR store from the
                    // firmware, observed using its source-map PC (no test firmware).
                    assert_eq!(m.cpu().ar[11], 0x6002_4004, "command marks identify SPI_ADDR stores");
                    let address = m.cpu().ar[10];
                    assert_eq!(address & !0x00ff_0000, 0, "commands use the 00 CMD 00 address encoding");
                    self.commands.push((m.now(), (address >> 16) as u8));
                }
                Ran::Barrier => (),
                other => panic!("panel setup must return without a model error: {other:?}, {:?}", m.status()),
            }
            self.take(m);
        }
        assert_eq!(m.status(), Status::Running, "setup returns without a peripheral or timing halt");
        eprintln!("panel setup: {} simulated ticks / {} Hz = {:.6} s; {} instructions",
            m.now(), m.tick_hz(), m.now() as f64 / m.tick_hz() as f64, m.cpu().instret);
        self.take(m);
        // The last Fault can still be queued for the next USB frame (runtime.rs
        // uses the same follow-mode drain). Let that frame leave after setup.
        self.drain(m, image);
    }

    fn drain(&mut self, m: &mut S3Machine<Netlist>, image: &Image) {
        // The runtime queues frames in its TX ring and the model's host reads one
        // 64-byte packet per 1 ms USB frame. Run whole frames until the ring is
        // empty and a frame passes with no output. Timer firings are 100 ms
        // apart, so this cannot run the next handler.
        let marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32);
        let l = &image.slot.layout;
        let word = |m: &S3Machine<Netlist>, address: u32| {
            let mut b = [0; 4];
            assert!(m.read_data(address, &mut b), "the TX ring indices are readable in SRAM");
            u32::from_le_bytes(b)
        };
        for frame in 0.. {
            assert!(frame < 50, "queued watch frames leave within 50 USB frames");
            let deadline = m.now() + m.tick_hz() / 1000;
            let mut sent = 0;
            loop {
                let ran = m.run(&Until { tick: deadline, marks: &marks, stops: &[], stops_after: 0, on_drive: false });
                sent += self.take(m);
                match ran {
                    Ran::Tick => break,
                    Ran::Barrier => (),
                    other => panic!("USB drain must not hide a model failure: {other:?}, {:?}", m.status()),
                }
            }
            if sent == 0 && word(m, l.tx_head) == word(m, l.tx_tail) { break; }
        }
    }
}

#[test]
fn voicebox_panel_matches_the_reference() {
    let p = VOICEBOX_PANEL;
    let language = p.profile();
    let logo = png(4, 4, &[
        [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255],
        [16, 32, 48], [64, 80, 96], [112, 128, 144], [160, 176, 192],
        [255, 128, 0], [0, 128, 255], [255, 0, 128], [128, 255, 0],
        [8, 4, 8], [248, 252, 248], [40, 80, 120], [160, 200, 240],
    ]);
    let badge = png(2, 2, &[[255, 255, 0], [0, 255, 255], [255, 0, 255], [0, 0, 0]]);
    let source = "\
let screen = display()
var finished: i32 = 0
fn setup() {
    fill_rect(screen, 0, 0, display_w, display_h, rgb(17, 93, 151))
    draw_image(screen, logo, logo_w, logo_h, 0, 0)
    draw_image(screen, badge, badge_w, badge_h, 2, 2)
    present(screen, logo, 1, 1, 2, 2, 4, 0)
    draw_image(screen, logo, logo_w, logo_h, -2, 4)
    draw_image(screen, logo, logo_w, logo_h, 1, 0)
    finished = 1
}
";
    let compiled = compile(source, &language, &[], &[("logo.png", &logo), ("badge.png", &badge)])
        .unwrap_or_else(|e| panic!("the panel program compiles: {e:?}"));
    let module = module::load(&compiled.module, &language).expect("the panel module verifies");
    let handoff = profile();
    let image = build_image(&module, &Board { layout: VOICEBOX,
        time: BoardTime { rc_slow_hz: handoff.time.rc_slow_hz, wdt_delay_sel: handoff.time.wdt_delay_sel },
        panel: Some(p), i2c: Some(fab_elec_mcu_emit_s3::VOICEBOX_I2C), motion: None, touch: None, radio: None },
        &SlotOptions { image_names: compiled.images.clone(), ..SlotOptions::default() }).expect("the board panel links into the image");
    let finished = compiled.lowered.state_vars.iter().find(|v| v.name == "finished")
        .expect("setup completion has a state word").offset * 4 + image.slot.layout.state;
    let mut reference = Device::new(module, &language);
    let mut hal = Reference::default(); reference.boot(&mut hal).expect("reference setup executes");
    assert!(matches!(hal.events.as_slice(), [watch::Event::Fault { code: watch::FAULT_DRAW, arg, .. }]
        if *arg == DrawRefusal::OddWindow as i32), "the reference reports exactly the odd-window refusal");
    let desc = board();
    let mut machine = S3Machine::with_board(Arc::new(image.flash.clone()), &handoff,
        Netlist::new(desc.clone()).expect("R01–R05 describe a valid panel netlist"))
        .expect("the panel netlist binds to the S3");
    let Some(Part::Co5300(panel)) = machine.board().part(0) else { panic!("U1 is the panel"); };
    assert!(panel.frame().iter().any(|&pixel| pixel != 0), "seeded panel memory is junk before any draw");
    let mut observed = Observed::default(); observed.run_setup(&mut machine, &image);
    let Some(Part::Co5300(panel)) = machine.board().part(0) else { panic!("U1 remains the panel"); };
    let expected = reference.framebuffer().expect("the reference owns a display framebuffer");
    for y in 0..p.height as usize {
        let start = (y + p.row_offset as usize) * 480 + p.column_offset as usize;
        let actual = &panel.frame()[start..start + p.width as usize];
        let row = &expected[y * p.width as usize..(y + 1) * p.width as usize];
        if actual != row {
            let x = actual.iter().zip(row).position(|(a, b)| a != b).expect("unequal row has a differing pixel");
            panic!("panel equals reference: first mismatch ({x}, {y}): panel={:#06x}, reference={:#06x}", actual[x], row[x]);
        }
    }
    assert!(panel.display_on(), "DISPON completes before setup draws");
    assert!(!panel.inversion(), "INVOFF leaves inversion disabled");
    assert_eq!(panel.brightness(), p.brightness, "WRDISBV sets the board brightness");
    let mut completed = [0; 4];
    assert!(machine.read_data(finished, &mut completed), "setup completion is readable");
    assert_eq!(u32::from_le_bytes(completed), 1, "setup continues after its refused draw");
    let faults: Vec<_> = observed.frames.iter().filter_map(|f| match f.event {
        e @ watch::Event::Fault { .. } => Some(e), _ => None,
    }).collect();
    assert_eq!(faults, hal.events, "S3 Draw fault site and refusal equal the reference without extra faults");

    let reset = machine.board().net("LCD_RESET").expect("LCD_RESET is a named net");
    let reset_edges: Vec<_> = observed.bus.iter().filter_map(|e| match *e {
        BusEvent::Level { t, net, high } if net == reset => Some((t, high)), _ => None,
    }).collect();
    let low = reset_edges.iter().position(|&(_, high)| !high).expect("EXIO0 asserts reset");
    let &(released, high) = reset_edges.get(low + 1).expect("EXIO0 releases reset");
    assert!(high && released - reset_edges[low].0 >= machine.tick_hz() * 20 / 1_000_000,
        "the reset pulse exceeds P06's 10 us and includes the 20 us wait");
    let i2c_bytes: Vec<_> = observed.bus.iter().filter_map(|e| match *e {
        BusEvent::I2c { what: I2cWhat::Byte { value, ack }, .. } => Some((value, ack)), _ => None,
    }).collect();
    assert_eq!(i2c_bytes, [0x40, 1, 0xfe, 0x40, 3, 0xfe, 0x40, 1, 0xff].map(|b| (b, true)),
        "all reset writes ACK in latch/config/release order, preserving the other expander inputs");
    let byte_ticks: Vec<_> = observed.bus.iter().filter_map(|e| match *e {
        BusEvent::I2c { t, what: I2cWhat::Byte { .. }, .. } => Some(t), _ => None,
    }).collect();
    assert_eq!(byte_ticks[1] - byte_ticks[0], 9 * machine.tick_hz() / 400_000,
        "panel reset writes use the shared board's 400 kHz configuration");
    let transfers: Vec<_> = observed.bus.iter().filter_map(|e| match *e {
        BusEvent::Spi { t0, t1, bit_ticks, mode, lines, head, len, .. } =>
            Some((t0, t1, bit_ticks, mode, lines, head, len)), _ => None,
    }).collect();
    assert_eq!(transfers.len(), observed.commands.len(), "every executed panel command reaches one bus transfer");
    assert_eq!(observed.commands.iter().take(8).map(|&(_, cmd)| cmd).collect::<Vec<_>>(),
        [0x01, 0x11, 0xc4, 0x3a, 0x53, 0x51, 0x20, 0x29], "the init commands reach the panel in order");
    let wait = machine.tick_hz() * 120 / 1000;
    assert!(transfers[0].0 >= released + wait, "SWRESET follows the full hardware-reset wait");
    assert!(transfers[1].0 >= transfers[0].1 + wait, "SLPOUT follows the unknown-sleep SWRESET wait");
    assert!(transfers[2].0 >= transfers[1].1 + wait, "SetSPIMode follows the full SLPOUT wait");
    assert_eq!((transfers[2].5[0], transfers[2].6), (0x80, 1), "SetSPIMode sets SPI_WRAM alone (P23)");
    assert_eq!((transfers[3].5[0], transfers[3].6), (0x55, 1), "COLMOD has one RGB565 parameter");
    assert_eq!((transfers[4].5[0], transfers[4].6), (0x20, 1), "WRCTRLD sets BCTRL alone, no dimming (P24)");
    assert_eq!((transfers[5].5[0], transfers[5].6), (p.brightness, 1), "brightness has one board-selected parameter");
    for (i, (transfer, &(store_tick, _))) in transfers.iter().zip(&observed.commands).enumerate() {
        assert!(store_tick < transfer.0, "command store {i} precedes its wire transfer");
        if i != 0 { assert!(store_tick > transfers[i - 1].1, "transfer {i} waits for the previous transfer to finish"); }
        assert_eq!((transfer.2, transfer.3, transfer.4), (12, 0, [1, 1, 1]),
            "transfer {i} uses 40 MHz, mode zero and one wire for every phase");
    }
    assert!(observed.commands.iter().filter(|&&(_, cmd)| cmd == 0x2c).count() > 5,
        "the full-screen fill spans multiple DMA strips before the four image draws");

    let mut absent = desc; absent.parts[1].kind = PartKind::Tca9554 { addr: 0x21 };
    let mut machine = S3Machine::with_board(Arc::new(image.flash.clone()), &handoff,
        Netlist::new(absent).expect("the absent-address variant is a valid netlist"))
        .expect("the absent-address netlist binds to the same image");
    let mut failed = Observed::default(); failed.run_setup(&mut machine, &image);
    let faults: Vec<_> = failed.frames.iter().filter_map(|f| match f.event {
        e @ watch::Event::Fault { .. } => Some(e), _ => None,
    }).collect();
    let mut expected_faults = vec![watch::Event::Fault { code: watch::FAULT_PANEL, site: 0, arg: 4 }];
    expected_faults.extend_from_slice(&hal.events);
    assert_eq!(faults, expected_faults, "missing U7 reports step 4, then the same setup Draw refusal");
    assert!(machine.read_data(finished, &mut completed), "failed-panel setup completion is readable");
    assert_eq!(u32::from_le_bytes(completed), 1, "setup completes even when panel bring-up fails");
    assert!(failed.commands.is_empty() && !failed.bus.iter().any(|e| matches!(e, BusEvent::Spi { .. })),
        "no SPI transfer reaches the panel after an expander NACK");
}

#[test]
fn surface_and_image_presents_match_after_each_call() {
    // Keep the same board wiring and real boot/slot path, with a small visible
    // area. The full-size test above already exercises multi-strip transfers. This
    // panel also reports its init (Panel::report), checked after setup.
    let p = Panel { width: 8, height: 8, report: true, ..VOICEBOX_PANEL };
    let language = p.profile();
    let rgb: Vec<_> = (0..16).map(|k| [8 * (k + 16), 12 * k, 8 * (k ^ 7)]).collect();
    let tile = png(4, 4, &rgb);
    let source = include_str!("../../lang/tests/support/surfaces.splash");
    let compiled = compile(source, &language, &[], &[("tile.png", &tile)]).expect("shared surface fixture compiles for S3");
    let module = module::load(&compiled.module, &language).expect("shared surface fixture verifies for S3");
    let handoff = profile();
    let config = Board { layout: VOICEBOX,
        time: BoardTime { rc_slow_hz: handoff.time.rc_slow_hz, wdt_delay_sel: handoff.time.wdt_delay_sel },
        panel: Some(p), i2c: Some(fab_elec_mcu_emit_s3::I2cBus { hz: 100_000, ..fab_elec_mcu_emit_s3::VOICEBOX_I2C }), motion: None, touch: None, radio: None };
    let image = build_image(&module, &config, &SlotOptions { image_names: compiled.images.clone(), ..SlotOptions::default() })
        .expect("surface and flash buffers link through the real image builder");
    let l = &image.slot.layout;
    let files = bundle::read(&image.slot.bundle).expect("the slot's file bundle reads");
    let file = files.find("tile").expect("the bundle files the image under its name");
    assert_eq!((file.a, file.b, file.data.len()), (4, 4, 32), "the bundle holds the image's RGB565 pixels and size");
    assert_eq!(l.buffers[0].address, VOICEBOX.bundle_data_base() + file.offset, "the image reads in place from the slot's bundle");
    assert_eq!(image.record.bundle_pages, 1, "R0 names the bundle's one page");
    assert_eq!(flash_regions(&image.flash).last(), Some(&(VOICEBOX.bundles[0], &image.slot.bundle[..])),
        "Flash to writes bundle A after the head and slot A");
    assert_eq!(l.buffers.iter().map(|b| b.region).collect::<Vec<_>>(), [None, Some(0), Some(0), None],
        "only writable buffers are placed in SRAM; both image declarations live in flash");
    assert_eq!(l.regions.iter().map(|r| r.used - r.start).sum::<u64>(), 52,
        "flash images consume no part of the RAM placement regions or boot clear spans");
    assert_eq!(l.buffers[0].address, l.buffers[3].address, "two declarations of one image link to the same pixels despite distinct provisional addresses");
    let mut reference = Device::new(module, &language);
    let mut hal = Reference::default(); reference.boot(&mut hal).expect("reference surface setup executes");
    let mut machine = S3Machine::with_board(Arc::new(image.flash.clone()), &handoff,
        Netlist::new(board()).expect("surface panel wiring is valid")).expect("surface image binds to the board model");
    let mut observed = Observed::default(); observed.run_setup(&mut machine, &image);
    let byte_ticks: Vec<_> = observed.bus.iter().filter_map(|e| match *e {
        BusEvent::I2c { t, what: I2cWhat::Byte { .. }, .. } => Some(t), _ => None,
    }).collect();
    assert_eq!(byte_ticks[1] - byte_ticks[0], 9 * machine.tick_hz() / 100_000,
        "the panel also follows a board configured for standard-mode I2C");
    let probes: Vec<_> = observed.frames.iter().filter_map(|f| match f.event {
        watch::Event::Probe { id, value } => Some((id, value)), _ => None,
    }).collect();
    assert_eq!(probes.iter().map(|&(id, _)| id).collect::<Vec<_>>(), (PANEL_PROBES..PANEL_PROBES + 10).collect::<Vec<_>>(),
        "a panel report probes each stage once, in init's order");
    assert_eq!([probes[0].1, probes[1].1, probes[2].1], [0xff; 3],
        "as found: every expander pin an input pulled high, Output and Config at their resets (E03-E05)");
    assert_eq!([probes[4].1, probes[5].1, probes[6].1], [0xff, 0xff, 0xfe],
        "after the reset pulse: EXIO0 drives RESX high, every other pin stays an input");
    // A time probe counts 1,024 systimer ticks (16 MHz): 120 ms is 1,875 of them.
    let times = [probes[3].1, probes[7].1, probes[8].1, probes[9].1];
    assert!(times[1] - times[0] >= 1875 && times[2] - times[1] >= 1875 && times[3] > times[2],
        "SWRESET's and SLPOUT's 120 ms waits pass between the stages: {times:?}");
    let same = |machine: &S3Machine<Netlist>, reference: &Device<'_>, phase: &str| {
        let mut state = vec![0; l.state_words as usize * 4];
        assert!(machine.read_data(l.state, &mut state), "{phase}: firmware state is readable");
        assert_eq!(state, reference.state().iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>(),
            "{phase}: dynamic image indexing, constant alias indexing and copied pixels equal the reference");
        for (placed, words) in l.buffers.iter().zip(reference.buffers()) {
            let expected: Vec<_> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            if placed.region.is_some() {
                let mut actual = vec![0; placed.allocated_bytes as usize];
                assert!(machine.read_data(placed.address, &mut actual), "{phase}: surface allocation is readable");
                assert_eq!(actual, expected, "{phase}: buffer {} equals the reference, including the image-to-surface row copy", placed.name);
            } else {
                // Machine::read_data exposes only SRAM. Inspect the bundle bytes
                // here; actual DROM L16UI reads are proved by state/copy above.
                let at = (placed.address - VOICEBOX.bundle_data_base()) as usize;
                assert_eq!(image.slot.bundle[at..at + expected.len()], expected,
                    "{phase}: flash image {} uses little-endian buffer packing", placed.name);
            }
        }
        let Some(Part::Co5300(panel)) = machine.board().part(0) else { panic!("U1 remains the panel"); };
        let expected = reference.framebuffer().expect("reference declares a display");
        for y in 0..p.height as usize {
            let at = (y + p.row_offset as usize) * 480 + p.column_offset as usize;
            assert_eq!(&panel.frame()[at..at + p.width as usize], &expected[y * p.width as usize..(y + 1) * p.width as usize],
                "{phase}: panel row {y} equals the reference after the blocking present returns");
        }
        assert!(machine.last_reset().is_none(), "{phase}: boot, copy and display loops keep the watchdog fed");
    };
    same(&machine, &reference, "setup");
    for phase in ["surface_first", "image_next", "clipped", "refused"] {
        let ran = reference.step(&mut hal).expect("reference has the next present handler");
        assert_eq!(compiled.lowered.entries[ran.entry as usize].name, phase, "present handlers execute in source order");
        observed.until_pc(&mut machine, &image, image.slot.handlers[ran.entry as usize].address);
        observed.until_pc(&mut machine, &image, image.slot.symbols.handler_return);
        same(&machine, &reference, phase);
    }
    observed.drain(&mut machine, &image);
    let faults: Vec<_> = observed.frames.iter().filter_map(|f| match f.event {
        e @ watch::Event::Fault { .. } => Some(e), _ => None,
    }).collect();
    assert_eq!(faults, hal.events, "surface and image draw refusals retain their reference sites and kinds");

    let oversized = fab_elec_mcu_rt::Profile { buffer_bytes: language.buffer_bytes + 4, ..language };
    let large = compile("let largest = bytes(393216)\nlet small = bytes(1)\nfn setup() {}", &oversized, &[], &[("tile.png", &tile)])
        .expect("image plus oversized RAM fixture compiles for placement refusal");
    let large = module::load(&large.module, &oversized).expect("oversized fixture verifies under its declared budget");
    let refused = build_image(&large, &config, &SlotOptions::default()).err().expect("image builder refuses oversized writable allocations");
    for text in ["tile", "flash", "largest", "small", "does not fit"] {
        assert!(refused.message.contains(text), "placement refusal names {text}, including flash images: {refused}");
    }
}

#[test]
fn tether_pictures_land_on_the_panel() {
    // The handheld's pictures firmware on the voicebox panel, fed by the flasher's own frame encoder
    // through the host link: each two-row band lands in the panel's memory as sent, and a frame of
    // another EtherType or at an odd row is counted and dropped.
    let p = VOICEBOX_PANEL;
    let language = p.profile();
    let source = include_str!("../../../fixtures/handheld_pictures.fw.splash");
    let compiled = compile(source, &language, &[], &[]).expect("the pictures firmware compiles for S3");
    let module = module::load(&compiled.module, &language).expect("the pictures firmware verifies");
    let handoff = profile();
    let config = Board { layout: VOICEBOX,
        time: BoardTime { rc_slow_hz: handoff.time.rc_slow_hz, wdt_delay_sel: handoff.time.wdt_delay_sel },
        panel: Some(p), i2c: Some(VOICEBOX_I2C), motion: None, touch: None, radio: None };
    let image = build_image(&module, &config, &SlotOptions::default()).expect("the pictures image links");
    let mut machine = S3Machine::with_board(Arc::new(image.flash.clone()), &handoff,
        Netlist::new(board()).expect("the panel wiring is valid")).expect("the pictures image binds to the board model");
    let width = p.width as usize;
    // Sixteen rows whose every pixel differs from its neighbours and from the cleared panel's black.
    let pixels: Vec<u16> = (0..width * 16).map(|n| (n as u16).wrapping_mul(40503) | 1).collect();
    let mut frames = picture_frames(7, width, &pixels);
    let mut foreign = frames[0].clone(); foreign[13] = 0xb6;
    let mut odd = frames[1].clone(); odd[16] = 3;
    frames.extend([foreign, odd]);
    let marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32);
    let ms = machine.tick_hz() / 1000;
    let mut link = Link::new();
    let mut queued = false;
    let mut probe = [0u16; 3];
    let mut reports = Vec::new();
    machine.set_usb_host_listening(true);
    while reports.last().is_none_or(|r: &[u16; 3]| r[0] < 8 || r[2] < 2) {
        assert!(machine.now() < 3 * machine.tick_hz(), "the band lands within three simulated seconds: {reports:?}");
        let end = (machine.now() / ms + 1) * ms;
        loop {
            let ran = machine.run(&Until { tick: end, marks: &marks, stops: &[], stops_after: 0, on_drive: false });
            assert!(matches!(ran, Ran::Tick | Ran::Barrier), "the firmware runs without a model error: {ran:?}, {:?}", machine.status());
            let mut out = Vec::new(); machine.take_out(&mut out);
            let now_ms = machine.now() / ms;
            for byte in out {
                let Some(decoded) = link.push_raw(now_ms, byte.byte) else { continue };
                match decoded.expect("every device frame decodes").event {
                    watch::Event::Hello { .. } => {
                        assert!(!queued, "the device starts once");
                        for frame in &frames { link.queue_frame(frame).expect("picture frames have Ethernet lengths"); }
                        queued = true;
                    }
                    watch::Event::Probe { id, value } => {
                        probe[usize::from(id) - 1] = value;
                        if id == 3 { reports.push(probe); }
                    }
                    e @ watch::Event::Fault { .. } => panic!("the pictures firmware faults: {e:?}"),
                    _ => {}
                }
            }
            let mut wire = Vec::new();
            link.poll_raw(now_ms, &mut |packet| wire.extend_from_slice(packet));
            for byte in wire { machine.link_in(0, byte); }
            machine.mark();
            if ran == Ran::Tick { break; }
        }
    }
    assert_eq!(*reports.last().unwrap(), [8, 1, 2], "eight bands of one picture drawn, the two malformed frames dropped");
    let Some(Part::Co5300(panel)) = machine.board().part(0) else { panic!("U1 remains the panel"); };
    for y in 0..17 {
        let at = (y + p.row_offset as usize) * 480 + p.column_offset as usize;
        let expected: Vec<u16> = if y < 16 { pixels[y * width..(y + 1) * width].to_vec() } else { vec![0; width] };
        assert_eq!(panel.frame()[at..at + width], expected[..], "panel row {y} holds the band sent for it (row 16 stays cleared)");
    }
    assert!(machine.last_reset().is_none(), "receiving and drawing keep the watchdog fed");
}
