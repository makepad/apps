use super::{arm, bits, boot_delay, cc, ccz, enc, i2c_list, i2c_transaction, Code, Link, Reg, SramLayout, Timing,
    BUFFER_ROW_SHIFT, BUF_COUNT, BUF_N, BUF_SRC, EVENT, GDMA, GPIO, I2C0, INTENABLE, IO_MUX, PORT,
    MIC_SERVICE, SPEAKER_CLOCK, SPEAKER_SERVICE, STAMP};
use crate::{image::SpeakerSram, wiring::AUDIO_CLOCKS, Audio};
use fab_elec_mcu_rt::{ports, watch};

// facts-audio.md, TRM lines 13407, 13450, 11687-11710. The panel owns OUT0.
const I2S0: u32 = 0x6000_f000;
const OUT1: u32 = GDMA + 0xc0 + 0x60;
const EOF: u32 = 1 << 1;
const PERIOD: u32 = 5_000 * 16;
const SAFE_PHASE_END: u32 = PERIOD - 1_000 * 16;
const SYNC: u32 = 1;
const FIRST: u32 = 2;
const RUNNING: u32 = 3;
const PREPARED: u32 = 4; // UPDATE and reset completed, codec setup still owns the bus
const TIMER_HZ: u32 = 16_000_000;

// TX_UPDATE crosses APB to the TX clock domain (TRM 37995-37997, 38619-38620).
// Allow four periods at the slowest supported MCLK. The twin uses one
// destination clock. NOT FOUND: a silicon maximum synchronizer delay.
fn sync_ticks() -> u32 {
    let slowest = AUDIO_CLOCKS.iter().map(|clock| clock.hz).min().expect("S3 audio clock table");
    (4 * TIMER_HZ).div_ceil(256 * slowest)
}

fn eof_budget(s: &SpeakerSram, periods: u32) -> u32 {
    // TRM 11497-11499: GDMA L1/L2/L3 hold 24+128+16 bytes, or 42 stereo
    // I16 frames. I2S TX holds 64 more 32-bit words (37357, 37594-37601).
    // Allow one traversal of both FIFOs at the wire rate in addition to
    // the descriptor(s). NOT FOUND: a worst-case GDMA bus arbitration time.
    let frames = (periods * s.clock.hz).div_ceil(200) + 42 + 64;
    (u64::from(frames) * u64::from(TIMER_HZ)).div_ceil(u64::from(s.clock.hz)) as u32
}

// From any anchor the Bresenham split can bring an edge forward by less
// than one frame. Keep the full copy guard even at that earliest edge.
fn grid_skew(s: &SpeakerSram) -> u32 {
    if s.clock.descriptor_cycle() == 1 { 0 } else { TIMER_HZ.div_ceil(s.clock.hz) }
}

// TX drives the shared clock pair, TDM, PCM bypass, stop on empty and left alignment
// (TRM lines 38560-38650). The ring always supplies another frame.
const TX_CONF: u32 = (1 << 27) | (1 << 19) | (1 << 15) | (1 << 13) | (1 << 12);

pub(super) fn amps_off(c: &mut Code<'_>, audio: &Audio) {
    for amp in audio.amp_enables() {
        let pin = amp.gpio;
        let bank = u32::from(pin >= 32) * 12;
        let mask = 1 << (pin & 31);
        // Set the inactive latch before selecting and enabling the GPIO output.
        c.set(GPIO + bank + if amp.enable_high { 0x0c } else { 0x08 }, mask);
        c.set(GPIO + 0x74 + pin * 4, 0);
        c.set(IO_MUX + 4 + pin * 4, (1 << 12) | (1 << 9));
        c.set(GPIO + 0x554 + pin * 4, 256 | (1 << 10));
        c.set(GPIO + bank + 0x24, mask);
    }
}

fn i2s_configure(c: &mut Code<'_>, s: &SpeakerSram) {
    // Clocked SYSTEM reset, then PLL_F160M to MCLK. RX_CLKM_CONF resets
    // with MCLK_SEL clear (TX drives MCLK). TRM lines 38345-38370, 38697-38721.
    bits(c, 0x600c_0020, 0, 1 << 4);
    bits(c, 0x600c_0018, 0, 1 << 4);
    bits(c, 0x600c_0020, 1 << 4, 0);
    c.set(I2S0 + 0x34, (1 << 29) | (2 << 27) | (1 << 26) | s.clock.n);
    c.set(I2S0 + 0x3c, s.clock.fraction);
    // TRM lines 15930-15954, 16185-16187, 16205-16207. The matrix uses
    // the peripheral's output enable for all four push-pull TX signals.
    let a = &s.audio;
    for (pin, signal) in [(a.mclk, 23), (a.sclk, 22), (a.lrck, 24), (a.dsdin, 25)] {
        c.set(GPIO + 0x74 + pin * 4, 0);
        c.set(IO_MUX + 4 + pin * 4, (1 << 12) | (1 << 9));
        c.set(GPIO + 0x554 + pin * 4, signal);
    }
    // Philips I16, two 16-bit slots, BCK = MCLK / 8. HALF_SAMPLE_BITS=16
    // follows the TRM and twin. NOT FOUND: silicon resolution of IDF's 15
    // (facts-audio.md, TRM lines 37692-37695, 38660-38691, 38826-38831).
    c.set(I2S0 + 0x2c, (1 << 30) | (1 << 29) | (15 << 24) | (16 << 18) | (15 << 13) | (7 << 7) | 15);
    c.set(I2S0 + 0x54, (1 << 16) | 3);
    c.set(s.phase, SYNC); c.call(SPEAKER_CLOCK);
    c.store(2, s.last_eof); c.store(3, s.last_eof + 4); c.set(I2S0 + 0x24, TX_CONF | (1 << 8));
    let poll = c.label(); let ready = c.label(); let done = c.label(); c.bind(poll);
    c.call(SPEAKER_CLOCK); c.load(2, I2S0 + 0x24); c.a.extui(2, 2, 8, 1); c.zero(ccz::EQ, 2, ready);
    check_wait(c, s, s.last_eof, sync_ticks(), poll, done);
    c.bind(ready); reset_tx(c); c.set(s.phase, PREPARED); c.bind(done);
}

