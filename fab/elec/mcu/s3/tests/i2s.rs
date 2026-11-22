use fab_elec_mcu_emit32::xtensa::{ccz, enc};
use fab_elec_mcu_emu::board::{Board, BusEvent, I2cAns, I2cOp, I2sFrame, I2sStandard, NetInfo, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, HaltKind, Machine, PcSet, Ran, Status, Until};
use fab_elec_mcu_s3::S3Machine;

mod support;
use support::{mux_reg, out_sel_reg, pin_reg, pins_at_pc, profile, Program, GPIO_BASE, MUX_GPIO, MUX_IE, RAM, TICK_HZ};

// trm.txt:13407, 13450. Descriptor and payload locations are fixture allocations in SRAM.
const I2S: u32 = 0x6000_f000;
const DMA: u32 = 0x6003_f000;
const TX_DESC: u32 = RAM + 64;
const RX_DESC: u32 = RAM + 96;
const TX_BUF: u32 = RAM + 128;
const RX_BUF: u32 = RAM + 160;
const TX: [[i16; 2]; 3] = [[-32768, 0x1234], [0x4321, -2], [-7654, 32767]];
const RX: [[i16; 2]; 3] = [[-989, 1234], [256, -32768], [32767, -1]];

#[derive(Clone, Default)]
struct Receiver {
    frames: Vec<I2sFrame>,
    mark: usize,
}

