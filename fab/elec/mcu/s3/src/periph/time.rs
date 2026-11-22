//! SYSTIMER, RTC_CNTL reset/watchdogs, and the timer-group watchdogs.
//!
//! Accesses occur at instruction start. The machine first processes events
//! due at or before that tick: a feed cannot beat an expiry at its own tick.
//! This is a model convention, not a hardware precedence claim. Watchdog
//! writes within the profile's synchronization window land immediately after
//! that watchdog's expiry action. Its reset action discards its late writes.
//! This is the brief's deterministic late-write convention, not a measured latency
//! (facts-machine.md §5, NOT FOUND: write synchronization latency). Each late
//! write records a note at its instruction's start tick; snapshots keep notes.
//! SYSTIMER loads take effect at the access tick (TRM p.636, "immediately").
//!
//! Counters are closed forms; work scales with accesses and alarms/stages,
//! never oscillator edges. Retained clocks use an absolute epoch internally
//! so resetting machine now() does not lose phase. Fidelity scope:
//! fable-answers.md:1624–1640, SYSTIMER/TIMG/RWDT/SWD rows.

use fab_elec_mcu_emu::Clock;

use super::{Access, ResetRequest, ResetScope};
use crate::Junk;

// facts.md §3, TRM pp.408,530; machine.rs uses the same 480 MHz grid.
const HZ: u64 = 480_000_000;
const SYS: u32 = 0x6002_3000;
const RTC: u32 = 0x6000_8000;
// ESP-IDF v6.1.1, Apache-2.0, rtc_cntl_reg.h:1091–1121,2189–2219:
// eight independent R/W scratch words, all reset to zero.
const RTC_STORE: [u32; 8] = [0x50, 0x54, 0x58, 0x5c, 0xc0, 0xc4, 0xc8, 0xcc];
const RTC_STORE_RESET: u32 = 0;
const TIMG: [u32; 2] = [0x6001_f000, 0x6002_0000];
// facts-machine.md §1, TRM pp.634–637: 52-bit counters and half-range.
const MASK52: u64 = (1 << 52) - 1;
const HALF: u64 = 1 << 51;
// facts-machine.md §5, TRM pp.677–678: digital and analog unlock keys.
const WKEY: u32 = 0x50d8_3aa1;
const SWKEY: u32 = 0x8f1d_312a;
const SYNC: &str = "watchdog fed inside its sync window; hardware timing undocumented; modelled as late";
// Timing races in ordinary firmware are modelled on the adverse side with a
// note, never a stop (facts-machine.md, modelled assumptions).
const FED_PENDING: &str = "watchdog fed with its interrupt pending; effect on the interrupt undocumented; modelled as still pending";
const SWD_FED_PENDING: &str = "super watchdog fed with its feed request pending; effect on the request undocumented; modelled as still pending";
const BEHIND: &str = "watchdog timeout set at or behind its count; hardware behaviour undocumented; modelled as immediate expiry";
const COINCIDE: &str = "reset causes coincide; hardware priority undocumented; modelled as the widest reset";
// Access types: facts-machine.md §1, TRM pp.1520–1521. Neither WO reads,
// RO writes nor writes to reserved fields have a documented value/effect.
const RO: &str = "write to read-only time register is undocumented";
const WO: &str = "read of write-only time register is undocumented";
const RESERVED: &str = "reserved time-register field is undocumented";

/// Explicit board and ROM-handoff assumptions, to be checked by a chip probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeProfile {
    /// M: TRM pp.634–635 gives no phase for XTAL/3, XTAL/2 alternation.
    pub systimer_first_long: bool,
    /// M: TRM p.530 calls RC_SLOW approximately 136 kHz; numerator/denominator Hz.
    pub rc_slow_hz: (u64, u64),
    /// M: eFuse WDT_DELAY_SEL, IDF efuse_reg.h:382–389, reset/default 0.
    pub wdt_delay_sel: u8,
    /// M: TRM p.677 says SWD expires "slightly less than one second".
    pub swd_timeout_ticks: u64,
    /// M: TRM p.677 says SWD requests feeding "about 100 ms" before expiry.
    pub swd_request_lead_ticks: u64,
    /// M: facts-machine.md §5 NOT FOUND: watchdog write synchronization latency.
    /// Inclusive window, in the watchdog's own (prescaled for MWDT) cycles.
    pub wdt_sync_cycles: u64,
    /// M: facts-machine.md §5 NOT FOUND: MWDT divider phase after feeding.
    /// True restarts its divider with a full cycle; false keeps its phase.
    pub mwdt_rephase_on_feed: bool,
    /// M: facts-machine.md §5 NOT FOUND: divider phase after configuration
    /// changes. True starts a full cycle at an idle prescale/APB write; false
    /// retains the oscillator origin while adopting the new period.
    pub mwdt_rephase_on_config: bool,
    /// M: TRM pp.675–676 specifies enabling progression, but not re-enabling
    /// a previously disabled digital counter. True restarts stage/count,
    /// false resumes.
    pub wdt_restart_on_enable: bool,
}

impl Default for TimeProfile {
    fn default() -> Self {
        Self {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: HZ * 9 / 10, swd_request_lead_ticks: HZ / 10,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false,
            mwdt_rephase_on_config: true, wdt_restart_on_enable: true,
        }
    }
}

fn junk52(junk: &mut Junk) -> u64 {
    ((u64::from(junk.u32()) << 32) | u64::from(junk.u32())) & MASK52
}

fn fields(value: u32, mask: u32) -> Result<(), &'static str> {
    if value & !mask != 0 { Err(RESERVED) } else { Ok(()) }
}

/// Ticks from UPDATE to VALUE_VALID. Observed (board sessions 20-21, the analog check's clock rounds): a unit-0
/// read (UPDATE, VALUE_VALID polls, the low word) took 0.437 us at a 160 MHz CPU and 0.424 us at 240 MHz (229 and
/// 236 reads in 100 us of the systimer, warm), with 2 and 3 VALID polls a read. The model's register accesses take
/// one cycle, where the chip's take part of that time, so the model puts the read's whole cost in VALID. With the
/// clock check's loop, 175-186 ticks of the 480 MHz grid give the board's read rate at 160 MHz and 181-188 at
/// 240 MHz: 183 (0.381 us). A read then takes the chip's time at both clocks, and the model polls more often than
/// the chip, never fewer times.
const UPDATE_TO_VALID_TICKS: u64 = 183;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Unit { count: u64, edge: u64, load: u64, captured: u64, valid_at: u64 }

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Comparator {
    target: u64,
    conf: u32,
    loaded_target: u64,
    period: u64,
    real: u64,
    armed: bool,
    // The one-shot is documented, but whether it clears WORK_EN is not.
    work_unknown: bool,
    due: Option<u64>,
}

#[derive(Clone, PartialEq, Eq)]
struct Systimer {
    start: u64,
    first_long: bool,
    conf: u32,
    units: [Unit; 2],
    comps: [Comparator; 3],
    ena: u32,
    raw: u32,
    date: u32,
}

impl Systimer {
    fn new(tick: u64, profile: &TimeProfile, junk: &mut Junk) -> Self {
        // Register defaults: facts-machine.md §1 table, systimer_reg.h:16–631.
        // The live count at ROM handoff is not a documented captured VALUE.
        Self {
            start: tick, first_long: profile.systimer_first_long, conf: 0x4600_0000,
            units: std::array::from_fn(|_| Unit {
                count: junk52(junk), edge: 0, load: 0, captured: 0, valid_at: u64::MAX,
            }),
            comps: [Comparator::default(); 3], ena: 0, raw: 0, date: 0x0201_2251,
        }
    }

    // TRM pp.634–635: 40 MHz XTAL /3 and /2, hence 36/24 on this grid.
    // WORK_EN gates count increments, not the shared CNT_CLK oscillator
    // (TRM pp.635–636); disabling a unit does not restart the oscillator.
    fn edges(&self, tick: u64) -> u64 {
        let dt = tick - self.start;
        dt / 60 * 2 + u64::from(dt % 60 >= if self.first_long { 36 } else { 24 })
    }

    fn edge_tick(&self, edge: u64) -> u64 {
        self.start + edge / 2 * 60 + if edge % 2 == 0 { 0 } else if self.first_long { 36 } else { 24 }
    }

    fn working(&self, n: usize) -> bool { self.conf & (1 << (30 - n)) != 0 }
    fn work_bit(n: usize) -> u32 { 1 << (24 - n) }
    fn count(&self, n: usize, tick: u64) -> u64 {
        let u = self.units[n];
        (u.count + if self.working(n) { self.edges(tick) - u.edge } else { 0 }) & MASK52
    }

