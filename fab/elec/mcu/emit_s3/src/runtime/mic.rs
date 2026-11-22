use super::{bits, cc, ccz, enc, Code, Link, Reg, SramLayout, BUFFER_ROW_SHIFT, BUF_COUNT, BUF_DST, BUF_N, CCOUNT, EVENT,
    EXCSAVE1, GDMA, GPIO, INTENABLE, IO_MUX, MIC_IRQ, MIC_SERVICE, PORT, RADIO_IRQ};
use crate::{radio, MicSram, CPU_HZ};
use fab_elec_mcu_rt::{ports, watch};

const I2S: u32 = 0x6000_f000;
// GDMA IN2, independent of panel OUT0 and speaker OUT1 (TRM 11687-11710).
const IN2: u32 = GDMA + 2 * 0xc0;
const EOF: u32 = 1 << 1;
pub(super) const LINE_BIT: u32 = 1 << 2;
const BLOCK_TICKS: u32 = 20_000 * 16;
const TIMER_HZ: u32 = 16_000_000;
pub(super) const CONFIGURED: u32 = 1;
const SYNC: u32 = 2;
pub(super) const PREPARED: u32 = 3;
const RUNNING: u32 = 4;

fn rx_conf(duplex: bool) -> u32 {
    // DERIVED: with TX master, SIG_LOOPBACK shares WS/BCK (TRM 38649-38650)
    // and the RX slave waits for master BCK (§28.8.2, 37575-37582). NOT FOUND: an
    // explicit statement that loopback requires RX slave mode.
    (1 << 19) | (1 << 15) | (1 << 12) | if duplex { 1 << 3 } else { 0 }
}

pub(super) fn configure(c: &mut Code<'_>, m: &MicSram, duplex: bool) {
    let a = &m.audio;
    if !duplex {
        bits(c, 0x600c_0020, 0, 1 << 4); bits(c, 0x600c_0018, 0, 1 << 4);
        bits(c, 0x600c_0020, 1 << 4, 0); c.set(I2S + 0x34, 1 << 29);
        for (pad, sig) in [(a.mclk, 23), (a.sclk, 26), (a.lrck, 27)] {
            c.set(GPIO + 0x74 + pad * 4, 0); c.set(IO_MUX + 4 + pad * 4, (1 << 12) | (1 << 9));
            c.set(GPIO + 0x554 + pad * 4, sig);
        }
    }
    // facts-audio clock table, TRM 37517-37538, 38345-38370. Keep the RX
    // module clock at 8 x BCK in slave mode. Only RX master drives the pads.
    c.set(I2S + 0x30, (u32::from(!duplex) << 29) | (2 << 27) | (1 << 26) | m.clock.n);
    c.set(I2S + 0x38, m.clock.fraction);
    let din = a.asdout.expect("verified mic pad");
    // ASDOUT is driven by the codec. Disable the pad's GPIO output and
    // select that enable before routing its input (TRM 15868-15888, 15930-15954).
    c.set(GPIO + 0x28 + u32::from(din >= 32) * 12, 1 << (din & 31));
    c.set(GPIO + 0x554 + din * 4, 256 | (1 << 10));
    c.set(GPIO + 0x74 + din * 4, 0); c.set(IO_MUX + 4 + din * 4, (1 << 12) | (1 << 9));
    c.set(GPIO + 0x154 + 4 * 25, (1 << 7) | din); // TRM 15930-15954, 16185-16187
    c.set(I2S + 0x28, (1 << 29) | (15 << 24) | (16 << 18) | (15 << 13) | (7 << 7) | 15);
    c.set(I2S + 0x50, (1 << 16) | 3);
    c.set(I2S + 0x64, 2 * m.frames - 1); // channel words minus one, ring-facts §2, TRM 38519-38529
}