fn reset_tx(c: &mut Code<'_>) {
    // Reset the unit and FIFO only after UPDATE cleared (TRM 37995-37997).
    c.set(I2S0 + 0x24, TX_CONF | 3); c.set(I2S0 + 0x24, TX_CONF);
}

fn start_ring(c: &mut Code<'_>, s: &SpeakerSram) {
    // ring-facts.md §§1-3, TRM lines 11414-11453, 11668-11672, 12292-12317.
    // Init has cleared every stereo frame. DW0 has suc_eof, the playable
    // byte length and the padded capacity. Only OUTLINK truncates the address.
    // The TRM describes the successor pointer. NOT FOUND: an explicit lap guarantee.
    let slot_bytes = s.slot_frames * 4;
    let header = |frames: u32| (1 << 30) | ((frames * 4) << 12) | slot_bytes;
    let cycle = s.clock.descriptor_cycle();
    let first = header(s.clock.hz / 200);
    c.feed(); c.address(2, s.descriptors); c.address(3, s.buffer);
    c.imm(4, s.count / cycle); c.imm(5, slot_bytes); c.imm(6, first);
    if cycle > 1 { c.imm(8, header(s.slot_frames)); }
    let next = c.label(); c.bind(next);
    // Emit one cycle. Only its last descriptor takes the extra frame.
    for i in 0..cycle {
        let dw0 = if cycle > 1 && i + 1 == cycle { 8 } else { 6 };
        c.a.addi(7, 2, 12); c.a.s32i(dw0, 2, 0); c.a.s32i(3, 2, 4); c.a.s32i(7, 2, 8);
        c.a.mov_n(2, 7); c.a.add_n(3, 3, 5);
    }
    c.a.addi(4, 4, -1); c.zero(ccz::NE, 4, next);
    c.set(s.descriptors + (s.count - 1) * 12 + 8, s.descriptors);
    // Hand the first silent descriptor to GDMA. The first refill can write
    // the following CPU-owned payloads before they have made their first lap.
    c.set(s.descriptors, (1 << 31) | first);
    c.a.memw();
    // OUT_EOF_MODE at FIFO exit, AUTO_WRBACK hands the retired payload to
    // the CPU, CHECK_OWNER off so a missed handback cannot stop the clocks.
    let out_conf = (1 << 3) | (1 << 2);
    c.set(OUT1, out_conf | 1); c.set(OUT1, out_conf); c.set(OUT1 + 4, 0);
    c.set(OUT1 + 0x48, 3); // I2S0
    // GDMA source 72 is OUT1's masked level (TRM Table 9.3-1, facts-machine.md:186).
    // Clear the EOF latch before START.
    c.set(OUT1 + 0x10, 0); c.set(OUT1 + 0x14, 0x0f);
    c.set(0x600c_2120, 1); // CORE0_DMA_OUT_CH1_INT_MAP, shared level with TARGET0
    c.set(OUT1 + 0x20, (s.descriptors & 0x000f_ffff) | (1 << 21));
    c.call(SPEAKER_CLOCK); c.store(2, s.last_eof); c.store(3, s.last_eof + 4);
    c.set(I2S0 + 0x24, TX_CONF | 4);
    c.set(s.phase, FIRST); c.set(s.stalled, 0);
}

pub(super) fn audio_init(c: &mut Code<'_>, l: &SramLayout, t: &Timing) {
    let speaker = l.speaker.as_ref();
    let mic = l.mic.as_ref();
    let (audio, clock, scratch) = speaker.map(|s| (&s.audio, s.clock, s.due))
        .or_else(|| mic.map(|m| (&m.audio, m.clock, m.due))).expect("audio declaration");
    if l.panel.is_none() {
        // panel_init already releases this shared reset when a panel is declared.
        bits(c, 0x600c_0024, 0, 1 << 6);
        bits(c, 0x600c_001c, 0, 1 << 6);
        bits(c, 0x600c_0024, 1 << 6, 0);
    }
    if let Some(s) = speaker { i2s_configure(c, s); }
    if let Some(m) = mic { super::mic::configure(c, m, speaker.is_some()); }
    let write = c.label(); let settle = c.label(); let done = c.label();
    let codec = |c: &mut Code<'_>, reg: u8, value: u8| {
        c.imm(13, u32::from(reg)); c.imm(14, u32::from(value)); c.call(write);
    };
    // The codec can retain its registers across an S3 reset. Pulse INI_REG
    // (ES8311 datasheet lines 1399-1404), then use manual slave power-up
    // (ring-facts.md §4, ES8311 user guide lines 1324-1398).
    codec(c, 0xfa, 1); codec(c, 0xfa, 0);
    codec(c, 0x00, 0x3f); c.call(settle); codec(c, 0x00, 0x24);
    // facts-audio.md, Recording the analog microphone, and ring-facts §4.
    // Charge VMID before selecting normal operation. Power only the paths in use.
    let (gates, charge) = match (speaker.is_some(), mic.is_some()) {
        (true, true) => (0x3f, 0x07), (false, true) => (0x3a, 0x0f), _ => (0x35, 0x37),
    };
    if speaker.is_some() { codec(c, 0x31, 0x60); codec(c, 0x32, 0xbf); }
    codec(c, 0x01, gates); codec(c, 0x02, clock.codec_pre); codec(c, 0x05, 0);
    if speaker.is_some() { codec(c, 0x04, 16); codec(c, 0x09, 0x0c); }
    codec(c, 0x0d, charge);
    if speaker.is_some() { codec(c, 0x12, 1); }
    if mic.is_some() {
        // facts-audio.md rows 77-82: field-only PGA/modulator enable,
        // 0 dB PGA and digital volume, table-based ADC scale, ADC on both slots.
        for (reg, value) in [(0x03, 16), (0x0a, 0x0c), (0x0e, 0x08), (0x14, 0x10),
            (0x16, 0x04), (0x17, 0xbf), (0x44, 0)] { codec(c, reg, value); }
    }
    c.call(settle); codec(c, 0x0d, charge - 1);
    codec(c, 0x00, if speaker.is_some() { 0xa4 } else { 0xa5 });
    // A failed codec write jumps to done before publishing availability.
    if let Some(m) = mic { c.set(m.phase, super::mic::CONFIGURED); c.call(MIC_SERVICE); }
    if let Some(s) = speaker {
        c.set(s.available, 1);
        let pump = c.label(); c.bind(pump); c.call(SPEAKER_SERVICE);
        c.load(2, s.stalled); c.zero(ccz::NE, 2, done);
        c.load(2, s.phase); c.imm(3, FIRST);
        let wait = c.label(); c.branch(cc::EQ, 2, 3, wait);
        // Boot was waiting on EOF itself with the dispatcher still masked.
        // A first observation after boot instead brackets a later edge.
        c.set(s.uncertain, 0); c.jump(done); c.bind(wait);
        // EOF and TARGET0 share level line 1. Even if GDMA never requests an
        // interrupt, the alarm wakes the boot wait at its absolute deadline.
        c.a.op24(enc::rrr(0, 0, 0, 6, 15, 2));
        deadline(c, s.last_eof, eof_budget(s, 1)); arm(c, 2, 3, 4, 5, 6, 7);
        c.set(OUT1 + 0x10, EOF); c.imm(2, 1 << 1); c.a.wsr(2, INTENABLE); c.a.rsync();
        c.a.waiti(0); c.a.op24(enc::rrr(0, 0, 0, 6, 15, 2)); c.jump(pump);
    } else { c.jump(done); }

    // One copy of the shared-bus transfer for all codec writes. a12 saves
    // CALL0's return across STAMP, a13/a14 carry register and value.
    c.bind(write); c.a.mov_n(12, 0);
    let addr = audio.addr;
    i2c_transaction(c, l.i2c_pending.expect("audio I2C owner"), addr, t, done, STAMP, |c| c.a.mov_n(5, 13), |c| {
        i2c_list(c, &[addr << 1], &[6 << 11, (1 << 11) | (1 << 8) | 3, 2 << 11]);
        c.store(13, I2C0 + 0x1c); c.store(14, I2C0 + 0x1c);
        (3, 1)
    });
    c.a.mov_n(0, 12); c.a.ret_n();

    // The guide gives "several ms" in reset. NOT FOUND: a manual VMID
    // settling time. Use 5 ms for each wait. The refill deadline is scratch
    // until START publishes the first refill. Boot waits keep a12.
    c.bind(settle); c.a.mov_n(12, 0); boot_delay(c, l, scratch, 5_000);
    c.a.mov_n(0, 12); c.a.ret_n();
    c.bind(done);
}

