//! Firmware boundaries of the S3 spin skip, compared with the reference at every run.

use std::ops::Range;
use std::sync::Arc;

use fab_elec_mcu_emit32::xtensa::{cc, ccz, enc};
use fab_elec_mcu_emu::board::{fnv1a, Board, BusEvent, I2cAns, I2cOp, NetInfo, Payload, Res, SpiXfer};
use fab_elec_mcu_emu::{Drive, HaltKind, Lockstep, Machine, PcSet, Ran, Status, Tier, Until};
use fab_elec_mcu_s3::S3Machine;

mod support;
use support::spi_fixture::{SPI2, SPI_CLR, SPI_DONE, SPI_RAW};
use support::*;

const TIERS: [Tier; 2] = [Tier::Decoded, Tier::Native];
const SEED: u64 = 0x5300;
const END: u64 = 2 * TICK_HZ / 1000;
const CPU_PERIOD: u64 = 6; // spi_setup selects the 80 MHz CPU on the 480 MHz tick grid.
const POLL_INSNS: u64 = 13;
const TRANSFER_CYCLES: u64 = 64 * 512;

// periph/time.rs Rtc::write and Watchdog::action/hold.
const CONFIG0: u32 = RTC + 0x98;
const HOLD0: u32 = RTC + 0x9c;
const FEED: u32 = RTC + 0xac;
const PROTECT: u32 = RTC + 0xb0;
const FEED_BIT: u32 = 1 << 31;
const IRQ: u32 = 1;
const SYSTEM_RESET: u32 = 4;
const LONG_HOLD: u32 = 200_000;

// NoBoard rejects SPI at the last clock. This write-only receiver records what reached
// the wire. SPI2 schedules the clocks, releases CS and raises DONE (periph/spi.rs).
#[derive(Clone, Default, PartialEq)]
struct SpiReceiver {
    transfers: Vec<(SpiXfer, u64)>,
    saved_len: usize,
}