    fn distance(count: u64, target: u64) -> Result<u64, &'static str> {
        // Exactly the three rows of TRM p.637 Table 11.4-2; do not invent
        // a complete modular predicate for the table's missing cases.
        if count <= target { return Ok(target - count); }
        let delta = count - target;
        if delta == HALF - 1 {
            return Err("SYSTIMER comparison at the disputed Table 11.4-2 boundary");
        }
        if delta > HALF - 1 { return Ok((target.wrapping_sub(count)) & MASK52); }
        if (count < HALF) != (target < HALF) {
            return Err("SYSTIMER comparison across the undocumented half-range boundary");
        }
        Ok(0)
    }

    fn schedule(&mut self, n: usize, tick: u64) -> Result<(), &'static str> {
        self.comps[n].due = None;
        let c = self.comps[n];
        if !c.armed || self.conf & Self::work_bit(n) == 0 { return Ok(()); }
        let unit = (c.conf >> 31) as usize;
        let distance = Self::distance(self.count(unit, tick), c.real)?;
        if distance == 0 { self.comps[n].due = Some(tick); }
        else if self.working(unit) { self.comps[n].due = Some(self.edge_tick(self.edges(tick) + distance)); }
        Ok(())
    }

    fn arm(&mut self, n: usize, tick: u64) -> Result<(), &'static str> {
        let c = self.comps[n];
        let periodic = c.conf & (1 << 30) != 0;
        if periodic && c.period == 0 { return Err("SYSTIMER zero period is undocumented"); }
        self.comps[n].real = if periodic {
            (self.count((c.conf >> 31) as usize, tick) + c.period) & MASK52
        } else { c.loaded_target };
        self.comps[n].armed = true;
        self.comps[n].work_unknown = false;
        self.schedule(n, tick)
    }

    fn read(&self, off: u32, tick: u64) -> Result<u32, &'static str> {
        // Complete register map: facts-machine.md §1 table / IDF systimer_reg.h.
        Ok(match off {
            0 => {
                if self.comps.iter().any(|c| c.work_unknown) {
                    return Err("SYSTIMER WORK_EN after one-shot firing is undocumented");
                }
                self.conf
            }
            4 | 8 => u32::from(tick >= self.units[(off / 4 - 1) as usize].valid_at) << 29,
            0x0c..=0x18 => {
                let u = self.units[((off - 0x0c) / 8) as usize];
                if off & 4 != 0 { (u.load >> 32) as u32 } else { u.load as u32 }
            }
            0x1c..=0x30 => {
                let c = self.comps[((off - 0x1c) / 8) as usize];
                if off & 4 != 0 { (c.target >> 32) as u32 } else { c.target as u32 }
            }
            0x34..=0x3c => self.comps[((off - 0x34) / 4) as usize].conf,
            0x40..=0x4c => {
                let u = self.units[((off - 0x40) / 8) as usize];
                if u.valid_at != u64::MAX && tick < u.valid_at {
                    return Err("SYSTIMER VALUE read before VALUE_VALID is undocumented");
                }
                if off & 4 == 0 { (u.captured >> 32) as u32 } else { u.captured as u32 }
            }
            0x50..=0x60 | 0x6c => return Err(WO),
            0x64 => self.ena, 0x68 => self.raw, 0x70 => self.raw & self.ena,
            0x74..=0x88 => {
                let c = self.comps[((off - 0x74) / 8) as usize];
                if off & 4 != 0 { c.real as u32 } else { (c.real >> 32) as u32 }
            }
            0xfc => self.date,
            _ => return Err("SYSTIMER register not modelled"),
        })
    }

    fn write(&mut self, off: u32, value: u32, tick: u64) -> Result<(), &'static str> {
        match off {
            0 => {
                // IDF bit 0 has no reconciled TRM clock-force semantics (§1).
                if value & 1 != 0 { return Err("SYSTIMER CLK_FO semantics are undocumented"); }
                fields(value, 0xffc0_0000)?;
                let old = self.conf;
                for n in 0..2 {
                    self.units[n].count = self.count(n, tick);
                    self.units[n].edge = self.edges(tick);
                }
                self.conf = value;
                for n in 0..3 {
                    let bit = Self::work_bit(n);
                    if value & bit == 0 {
                        self.comps[n].armed = false;
                        self.comps[n].work_unknown = false;
                    } else if old & bit == 0 { self.arm(n, tick)?; }
                    else if self.comps[n].work_unknown {
                        return Err("SYSTIMER WORK_EN after one-shot firing is undocumented");
                    }
                }
            }
            4 | 8 => {
                fields(value, (1 << 30) | (1 << 29))?;
                // WTC is cleared by its corresponding WT, not direct W1C
                // (TRM pp.1520–1521; facts-machine.md §1 final paragraph).
                if value & (1 << 29) != 0 { return Err("direct SYSTIMER VALID write is undocumented"); }
                if value & (1 << 30) != 0 {
                    let n = (off / 4 - 1) as usize;
                    // TRM pp.636,642,644: capture, then VALID; the latency is the board's
                    // (UPDATE_TO_VALID_TICKS). The count is the one at UPDATE.
                    self.units[n].captured = self.count(n, tick);
                    self.units[n].valid_at = tick + UPDATE_TO_VALID_TICKS;
                }
            }
            0x0c..=0x18 => {
                let u = &mut self.units[((off - 0x0c) / 8) as usize];
                if off & 4 != 0 { fields(value, 0xfffff)?; u.load = (u.load as u32 as u64) | (u64::from(value) << 32); }
                else { u.load = (u.load & !0xffff_ffff) | u64::from(value); }
            }
            0x1c..=0x30 => {
                let c = &mut self.comps[((off - 0x1c) / 8) as usize];
                if off & 4 != 0 { fields(value, 0xfffff)?; c.target = (c.target as u32 as u64) | (u64::from(value) << 32); }
                else { c.target = (c.target & !0xffff_ffff) | u64::from(value); }
            }
            0x34..=0x3c => {
                fields(value, 0xc3ff_ffff)?;
                let n = ((off - 0x34) / 4) as usize;
                let old = self.comps[n].conf;
                self.comps[n].conf = value;
                if (old ^ value) & 0xc000_0000 != 0 && self.conf & Self::work_bit(n) != 0 {
                    self.arm(n, tick)?;
                }
            }
            0x50..=0x58 => {
                fields(value, 1)?;
                if value & 1 != 0 {
                    let n = ((off - 0x50) / 4) as usize;
                    self.comps[n].loaded_target = self.comps[n].target;
                    self.comps[n].period = u64::from(self.comps[n].conf & 0x03ff_ffff);
                    self.comps[n].real = self.comps[n].loaded_target;
                    if self.conf & Self::work_bit(n) != 0 { self.arm(n, tick)?; }
                }
            }
            0x5c | 0x60 => {
                fields(value, 1)?;
                if value & 1 != 0 {
                    let n = ((off - 0x5c) / 4) as usize;
                    self.units[n].count = self.units[n].load;
                    self.units[n].edge = self.edges(tick);
                }
            }
            0x64 => { fields(value, 7)?; self.ena = value; }
            0x68 => return Err("direct SYSTIMER RAW write is undocumented"),
            0x6c => { fields(value, 7)?; self.raw &= !value; }
            0x40..=0x4c | 0x70..=0x88 => return Err(RO),
            // DATE is R/W, with no documented effect beyond its version word
            // (TRM p.653 Register 11.36; facts-machine.md §1).
            0xfc => self.date = value,
            _ => return Err("SYSTIMER register not modelled"),
        }
        for n in 0..3 { self.schedule(n, tick)?; }
        Ok(())
    }

    fn fire(&mut self, n: usize, tick: u64) -> Result<(), &'static str> {
        // TRM pp.637–638: held interrupt, one alarm or start+n*period.
        self.raw |= 1 << n;
        if self.comps[n].conf & (1 << 30) != 0 {
            self.comps[n].real = (self.comps[n].real + self.comps[n].period) & MASK52;
            self.schedule(n, tick)?;
        } else {
            self.comps[n].armed = false;
            self.comps[n].work_unknown = true;
            self.comps[n].due = None;
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Watchdog {
    // kind 0 = RWDT; 1/2 = MWDT0/1.
    kind: usize,
    config: u32,
    prescale: u16,
    holds: [u32; 4],
    key: u32,
    clock: Clock,
    stage: usize,
    count: u64,
    anchor: u64,
    due: Option<u64>,
    ena: u32,
    raw: u32,
    // Modelled assumptions made since the last drain (Time tags their tick).
    notes: Vec<&'static str>,
    // Writes accepted while unlocked, ordered after this stage's expiry.
    late: Vec<(usize, u32)>,
}

/// What a refused `Watchdog::write` of FEED (6) or KEY (7) may have changed before it returned
/// Err: `apply` re-counts (count, anchor), a feed resets the stage (and an MWDT's clock phase)
/// and may note, and a write inside the sync window queues. KEY writes never fail.
#[derive(Clone, Copy)]
struct FeedUndo {
    count: u64,
    anchor: u64,
    stage: usize,
    clock: Clock,
    due: Option<u64>,
    notes: usize,
    late: usize,
}

impl Watchdog {
    fn feed_undo(&self) -> FeedUndo {
        FeedUndo {
            count: self.count, anchor: self.anchor, stage: self.stage, clock: self.clock, due: self.due,
            notes: self.notes.len(), late: self.late.len(),
        }
    }

    fn undo(&mut self, u: FeedUndo) {
        (self.count, self.anchor, self.stage, self.clock, self.due) = (u.count, u.anchor, u.stage, u.clock, u.due);
        self.notes.truncate(u.notes);
        self.late.truncate(u.late);
    }

    fn new(kind: usize, tick: u64, p: &TimeProfile, apb: u64, junk: &mut Junk) -> Self {
        // facts-machine.md §5 tables; rtc_cntl_reg.h:1988–2115 and
        // timer_group_reg.h:318–475. Readable EN/STG remain reset-zero;
        // boot override is effective control only (brief's handoff contract).
        let mut w = Self {
            kind, config: if kind == 0 { 0x0001_3214 } else { 0x0004_c000 },
            prescale: 1,
            holds: if kind == 0 { [200_000, 80_000, 0xfff, 0xfff] }
                else { [26_000_000, 0x7ff_ffff, 0xf_ffff, 0xf_ffff] },
            key: junk.u32(),
            clock: if kind == 0 { Clock::new(p.rc_slow_hz.0, p.rc_slow_hz.1, tick, HZ) }
                else { Clock::new(HZ, apb, tick, HZ) },
            stage: 0, count: 0, anchor: 0, due: None, ena: 0, raw: 0,
            notes: Vec::new(), late: Vec::new(),
        };
        if w.running() { w.count = u64::from(junk.u32()) % w.hold(p); }
        w.schedule(tick, p);
        w
    }

    fn boot(&self) -> bool {
        self.kind < 2 && self.config & (1 << if self.kind == 0 { 12 } else { 14 }) != 0
    }
    fn running(&self) -> bool { self.config & (1 << 31) != 0 || self.boot() }
    fn action(&self) -> u32 {
        if self.boot() && self.stage == 0 { return if self.kind == 0 { 4 } else { 3 }; }
        if self.kind == 0 { (self.config >> (28 - self.stage * 3)) & 7 }
        else { (self.config >> (29 - self.stage * 2)) & 3 }
    }
    fn hold(&self, p: &TimeProfile) -> u64 {
        let h = u64::from(self.holds[self.stage]);
        if self.kind == 0 && self.stage == 0 { h << (p.wdt_delay_sel + 1) } else { h }
    }
    fn edge(&self, tick: u64) -> u64 { self.clock.edges_through(tick) - 1 }
    fn count(&self, tick: u64) -> u64 {
        self.count + if self.running() { self.edge(tick) - self.anchor } else { 0 }
    }
    fn schedule(&mut self, tick: u64, p: &TimeProfile) {
        self.due = if !self.running() { None } else {
            let remaining = self.hold(p).saturating_sub(self.count(tick));
            Some(if remaining == 0 { tick } else { self.clock.edge(self.edge(tick) + remaining) })
        };
    }
    fn in_sync_window(&self, tick: u64, p: &TimeProfile) -> bool {
        // Inclusive beginning, exclusive expiry: events at the access tick
        // have already happened. Count is in this watchdog's own clock cycles.
        self.running() && self.due.is_some_and(|due| tick < due)
            && self.hold(p).saturating_sub(self.count(tick)) <= p.wdt_sync_cycles
    }

    fn valid_stage(&self, p: &TimeProfile) -> Result<(), &'static str> {
        if !self.running() { return Ok(()); }
        if self.hold(p) == 0 {
            return Err(if self.action() == 0 { "watchdog off stage with zero hold is undocumented" }
                else { "watchdog zero stage hold is undocumented" });
        }
        // §13.2.1's 32-bit counter cannot silently become a 36-bit one.
        if self.hold(p) > u64::from(u32::MAX) { return Err("RWDT scaled hold exceeds its documented 32-bit counter"); }
        if self.action() > 4 { return Err("RWDT stage action is undocumented"); }
        Ok(())
    }

    fn write(&mut self, reg: usize, value: u32, tick: u64, p: &TimeProfile, apb: u64) -> Result<(), &'static str> {
        // Abstract register numbering: CONFIG0, prescaler, HOLD0..3, FEED, KEY.
        if reg == 7 { self.key = value; return Ok(()); }
        // TRM p.677 explicitly ignores all protected config/feed writes.
        if self.key != WKEY { return Ok(()); }
        let old_running = self.running();
        let changing = match reg {
            0 => {
                let mut proposed = self.clone();
                proposed.config = value;
                proposed.running() != old_running || proposed.action() != self.action()
                    || (self.action() == 2 && (self.config ^ value)
                        & (1 << if self.kind == 0 { 11 } else { 13 }) != 0)
            }
            1 => u32::from(self.prescale) != value >> 16,
            2..=5 => reg - 2 == self.stage && self.holds[reg - 2] != value,
            6 => true,
            _ => unreachable!(),
        };
        if changing && self.in_sync_window(tick, p) {
            self.notes.push(SYNC);
            self.late.push((reg, value));
            return Ok(());
        }
        self.apply(reg, value, tick, p, apb)
    }

    // Already accepted under the unlock key, including writes followed by a
    // relock before expiry. Never check the key again at synchronization time.
    fn apply(&mut self, reg: usize, value: u32, tick: u64, p: &TimeProfile, apb: u64) -> Result<(), &'static str> {
        let old_running = self.running();
        self.count = self.count(tick);
        self.anchor = self.edge(tick);
        match reg {
            0 => {
                if self.kind != 0 { fields(value, 0xff9f_f000)?; }
                // IDF-only CHIP_RESET_EN has no documented scope truth table.
                if self.kind == 0 && value & (1 << 8) != 0 {
                    return Err("RWDT CHIP_RESET_EN scope is undocumented");
                }
                self.config = value;
                if !old_running && self.running() && p.wdt_restart_on_enable {
                    self.stage = 0; self.count = 0;
                }
            }
            1 => {
                fields(value, 0xffff_0000)?;
                if value >> 16 == 0 { return Err("MWDT prescale of zero is undocumented"); }
                if old_running && value >> 16 != u32::from(self.prescale) {
                    return Err("MWDT divider change while counting is undocumented");
                }
                if value >> 16 != u32::from(self.prescale) {
                    self.prescale = (value >> 16) as u16;
                    self.idle_clock(apb, tick, p);
                }
            }
            2..=5 => self.holds[reg - 2] = value,
            6 => {
                // TRM pp.615,676 disagree for a RWDT feed with bit 31 clear.
                if self.kind == 0 && value & (1 << 31) == 0 {
                    return Err("RWDT feed value without bit 31 is undocumented");
                }
                // A feed is documented to reset stage/count; its effect on a
                // pending interrupt is NOT FOUND (§5): the interrupt stays.
                if self.raw != 0 { self.notes.push(FED_PENDING); }
                self.stage = 0; self.count = 0;
                if self.kind != 0 && p.mwdt_rephase_on_feed {
                    self.clock.start = tick; self.anchor = 0;
                }
            }
            _ => unreachable!(),
        }
        self.valid_stage(p)?;
        // Whether the write lands before or after the count passes the new
        // hold is a race; schedule() expires it at this tick.
        if self.running() && self.count >= self.hold(p) {
            self.notes.push(BEHIND);
        }
        self.schedule(tick, p);
        Ok(())
    }

    fn idle_clock(&mut self, apb: u64, tick: u64, p: &TimeProfile) {
        let start = if p.mwdt_rephase_on_config { tick } else { self.clock.start };
        self.clock = Clock::new(HZ, apb * u64::from(self.prescale), start, HZ);
        self.anchor = self.edge(tick);
    }

    fn expire(&mut self, tick: u64, p: &TimeProfile, apb: u64) -> Result<Option<(u8, ResetScope)>, &'static str> {
        self.valid_stage(p)?;
        let action = self.action();
        // Reset actions and causes: facts-machine.md §§4–5, TRM pp.528,676.
        let reset = match action {
            0 => None,
            1 => {
                self.raw = if self.kind == 0 { 1 << 3 } else { 1 << 2 };
                None
            }
            2 => {
                let enabled = self.config & (1 << if self.kind == 0 { 11 } else { 13 }) != 0;
                if !enabled { return Err("watchdog CPU reset action with PROCPU_RESET_EN clear is undocumented"); }
                Some(([0x0d, 0x0b, 0x11][self.kind], ResetScope::Cpu))
            }
            3 => Some(([0x09, 0x07, 0x08][self.kind], ResetScope::Core)),
            4 => Some((0x10, ResetScope::System)),
            _ => return Err("RWDT stage action is undocumented"),
        };
        // TRM p.676: every expiry resets count and advances in the 0..3 loop.
        // Apply that generic expiry rule before a reset. A reset itself does
        // not perform an additional feed of a retained RTC/CPU-domain timer.
        self.stage = (self.stage + 1) % 4;
        self.count = 0;
        self.anchor = self.edge(tick);
        self.schedule(tick, p);
        let late = std::mem::take(&mut self.late);
        if reset.is_none() {
            for (reg, value) in late { self.apply(reg, value, tick, p, apb)?; }
        }
        Ok(reset)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Rtc {
    options: u32,
    reset_state: u32,
    // INFERRED retention by domain, facts-machine.md:356: Cpu/Core retain
    // RTC; System and initial Chip construction restore header defaults.
    store: [u32; 8],
    // Same INFERRED RTC retention for digital power and isolation controls.
    dig_pwc: u32,
    dig_iso: u32,
    wdt: Watchdog,
    swd_conf: u32,
    swd_key: u32,
    swd_expiry: u64,
    swd_request: Option<u64>,
    ena: u32,
    swd_raw: u32,
    // RTC_CNTL_DATE_REG: R/W. Its [18:13] are the LDO slave power-downs (rtc_cntl_reg.h:3679-3694),
    // which change the regulator, not timing; the regulator is not modelled.
    date: u32,
    notes: Vec<&'static str>,
}

