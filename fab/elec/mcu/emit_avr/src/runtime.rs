//! Instruction emission and uninterrupted cycle budgets. Interrupt service is
//! additional to these budgets; timer edges keep running during every block.

use fab_elec_mcu_avr::asm::{Asm, Label, Target};
use fab_elec_mcu_avr::chip;
use fab_elec_mcu_avr::insn::{cycles, Insn, Ptr};
use fab_elec_mcu_rt::{ports, pwm, watch};

use crate::image::{pwm_registers, scaled_pwm, Labels, Layout, Program, Role, Stubs, Symbols};
use Insn::*;

pub const PORTS_VERSION: u8 = fab_elec_mcu_rt::ports::PORT_TABLE_VERSION as u8;
pub const PRESCALE: u8 = 8;

/// Reduced Timer0 tick-to-time ratios: floor(ticks * numerator / denominator).
pub struct Timebase {
    pub f_clk: u32,
    pub us: (u32, u32),
    pub ms: (u32, u32),
}

impl Timebase {
    pub fn new(f_clk: u32) -> Timebase {
        assert!(f_clk >= 1, "CPU clock must be at least one Hz");
        let reduce = |n| {
            let (mut a, mut b) = (n, f_clk);
            while b != 0 { (a, b) = (b, a % b); }
            (n / a, f_clk / a)
        };
        Timebase { f_clk, us: reduce(8_000_000), ms: reduce(8_000) }
    }
}

pub const TOV_INT: u32 = 4 + 2 + 6 + 4;
/// On a pass that does not defer, an RX service completed before the first
/// handler instruction adds its measured service length to entry (at most
/// Bounds::rx_int), plus any additional TOV service
/// made eligible by that time.
/// Count service starts, with their own lengths, on each side of entry:
/// pre-entry services delay entry; services in [entry, effect) delay effects.
/// Timer0 wins IRQ ties. If both are pending at SEI/CALL, TOV's RETI lets the
/// first handler instruction run before RX: that RX delays the prefix only.
/// No compensation or interrupt-draining trampoline is added to dispatch.
/// The masked FIFO scan publishes a cut and clears rx_early. An accepted
/// byte stamped strictly before that cut defers the selected timer: check A
/// precedes timer bookkeeping, check B is in the masked dispatch window.
/// B restores the timer's due before rescanning; a tie still runs the timer
/// first. Byte dispatch cuts cannot be overtaken by later reception stamps.
/// SerialLost counts FIFO drops exactly and adds
/// one per DOR0-tagged frame, saturating at u32::MAX. Since DOR0 can coalesce
/// hardware losses, the report uses FAULT_SERIAL_LOST_AT_LEAST when any
/// hardware overrun contributed to its count, and FAULT_SERIAL_LOST otherwise.
/// B's CLI through SEI, including the taken check and due restoration.
pub const RX_DEFER_MASK: u32 = 50;
/// B's CLI through the jump back to the pass (no CALL frame was pushed).
pub const RX_DEFER: u32 = RX_DEFER_MASK + 3;
pub const NOW48: u32 = 43;
pub const LOCK: u32 = 224;
pub const DELAY_BASE: u32 = 20;
pub const N_MIN: u32 = DELAY_BASE + 4;
pub const L: u32 = TOV_INT + 6;
pub const DISPATCH: u32 = 65;
pub const E: u32 = (DISPATCH - L - 1) / 8;
pub const BOOKKEEP: u32 = 256;
pub const C: u32 = LOCK + BOOKKEEP + L - 7 - DISPATCH;

/// A first conversion on every call: ADEN=0 resets the ADC prescaler phase.
pub(crate) struct Adc { pub div: u32 }

impl Adc {
    pub(crate) fn new(f_clk: u32) -> Result<Self, String> {
        // Compare products, not truncated quotients, at the datasheet's limits.
        let div = [2, 4, 8, 16, 32, 64, 128].into_iter()
            .find(|&div| f_clk <= 200_000 * div);
        match div {
            Some(div) if f_clk >= 50_000 * div => Ok(Self { div }),
            _ => Err(format!("analog inputs at CPU clock {f_clk} Hz require an ADC clock in 50000..=200000 Hz (50..=200 kHz), with divisor 2, 4, 8, 16, 32, 64 or 128")),
        }
    }

    // First-conversion completion plus one clock, so ADCL cannot precede completion.
    fn wait(&self) -> u32 { self.div / 2 + 25 * self.div + 1 }
    fn convert(&self) -> u32 { 7 + self.wait() + 4 + 3 + 2 + 4 }
    fn sample(&self) -> u32 { 7 + 14 * self.div }
}

pub const UDRE_BYTE: u32 = 39;
/// CLI, two cursor stores, UART enable, SEI: 1 + 4 + 3 + 1.
pub const TX_PUBLISH: u32 = 9;
/// Threshold 82 + TX decision 21 + STS/SEI/SLEEP 4. Excludes the wake check.
pub const FAR_SUFFIX: u32 = 107;
/// Serial fast check 5 + CLI 1 + FIFO check 7 + Lost/room check 24
/// + threshold 13 + empty TX/work check 17 + SEI/SLEEP/RJMP 4 = 71.
/// Without serial subtract twelve. Excludes encoding, sleep wait and ISRs.
pub const IDLE_CHECK_MAX: u32 = 71;
/// Repeated sleeps return through RJMP 2 before the check; the first does not.
pub const IDLE_WAKE_MAX: u32 = 2 + IDLE_CHECK_MAX;

/// Uninterrupted worst cases. Stub totals include RET but not the caller's
/// CALL; interrupt totals include hardware entry, vector and RETI. Bounded
/// routines fill these as they emit; an unlinked bounded stub has a zero bound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub mul32: u32,
    pub udivmod32: u32,
    pub sdiv32: u32,
    pub srem32: u32,
    pub shl32: u32,
    pub shr32: u32,
    pub sar32: u32,
    pub eeprom_read: u32,
    pub eeprom_write: u32,
    /// A full previous write, rounded up to whole three-cycle busy polls.
    /// Stub totals add the final two-cycle skip and their own CPU halt.
    pub eeprom_wait: u32,
    pub shift_spi: u32,
    /// Largest linked pair's bound (SBI/CBI have the same cost on every GPIO port).
    pub shift_bitbang: u32,
    pub push: u32,
    pub write: u32,
    /// Entry through the completed PORT store, excluding interrupt service.
    pub write_effect: u32,
    pub read: u32,
    pub record_write: u32,
    pub record_read: u32,
    pub adc: u32,
    pub adc_convert: u32,
    /// Conversion routine entry to sample-and-hold, excluding interrupts before ADEN/ADSC.
    pub adc_sample: u32,
    pub duty: u32,
    /// Entry through the completed OCR/COM store, excluding interrupt service.
    pub duty_effect: u32,
    pub clock_us: u32,
    pub clock_ms: u32,
    pub timer_stop: u32,
    pub timer_start: u32,
    pub timer_period: u32,
    pub loop_cap: u32,
    pub serial_byte: u32,
    /// Pass entry through the near branch, excluding the optional RX scan.
    pub pass_prefix: u32,
    /// Far suffix through SLEEP, excluding the pass prefix and branch.
    pub far: u32,
    pub idle_check: u32,
    /// Extra bookkeeping beyond the exact no-overrun path.
    pub overrun: u32,
    pub rx_int: u32,
    /// Active reset and masked FIFO scan through SEI.
    pub rx_scan: u32,
    /// FIFO scan's CLI through SEI, excluding the active reset.
    pub rx_scan_mask: u32,
    /// Byte take's CLI through SEI.
    pub rx_take: u32,
    /// Bookkeeping gate through IJMP to the byte dispatch stub.
    pub rx_bookkeep: u32,
    pub udre: u32,
    /// First encode SEI through publication and the jump back to the idle check.
    pub encode: u32,
}

impl Bounds {
    fn new() -> Self {
        Self { far: FAR_SUFFIX, idle_check: IDLE_CHECK_MAX, ..Self::default() }
    }

    /// Fixed work through entry dispatch, excluding the deliberate delay and the
    /// separately counted late-only overrun extension. The far path is shorter.
    pub fn pass_max(&self) -> u32 { self.pass_prefix + LOCK + 4 + BOOKKEEP + L }
    pub fn redispatch(&self) -> u32 { self.pass_max() + N_MIN }
    // A threshold crossed after its masked test can leave one foreground
    // encode, its publication, and another idle check before the next pass.
    // During encoding the cursors remain equal: at most one empty pump runs
    // and disables itself. Short pumps around wake/publication and pass entry
    // fit the far-path allowance asserted in emit_pass. Keep Off's W unchanged.
    fn idle_encode(&self) -> u32 {
        self.encode + self.udre + if self.encode == 0 { 0 } else { self.idle_check }
    }
    pub fn window(&self) -> u32 {
        256 + (2 * self.pass_max() + self.idle_encode() + 2 * TOV_INT + LOCK + N_MIN).div_ceil(8) + 2
    }

    /// Serial scans add the same work to timer and byte passes. Byte dispatch
    /// uses the late clamp and its own bookkeeping bound for Rx and SerialLost.
    /// At Off that can be shorter than timer bookkeeping; W must cover both.
    pub fn rx_pass_max(&self) -> u32 { self.pass_max() + self.rx_scan + self.rx_bookkeep.saturating_sub(BOOKKEEP) }
    pub fn rx_redispatch(&self) -> u32 { self.rx_pass_max() + N_MIN }
    pub fn rx_window(&self) -> u32 {
        256 + (2 * self.rx_pass_max() + self.idle_encode() + 2 * (TOV_INT + self.rx_int) + LOCK + N_MIN).div_ceil(8) + 2
    }
}

const _: () = assert!(DISPATCH > L && L >= TOV_INT + 6 && LOCK < 2040);
const _: () = assert!(TOV_INT == 16);
const _: () = assert!(RX_DEFER_MASK < 2040);
/// CLI, SBI EEMPE, SBI EEPE, two halted clocks, restore SREG.
pub const EEPROM_WRITE_MASK: u32 = 1 + 2 + 2 + 2 + 1;
const _: () = assert!(EEPROM_WRITE_MASK < 2040);
const _: () = assert!(E < 256);

// Data addresses are used for LDS/STS; IN/OUT use the corresponding offset.
const SREG: u8 = 0x3f;
const TCNT0: u8 = 0x26;
const TIFR0: u8 = 0x15;
const UCSR0B: u16 = 0xc1;

/// Words in the shared pad sled: RJMP .+0 words that end in one RET ([`emit_sled`]).
pub const SLED: u32 = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class { Exact, Bounded }

/// Counts instruction cycles: exact blocks balance selects; bounded blocks
/// retain real paths and count the longer arm, including branch/jump costs.
struct Code<'a> {
    a: &'a mut Asm,
    t: u32,
    class: Class,
    /// The sled's entry labels (`Labels::sled`); padding requires the sled.
    sled: &'a [Label],
}

impl<'a> Code<'a> {
    /// Exact timing: selects balance and `to` pads to the requested total.
    fn new(a: &'a mut Asm, sled: &'a [Label]) -> Self {
        Self { a, t: 0, class: Class::Exact, sled }
    }
    fn bounded(a: &'a mut Asm, sled: &'a [Label]) -> Self {
        Self { class: Class::Bounded, ..Self::new(a, sled) }
    }
    fn i(&mut self, i: Insn) {
        self.t += u32::from(cycles(&i).0);
        self.a.emit(i);
    }
    fn seq(&mut self, insns: &[Insn]) { for &i in insns { self.i(i); } }
    fn call(&mut self, target: Label, total: u32) {
        self.a.jump(Target::Call, target);
        self.t += 4 + total;
    }
    fn jump(&mut self, target: Label) { self.a.jump(Target::Jmp, target); self.t += 3; }
    fn pad(&mut self, mut n: u32) {
        assert!(!self.sled.is_empty(), "padding requires the shared sled");
        // A CALL into the shared sled runs k of its RJMP .+0 words and its RET: 8 + 2k cycles in
        // two flash words, with no register, flag or RAM change. Its return
        // address fits the runtime's call-depth budget (Layout::new).
        while n >= 8 {
            let k = ((n - 8) / 2).min(self.sled.len() as u32 - 1);
            self.a.jump(Target::Call, self.sled[k as usize]);
            self.t += 8 + 2 * k;
            n -= 8 + 2 * k;
        }
        // RJMP to the following word: two cycles, no register or flag changes.
        for _ in 0..n / 2 { self.i(Rjmp(0)); }
        if n & 1 != 0 { self.i(Nop); }
    }
    fn to(&mut self, end: u32) {
        assert!(self.class == Class::Exact, "Code::to is not allowed in bounded code; use pad for a deliberate wait");
        assert!(self.t <= end, "cycle budget {end} is below emitted {}", self.t);
        self.pad(end - self.t);
    }
    fn select(&mut self, bit: u8, yes: impl Fn(&mut Code<'_>), no: impl Fn(&mut Code<'_>)) {
        // Count the pure emission closures before laying out either arm. No
        // instructions are executed; unresolved labels need no relocation in
        // these discarded counting assemblers.
        let mut probe = Asm::new();
        let mut counted = Code::new(&mut probe, self.sled);
        counted.class = self.class;
        yes(&mut counted);
        let yes_cycles = counted.t;
        let yes_words = counted.a.here();
        counted.t = 0;
        no(&mut counted);
        let no_cycles = counted.t;
        let no_words = counted.a.here() - yes_words;
        if self.class == Class::Bounded {
            if yes_words == 0 && no_words == 0 { return; }
            // Fixed-width jumps keep the probe's word counts valid after assembly.
            // Use the short arm first if it lets a relative branch reach the other.
            let jump_words = |words| if words == 0 { 0 } else if words <= 2047 { 1 } else { 2 };
            let reversed = no_words == 0 || (yes_words != 0
                && no_words + jump_words(yes_words) > 63
                && yes_words + jump_words(no_words) <= 63);
            let (first_words, last_words, first_cycles, last_cycles) = if reversed {
                (yes_words, no_words, yes_cycles, no_cycles)
            } else { (no_words, yes_words, no_cycles, yes_cycles) };
            let skip_words = jump_words(last_words);
            let skip_cycles = if skip_words == 0 { 0 } else { skip_words + 1 };
            let last = self.a.label();
            let done = self.a.label();
            let start = self.t;
            let (first_prefix, last_prefix) = if first_words + skip_words <= 63 {
                self.a.jump(if reversed { Target::Brbc(bit) } else { Target::Brbs(bit) }, last);
                (1, 2)
            } else {
                // The branch skips only JMP, so neither arm needs to fit BRxx.
                self.a.emit(if reversed { Brbs(bit, 2) } else { Brbc(bit, 2) });
                self.a.jump(Target::Jmp, last);
                (2, 4)
            };
            if reversed { yes(self); } else { no(self); }
            if last_words != 0 {
                self.a.jump(if skip_words == 1 { Target::Rjmp } else { Target::Jmp }, done);
            }
            self.a.bind(last);
            if reversed { no(self); } else { yes(self); }
            self.a.bind(done);
            self.t = start + (first_prefix + first_cycles + skip_cycles).max(last_prefix + last_cycles);
            return;
        }
        let total = (4 + yes_cycles).max(5 + no_cycles);
        let yes_label = self.a.label();
        let done = self.a.label();
        let start = self.t;
        // True prefix: BRBC 1 + JMP 3. False prefix: taken BRBC 2.
        self.a.emit(Brbc(bit, 2));
        self.a.jump(Target::Jmp, yes_label);
        self.t = 0;
        no(self);
        self.pad(total - 5 - no_cycles);
        self.jump(done);
        self.a.bind(yes_label);
        self.t = 0;
        yes(self);
        self.pad(total - 4 - yes_cycles);
        self.a.bind(done);
        self.t = start + total;
    }
    fn store(&mut self, addr: u16, value: u8) { self.seq(&[Ldi(26, value), Sts(addr, 26)]); }
    fn copy(&mut self, dst: u16, src: u16, len: u16) {
        // Four cycles per byte, using a caller-clobbered temporary.
        for offset in 0..len { self.seq(&[Lds(26, src + offset), Sts(dst + offset, 26)]); }
    }
    fn flash(&mut self, label: Label) {
        // LDI pair 2 + byte-address conversion 2 = four cycles.
        self.a.ldi_label(30, 31, label); self.t += 2;
        self.seq(&[Add(30, 30), Adc(31, 31)]);
    }
}