impl Board for SpiReceiver {
    type In = ();
    fn bind(&mut self, _: &[&'static str], _: u64) -> Result<(), String> { Ok(()) }
    fn pad_drive(&mut self, _: u64, _: u16, _: Drive) -> Res { Ok(()) }
    fn pad_inputs(&mut self, _: &mut Vec<(u16, bool)>) {}
    fn chip_reset(&mut self, _: u64) -> Res { Ok(()) }
    fn i2c(&mut self, _: u64, _: u16, _: u16, _: I2cOp) -> Res<I2cAns> { Err("no I2C target") }
    fn spi(&mut self, x: &SpiXfer, tx: &dyn Payload, _: &mut Vec<u8>) -> Res {
        self.transfers.push((*x, fnv1a(tx)));
        Ok(())
    }
    fn next_event(&self) -> Option<u64> { None }
    fn event(&mut self, _: u64) -> Res { Err("unscheduled SPI receiver event") }
    fn input(&mut self, _: u64, _: ()) -> Res { Ok(()) }
    fn mark(&mut self) { self.saved_len = self.transfers.len(); }
    fn rewind(&mut self) { self.transfers.truncate(self.saved_len); }
    fn same_state(&self, other: &Self) -> Option<&'static str> {
        (self != other).then_some("SPI receiver transfers or mark")
    }
    fn barrier(&self) -> bool { false }
    fn nets(&self) -> &[NetInfo] { &[] }
    fn take_bus(&mut self, _: &mut Vec<BusEvent>) {}
    fn publish(&mut self, _: f64) {}
}

fn pair<M: Machine>(m: M, tier: Tier, alternating: bool) -> Lockstep<M> {
    #[cfg(feature = "jit")]
    if tier == Tier::Native { fab_elec_mcu_emu::jit::set_inline_compile(true); }
    let mut m = if alternating { Lockstep::alternating(m) } else { Lockstep::new(m) };
    m.set_tier(tier);
    m
}

fn lockstep(image: Arc<Vec<u8>>, seed: u64, tier: Tier) -> Lockstep<S3Machine> {
    pair(S3Machine::new(image, &profile(seed)), tier, false)
}

fn spi_lockstep(image: Arc<Vec<u8>>, tier: Tier, alternating: bool) -> Lockstep<S3Machine<SpiReceiver>> {
    let m = S3Machine::with_board(image, &profile(SEED), SpiReceiver::default())
        .expect("the write-only SPI target accepts the S3 handoff");
    pair(m, tier, alternating)
}

fn program(action: u32, hold: u32) -> Program {
    let mut p = Program::new();
    p.disable_watchdogs(); // Stop the boot RWDT before shortening its hold below the junk count.
    p.spi_setup();
    // Spi::prepare: N=L=63, H=31, PRE=7 divides PLL_F80M by 64*8.
    p.store(SPI2 + 0x0c, (7 << 18) | (63 << 12) | (31 << 6) | 63);
    p.store(PROTECT, WKEY);
    p.store(CONFIG0, (1 << 31) | (action << 28));
    p.store(HOLD0, hold);
    p.store(FEED, FEED_BIT);
    p.store(PROTECT, 0);
    p
}

fn transfer(p: &mut Program) -> u32 {
    // 8 command + 24 address + 32 W0 data clocks: 32,768 CPU cycles, over 2,500 driver polls.
    p.spi_transfer(0x2c, &[0xf8, 0, 0x07, 0xe0], None)
}

fn displacement(pc: u32, target: u32) -> i32 { (i64::from(target) - i64::from(pc) - 4) as i32 }

fn branch_zero(p: &mut Program, cond: u8, reg: u8, target: u32) {
    p.emit(enc::bccz(cond, reg, displacement(p.pc(), target)));
}

fn feed(p: &mut Program) {
    // emit_s3/runtime.rs::feed keeps the protect address in a9 across the FEED store.
    p.load(9, PROTECT);
    p.load(10, WKEY);
    p.emit(enc::s32i(10, 9, 0));
    p.load(11, FEED);
    p.load(10, FEED_BIT);
    p.emit(enc::s32i(10, 11, 0));
    p.emit(enc::movi(10, 0));
    p.emit(enc::s32i(10, 9, 0));
}

fn poll(p: &mut Program, body: impl FnOnce(&mut Program)) -> Range<u32> {
    let start = p.pc();
    body(p);
    p.load(11, SPI_RAW);
    p.emit(enc::l32i(2, 11, 0));
    p.load(3, SPI_DONE);
    p.emit(enc::and(2, 2, 3));
    branch_zero(p, ccz::EQ, 2, start);
    let end = p.pc();
    p.store(SPI_CLR, SPI_DONE);
    start..end
}

fn driver() -> (Arc<Vec<u8>>, [Range<u32>; 3], u32) {
    let mut p = program(IRQ, LONG_HOLD);
    let polls = std::array::from_fn(|_| { transfer(&mut p); poll(&mut p, feed) });
    let halt = p.halt();
    (p.finish(), polls, halt)
}

fn run_tick(m: &mut impl Machine, tick: u64) -> Ran {
    m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false })
}

fn check_halt(m: &impl Machine, pc: u32) {
    let Status::Halted(h) = m.status() else { panic!("firmware must reach its BREAK at {pc:#x}"); };
    assert_eq!((h.kind, h.pc), (HaltKind::Break, pc), "firmware must halt at its own BREAK: {h:?}");
}

fn finish(m: &mut impl Machine, halt: u32) {
    assert_eq!(run_tick(m, END), Ran::Halted, "firmware must finish within two simulated milliseconds");
    check_halt(m, halt);
}

fn one_call(image: Arc<Vec<u8>>, tier: Tier, halt: u32) -> u64 {
    let mut m = spi_lockstep(image, tier, false);
    finish(&mut m, halt);
    assert_eq!(m.board().transfers.len(), 3, "all three driver polls must wait for a separate transfer");
    assert!(m.spin_skipped() > 0, "{tier:?} must skip the driver's unlock/feed/relock poll");
    m.now()
}

#[test]
fn driver_poll() {
    let (image, _, halt) = driver();
    for tier in TIERS { one_call(image.clone(), tier, halt); }
}

fn random(state: &mut u32, range: u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state % range
}