// Line 1 already wakes both WAITI and the radio dispatcher. The matrix ORs
// level sources (facts-machine.md §2). a14/a15 hold sleep's earliest deadline.
pub(super) fn arm_eof(c: &mut Code<'_>, s: &SpeakerSram) {
    let done = c.label(); let retry = c.label(); let probe = c.label(); let first = c.label();
    let compare = c.label(); let choose = c.label(); let running = c.label();
    c.load(2, s.available); c.zero(ccz::EQ, 2, done);
    c.feed(); c.call(SPEAKER_CLOCK);
    c.load(2, s.phase); c.imm(3, SYNC); c.branch(cc::EQ, 2, 3, retry);
    if c.mic { c.imm(3, PREPARED); c.branch(cc::EQ, 2, 3, retry); } // waiting for RX_UPDATE
    c.imm(3, FIRST); c.branch(cc::EQ, 2, 3, first);
    c.load(2, s.stalled); c.zero(ccz::NE, 2, retry);
    c.load(2, s.uncertain); c.load(3, s.raw_seen); c.a.or(2, 2, 3); c.zero(ccz::NE, 2, probe);
    c.load(2, s.dirty); c.load(3, s.confirm); c.a.or(2, 2, 3);
    c.load(3, s.retry_refill); c.a.or(2, 2, 3); c.zero(ccz::EQ, 2, running);
    c.set(OUT1 + 0x10, EOF);
    c.bind(running); deadline(c, s.progress_at, eof_budget(s, s.count)); c.jump(compare);
    c.bind(first); c.set(OUT1 + 0x10, EOF); c.load(2, s.stalled); c.zero(ccz::NE, 2, retry);
    deadline(c, s.last_eof, eof_budget(s, 1)); c.jump(compare);
    // A stalled device gets another observation next block. A sticky RAW
    // or a late anchor needs a pair of closer observations to locate an edge.
    c.bind(retry); deadline(c, s.now, PERIOD); c.jump(compare);
    c.bind(probe); deadline(c, s.now, TIMER_HZ.div_ceil(s.clock.hz));
    c.bind(compare);
    c.branch(cc::LTU, 3, 15, choose); c.branch(cc::NE, 3, 15, done); c.branch(cc::GEU, 2, 14, done);
    c.bind(choose); c.a.mov_n(14, 2); c.a.mov_n(15, 3); c.bind(done);
}

fn mask(c: &mut Code<'_>, s: &SpeakerSram) {
    if c.radio || c.mic { c.a.op24(enc::rrr(0, 0, 0, 6, 15, 11)); c.address(10, s.saved + 4); c.a.s32i(11, 10, 0); }
}

fn unmask(c: &mut Code<'_>, s: &SpeakerSram) {
    if c.radio || c.mic { c.load(10, s.saved + 4); c.a.wsr(10, 230); c.a.rsync(); }
}

// OUT_EOF_DES_ADDR names the descriptor just retired (TRM lines 12962-12968).
// Return its successor's index in a8. Read only after observing a real EOF.
fn dma_head(c: &mut Code<'_>, s: &SpeakerSram) {
    c.load(8, OUT1 + 0x28); c.address(7, s.descriptors); c.a.sub(8, 8, 7);
    c.imm(7, 12); c.a.quou(8, 8, 7); c.a.addi(8, 8, 1);
    c.imm(7, s.count); c.a.remu(8, 8, 7);
}