// Clock expansion after TCNT0/TIFR0 have been sampled into r20/r26: 37
// cycles, including the branchless pending-byte mask; no interrupt may land.
fn expand_clock(c: &mut Code<'_>, l: &Layout) {
    c.seq(&[Mov(21, 13), Mov(22, 14), Mov(23, 15),
        Lds(24, l.clock_hi), Lds(25, l.clock_hi + 1), Lds(27, l.clock_b31),
        Mov(30, 15), Andi(30, 0x80), Mov(31, 30), Com(31), And(31, 27),
        Bst(31, 7), Ldi(31, 0), Bld(31, 0), Add(24, 31), Adc(25, 2),
        Sts(l.clock_hi, 24), Sts(l.clock_hi + 1, 25), Sts(l.clock_b31, 30),
        Andi(26, 1), Mov(27, 20), Subi(27, 255), Sbc(27, 27), And(26, 27), Rjmp(0)]);
    // SUBI's borrow is one exactly when t != 255; SBC expands it to a mask.
    // Seven clocks including the ANDI and padding, with no conditional PCs.
    c.seq(&[Add(21, 26), Adc(22, 2), Adc(23, 2), Adc(24, 2), Adc(25, 2)]);
}

fn stamp(c: &mut Code<'_>, l: &Layout, q: &Labels) {
    // Save/restore I as well as the other status bits. Only the stamp masks.
    // Eleven clocks of wrapper, NOW48 clocks in the callee, twelve to store.
    c.seq(&[In(0, SREG), Push(0), Bclr(7)]);
    c.call(q.now48, NOW48);
    c.seq(&[Pop(0), Out(SREG, 0)]);
    for r in 20..26 { c.i(Sts(l.stamp + u16::from(r - 20), r)); }
}

fn save_args(c: &mut Code<'_>, l: &Layout) {
    // B, site and A0, then A1..A3 after the scratch byte: twenty clocks.
    for (offset, r) in [20, 21, 22, 23, 24, 25, 16].into_iter().enumerate() {
        c.i(Sts(l.args + offset as u16, r));
    }
    for r in 17..20 { c.i(Sts(l.args + u16::from(r - 9), r)); }
}

fn save_stub_args(c: &mut Code<'_>, l: &Layout, value: bool) {
    if l.records != 0 { save_args(c, l); return; }
    // Off needs the site and, for writes/duty/period, the effect's value. A's copy exists
    // only for a Fault frame's argument; the caller's A is already preserved on the stack.
    if value { for r in 20..24 { c.i(Sts(l.args + u16::from(r - 20), r)); } }
    c.seq(&[Sts(l.args + 4, 24), Sts(l.args + 5, 25)]);
}

fn record_stamp(c: &mut Code<'_>, l: &Layout) { c.copy(l.record + 1, l.stamp, 4); }

fn watch_stamp(c: &mut Code<'_>, l: &Layout, q: &Labels) {
    if l.records != 0 { stamp(c, l, q); record_stamp(c, l); }
}

fn push(c: &mut Code<'_>, l: &Layout, q: &Labels, b: &Bounds) {
    if l.records != 0 { c.call(q.push, b.push); }
}

/// Z points at the cell, r24:r25 is its site. Only X, memory and flags change.
fn fault_increment(c: &mut Code<'_>) {
    c.seq(&[Ld(26, Ptr::Z), Ldd(27, false, 1), Adiw(26, 1)]);
    c.select(0, |_| {}, |c| c.seq(&[St(Ptr::Z, 26), Std(false, 1, 27)]));
    c.seq(&[Std(false, 2, 24), Std(false, 3, 25)]);
}

const FAULT_CELL: u32 = 19;

fn fault_cell(c: &mut Code<'_>, l: &Layout, q: &Labels, code: u8) {
    let addr = l.fault_cells.iter().find(|cell| cell.code == code).expect("reachable fault has a cell").addr as u16;
    c.seq(&[Ldi(30, addr as u8), Ldi(31, (addr >> 8) as u8)]);
    if l.shared_fault { c.call(q.fault, FAULT_CELL); }
    else {
        let mut bounded = Code::bounded(c.a, &[]);
        fault_increment(&mut bounded);
        c.t += bounded.t;
    }
}

fn fault(c: &mut Code<'_>, l: &Layout, q: &Labels, code: u8) {
    if l.records == 0 {
        c.seq(&[Lds(24, l.args + 4), Lds(25, l.args + 5)]);
        fault_cell(c, l, q, code);
        return;
    }
    // Fault is two records; the continuation's tick field holds the argument.
    c.store(l.record, watch::FAULT);
    c.store(l.record + 5, code);
    c.copy(l.record + 6, l.args + 4, 2);
    c.store(l.record + 8, watch::FAULT | 0x80);
    c.copy(l.record + 9, l.args + 6, 1);
    c.copy(l.record + 10, l.args + 8, 3);
    c.store(l.record_count, 2);
}

pub(crate) fn emit(a: &mut Asm, p: &Program<'_>, tb: &Timebase, adc: Option<&Adc>, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols) -> Bounds {
    // Fixed vector locations, ending at the last vector the image enables:
    // TOV0 always, USART RX with serial, UDRE with a watch stream. Nothing else
    // is ever enabled (the ADC is polled), so later vectors need no RETI. The
    // overflow body follows the table, so its vector is the two-clock RJMP.
    let last = if p.watch != watch::Level::Off { 38 } else if p.serial.is_some() { 36 } else { 32 };
    a.jump(Target::Jmp, q.reset);
    for vector in (2..=last).step_by(2) {
        a.org(vector);
        if vector == 32 { a.jump(Target::Rjmp, q.tov); a.emit(Nop); }
        else if vector == 36 && p.serial.is_some() { a.jump(Target::Jmp, q.rx); }
        else if vector == 38 && p.watch != watch::Level::Off { a.jump(Target::Jmp, q.udre); }
        else { a.emit(Reti); a.emit(Nop); }
    }
    a.bind(q.tov); sy.tov = a.here();
    for i in [In(3, SREG), Bset(0), Adc(13, 2), Adc(14, 2), Adc(15, 2), Out(SREG, 3), Reti] { a.emit(i); }
    a.bind(q.now48); sy.now48 = a.here();
    let mut c = Code::new(a, &q.sled);
    c.seq(&[In(20, TCNT0), In(26, TIFR0)]);
    expand_clock(&mut c, l);
    c.i(Ret);
    assert_eq!(c.t, NOW48, "now48 hand count");
    sy.now48_end = c.a.here();
    emit_delay(c.a, q, sy);
    emit_sled(c.a, q, sy);
    let mut b = Bounds::new();
    if l.shared_fault {
        c.a.bind(q.fault); sy.fault = c.a.here();
        let mut counter = Code::bounded(c.a, &[]);
        fault_increment(&mut counter); counter.i(Ret);
        assert_eq!(counter.t, FAULT_CELL, "fault cell bound");
    }
    b.push = emit_push(c.a, p, l, q, sy);
    emit_rx(c.a, p, l, q, sy, &mut b);
    sy.rx_end = c.a.here();
    emit_ports(c.a, p, tb, l, q, s, sy, &mut b);
    if p.uses_adc() { emit_adc(c.a, p, adc.unwrap(), l, q, s, sy, &mut b); }
    emit_records(c.a, p, l, q, s, sy, &mut b);
    emit_shifts(c.a, p, l, q, s, sy, &mut b);
    emit_eeprom(c.a, p, l, q, s, sy, &mut b);
    if p.uses_port(ports::PWM_DUTY) { emit_duty(c.a, p, l, q, s, sy, &mut b); }
    if p.uses_port(ports::SERIAL_BYTE) { emit_serial_byte(c.a, p, l, q, s, sy, &mut b); }
    b.udre = emit_udre(c.a, p, l, q, sy);
    emit_pass(c.a, p, l, q, sy, &mut b);
    emit_reset(c.a, p, l, q, sy);
    b
}

fn emit_delay(a: &mut Asm, q: &Labels, sy: &mut Symbols) {
    a.bind(q.delay); sy.delay = a.here();
    // n is a 24-bit value in r23:r25:r24. Subtract twenty clocks of
    // non-loop overhead; q=floor((n-20)/4), remainder in r16. q>=1.
    for i in [Subi(24, DELAY_BASE as u8), Sbci(25, 0), Sbci(23, 0),
        Mov(16, 24), Andi(16, 3), Lsr(23), Ror(25), Ror(24),
        Lsr(23), Ror(25), Ror(24), Ldi(30, 0), Ldi(31, 0)] { a.emit(i); }
    // Loop: 4q-1. Skip pairs: 2+bit0 and 2+2*bit1. RET: four.
    // Put the remainder before the loop so both optional instructions can
    // straddle an overflow in the phase sweep. LPM uses zero and clobbers r0.
    a.emit(Sbrc(16, 0)); a.emit(Rjmp(0));
    a.emit(Sbrc(16, 1)); a.emit(Lpm);
    let again = a.label(); a.bind(again);
    a.emit(Sbiw(24, 1)); a.jump(Target::Brbc(1), again);
    a.emit(Ret); sy.delay_end = a.here();
}

fn emit_sled(a: &mut Asm, q: &Labels, sy: &mut Symbols) {
    // Entry k runs k RJMP .+0 words, then RET: Code::pad's long padding, shared by the runtime.
    sy.pad = a.here();
    for k in (1..q.sled.len()).rev() { a.bind(q.sled[k]); a.emit(Rjmp(0)); }
    a.bind(q.sled[0]); a.emit(Ret); sy.pad_end = a.here();
}

fn rx_pointer(c: &mut Code<'_>, l: &Layout, reg: u8, address: u16) {
    // End compare 4 + wrap at most 3 + pointer publication 4 = 11.
    let tmp = if reg == 26 { 30 } else { 26 };
    let end = l.rx_fifo + 7 * l.rx_depth;
    c.seq(&[Ldi(tmp, end as u8), Ldi(tmp + 1, (end >> 8) as u8),
        Cp(reg, tmp), Cpc(reg + 1, tmp + 1)]);
    c.select(1, |c| c.seq(&[Ldi(reg, l.rx_fifo as u8), Ldi(reg + 1, (l.rx_fifo >> 8) as u8)]), |_| {});
    c.seq(&[Sts(address, reg), Sts(address + 1, reg + 1)]);
}

fn emit_rx(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, sy: &mut Symbols, b: &mut Bounds) {
    a.bind(q.rx); sy.rx = a.here();
    if p.serial.is_none() { a.emit(Reti); return; }
    let mut c = Code::bounded(a, &[]);
    // Sixteen registers and SREG: 35 save + 39 restore/RETI.
    c.seq(&[Push(0), In(0, SREG), Push(0), Push(1)]);
    for r in (16..28).chain([30, 31]) { c.i(Push(r)); }
    // Status belongs to the current frame; UDR advances both data and flags.
    c.seq(&[Lds(0, 0xc0), Lds(16, 0xc6)]);
    sy.rx_sample = c.a.here();
    c.seq(&[In(20, TCNT0), In(26, TIFR0)]);
    expand_clock(&mut c, l);
    // r0 holds one for a DOR0-tagged frame, zero otherwise. FE0/UPE0 do
    // not discard the byte. The full arm adds its independent FIFO drop.
    c.seq(&[Bst(0, 3), Mov(0, 2), Bld(0, 0)]);
    // Sticky until byte dispatch snapshots the count and its uncertainty.
    c.seq(&[Lds(19, l.rx_overrun), Or(19, 0), Sts(l.rx_overrun, 19)]);
    c.seq(&[Lds(17, l.rx_count), Lds(18, l.rx_count + 1), Ldi(19, (l.rx_depth >> 8) as u8),
        Cpi(17, l.rx_depth as u8), Cpc(18, 19)]);
    c.select(1, |c| {
        // Full means drop the NEW byte; never touch head, tail or payloads.
        c.i(Inc(0));
    }, |c| {
        c.seq(&[Lds(26, l.rx_head), Lds(27, l.rx_head + 1), St(Ptr::XInc, 16)]);
        for r in 20..26 { c.i(St(Ptr::XInc, r)); }
        rx_pointer(c, l, 26, l.rx_head);
        c.seq(&[Subi(17, 255), Sbci(18, 255), Sts(l.rx_count, 17), Sts(l.rx_count + 1, 18)]);
        // Compare all six stamp bytes with the atomically published cut.
        // Borrow means strictly earlier; OR makes the flag sticky without
        // incrementing it (a burst must not wrap it back to zero).
        for r in 20..26 {
            c.i(Lds(19, l.rx_cut + u16::from(r - 20)));
            c.i(if r == 20 { Cp(r, 19) } else { Cpc(r, 19) });
        }
        c.seq(&[Sbc(26, 26), Lds(19, l.rx_early), Or(19, 26), Sts(l.rx_early, 19)]);
    });
    // One common saturating addition handles zero, one or two losses.
    for r in 16..20 { c.i(Lds(r, l.rx_lost + u16::from(r - 16))); }
    c.seq(&[Add(16, 0), Adc(17, 2), Adc(18, 2), Adc(19, 2)]);
    c.select(0, |c| { for r in 16..20 { c.i(Ldi(r, 255)); } }, |_| {});
    for r in 16..20 { c.i(Sts(l.rx_lost + u16::from(r - 16), r)); }
    drain_return(&mut c);
    b.rx_int = c.t + 7; // Hardware entry and vector JMP, plus the body through RETI.
}

fn emit_rx_scan(c: &mut Code<'_>, l: &Layout, sy: &mut Symbols, b: &mut Bounds) {
    let start = c.t;
    c.i(Sts(l.rx_active, 2));
    sy.rx_scan = c.a.here();
    let mask = c.t;
    c.i(Bclr(7));
    c.seq(&[Lds(16, l.rx_count), Lds(17, l.rx_count + 1), Or(16, 17),
        Lds(30, l.rx_tail), Lds(31, l.rx_tail + 1), Adiw(30, 1)]);
    for r in 20..26 { c.i(Ld(r, Ptr::ZInc)); }
    c.i(Cpi(16, 0));
    c.select(1, |c| { for r in 20..26 { c.i(Ldi(r, 255)); } }, |_| {});
    for r in 20..26 { c.i(if r == 20 { Cp(r, r - 14) } else { Cpc(r, r - 14) }); }
    // Strict < after the timer scan: a timer wins equal reception ticks.
    c.select(0, |c| {
        for r in 20..26 { c.i(Mov(r - 14, r)); }
        c.seq(&[Ldi(16, 255), Mov(12, 16)]);
    }, |_| {});
    // FIFO snapshot, selection, cut and flag reset are one masked section.
    // A queued stamp precedes a future timer due and would have won here.
    for r in 6..12 { c.i(Sts(l.rx_cut + u16::from(r - 6), r)); }
    c.i(Sts(l.rx_early, 2));
    c.i(Bset(7));
    b.rx_scan_mask = c.t - mask;
    b.rx_scan = c.t - start;
}

// Clear: LDS 2 + CPI 1 + taken BREQ 2 = 5. Set: 2 + 1 + 1 + JMP 3 = 7.
// The caller counts the clear path; the taken path leaves its cycle budget.
fn rx_check(c: &mut Code<'_>, l: &Layout, defer: Label) {
    c.seq(&[Lds(16, l.rx_early), Cpi(16, 0)]);
    c.a.emit(Brbs(1, 2)); c.a.jump(Target::Jmp, defer);
    c.t += 2;
}

fn emit_rx_bookkeep(a: &mut Asm, l: &Layout, q: &Labels, sy: &mut Symbols, entry: usize, b: &mut Bounds) {
    let mut c = Code::bounded(a, &q.sled); c.t = 7; // RX arm of the bookkeeping gate.
    sy.rx_take = c.a.here();
    let start = c.t;
    c.i(Bclr(7));
    if l.records != 0 {
        c.call(q.now48, NOW48);
        for r in 20..26 { c.i(Sts(l.stamp + u16::from(r - 20), r)); }
    }
    c.seq(&[Lds(30, l.rx_tail), Lds(31, l.rx_tail + 1), Ld(16, Ptr::Z),
        Sts(l.rx_byte, 16), Adiw(30, 7)]);
    rx_pointer(&mut c, l, 30, l.rx_tail);
    c.seq(&[Lds(16, l.rx_count), Lds(17, l.rx_count + 1), Subi(16, 1), Sbci(17, 0),
        Sts(l.rx_count, 16), Sts(l.rx_count + 1, 17)]);
    c.copy(l.rx_report, l.rx_lost, 4);
    for offset in 0..4 { c.i(Sts(l.rx_lost + offset, 2)); }
    c.copy(l.rx_report_overrun, l.rx_overrun, 1);
    c.i(Sts(l.rx_overrun, 2));
    c.store(l.rx_active, 1);
    // Dispatch starts here: the current byte no longer counts against the
    // pending FIFO. Capture/reset count and overrun atomically with it;
    // Fault reporting is this handler's prelude, before its program body.
    // Off omits the report stamp; reception stamps still come from RX.
    c.i(Bset(7));
    b.rx_take = c.t - start;
    if l.records == 0 {
        // One fault per report, just as in the stream; the lost-byte count is its argument,
        // not the number of fault occurrences. Link zero is the site for either loss code.
        for r in 16..20 { c.i(Lds(r, l.rx_report + u16::from(r - 16))); }
        c.seq(&[Or(16, 17), Or(16, 18), Or(16, 19), Cpi(16, 0)]);
        c.select(1, |_| {}, |c| {
            c.seq(&[Ldi(24, 0), Ldi(25, 0), Lds(26, l.rx_report_overrun), Cpi(26, 0)]);
            c.select(1, |c| fault_cell(c, l, q, watch::FAULT_SERIAL_LOST),
                |c| fault_cell(c, l, q, watch::FAULT_SERIAL_LOST_AT_LEAST));
        });
        c.a.ldi_label(30, 31, q.dispatch[entry]); c.t += 2;
        c.i(Ijmp);
        b.rx_bookkeep = c.t;
        return;
    }
    c.copy(l.record + 1, l.due, 4); c.store(l.record, watch::RX);
    c.copy(l.record + 5, l.rx_byte, 1);
    c.i(Sts(l.record + 6, 2)); c.i(Sts(l.record + 7, 2));
    c.store(l.record_count, 1); push(&mut c, l, q, b);
    record_stamp(&mut c, l); c.store(l.record, watch::FAULT);
    c.seq(&[Lds(26, l.rx_report_overrun), Cpi(26, 0)]);
    c.select(1, |c| c.i(Ldi(26, watch::FAULT_SERIAL_LOST)),
        |c| c.i(Ldi(26, watch::FAULT_SERIAL_LOST_AT_LEAST)));
    c.i(Sts(l.record + 5, 26));
    // site carries the hardware link (zero), arg carries the u32 lost count.
    c.i(Sts(l.record + 6, 2)); c.i(Sts(l.record + 7, 2));
    c.store(l.record + 8, watch::FAULT | 0x80); c.copy(l.record + 9, l.rx_report, 4);
    for r in 16..20 { c.i(Lds(r, l.rx_report + u16::from(r - 16))); }
    c.seq(&[Or(16, 17), Or(16, 18), Or(16, 19), Cpi(16, 0)]);
    c.select(1, |c| c.i(Sts(l.record_count, 2)), |c| c.store(l.record_count, 2));
    push(&mut c, l, q, b);
    c.a.ldi_label(30, 31, q.dispatch[entry]); c.t += 2;
    c.i(Ijmp);
    b.rx_bookkeep = c.t;
}

fn emit_serial_byte(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    a.bind(s.serial_byte); sy.serial_byte = a.here();
    let mut c = Code::bounded(a, &q.sled);
    if p.watch == watch::Level::Off {
        let invalid = |c: &mut Code<'_>| {
            fault_cell(c, l, q, watch::FAULT_SERIAL);
            for r in 16..20 { c.i(Ldi(r, 255)); }
        };
        if p.serial.is_some() {
            c.seq(&[Or(16, 17), Or(16, 18), Or(16, 19),
                Lds(17, l.rx_active), Subi(17, 1), Or(16, 17), Cpi(16, 0)]);
            c.select(1, |c| {
                c.i(Lds(16, l.rx_byte));
                for r in 17..20 { c.i(Ldi(r, 0)); }
            }, invalid);
        } else { invalid(&mut c); }
        c.i(Ret); b.serial_byte = c.t;
        return;
    }
    // Keep all four link bytes for both validity and the fault argument.
    for r in 16..20 { c.i(Sts(l.args + u16::from(r - 16), r)); }
    c.seq(&[Sts(l.args + 4, 24), Sts(l.args + 5, 25)]);
    watch_stamp(&mut c, l, q);
    c.store(l.record, watch::FAULT); c.store(l.record + 5, watch::FAULT_SERIAL);
    c.copy(l.record + 6, l.args + 4, 2); c.store(l.record + 8, watch::FAULT | 0x80);
    c.copy(l.record + 9, l.args, 4);
    for r in 16..20 { c.i(Lds(r, l.args + u16::from(r - 16))); }
    c.seq(&[Or(16, 17), Or(16, 18), Or(16, 19)]);
    if p.serial.is_some() {
        c.seq(&[Lds(17, l.rx_active), Subi(17, 1), Or(16, 17)]);
    } else { c.i(Ldi(16, 1)); }
    c.i(Cpi(16, 0));
    c.select(1, |c| {
        c.i(Sts(l.record_count, 2));
        c.i(Lds(20, l.rx_byte));
        c.i(Sts(l.args, 20));
        for offset in 1..4 { c.i(Sts(l.args + offset, 2)); }
    }, |c| {
        c.store(l.record_count, 2); c.i(Ldi(20, 255));
        for offset in 0..4 { c.i(Sts(l.args + offset, 20)); }
    });
    push(&mut c, l, q, b);
    for r in 16..20 { c.i(Lds(r, l.args + u16::from(r - 16))); }
    c.i(Ret); b.serial_byte = c.t;
}