impl Rtc {
    fn new(tick: u64, p: &TimeProfile, apb: u64, junk: &mut Junk) -> Self {
        let wdt = Watchdog::new(0, tick, p, apb, junk);
        let swd_key = junk.u32();
        let elapsed = u64::from(junk.u32()) % p.swd_timeout_ticks;
        let expiry = tick + p.swd_timeout_ticks - elapsed;
        Self {
            // rtc_cntl_reg.h:22–160 and :473–569; flags reset to zero,
            // vector selectors to one. Power-on records cause 1 (TRM p.528).
            options: 0x1c00_a000, reset_state: 0x3000 | (1 << 6) | 1,
            store: [RTC_STORE_RESET; 8],
            // ESP-IDF v6.1.1 rtc_cntl_reg.h:1774–1896.
            dig_pwc: 0x0054_5010, dig_iso: 0xaa80_5080,
            wdt,
            // rtc_cntl_reg.h:2117–2173: width=300, other SWD controls zero.
            swd_conf: 300 << 18, swd_key, swd_expiry: expiry,
            swd_request: Some(expiry.saturating_sub(p.swd_request_lead_ticks).max(tick)),
            // rtc_cntl_reg.h:3683: DATE resets to 28'h2101271, so all six LDO slaves on.
            ena: 0, swd_raw: 0, date: 0x0210_1271, notes: Vec::new(),
        }
    }
    fn swd_running(&self) -> bool { self.swd_conf & 0xc000_0000 == 0 }
    fn next_swd(&self) -> Option<u64> {
        self.swd_running().then_some(self.swd_request.unwrap_or(self.swd_expiry))
    }
    fn raw(&self) -> u32 { self.wdt.raw | self.swd_raw }