// Signed tick difference in a6 (low), a4 (high).
fn elapsed(c: &mut Code<'_>, s: &SpeakerSram, since: u32) {
    c.load(6, s.now); c.load(7, since); c.load(4, s.now + 4); c.load(5, since + 4);
    let no_borrow = c.label(); c.branch(cc::GEU, 6, 7, no_borrow); c.a.addi(4, 4, -1);
    c.bind(no_borrow); c.a.sub(6, 6, 7); c.a.sub(4, 4, 5);
}

fn deadline(c: &mut Code<'_>, since: u32, ticks: u32) {
    c.load(2, since); c.load(3, since + 4); c.imm(4, ticks); c.imm(5, 0); c.add64(2, 3, 4, 5, 6, 7);
}

// a13 is the observed head. Raw is a latch and the saved address repeats
// every lap. Select the count congruent to the address change nearest the
// elapsed 5 ms periods. One boundary of timestamp/FIFO skew is unambiguous
// with at least four descriptors (ring-facts.md §§1-2). Every lap contains
// whole cycles, so rounding individual descriptors never accumulates here.
fn finished(c: &mut Code<'_>, s: &SpeakerSram) {
    elapsed(c, s, s.last_eof); c.a.mov_n(2, 6); c.a.mov_n(3, 4);
    let short = c.label(); let estimate = c.label();
    c.imm(12, 0); c.zero(ccz::LT, 3, estimate); // DMA boundary slightly ahead of its nominal tick.
    c.imm(4, PERIOD); c.zero(ccz::EQ, 3, short);
    // Divide the 64-bit difference in three limbs. Each remainder times
    // 4096 fits a word. Counts beyond the watch argument saturate.
    c.imm(12, i32::MAX as u32 + s.count); c.imm(5, PERIOD / 2 + 1); c.branch(cc::GEU, 3, 5, estimate);
    c.imm(12, 0);
    for (shift, width) in [(24, 8), (12, 12), (0, 12)] {
        c.a.extui(5, 2, shift, width); c.a.slli(3, 3, width); c.a.or(3, 3, 5);
        c.a.quou(5, 3, 4); c.a.remu(3, 3, 4);
        c.a.slli(12, 12, width); c.a.or(12, 12, 5);
    }
    c.jump(estimate); c.bind(short); c.a.quou(12, 2, 4); c.bind(estimate);
    c.load(2, s.head); c.imm(3, s.count); c.a.add_n(4, 13, 3); c.a.sub(4, 4, 2); c.a.remu(4, 4, 3);
    c.a.addi(12, 12, (s.count / 2) as i32); c.a.maxu(12, 12, 4); c.a.sub(12, 12, 4);
    c.a.quou(12, 12, 3); c.a.mull(12, 12, 3); c.a.add_n(12, 12, 4);
}

// a4 descriptor periods after last_eof, returned as a two-word tick in a2/a3.
fn boundary(c: &mut Code<'_>, s: &SpeakerSram) {
    c.imm(6, PERIOD); c.a.muluh(5, 4, 6); c.a.mull(4, 4, 6);
    c.load(2, s.last_eof); c.load(3, s.last_eof + 4); c.add64(2, 3, 4, 5, 6, 7);
}

// A full tail drained with a refill still pending. Its silent successor
// was already in flight. Count the silence once that descriptor retires.
fn confirm_loss(c: &mut Code<'_>, s: &SpeakerSram) {
    let done = c.label(); let report = c.label(); let next = c.label();
    c.load(2, s.confirm); c.zero(ccz::EQ, 2, done); c.set(s.confirm, 0);
    c.a.mov_n(5, 12); c.load(2, s.queued); c.zero(ccz::EQ, 2, report);
    c.imm(3, s.count); c.a.minu(6, 12, 3); c.imm(5, 0); c.load(4, s.head);
    c.bind(next); c.address(2, s.lengths); c.a.addx4(2, 4, 2); c.a.l32i(2, 2, 0);
    c.imm(7, 0); c.branch(cc::LT, 7, 2, report); c.a.addi(5, 5, 1);
    c.a.addi(4, 4, 1); c.a.remu(4, 4, 3); c.branch(cc::LTU, 5, 6, next);
    c.bind(report); c.imm(2, i32::MAX as u32); c.a.minu(5, 5, 2); report_lost(c, s);
    c.bind(done);
}

// GDMA DW0[23:12] gives the valid byte length (TRM 11446-11451).
// Auto-writeback changes ownership. These word-aligned lengths stay fixed.
fn descriptor_frames(c: &mut Code<'_>, s: &SpeakerSram, index: Reg, dst: Reg, tmp: Reg) {
    if s.clock.descriptor_cycle() == 1 { c.imm(dst, s.slot_frames); return; }
    c.a.addx2(tmp, index, index); c.address(dst, s.descriptors); c.a.addx4(dst, tmp, dst);
    c.a.l32i(dst, dst, 0); c.a.extui(dst, dst, 14, 10);
}