fn ring_append(c: &mut Code<'_>, l: &Layout) {
    // Z points at the source. Pointer 9 + copy 32 + wrapped head advance 6
    // + RET 4 = 51 clocks; the caller adds two LDIs and CALL's four clocks.
    // Head is published only while UDRIE is disabled by the producer.
    c.seq(&[Lds(16, l.head), Ldi(18, 8), Mul(16, 18),
        Ldi(26, l.ring as u8), Ldi(27, (l.ring >> 8) as u8), Add(26, 0), Adc(27, 1)]);
    for _ in 0..8 { c.seq(&[Ld(20, Ptr::ZInc), St(Ptr::XInc, 20)]); }
    c.seq(&[Inc(16), Cpi(16, l.records)]);
    c.select(1, |c| c.i(Ldi(16, 0)), |_| {});
    c.i(Sts(l.head, 16)); c.i(Ret);
}

fn append_record(c: &mut Code<'_>, append: Label, bound: u32, source: u16) {
    c.seq(&[Ldi(30, source as u8), Ldi(31, (source >> 8) as u8)]);
    c.call(append, bound);
}

fn lost_records(c: &mut Code<'_>, l: &Layout, append: Label, bound: u32) {
    // Two staged records and two calls to append, then four clears.
    // The sink is a staging record; append each half before reusing it.
    c.store(l.sink, watch::LOST);
    c.copy(l.sink + 1, l.lost_last, 4);
    c.copy(l.sink + 5, l.lost, 3);
    append_record(c, append, bound, l.sink);
    c.store(l.sink, watch::LOST | 0x80);
    c.copy(l.sink + 1, l.lost_first, 4);
    c.copy(l.sink + 5, l.lost + 3, 1);
    append_record(c, append, bound, l.sink);
    for offset in 0..4 { c.i(Sts(l.lost + offset, 2)); }
}

fn emit_push(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, sy: &mut Symbols) -> u32 {
    if p.watch == watch::Level::Off { return 0; }
    // Emit the shared local callee first so its RET-inclusive bound is known.
    let append = a.label();
    a.bind(append);
    let append_bound = {
        let mut c = Code::bounded(a, &q.sled);
        ring_append(&mut c, l);
        c.t
    };
    a.bind(q.push); sy.push = a.here();
    let mut c = Code::bounded(a, &q.sled);
    c.seq(&[Lds(16, l.head), Lds(17, l.tail), Sub(16, 17)]);
    c.select(0, |c| c.i(Subi(16, 0u8.wrapping_sub(l.records))), |_| {});
    c.seq(&[Ldi(18, l.records - 1), Sub(18, 16), Lds(19, l.record_count)]);
    for r in 20..24 { c.i(Lds(r, l.lost + u16::from(r - 20))); }
    c.seq(&[Mov(24, 20), Or(24, 21), Or(24, 22), Or(24, 23), Cpi(24, 0)]);
    c.select(1, |_| {}, |c| c.i(Subi(19, 254)));
    c.i(Cp(18, 19));
    c.select(0, |c| {
        c.seq(&[Lds(19, l.record_count), Cpi(19, 0)]);
        c.select(1, |_| {}, |c| {
            c.i(Cpi(24, 0));
            c.select(1, |c| c.copy(l.lost_first, l.record + 1, 4), |_| {});
            // Saturate at 0x7fffffff and retain the saturation flag thereafter.
            c.i(Bst(23, 7));
            c.select(6, |c| { for r in 20..24 { c.i(Ldi(r, 255)); } }, |c| {
                c.seq(&[Subi(20, 255), Sbci(21, 255), Sbci(22, 255), Sbci(23, 255), Bst(23, 7)]);
                c.select(6, |c| { for r in 20..24 { c.i(Ldi(r, 255)); } }, |_| {});
            });
            for r in 20..24 { c.i(Sts(l.lost + u16::from(r - 20), r)); }
            c.copy(l.lost_last, l.record + 1, 4);
        });
    }, |c| {
        c.i(Cpi(24, 0));
        c.select(1, |_| {}, |c| lost_records(c, l, append, append_bound));
        c.seq(&[Lds(19, l.record_count), Cpi(19, 0)]);
        c.select(1, |_| {}, |c| {
            append_record(c, append, append_bound, l.record);
            c.seq(&[Lds(19, l.record_count), Cpi(19, 2)]);
            c.select(1, |c| append_record(c, append, append_bound, l.record + 8), |_| {});
        });
    });
    c.i(Ret);
    c.t
}

fn role_lookup(c: &mut Code<'_>, q: &Labels) {
    pin_lookup(c, q.roles);
}

fn pin_lookup(c: &mut Code<'_>, table: Label) {
    // Clamp the full-width pin to the unused sentinel, then load its row.
    c.seq(&[Mov(24, 16), Cpi(24, 20), Cpc(17, 2), Cpc(18, 2), Cpc(19, 2)]);
    c.select(0, |_| {}, |c| c.i(Ldi(24, 20)));
    c.flash(table);
    c.seq(&[Ldi(25, 4), Mul(24, 25), Add(30, 0), Adc(31, 1)]);
    for r in 20..24 { c.i(LpmZInc(r)); }
}

fn emit_adc(a: &mut Asm, p: &Program<'_>, adc: &Adc, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    a.bind(s.adc_convert); sy.adc_convert = a.here();
    let mut c = Code::new(a, &q.sled);
    // ADMUX 2 + disable 2 + LDI 1 + enable/start/W1C 2 = 7 clocks.
    // ADPS=log2(div); ADIE stays clear. The runtime never sets PRR, so PRADC stays zero.
    c.seq(&[Sts(0x7c, 20), Sts(0x7a, 2),
        Ldi(20, 0xd0 | adc.div.trailing_zeros() as u8), Sts(0x7a, 20)]);
    // Completion is div/2 + 25*div after that STS, sample-and-hold at 14*div.
    assert_eq!(c.t + 14 * adc.div, adc.sample(), "adc sample hand count");
    // Three LDIs + CALL + delay(n) retain the wait's one-clock completion margin.
    // At clk/2 n=45, above N_MIN; at clk/128 n=3258, well inside the 16-bit loop.
    let n = adc.wait() - 3 - 4;
    assert!(n >= N_MIN && (n - DELAY_BASE) / 4 <= u32::from(u16::MAX), "ADC delay count fits");
    // Entry values live in SRAM after flush; r0 is scratch, r2 is zero. The computed
    // stub saved its arguments and needs only the result. Delay's r0/r16/r23..r25/Z
    // clobbers are free at both call sites. RX/UDRE preserve them, and TOV leaves them
    // alone, so interrupt service can only lengthen the wait, without changing I.
    c.seq(&[Ldi(24, n as u8), Ldi(25, (n >> 8) as u8), Ldi(23, (n >> 16) as u8)]);
    c.call(q.delay, n);
    c.seq(&[Lds(16, 0x78), Lds(17, 0x79)]); // ADCL first locks both data bytes.
    // ADIF is W1C: clear the completion flag while turning ADEN off, leaving ADCSRA=0.
    c.seq(&[Ldi(20, 0x10), Sts(0x7a, 20), Ldi(18, 0), Ldi(19, 0), Ret]);
    // Prefix 7 + wait + data 4 + shutdown 3 + zero extension 2 + RET 4.
    assert_eq!(c.t, adc.convert(), "adc_convert hand count");
    b.adc_convert = c.t; b.adc_sample = adc.sample();

    if !p.uses_port(ports::ADC_READ) { return; }
    c.a.bind(s.adc); sy.adc = c.a.here();
    let mut c = Code::bounded(c.a, &q.sled);
    save_stub_args(&mut c, l, false);
    // Clamp to the sentinel before looking up the declared analog channel.
    c.seq(&[Mov(24, 16), Cpi(24, 22), Cpc(17, 2), Cpc(18, 2), Cpc(19, 2)]);
    c.select(0, |_| {}, |c| c.i(Ldi(24, 22)));
    c.flash(q.analog_pins);
    c.seq(&[Add(30, 24), Adc(31, 2), LpmZ(20), Cpi(20, 0)]);
    // No analog read record at any watch level: the protocol has no ten-bit record kind.
    c.select(1, |c| {
        watch_stamp(c, l, q); fault(c, l, q, watch::FAULT_PIN);
        push(c, l, q, b);
        for r in 16..20 { c.i(Ldi(r, 0)); }
    }, |c| c.call(s.adc_convert, adc.convert()));
    c.i(Ret);
    b.adc = c.t;
}

