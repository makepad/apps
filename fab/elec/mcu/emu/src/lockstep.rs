//! Differential runs of a machine's fast tier against its reference: one `Machine` that is two.

use crate::board::{BusEvent, NetInfo};
use crate::counts::{CodeMap, ExecCounts};
use crate::machine::*;
use std::any::Any;
use std::sync::Arc;

/// A machine run twice: `fast` in its own tier and `oracle`, a copy forced to `Tier::Ref`. Every
/// call goes to both. After each `run` and `rewind` it compares the returns, the tick, the PC, the
/// status and the whole state (`Machine::same_state`); each untaken output stream (drive edges,
/// samples, link output, trace, notes, bus records, counts) is compared as it is taken. Queries
/// answer from `fast`, which machine-specific reads reach through `Deref`; machine-specific
/// inputs go to both through `both`. A difference panics with the call, both sides' values and
/// the first differing field: a fast tier's result must never depend on the tier.
#[derive(Clone)]
pub struct Lockstep<M: Machine> {
    fast: M,
    oracle: M,
    runs: u64,
    /// The fast side's tier, and whether it alternates with the reference from run to run.
    tier: Tier,
    alternate: bool,
}

impl<M: Machine> Lockstep<M> {
    /// `machine` runs in `Tier::default()`; its copy runs the reference.
    pub fn new(machine: M) -> Self {
        let mut oracle = machine.clone();
        oracle.set_tier(Tier::Ref);
        let lockstep = Self { fast: machine, oracle, runs: 0, tier: Tier::default(), alternate: false };
        lockstep.check("new", None);
        lockstep
    }

    /// As `new`, with the fast side switching between its tier and the reference at every `run`:
    /// a run may start in either tier wherever the other left the machine.
    pub fn alternating(machine: M) -> Self { Self { alternate: true, ..Self::new(machine) } }

    pub fn fast(&self) -> &M { &self.fast }
    pub fn oracle(&self) -> &M { &self.oracle }
    /// Runs compared so far.
    pub fn runs(&self) -> u64 { self.runs }

    /// A machine-specific call with effects (a board input, say) made on both sides; both must
    /// return the same, and the states must still agree.
    pub fn both<R: PartialEq + std::fmt::Debug>(&mut self, mut call: impl FnMut(&mut M) -> R) -> R {
        let a = call(&mut self.fast);
        let b = call(&mut self.oracle);
        assert_eq!(a, b, "lockstep: a machine-specific call returned differently");
        self.check("both", None);
        a
    }

    fn check(&self, call: &str, ran: Option<(Ran, Ran)>) {
        let (f, o) = (&self.fast, &self.oracle);
        let what = if let Some((a, b)) = ran.filter(|(a, b)| a != b) {
            Some(format!("returned {a:?} against the reference's {b:?}"))
        } else if f.now() != o.now() {
            Some(format!("tick {} against {}", f.now(), o.now()))
        } else if f.pc() != o.pc() {
            Some(format!("pc {:#x} against {:#x}", f.pc(), o.pc()))
        } else if f.status() != o.status() {
            Some(format!("status {:?} against {:?}", f.status(), o.status()))
        } else if f.drive_gen() != o.drive_gen() {
            Some(format!("drive generation {} against {}", f.drive_gen(), o.drive_gen()))
        } else {
            f.same_state(o).map(|field| format!("state differs in {field}"))
        };
        if let Some(what) = what {
            panic!("lockstep: after {call} #{} (fast at tick {}, pc {:#x}): {what}", self.runs, f.now(), f.pc());
        }
    }

    fn same<T: PartialEq + std::fmt::Debug>(&self, stream: &str, fast: &[T], oracle: &[T]) {
        if fast != oracle {
            let at = fast.iter().zip(oracle).position(|(a, b)| a != b).unwrap_or(fast.len().min(oracle.len()));
            panic!("lockstep: untaken {stream} differ after run #{} at entry {at} of {} against {}: {:?} against {:?}",
                self.runs, fast.len(), oracle.len(), fast.get(at), oracle.get(at));
        }
    }
}

/// Appends both sides' untaken output with `take`, compares what each appended, keeps the fast
/// side's in `out`.
macro_rules! take_both {
    ($self:ident, $out:ident, $stream:literal, $take:ident) => {{
        let start = $out.len();
        $self.fast.$take($out);
        let mut theirs = Vec::new();
        $self.oracle.$take(&mut theirs);
        $self.same($stream, &$out[start..], &theirs);
    }};
}