fn retire(c: &mut Code<'_>, s: &SpeakerSram) {
    // a12 holds the full count. First account the queued prefix of each
    // descriptor once, even if the DMA has made many laps.
    confirm_loss(c, s);
    let advance = c.label(); c.load(2, s.queued); c.zero(ccz::EQ, 2, advance);
    c.imm(3, s.count); c.a.minu(15, 12, 3); c.load(14, s.head);
    let next = c.label(); let refill = c.label(); let skip = c.label(); c.bind(next);
    c.feed(); c.address(2, s.lengths); c.a.addx4(2, 14, 2); c.a.l32i(3, 2, 0);
    c.zero(ccz::LT, 3, skip); c.zero(ccz::EQ, 3, skip);
    c.load(4, s.queued); c.a.sub(4, 4, 3); c.store(4, s.queued); c.a.neg(5, 3); c.a.s32i(5, 2, 0);
    c.zero(ccz::NE, 4, refill);
    // Test at the drain, before this EOF can publish its own refill. Earlier
    // EOFs in this same service may have already raised an unanswered request.
    c.load(2, s.received); c.load(7, s.seen); c.branch(cc::EQ, 2, 7, refill);
    descriptor_frames(c, s, 14, 6, 7);
    // All descriptors after the last valid one were silent or stale. A
    // partly filled final descriptor also played silence in its suffix.
    c.imm(5, s.count); c.a.minu(5, 12, 5); c.a.sub(5, 12, 5); c.a.add_n(5, 5, 15); c.a.addi(5, 5, -1);
    let full = c.label(); c.branch(cc::EQ, 3, 6, full); c.a.addi(5, 5, 1); c.bind(full);
    let missed = c.label(); c.zero(ccz::NE, 5, missed);
    // An exactly full tail ending at this EOF has missed no complete
    // descriptor yet. Re-seat now and report when its silent successor retires.
    c.set(s.confirm, 1); c.jump(refill);
    c.bind(missed); c.imm(6, i32::MAX as u32); c.a.minu(5, 5, 6); c.store(5, s.lost);
    c.bind(refill); c.imm(3, s.capacity_frames() / 2); c.branch(cc::LTU, 3, 4, skip);
    c.load(2, s.received); c.load(3, s.seen); c.branch(cc::NE, 2, 3, skip);
    c.a.addi(2, 2, 1); c.store(2, s.received);
    c.imm(4, s.count); c.a.minu(4, 12, 4); c.a.sub(4, 4, 15); c.a.addi(4, 4, 1);
    boundary(c, s); c.store(2, s.due); c.store(3, s.due + 4);
    c.bind(skip); c.a.addi(14, 14, 1); c.imm(3, s.count); c.a.remu(14, 14, 3);
    c.a.addi(15, 15, -1); c.zero(ccz::NE, 15, next);
    c.bind(advance); c.store(13, s.head);
    c.imm(2, s.count); c.a.minu(2, 12, 2); c.imm(3, s.slot_frames); c.a.mull(2, 2, 3);
    c.load(4, s.write); c.a.sub(4, 4, 2); c.a.max(4, 4, 3); c.store(4, s.write);
}

// A batch touches at most one 5 ms payload. Leave the successor alone in
// the last millisecond of a period, or if the nominal edge is ahead of the
// stamp. This allows for FIFO phase and the bounded masked copy on silicon.
// NOT FOUND: a silicon bound for DMA-to-pin lag or worst-case store latency.
fn late(c: &mut Code<'_>, s: &SpeakerSram, label: usize) {
    c.load(4, s.uncertain); c.zero(ccz::NE, 4, label);
    elapsed(c, s, s.last_eof); c.zero(ccz::NE, 4, label);
    c.imm(7, SAFE_PHASE_END - grid_skew(s)); c.branch(cc::GEU, 6, 7, label);
}

fn clear_played(c: &mut Code<'_>, s: &SpeakerSram) {
    let done = c.label(); let next = c.label(); let skip = c.label(); let unlock = c.label(); let clear = c.label();
    c.load(2, s.dirty); c.zero(ccz::EQ, 2, done);
    c.a.mov_n(14, 13); c.imm(15, s.count); c.bind(next);
    c.a.addi(14, 14, 1); c.imm(3, s.count); c.a.remu(14, 14, 3);
    c.address(2, s.lengths); c.a.addx4(2, 14, 2); c.a.l32i(3, 2, 0); c.zero(ccz::GE, 3, skip);
    // After a full lap the current buffer may be stale. Leave its negative
    // length as a zero-fill debt; the next EOF wakes us even with queued=0.
    mask(c, s); c.call(SPEAKER_CLOCK); dma_head(c, s); c.branch(cc::EQ, 14, 8, unlock);
    c.a.addi(8, 8, 1); c.imm(7, s.count); c.a.remu(8, 8, 7); c.branch(cc::NE, 14, 8, clear);
    late(c, s, unlock); c.bind(clear);
    c.address(2, s.lengths); c.a.addx4(2, 14, 2); c.a.l32i(3, 2, 0); c.a.neg(3, 3);
    c.imm(4, 0); c.a.s32i(4, 2, 0); c.imm(5, s.slot_frames * 4); c.a.mull(5, 14, 5);
    c.address(2, s.buffer); c.a.add_n(2, 2, 5); c.feed();
    // At most 240 words, four instructions each between feeds.
    let word = c.label(); c.bind(word); c.a.s32i(4, 2, 0); c.a.addi(2, 2, 4);
    c.a.addi(3, 3, -1); c.zero(ccz::NE, 3, word); c.a.memw();
    c.load(2, s.dirty); c.a.addi(2, 2, -1); c.store(2, s.dirty);
    c.bind(unlock); unmask(c, s);
    c.bind(skip); c.a.addi(15, 15, -1); c.zero(ccz::NE, 15, next); c.bind(done);
}

fn report_lost(c: &mut Code<'_>, s: &SpeakerSram) {
    c.feed(); c.imm(2, u32::from(watch::FAULT)); c.imm(3, u32::from(watch::FAULT_SPEAKER_LOST));
    c.imm(4, s.entry as u32); c.call(EVENT);
}

// The caller has sampled the condition before testing its absolute deadline.
// Repeated polls cannot move that deadline or publish a second fault.
fn check_wait(c: &mut Code<'_>, s: &SpeakerSram, since: u32, ticks: u32, pending: usize, done: usize) {
    let expired = c.label();
    c.load(2, s.stalled); c.zero(ccz::NE, 2, done);
    elapsed(c, s, since); c.zero(ccz::LT, 4, pending); c.zero(ccz::NE, 4, expired);
    c.imm(7, ticks); c.branch(cc::LTU, 6, 7, pending);
    c.bind(expired); c.set(s.stalled, 1); c.feed(); c.imm(2, u32::from(watch::FAULT));
    c.imm(3, u32::from(watch::FAULT_SPEAKER_STALL)); c.imm(4, s.entry as u32); c.load(5, s.phase); c.call(EVENT);
    c.jump(done);
}