fn emit_duty(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    a.bind(s.duty); sy.duty = a.here();
    let mut c = Code::bounded(a, &q.sled);
    // Preserve A across the stamp, lookup and watch push.
    for r in 16..20 { c.i(Push(r)); }
    save_stub_args(&mut c, l, true); watch_stamp(&mut c, l, q);
    pin_lookup(&mut c, q.pwm_pins);
    // Clamp signed i32: high bytes nonzero saturate up, sign overrides down.
    c.seq(&[Lds(17, l.args), Lds(24, l.args + 1), Lds(25, l.args + 2), Lds(26, l.args + 3),
        Mov(27, 24), Or(27, 25), Or(27, 26), Cpi(27, 0)]);
    c.select(1, |_| {}, |c| c.i(Ldi(17, 255)));
    c.i(Bst(26, 7)); c.select(6, |c| c.i(Ldi(17, 0)), |_| {});
    if p.watch != watch::Level::Off { c.i(Sts(l.args + 7, 17)); }
    if scaled_pwm(p).is_some() {
        // Exact rounded OCR from the shared Timer1 flash table; eight-bit
        // timers use the unscaled level.
        c.flash(q.pwm_levels);
        c.seq(&[Mov(18, 17), Ldi(19, 0), Add(18, 18), Adc(19, 19),
            Add(30, 18), Adc(31, 19), LpmZInc(18), LpmZ(19), Cpi(21, 0)]);
        c.select(1, |c| c.seq(&[Mov(18, 17), Ldi(19, 0)]), |_| {});
    } else { c.seq(&[Mov(18, 17), Ldi(19, 0)]); }
    // Only declared PWM pins touch hardware. Timer1 writes TEMP first, then
    // commits on OCR low; neither runtime ISR touches Timer1 16-bit registers.
    c.i(Cpi(20, 0));
    c.select(1, |_| {}, |c| {
        c.i(Cpi(21, 0));
        c.select(1, |_| {}, |c| {
            c.seq(&[Mov(30, 21), Ldi(31, 0), St(Ptr::Z, 19)]);
        });
        c.seq(&[Mov(30, 20), Ldi(31, 0)]);
        // Connected nonzero Fast duties only write OCR. Zero leaves OCR alone
        // and disconnects COM, exposing PORT low immediately. Reconnect writes
        // OCR first, then COM: the frozen latch is exposed until the next BOTTOM.
        // Phase-correct COM stays connected, including zero; only OCR changes.
        c.i(Cpi(22, 0));
        c.select(1, |_| {}, |c| {
            c.seq(&[Lds(24, 0x44), Mov(25, 24), And(25, 22), Cpi(17, 0)]);
            c.select(1, |c| c.seq(&[Com(22), And(24, 22), Mov(18, 24), Ldi(30, 0x44)]), |c| {
                c.i(Cp(25, 23));
                c.select(1, |_| {}, |c| c.seq(&[St(Ptr::Z, 18), Or(24, 23), Mov(18, 24), Ldi(30, 0x44)]));
            });
        });
        c.i(St(Ptr::Z, 18));
    });
    // The nonempty arm ends in the one-word effect store. Its completed store
    // is also this select's worst case; the invalid-pin arm does nothing.
    sy.duty_effect = c.a.here() - 1; b.duty_effect = c.t;
    c.i(Cpi(20, 0));
    c.select(1, |c| fault(c, l, q, watch::FAULT_PIN), |c| {
        if p.watch == watch::Level::Off { return; }
        if p.watch == watch::Level::Faults { c.store(l.record_count, 0); return; }
        c.store(l.record, watch::DUTY);
        c.copy(l.record + 5, l.args + 6, 1);
        c.copy(l.record + 6, l.args + 7, 1);
        c.copy(l.record + 7, l.args + 4, 1);
        c.store(l.record + 8, watch::DUTY | 0x80);
        c.copy(l.record + 9, l.args + 5, 1);
        c.store(l.record_count, 2);
    });
    push(&mut c, l, q, b);
    for r in (16..20).rev() { c.i(Pop(r)); }
    c.i(Ret); b.duty = c.t;
}

fn port_record(c: &mut Code<'_>, l: &Layout, kind: u8) {
    // Pack the level into the pin byte and retain the two-byte source site.
    c.store(l.record, kind);
    c.seq(&[Lds(24, l.args + 7), Cpi(24, 0)]);
    c.select(1, |c| c.i(Ldi(24, 0)), |c| c.i(Ldi(24, 128)));
    c.seq(&[Or(24, 16), Sts(l.record + 5, 24)]);
    c.copy(l.record + 6, l.args + 4, 2);
    c.store(l.record_count, 1);
}

/// r16 is the validated bit count, r20..r23 the value. Only caller-clobbered
/// registers change; interrupts may delay edges but cannot alter these registers.
fn shift_effect(c: &mut Code<'_>, pair: (u8, u8), spi: bool) {
    let gpio = |pin| {
        let pin = chip::by_number(u32::from(pin)).expect("declared shift output");
        (chip::pin_register(pin.port) as u8 - 0x20 + 2, pin.bit)
    };
    let (data, clock) = (gpio(pair.0), gpio(pair.1));
    let start = c.t;
    c.i(Cbi(clock.0, clock.1));
    if spi { c.i(Mov(0, 20)); } // Retain bit zero across alignment and byte shifts.
    c.i(Mov(17, 16));
    if spi { c.i(Andi(17, 7)); }
    c.seq(&[Ldi(18, 32), Sub(18, 16)]);
    // Align whole bytes first: a 24-bit call needs only one byte move, not eight
    // four-register shifts. Then at most seven single-bit shifts remain.
    let (bytes, bits, align, aligned) = (c.a.label(), c.a.label(), c.a.label(), c.a.label());
    c.a.bind(bytes);
    c.i(Cpi(18, 8)); c.a.jump(Target::Brbs(0), bits);
    c.seq(&[Mov(23, 22), Mov(22, 21), Mov(21, 20), Mov(20, 2), Subi(18, 8)]);
    c.a.jump(Target::Rjmp, bytes);
    c.a.bind(bits);
    c.i(Cpi(18, 0)); c.a.jump(Target::Brbs(1), aligned);
    c.a.bind(align);
    c.seq(&[Add(20, 20), Adc(21, 21), Adc(22, 22), Adc(23, 23), Dec(18)]);
    c.a.jump(Target::Brbc(1), align);
    c.a.bind(aligned);
    let (bit, shifted) = (c.a.label(), c.a.label());
    c.i(Cpi(17, 0)); c.a.jump(Target::Brbs(1), shifted);
    c.a.bind(bit);
    if spi {
        // SPE is still off during the leading bits. Each skip discards one word.
        c.seq(&[Sbrc(23, 7), Sbi(data.0, data.1), Sbrs(23, 7), Cbi(data.0, data.1),
            Sbi(clock.0, clock.1), Cbi(clock.0, clock.1),
            Add(20, 20), Adc(21, 21), Adc(22, 22), Adc(23, 23)]);
    } else {
        c.seq(&[Add(20, 20), Adc(21, 21), Adc(22, 22), Adc(23, 23)]);
        c.select(0, |c| c.i(Sbi(data.0, data.1)), |c| c.i(Cbi(data.0, data.1)));
        c.seq(&[Sbi(clock.0, clock.1), Cbi(clock.0, clock.1)]);
    }
    c.i(Dec(17)); c.a.jump(Target::Brbc(1), bit);
    c.a.bind(shifted);
    if spi {
        let (byte, poll, done) = (c.a.label(), c.a.label(), c.a.label());
        c.seq(&[Lsr(16), Lsr(16), Lsr(16), Cpi(16, 0)]);
        c.a.jump(Target::Brbs(1), done); // A sub-byte call needs no SPI ownership.
        c.seq(&[Ldi(18, 1), Out(0x2d, 18), Ldi(18, 0x50), Out(0x2c, 18)]);
        c.a.bind(byte);
        c.i(Out(0x2e, 23));
        c.a.bind(poll);
        c.seq(&[In(18, 0x2d), Sbrs(18, 7)]); c.a.jump(Target::Rjmp, poll);
        c.seq(&[Mov(23, 22), Mov(22, 21), Mov(21, 20), Dec(16)]);
        c.a.jump(Target::Brbc(1), byte);
        // Each following SPDR write clears the preceding polled SPIF. The last
        // byte needs an explicit read, or a back-to-back call could see stale SPIF.
        c.seq(&[In(18, 0x2e), Sbrc(0, 0), Sbi(data.0, data.1), Sbrs(0, 0), Cbi(data.0, data.1),
            Cbi(clock.0, clock.1), Out(0x2c, 2)]);
        // SPE-off releases MOSI/SCK immediately to PORT: preload both latches
        // above while SPI still owns the pins, so neither pin changes on release.
        c.a.bind(done);
    }
    // Correlate alignment, leading bits and bytes instead of adding independent
    // loop maxima. A mixed call can exceed the four-byte path; cover all 1..=32.
    // Alignment: 9 per byte plus exit 3; zero residual 3, else 7*n+1.
    // Bit loop: zero 3, else 16*n+1 (bit-bang's longer data arm).
    // SPI poll: four failed IN/SBRS/RJMP rounds (4 each), then IN/SBRS (3).
    // SPIF arrives 16 clocks after OUT; reads are at +1,+5,+9,+13,+17.
    c.t = start + (1..=32).map(|n| {
        let align = 32 - n;
        let leading = if spi { n % 8 } else { n };
        let prefix = if spi { 7 } else { 5 };
        prefix + 9 * (align / 8) + 3 + if align % 8 == 0 { 3 } else { 7 * (align % 8) + 1 }
            + if leading == 0 { 3 } else { 16 * leading + 1 }
            + if !spi { 0 } else if n < 8 { 6 } else { 26 * (n / 8) + 17 }
    }).max().unwrap();
}

fn emit_shifts(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    for &(pair, label) in &s.shifts {
        a.bind(label);
        let start = a.here();
        let spi = p.shift_spi(pair);
        let mut c = Code::bounded(a, &q.sled);
        // Compare (low byte - 1) against 32 and require all upper bytes zero.
        // Keep the original signed argument intact for the fault path.
        c.seq(&[Mov(26, 16), Subi(26, 1), Cpi(26, 32), Cpc(17, 2), Cpc(18, 2), Cpc(19, 2)]);
        c.select(0, |c| {
            if p.watch == watch::Level::Ports {
                for r in 20..26 { c.i(Sts(l.args + u16::from(r - 20), r)); }
                c.i(Sts(l.args + 6, 16));
            }
            shift_effect(c, pair, spi);
            if p.watch == watch::Level::Ports {
                // Stamp and publish only after the pins have reached their final levels.
                watch_stamp(c, l, q);
                c.store(l.record, watch::SHIFT);
                c.store(l.record + 5, pair.0); c.store(l.record + 6, pair.1);
                c.copy(l.record + 7, l.args + 6, 1);
                c.store(l.record + 8, watch::SHIFT | 0x80);
                c.copy(l.record + 9, l.args, 4);
                c.copy(l.record + 13, l.args + 4, 2);
                c.store(l.record + 15, 0); c.store(l.record_count, 2);
                push(c, l, q, b);
            }
        }, |c| {
            save_stub_args(c, l, false); watch_stamp(c, l, q);
            fault(c, l, q, watch::FAULT_SHIFT_BITS); push(c, l, q, b);
        });
        c.i(Ret);
        if spi {
            sy.shift_spi = start; sy.shift_spi_end = c.a.here(); b.shift_spi = c.t;
        } else {
            sy.shift_bitbang.push((pair, start, c.a.here())); b.shift_bitbang = b.shift_bitbang.max(c.t);
        }
    }
}

/// No ISR uses the EEPROM registers. Poll with interrupts enabled (or with
/// the caller's mask intact); only the write-enable window needs a mask.
fn emit_eeprom(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    const EECR: u8 = 0x1f;
    for read in [true, false] {
        if !p.uses_port(if read { ports::EEPROM_READ } else { ports::EEPROM_WRITE }) { continue; }
        let profile = crate::chip::profile(p.f_clk);
        let clocks = (u64::from(profile.eeprom_write_ns) * u64::from(p.f_clk)).div_ceil(1_000_000_000);
        b.eeprom_wait = (clocks.div_ceil(3) * 3) as u32;
        a.bind(if read { s.eeprom_read } else { s.eeprom_write });
        let start = a.here();
        let mut c = Code::bounded(a, &q.sled);
        // Unsigned 32-bit addr < 1024: negative and nonzero upper bytes fail.
        // Keep the original address intact for the fault's signed argument.
        c.seq(&[Ldi(26, 4), Cpi(16, 0), Cpc(17, 26), Cpc(18, 2), Cpc(19, 2)]);
        c.select(0, |c| {
            if p.watch == watch::Level::Ports {
                c.seq(&[Sts(l.args, 16), Sts(l.args + 1, 17),
                    Sts(l.args + 4, 24), Sts(l.args + 5, 25)]);
                if !read { c.i(Sts(l.args + 2, 20)); }
            }
            let poll = c.a.label(); c.a.bind(poll);
            c.a.emit(Sbic(EECR, 1)); c.a.jump(Target::Rjmp, poll);
            c.t += b.eeprom_wait + 2; // Three per busy round, two for the final skip.
            // Clear EEPM and EERIE only after the previous write has finished.
            c.seq(&[Out(EECR, 2), Out(0x22, 17), Out(0x21, 16)]);
            if read {
                c.seq(&[Sbi(EECR, 0), In(16, 0x20)]); c.t += 4;
                if p.watch == watch::Level::Ports { c.i(Sts(l.args + 2, 16)); }
            } else {
                c.seq(&[Out(0x20, 20), In(0, SREG)]);
                let mask = c.t;
                c.seq(&[Bclr(7), Sbi(EECR, 2), Sbi(EECR, 1), Out(SREG, 0)]);
                c.t += 2;
                assert_eq!(c.t - mask, EEPROM_WRITE_MASK, "EEPROM write mask hand count");
            }
            if p.watch == watch::Level::Ports {
                watch_stamp(c, l, q);
                let kind = if read { watch::EEPROM_READ } else { watch::EEPROM_WRITE };
                c.store(l.record, kind);
                c.copy(l.record + 5, l.args, 3); // Address then byte.
                c.store(l.record + 8, kind | 0x80);
                c.copy(l.record + 9, l.args + 4, 2); // Site in the continuation.
                c.store(l.record_count, 2);
                push(c, l, q, b);
                if read { c.i(Lds(16, l.args + 2)); }
            }
        }, |c| {
            save_stub_args(c, l, false); watch_stamp(c, l, q);
            fault(c, l, q, watch::FAULT_EEPROM_ADDR); push(c, l, q, b);
            if read { c.i(Ldi(16, 0)); }
        });
        if read { for r in 17..20 { c.i(Ldi(r, 0)); } }
        c.i(Ret);
        if read {
            sy.eeprom_read = start; sy.eeprom_read_end = c.a.here(); b.eeprom_read = c.t;
        } else {
            sy.eeprom_write = start; sy.eeprom_write_end = c.a.here(); b.eeprom_write = c.t;
        }
    }
}