    fn read(&self, off: u32) -> Result<u32, &'static str> {
        if let Some(n) = RTC_STORE.iter().position(|&a| a == off) { return Ok(self.store[n]); }
        // facts-machine.md §§4–5: RTC subset only.
        Ok(match off {
            0 => self.options,
            0x38 => self.reset_state,
            0x40 => self.ena, 0x44 => self.raw(), 0x48 => self.raw() & self.ena,
            0x4c | 0xac => return Err(WO),
            0x90 => self.dig_pwc,
            0x94 => self.dig_iso,
            0x98 => self.wdt.config,
            0x9c..=0xa8 => self.wdt.holds[((off - 0x9c) / 4) as usize],
            0xb0 => self.wdt.key,
            0xb4 => self.swd_conf,
            0xb8 => self.swd_key,
            0x1fc => self.date,
            _ => return Err("RTC_CNTL register not modelled"),
        })
    }

    fn write(&mut self, off: u32, value: u32, tick: u64, p: &TimeProfile, apb: u64) -> Result<Option<(u8, ResetScope)>, &'static str> {
        if let Some(n) = RTC_STORE.iter().position(|&a| a == off) {
            self.store[n] = value;
            return Ok(None);
        }
        match off {
            0 => {
                // OPTIONS0 only software CPU0/Core reset and low stall keys.
                // Other field actions are outside this RTC subset. Permit a
                // pulse-only write or an RMW preserving those fields
                // (rtc_cntl_reg.h:22–160; TRM p.587).
                let supported = (1 << 31) | (1 << 5) | 15;
                let other = value & !supported;
                if other != 0 && other != self.options & !supported {
                    return Err("RTC_CNTL OPTIONS0 field not modelled");
                }
                self.options = (self.options & !15) | (value & 15);
                return match (value & (1 << 31) != 0, value & (1 << 5) != 0) {
                    (true, true) => Err("simultaneous software reset causes are undocumented"),
                    (true, false) => Ok(Some((0x03, ResetScope::Core))),
                    (false, true) => Ok(Some((0x0c, ResetScope::Cpu))),
                    _ => Ok(None),
                };
            }
            0x38 => {
                // rtc_cntl_reg.h:473–569; TRM p.593: set CLR to clear flags.
                // Debug halt, dreset masks, and vector selection are not modelled.
                if (value ^ self.reset_state) & 0x030c_3000 != 0 {
                    return Err("RTC_CNTL reset mask, vector or OCD halt not modelled");
                }
                fields(value, 0x03ff_ffff)?;
                self.reset_state &= !(((value >> 16) & 3) << 14);
                self.reset_state &= !(((value >> 22) & 3) << 20);
            }
            0x40 => {
                fields(value, (1 << 3) | (1 << 15))?;
                if (value ^ self.ena) & (1 << 3) != 0 && self.wdt.key != WKEY {
                    return Err("shared watchdog interrupt-register write protection is undocumented");
                }
                if (value ^ self.ena) & (1 << 15) != 0 && self.swd_key != SWKEY {
                    return Err("shared watchdog interrupt-register write protection is undocumented");
                }
                self.ena = value;
            }
            0x44 | 0x48 => return Err(RO),
            // §5 NOT FOUND: WO "clears" does not specify a write-value rule.
            0x4c => return Err("RTC_CNTL interrupt clear write-value rule is undocumented"),
            0x90 => {
                if (value ^ self.dig_pwc) & !(1 << 17) != 0 {
                    return Err("RTC_CNTL DIG_PWC field other than WIFI_FORCE_PD not modelled");
                }
                self.dig_pwc = value;
            }
            0x94 => {
                if value & (1 << 10) != 0 {
                    return Err("RTC_CNTL DIG_ISO pad autohold clear not modelled");
                }
                if (value ^ self.dig_iso) & !(1 << 28) != 0 {
                    return Err("RTC_CNTL DIG_ISO field other than WIFI_FORCE_ISO not modelled");
                }
                self.dig_iso = value;
            }
            0x98 => self.wdt.write(0, value, tick, p, apb)?,
            0x9c..=0xa8 => self.wdt.write(((off - 0x9c) / 4 + 2) as usize, value, tick, p, apb)?,
            0xac => self.wdt.write(6, value, tick, p, apb)?,
            0xb0 => self.wdt.write(7, value, tick, p, apb)?,
            0xb4 => {
                // TRM p.678 requires unlock for FEED; rtc_cntl_reg.h:15 calls
                // SWKEY the write-enable key. No explicit wrong-key ignore
                // or non-key relock rule is given for SWD (§5 NOT FOUND).
                let feed = value & (1 << 29) != 0;
                if self.swd_key != SWKEY {
                    if feed { return Err("SWD feed without its unlock key is undocumented"); }
                    return Err("SWD configuration write protection is undocumented");
                }
                fields(value, 0xfffe_0003)?;
                if value & (1 << 17) != 0 { return Err("SWD BYPASS_RST truth table is undocumented"); }
                if (value ^ self.swd_conf) & 0x0ffc_0000 != 0 {
                    return Err("SWD SIGNAL_WIDTH units are undocumented");
                }
                if feed && (self.swd_raw != 0 || self.swd_conf & 2 != 0) { self.notes.push(SWD_FED_PENDING); }
                let running = self.swd_running();
                self.swd_conf = (self.swd_conf & 3) | (value & 0xcffe_0000);
                if value & (1 << 28) != 0 { self.swd_conf &= !1; }
                // TRM pp.677–678 specifies feeding, but not elapsed analog
                // time after DISABLE/AUTO_FEED is cleared without a feed.
                if !running && self.swd_running() && !feed {
                    return Err("SWD re-enable timing without a feed is undocumented");
                }
                if feed {
                    self.swd_expiry = tick + p.swd_timeout_ticks;
                    self.swd_request = Some(self.swd_expiry - p.swd_request_lead_ticks);
                }
            }
            0xb8 => self.swd_key = value,
            0x1fc => self.date = value,
            _ => return Err("RTC_CNTL register not modelled"),
        }
        Ok(None)
    }
}