// Service runs once per call. a14 is 0 when the previous head observation
// brackets this one within half the copy guard, or 1 when the phase is unknown.
// a12 is nonzero if a full lap could separate two observations of the same address.
fn sample_head_time(c: &mut Code<'_>, s: &SpeakerSram) {
    c.call(SPEAKER_CLOCK); elapsed(c, s, s.previous_poll);
    let store = c.label(); c.imm(14, 1); c.imm(12, 1); c.zero(ccz::NE, 4, store);
    c.imm(7, (s.count - 1) * PERIOD - grid_skew(s)); c.branch(cc::GEU, 6, 7, store); c.imm(12, 0);
    c.imm(7, (PERIOD - SAFE_PHASE_END) / 2); c.branch(cc::LTU, 7, 6, store); c.imm(14, 0);
    c.bind(store); save_observation(c, s, s.previous_poll);
}

fn save_observation(c: &mut Code<'_>, s: &SpeakerSram, at: u32) {
    c.load(2, s.now); c.load(3, s.now + 4); c.store(2, at); c.store(3, at + 4);
}

fn refill_if_low(c: &mut Code<'_>, s: &SpeakerSram) {
    let done = c.label();
    c.load(2, s.queued); c.imm(3, s.capacity_frames() / 2); c.branch(cc::LTU, 3, 2, done);
    c.load(2, s.received); c.load(3, s.seen); c.branch(cc::NE, 2, 3, done);
    c.a.addi(2, 2, 1); c.store(2, s.received);
    c.load(2, s.now); c.load(3, s.now + 4); c.store(2, s.due); c.store(3, s.due + 4); c.bind(done);
}

pub(super) fn service(c: &mut Code<'_>, s: &SpeakerSram, mic: Option<&crate::MicSram>) {
    c.routine(SPEAKER_SERVICE, "speaker_service");
    let available = c.label(); c.load(2, s.available); c.zero(ccz::NE, 2, available); c.a.ret_n(); c.bind(available);
    c.feed(); c.store(0, s.saved);
    for r in 12..=15 { c.store(r, s.saved + 8 + u32::from(r - 12) * 4); }
    let finish = c.label(); let unlock = c.label(); let rebase = c.label(); let save_tick = c.label();
    let sync = c.label(); let start = c.label(); let first = c.label(); let quiet = c.label();
    let wait_first = c.label(); let progress = c.label(); let paused = c.label(); let account = c.label();
    let ack = c.label(); let observed = c.label();
    mask(c, s); sample_head_time(c, s);
    c.load(15, s.phase); c.imm(2, SYNC); c.branch(cc::EQ, 15, 2, sync);
    c.imm(2, PREPARED); c.branch(cc::EQ, 15, 2, start);
    c.set(OUT1 + 0x10, 0);
    c.load(2, OUT1 + 8); c.a.extui(2, 2, 1, 1); c.zero(ccz::EQ, 2, quiet);
    dma_head(c, s); c.a.mov_n(13, 8);
    c.imm(2, FIRST); c.branch(cc::EQ, 15, 2, first);
    // Elapsed periods disambiguate laps only with fresh DMA evidence.
    // A stopped transmitter can leave both RAW and its last address set.
    c.load(2, s.raw_seen); c.zero(ccz::EQ, 2, progress);
    c.load(2, s.head); c.branch(cc::EQ, 13, 2, quiet);
    c.bind(progress); c.set(s.raw_seen, 1); save_observation(c, s, s.progress_at);
    c.load(2, s.stalled); c.load(3, s.uncertain); c.a.or(2, 2, 3); c.zero(ccz::NE, 2, paused);
    finished(c, s); c.jump(account);
    c.bind(paused);
    // While the clock was stopped its elapsed ticks played no samples.
    // Retire the observed distance, counting one lap for a fresh equal address.
    c.load(2, s.head); c.imm(3, s.count); c.a.add_n(12, 13, 3); c.a.sub(12, 12, 2);
    c.a.remu(12, 12, 3); let nonzero = c.label(); c.zero(ccz::NE, 12, nonzero);
    c.a.mov_n(12, 3); c.bind(nonzero);
    c.store(14, s.uncertain);
    c.load(2, s.stalled); c.zero(ccz::EQ, 2, observed);
    c.set(s.stalled, 0); c.set(s.retry_refill, 1); c.bind(observed);
    // Rebase the retired interval so its last edge lands at this observation.
    c.imm(4, PERIOD); c.a.mull(4, 12, 4); c.load(2, s.now); c.load(3, s.now + 4);
    let borrow = c.label(); c.branch(cc::GEU, 2, 4, borrow); c.a.addi(3, 3, -1); c.bind(borrow);
    c.a.sub(2, 2, 4); c.store(2, s.last_eof); c.store(3, s.last_eof + 4);
    c.bind(account); unmask(c, s); c.zero(ccz::EQ, 12, ack);
    retire(c, s);
    c.imm(2, i32::MAX as u32); c.branch(cc::GEU, 12, 2, rebase);
    c.a.mov_n(4, 12); boundary(c, s); c.jump(save_tick);
    c.bind(rebase); c.load(2, s.now); c.load(3, s.now + 4);
    c.bind(save_tick); c.store(2, s.last_eof); c.store(3, s.last_eof + 4);
    clear_played(c, s);
    let loss = c.label(); c.load(2, s.retry_refill); c.zero(ccz::EQ, 2, loss);
    c.set(s.retry_refill, 0); refill_if_low(c, s); c.bind(loss);
    c.load(5, s.lost); c.zero(ccz::EQ, 5, ack); c.set(s.lost, 0); report_lost(c, s); c.jump(ack);

    let synced = c.label();
    c.bind(sync); c.load(2, I2S0 + 0x24); c.a.extui(2, 2, 8, 1); c.zero(ccz::EQ, 2, synced);
    unmask(c, s); check_wait(c, s, s.last_eof, sync_ticks(), finish, finish);
    c.bind(synced); reset_tx(c);
    if mic.is_some() { c.set(s.phase, PREPARED); }
    c.bind(start);
    if let Some(m) = mic {
        c.load(2, m.phase); c.imm(3, super::mic::PREPARED); c.branch(cc::LTU, 2, 3, unlock);
    }
    start_ring(c, s); c.jump(unlock);
    c.bind(first); c.store(13, s.head); c.set(s.write, s.slot_frames);
    c.store(14, s.uncertain); c.set(s.phase, RUNNING); c.set(s.raw_seen, 1); c.set(s.stalled, 0);
    save_observation(c, s, s.last_eof); save_observation(c, s, s.progress_at);
    c.set(s.retry_refill, 0); refill_if_low(c, s); unmask(c, s); c.jump(ack);

    c.bind(quiet); unmask(c, s); c.imm(2, FIRST); c.branch(cc::EQ, 15, 2, wait_first);
    // A handler can return a whole number of laps later to the very same
    // serviced RAW/address. Give it one descriptor + FIFO to move before
    // declaring a stall. Keep last_eof so fresh evidence still recovers the laps.
    let check = c.label(); let verify = c.label();
    c.load(2, s.raw_seen); c.imm(3, 2); c.branch(cc::EQ, 2, 3, verify);
    c.zero(ccz::EQ, 2, check); c.zero(ccz::EQ, 12, check);
    c.set(s.raw_seen, 2); save_observation(c, s, s.progress_at);
    c.bind(verify); check_wait(c, s, s.progress_at, eof_budget(s, 1), ack, ack);
    c.bind(check); check_wait(c, s, s.progress_at, eof_budget(s, s.count), ack, ack);
    c.bind(wait_first); check_wait(c, s, s.last_eof, eof_budget(s, 1), finish, finish);
    // Acknowledge the serviced latch away from the next edge. A fresh RAW
    // can then prove a complete lap even if the address repeats. The twin
    // refuses simultaneous set and W1C (gdma.rs Q01, ring-facts.md §2).
    c.bind(ack); mask(c, s); c.load(2, s.raw_seen); c.zero(ccz::EQ, 2, unlock);
    c.load(2, s.stalled); c.zero(ccz::NE, 2, unlock);
    c.call(SPEAKER_CLOCK); late(c, s, unlock); dma_head(c, s); c.load(2, s.head); c.branch(cc::NE, 8, 2, unlock);
    c.set(OUT1 + 0x14, EOF); c.set(s.raw_seen, 0);
    c.bind(unlock); unmask(c, s); c.bind(finish); c.feed();
    for r in 12..=15 { c.load(r, s.saved + 8 + u32::from(r - 12) * 4); }
    c.load(0, s.saved); c.a.ret_n();
}

