//! F10: a recorded ramp through the codec's ASDOUT, I2S0 RX and a two-descriptor SRAM ring.

use std::sync::Arc;

use fab_elec_mcu_emu::{Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, PartDesc, PartKind, PinRef, SoundTrack, Stimulus, TrackRef};
use fab_elec_mcu_s3::S3Machine;

mod support;
use support::i2c_fixture::{i2c_write, I2C_CLR, I2C_COMPLETE, I2C_RSTART, I2C_STOP};
use support::{mux_reg, out_sel_reg, pin_reg, pins_at_pc, profile, Program, GPIO_BASE, MUX_GPIO, MUX_IE, RAM, TICK_HZ};

// trm.txt:13407,13450. The ring holds eight stereo frames in internal SRAM.
const I2S: u32 = 0x6000_f000;
const DMA: u32 = 0x6003_f000;
const DESC: u32 = RAM + 64;
const BUF: u32 = RAM + 128;
const RAMP: [i16; 8] = [-28000, -20000, -12000, -4000, 4000, 12000, 20000, 28000];
const RATE: u32 = 16_000;
// es8311-datasheet.txt:303-307, CE low.
const CODEC: u8 = 0x18;

fn board() -> Netlist {
    // es8311-datasheet.txt:231-238. I2C control, three clocks and ASDOUT back to DIN.
    let nets = [(14, "SCL", Pull::Up), (15, "SDA", Pull::Up), (4, "MCLK", Pull::None),
        (5, "SCLK", Pull::None), (6, "LRCK", Pull::None), (7, "ASDOUT", Pull::None), (8, "DSDIN", Pull::None)]
        .into_iter().map(|(gpio, pin, pull)| NetDesc {
            name: pin.into(), rise_ps: None, pull,
            pins: vec![PinRef::Chip(format!("GPIO{gpio}").into()), PinRef::Part { part: 0, pin: pin.into() }],
        }).collect();
    let codec = PartDesc { refdes: "U1".into(), kind: PartKind::Es8311 { addr: CODEC } };
    Netlist::new(BoardDesc { seed: 0x8311, parts: vec![codec], nets }).expect("the codec board resolves")
}

#[derive(Clone, Copy, PartialEq)]
enum Clocks { Rx, TxFirst, RxFirst }

