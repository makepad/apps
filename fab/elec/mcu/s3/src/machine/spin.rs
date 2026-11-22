//! The spin skip (`Tier::Decoded` and `Tier::Native`; the reference never skips): a polling
//! cycle that leaves the whole machine as it found it, but for its time, is run once for real
//! and then counted in for as many whole cycles as fit before anything could tell the
//! difference. Firmware that waits on a register (an SPI transfer's done bit, with a watchdog
//! feed in every poll) spends thousands of cycles so; each costs a register write's handling.
//!
//! The statement it rests on: a cycle from an anchor back to the same anchor is skippable when
//! (a) the whole machine state after it equals the state before it, except declared time
//! fields, and (b) nothing the cycle does depends on those fields before a computed tick.
//!
//! - Trigger (a hint): the lane goes on after a stable register read (`Periph::stable_read`) at
//!   the PC of the last one. A failed probe there doubles the visits to wait before the next.
//!   The ordinary lane notes nothing more for it (`Plain`): its bus's `slow` holds the address.
//! - Probe (the proof): a snapshot at the anchor, then the cycle runs through the decoded lane
//!   one instruction at a time (`lane_probe`, no native unit, every access noted: `Probing`),
//!   until the lane goes on after the read at the anchor again. Any SRAM store, a read that is not stable, a write that cannot be idle
//!   (`Periph::idle_target`), a marked PC, the lane ending or more than `PROBE_INSNS`
//!   instructions abandon it, with no effect on state: its instructions ran as the lane's own.
//! - Fixed point: the CPU equal but for CCOUNT and INSTRET, the peripherals equal but for the
//!   write generation and the RWDT's anchor and due (`Periph::same_but_time`), the bus
//!   registers, drive generation, period, code generation and the trace, link output and drive
//!   log as they were; n > 0 instructions taking n periods.
//! - Quiet: every write of the cycle idle on the state after it (`Periph::idle_write`), the
//!   earliest tick any stops being so.
//! - Skip: of the k whole cycles that start every instruction before both the lane's budget
//!   (the next event, input, board deadline, run end, comparator match) and quiet, k - 1 are
//!   counted in: TICK, INSTRET, CCOUNT, the budget and the write generation. The k-th and what
//!   follows run for real, so the last feed lands at its own tick: after a feed the RWDT's
//!   state is a function of that tick alone.

use fab_elec_mcu_emu::board::Board;
use fab_elec_mcu_xtensa::sim::Cpu;

use super::{LaneRun, S3Machine};
use crate::bus::Registers;
use crate::periph::Periph;

/// Decoded instructions a probe runs before it gives up: a cost cap, hitting it only forgoes a skip.
const PROBE_INSNS: u64 = 256;

/// The skip's own bookkeeping: hints and a counter, never machine state.
#[derive(Clone, Default)]
pub(super) struct Spin {
    /// The PC of the last stable register read the lane went on after.
    last: u32,
    /// The anchor of the last failed probe, its failures in a row, and the visits left to wait.
    failed: u32,
    misses: u32,
    wait: u64,
    probe: Option<Box<Probe>>,
    /// Cycles counted in without running them.
    pub skipped: u64,
}

#[derive(Clone)]
struct Probe {
    anchor: u32,
    cpu: Cpu,
    periph: Periph,
    regs: Registers,
    tick: u64,
    period: u64,
    writes: u64,
    drive: u64,
    code_gen: u64,
    lens: (usize, usize, usize),
    /// The cycle's register writes: (address, value).
    stores: Vec<(u32, u32)>,
}

/// What a lane instruction's register access was, for the probe.
#[derive(Clone, Copy, Default)]
pub(super) struct Access {
    /// The address of a register read, or 0.
    pub read: u32,
    /// A register write: (address, value).
    pub write: Option<(u32, u32)>,
}

/// What the lane's bus notes of an instruction's accesses, by the lane it runs in.
pub(super) trait Notes: Default {
    /// Whether a probe runs: only then does the lane check it at each instruction.
    const PROBE: bool;
    /// A register read, a register write, an SRAM store.
    fn read(&mut self, addr: u32);
    fn write(&mut self, addr: u32, value: u32);
    fn store(&mut self);
    fn access(&self) -> Access;
    fn stored(&self) -> bool;
}

/// The ordinary lane's notes: none (the trigger reads `LaneBus::slow`).
#[derive(Default)]
pub(super) struct Plain;

impl Notes for Plain {
    const PROBE: bool = false;
    #[inline(always)]
    fn read(&mut self, _: u32) {}
    #[inline(always)]
    fn write(&mut self, _: u32, _: u32) {}
    #[inline(always)]
    fn store(&mut self) {}
    #[inline(always)]
    fn access(&self) -> Access { Access::default() }
    #[inline(always)]
    fn stored(&self) -> bool { false }
}

/// A probe's notes: every register access and whether the instruction stored to SRAM.
#[derive(Default)]
pub(super) struct Probing(Access, bool);

impl Notes for Probing {
    const PROBE: bool = true;
    #[inline(always)]
    fn read(&mut self, addr: u32) { self.0.read = addr; }
    #[inline(always)]
    fn write(&mut self, addr: u32, value: u32) { self.0.write = Some((addr, value)); }
    #[inline(always)]
    fn store(&mut self) { self.1 = true; }
    #[inline(always)]
    fn access(&self) -> Access { self.0 }
    #[inline(always)]
    fn stored(&self) -> bool { self.1 }
}