// Refresh before looking at room, including inside a long handler. Enter the
// masked section only for the snapshot and one descriptor's copy. A raced
// snapshot returns short. The next call takes a new snapshot without waiting.
fn write_position(c: &mut Code<'_>, s: &SpeakerSram) {
    let raced = c.label(); let avoid = c.label(); let done = c.label();
    c.call(SPEAKER_SERVICE); mask(c, s); c.load(2, s.phase); c.imm(3, RUNNING); c.branch(cc::NE, 2, 3, raced);
    c.call(SPEAKER_CLOCK);
    dma_head(c, s); c.load(2, s.head); c.branch(cc::NE, 8, 2, raced);
    // Radio could have run for a whole lap between service and this mask.
    elapsed(c, s, s.previous_poll); c.zero(ccz::NE, 4, raced);
    c.imm(7, PERIOD - grid_skew(s)); c.branch(cc::GEU, 6, 7, raced);
    let frames = s.slot_frames;
    c.load(2, s.write); c.imm(3, 2 * frames); c.branch(cc::GEU, 2, 3, done);
    late(c, s, avoid);
    c.load(3, s.head); c.a.addi(3, 3, 1); c.imm(4, s.count); c.a.remu(3, 3, 4);
    c.address(4, s.lengths); c.a.addx4(3, 3, 4); c.a.l32i(3, 3, 0); c.zero(ccz::GE, 3, done);
    c.bind(avoid);
    // An empty stream can start one slot later. Never splice a silent hole
    // into a live stream by skipping the unwritten end of its next slot.
    c.imm(2, 2 * frames); c.load(4, s.queued); c.zero(ccz::EQ, 4, done);
    c.bind(raced); c.imm(2, s.count * frames);
    c.bind(done);
}