fn emit_records(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    for read in [false, true] {
        if !p.records_port(if read { ports::PIN_READ } else { ports::PIN_WRITE }) { continue; }
        a.bind(if read { s.record_read } else { s.record_write });
        if read { sy.record_read = a.here(); } else { sy.record_write = a.here(); }
        let mut c = Code::bounded(a, &q.sled);
        // The effect has already happened. Keep only the record arguments across the stamp.
        c.seq(&[Sts(l.args + 6, 16), Sts(l.args + 7, 20),
            Sts(l.args + 4, 24), Sts(l.args + 5, 25)]);
        watch_stamp(&mut c, l, q);
        c.i(Lds(16, l.args + 6));
        port_record(&mut c, l, if read { watch::READ } else { watch::WRITE });
        push(&mut c, l, q, b);
        if read {
            c.i(Lds(16, l.args + 7));
            for r in 17..20 { c.i(Ldi(r, 0)); }
        }
        c.i(Ret);
        if read { b.record_read = c.t; } else { b.record_write = c.t; }
    }
}

fn emit_ports(a: &mut Asm, p: &Program<'_>, tb: &Timebase, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    for read in [false, true] {
        if !p.uses_port(if read { ports::PIN_READ } else { ports::PIN_WRITE }) { continue; }
        a.bind(if read { s.read } else { s.write });
        if read { sy.read = a.here(); } else { sy.write = a.here(); }
        let mut c = Code::bounded(a, &q.sled);
        // Eight clocks each to preserve and restore A across agent push.
        for r in 16..20 { c.i(Push(r)); }
        save_stub_args(&mut c, l, !read);
        watch_stamp(&mut c, l, q);
        role_lookup(&mut c, q);
        if read {
            c.i(Cpi(22, 0));
            c.select(1, |c| {
                c.i(Sts(l.args + 7, 2)); fault(c, l, q, watch::FAULT_PIN);
            }, |c| {
                c.seq(&[Mov(30, 23), Ldi(31, 0), Ld(24, Ptr::Z), And(24, 20), Cpi(24, 0)]);
                c.select(1, |c| c.i(Ldi(24, 0)), |c| c.i(Ldi(24, 1)));
                c.i(Sts(l.args + 7, 24));
                if p.watch == watch::Level::Faults { c.store(l.record_count, 0); }
                else if p.watch == watch::Level::Ports { port_record(c, l, watch::READ); }
            });
        } else {
            c.i(Cpi(22, 2));
            c.select(1, |c| {
                c.seq(&[Mov(30, 21), Ldi(31, 0), Lds(24, l.args)]);
                if p.watch != watch::Level::Off { c.i(Sts(l.args + 7, 24)); }
                c.i(Cpi(24, 0));
                c.select(1, |c| c.i(Ldi(25, 0)), |c| c.i(Mov(25, 20)));
                // No interrupt writes PORT registers (PWM is hardware; handlers never nest),
                // so the computed-pin read/modify/write needs no interrupt mask.
                c.seq(&[Com(20), Ld(21, Ptr::Z), And(21, 20), Or(21, 25), St(Ptr::Z, 21)]);
            }, |_| {});
            // The nonempty arm's final one-word store is the longest path to
            // the effect; an invalid pin skips the whole read/modify/write.
            sy.write_effect = c.a.here() - 1; b.write_effect = c.t;
            c.i(Cpi(22, 2));
            c.select(1, |c| {
                if p.watch == watch::Level::Faults { c.store(l.record_count, 0); }
                else if p.watch == watch::Level::Ports { port_record(c, l, watch::WRITE); }
            }, |c| fault(c, l, q, watch::FAULT_PIN));
        }
        push(&mut c, l, q, b);
        for r in (16..20).rev() { c.i(Pop(r)); }
        if read {
            c.i(Lds(16, l.args + 7));
            for r in 17..20 { c.i(Ldi(r, 0)); }
        }
        c.i(Ret);
        if read { b.read = c.t; } else { b.write = c.t; }
    }
    let mut c = Code::bounded(a, &q.sled);
    if p.uses_port(ports::CLOCK_US) {
        c.a.bind(s.clock_us); sy.clock_us = c.a.here();
        stamp(&mut c, l, q);
        clock_value(&mut c, l.stamp, tb.us);
        c.i(Ret); b.clock_us = c.t;
    }
    if p.uses_port(ports::CLOCK_MS) {
        c.a.bind(s.clock_ms); sy.clock_ms = c.a.here();
        c.t = 0; stamp(&mut c, l, q);
        clock_value(&mut c, l.stamp, tb.ms);
        c.i(Ret); b.clock_ms = c.t;
    }
    emit_timer_stubs(c.a, p, l, q, s, sy, b);
    if p.uses_port(ports::LOOP_CAP) { emit_loop_cap(c.a, l, q, s, sy, b); }
}

fn clock_value(c: &mut Code<'_>, stamp: u16, (n, m): (u32, u32)) {
    if n == 1 && m.is_power_of_two() {
        let shifts = m.trailing_zeros();
        if shifts == 1 {
            // Keep the original 16 MHz clock_us instructions: five shifts
            // include bit 32 in the returned low 32 bits; two MOVWs.
            c.seq(&[Lsr(24), Ror(23), Ror(22), Ror(21), Ror(20), Movw(16, 20), Movw(18, 22)]);
            return;
        }
        let bytes = (32 + shifts).div_ceil(8).min(6) as u8;
        for _ in 0..shifts {
            c.i(Lsr(20 + bytes - 1));
            for r in (20..20 + bytes - 1).rev() { c.i(Ror(r)); }
        }
        c.seq(&[Movw(16, 20), Movw(18, 22)]);
        return;
    }

    // Emitter::port keeps values in SRAM, flush has no register cache, and
    // result consumes r16..r19. Like helpers.rs we may clobber r0/r1,
    // r16..r27 and Z; neither r0 nor r1 is a preserved/zero register. r2 is
    // zero. Never touch r3..r15 or Y, including the overflow ISR's r3 and
    // r13..r15. RX/UDRE save all of our scratch registers and leave stamp
    // alone, so the multiply/divide remains interruptible after the stamp.
    let numerator_bytes = (32 - n.leading_zeros()).div_ceil(8) as usize;
    let width = if n == 1 { 6 } else { 6 + numerator_bytes };
    let product = &[20, 21, 22, 23, 24, 25, 16, 17, 18][..width];
    if n != 1 {
        // Six stamped bytes times at most three constant bytes. Carry runs
        // through every remaining product limb, including zero high limbs.
        for &r in product { c.i(Ldi(r, 0)); }
        for j in 0..numerator_bytes {
            let byte = (n >> (8 * j)) as u8;
            if byte == 0 { continue; } // Timebase constant, never runtime data.
            c.i(Ldi(30, byte));
            for i in 0..6 {
                c.seq(&[Lds(31, stamp + i as u16), Mul(31, 30), Add(product[i + j], 0)]);
                for (k, &r) in product[i + j + 1..].iter().enumerate() {
                    c.i(Adc(r, if k == 0 { 1 } else { 2 }));
                }
            }
        }
    }

    // Before each shift remainder < m; allocate enough bytes for 2*m,
    // including the 33rd bit when m reaches the upper half of u32.
    let remainder_bytes = (33 - m.leading_zeros()).div_ceil(8) as usize;
    let remainder = &[26, 27, 19, 30, 31][..remainder_bytes];
    let bits = (8 * width) as u32;
    if remainder_bytes <= 2 {
        for &r in remainder { c.i(Ldi(r, 0)); }
        for i in 0..remainder_bytes { c.i(Ldi(30 + i as u8, (m >> (8 * i)) as u8)); }
        let counter = if n == 1 { 16 } else { 19 };
        c.seq(&[Ldi(counter, bits as u8), Mov(1, counter)]);
    } else {
        // Wide remainders need the divisor registers too. Use immediates
        // below, and seed the counter before clearing its temporary limb.
        c.seq(&[Ldi(26, bits as u8), Mov(1, 26)]);
        for &r in remainder { c.i(Ldi(r, 0)); }
    }
    let again = c.a.label(); c.a.bind(again);
    let before = c.t;
    c.i(Add(product[0], product[0]));
    for &r in &product[1..] { c.i(Adc(r, r)); }
    for &r in remainder { c.i(Adc(r, r)); }
    if remainder_bytes <= 2 {
        for (i, &r) in remainder.iter().enumerate() {
            c.i(if i == 0 { Cp(r, 30) } else { Cpc(r, 30 + i as u8) });
        }
        c.select(0, |_| {}, |c| {
            for (i, &r) in remainder.iter().enumerate() {
                c.i(if i == 0 { Sub(r, 30) } else { Sbc(r, 30 + i as u8) });
            }
            c.i(Ori(product[0], 1));
        });
    } else {
        for (i, &r) in remainder.iter().enumerate() {
            let byte = (u64::from(m) >> (8 * i)) as u8;
            c.i(if i == 0 { Subi(r, byte) } else { Sbci(r, byte) });
        }
        c.select(0, |c| {
            let negative = 0u64.wrapping_sub(u64::from(m));
            for (i, &r) in remainder.iter().enumerate() {
                let byte = (negative >> (8 * i)) as u8;
                c.i(if i == 0 { Subi(r, byte) } else { Sbci(r, byte) });
            }
        }, |c| c.i(Ori(product[0], 1)));
    }
    c.i(Dec(1)); c.a.jump(Target::Brbc(1), again);
    let iteration = c.t - before + 2;
    c.t = before + bits * iteration - 1;
    c.seq(&[Movw(16, 20), Movw(18, 22)]);
}

fn emit_loop_cap(a: &mut Asm, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    a.bind(s.loop_cap); sy.loop_cap = a.here();
    let mut c = Code::bounded(a, &q.sled);
    if l.records == 0 {
        fault_cell(&mut c, l, q, watch::FAULT_LOOP_CAP);
        c.i(Ret); b.loop_cap = c.t;
        return;
    }
    for r in 16..20 { c.i(Push(r)); }
    save_args(&mut c, l);
    // LoopCap has no argument; the fault continuation must not expose old A.
    for offset in [6, 8, 9, 10] { c.i(Sts(l.args + offset, 2)); }
    watch_stamp(&mut c, l, q);
    fault(&mut c, l, q, watch::FAULT_LOOP_CAP);
    push(&mut c, l, q, b);
    for r in (16..20).rev() { c.i(Pop(r)); }
    c.i(Ret); b.loop_cap = c.t;
}

fn timer_pointer(c: &mut Code<'_>, l: &Layout) {
    // Twelve-byte RAM rows: due followed by mutable period. Invalid timers
    // target separate scratch, never the live timer array. Eight setup clocks
    // and seven balanced range-selection clocks: fifteen in total.
    c.seq(&[Ldi(24, 12), Mul(16, 24), Ldi(30, l.timers as u8),
        Ldi(31, (l.timers >> 8) as u8), Add(30, 0), Adc(31, 1), Cpi(16, l.timer_count)]);
    c.select(0, |_| {}, |c| c.seq(&[Ldi(30, l.timer_sink as u8), Ldi(31, (l.timer_sink >> 8) as u8)]));
}

fn emit_timer_stubs(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, s: &Stubs, sy: &mut Symbols, b: &mut Bounds) {
    for (kind, (port, label)) in [(ports::TIMER_STOP, s.timer_stop), (ports::TIMER_START, s.timer_start),
        (ports::TIMER_PERIOD, s.timer_period)].into_iter().enumerate() {
        if !p.uses_port(port) { continue; }
        a.bind(label);
        match kind { 0 => sy.timer_stop = a.here(), 1 => sy.timer_start = a.here(), _ => sy.timer_period = a.here() }
        let mut c = Code::bounded(a, &q.sled);
        for r in 16..20 { c.i(Push(r)); }
        save_stub_args(&mut c, l, kind == 2);
        // Saved A stays raw; nonzero upper bytes select an invalid index.
        c.seq(&[Mov(24, 17), Or(24, 18), Or(24, 19), Cpi(24, 0)]);
        c.select(1, |_| {}, |c| c.i(Ldi(16, 255)));
        watch_stamp(&mut c, l, q);
        c.i(Cpi(16, l.timer_count));
        c.select(0, |c| {
            // Starting a timer needs the clock even without observation. Invalid handles and
            // stop/period operations need no stamp at Off.
            if p.watch == watch::Level::Off && kind == 1 { stamp(c, l, q); }
            // A valid handle alone addresses and changes a live timer row.
            c.seq(&[Ldi(24, 12), Mul(16, 24), Ldi(30, l.timers as u8),
                Ldi(31, (l.timers >> 8) as u8), Add(30, 0), Adc(31, 1)]);
            match kind {
                0 => {
                    c.i(Ldi(20, 255));
                    for offset in 0..6 { c.i(Std(false, offset, 20)); }
                }
                1 => {
                    for r in 20..26 { c.i(Ldd(r, false, r - 14)); }
                    for r in 20..26 {
                        c.i(Lds(26, l.stamp + u16::from(r - 20)));
                        c.i(if r == 20 { Add(r, 26) } else { Adc(r, 26) });
                        c.i(Std(false, r - 20, r));
                    }
                }
                _ => {
                    for r in 20..24 { c.i(Lds(r, l.args + u16::from(r - 20))); }
                    c.seq(&[Ldi(24, 0), Ldi(25, 0), Bst(23, 7)]);
                    c.select(6, |c| { for r in 20..24 { c.i(Ldi(r, 0)); } }, |_| {});
                    c.i(Add(20, 20)); for r in 21..26 { c.i(Adc(r, r)); }
                    c.seq(&[Push(30), Push(31)]);
                    if l.timer_count == 0 { c.i(Ldi(27, 0)); }
                    else {
                        c.i(Mov(26, 16));
                        c.flash(q.timer_table);
                        c.seq(&[Ldi(27, 8), Mul(26, 27), Add(30, 0), Adc(31, 1), LpmZ(27)]);
                    }
                    c.seq(&[Pop(31), Pop(30), Mov(26, 20)]);
                    for r in 21..26 { c.i(Or(26, r)); }
                    c.i(Cpi(26, 0));
                    c.select(1, |c| {
                        c.i(Bst(27, 0)); c.select(6, |c| c.i(Ldi(20, 1)), |_| {});
                    }, |_| {});
                    for r in 20..26 { c.i(Std(false, r - 14, r)); }
                }
            }
        }, |c| { fault(c, l, q, watch::FAULT_TIMER); push(c, l, q, b); });
        for r in (16..20).rev() { c.i(Pop(r)); }
        c.i(Ret);
        match kind { 0 => b.timer_stop = c.t, 1 => b.timer_start = c.t, _ => b.timer_period = c.t }
    }
}

fn tail_pointer(c: &mut Code<'_>, l: &Layout) {
    // Index load 2 + multiplier 3 + base 2 + add 2: nine clocks.
    c.seq(&[Lds(16, l.tail), Ldi(18, 8), Mul(16, 18),
        Ldi(26, l.ring as u8), Ldi(27, (l.ring >> 8) as u8), Add(26, 0), Adc(27, 1)]);
}