fn split_run(m: &mut Lockstep<S3Machine<SpiReceiver>>, polls: &[Range<u32>; 3], halt: u32, mut rng: u32) {
    let mut marks = PcSet::new(ENTRY, halt - ENTRY + 1);
    let head = polls[0].start;
    marks.insert(head);
    let after = 10_000 + u64::from(random(&mut rng, 8192));
    assert_eq!(m.run(&Until { tick: END, marks: &marks, stops: &[head], stops_after: after, on_drive: false }),
        Ran::Pc, "the first poll must hit the marked stop after its randomized threshold");
    assert!(m.now() > after && m.pc() == head, "the stop must honor stops_after at the requested PC");
    m.mark();

    // End between instruction boundaries, partway through a randomly chosen full poll.
    let insn = 1 + random(&mut rng, POLL_INSNS as u32 - 1);
    let cycles = (10 + u64::from(random(&mut rng, 200))) * POLL_INSNS + u64::from(insn);
    let tick = m.now() + cycles * CPU_PERIOD - 1;
    assert_eq!(run_tick(m, tick), Ran::Tick, "an unmarked piece must reach its in-poll deadline");
    let cut = (m.now(), m.pc(), m.cpu().instret);
    assert_eq!((cut.0, cut.1), (tick + 1, head + 3 * insn), "the cut must select the intended poll instruction");
    m.rewind();
    assert_eq!(m.run(&Until { tick, marks: &marks, stops: &[], stops_after: 0, on_drive: true }),
        Ran::Tick, "replay with the poll marked must reach the same deadline");
    assert_eq!((m.now(), m.pc(), m.cpu().instret), cut, "marks and rewind must preserve the retirement boundary");

    // The next drive is CS release. An unmarked run can skip almost to that edge.
    let edge = m.run(&Until { tick: END, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: true });
    let [(x, _)] = m.board().transfers.as_slice() else { panic!("CS release must follow the first wire transfer"); };
    assert_eq!(edge, Ran::Drive { at: x.t1 + u64::from(x.bit_ticks / 2) },
        "on_drive must return at the modeled half-clock CS hold after the last data edge");

    loop {
        let now = m.now();
        assert!(now < END, "split runs must reach the firmware BREAK before the deadline");
        let span = 24_000 + random(&mut rng, 90_000);
        let mut marks = PcSet::new(ENTRY, halt - ENTRY + 1);
        let mut stops = Vec::new();
        for poll in polls {
            let pc = poll.start + 3 * random(&mut rng, (poll.end - poll.start) / 3);
            if random(&mut rng, 2) != 0 {
                marks.insert(pc);
                if random(&mut rng, 2) != 0 { stops.push(pc); }
            }
        }
        let until = Until { tick: (now + u64::from(span)).min(END), marks: &marks, stops: &stops,
            stops_after: now + u64::from(random(&mut rng, span)), on_drive: random(&mut rng, 2) != 0 };
        match m.run(&until) {
            Ran::Halted => break,
            Ran::Pc => assert!(stops.contains(&m.pc()) && m.now() > until.stops_after,
                "a split PC stop must be armed and past its threshold"),
            Ran::Drive { .. } | Ran::Tick => (),
            why => panic!("unexpected return from a split driver poll: {why:?}"),
        }
        assert!(m.now() > now, "each unfinished piece must advance the driver");
    }
    check_halt(m, halt);
    assert_eq!(m.board().transfers.len(), 3, "split runs must complete the three transfers once each");
}

#[test]
fn split_invariance() {
    let (image, polls, halt) = driver();
    for tier in TIERS {
        let straight = one_call(image.clone(), tier, halt);
        let mut skipped = 0;
        for (seed, alternating) in [(17, false), (53, false), (89, false), (101, true)] {
            let mut m = spi_lockstep(image.clone(), tier, alternating);
            split_run(&mut m, &polls, halt, seed);
            assert_eq!(m.now(), straight, "split {tier:?} seed {seed} must halt at the one-call tick");
            skipped += m.spin_skipped();
        }
        assert!(skipped > 0, "piecewise {tier:?} execution must skip at least one complete poll");
    }
}

fn spi_test(mut p: Program, check: impl Fn(&mut Lockstep<S3Machine<SpiReceiver>>, Tier)) {
    let halt = p.halt();
    let image = p.finish();
    for tier in TIERS {
        let mut m = spi_lockstep(image.clone(), tier, false);
        finish(&mut m, halt);
        assert_eq!(m.board().transfers.len(), 1, "the poll must complete its SPI transfer");
        check(&mut m, tier);
    }
}

const MATCH_COUNT: u32 = 4096;
const TIMER0_LINE: u32 = 1 << 6;

fn arm_compare(p: &mut Program) -> u32 {
    // xtensa/sim.rs: WSR CCOUNT retires to 1; WSR CCOMPARE0 clears its pending bit.
    let count_write = p.sr(234, 0);
    p.sr(240, MATCH_COUNT);
    p.sr(228, TIMER0_LINE);
    count_write
}