fn play(c: &mut Code<'_>, l: &SramLayout, s: &SpeakerSram, link: &Link, done: usize) {
    let stride = s.slot_frames;
    let uneven = s.clock.descriptor_cycle() > 1;
    let b = l.buffer_scratch;
    let bad = c.label(); let batch = c.label(); let stop = c.label();
    // The loader proves an I16 buffer. Reject signed or overflowing ranges
    // before touching the source, as in rt::sched's PLAY.
    c.shl(2, 2, BUFFER_ROW_SHIFT); c.address(5, link.buffers); c.a.add_n(2, 2, 5);
    c.a.l32i(5, 2, 4); c.branch(cc::LTU, 5, 3, bad); c.a.sub(5, 5, 3); c.branch(cc::LTU, 5, 4, bad);
    c.a.l32i(2, 2, 0); c.a.addx2(2, 3, 2); c.store(2, b + BUF_SRC);
    // At least the shortest descriptor is already in flight.
    c.imm(5, s.capacity_frames() - s.clock.hz / 200); c.a.minu(4, 4, 5); c.store(4, b + BUF_N); c.set(b + BUF_COUNT, 0);
    let result = c.label(); c.zero(ccz::EQ, 4, result);
    c.bind(batch); write_position(c, s); // a2 offset from head
    c.imm(3, s.count * stride); c.branch(cc::GEU, 2, 3, stop);
    c.load(4, s.head); c.imm(5, stride); c.a.mull(4, 4, 5); c.a.add_n(4, 4, 2); c.a.remu(4, 4, 3);
    c.a.quou(6, 4, 5); c.a.remu(7, 4, 5);
    if uneven { descriptor_frames(c, s, 6, 5, 9); }
    c.a.sub(5, 5, 7); c.load(8, b + BUF_N);
    if uneven {
        // On a full slot advance through its padding. A partial copy advances
        // only through the samples written. a9 survives until the cursor store.
        c.imm(9, stride); c.a.sub(9, 9, 7);
        let full = c.label(); c.branch(cc::GEU, 8, 5, full); c.a.mov_n(9, 8); c.bind(full);
    }
    c.a.minu(5, 5, 8); // playable frames this batch
    c.address(3, s.lengths); c.a.addx4(3, 6, 3); c.a.l32i(6, 3, 0);
    c.zero(ccz::LT, 6, stop);
    let dirty = c.label(); c.zero(ccz::NE, 6, dirty);
    c.load(7, s.dirty); c.a.addi(7, 7, 1); c.store(7, s.dirty); c.bind(dirty);
    c.a.add_n(6, 6, 5); c.a.s32i(6, 3, 0);
    c.a.add_n(2, 2, if uneven { 9 } else { 5 }); c.store(2, s.write);
    c.load(2, s.queued); c.a.add_n(2, 2, 5); c.store(2, s.queued);
    c.load(2, b + BUF_COUNT); c.a.add_n(2, 2, 5); c.store(2, b + BUF_COUNT);
    c.a.sub(8, 8, 5); c.store(8, b + BUF_N);
    c.address(2, s.buffer); c.a.addx4(2, 4, 2); c.load(3, b + BUF_SRC);
    // One aligned word per stereo frame, including odd source offsets in
    // packed I16 SRAM or DROM. At most 64 iterations between watchdog feeds.
    let sample = c.label(); let fed = c.label(); c.feed(); c.bind(sample);
    c.a.l16ui(4, 3, 0); c.a.slli(6, 4, 16); c.a.or(4, 4, 6); c.a.s32i(4, 2, 0);
    c.a.addi(3, 3, 2); c.a.addi(2, 2, 4); c.a.addi(5, 5, -1);
    c.a.extui(6, 5, 0, 6); c.zero(ccz::NE, 6, fed); c.feed(); c.bind(fed); c.zero(ccz::NE, 5, sample);
    c.a.memw(); c.store(3, b + BUF_SRC); unmask(c, s); c.zero(ccz::NE, 8, batch); c.jump(result);
    // A short write in an empty refill handler also gets another chance.
    // Wait for actual DMA progress before publishing that refill.
    c.bind(stop); c.set(s.retry_refill, 1); unmask(c, s);
    c.bind(result); c.load(2, b + BUF_COUNT); c.jump(done);
    c.bind(bad); c.a.mov_n(5, 4); c.a.mov_n(4, 10);
    c.imm(3, u32::from(watch::FAULT_RANGE)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT); c.imm(2, 0);
}

fn speaker_free(c: &mut Code<'_>, s: &SpeakerSram) {
    write_position(c, s);
    let cycle = s.clock.descriptor_cycle();
    if cycle > 1 {
        // a2 is a physical offset from head. Among k complete slots before
        // it, floor((head % cycle + k) / cycle) are long. Each short slot
        // contributed one padding word to the offset.
        let shift = cycle.ilog2() as u8;
        c.imm(3, s.slot_frames); c.a.quou(4, 2, 3);
        c.load(5, s.head); c.a.extui(5, 5, 0, shift); c.a.add_n(5, 5, 4); c.a.srli(5, 5, shift);
        c.a.sub(4, 4, 5); c.a.sub(2, 2, 4);
    }
    c.imm(3, s.capacity_frames()); c.a.sub(2, 3, 2); unmask(c, s);
}

fn set_amps(c: &mut Code<'_>, audio: &Audio, on: bool) {
    for amp in audio.amp_enables() {
        let bank = u32::from(amp.gpio >= 32) * 12;
        let latch = if amp.enable_high == on { 0x08 } else { 0x0c };
        c.set(GPIO + bank + latch, 1 << (amp.gpio & 31));
    }
}

fn speaker_on(c: &mut Code<'_>, l: &SramLayout, s: &SpeakerSram, timing: &Timing, done: usize) {
    let bad = c.label(); let write = c.label();
    c.imm(3, 1); c.branch(cc::LTU, 3, 2, bad);
    c.store(2, l.read_value); c.zero(ccz::NE, 2, write); set_amps(c, &s.audio, false);
    c.bind(write);
    let addr = s.audio.addr;
    // ES8311 0x31[6:5] mutes the DAC (facts-audio.md, datasheet lines 1216-1235).
    i2c_transaction(c, l.i2c_pending.expect("speaker I2C owner"), addr, timing, done, SPEAKER_CLOCK, |c| c.imm(5, 0x31), |c| {
        i2c_list(c, &[addr << 1, 0x31], &[6 << 11, (1 << 11) | (1 << 8) | 3, 2 << 11]);
        c.load(2, l.read_value); c.imm(3, 0x60); c.imm(4, 0); c.a.movnez(3, 4, 2); c.store(3, I2C0 + 0x1c);
        (3, 1)
    });
    c.load(2, l.read_value); c.zero(ccz::EQ, 2, done); set_amps(c, &s.audio, true); c.jump(done);
    c.bind(bad); c.a.mov_n(5, 2); c.a.mov_n(4, 10);
    c.imm(3, u32::from(watch::FAULT_RANGE)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
}

pub(super) fn port(c: &mut Code<'_>, l: &SramLayout, s: &SpeakerSram, link: &Link, timing: &Timing, port: u16) {
    c.routine(PORT + usize::from(port), ports::PORTS[usize::from(port)].name);
    let running = c.label(); let done = c.label();
    // Failed setup already reported its I2C fault. No buffer access, DMA
    // read, watch event, codec write or amp transition on this path.
    c.load(5, s.available); c.zero(ccz::NE, 5, running); c.imm(2, 0); c.a.ret_n();
    c.bind(running); c.store(0, l.port_return);
    match port {
        ports::PLAY => play(c, l, s, link, done),
        ports::SPEAKER_FREE => speaker_free(c, s),
        ports::SPEAKER_QUEUED => { c.call(SPEAKER_SERVICE); c.load(2, s.queued); }
        ports::SPEAKER_ON => speaker_on(c, l, s, timing, done),
        _ => unreachable!("speaker port"),
    }
    c.bind(done); c.load(0, l.port_return); c.a.ret_n();
}