fn advance_tail(c: &mut Code<'_>, l: &Layout) {
    // Increment/test 2, wrap choice 2, publication 2: six clocks.
    c.seq(&[Inc(16), Cpi(16, l.records)]);
    c.select(1, |c| c.i(Ldi(16, 0)), |_| {});
    c.i(Sts(l.tail, 16));
}

fn hello(c: &mut Code<'_>, p: &Program<'_>, l: &Layout) {
    let frame = watch::Frame { kind: watch::HELLO, seq: 0, tick: 0,
        event: watch::Event::Hello { proto: watch::PROTO, ports_version: PORTS_VERSION,
            image: p.image_id, clock_hz: p.f_clk, prescale: PRESCALE,
            ring: u16::from(l.records), tick_hi: 0, mcusr: 0, wdtcsr: 0, level: p.watch, generation: 0 } };
    let mut wire = [0; watch::MAX_WIRE];
    let n = watch::encode(&frame, &mut wire);
    let mut body = [0; watch::MAX_BODY];
    let n = watch::cobs_decode(&wire[..n - 1], &mut body).expect("Hello fits one frame");
    assert_eq!(body[n - 1], watch::crc8(&body[..n - 1]));
    for (i, &byte) in body[..n - 1].iter().enumerate() { c.store(l.body + i as u16, byte); }
    c.copy(l.body + 21, l.mcusr, 1);
    c.copy(l.body + 22, l.wdtcsr, 1);
    c.i(Sts(l.hello_pending, 2)); c.i(Ldi(17, (n - 1) as u8));
}

fn ring_frame(c: &mut Code<'_>, l: &Layout, shifts: bool, eeprom: (bool, bool)) {
    // First record 47, kind/length selection 10. Without EEPROM, continuation
    // selection is at most 40 without Shift (97 total), 54 with it (111 total).
    // Linked EEPROM kinds add a shared two-byte continuation arm below.
    tail_pointer(c, l);
    for offset in 0..8 {
        let dst = if offset == 0 { l.body } else { l.body + offset + 1 };
        c.seq(&[Ld(20, Ptr::XInc), Sts(dst, 20)]);
    }
    advance_tail(c, l);
    c.seq(&[Lds(19, l.body), Ldi(17, 9), Cpi(19, watch::TIME)]);
    c.select(1, |c| c.i(Ldi(17, 8)), |_| {});
    c.i(Cpi(19, watch::RX));
    c.select(1, |c| c.i(Ldi(17, 7)), |_| {});
    c.i(Cpi(19, watch::LOST));
    c.select(1, |c| {
        tail_pointer(c, l); c.i(Ld(20, Ptr::XInc));
        for offset in 0..4 { c.seq(&[Ld(20, Ptr::XInc), Sts(l.body + 10 + offset, 20)]); }
        c.seq(&[Ld(20, Ptr::XInc), Sts(l.body + 9, 20)]);
        advance_tail(c, l); c.i(Ldi(17, 14));
    }, |c| {
        c.i(Cpi(19, watch::FAULT));
        c.select(1, |c| {
            tail_pointer(c, l); c.i(Ld(20, Ptr::XInc));
            for offset in 0..4 { c.seq(&[Ld(20, Ptr::XInc), Sts(l.body + 9 + offset, 20)]); }
            advance_tail(c, l); c.i(Ldi(17, 13));
        }, |c| {
            c.i(Cpi(19, watch::DUTY));
            c.select(1, |c| {
                tail_pointer(c, l); c.i(Ld(20, Ptr::XInc));
                c.seq(&[Ld(20, Ptr::XInc), Sts(l.body + 9, 20)]);
                advance_tail(c, l); c.i(Ldi(17, 10));
            }, |c| {
                if eeprom.0 || eeprom.1 {
                    if eeprom.0 && eeprom.1 {
                        c.seq(&[Mov(20, 19), Andi(20, 0xfe), Cpi(20, watch::EEPROM_READ)]);
                    } else { c.i(Cpi(19, if eeprom.0 { watch::EEPROM_READ } else { watch::EEPROM_WRITE })); }
                    c.select(1, |c| {
                        tail_pointer(c, l); c.i(Ld(20, Ptr::XInc));
                        for offset in 0..2 { c.seq(&[Ld(20, Ptr::XInc), Sts(l.body + 9 + offset, 20)]); }
                        advance_tail(c, l); c.i(Ldi(17, 11));
                    }, |_| {});
                }
                if shifts {
                    c.i(Cpi(19, watch::SHIFT));
                    c.select(1, |c| {
                        tail_pointer(c, l); c.i(Ld(20, Ptr::XInc));
                        for offset in 0..6 { c.seq(&[Ld(20, Ptr::XInc), Sts(l.body + 9 + offset, 20)]); }
                        advance_tail(c, l); c.i(Ldi(17, 15));
                    }, |_| {});
                }
            });
        });
    });
}

/// Sequence 8, polynomial setup 1 plus five cursor/count instructions, present byte 51 plus
/// CP/choice/DEC/BRNE 5 (last -1), cursors 14, COBS 29*14-1, finish 3.
const FRAME_WIRE: u32 = 8 + 1 + 5 + (watch::MAX_BODY - 1) as u32 * (CRC_BYTE + 5) - 1
    + 14 + watch::MAX_BODY as u32 * 14 - 1 + 3;
const CRC_BYTE: u32 = 51;
const _: () = assert!(FRAME_WIRE == 2003);

/// One CRC-8/SMBUS byte (polynomial 0x07 in r22, MSB first), bitwise: eight words and
/// no flash table. Measured against the 256-byte table (9 cycles, 278 bytes in all) and
/// a 16-byte nibble table (25 cycles, 66 bytes): this form keeps the image smallest.
/// Shift/conditional XOR 3 + DEC/BRNE 3 per bit, with the final branch -1.
fn crc_byte(c: &mut Code<'_>) {
    let start = c.t;
    let words = c.a.here();
    c.seq(&[Ld(21, Ptr::XInc), Eor(20, 21), Ldi(21, 8)]);
    let bit = c.a.label(); c.a.bind(bit);
    let before = c.t;
    c.i(Add(20, 20));
    c.select(0, |c| c.i(Eor(20, 22)), |_| {});
    c.i(Dec(21)); c.a.jump(Target::Brbc(1), bit);
    let iteration = c.t - before + 2;
    c.t = before + 8 * iteration - 1;
    assert_eq!(c.a.here() - words, 8, "CRC byte word hand count");
    assert_eq!(c.t - start, CRC_BYTE, "CRC byte cycle hand count");
}

fn frame_wire(c: &mut Code<'_>, l: &Layout) {
    let start = c.t;
    assert!(c.class == Class::Bounded, "frame wire uses bounded CRC and COBS loops");
    c.seq(&[Lds(20, l.seq), Sts(l.body + 1, 20), Inc(20), Sts(l.seq, 20), Ldi(20, 0)]);
    // The polynomial stays in r22 for the whole body.
    c.i(Ldi(22, 0x07));
    // With r19 counting down, r16 < r19 means present; X advances only
    // then and ends at the raw length's CRC. Absent slots skip the update.
    c.seq(&[Ldi(26, l.body as u8), Ldi(27, (l.body >> 8) as u8),
        Ldi(19, (watch::MAX_BODY - 1) as u8), Ldi(16, (watch::MAX_BODY - 1) as u8), Sub(16, 17)]);
    let crc = c.a.label(); c.a.bind(crc);
    let before = c.t;
    c.i(Cp(16, 19));
    c.select(0, crc_byte, |_| {});
    c.i(Dec(19)); c.a.jump(Target::Brbc(1), crc);
    let iteration = c.t - before + 2;
    c.t = before + (watch::MAX_BODY - 1) as u32 * iteration - 1;
    c.seq(&[St(Ptr::XInc, 20), Inc(17), Movw(30, 26),
        Ldi(26, (l.tx + 1) as u8), Ldi(27, ((l.tx + 1) >> 8) as u8), Add(26, 17), Adc(27, 2),
        St(Ptr::X, 2), Ldi(18, 1), Mov(16, 17), Inc(16), Ldi(19, watch::MAX_BODY as u8)]);
    // Scan backwards: a zero becomes the code for the run to its right.
    // X/Z need only output/input cursors; no third pointer or backpatching.
    // Skip absent trailing slots first. Present: load/test 3 + choice 4 +
    // store 2; outer choice at most 10 + compare 1 + DEC/BRNE 3 = 14 (last 13).
    let cobs = c.a.label(); c.a.bind(cobs);
    let before = c.t;
    c.i(Cp(19, 16));
    c.select(0, |c| {
        c.seq(&[Ld(20, Ptr::ZDec), Cpi(20, 0)]);
        // Both zero/nonzero paths naturally take four clocks, with no padding.
        let choice = c.t;
        c.seq(&[Brbs(1, 2), Inc(18), Rjmp(2), Mov(20, 18), Ldi(18, 1)]);
        c.t = choice + 4;
        c.i(St(Ptr::XDec, 20));
    }, |_| {});
    c.i(Dec(19)); c.a.jump(Target::Brbc(1), cobs);
    let iteration = c.t - before + 2;
    c.t = before + watch::MAX_BODY as u32 * iteration - 1;
    // Return the wire length in r17. Only masked publication changes cursors.
    c.seq(&[St(Ptr::XDec, 18), Subi(17, 254)]);
    assert_eq!(c.t - start, FRAME_WIRE, "frame wire bound hand count");
}

fn emit_udre(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, sy: &mut Symbols) -> u32 {
    if p.watch == watch::Level::Off { return 0; }
    a.bind(q.udre); sy.udre = a.here();
    let mut c = Code::bounded(a, &[]);
    // r4/r5 were unused by the emitter, stubs and arithmetic helpers. Reserve
    // them for this non-nesting ISR's SREG and position. Only Z needs saving;
    // in particular a pump between far wake and UDRIE-off preserves the pass.
    c.seq(&[In(4, SREG), Push(30), Push(31), Lds(5, l.tx_pos), Lds(30, l.tx_len), Cp(5, 30)]);
    c.select(0, |c| {
        c.seq(&[Ldi(30, l.tx as u8), Ldi(31, (l.tx >> 8) as u8), Add(30, 5), Adc(31, 2),
            Ld(30, Ptr::Z), Sts(0xc6, 30), Inc(5), Sts(l.tx_pos, 5)]);
    }, |c| {
        c.seq(&[Ldi(30, p.uart(false)), Sts(UCSR0B, 30)]);
    });
    c.seq(&[Pop(31), Pop(30), Out(SREG, 4), Reti]);
    // Entry/vector 7 + save 5 + compare 5 + send arm 13 + restore/RETI 9.
    assert_eq!(c.t + 7, UDRE_BYTE, "pump byte hand count");
    c.t + 7
}

fn drain_return(c: &mut Code<'_>) {
    // RX restores the saved foreground registers and flags before RETI.
    // Fifteen restored registers, saved SREG, original r0 and RETI: 39.
    for r in [31, 30].into_iter().chain((16..28).rev()) { c.i(Pop(r)); }
    c.seq(&[Pop(1), Pop(0), Out(SREG, 0), Pop(0), Reti]);
}

fn emit_reset(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, sy: &mut Symbols) {
    a.bind(q.reset);
    let mut c = Code::new(a, &q.sled);
    c.seq(&[Bclr(7), Eor(2, 2)]);
    if p.watch != watch::Level::Off { c.i(In(16, 0x34)); }
    c.i(Out(0x34, 2));
    if p.watch != watch::Level::Off { c.i(Lds(17, 0x60)); }
    c.seq(&[Ldi(18, 0x18), Sts(0x60, 18), Sts(0x60, 2),
        Ldi(18, (l.stack_top >> 8) as u8), Out(0x3e, 18), Ldi(18, l.stack_top as u8), Out(0x3d, 18)]);
    if p.watch != watch::Level::Off || p.serial.is_some() { c.store(UCSR0B, p.uart(false)); }
    for addr in [0x44, 0x45, 0x80, 0x81, 0x82, 0xb0, 0xb1, 0x6e, 0x6f, 0x70,
        0x68, 0x3d, 0x24, 0x27, 0x2a, 0x25, 0x28, 0x2b] { c.i(Sts(addr, 2)); }
    c.i(Ldi(18, 7)); for addr in 0x35..=0x37 { c.i(Sts(addr, 18)); }
    // Clock is stopped throughout RAM initialization. Six clocks per byte
    // except the final untaken branch (one less), plus four setup clocks.
    let n = l.state_bytes;
    let end = l.ram_end;
    let zero_start = if n < 8 { 0x100 } else { l.state + n };
    c.seq(&[Ldi(26, zero_start as u8), Ldi(27, (zero_start >> 8) as u8),
        Ldi(24, (end - zero_start) as u8), Ldi(25, ((end - zero_start) >> 8) as u8)]);
    let zero = c.a.label(); c.a.bind(zero);
    c.seq(&[St(Ptr::XInc, 2), Sbiw(24, 1)]); c.a.jump(Target::Brbc(1), zero);
    if p.watch != watch::Level::Off { c.seq(&[Sts(l.mcusr, 16), Sts(l.wdtcsr, 17)]); }
    c.flash(q.state_init);
    if n < 8 {
        for offset in 0..n { c.seq(&[LpmZInc(18), Sts(l.state + offset, 18)]); }
    } else {
        // The state is the first RAM area: the zero loop above starts right after it.
        assert_eq!(l.state, 0x100, "state is the first RAM area");
        c.seq(&[Ldi(26, l.state as u8), Ldi(27, (l.state >> 8) as u8),
            Ldi(24, n as u8), Ldi(25, (n >> 8) as u8)]);
        let copy = c.a.label(); c.a.bind(copy);
        c.seq(&[LpmZInc(18), St(Ptr::XInc, 18), Sbiw(24, 1)]); c.a.jump(Target::Brbc(1), copy);
        c.seq(&[Ldi(26, end as u8), Ldi(27, (end >> 8) as u8)]);
        // Reset's loops are not expanded in c.t. By hand: zeroing saves 6n;
        // copy changes 5n to 4 + 9n - 1; restoring X costs 2. Pad 2n - 5 to
        // keep t0 unchanged. r18/Z end at the last byte/one past it, r24:r25
        // are zero, X is the old zero loop's end, and SREG is as the old zero
        // loop left it: both last SBIWs take 1 to 0.
        c.pad(2 * u32::from(n) - 5);
    }
    for pin in &p.pins {
        if pin.role == Role::Analog { continue; }
        let gpio = chip::by_number(u32::from(pin.pin)).expect("build checked the pin");
        let (base, bit) = (chip::pin_register(gpio.port) as u8, gpio.bit);
        match pin.role {
            Role::Output | Role::Pwm(_) => c.i(Sbi(base + 1 - 0x20, bit)),
            Role::Input { pull: true } => c.i(Sbi(base + 2 - 0x20, bit)),
            Role::Input { pull: false } | Role::Analog => {}
        }
    }
    if p.pins.iter().any(|pin| pin.role == Role::Analog) {
        let didr = p.pins.iter().filter(|pin| pin.role == Role::Analog)
            .filter_map(|pin| chip::adc_channel(u32::from(pin.pin)))
            .filter(|&channel| channel < 6).fold(0, |bits, channel| bits | (1 << channel));
        c.store(0x7e, didr);
        // ADCSRA retains its reset value zero; conversions alone enable the ADC.
        // The runtime never sets PRR, so PRADC stays zero.
    }
    if p.watch != watch::Level::Off || p.serial.is_some() {
        let (ubrr, u2x) = p.baud();
        c.store(0xc4, ubrr as u8); c.store(0xc5, (ubrr >> 8) as u8);
        c.store(0xc0, if u2x { 2 } else { 0 }); c.store(0xc2, 6); c.store(UCSR0B, p.uart(false));
    }
    c.store(0x43, 0x83); c.store(0x44, 3); c.store(0x45, 2);
    c.i(Sts(0x46, 2)); c.store(0x6e, 1);
    emit_pwm_reset(&mut c, p);
    for r in 13..16 { c.i(Mov(r, 2)); }
    for addr in [l.clock_hi, l.clock_hi + 1, l.clock_b31] { c.i(Sts(addr, 2)); }
    for (k, timer) in p.timers.iter().enumerate() {
        for offset in 0..6 {
            let byte = (timer.period_ticks >> (offset * 8)) as u8;
            c.store(l.timers + k as u16 * 12 + offset, byte);
            c.store(l.timers + k as u16 * 12 + 6 + offset, byte);
        }
    }
    if p.serial.is_some() {
        for ptr in [l.rx_head, l.rx_tail] {
            c.store(ptr, l.rx_fifo as u8); c.store(ptr + 1, (l.rx_fifo >> 8) as u8);
        }
    }
    if p.watch != watch::Level::Off { c.store(l.hello_pending, 1); }
    c.i(Sts(0x43, 2)); sy.t0 = c.a.here();
    c.i(Bset(7)); sy.setup_call = c.a.here(); c.call(q.setup, 0); c.jump(q.pass);
}