#[test]
fn ccompare_masked() {
    let mut p = program(IRQ, LONG_HOLD);
    let count_write = arm_compare(&mut p);
    transfer(&mut p);
    let polling = poll(&mut p, feed);
    let halt = p.halt();
    let image = p.finish();
    for tier in TIERS {
        let mut m = spi_lockstep(image.clone(), tier, false);
        pins_at_pc(&mut m, count_write);
        m.mark();
        let due = m.now() + u64::from(MATCH_COUNT) * CPU_PERIOD;
        assert_eq!(run_tick(&mut m, due - CPU_PERIOD), Ran::Tick, "the masked timer must reach the pre-match cycle");
        assert_eq!(m.cpu().interrupt & TIMER0_LINE, 0, "CCOMPARE0 must stay clear one cycle before its match");
        assert_eq!(run_tick(&mut m, due), Ran::Tick, "the masked timer must reach its match tick");
        assert!(polling.contains(&m.pc()), "the masked match must occur inside the driver poll");
        assert_eq!(m.cpu().interrupt & TIMER0_LINE, TIMER0_LINE, "masked CCOMPARE0 must latch at its match");
        m.rewind();
        let before = m.spin_skipped();
        finish(&mut m, halt); // This run has no host deadline at the comparator.
        assert_eq!(m.cpu().interrupt & TIMER0_LINE, TIMER0_LINE, "the masked request must remain pending at BREAK");
        assert_eq!(m.cpu().ps & 15, 15, "level 15 must mask timer0 for the whole transfer");
        assert!(m.spin_skipped() > before, "{tier:?} must skip polls in the uninterrupted run across CCOMPARE0");
    }
}

fn short_watchdog(action: u32) {
    let mut p = program(action, 20);
    // Spi::prepare: XTAL / (64 * 16) stretches one transfer across five 40-slow-cycle holds.
    p.store(SPI2 + 0xe8, 2);
    p.store(SPI2 + 0x0c, (15 << 18) | (63 << 12) | (31 << 6) | 63);
    transfer(&mut p);
    poll(&mut p, feed);
    p.read_time(RTC + 0x44, 7);
    spi_test(p, |m, tier| {
        assert_eq!(m.cpu().ar[7] & (1 << 3), 0, "timely feeds must keep the RWDT interrupt clear");
        assert!(m.last_reset().is_none(), "timely feeds must prevent the stage-0 reset");
        assert!(m.spin_skipped() > 0, "{tier:?} must skip within the stage-0 action {action} sync bound");
    });
}

#[test]
fn rwdt_interrupt_window() { short_watchdog(IRQ); }

#[test]
fn rwdt_system_reset_window() { short_watchdog(SYSTEM_RESET); }

#[test]
fn rwdt_interrupt_pending() {
    let mut p = program(IRQ, 8);
    p.sr(228, 0);
    p.load(4, RTC + 0x44); // Rtc::read RAW; Watchdog::expire sets bit 3.
    p.emit(enc::movi(5, 1 << 3));
    let wait = p.emit(enc::l32i(6, 4, 0));
    p.emit(enc::and(6, 6, 5));
    branch_zero(&mut p, ccz::EQ, 6, wait);
    transfer(&mut p);
    poll(&mut p, feed);
    p.read_time(RTC + 0x44, 7);
    spi_test(p, |m, tier| {
        assert_eq!(m.cpu().ar[7] & (1 << 3), 1 << 3, "feeds must leave the expired RWDT interrupt pending");
        let mut notes = Vec::new();
        m.take_notes(&mut notes);
        let feeds = notes.iter().filter(|n| n.what.starts_with("watchdog fed with its interrupt pending")).count();
        // USR at cycle 0, first RAW load at 10, DONE at cycle 32,768. The last poll feeds too.
        let polls = 1 + (TRANSFER_CYCLES - 10).div_ceil(POLL_INSNS);
        assert_eq!(feeds as u64, polls, "every feed while RAW is pending must produce its own note");
        assert_eq!(m.spin_skipped(), 0, "{tier:?} must execute every poll that produces FED_PENDING");
    });
}