fn firmware(clocks: Clocks) -> (Arc<Vec<u8>>, u32) {
    let duplex = clocks != Clocks::Rx;
    let mut p = Program::new();
    p.disable_watchdogs();
    // trm.txt:28796-28892,28916-28960,28970-29065,29090-29134. I2S0 and GDMA out of reset.
    for (clk, rst, bit) in [(0x600c_0018, 0x600c_0020, 4), (0x600c_001c, 0x600c_0024, 6)] {
        p.time_bits(rst, 0, 1 << bit);
        p.time_bits(clk, 0, 1 << bit);
        p.time_bits(rst, 1 << bit, 0);
    }
    p.i2c_setup();
    for (reg, value) in [
        (0x00, 0x1f), (0x01, if duplex { 0x3f } else { 0x3a }),
        (0x09, 0x0c), (0x12, if duplex { 1 } else { 2 }), // ring-facts §4, DAC in duplex.
        (0x0a, 0x0c), // es8311-datasheet.txt:842-861. Unmuted Philips, 16-bit output.
        (0x0d, 0x06), (0x0e, 0), // es8311-datasheet.txt:886-915. Normal VMID, references and PGA on.
        (0x14, 0x10), (0x16, 0), (0x17, 0xbf), // es8311-datasheet.txt:973-982,1023-1046. MIC1, unity gains.
        (0x00, if duplex { 0xa4 } else { 0xa5 }),
    ] {
        p.i2c_list(&[CODEC << 1, reg, value], &[I2C_RSTART, i2c_write(3), I2C_STOP]);
        p.i2c_wait();
        p.store(I2C_CLR, I2C_COMPLETE);
    }
    // trm.txt:37490-37506,37517-37536,38345-38370,38704-38721,38837-38864. Module clock on,
    // XTAL/(9+49/64) = 4.096 MHz, BCK = MCLK/8. Program only the master divider.
    p.store(I2S + 0x34, 1 << 29);
    let clkm = if duplex { 0x34 } else { 0x30 };
    p.store(I2S + clkm, (1 << 29) | (1 << 26) | 9);
    p.store(I2S + clkm + 8, (1 << 27) | (3 << 18) | (4 << 9) | 15);
    // trm.txt:15868-15888,15930-15949,16185-16187,16205-16207,17188-17239.
    for (gpio, signal) in [(4, 23), (5, if duplex { 22 } else { 26 }), (6, if duplex { 24 } else { 27 })] {
        p.store(pin_reg(gpio), 0);
        p.store(mux_reg(gpio), MUX_GPIO);
        p.store(out_sel_reg(gpio), signal);
    }
    p.store(mux_reg(7), MUX_GPIO | MUX_IE);
    p.store(GPIO_BASE + 0x154 + 4 * 25, 0x80 | 7);
    // trm.txt:38264-38339,38421-38476,38517-38529. Two 16-bit Philips
    // slots, stopping on EOF after the eight ramp frames.
    let format = (1 << 29) | (15 << 24) | (16 << 18) | (15 << 13) | (7 << 7) | 15;
    let conf = (1 << 19) | (1 << 15) | (1 << 13) | (1 << 12);
    let tx = conf | (1 << 27);
    if duplex {
        p.store(mux_reg(8), MUX_GPIO); p.store(out_sel_reg(8), 25);
        p.store(I2S + 0x2c, format | (1 << 30)); p.store(I2S + 0x54, 0x10003);
        p.store(I2S + 0x24, tx | (1 << 8));
        // Four iterations at the 20 MHz fixture CPU exceed one 4.096 MHz UPDATE period.
        p.counted_loop(4); p.store(I2S + 0x24, tx | 3);
        p.store(DMA + 0x60, 13); p.store(DMA + 0x60, 12); p.store(DMA + 0xa8, 3);
        p.store(DMA + 0x80, ((DESC + 24) & 0xfffff) | (1 << 21));
    }
    p.store(I2S + 0x28, format);
    p.store(I2S + 0x50, 0x10003);
    let initial_frame = u32::from(clocks != Clocks::TxFirst);
    p.store(I2S + 0x64, 2 * (initial_frame + RAMP.len() as u32) - 1);
    let rx = conf | if duplex { 1 << 3 } else { 0 };
    p.store(I2S + 0x20, rx | (1 << 8));
    p.counted_loop(4);
    p.store(I2S + 0x20, rx | 3);
    // trm.txt:11692-11710,12156-12208,12230-12255,13030-13051. GDMA IN0,
    // default watermark, owner check off so a ninth frame can wrap to the first buffer.
    p.store(DMA, 1);
    p.store(DMA, 0);
    p.store(DMA + 4, 12);
    p.store(DMA + 0x48, 3);
    p.store(DMA + 0x20, (DESC & 0xfffff) | (1 << 22));
    let start = match clocks {
        Clocks::Rx => p.store(I2S + 0x20, rx | 4),
        Clocks::TxFirst => {
            let pc = p.store(I2S + 0x24, tx | 4);
            p.counted_loop(100); // 10 us at 20 MHz: the first WS period is in flight.
            p.store(I2S + 0x20, rx | 4);
            pc
        }
        Clocks::RxFirst => {
            p.store(I2S + 0x20, rx | 4);
            p.counted_loop(1000); // More than a WS period waiting for BCK.
            p.store(I2S + 0x24, tx | 4)
        }
    };
    p.time_sleep();
    p.halt();
    (p.finish(), start)
}

#[test]
fn first_whole_frame_after_attach_starts_the_ramp_in_sram() {
    record_ramp(Clocks::Rx);
}

#[test]
fn loopback_slave_records_asdout_on_tx_clocks() {
    record_ramp(Clocks::TxFirst);
    record_ramp(Clocks::RxFirst);
}