#[derive(Clone)]
pub(crate) struct Time {
    profile: TimeProfile,
    apb: u64,
    handoff_apb: u64,
    epoch: u64,
    systimer: Systimer,
    rtc: Rtc,
    mwdt: [Watchdog; 2],
    reset_request: Option<ResetRequest>,
    notes: Vec<(u64, &'static str)>,
}

impl Time {
    pub(super) fn same_state(&self, other: &Self) -> Option<&'static str> {
        let Self {
            profile, apb, handoff_apb, epoch, systimer, rtc, mwdt: [mwdt0, mwdt1], reset_request, notes,
        } = self;
        if profile != &other.profile { return Some("periph.time.profile"); }
        if apb != &other.apb { return Some("periph.time.apb"); }
        if handoff_apb != &other.handoff_apb { return Some("periph.time.handoff_apb"); }
        if epoch != &other.epoch { return Some("periph.time.epoch"); }
        if systimer != &other.systimer { return Some("periph.time.systimer"); }
        if rtc != &other.rtc { return Some("periph.time.rtc"); }
        if mwdt0 != &other.mwdt[0] { return Some("periph.time.mwdt0"); }
        if mwdt1 != &other.mwdt[1] { return Some("periph.time.mwdt1"); }
        if reset_request != &other.reset_request { return Some("periph.time.reset_request"); }
        if notes != &other.notes { return Some("periph.time.notes"); }
        None
    }

    pub fn interrupt_sources(&self) -> u128 {
        // facts-machine.md:25 (§1): TARGET0/1/2 are held level sources
        // 57/58/59; INFERRED delivery is RAW AND ENA.
        u128::from(self.systimer.raw & self.systimer.ena & 7) << 57
    }

    pub fn interrupt_event_source(&self, tick: u64) -> Option<usize> {
        self.systimer.comps.iter().position(|c| c.due == Some(self.epoch + tick)).map(|n| 57 + n)
    }

    pub fn new(profile: &TimeProfile, apb_ticks: u64, junk: &mut Junk) -> Self {
        assert!(profile.wdt_delay_sel <= 3, "WDT_DELAY_SEL is a two-bit eFuse field");
        assert!(profile.swd_timeout_ticks > profile.swd_request_lead_ticks, "SWD request must precede expiry");
        assert!(apb_ticks != 0, "APB period must be nonzero");
        Self {
            profile: profile.clone(), apb: apb_ticks, handoff_apb: apb_ticks, epoch: 0,
            systimer: Systimer::new(0, profile, junk),
            rtc: Rtc::new(0, profile, apb_ticks, junk),
            mwdt: std::array::from_fn(|n| Watchdog::new(n + 1, 0, profile, apb_ticks, junk)),
            reset_request: None, notes: Vec::new(),
        }
    }

    fn decode(addr: u32) -> Option<(usize, u32)> {
        // facts.md §3 / TRM p.408 Table 4.3-3: 4 KiB peripheral blocks.
        [SYS, RTC, TIMG[0], TIMG[1]].into_iter().enumerate().find_map(|(n, base)| {
            addr.checked_sub(base).filter(|&off| off < 0x1000).map(|off| (n, off))
        })
    }

    pub fn read(&mut self, addr: u32, tick: u64) -> Access<u32> {
        let (block, off) = Self::decode(addr)?;
        Some(match block {
            0 => self.systimer.read(off, self.epoch + tick),
            1 => self.rtc.read(off),
            _ => {
                let w = &self.mwdt[block - 2];
                match off {
                    0x48 => Ok(w.config), 0x4c => Ok(u32::from(w.prescale) << 16),
                    0x50..=0x5c => Ok(w.holds[((off - 0x50) / 4) as usize]),
                    0x60 | 0x7c => Err(WO), 0x64 => Ok(w.key),
                    0x70 => Ok(w.ena), 0x74 => Ok(w.raw), 0x78 => Ok(w.raw & w.ena),
                    _ => Err("timer group general timer not modelled"),
                }
            }
        })
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64) -> Access<()> {
        let (block, off) = Self::decode(addr)?;
        // The writes firmware makes in its loops change one block alone and never request a
        // reset, so each undoes only that block on Err, not the whole of `transact`'s clone;
        // the result is `transact`'s. Other writes (configuration) take the transaction.
        let absolute = self.epoch + tick;
        let feed = match (block, off) {
            // RWDT FEED and WPROTECT: `Rtc::write` sends them straight to the RWDT as its
            // registers 6 and 7, which change the RWDT alone.
            (1, 0xac | 0xb0) => Some((&mut self.rtc.wdt, if off == 0xac { 6 } else { 7 })),
            // MWDT FEED and WPROTECT: the same registers 6 and 7 of the timer group's watchdog.
            (2 | 3, 0x60 | 0x64) => Some((&mut self.mwdt[block - 2], ((off - 0x48) / 4) as usize)),
            _ => None,
        };
        if let Some((w, reg)) = feed {
            // what a refused feed may have changed (`FeedUndo`)
            let undo = w.feed_undo();
            #[cfg(debug_assertions)]
            let before = w.clone();
            return Some(match w.write(reg, value, absolute, &self.profile, self.apb) {
                Ok(()) => {
                    self.collect(tick);
                    Ok(())
                }
                Err(why) => {
                    w.undo(undo);
                    #[cfg(debug_assertions)]
                    debug_assert!(*w == before, "a refused watchdog feed left a change FeedUndo misses");
                    Err(why)
                }
            });
        }
        if block == 0 {
            // SYSTIMER: its write changes the SYSTIMER alone (plain data, no notes of its own),
            // and it requests no reset.
            let before = self.systimer.clone();
            return Some(match self.systimer.write(off, value, absolute) {
                Ok(()) => {
                    self.collect(tick);
                    Ok(())
                }
                Err(why) => {
                    self.systimer = before;
                    Err(why)
                }
            });
        }
        Some(self.transact(|next| {
            next.write_inner(block, off, value, tick)?;
            next.collect(tick);
            Ok(())
        }))
    }

    fn transact<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, &'static str>) -> Result<T, &'static str> {
        // Every Err preserves deadlines, phases, queued writes and notes.
        // Existing notes are append-only here: do not clone the entire history
        // on every register access/event. A machine snapshot still clones it.
        let notes = std::mem::take(&mut self.notes);
        let mut next = self.clone();
        self.notes = notes;
        let result = f(&mut next)?;
        self.notes.append(&mut next.notes);
        std::mem::swap(&mut self.notes, &mut next.notes);
        *self = next;
        Ok(result)
    }