#[test]
fn feed_before_unlock() {
    let mut p = program(IRQ, LONG_HOLD);
    p.load(4, SPI_RAW);
    p.load(5, SPI_DONE);
    p.load(9, FEED);
    p.load(10, FEED_BIT);
    p.load(11, PROTECT);
    p.load(12, WKEY);
    p.emit(enc::movi(7, 1));
    p.store(PROTECT, 0);
    transfer(&mut p);
    // Visit the same RAW load twice while locked. At the second anchor a7 and the
    // masked result have settled. Only the key changes over the first probed cycle.
    let enter = p.emit(enc::j(3 * 3 - 4));
    let stores = p.emit(enc::s32i(10, 9, 0));
    p.emit(enc::s32i(12, 11, 0));
    let read = p.emit(enc::l32i(6, 4, 0));
    p.emit(enc::and(3, 6, 5));
    p.emit(enc::bccz(ccz::EQ, 7, 3 * 3 - 4));
    p.emit(enc::movi(7, 0));
    p.emit(enc::j(displacement(p.pc(), read)));
    branch_zero(&mut p, ccz::EQ, 3, stores);
    p.store(SPI_CLR, SPI_DONE);
    p.read_time(PROTECT, 7);
    let halt = p.halt();
    let image = p.finish();
    for tier in TIERS {
        let mut m = spi_lockstep(image.clone(), tier, false);
        pins_at_pc(&mut m, enter);
        // First probe ends at cycle 13; two more six-instruction cycles fit.
        let tick = m.now() + 25 * CPU_PERIOD;
        assert_eq!(run_tick(&mut m, tick), Ran::Tick, "the first key-changing cycle must fit before the cut");
        assert_eq!(m.spin_skipped(), 0, "{tier:?} must reject the cycle that changed the protect key");
        finish(&mut m, halt);
        assert_eq!(m.cpu().ar[7], WKEY, "the unlocked key must survive the later FEED writes");
        assert!(m.spin_skipped() > 0, "{tier:?} must skip after the key stays unlocked");
    }
}

#[test]
fn ccount_timeout() {
    let mut p = program(IRQ, LONG_HOLD);
    const TIMEOUT: u32 = 4096;
    p.load(4, SPI_RAW);
    p.load(5, SPI_DONE);
    p.load(8, TIMEOUT);
    p.emit(enc::rsr(0, 3, 0, 234, 6)); // RSR a6, CCOUNT
    let top = p.emit(enc::rsr(0, 3, 0, 234, 7));
    p.emit(enc::sub(7, 7, 6));
    p.emit(enc::bcc(cc::GEU, 7, 8, 4 * 3 - 4)); // BGEU to BREAK
    p.emit(enc::l32i(2, 4, 0));
    p.emit(enc::and(2, 2, 5));
    branch_zero(&mut p, ccz::EQ, 2, top);
    let halt = p.halt();
    let image = p.finish();
    for tier in TIERS {
        let mut m = lockstep(image.clone(), SEED, tier);
        finish(&mut m, halt);
        assert_eq!(m.cpu().ar[2], 0, "TRANS_DONE must stay low without a transfer");
        // The first sample has elapsed=1 and the loop samples every six instructions.
        assert_eq!(m.cpu().ar[7], 1 + (TIMEOUT - 1).div_ceil(6) * 6, "BGEU must take the first expired sample");
        assert_eq!(m.spin_skipped(), 0, "{tier:?} must end the lane at RSR CCOUNT");
    }
}

#[test]
fn ana_busy_poll() {
    // periph/ana.rs: DIG_REG (0x6d), register 0, read; bit 25 is BUSY for 9,600 ticks.
    const ANA: u32 = 0x6000_e000;
    // With SPI_RAW, the stable read starts probes that the ANA read must end; alone, the ANA
    // read must not start one (`while (cmd & BUSY)`).
    for with_spi in [true, false] {
        let mut p = program(IRQ, LONG_HOLD);
        p.load(4, SPI_RAW);
        p.load(5, ANA);
        p.load(6, 1 << 25);
        let command = p.store(ANA, 0x6d);
        let top = p.pc();
        if with_spi { p.emit(enc::l32i(2, 4, 0)); }
        p.emit(enc::l32i(3, 5, 0));
        p.emit(enc::and(7, 3, 6));
        branch_zero(&mut p, ccz::NE, 7, top);
        let halt = p.halt();
        let image = p.finish();
        for tier in TIERS {
            let mut m = lockstep(image.clone(), SEED, tier);
            pins_at_pc(&mut m, command);
            let ready = m.now() + 9_600;
            finish(&mut m, halt);
            assert!((ready..=ready + 7 * CPU_PERIOD).contains(&m.now()), "the first non-busy ANA read must end the poll");
            assert_eq!(m.cpu().ar[7], 0, "the final ANA sample must have BUSY clear");
            assert_eq!(m.spin_skipped(), 0, "{tier:?} (SPI_RAW read too: {with_spi}) must not skip a tick-dependent ANA poll");
        }
    }
}