fn record_ramp(clocks: Clocks) {
    let (image, start) = firmware(clocks);
    let mut m = S3Machine::with_board(image, &profile(0x8311), board()).expect("the codec board binds");
    // trm.txt:11414-11455. Two linked IN descriptors, each with capacity for four frames.
    let ring: Vec<_> = [0x8000_0010, BUF, DESC + 12, 0x8000_0010, BUF + 16, DESC]
        .into_iter().flat_map(u32::to_le_bytes).collect();
    assert!(m.seed_data(DESC, &ring));
    assert!(m.seed_data(BUF, &[0xa5; 32]));
    if clocks != Clocks::Rx {
        let tx: Vec<_> = [0xc002_4024, BUF + 32, 0].into_iter().flat_map(u32::to_le_bytes).collect();
        assert!(m.seed_data(DESC + 24, &tx));
        assert!(m.seed_data(BUF + 32, &[0; 36]));
    }
    pins_at_pc(&mut m, start);
    let mut initial = [0; 32];
    assert!(m.read_data(BUF, &mut initial));
    assert_eq!(initial, [0xa5; 32], "no RX transfer before the master starts");
    // The twin presents frame zero one BCK after the START store (tests/i2s.rs).
    // Attach halfway through it. Its successor is the first whole frame after attach,
    // independently of the sample values or the tick at which the I2C setup finished.
    let first_edge = m.now() + TICK_HZ.div_ceil(512_000);
    let period = TICK_HZ / u64::from(RATE);
    let marks = PcSet::default();
    let advance = |m: &mut S3Machine<Netlist>, tick| {
        assert_eq!(m.run(&Until { tick, marks: &marks, stops: &[], stops_after: 0, on_drive: false }), Ran::Tick);
    };
    advance(&mut m, first_edge + period / 2);
    assert!((first_edge + 1..first_edge + period).contains(&m.now()));
    let origin = (u128::from(m.board_time()) * 1_000_000_000_000 / u128::from(TICK_HZ)) as u64;
    let track = Arc::new(SoundTrack::new(origin, (RATE, 1)));
    for x in RAMP { track.push(x); }
    track.close();
    assert!(m.host_input(&Stimulus::Sound { part: 0, track: TrackRef(track) }));
    advance(&mut m, first_edge + period + period / 2);
    let mut first = [0xa5; 4];
    assert!(m.read_data(BUF, &mut first));
    let joined_late = clocks == Clocks::TxFirst;
    assert_eq!(first, if joined_late { [0xa5; 4] } else { [0; 4] },
        "RX accepts the silent first frame only when armed before its first BCK");
    advance(&mut m, first_edge + 10 * period);
    let mut bytes = [0; 32];
    assert!(m.read_data(BUF, &mut bytes));
    for (k, x) in RAMP.into_iter().enumerate() {
        let offset = 4 * ((k + usize::from(!joined_late)) % RAMP.len());
        let [lo, hi] = x.to_le_bytes();
        assert_eq!(bytes[offset..offset + 4], [lo, hi, lo, hi], "both ASDOUT slots of ramp sample {k}");
    }
    if clocks != Clocks::Rx {
        let mut header = [0; 4];
        assert!(m.read_data(DESC + 24, &mut header));
        assert_eq!(u32::from_le_bytes(header), 0x4002_4024, "TX returned its completed descriptor");
    }
}

#[test]
fn rx_slave_requires_synchronized_tx_loopback() {
    let mut p = Program::new();
    p.time_bits(0x600c_0020, 0, 1 << 4);
    p.time_bits(0x600c_0018, 0, 1 << 4);
    p.time_bits(0x600c_0020, 1 << 4, 0);
    let pc = p.store(I2S + 0x20, (1 << 12) | (1 << 3));
    let mut m = S3Machine::new(p.finish(), &profile(0x8311));
    let marks = PcSet::default();
    assert_eq!(m.run(&Until { tick: TICK_HZ / 1000, marks: &marks, stops: &[], stops_after: 0, on_drive: false }), Ran::Halted);
    let Status::Halted(halt) = m.status() else { panic!("expected RX slave refusal"); };
    assert_eq!((halt.pc, halt.what), (pc, "I2S RX slave requires synchronized TX SIG_LOOPBACK"));
}