    fn write_inner(&mut self, block: usize, off: u32, value: u32, tick: u64) -> Result<(), &'static str> {
        let absolute = self.epoch + tick;
        match block {
            0 => self.systimer.write(off, value, absolute)?,
            1 => {
                if let Some((cause, scope)) = self.rtc.write(off, value, absolute, &self.profile, self.apb)? {
                    self.check_reset(scope)?;
                    self.reset_request = Some(ResetRequest { tick, cause, scope });
                }
            }
            _ => {
                let w = &mut self.mwdt[block - 2];
                match off {
                    0x48..=0x64 => w.write(((off - 0x48) / 4) as usize, value, absolute, &self.profile, self.apb)?,
                    0x70 | 0x7c => {
                        fields(value, 4)?;
                        if w.key != WKEY {
                            return Err("shared watchdog interrupt-register write protection is undocumented");
                        }
                        if off == 0x70 { w.ena = value; } else {
                            w.raw &= !value;
                        }
                    }
                    0x74 => return Err("direct MWDT RAW write is undocumented"),
                    0x78 => return Err(RO),
                    _ => return Err("timer group general timer not modelled"),
                }
            }
        }
        Ok(())
    }

    /// A write the spin skip may absorb (`Periph::idle_write`): RWDT WPROTECT and FEED.
    pub(super) fn idle_target(addr: u32) -> bool { matches!(addr, 0x6000_80ac | 0x6000_80b0) }

    /// For a write of `value` to `addr` in a polling cycle, on the state after the cycle: the
    /// first tick (epoch-relative) at or after which the write may stop being idle, when below
    /// it its effect is independent of the tick except through the RWDT's time fields (anchor,
    /// due), with no note, no error and no reset; None when it is not idle now.
    /// - WPROTECT (KEY) only stores the value: always idle.
    /// - FEED with bit 31, no interrupt pending (`FED_PENDING`) and no late write queued: idle
    ///   until the tick the sync window opens (`in_sync_window`: `hold - count(tick)` reaches
    ///   `wdt_sync_cycles`), which is at or before `due`. The key is not looked at: the bound
    ///   holds whether the feed is applied or ignored.
    pub(super) fn idle_write(&self, addr: u32, value: u32) -> Option<u64> {
        let w = &self.rtc.wdt;
        match addr {
            0x6000_80b0 => Some(u64::MAX),
            0x6000_80ac => {
                if value & (1 << 31) == 0 || w.raw != 0 || !w.late.is_empty() { return None; }
                if !w.running() { return Some(u64::MAX); }
                // A hold within the sync cycles is always in the window. Otherwise count(tick) =
                // count + edge(tick) - anchor reaches hold - sync at edge e.
                let room = w.hold(&self.profile).checked_sub(self.profile.wdt_sync_cycles)?;
                let e = (w.anchor + room).checked_sub(w.count)?;
                Some(w.clock.edge(e).saturating_sub(self.epoch))
            }
            _ => None,
        }
    }

    /// Takes `other`'s RWDT time fields (anchor, due): what an idle feed changes by its tick.
    pub(super) fn take_rwdt_time(&mut self, other: &Time) {
        (self.rtc.wdt.anchor, self.rtc.wdt.due) = (other.rtc.wdt.anchor, other.rtc.wdt.due);
    }

    /// Tags the blocks' new notes with the access or event tick.
    fn collect(&mut self, tick: u64) {
        let [m0, m1] = &mut self.mwdt;
        for notes in [&mut self.rtc.notes, &mut self.rtc.wdt.notes, &mut m0.notes, &mut m1.notes] {
            // Most accesses note nothing: skip the drain's set-up for an empty list.
            if !notes.is_empty() { self.notes.extend(notes.drain(..).map(|what| (tick, what))); }
        }
    }

    /// Drains diagnostic notes; cloned machine state includes undrained notes.
    pub fn take_notes(&mut self, out: &mut Vec<(u64, &'static str)>) {
        out.append(&mut self.notes);
    }

    /// Records a clock-tree model choice at `tick` beside the timers' notes.
    pub fn note(&mut self, tick: u64, what: &'static str) { self.notes.push((tick, what)); }

    fn check_reset(&self, scope: ResetScope) -> Result<(), &'static str> {
        // CPU resets retain peripherals (TRM pp.526–527 §7.1.3), but the
        // machine re-handoffs APB. Validate before emitting a reset request:
        // the reset hook has no fallible return and must never guess a switch.
        if scope == ResetScope::Cpu && self.apb != self.handoff_apb && self.mwdt.iter().any(Watchdog::running) {
            return Err("MWDT APB switch across CPU reset is undocumented");
        }
        Ok(())
    }

    pub fn next_event(&self) -> Option<u64> {
        // Asked after every register access: the earliest absolute deadline first, then its
        // epoch-relative tick (a saturating difference keeps the order, so the minimum of the
        // differences is the difference of the minimum).
        let [c0, c1, c2] = &self.systimer.comps;
        let [m0, m1] = &self.mwdt;
        let earliest = [c0.due, c1.due, c2.due, self.rtc.wdt.due, self.rtc.next_swd(), m0.due, m1.due]
            .into_iter().fold(None, |min: Option<u64>, due| match (min, due) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            });
        let reset = self.reset_request.map(|r| r.tick);
        match (earliest.map(|t| t.saturating_sub(self.epoch)), reset) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Performs exactly one event. Equal-tick order within Time is software
    /// reset, comparator index, RWDT, SWD, MWDT index. This only orders model
    /// work. Coinciding reset causes are a race: the widest scope wins (ties
    /// in that order), with a note.
    pub fn event(&mut self, tick: u64) -> Result<Option<ResetRequest>, &'static str> {
        self.transact(|next| {
            let reset = next.event_inner(tick)?;
            next.collect(tick);
            Ok(reset)
        })
    }

    fn event_inner(&mut self, tick: u64) -> Result<Option<ResetRequest>, &'static str> {
        let absolute = self.epoch + tick;
        fn scope(w: &Watchdog, absolute: u64) -> Option<ResetScope> {
            if w.due != Some(absolute) { return None; }
            match w.action() { 2 => Some(ResetScope::Cpu), 3 => Some(ResetScope::Core), 4 => Some(ResetScope::System), _ => None }
        }
        // Candidates in tie order: software, RWDT, SWD, MWDT0, MWDT1.
        let causes = [
            self.reset_request.filter(|r| r.tick == tick).map(|r| r.scope),
            scope(&self.rtc.wdt, absolute),
            (self.rtc.next_swd() == Some(absolute) && self.rtc.swd_request.is_none()).then_some(ResetScope::System),
            scope(&self.mwdt[0], absolute),
            scope(&self.mwdt[1], absolute),
        ];
        if causes.iter().flatten().count() > 1 {
            self.notes.push((tick, COINCIDE));
            let rank = |s: ResetScope| match s { ResetScope::Cpu => 0, ResetScope::Core => 1, ResetScope::System => 2 };
            let mut best = 0;
            for (n, s) in causes.iter().enumerate() {
                if let Some(s) = *s {
                    if causes[best].is_none_or(|b| rank(s) > rank(b)) { best = n; }
                }
            }
            let reset = match best {
                0 => return Ok(self.reset_request.take()),
                1 => self.rtc.wdt.expire(absolute, &self.profile, self.apb)?,
                2 => Some((0x12, ResetScope::System)),
                n => self.mwdt[n - 3].expire(absolute, &self.profile, self.apb)?,
            };
            let (cause, scope) = reset.expect("a reset action expires into a reset");
            self.check_reset(scope)?;
            return Ok(Some(ResetRequest { tick, cause, scope }));
        }
        if let Some(reset) = self.reset_request.take() { return Ok(Some(reset)); }
        for n in 0..3 {
            if self.systimer.comps[n].due == Some(absolute) {
                self.systimer.fire(n, absolute)?;
                return Ok(None);
            }
        }
        let mut reset = None;
        if self.rtc.wdt.due == Some(absolute) {
            reset = self.rtc.wdt.expire(absolute, &self.profile, self.apb)?;
        } else if self.rtc.next_swd() == Some(absolute) {
            if self.rtc.swd_request.is_some() {
                self.rtc.swd_request = None;
                self.rtc.swd_raw |= 1 << 15;
                self.rtc.swd_conf |= 2; // rtc_cntl_reg.h:2154–2159 FEED_INT.
            } else { reset = Some((0x12, ResetScope::System)); }
        } else {
            for w in &mut self.mwdt {
                if w.due == Some(absolute) { reset = w.expire(absolute, &self.profile, self.apb)?; break; }
            }
        }
        if let Some((cause, scope)) = reset {
            self.check_reset(scope)?;
            return Ok(Some(ResetRequest { tick, cause, scope }));
        }
        Ok(None)
    }

    pub fn reset(&mut self, reset: ResetRequest, profile: &TimeProfile, apb_ticks: u64, junk: &mut Junk) {
        self.epoch += reset.tick;
        self.reset_request = None;
        // TRM pp.526–527 §7.1.3: CPU only (+ PMS), Core excludes RTC,
        // System includes RTC. No CPU-reset SYSTIMER-retention guess is needed.
        if reset.scope != ResetScope::Cpu {
            self.systimer = Systimer::new(self.epoch, profile, junk);
            self.mwdt = std::array::from_fn(|n| Watchdog::new(n + 1, self.epoch, profile, apb_ticks, junk));
        } else if apb_ticks != self.apb {
            // A running divider was ruled out by check_reset. Stopped dividers
            // adopt the new source just as in apb_changed.
            for w in &mut self.mwdt {
                w.idle_clock(apb_ticks, self.epoch, profile);
            }
        }
        if reset.scope == ResetScope::System {
            self.rtc = Rtc::new(self.epoch, profile, apb_ticks, junk);
        }
        self.apb = apb_ticks;
        self.profile = profile.clone();
        // TRM pp.527–528,593: reset cause and flags; CPU-local causes leave
        // CPU1's cause unchanged. SWD flag is cleared explicitly (p.678).
        self.rtc.reset_state = (self.rtc.reset_state & !63) | u32::from(reset.cause) | (1 << 14);
        if reset.scope != ResetScope::Cpu {
            self.rtc.reset_state = (self.rtc.reset_state & !(63 << 6)) | (u32::from(reset.cause) << 6) | (1 << 15);
        }
        if reset.cause == 0x12 { self.rtc.swd_conf |= 1; }
    }

    pub fn apb_changed(&mut self, apb_ticks: u64, tick: u64) -> Result<(), &'static str> {
        if apb_ticks == self.apb { return Ok(()); }
        // facts-machine.md §5 NOT FOUND: divider behavior across clock switch.
        if self.mwdt.iter().any(Watchdog::running) {
            return Err("APB clock change while MWDT counts is undocumented");
        }
        self.apb = apb_ticks;
        for w in &mut self.mwdt {
            w.idle_clock(apb_ticks, self.epoch + tick, &self.profile);
        }
        Ok(())
    }
}