impl Spin {
    #[inline]
    pub(super) fn probing(&self) -> bool { self.probe.is_some() }

    /// Gives up the probe under way, if any, as a failed one at its anchor.
    pub(super) fn abandon(&mut self) {
        if let Some(probe) = self.probe.take() { self.miss(probe.anchor); }
    }

    fn miss(&mut self, anchor: u32) {
        self.misses = if self.failed == anchor { (self.misses + 1).min(16) } else { 1 };
        self.failed = anchor;
        self.wait = 1 << self.misses;
    }
}

impl<B: Board> S3Machine<B> {
    /// At the ordinary lane's continue point after a register access at `pc` that read `read`
    /// (0 for none), the budget fresh for the next instruction: whether a probe starts here,
    /// at a repeated stable read.
    #[inline]
    pub(super) fn spin_trigger(&mut self, run: &LaneRun, pc: u32, read: u32) -> bool {
        if read == 0 || !Periph::stable_read(read) { return false; }
        if self.spin.last != pc {
            self.spin.last = pc;
            return false;
        }
        if self.spin.failed == pc && self.spin.wait > 0 {
            self.spin.wait -= 1;
            return false;
        }
        self.spin_start(run, pc);
        true
    }

    #[cold]
    fn spin_start(&mut self, run: &LaneRun, pc: u32) {
        let bus = &self.bus;
        self.spin.probe = Some(Box::new(Probe {
            anchor: pc, cpu: self.cpu.clone(), periph: bus.periph.clone(), regs: bus.regs.clone(),
            tick: self.tick, period: run.period, writes: bus.periph.write_gen(), drive: bus.periph.drive_gen(),
            code_gen: bus.code_gen, lens: (self.trace.len(), self.out.len(), self.drives.len()), stores: Vec::new(),
        }));
    }

    /// Whether an instruction without a register access ends the probe under way: an SRAM
    /// store, or the cost cap.
    #[inline]
    pub(super) fn spin_plain(&mut self, stored: bool) {
        let Some(probe) = &self.spin.probe else { return };
        if stored || self.cpu.instret - probe.cpu.instret > PROBE_INSNS { self.spin.abandon(); }
    }

    /// At a probe's continue point after a register access at `pc`: feeds it, or ends it at
    /// the anchor.
    pub(super) fn spin_probe(&mut self, run: &mut LaneRun, pc: u32, access: Access) {
        let fits = (access.read == 0 || Periph::stable_read(access.read))
            && access.write.is_none_or(|(addr, _)| Periph::idle_target(addr));
        // A marked PC on the way abandoned it.
        let Some(probe) = self.spin.probe.as_mut() else { return };
        if !fits || self.cpu.instret - probe.cpu.instret > PROBE_INSNS { return self.spin.abandon(); }
        if let Some(store) = access.write { probe.stores.push(store); }
        if access.read == 0 || pc != probe.anchor { return; }
        let probe = self.spin.probe.take().expect("a probe under way");
        match self.spin_cycle(&probe) {
            Some((n, w, quiet)) => {
                self.spin.misses = 0;
                self.spin.wait = 0;
                self.spin_skip(run, n, w, quiet);
            }
            None => self.spin.miss(probe.anchor),
        }
    }

    /// The fixed point: the cycle's instructions n, its write-generation steps w and its quiet
    /// tick, when the cycle left everything but the declared time fields as it found it.
    fn spin_cycle(&self, probe: &Probe) -> Option<(u64, u64, u64)> {
        let n = self.cpu.instret - probe.cpu.instret;
        if n == 0 || self.bus.regs.ticks_per_cycle() != probe.period || self.halt.is_some() { return None; }
        debug_assert_eq!(self.tick - probe.tick, n * probe.period, "a lane instruction takes one period");
        if self.bus.code_gen != probe.code_gen || self.bus.periph.drive_gen() != probe.drive
            || (self.trace.len(), self.out.len(), self.drives.len()) != probe.lens || self.bus.regs != probe.regs
        {
            return None;
        }
        let mut cpu = self.cpu.clone();
        (cpu.ccount, cpu.instret) = (probe.cpu.ccount, probe.cpu.instret);
        if cpu.same_state(&probe.cpu).is_some() || !self.bus.periph.same_but_time(&probe.periph) { return None; }
        let quiet = probe.stores.iter()
            .map(|&(addr, value)| self.bus.periph.idle_write(addr, value))
            .try_fold(u64::MAX, |quiet, q| q.map(|q| quiet.min(q)))?;
        Some((n, self.bus.periph.write_gen().wrapping_sub(probe.writes), quiet))
    }

    /// Counts in all but the last of the whole cycles of `n` instructions that start every
    /// instruction before the budget and before `quiet`.
    fn spin_skip(&mut self, run: &mut LaneRun, n: u64, w: u64, quiet: u64) {
        let room = if quiet > self.tick { (quiet - self.tick).div_ceil(run.period) } else { 0 };
        let k = run.budget.min(room) / n;
        if k < 2 { return; }
        // s < budget <= the comparator budget (and 2^32): CCOUNT passes no comparator match.
        let s = (k - 1) * n;
        self.tick += s * run.period;
        self.cpu.instret += s;
        self.cpu.advance(s as u32);
        run.budget -= s;
        run.start = self.cpu.instret;
        self.bus.periph.add_writes((k - 1).wrapping_mul(w));
        self.spin.skipped += k - 1;
    }
}