fn emit_pwm_reset(c: &mut Code<'_>, p: &Program<'_>) {
    // The synchronous and asynchronous prescalers are held by TSM here.
    // Set WGM before ICR1 (writes require an ICR1-TOP mode), then clear
    // every counter and OCR before GTCCR releases all timers at t0.
    for timer in 1..=2 {
        let Some(setting) = p.pins.iter().find_map(|pin| match pin.role {
            Role::Pwm(setting) if setting.timer == timer => Some(setting), _ => None,
        }) else { continue; };
        let cs = pwm::ATMEGA328P.timers[timer as usize].prescalers.iter()
            .position(|&n| n == setting.prescale).unwrap() as u8 + 1;
        let mut com = 0;
        for pin in &p.pins {
            if matches!(pin.role, Role::Pwm(s) if s.timer == timer) {
                com |= if matches!(pin.pin, 9 | 11) { 0x80 } else { 0x20 };
            }
        }
        if timer == 1 {
            let free_top = setting.top != 255;
            c.store(0x80, com | if free_top { 2 } else { 1 });
            c.store(0x81, cs | if free_top { 0x10 } else { 0 });
            if free_top {
                c.store(0x87, (setting.top >> 8) as u8); c.store(0x86, setting.top as u8);
            }
            c.seq(&[Sts(0x85, 2), Sts(0x84, 2)]);
        } else {
            c.store(0xb0, com | 1); c.store(0xb1, cs); c.i(Sts(0xb2, 2));
        }
    }
    for pin in &p.pins {
        if matches!(pin.role, Role::Pwm(_)) {
            let (ocr, high, _, _) = pwm_registers(pin.pin);
            if high != 0 { c.i(Sts(u16::from(high), 2)); }
            c.i(Sts(u16::from(ocr), 2));
        }
    }
}

fn pass_agent(c: &mut Code<'_>, l: &Layout, q: &Labels, b: &Bounds) {
    // Compress and compare: 33 clocks. Time decision: 43 + push bound;
    // empty push for Lost: 6 + push bound. Interrupts stay enabled throughout.
    c.seq(&[Lds(20, l.stamp + 3), Lds(21, l.stamp + 4), Lds(22, l.stamp + 5)]);
    for _ in 0..6 { c.seq(&[Lsr(22), Ror(21), Ror(20)]); }
    for r in 20..23 {
        c.i(Lds(23, l.time_sent + u16::from(r - 20)));
        c.i(if r == 20 { Cp(r, 23) } else { Cpc(r, 23) });
    }
    c.select(1, |_| {}, |c| {
        for r in 20..23 { c.i(Sts(l.time_sent + u16::from(r - 20), r)); }
        record_stamp(c, l); c.store(l.record, watch::TIME);
        c.copy(l.record + 5, l.stamp + 4, 2); c.i(Sts(l.record + 7, 2));
        c.store(l.record_count, 1); push(c, l, q, b);
    });
    // n=0 flushes a pending Lost pair if there is room, without counting a
    // new loss when there is not. Both calls use the same push bound.
    c.i(Sts(l.record_count, 2)); push(c, l, q, b);
}

fn far_until(c: &mut Code<'_>, l: &Layout) {
    let start = c.t;
    // Next Time epoch is 2^22 - (stamp_overflows & (2^22-1)) away: 13.
    // Work with distances before truncation, so a distant 48-bit due cannot
    // alias an earlier epoch when the served 24-bit counter wraps.
    c.seq(&[Ldi(16, 0), Ldi(17, 0), Ldi(18, 0x40)]);
    for r in 16..19 {
        c.i(Lds(26, l.stamp + u16::from(r - 15)));
        if r == 18 { c.i(Andi(26, 0x3f)); }
        c.i(if r == 16 { Sub(r, 26) } else { Sbc(r, 26) });
    }
    // due == all ones contributes no deadline: test 7, balanced choice 47.
    c.i(Ldi(26, 255));
    for r in 6..12 { c.i(if r == 6 { Cp(r, 26) } else { Cpc(r, 26) }); }
    c.select(1, |_| {}, |c| {
        // r20..r25 still hold due-stamp from the far decision. In ticks,
        // subtract W, add stamp's low byte, then subtract two overflows:
        // 6 + 8 + 6 + balanced zero clamp 10 = 30. Discarding r20 gives
        // ((due-W)>>8)-2-(stamp>>8), clamped to the current overflow.
        for r in 20..26 {
            let byte = (u64::from(l.w) >> (8 * (r - 20))) as u8;
            c.i(if r == 20 { Subi(r, byte) } else { Sbci(r, byte) });
        }
        c.i(Lds(26, l.stamp));
        c.i(Add(20, 26)); for r in 21..26 { c.i(Adc(r, 2)); }
        c.i(Subi(20, 0));
        for r in 21..26 { c.i(Sbci(r, if r == 21 { 2 } else { 0 })); }
        c.select(0, |c| { for r in 20..26 { c.i(Ldi(r, 0)); } }, |_| {});
        // Full forty-bit unsigned distance comparison 5, balanced min 7.
        c.seq(&[Cp(21, 16), Cpc(22, 17), Cpc(23, 18), Cpc(24, 2), Cpc(25, 2)]);
        c.select(0, |c| c.seq(&[Mov(16, 21), Mov(17, 22), Mov(18, 23)]), |_| {});
    });
    // Add the sampled overflow count, not the later live ISR count: 9 + 6.
    // The chosen distance is <= 2^22, safely inside the signed 24-bit range.
    for r in 16..19 {
        c.i(Lds(26, l.stamp + u16::from(r - 15)));
        c.i(if r == 16 { Add(r, 26) } else { Adc(r, 26) });
        c.i(Sts(l.far_until + u16::from(r - 16), r));
    }
    assert_eq!(c.t - start, 82, "far threshold hand count");
}

fn emit_idle(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, sy: &mut Symbols, b: &mut Bounds) {
    // Timer changes and watch pushes are foreground-only. RX loss/overrun
    // and early-cut flags are consumed with queued bytes; PWM runs entirely
    // in hardware. The epoch wake also keeps now48's bit-31 extension current.
    let check = a.label();
    let ready = a.label();
    let sleep = a.label();
    let encode = a.label();
    a.bind(check); sy.idle_check = a.here();
    let mut c = Code::bounded(a, &q.sled);
    if p.serial.is_some() {
        // First wake RX: LDS 2 + CPSE 1 + JMP 3 = 6 (old JMP was 3).
        // Empty skips the two-word JMP in three clocks: 2 + 3 = 5.
        c.seq(&[Lds(16, l.rx_count), Cpse(16, 2)]); c.jump(q.pass);
        assert_eq!(c.t, 6, "idle fast RX hand count");
        c.t = 5;
    }
    sy.idle_mask = c.a.here(); c.i(Bclr(7));
    if p.serial.is_some() {
        // Retest both bytes atomically: full FIFOs can have head == tail,
        // and counts such as 256 have a zero low byte. Clear 7, ready 8.
        c.seq(&[Lds(16, l.rx_count), Lds(17, l.rx_count + 1), Or(16, 17), Cpse(16, 2)]);
        c.a.jump(Target::Rjmp, ready); c.t += 1; // CPSE skips one word.
        assert_eq!(c.t + 1 + 4, 18, "idle masked RX through SEI/JMP hand count");
    }
    if p.watch != watch::Level::Off {
        // Match push's occupancy arithmetic, including head < tail. Lost
        // without room means at least records-2 >= 2 records are queued.
        // Each drained TX buffer then permits an encode freeing >=1 record.
        // Far enables an empty pump to wake idle. If it runs before SLEEP,
        // Timer0 supplies the next wake within one overflow instead. Once two
        // slots are free, the next pass flushes Lost before new events.
        let no_lost = c.a.label();
        c.i(Lds(16, l.lost));
        for offset in 1..4 { c.seq(&[Lds(17, l.lost + offset), Or(16, 17)]); }
        c.i(Cpi(16, 0)); c.a.jump(Target::Brbs(1), no_lost); c.t += 1;
        c.seq(&[Lds(16, l.head), Lds(17, l.tail), Sub(16, 17)]);
        c.select(0, |c| c.i(Subi(16, 0u8.wrapping_sub(l.records))), |_| {});
        c.seq(&[Ldi(18, l.records - 1), Sub(18, 16), Cpi(18, 2)]);
        c.a.jump(Target::Brbc(0), ready); c.t += 1;
        c.a.bind(no_lost);
        assert_eq!(c.t, if p.serial.is_some() { 37 } else { 25 }, "idle Lost/room hand count");
    }
    // Snapshot and subtract while masked: 3 MOV + 3 LDS + SUB/SBC = 12.
    // Test N, not S: this is the sign of the *modulo* difference, including
    // crossings of 0x800000 and zero, not signed ordering of the operands.
    for r in 16..19 {
        c.seq(&[Mov(r, r - 3), Lds(26, l.far_until + u16::from(r - 16))]);
        c.i(if r == 16 { Sub(r, 26) } else { Sbc(r, 26) });
    }
    c.a.jump(Target::Brbc(2), ready); c.t += 1;
    let reached = c.t + 1;
    if p.watch != watch::Level::Off {
        c.seq(&[Lds(16, l.tx_pos), Lds(17, l.tx_len), Cp(16, 17)]);
        c.a.jump(Target::Brbc(1), sleep); c.t += 1;
        c.seq(&[Lds(16, l.head), Lds(17, l.tail), Sub(16, 17), Lds(18, l.hello_pending), Or(16, 18), Cpi(16, 0)]);
        c.a.jump(Target::Brbs(1), sleep); c.t += 1;
        // Keep ready and sleep within BRxx range; one RJMP is smaller than a
        // sole caller's CALL/RET. r18 carries Hello priority into the body.
        sy.encode = c.a.here();
        c.a.emit(Bset(7)); c.a.jump(Target::Rjmp, encode);
        c.t += 1; // The empty-work branch to sleep takes two clocks.
    }
    // No producer can land between the masked recheck and SEI/SLEEP.
    // A buffered pump never consumes ring records; an empty one disables itself.
    c.a.bind(sleep);
    c.seq(&[Bset(7), Sleep]); c.a.jump(Target::Rjmp, check); c.t += 2;
    let sleeping = c.t;
    c.a.bind(ready); c.t = reached;
    c.i(Bset(7)); c.jump(q.pass);
    b.idle_check = c.t.max(sleeping);
    assert_eq!(b.idle_check, (if p.watch == watch::Level::Off { 31 } else { IDLE_CHECK_MAX })
        - if p.serial.is_some() { 0 } else { 12 }, "idle check hand count");
    if p.watch != watch::Level::Off {
        c.a.bind(encode); sy.encode_body = c.a.here(); c.t = 3; // SEI/RJMP above.
        c.i(Cpi(18, 0));
        let shifts = p.watch == watch::Level::Ports && !p.shift_pairs.is_empty();
        let eeprom = (p.watch == watch::Level::Ports && p.uses_port(ports::EEPROM_READ),
            p.watch == watch::Level::Ports && p.uses_port(ports::EEPROM_WRITE));
        c.select(1, |c| ring_frame(c, l, shifts, eeprom), |c| hello(c, p, l));
        frame_wire(&mut c, l);
        // Until CLI both old cursors remain equal. An empty pump can only
        // disable UDRIE, never read this buffer. Publish both cursors before
        // enabling the pump; no producer or second consumer can interleave.
        sy.tx_publish = c.a.here();
        let start = c.t;
        c.seq(&[Bclr(7), Sts(l.tx_len, 17), Sts(l.tx_pos, 2)]);
        c.store(UCSR0B, p.uart(true)); c.i(Bset(7));
        assert_eq!(c.t - start, TX_PUBLISH, "TX publication hand count");
        c.a.jump(Target::Rjmp, check); c.t += 2;
        // SEI/RJMP 3 + Hello test 1 + base frame choice 101 + wire
        // + publication 9 + recheck jump 2, excluding ISR service. The
        // continuation arms and their fixed-width branch choices add:
        let extra = match (shifts, eeprom) {
            (false, (false, false)) => 0, (false, _) => 1,
            (true, (false, false)) => 14, (true, (true, true)) => 43, (true, _) => 41,
        };
        assert_eq!(c.t, 116 + FRAME_WIRE + extra, "foreground encode hand count");
        b.encode = c.t; sy.encode_end = c.a.here();
    }
}