fn word(m: &impl Machine, addr: u32) -> u32 {
    let mut bytes = [0; 4];
    assert!(m.read_data(addr, &mut bytes), "the firmware result word must be readable in SRAM");
    u32::from_le_bytes(bytes)
}

#[test]
fn sram_store_in_body() {
    let mut p = program(IRQ, LONG_HOLD);
    p.store(RAM, 0);
    p.load(12, RAM);
    p.load(13, 0x53);
    transfer(&mut p);
    poll(&mut p, |p| { feed(p); p.emit(enc::s32i(13, 12, 0)); });
    spi_test(p, |m, tier| {
        assert_eq!(word(m, RAM), 0x53, "the poll's SRAM store must reach its result word");
        assert_eq!(m.spin_skipped(), 0, "{tier:?} must reject even a repeated same-value SRAM store");
    });
}

#[test]
fn gpio_input_mid_window() {
    const PAD: u32 = 15;
    let mut p = program(IRQ, LONG_HOLD);
    p.gpio_setup(PAD, MUX_IE);
    p.store(GPIO_ENABLE + 8, 1 << PAD); // ENABLE_W1TC
    p.store(pin_reg(PAD), 1 << 7); // Pins::set_input also records the rising edge's tick.
    p.pin_read(6, GPIO_IN);
    p.load(12, RAM);
    p.emit(enc::s32i(6, 12, 0));
    transfer(&mut p);
    let polling = poll(&mut p, feed);
    p.pin_read(6, GPIO_IN);
    p.emit(enc::s32i(6, 12, 4));
    let halt = p.halt();
    let image = p.finish();
    for tier in TIERS {
        let mut m = spi_lockstep(image.clone(), tier, false);
        m.set_input(PAD as usize, false);
        pins_at_pc(&mut m, polling.start);
        let edge = m.now() + (300 * POLL_INSNS + 5) * CPU_PERIOD;
        assert_eq!(run_tick(&mut m, edge), Ran::Tick, "the host input deadline must cut the SPI wait");
        assert_eq!((m.now(), m.pc()), (edge, polling.start + 5 * 3), "deliver the GPIO edge inside the feed");
        m.set_input(PAD as usize, true);
        finish(&mut m, halt);
        assert_eq!((word(&m, RAM) & (1 << PAD), word(&m, RAM + 4) & (1 << PAD)), (0, 1 << PAD),
            "firmware must sample the pad low before the transfer and high after the in-poll edge");
        assert!(m.spin_skipped() > 0, "{tier:?} must skip polling cycles around the input tick");
    }
}

#[test]
fn radio_critical_section() {
    let mut p = program(IRQ, LONG_HOLD);
    p.sr(230, 0);
    transfer(&mut p);
    poll(&mut p, |p| {
        p.emit(enc::rrr(0, 0, 0, 6, 15, 8)); // RSIL a8, 15, as in runtime.rs::feed.
        feed(p);
        p.emit(enc::wsr(8, 230));
        p.emit(enc::rsync());
    });
    spi_test(p, |m, tier| {
        assert_eq!(m.cpu().ps & 15, 0, "the radio feed must restore the original interrupt level");
        assert_eq!(m.spin_skipped(), 0, "{tier:?} must end the lane across the radio feed's RSIL");
    });
}

const HANDLER: u32 = IRAM + 0x300;

fn install_handler(p: &mut Program, code: &[u32]) {
    // tests/intr.rs: pack 24-bit instructions into aligned words through IRAM's data alias.
    let bytes: Vec<_> = code.iter().flat_map(|insn| {
        let [lo, mid, hi, _] = insn.to_le_bytes();
        [lo, mid, hi]
    }).collect();
    for (i, chunk) in bytes.chunks(4).enumerate() {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        p.store(RAM + 0x300 + i as u32 * 4, u32::from_le_bytes(word));
    }
    p.sr(231, IRAM); // VECBASE, level-1 kernel vector +0x300
    p.sr(230, 0); // PS.INTLEVEL=0, EXCM and UM clear
}