impl Board for Receiver {
    type In = ();
    fn bind(&mut self, _: &[&'static str], _: u64) -> Result<(), String> { Ok(()) }
    fn pad_drive(&mut self, _: u64, _: u16, _: Drive) -> Res { Ok(()) }
    fn pad_inputs(&mut self, _: &mut Vec<(u16, bool)>) {}
    fn chip_reset(&mut self, _: u64) -> Res { Ok(()) }
    fn i2c(&mut self, _: u64, _: u16, _: u16, _: I2cOp) -> Res<I2cAns> { Err("unexpected I2C transfer") }
    fn spi(&mut self, _: &SpiXfer, _: &dyn Payload, _: &mut Vec<u8>) -> Res { Err("unexpected SPI transfer") }
    fn i2s(&mut self, f: &I2sFrame) -> Res<[i32; 2]> {
        let answer = RX.get(self.frames.len()).ok_or("unexpected extra I2S frame")?;
        self.frames.push(*f);
        Ok(answer.map(i32::from))
    }
    fn next_event(&self) -> Option<u64> { None }
    fn event(&mut self, _: u64) -> Res { Err("unexpected receiver event") }
    fn input(&mut self, _: u64, _: ()) -> Res { Ok(()) }
    fn mark(&mut self) { self.mark = self.frames.len(); }
    fn rewind(&mut self) { self.frames.truncate(self.mark); }
    fn barrier(&self) -> bool { false }
    fn nets(&self) -> &[NetInfo] { &[] }
    fn take_bus(&mut self, _: &mut Vec<BusEvent>) {}
    fn publish(&mut self, _: f64) {}
}

fn save(p: &mut Program, addr: u32, dst: u32) {
    p.read_time(addr, 2);
    p.load(3, dst);
    p.emit(enc::s32i(2, 3, 0));
}

fn poll(p: &mut Program, addr: u32, mask: u32, until_set: bool) {
    p.load(1, addr);
    p.load(2, mask);
    let again = p.emit(enc::l32i(3, 1, 0));
    p.emit(enc::and(3, 3, 2));
    p.emit(enc::bccz(if until_set { ccz::EQ } else { ccz::NE }, 3, (i64::from(again) - i64::from(p.pc()) - 4) as i32));
}

fn word(m: &impl Machine, addr: u32) -> u32 {
    let mut bytes = [0; 4];
    assert!(m.read_data(addr, &mut bytes), "firmware results reside in internal SRAM");
    u32::from_le_bytes(bytes)
}

fn samples(data: [[i16; 2]; 3]) -> Vec<u8> {
    data.into_iter().flatten().flat_map(i16::to_le_bytes).collect()
}

#[test]
fn philips_duplex_frames_follow_the_programmed_clocks_and_descriptor_lists() {
    let mut p = Program::new();
    p.disable_watchdogs();
    // trm.txt:13455, 28796–28892, 28916–28960, 28970–29065, 29090–29134: SYSTEM I2S0 and DMA.
    for (clk, rst, bit) in [(0x600c_0018, 0x600c_0020, 4), (0x600c_001c, 0x600c_0024, 6)] {
        p.time_bits(rst, 0, 1 << bit);
        p.time_bits(clk, 0, 1 << bit);
        p.time_bits(rst, 1 << bit, 0);
    }
    // trm.txt:38101–38122, 38519–38529, 38837–38853, 39062–39079.
    for (i, reg) in [0x0c, 0x64, 0x38, 0x6c, 0x80].into_iter().enumerate() {
        save(&mut p, I2S + reg, RAM + 4 * i as u32);
    }

    // trm.txt:37989–38014, 38016–38032. XTAL/(9+49/64), complement Z=15, X=3, Y=4;
    // trm.txt:37477–37516, 37517–37536, 38345–38370, 38697–38721, 38837–38864, 38941–38968.
    for (off, extra) in [(0x30, 0), (0x34, 1 << 29)] {
        p.store(I2S + off, extra | (1 << 26) | 9);
        p.store(I2S + off + 8, (1 << 27) | (3 << 18) | (4 << 9) | 15);
    }
    // trm.txt:16185–16187, 16205–16207, 15930–15949, 16764–16775, 17188–17239.
    // Use GPIO above the pad-number gap for DIN, checking the board's compact pad index.
    for (gpio, signal) in [(4, 23), (5, 22), (6, 24), (7, 25)] {
        p.store(pin_reg(gpio), 0);
        p.store(mux_reg(gpio), MUX_GPIO);
        p.store(out_sel_reg(gpio), signal);
    }
    p.store(mux_reg(26), MUX_GPIO | MUX_IE);
    p.store(GPIO_BASE + 0x154 + 4 * 25, 0x80 | 26);

    // trm.txt:38304–38339, 38651–38691: 16 BCK per half, width fields encoded minus one.
    let format = (1 << 29) | (15 << 24) | (16 << 18) | (15 << 13) | (7 << 7) | 15;
    p.store(I2S + 0x28, format);
    p.store(I2S + 0x2c, format | (1 << 30));
    // trm.txt:38421–38476, 38517–38518, 38727–38785, 38826–38831: channels 0 and 1.
    p.store(I2S + 0x50, 0x10003);
    p.store(I2S + 0x54, 0x10003);
    // trm.txt:38264–38287, 38298–38301, 38609–38637, 38649–38650.
    // RX stops on successful EOF; TX stops on empty and drives the shared clocks.
    let rx = (1 << 19) | (1 << 15) | (1 << 13) | (1 << 12);
    let tx = rx | (1 << 27);
    for (reg, conf) in [(0x20, rx), (0x24, tx)] {
        p.store(I2S + reg, conf | (1 << 8));
        poll(&mut p, I2S + reg, 1 << 8, false);
        p.store(I2S + reg, conf | 3);
    }
    p.store(I2S + 0x14, 3); // trm.txt:38149–38169: RX_DONE and TX_DONE.
    p.store(I2S + 0x64, 5); // trm.txt:38527–38529: six 16-bit samples.

    // trm.txt:11692–11710, 12156–12208, 12230–12319, 12339–12367, 13030–13051.
    for (base, desc, start, cfg) in [(DMA, RX_DESC, 1 << 22, 0), (DMA + 0x60, TX_DESC, 1 << 21, 12)] {
        p.store(base, cfg | 1);
        p.store(base, cfg);
        p.store(base + 4, (1 << 12) | if base == DMA { 12 } else { 0 });
        p.store(base + 0x48, 3);
        p.store(base + 0x20, (desc & 0xfffff) | start);
    }
    let start = p.store(I2S + 0x24, tx | 4);
    p.store(I2S + 0x20, rx | 4);
    poll(&mut p, I2S + 0x10, 2, true);
    save(&mut p, I2S + 0x10, RAM + 20);
    save(&mut p, DMA + 0x28, RAM + 24); // trm.txt:12833–12839.
    save(&mut p, DMA + 0x88, RAM + 28); // trm.txt:12961–12974.
    save(&mut p, DMA + 0x08, RAM + 32); // trm.txt:12424–12465.
    save(&mut p, DMA + 0x68, RAM + 36); // trm.txt:12590–12617.
    p.store(I2S + 0x24, tx);
    p.store(I2S + 0x20, rx);
    let refused = p.store(I2S + 0x24, (tx & !(1 << 19)) | (1 << 20));
    let mut m = S3Machine::with_board(p.finish(), &profile(531), Receiver::default()).expect("audio board binds");
    // trm.txt:11414–11455. End OUT descriptors after 1 and 3 frames, IN after 2 and 3.
    for (addr, words) in [
        (TX_DESC, [0x8000_4004, TX_BUF, TX_DESC + 12, 0xc000_8008, TX_BUF + 4, 0]),
        (RX_DESC, [0x8000_0008, RX_BUF, RX_DESC + 12, 0x8000_0008, RX_BUF + 8, 0]),
    ] {
        let bytes: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        assert!(m.seed_data(addr, &bytes), "DMA descriptor list fits in internal SRAM");
    }
    assert!(m.seed_data(TX_BUF, &samples(TX)), "DMA OUT sample data fits");
    assert!(m.seed_data(RX_BUF, &[0xa5; 16]), "DMA IN buffer starts with a distinguishable unwritten tail");
    pins_at_pc(&mut m, start);
    let start_tick = m.now();
    assert_eq!([word(&m, RAM), word(&m, RAM+4), word(&m, RAM+8), word(&m, RAM+12), word(&m, RAM+16)],
        [0, 64, 1 << 9, 1, 0x0200_9070], "clocked reset restores RAW, RXEOF, fractional divider, TX_IDLE and DATE");
    m.mark();
    let marks = PcSet::default();
    let mut until = Until { tick: start_tick + TICK_HZ / 32_000, marks: &marks, stops: &[], stops_after: 0, on_drive: false };
    assert_eq!(m.run(&until), Ran::Tick, "firmware can be observed halfway through the first frame");
    assert_eq!(m.board().frames.len(), 1, "the board sees the frame at its first edge");
    assert_eq!(word(&m, RX_BUF), 0xa5a5_a5a5, "an incomplete frame has not reached GDMA IN");
    until.tick = start_tick + 5 * TICK_HZ / 16_000;
    assert_eq!(m.run(&until), Ran::Halted, "firmware reaches its explicit unsupported-mode write");
    let Status::Halted(halt) = m.status() else { panic!("PDM refusal is inspectable"); };
    assert_eq!((halt.pc, halt.kind, halt.what), (refused, HaltKind::Unmodeled, "I2S PDM mode is unsupported"), "PDM stops at its control write with the reason");
    assert_eq!(m.board().frames.len(), 3, "both directions share exactly one board call per WS period");
    for (i, f) in m.board().frames.iter().enumerate() {
        // M: the controller's first BCK edge is one BCK after START, rounded toward a later tick.
        assert_eq!(f.t, start_tick + TICK_HZ.div_ceil(512_000) + i as u64 * 30_000, "first edge and exact 16 kHz frame spacing");
        assert_eq!((f.mclk, f.bclk, f.ws, f.dout, f.din), (Some(4), 5, 6, Some(7), Some(22)), "matrix routes use board pad indices");
        assert_eq!((f.mclk_hz, f.bclk_hz), (Some((4_096_000, 1)), (512_000, 1)), "MCLK is 256 times fs and BCK is 32 times fs");
        assert_eq!((f.standard, f.slot_bits, f.data_bits), (I2sStandard::Philips, 16, 16), "Philips alignment and both widths reach the board");
        assert_eq!(f.tx, TX[i].map(i32::from), "GDMA OUT preserves channel order, sign and byte order across descriptors");
    }
    let mut received = [0; 16];
    assert!(m.read_data(RX_BUF, &mut received), "GDMA IN buffer is readable");
    assert_eq!(&received[..12], samples(RX).as_slice(), "GDMA IN writes each signed board answer in slot and frame order");
    assert_eq!(received[12..], [0xa5; 4], "early RX EOF leaves unused capacity untouched");
    assert_eq!((word(&m, RX_DESC), word(&m, RX_DESC + 12)), (0x8008, 0x4000_4008), "IN writes actual lengths, returns ownership and marks only successful EOF");
    assert_eq!((word(&m, TX_DESC), word(&m, TX_DESC + 12)), (0x4004, 0x4000_8008), "OUT writeback returns both descriptors to the CPU");
    assert_eq!((word(&m, RAM + 20), word(&m, RAM + 24), word(&m, RAM + 28)), (3, RX_DESC + 12, TX_DESC + 12), "I2S DONE and GDMA EOF addresses survive automatic stop");
    assert_eq!((word(&m, RAM + 32) & 63, word(&m, RAM + 36) & 15), (3, 11), "IN DONE/SUC_EOF and OUT DONE/EOF/TOTAL_EOF latch at completion");
    let frames = m.board().frames.clone();
    m.rewind();
    assert_eq!(word(&m, RX_BUF), 0xa5a5_a5a5, "rewind restores the DMA IN buffer");
    assert_eq!(word(&m, RX_DESC), 0x8000_0008, "rewind restores GDMA descriptor ownership and length");
    assert!(m.board().frames.is_empty(), "rewind restores board state before the first frame");
    assert_eq!(m.run(&until), Ran::Halted, "rewound channel and controller state can replay the firmware");
    assert_eq!(m.board().frames, frames, "replay preserves every frame timestamp, clock, route and sample");
}