// Notes and pre-event transactional state have no S3Machine inspection API.
// Firmware-level behavior is tested in tests/time.rs; these tests exercise
// only the private seam that the future note-forwarding plumbing will use.
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Time {
        let p = TimeProfile {
            systimer_first_long: true, rc_slow_hz: (2_000_000, 3), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false,
            mwdt_rephase_on_config: true, wdt_restart_on_enable: true,
        };
        let mut t = Time::new(&p, 24, &mut Junk(51));
        while t.next_event() == Some(0) {
            assert_eq!(t.event(0), Ok(None), "initial SWD request has no reset action");
        }
        write(&mut t, RTC + 0xb0, WKEY, 0);
        write(&mut t, RTC + 0x98, 0, 0);
        write(&mut t, RTC + 0xb8, SWKEY, 0);
        write(&mut t, RTC + 0xb4, (300 << 18) | (1 << 31), 0);
        for base in TIMG {
            write(&mut t, base + 0x64, WKEY, 0);
            write(&mut t, base + 0x48, 0, 0);
            write(&mut t, base + 0x4c, 10 << 16, 0);
        }
        t
    }

    fn write(t: &mut Time, addr: u32, value: u32, tick: u64) {
        assert_eq!(t.write(addr, value, tick), Some(Ok(())), "fixture write {addr:#x} at {tick} must succeed");
    }

    fn registers(kind: usize) -> (u32, u32, u32) {
        if kind == 0 { (RTC + 0x98, RTC + 0x9c, RTC + 0xac) }
        else { (TIMG[kind - 1] + 0x48, TIMG[kind - 1] + 0x50, TIMG[kind - 1] + 0x60) }
    }

    fn watchdog(t: &Time, kind: usize) -> &Watchdog {
        if kind == 0 { &t.rtc.wdt } else { &t.mwdt[kind - 1] }
    }

    #[test]
    fn sync_window_notes_are_exact_drainable_and_part_of_saved_state() {
        for kind in 0..3 {
            let mut t = fixture();
            let (config, hold, feed) = registers(kind);
            write(&mut t, hold, if kind == 0 { 10 } else { 20 }, 0);
            write(&mut t, feed, 1 << 31, 0);
            write(&mut t, config, 1 << 31, 0); // off stages still advance; TRM p.676
            let period = if kind == 0 { 720 } else { 240 };
            let due = 20 * period;
            assert_eq!(t.next_event(), Some(due), "each watchdog uses its own prescaled/rational clock");
            let original = t.clone();

            write(&mut t, feed, 1 << 31, due - 2 * period - 1);
            assert!(t.notes.is_empty(), "one tick before the window feeds without a note");
            assert_eq!(t.next_event(), Some(37 * period), "early feed resets stage/count on the retained clock phase");

            t = original;
            let tick = due - 2 * period;
            write(&mut t, feed, 1 << 31, tick);
            assert_eq!(t.next_event(), Some(due), "inclusive window boundary cannot postpone the expiring stage");
            assert_eq!(watchdog(&t, kind).count(tick), 18, "late feed has no early counter effect");
            let saved = t.clone();
            let mut notes = vec![(1, "existing")];
            t.take_notes(&mut notes);
            assert_eq!(notes, [(1, "existing"), (tick, SYNC)], "take_notes appends the exact text and access tick");
            let mut empty = Vec::new();
            t.take_notes(&mut empty);
            assert!(empty.is_empty(), "take_notes drains each note once");
            t = saved;
            t.take_notes(&mut empty);
            assert_eq!(empty, [(tick, SYNC)], "restoring saved state restores undrained notes");
            assert_eq!(t.event(due), Ok(None), "non-reset expiry and deferred feed do not stop");
            assert_eq!(watchdog(&t, kind).stage, 0, "deferred feed runs after expiry advances the stage");
            assert_eq!(t.next_event(), Some(due + 20 * period), "late feed restarts at the expiry tick");
        }
    }

    #[test]
    fn late_configuration_is_ordered_and_relocking_does_not_cancel_it() {
        let mut t = fixture();
        write(&mut t, TIMG[0] + 0x50, 20, 0);
        write(&mut t, TIMG[0] + 0x48, 1 << 31, 0);
        let due = 20 * 240;
        write(&mut t, TIMG[0] + 0x48, 0, due - 480);
        write(&mut t, TIMG[0] + 0x64, 0, due - 240);
        assert!(t.mwdt[0].running(), "late disable leaves the current stage running until expiry");
        assert_eq!(t.event(due), Ok(None), "off-stage expiry applies the already accepted disable");
        assert!(!t.mwdt[0].running(), "relocking cannot cancel an accepted late configuration write");
        assert_eq!(t.mwdt[0].stage, 1, "expiry advances the stage before disabling");
        assert_eq!(t.notes, [(due - 480, SYNC)], "only the late configuration produces a note");

        let mut t = fixture();
        write(&mut t, TIMG[0] + 0x50, 20, 0);
        write(&mut t, TIMG[0] + 0x48, (1 << 31) | (1 << 29), 0);
        write(&mut t, TIMG[0] + 0x60, 0, due - 480);
        assert_eq!(t.event(due), Ok(None), "late feed after interrupt action must continue running");
        assert_eq!(t.mwdt[0].stage, 0, "interrupt action precedes feeding");
        assert_eq!(t.read(TIMG[0] + 0x74, due), Some(Ok(4)), "a feed leaves the pending interrupt raised");
        assert_eq!(t.notes, [(due - 480, SYNC), (due, FED_PENDING)], "the late feed onto a pending interrupt is noted when it lands");
        write(&mut t, TIMG[0] + 0x7c, 4, due);
        assert_eq!(t.read(TIMG[0] + 0x74, due), Some(Ok(0)), "documented CLR clears the RAW bit");
    }

    #[test]
    fn late_feed_cannot_cancel_reset_and_same_tick_access_follows_expiry() {
        let mut t = fixture();
        write(&mut t, RTC + 0x9c, 10, 0);
        write(&mut t, RTC + 0xac, 1 << 31, 0);
        write(&mut t, RTC + 0x98, (1 << 31) | (3 << 28), 0);
        let due = 20 * 720;
        write(&mut t, RTC + 0xac, 1 << 31, due - 720);
        let reset = t.event(due).expect("late feed must not stop").expect("Core reset still occurs");
        assert_eq!((reset.cause, reset.scope), (9, ResetScope::Core), "RWDT Core action survives late feed");
        let profile = t.profile.clone();
        t.reset(reset, &profile, 24, &mut Junk(52));
        assert_eq!(t.rtc.wdt.stage, 1, "Core reset preserves the expired RWDT's next stage");
        assert!(t.rtc.wdt.late.is_empty(), "reset discards feeds queued in the old epoch");
        assert_eq!(t.notes, [(due - 720, SYNC)], "reset keeps the undrained old-epoch diagnostic");

        let mut t = fixture();
        write(&mut t, RTC + 0x9c, 10, 0);
        write(&mut t, RTC + 0xac, 1 << 31, 0);
        write(&mut t, RTC + 0x98, 1 << 31, 0);
        assert_eq!(t.event(due), Ok(None), "machine convention expires an off stage before its same-tick access");
        write(&mut t, RTC + 0xac, 1 << 31, due);
        assert!(t.notes.is_empty(), "feed at expiry applies to the new stage and is not an early-window write");
        assert_eq!(t.rtc.wdt.stage, 0, "same-tick feed follows the already performed stage advancement");
    }

    #[test]
    fn races_are_modelled_adversely_with_notes() {
        // Shortening a hold behind the count expires at the write.
        let mut t = fixture();
        write(&mut t, TIMG[0] + 0x50, 20, 0);
        write(&mut t, TIMG[0] + 0x48, (1 << 31) | (1 << 29), 0);
        write(&mut t, TIMG[0] + 0x50, 1, 5 * 240);
        assert_eq!(t.next_event(), Some(5 * 240), "a hold behind the count expires at the write's tick");
        assert_eq!(t.notes, [(5 * 240, BEHIND)], "the shortened hold is noted at its write");
        assert_eq!(t.event(5 * 240), Ok(None), "the interrupt stage expires");
        assert_eq!(t.mwdt[0].raw, 4, "its interrupt is raised");

        // Two reset actions on one tick: the widest scope wins, though
        // MWDT0 comes first in tie order. RWDT: 10 held = 20 RC_SLOW
        // cycles of 720 ticks; MWDT0: 60 cycles of 240 ticks.
        let mut t = fixture();
        write(&mut t, TIMG[0] + 0x50, 60, 0);
        write(&mut t, TIMG[0] + 0x48, (1 << 31) | (3 << 29), 0);
        write(&mut t, RTC + 0x9c, 10, 0);
        write(&mut t, RTC + 0xac, 1 << 31, 0);
        write(&mut t, RTC + 0x98, (1 << 31) | (4 << 28), 0);
        let due = 20 * 720;
        assert_eq!(t.next_event(), Some(due), "both watchdogs are due at one tick");
        let reset = t.event(due).expect("coinciding resets do not stop").expect("one reset occurs");
        assert_eq!((reset.cause, reset.scope), (0x10, ResetScope::System), "RWDT's System action beats MWDT0's Core action");
        assert_eq!(t.notes, [(due, COINCIDE)], "the coincidence is noted at its tick");
    }

    #[test]
    fn rejected_writes_and_events_leave_deadlines_and_state_intact() {
        let mut t = fixture();
        write(&mut t, TIMG[0] + 0x50, 20, 0);
        write(&mut t, TIMG[0] + 0x48, 1 << 31, 0);
        let before = t.next_event();
        assert_eq!(t.write(TIMG[0] + 0x4c, 2 << 16, 5 * 240), Some(Err("MWDT divider change while counting is undocumented")),
            "a divider change while counting has no documented result");
        assert_eq!(t.next_event(), before, "rejected write retains the old expiry");
        assert_eq!(t.mwdt[0].prescale, 10, "rejected write retains the old divider");
        assert!(t.notes.is_empty(), "a rejected write leaves no note");
        assert_eq!(t.apb_changed(12, 5 * 240), Err("APB clock change while MWDT counts is undocumented"), "active divider refuses a source switch");
        assert_eq!(t.apb, 24, "failed clock switch leaves APB intact");

        write(&mut t, TIMG[0] + 0x48, (1 << 31) | (2 << 29), 5 * 240);
        let due = t.next_event().expect("CPU-reset stage has a deadline");
        assert_eq!(t.event(due), Err("watchdog CPU reset action with PROCPU_RESET_EN clear is undocumented"), "disabled reset output is not guessed");
        assert_eq!(t.next_event(), Some(due), "failed event does not consume its deadline");
        assert_eq!(t.mwdt[0].stage, 0, "failed event does not advance its stage");
    }

    #[test]
    fn cpu_and_core_resets_keep_rtc_count_and_cpu_keeps_systimer() {
        for scope in [ResetScope::Cpu, ResetScope::Core] {
            let mut t = fixture();
            write(&mut t, RTC + 0x9c, 100, 0);
            write(&mut t, RTC + 0xac, 1 << 31, 0);
            write(&mut t, RTC + 0x98, (1 << 31) | (4 << 28), 0);
            let deadline = t.rtc.wdt.due.expect("RWDT is running");
            let tick = 1234; // deliberately between RC_SLOW edges
            let count = t.rtc.wdt.count(tick);
            let systimer = t.systimer.count(0, tick);
            let profile = t.profile.clone();
            t.reset(ResetRequest { tick, cause: 3, scope }, &profile, 24, &mut Junk(53));
            assert_eq!(t.rtc.wdt.count(t.epoch), count, "retained RTC count does not restart at epoch zero");
            assert_eq!(t.rtc.wdt.due, Some(deadline), "retained RTC clock keeps its original phase and expiry");
            if scope == ResetScope::Cpu {
                assert_eq!(t.systimer.count(0, t.epoch), systimer, "TRM §7.1 CPU reset retains peripheral state");
            } else {
                assert_eq!(t.systimer.conf, 0x4600_0000, "Core reset restores SYSTIMER's configuration");
            }
        }
    }

    #[test]
    fn divider_phase_and_enable_assumptions_are_controlled_by_the_profile() {
        for rephase in [false, true] {
            let mut t = fixture();
            t.profile.mwdt_rephase_on_config = rephase;
            t.profile.mwdt_rephase_on_feed = rephase;
            t.profile.wdt_restart_on_enable = rephase;
            write(&mut t, TIMG[1] + 0x4c, 20 << 16, 100);
            assert_eq!(t.mwdt[1].clock.start, if rephase { 100 } else { 0 }, "idle prescale phase follows its M field");
            write(&mut t, TIMG[1] + 0x50, 100, 100);
            write(&mut t, TIMG[1] + 0x60, 0, 100);
            write(&mut t, TIMG[1] + 0x48, 1 << 31, 100);
            write(&mut t, TIMG[1] + 0x60, 0, 1_000);
            assert_eq!(t.mwdt[1].clock.start, if rephase { 1_000 } else { 0 }, "feed phase follows its M field");
            write(&mut t, TIMG[1] + 0x48, 0, 2_000);
            let stopped_count = t.mwdt[1].count;
            write(&mut t, TIMG[1] + 0x48, 1 << 31, 3_000);
            assert_eq!(t.mwdt[1].count, if rephase { 0 } else { stopped_count }, "re-enabling resets or retains the digital count as profiled");
        }
    }

    #[test]
    fn swd_request_precedes_expiry_by_the_profile_lead_and_is_masked() {
        let mut t = fixture();
        // TRM pp.677–678: feed creates a fresh analog interval; the request
        // is an independent event and ENA gates ST, not RAW.
        write(&mut t, RTC + 0xb4, (300 << 18) | (1 << 29), 100);
        let request = 100 + t.profile.swd_timeout_ticks - t.profile.swd_request_lead_ticks;
        assert_eq!(t.next_event(), Some(request), "SWD first schedules its feed request");
        assert_eq!(t.event(request), Ok(None), "SWD request is not an expiry reset");
        assert_eq!(t.read(RTC + 0x44, request), Some(Ok(1 << 15)), "SWD request latches RTC RAW bit 15");
        assert_eq!(t.read(RTC + 0x48, request), Some(Ok(0)), "disabled interrupt remains masked in ST");
        write(&mut t, RTC + 0x40, 1 << 15, request);
        assert_eq!(t.read(RTC + 0x48, request), Some(Ok(1 << 15)), "ENA exposes the already-latched SWD request");
        let expiry = request + t.profile.swd_request_lead_ticks;
        assert_eq!(t.next_event(), Some(expiry), "request handling retains the analog expiry");
        assert_eq!(t.event(expiry), Ok(Some(ResetRequest { tick: expiry, cause: 0x12, scope: ResetScope::System })),
            "unfed SWD expires with System cause 0x12");
    }

    /// The spin skip's absorption (machine/spin.rs): from a state on which `idle_write` bounds
    /// each write of the driver's poll (unlock, feed, relock) by q, the poll at t1 and again at
    /// t2, every write before q, leaves `Time` as the poll at t2 alone would, notes included:
    /// skipped polls leave no trace once a real one runs. And q is tight: with the second
    /// poll's feed at q, the states differ (the lone feed falls in the sync window).
    #[test]
    fn idle_rwdt_polls_absorb_earlier_ones_up_to_the_sync_window() {
        const GAP: u64 = 7;
        const EDGE: u64 = 720; // the fixture's slow clock: 480 MHz / (2 MHz / 3)
        // Events up to `at` first, as the machine runs them before an access; false on a reset.
        fn settle(t: &mut Time, at: u64) -> bool {
            while let Some(e) = t.next_event().filter(|&e| e <= at) {
                if t.event(e) != Ok(None) { return false; }
            }
            true
        }
        fn poll(t: &mut Time, at: u64) -> bool {
            for (n, (addr, value)) in [(RTC + 0xb0, WKEY), (RTC + 0xac, 1 << 31), (RTC + 0xb0, 0)].into_iter().enumerate() {
                let tick = at + n as u64 * GAP;
                if !settle(t, tick) { return false; }
                write(t, addr, value, tick);
            }
            true
        }
        let quiet = |t: &Time| [(RTC + 0xb0, WKEY), (RTC + 0xac, 1 << 31), (RTC + 0xb0, 0)].into_iter()
            .map(|(addr, value)| t.idle_write(addr, value))
            .try_fold(u64::MAX, |q, w| w.map(|w| q.min(w)));
        let mut junk = Junk(77);
        let (mut absorbed, mut tight, mut refused) = (0, 0, 0);
        for case in 0..300 {
            // A random RWDT: stage-0 hold (scaled by two), action interrupt or system reset,
            // running or not, fed at 0, then one poll at a random tick, maybe past expiry.
            let mut t = fixture();
            let hold = 3 + junk.word() % 30;
            let action = if junk.word() % 2 == 0 { 1 } else { 4 };
            let en = if junk.word() % 4 == 0 { 0 } else { 1 << 31 };
            write(&mut t, RTC + 0x9c, hold as u32, 0);
            write(&mut t, RTC + 0xac, 1 << 31, 0);
            write(&mut t, RTC + 0x98, en | (action << 28), 0);
            let t0 = junk.word() % (3 * hold * EDGE);
            if !poll(&mut t, t0) { continue; }
            let Some(q) = quiet(&t) else {
                // Not idle: an interrupt pending or a feed queued in the window.
                assert!(t.rtc.wdt.raw != 0 || !t.rtc.wdt.late.is_empty(), "case {case}: idle_write refuses only a pending or queued RWDT");
                refused += 1;
                continue;
            };
            let after = t0 + 2 * GAP + 1;
            let end = q.min(after + 4 * hold * EDGE);
            // Two polls inside [after, end), every write before q.
            if end > after + 4 * GAP + 2 {
                let t2 = after + 2 * GAP + 1 + junk.word() % (end - after - 4 * GAP - 2);
                let t1 = after + junk.word() % (t2 - 2 * GAP - after);
                let (mut a, mut b) = (t.clone(), t.clone());
                assert!(poll(&mut a, t1) && poll(&mut a, t2) && poll(&mut b, t2), "case {case}: no event before q");
                assert_eq!(a.same_state(&b), None, "case {case}: a poll at {t1} leaves a trace after the poll at {t2} (q {q})");
                absorbed += 1;
            }
            // Tightness: the second poll's feed at q, the first one a slow edge after the probe's.
            if q != u64::MAX && q >= after + EDGE + 3 * GAP {
                let t2 = q - GAP;
                let t1 = t0 + EDGE + 2 * GAP;
                if t1 + 2 * GAP < t2 {
                    let (mut a, mut b) = (t.clone(), t.clone());
                    assert!(poll(&mut a, t1) && poll(&mut a, t2) && poll(&mut b, t2), "case {case}: no event before q");
                    assert!(a.same_state(&b).is_some(), "case {case}: q {q} is not tight: a feed there is still absorbed");
                    assert!(!b.rtc.wdt.late.is_empty(), "case {case}: the lone feed at q falls in the sync window");
                    tight += 1;
                }
            }
        }
        assert!(absorbed > 100 && tight > 50 && refused > 0, "the cases reach every outcome: {absorbed} absorbed, {tight} tight, {refused} refused");
    }
}