fn emit_pass(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, sy: &mut Symbols, b: &mut Bounds) {
    a.bind(q.pass); sy.pass = a.here();
    let far = a.label();
    let defer = p.serial.map(|_| a.label());
    let mut c = Code::bounded(a, &q.sled);
    c.i(Bclr(7)); c.call(q.now48, NOW48); c.i(Bset(7));
    // UDRIE may still be enabled after a far wake. The drain preserves these
    // registers until this store; all subsequent producers own the ring.
    if p.watch != watch::Level::Off { c.store(UCSR0B, p.uart(false)); }
    for r in 20..26 { c.i(Sts(l.stamp + u16::from(r - 20), r)); }
    c.i(Ldi(16, 255)); for r in 6..13 { c.i(Mov(r, 16)); }
    for k in 0..l.timer_count {
        // Twelve load clocks, six compare clocks, nine select clocks = 27.
        for r in 20..26 { c.i(Lds(r, l.timers + u16::from(k) * 12 + u16::from(r - 20))); }
        for r in 20..26 { c.i(if r == 20 { Cp(r, r - 14) } else { Cpc(r, r - 14) }); }
        c.select(0, |c| {
            for r in 20..26 { c.i(Mov(r - 14, r)); }
            c.seq(&[Ldi(16, k), Mov(12, 16)]);
        }, |_| {});
    }
    if p.serial.is_some() {
        emit_rx_scan(&mut c, l, sy, b);
    }
    for r in 6..12 { c.i(Sts(l.due + u16::from(r - 6), r)); }
    c.i(Sts(l.selected, 12));
    if p.watch != watch::Level::Off { pass_agent(&mut c, l, q, b); }
    for r in 20..26 {
        c.i(Lds(r, l.stamp + u16::from(r - 20)));
        c.seq(&[Mov(0, r - 14), if r == 20 { Sub(0, r) } else { Sbc(0, r) }, Mov(r, 0)]);
    }
    c.i(Ldi(17, 1));
    c.select(0, |_| {}, |c| {
        c.seq(&[Ldi(26, l.w as u8), Ldi(27, (l.w >> 8) as u8), Cp(20, 26), Cpc(21, 27)]);
        for r in 22..26 { c.i(Cpc(r, 2)); }
        c.select(0, |_| {}, |c| c.select(1, |_| {}, |c| c.i(Ldi(17, 0))));
    });
    c.i(Ldi(26, 255));
    for r in 6..12 { c.i(if r == 6 { Cp(r, 26) } else { Cpc(r, 26) }); }
    c.select(1, |c| c.i(Ldi(17, 0)), |_| {});
    c.i(Cpi(17, 0));
    // Near takes the two-cycle skip. Far takes BRBC 1 plus JMP 3.
    c.a.emit(Brbc(1, 2)); c.a.jump(Target::Jmp, far);
    b.pass_prefix = c.t + 2 - b.rx_scan;
    // The lock, no-overrun bookkeeping, dispatch and far suffix retain exact timing.
    let mut c = Code::new(c.a, &q.sled);
    emit_lock(c.a, l, q, sy);
    emit_bookkeep(c.a, p, l, q, sy, b);
    for (entry, target) in q.dispatch.iter().zip(&q.entries) {
        c.a.bind(*entry); sy.dispatch.push(c.a.here()); c.t = 0;
        // L=22: CLI 1, sixteen check/padding clocks, SEI 1, CALL 4. SEI defers
        // pending service through CALL, so it runs before the entry instruction.
        c.i(Bclr(7));
        if let Some(defer) = defer { rx_check(&mut c, l, defer); }
        c.to(L - 5);
        c.i(Bset(7)); sy.dispatch_call.push(c.a.here());
        c.call(*target, 0);
        assert_eq!(c.t, L, "dispatch clear path hand count");
        c.jump(q.pass);
    }
    if let Some(defer) = defer {
        c.a.bind(defer); sy.rx_defer = c.a.here(); c.t = 8; // CLI + set check.
        // B can fire only for an ISR after A's read with stamp < due. The
        // earlier lock sample is then below due, so bookkeeping did not take
        // the overrun arm or push a fault. Its only persistent timer change
        // was advancing/disarming the due; the other writes are scratch.
        // The window starts at 8*due + 43 or later, so any earlier-stamped
        // service has finished. Restore while masked, then enable for rescan.
        c.i(Lds(16, l.selected)); timer_pointer(&mut c, l);
        for offset in 0..6 {
            c.seq(&[Lds(26, l.rx_cut + u16::from(offset)), Std(false, offset, 26)]);
        }
        c.i(Bset(7));
        assert_eq!(c.t, RX_DEFER_MASK, "RX defer mask hand count");
        c.jump(q.pass);
        assert_eq!(c.t, RX_DEFER, "RX defer hand count");
    }
    c.a.bind(far); c.t = 0;
    far_until(&mut c, l);
    if p.watch != watch::Level::Off {
        c.seq(&[Lds(16, l.head), Lds(17, l.tail), Sub(16, 17), Lds(18, l.hello_pending), Or(16, 18),
            Lds(18, l.tx_len), Lds(19, l.tx_pos), Sub(18, 19), Or(16, 18), Cpi(16, 0)]);
        c.select(1, |c| c.i(Ldi(16, p.uart(false))), |c| c.i(Ldi(16, p.uart(true))));
        c.i(Sts(UCSR0B, 16));
    }
    c.i(Bset(7)); sy.sleep = c.a.here(); c.i(Sleep);
    if p.watch == watch::Level::Off {
        b.far = c.t;
    } else { assert_eq!(c.t, FAR_SUFFIX, "far suffix hand count"); }
    // Far takes two more branch clocks than near. pass_max reserves
    // LOCK 224 + CALL 4 + BOOKKEEP 256 + L 22 - those 2 = 504 here.
    // Include the repeated wake's RJMP and longest idle check (73). A ready
    // UART may fill its shifter and data buffer on successive IRQs. Reserve
    // six short pumps around wake/publication: one before far's SEI, one
    // waking SLEEP, at most two in the RX fast check, one after ready's
    // SEI/JMP, one after now48's SEI/LDI before the UCSR0B store. RETI
    // guarantees foreground progress at each boundary. Publication can first
    // take a pump before check's LDS, then the two fast-check slots and the
    // two pass-entry slots: five, also covered by this six-pump allowance.
    // 107 + 73 + 6*39 = 414 <= 504. The separate W term pays for the encode
    // and its possible empty pump; these short pumps need no extra W term.
    assert!(c.t + IDLE_WAKE_MAX + 6 * b.udre <= b.pass_max() - b.pass_prefix - 2,
        "far suffix and wake check fit pass_max's 504-cycle allowance");
    emit_idle(c.a, p, l, q, sy, b);
}

fn emit_lock(a: &mut Asm, l: &Layout, q: &Labels, sy: &mut Symbols) {
    // Sample/expand/store: 81 clocks; full owed product: 15; delta, burn and
    // clamp: 51; padding: 76; SEI: one. Total LOCK=224, excluding CALL delay.
    sy.lock = a.here();
    let mut c = Code::new(a, &q.sled); c.i(Bclr(7));
    for r in 16..25 { sy.samples[(r - 16) as usize] = c.a.here(); c.i(In(r, TCNT0)); }
    c.seq(&[In(26, TIFR0), Mov(0, 24), Ldi(25, 0)]);
    // Eight modulo-byte differences and eight additions: sixteen clocks.
    for r in 17..25 { c.seq(&[Sub(r, 16), Add(25, r)]); }
    c.seq(&[Sts(l.phase, 25), Mov(20, 0)]);
    expand_clock(&mut c, l);
    for r in 20..26 { c.i(Sts(l.stamp + u16::from(r - 20), r)); }
    // Full owed count modulo 256. D+E needs only its low sixteen bits here;
    // the served low overflow byte is r13, never the pending-corrected copy.
    let neg_e = 0u16.wrapping_sub(E as u16);
    c.seq(&[Mov(16, 6), Mov(17, 7), Subi(16, neg_e as u8), Sbci(17, (neg_e >> 8) as u8),
        Sub(17, 13), Mov(18, 17), Ldi(19, 0)]);
    for _ in 0..4 { c.seq(&[Add(18, 18), Adc(19, 19)]); }
    // Delta is signed 48-bit. Burn is signed 24-bit two's complement; a
    // negative delta discards these truncated low bits in the balanced clamp.
    for r in 6..12 { c.i(if r == 6 { Sub(r, r + 14) } else { Sbc(r, r + 14) }); }
    c.seq(&[Ldi(16, 0), Bst(11, 7), Bld(16, 0), Movw(20, 6), Mov(22, 8)]);
    for _ in 0..3 { c.seq(&[Add(20, 20), Adc(21, 21), Adc(22, 22)]); }
    c.seq(&[Lds(23, l.phase), Sub(20, 23), Sbc(21, 2), Sbc(22, 2),
        Subi(20, C as u8), Sbci(21, (C >> 8) as u8), Sbci(22, (C >> 16) as u8),
        Sub(20, 18), Sbc(21, 19), Sbc(22, 2), Sbrc(22, 7), Ori(16, 1)]);
    // SBRC/ORI is exactly two clocks on either path, unlike its base sum.
    c.seq(&[Ldi(23, N_MIN as u8), Cp(20, 23), Cpc(21, 2), Cpc(22, 2), Sbc(23, 23), Or(16, 23), Cpi(16, 0)]);
    c.select(1, |_| {}, |c| c.seq(&[Ldi(20, N_MIN as u8), Ldi(21, 0), Ldi(22, 0)]));
    c.seq(&[Mov(24, 20), Mov(25, 21), Mov(23, 22)]);
    c.to(LOCK - 1); c.i(Bset(7)); sy.lock_end = c.a.here();
    c.call(q.delay, 0);
}

fn emit_bookkeep(a: &mut Asm, p: &Program<'_>, l: &Layout, q: &Labels, sy: &mut Symbols, b: &mut Bounds) {
    // No-overrun work including IJMP stays exact at BOOKKEEP. Only late repeating
    // timers take the bounded arm; its extra work is published in b.overrun.
    let overrun = a.label();
    let rx = a.label();
    let mut c = Code::new(a, &q.sled);
    sy.bookkeep = c.a.here();
    if p.serial.is_some() {
        c.seq(&[Lds(16, l.selected), Cpi(16, 255)]);
        c.a.emit(Brbc(1, 2)); c.a.jump(Target::Jmp, rx);
        c.t += 2; // Timer: taken BRBC skips JMP. RX: one plus JMP, seven total.
        // A precedes every timer side effect, including an overrun fault.
        // Five clear-path clocks consume padding: 156 work + 100 pad = 256.
        sy.rx_check = c.a.here();
        rx_check(&mut c, l, q.pass);
    }
    c.i(Lds(16, l.selected)); timer_pointer(&mut c, l);
    c.seq(&[Push(30), Push(31)]);
    for r in 16..22 { c.i(Ldd(r, false, r - 10)); }
    c.flash(q.timer_table);
    c.seq(&[Lds(24, l.selected), Ldi(25, 8), Mul(24, 25), Add(30, 0), Adc(31, 1),
        LpmZInc(26), LpmZInc(27), LpmZInc(0), LpmZInc(1), Sts(l.args, 0), Sts(l.args + 1, 1), Pop(31), Pop(30)]);
    for r in 6..12 { c.i(Lds(r, l.due + u16::from(r - 6))); }
    for r in 6..12 { c.i(if r == 6 { Add(r, r + 10) } else { Adc(r, r + 10) }); }
    for r in 6..12 {
        c.i(Lds(22, l.stamp + u16::from(r - 6)));
        c.i(if r == 6 { Cp(r, 22) } else { Cpc(r, 22) });
    }
    c.i(Ldi(23, 0));
    c.select(0, |c| c.i(Ldi(23, 1)), |c| c.select(1, |c| c.i(Ldi(23, 1)), |_| {}));
    c.i(Bst(26, 0));
    c.select(6, |_| {}, |c| {
        c.seq(&[Ldi(23, 0), Ldi(24, 255)]); for r in 6..12 { c.i(Mov(r, 24)); }
    });
    c.i(Cpi(23, 0));
    let common = c.t;
    let can_overrun = p.watch != watch::Level::Off || p.timers.iter().any(|timer| timer.repeat);
    if can_overrun {
        c.a.emit(Brbs(1, 2)); c.a.jump(Target::Jmp, overrun);
        c.t += 2;
    }
    for r in 6..12 { c.i(Std(false, r - 6, r)); }
    c.seq(&[Lds(30, l.args), Lds(31, l.args + 1)]);
    c.to(BOOKKEEP - 2); c.i(Ijmp);
    if can_overrun {
        c.a.bind(overrun); sy.overrun = c.a.here();
        let mut c = Code::bounded(c.a, &q.sled);
        // The lock's SEI precedes delay/bookkeeping. Neither this arm nor its
        // push/fault callees mask interrupts. Keep the selected timer pointer
        // until the new due is ready; pop it before calling either callee.
        c.seq(&[Push(30), Push(31), Ldi(30, l.stamp as u8), Ldi(31, (l.stamp >> 8) as u8)]);
        // Bounded 48/48 restoring division of T-D by P, at most 48*29-1 clocks.
        // Its six-byte quotient replaces the dividend; remainder starts at zero.
        assert_eq!(l.due, l.stamp + 6, "stamp and due share one displacement base");
        for r in 6..12 {
            c.seq(&[Ldd(r, false, r - 6), Ldd(22, false, r)]);
            c.i(if r == 6 { Sub(r, 22) } else { Sbc(r, 22) });
        }
        for r in 22..28 { c.i(Ldi(r, 0)); }
        c.i(Ldi(30, 48)); let again = c.a.label(); c.a.bind(again);
        let before = c.t;
        c.i(Add(6, 6)); for r in 7..12 { c.i(Adc(r, r)); }
        for r in 22..28 { c.i(Adc(r, r)); }
        for r in 22..28 { c.i(if r == 22 { Cp(r, r - 6) } else { Cpc(r, r - 6) }); }
        c.select(0, |_| {}, |c| {
            for r in 22..28 { c.i(if r == 22 { Sub(r, r - 6) } else { Sbc(r, r - 6) }); }
            c.i(Inc(6));
        });
        c.i(Dec(30)); c.a.jump(Target::Brbc(1), again);
        let iteration = c.t - before + 2;
        c.t = before + 48 * iteration - 1;
        if p.watch != watch::Level::Off {
            // Saturate the low quotient to u32 without touching P or remainder.
            c.i(Or(10, 11));
            c.select(1, |_| {}, |c| {
                c.i(Ldi(30, 255)); for r in 6..10 { c.i(Mov(r, 30)); }
            });
            c.seq(&[Ldi(30, l.record as u8), Ldi(31, (l.record >> 8) as u8)]);
            for r in 6..10 { c.i(Std(false, r + 3, r)); }
        }
        // T - remainder + P is the first unmissed grid point. The remainder
        // stays in r22..r27; no balancing sink or RAM scratch is needed.
        c.seq(&[Ldi(30, l.stamp as u8), Ldi(31, (l.stamp >> 8) as u8)]);
        for r in 6..12 {
            c.i(Ldd(r, false, r - 6));
            c.i(if r == 6 { Sub(r, r + 16) } else { Sbc(r, r + 16) });
        }
        for r in 6..12 { c.i(if r == 6 { Add(r, r + 10) } else { Adc(r, r + 10) }); }
        c.seq(&[Pop(31), Pop(30)]);
        for r in 6..12 { c.i(Std(false, r - 6, r)); }
        if p.watch == watch::Level::Off {
            c.seq(&[Lds(24, l.selected), Ldi(25, 0)]);
            fault_cell(&mut c, l, q, watch::FAULT_OVERRUN);
        } else {
            c.seq(&[Ldi(30, l.record as u8), Ldi(31, (l.record >> 8) as u8)]);
            for offset in 0..4 { c.seq(&[Lds(26, l.stamp + u16::from(offset)), Std(false, offset + 1, 26)]); }
            c.seq(&[Ldi(26, watch::FAULT), St(Ptr::Z, 26),
                Ldi(26, watch::FAULT_OVERRUN), Std(false, 5, 26),
                Lds(26, l.selected), Std(false, 6, 26), Std(false, 7, 2),
                Ldi(26, watch::FAULT | 0x80), Std(false, 8, 26)]);
            c.store(l.record_count, 2); push(&mut c, l, q, b);
        }
        c.seq(&[Lds(30, l.args), Lds(31, l.args + 1)]);
        c.i(Ijmp);
        b.overrun = common + 4 + c.t - BOOKKEEP;
        sy.overrun_end = c.a.here();
    }
    c.a.bind(rx);
    if let Some(serial) = p.serial {
        emit_rx_bookkeep(c.a, l, q, sy, serial.entry, b);
    }
}