fn finish_with_interrupt(m: &mut impl Machine, halt: u32, due: u64) {
    // Trace only vector entry, leaving every poll PC unmarked across the interrupt.
    let mut marks = PcSet::new(HANDLER, 1);
    marks.insert(HANDLER);
    assert_eq!(m.run(&Until { tick: END, marks: &marks, stops: &[], stops_after: 0, on_drive: false }),
        Ran::Halted, "the level-1 handler must return to the polling firmware");
    check_halt(m, halt);
    let mut trace = Vec::new();
    m.take_trace(&mut trace);
    assert_eq!(trace, [(due, HANDLER)], "level-1 entry must occur exactly once, at the source's tick");
}

#[test]
fn ccompare_taken() {
    let mut p = program(IRQ, LONG_HOLD);
    p.store(RAM, 0);
    p.load(12, RAM);
    p.emit(enc::movi(13, 0));
    install_handler(&mut p, &[
        enc::wsr(13, 240), // Clear the request; CCOUNT next reaches zero after wrap.
        enc::l32i(14, 12, 0), enc::addi(14, 14, 1), enc::s32i(14, 12, 0), enc::rfe(),
    ]);
    let count_write = arm_compare(&mut p);
    transfer(&mut p);
    poll(&mut p, feed);
    let halt = p.halt();
    let image = p.finish();
    for tier in TIERS {
        let mut m = spi_lockstep(image.clone(), tier, false);
        pins_at_pc(&mut m, count_write);
        let due = m.now() + u64::from(MATCH_COUNT) * CPU_PERIOD;
        finish_with_interrupt(&mut m, halt, due);
        assert_eq!(word(&m, RAM), 1, "one CCOUNT match before wrap must run the timer handler once");
        assert_eq!(m.cpu().interrupt & TIMER0_LINE, 0, "the handler's CCOMPARE0 write must clear timer0");
        assert!(m.spin_skipped() > 0, "{tier:?} must skip around a taken CCOMPARE0 interrupt");
    }
}

#[test]
fn systimer_isr_flag() {
    const TARGET0_MAP: u32 = 0x600c_2000 + 57 * 4; // periph/intr.rs: SYSTIMER_TARGET0 source 57
    const TARGET: u32 = 1024;
    let mut p = program(IRQ, LONG_HOLD);
    p.store(RAM, 0);
    p.load(12, RAM);
    p.emit(enc::movi(13, 1));
    p.load(14, SYSTIMER + 0x6c);
    install_handler(&mut p, &[enc::s32i(13, 12, 0), enc::s32i(13, 14, 0), enc::rfe()]);
    p.store(TARGET0_MAP, 0);
    p.sr(228, 1); // matrix line 0 is level 1
    // periph/time.rs Systimer::write: stop/load UNIT0, load target 0, enable the one-shot.
    p.store(SYSTIMER, 0);
    p.store(SYSTIMER + 0x0c, 0);
    p.store(SYSTIMER + 0x10, 0);
    p.store(SYSTIMER + 0x5c, 1);
    p.store(SYSTIMER + 0x1c, 0);
    p.store(SYSTIMER + 0x20, TARGET);
    p.store(SYSTIMER + 0x50, 1);
    p.store(SYSTIMER + 0x64, 1);
    let enable = p.store(SYSTIMER, (1 << 30) | (1 << 24));
    p.load(4, SPI_RAW);
    let top = p.emit(enc::l32i(2, 4, 0));
    p.emit(enc::l32i(3, 12, 0));
    branch_zero(&mut p, ccz::EQ, 3, top);
    p.read_time(SYSTIMER + 0x68, 7);
    let halt = p.halt();
    let image = p.finish();
    for tier in TIERS {
        let mut m = lockstep(image.clone(), SEED, tier);
        pins_at_pc(&mut m, enable);
        // The 36/24 tick SYSTIMER edges all fall on 6-tick CPU instruction boundaries.
        let edge = systimer_edges(m.now(), true) + u64::from(TARGET);
        let due = edge / 2 * 60 + (edge % 2) * 36;
        finish_with_interrupt(&mut m, halt, due);
        assert_eq!((word(&m, RAM), m.cpu().ar[3]), (1, 1), "main must observe the SRAM flag set by the ISR");
        assert_eq!(m.cpu().ar[7] & 1, 0, "the handler must acknowledge the one-shot SYSTIMER alarm");
        assert!(m.spin_skipped() > 0, "{tier:?} must skip the flag poll up to the routed alarm");
    }
}