fn count(c: &mut Code<'_>, reg: Reg) { c.a.op24(enc::rsr(0, 3, 0, CCOUNT, reg)); }

fn arm_ring(c: &mut Code<'_>, m: &MicSram) {
    // DW0 owner=1, suc_eof=0, length=0, capacity in bytes. IN hardware
    // writes back actual length, owner=0 and suc_eof (TRM 11414-11455).
    let bytes = m.frames * 4;
    for i in 0..MicSram::DESCRIPTORS {
        let desc = m.descriptors + 12 * i;
        c.set(desc, (1 << 31) | bytes); c.set(desc + 4, m.buffer + i * bytes);
        c.set(desc + 8, m.descriptors + 12 * ((i + 1) % MicSram::DESCRIPTORS));
    }
    c.a.memw(); c.set(IN2, 1); c.set(IN2, 0); c.set(IN2 + 4, 12); // watermark, CHECK_OWNER off
    c.set(IN2 + 0x48, 3); c.set(IN2 + 0x14, 0x3f);
    c.set(0x600c_2000 + 68 * 4, 2); // DMA_IN_CH2 level, TRM 18545-18550
    c.set(IN2 + 0x10, EOF); c.set(IN2 + 0x20, (m.descriptors & 0xfffff) | (1 << 22));
}

fn fault(c: &mut Code<'_>, m: &MicSram, code: u8) {
    c.imm(2, u32::from(watch::FAULT)); c.imm(3, u32::from(code));
    c.imm(4, m.entry as u32); c.call(EVENT);
}

pub(super) fn service(c: &mut Code<'_>, l: &SramLayout, m: &MicSram) {
    c.routine(MIC_SERVICE, "mic_service"); c.store(0, m.call_return);
    let done = c.label(); let sync = c.label(); let prepared = c.label(); let running = c.label();
    let timeout = c.label(); let resumed = c.label();
    let duplex = l.speaker.is_some();
    let rx = rx_conf(duplex);
    c.load(2, m.phase); c.zero(ccz::EQ, 2, done);
    c.imm(3, RUNNING); c.branch(cc::EQ, 2, 3, running);
    c.imm(3, PREPARED); c.branch(cc::EQ, 2, 3, prepared);
    c.imm(3, SYNC); c.branch(cc::EQ, 2, 3, sync);
    if duplex {
        // The speaker owns the bounded TX_UPDATE wait and its fault.
        c.load(3, I2S + 0x24); c.a.extui(3, 3, 8, 1); c.zero(ccz::NE, 3, done);
    }
    count(c, 2); c.store(2, m.progress); c.set(I2S + 0x20, rx | (1 << 8)); c.set(m.phase, SYNC);
    c.bind(sync);
    // RX_UPDATE crosses the APB clock domain (TRM 38271-38272). Allow four
    // periods of this MCLK. NOT FOUND: a silicon maximum synchronization latency.
    c.load(2, I2S + 0x20); c.a.extui(2, 2, 8, 1); c.zero(ccz::EQ, 2, resumed);
    count(c, 2); c.load(3, m.progress); c.a.sub(2, 2, 3);
    c.imm(3, (4 * CPU_HZ).div_ceil(m.clock.hz * 256)); c.branch(cc::LTU, 2, 3, sync);
    c.imm(5, 1); c.jump(timeout);
    c.bind(resumed); c.set(m.stalled, 0); c.set(I2S + 0x20, rx | 3); c.set(I2S + 0x20, rx);
    arm_ring(c, m); c.set(m.phase, PREPARED);
    c.bind(prepared);
    if duplex { c.load(2, I2S + 0x24); c.a.extui(2, 2, 2, 1); c.zero(ccz::EQ, 2, done); }
    count(c, 2); c.store(2, m.progress); c.set(m.phase, RUNNING); c.set(I2S + 0x20, rx | 4); c.jump(done);

    // One block plus both FIFOs at the wire rate. TRM 37363: 64 32-bit
    // RX FIFO words. GDMA 11497-11499: 24+128+16 bytes (42 stereo frames).
    // NOT FOUND: a worst-case DMA bus arbitration delay.
    let budget = (u64::from(m.frames + 64 + 42) * u64::from(CPU_HZ)).div_ceil(u64::from(m.clock.hz));
    c.bind(running);
    // Serialize the loss snapshot and the check of the last EOF against the ISR.
    c.a.op24(enc::rrr(0, 0, 0, 6, 15, 8));
    c.load(5, m.lost); let check = c.label(); c.zero(ccz::EQ, 5, check);
    c.set(m.lost, 0); c.set(m.reported, 1); c.a.wsr(8, 230); c.a.rsync();
    fault(c, m, watch::FAULT_MIC_LOST); c.jump(done);
    c.bind(check); count(c, 2); c.load(3, m.progress); c.a.sub(2, 2, 3); c.imm(3, budget as u32);
    let healthy = c.label(); c.branch(cc::LTU, 2, 3, healthy);
    c.load(2, m.stalled); c.zero(ccz::NE, 2, healthy);
    c.set(m.stalled, 1); c.a.wsr(8, 230); c.a.rsync(); c.imm(5, 2); fault(c, m, watch::FAULT_MIC_STALL); c.jump(done);
    c.bind(healthy); c.a.wsr(8, 230); c.a.rsync(); c.jump(done);
    c.bind(timeout); c.load(2, m.stalled); c.zero(ccz::NE, 2, done);
    c.set(m.stalled, 1); fault(c, m, watch::FAULT_MIC_STALL);
    c.bind(done); c.load(0, m.call_return); c.a.ret_n();
}

// An EOF wakes the CPU directly. TARGET0 still checks a stopped receiver and
// covers an event published between the dispatch scan and the masked sleep.
pub(super) fn arm(c: &mut Code<'_>, l: &SramLayout, m: &MicSram) {
    let done = c.label(); let compare = c.label(); let choose = c.label();
    c.load(2, m.phase); c.zero(ccz::EQ, 2, done);
    c.load(2, l.stamp); c.load(3, l.stamp + 4); c.load(4, m.received); c.load(5, m.seen);
    c.branch(cc::NE, 4, 5, compare); c.imm(4, BLOCK_TICKS); c.imm(5, 0); c.add64(2, 3, 4, 5, 6, 7);
    c.bind(compare); c.branch(cc::LTU, 3, 15, choose); c.branch(cc::NE, 3, 15, done); c.branch(cc::GEU, 2, 14, done);
    c.bind(choose); c.a.mov_n(14, 2); c.a.mov_n(15, 3); c.bind(done);
}

pub(super) fn irq(c: &mut Code<'_>, l: &SramLayout, m: &MicSram) {
    c.routine(MIC_IRQ, "mic_interrupt");
    // Both level-1 vectors arrive with original a0 in EXCSAVE1. Line 2 is
    // dedicated to IN2 in this image; USB and TARGET0 share line 1.
    let other = c.label(); let capture = c.label(); let registers = c.label(); let restore = c.label();
    c.a.op24(enc::rsr(0, 3, 0, INTENABLE, 0)); c.a.extui(0, 0, 2, 1); c.zero(ccz::EQ, 0, other);
    c.a.op24(enc::rsr(0, 3, 0, 226, 0)); c.a.extui(0, 0, 2, 1); c.zero(ccz::NE, 0, capture);
    c.bind(other);
    if c.radio {
        c.a.op24(enc::rsr(0, 3, 0, 230, 0)); c.a.extui(0, 0, 5, 1); c.zero(ccz::NE, 0, RADIO_IRQ);
    }
    c.a.op24(enc::rsr(0, 3, 0, INTENABLE, 0)); c.a.extui(0, 0, 2, 1); c.a.slli(0, 0, 2);
    c.a.wsr(0, INTENABLE); c.a.rsync(); c.jump(restore);
    c.bind(capture); c.address(0, m.irq_save);
    for reg in 2..=11 { c.a.s32i(reg, 0, 4 * u32::from(reg - 2)); }
    c.set(IN2 + 0x14, EOF); c.a.memw();
    c.load(0, IN2 + 0x28); // last completed address, TRM 12833-12839
    // An EOF after the clear and before this snapshot is included below.
    // Its latched interrupt can enter again with the same, now returned header.
    c.a.l32i(2, 0, 0); c.a.extui(2, 2, 30, 1); c.zero(ccz::EQ, 2, registers);
    c.address(2, m.descriptors); c.a.sub(0, 0, 2); c.imm(2, 12); c.a.quou(0, 0, 2);
    count(c, 4); c.store(4, m.progress); c.set(m.stalled, 0);
    // Convert elapsed CPU cycles from the latest stamp to its SYSTIMER grid.
    // Keep the fractional tick across IRQs even during a long application handler.
    c.load(5, m.cycle_ref); c.a.sub(4, 4, 5); c.imm(6, CPU_HZ / TIMER_HZ); c.a.quou(4, 4, 6);
    c.a.mull(6, 4, 6); c.a.add_n(5, 5, 6); c.store(5, m.cycle_ref);
    c.load(2, m.edge); c.load(3, m.edge + 4); c.imm(5, 0); c.add64(2, 3, 4, 5, 6, 7);
    c.store(2, m.edge); c.store(3, m.edge + 4);
    let block = c.label(); let finish = c.label(); c.bind(block);
    c.load(8, m.next); c.a.addx2(2, 8, 8); c.address(7, m.descriptors); c.a.addx4(7, 2, 7);
    c.a.l32i(3, 7, 0); c.a.extui(3, 3, 30, 1); c.zero(ccz::EQ, 3, finish);
    c.a.l32i(2, 7, 4); c.load(6, m.write); c.address(3, m.history); c.a.addx2(3, 6, 3);
    c.imm(4, m.frames); let sample = c.label(); let fed = c.label(); c.feed(); c.bind(sample);
    // ADCDAT_SEL=0 supplies the ADC in both slots (facts-audio, Recording
    // the analog microphone). Copy the first signed I16 bit-for-bit.
    c.a.l16ui(5, 2, 0); c.a.s16i(5, 3, 0); c.a.addi(2, 2, 4); c.a.addi(3, 3, 2);
    c.a.addi(4, 4, -1); c.a.extui(5, 4, 0, 6); c.zero(ccz::NE, 5, fed); c.feed(); c.bind(fed);
    c.zero(ccz::NE, 4, sample);
    c.imm(2, (1 << 31) | m.frames * 4); c.a.memw(); c.a.s32i(2, 7, 0); c.a.memw();
    c.imm(2, m.frames); c.imm(3, m.capacity);
    c.a.add_n(6, 6, 2); c.a.remu(6, 6, 3); c.store(6, m.write);
    c.load(4, m.held); c.a.add_n(4, 4, 2); c.a.minu(4, 4, 3); c.store(4, m.held);
    c.load(4, m.ready); c.a.add_n(4, 4, 2); c.a.minu(5, 4, 3); c.store(5, m.ready);
    let publish = c.label(); c.a.sub(4, 4, 5); c.zero(ccz::EQ, 4, publish);
    c.load(5, m.taking); c.zero(ccz::EQ, 5, publish); c.load(5, m.reported); c.zero(ccz::NE, 5, publish);
    c.load(5, m.lost); c.a.add_n(5, 5, 4); c.imm(6, i32::MAX as u32); c.a.minu(5, 5, 6); c.store(5, m.lost);
    c.bind(publish); c.load(2, m.received); c.load(3, m.seen);
    let advance = c.label(); c.branch(cc::NE, 2, 3, advance);
    c.a.addi(2, 2, 1); c.store(2, m.received);
    // Date the oldest newly pending block, even if several descriptors retired.
    c.a.sub(4, 0, 8); c.a.extui(4, 4, 0, MicSram::DESCRIPTORS.ilog2() as u8);
    c.imm(5, BLOCK_TICKS); c.a.mull(4, 4, 5); c.load(2, m.edge); c.load(3, m.edge + 4);
    let borrow = c.label(); c.branch(cc::GEU, 2, 4, borrow); c.a.addi(3, 3, -1); c.bind(borrow);
    c.a.sub(2, 2, 4); c.store(2, m.due); c.store(3, m.due + 4);
    c.bind(advance); c.a.addi(2, 8, 1); c.a.extui(2, 2, 0, MicSram::DESCRIPTORS.ilog2() as u8); c.store(2, m.next);
    c.branch(cc::NE, 8, 0, block);
    c.bind(finish);
    if let Some(r) = &l.radio {
        // A radio dispatcher sleeps as a blocked task. Wake it just as a
        // dispatcher interrupt does. Preemption stays with the normal scheduler.
        let ready = c.label();
        c.load(2, r.dispatcher + radio::TCB_STATE); c.imm(3, radio::BLOCKED); c.branch(cc::NE, 2, 3, ready);
        c.set(r.dispatcher + radio::TCB_STATE, radio::READY); c.imm(2, 1 << radio::YIELD_LINE);
        c.a.wsr(2, 226); c.a.rsync(); c.bind(ready);
    }
    c.bind(registers); c.address(0, m.irq_save);
    for reg in 2..=11 { c.a.l32i(reg, 0, 4 * u32::from(reg - 2)); }
    c.bind(restore); c.a.op24(enc::rsr(0, 3, 0, EXCSAVE1, 0)); c.a.op24(enc::rfe());
}

fn record(c: &mut Code<'_>, l: &SramLayout, m: &MicSram, link: &Link, bad: usize, done: usize) {
    let b = l.buffer_scratch;
    // The loader has proved the handle is a writable I16 buffer. Unsigned
    // comparisons reject negative offsets/counts without adding off+n.
    c.shl(2, 2, BUFFER_ROW_SHIFT); c.address(5, link.buffers); c.a.add_n(2, 2, 5);
    c.a.l32i(5, 2, 4); c.branch(cc::LTU, 5, 3, bad); c.a.sub(5, 5, 3); c.branch(cc::LTU, 5, 4, bad);
    c.a.l32i(2, 2, 0); c.a.addx2(2, 3, 2); c.store(2, b + BUF_DST);
    c.load(5, m.ready); c.a.minu(4, 4, 5); c.store(4, b + BUF_N); c.set(b + BUF_COUNT, 0);
    let finished = c.label(); c.zero(ccz::EQ, 4, finished);
    let batch = c.label(); c.bind(batch); c.feed(); c.a.op24(enc::rrr(0, 0, 0, 6, 15, 8));
    // Cursor = write - unread modulo capacity. Limit each masked copy to
    // 64 samples and split at the end of the ring, including odd I16 offsets.
    c.load(2, m.write); c.imm(3, m.capacity); c.a.add_n(2, 2, 3); c.load(4, m.ready);
    c.a.sub(2, 2, 4); c.a.remu(2, 2, 3); c.a.sub(3, 3, 2); c.address(5, m.history); c.a.addx2(2, 2, 5);
    c.imm(5, 64); c.a.minu(3, 3, 5); c.load(6, b + BUF_N); c.a.minu(5, 3, 6);
    c.a.sub(6, 6, 5); c.store(6, b + BUF_N); c.a.sub(4, 4, 5); c.store(4, m.ready);
    let taking = c.label(); c.zero(ccz::NE, 4, taking); c.set(m.taking, 0); c.bind(taking);
    c.set(m.reported, 0); c.load(4, b + BUF_COUNT); c.a.add_n(4, 4, 5); c.store(4, b + BUF_COUNT);
    c.load(3, b + BUF_DST); let sample = c.label(); c.bind(sample);
    c.a.l16ui(4, 2, 0); c.a.s16i(4, 3, 0); c.a.addi(2, 2, 2); c.a.addi(3, 3, 2);
    c.a.addi(5, 5, -1); c.zero(ccz::NE, 5, sample); c.store(3, b + BUF_DST);
    c.a.wsr(8, 230); c.a.rsync(); c.zero(ccz::NE, 6, batch);
    c.bind(finished); c.load(2, b + BUF_COUNT); c.jump(done);
}

pub(super) fn port(c: &mut Code<'_>, l: &SramLayout, m: &MicSram, link: &Link, port: u16) {
    c.routine(PORT + usize::from(port), ports::PORTS[usize::from(port)].name);
    let zero = c.label(); let bad = c.label(); let done = c.label();
    c.store(0, l.port_return); c.load(5, m.phase); c.zero(ccz::EQ, 5, zero);
    if port != ports::MIC_READY {
        c.store(2, m.port_args); c.store(10, m.port_args + 12);
        if port == ports::RECORD { c.store(3, m.port_args + 4); c.store(4, m.port_args + 8); }
    }
    c.call(MIC_SERVICE);
    match port {
        ports::RECORD => {
            c.load(2, m.port_args); c.load(3, m.port_args + 4); c.load(4, m.port_args + 8);
            record(c, l, m, link, bad, done);
        }
        ports::MIC_READY => { c.load(2, m.ready); c.jump(done); }
        ports::MIC_REWIND => {
            c.load(2, m.port_args); c.zero(ccz::LT, 2, bad);
            c.a.op24(enc::rrr(0, 0, 0, 6, 15, 8));
            c.load(3, m.held); c.load(4, m.ready); c.a.sub(3, 3, 4); c.a.minu(2, 2, 3);
            c.a.add_n(4, 4, 2); c.store(4, m.ready);
            let empty = c.label(); c.zero(ccz::EQ, 4, empty); c.set(m.taking, 1); c.bind(empty);
            c.a.wsr(8, 230); c.a.rsync(); c.jump(done);
        }
        _ => unreachable!("mic port"),
    }
    c.bind(bad);
    if port != ports::MIC_READY {
        c.load(5, m.port_args + if port == ports::RECORD { 8 } else { 0 }); c.load(4, m.port_args + 12);
        c.imm(2, u32::from(watch::FAULT)); c.imm(3, u32::from(watch::FAULT_RANGE)); c.call(EVENT);
    }
    c.bind(zero); c.imm(2, 0); c.bind(done); c.load(0, l.port_return); c.a.ret_n();
}