impl<M: Machine> Machine for Lockstep<M> {
    fn set_code_roots(&mut self, roots: &[u32]) {
        self.fast.set_code_roots(roots);
        self.oracle.set_code_roots(roots);
    }
    fn code_map(&self) -> Option<Arc<CodeMap>> { self.fast.code_map() }
    fn take_counts(&mut self, out: &mut ExecCounts) {
        let mut theirs = ExecCounts::default();
        let mut ours = ExecCounts::default();
        self.fast.take_counts(&mut ours);
        self.oracle.take_counts(&mut theirs);
        if ours != theirs {
            panic!("lockstep: execution counts differ after run #{}: {ours:?} against {theirs:?}", self.runs);
        }
        out.blocks.extend(ours.blocks);
        out.conds.extend(ours.conds);
    }
    fn tick_hz(&self) -> u64 { self.fast.tick_hz() }
    fn now(&self) -> u64 { self.fast.now() }
    fn pc(&self) -> u32 { self.fast.pc() }

    fn run(&mut self, until: &Until) -> Ran {
        if self.alternate {
            self.fast.set_tier(if self.runs % 2 == 0 { self.tier } else { Tier::Ref });
        }
        let a = self.fast.run(until);
        let b = self.oracle.run(until);
        self.runs += 1;
        self.check("run", Some((a, b)));
        a
    }

    fn mark(&mut self) {
        self.fast.mark();
        self.oracle.mark();
    }

    fn host_input(&mut self, input: &dyn Any) -> bool {
        let a = self.fast.host_input(input);
        let b = self.oracle.host_input(input);
        assert_eq!(a, b, "lockstep: host input taken by one side only");
        if a { self.check("host_input", None); }
        a
    }

    /// Only the fast side has feeds (a clone carries none), so only it publishes.
    fn publish(&mut self, t: f64) { self.fast.publish(t); }
    fn take_feeds(&mut self) -> Option<Box<dyn Any + Send>> { self.fast.take_feeds() }
    fn put_feeds(&mut self, feeds: Box<dyn Any + Send>) { self.fast.put_feeds(feeds); }

    fn rewind(&mut self) {
        self.fast.rewind();
        self.oracle.rewind();
        self.check("rewind", None);
    }

    fn pins(&self) -> &[&'static str] { self.fast.pins() }
    fn drive(&self, pin: usize) -> Drive { self.fast.drive(pin) }
    fn drive_gen(&self) -> u64 { self.fast.drive_gen() }
    fn take_drives(&mut self, out: &mut Vec<DriveEdge>) { take_both!(self, out, "drive edges", take_drives) }
    fn take_samples(&mut self, out: &mut Vec<SampleAt>) { take_both!(self, out, "sample requests", take_samples) }
    fn set_sample(&mut self, at: SampleAt, volts: f64, supply: f64) {
        self.fast.set_sample(at, volts, supply);
        self.oracle.set_sample(at, volts, supply);
    }
    fn analog_only(&self, pin: usize) -> bool { self.fast.analog_only(pin) }
    fn set_input(&mut self, pin: usize, high: bool) {
        self.fast.set_input(pin, high);
        self.oracle.set_input(pin, high);
    }
    fn link_in(&mut self, link: u8, byte: u8) {
        self.fast.link_in(link, byte);
        self.oracle.link_in(link, byte);
    }
    fn take_out(&mut self, out: &mut Vec<LinkOut>) { take_both!(self, out, "link output", take_out) }
    fn take_trace(&mut self, out: &mut Vec<(u64, u32)>) { take_both!(self, out, "trace", take_trace) }
    fn take_notes(&mut self, out: &mut Vec<Note>) { take_both!(self, out, "notes", take_notes) }
    fn nets(&self) -> &[NetInfo] { self.fast.nets() }
    fn take_bus(&mut self, out: &mut Vec<BusEvent>) { take_both!(self, out, "bus records", take_bus) }
    fn status(&self) -> Status { self.fast.status() }
    fn halt_detail(&self) -> Option<String> { self.fast.halt_detail() }
    fn regs(&self, out: &mut Vec<(&'static str, u32)>) { self.fast.regs(out) }
    fn read_data(&self, addr: u32, out: &mut [u8]) -> bool { self.fast.read_data(addr, out) }
    fn chip(&self, out: &mut ChipState) { self.fast.chip(out) }
    fn eeprom_bytes(&self) -> Option<Arc<[u8]>> { self.fast.eeprom_bytes() }
    fn load_eeprom(&mut self, bytes: &[u8]) -> bool {
        let a = self.fast.load_eeprom(bytes);
        let b = self.oracle.load_eeprom(bytes);
        assert_eq!(a, b, "lockstep: EEPROM load taken by one side only");
        a
    }
    /// The fast side's tier; the oracle stays the reference.
    fn set_tier(&mut self, tier: Tier) {
        self.tier = tier;
        self.fast.set_tier(tier);
    }
    fn same_state(&self, other: &Self) -> Option<&'static str> { self.fast.same_state(&other.fast) }
}

/// Reads of machine-specific state answer from the fast side (the oracle has the same state).
/// There is no `DerefMut`: a machine-specific call with effects goes to both through `both`.
impl<M: Machine> std::ops::Deref for Lockstep<M> {
    type Target = M;
    fn deref(&self) -> &M { &self.fast }
}
