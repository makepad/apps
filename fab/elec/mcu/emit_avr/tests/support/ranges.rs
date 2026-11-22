use fab_elec_mcu_emit_avr::range::{self, Classes, Interval, Ranges};
use fab_elec_mcu_rt::{exec::{self, Observe, PortCall, Ports}, format::*, module::Module, ports};

pub struct Check<'a, 'm> {
    module: &'a Module<'m>,
    ranges: &'a Ranges,
    program: &'a str,
    pub values: usize,
}

impl<'a, 'm> Check<'a, 'm> {
    pub fn new(module: &'a Module<'m>, ranges: &'a Ranges, program: &'a str) -> Self {
        Self { module, ranges, program, values: 0 }
    }
    fn holds(&self, interval: Interval, bits: u64, ty: u8, what: &str, entry: usize, id: u32) {
        let value = if ty == TY_BOOL { i32::from(bits != 0) } else { bits as u32 as i32 };
        assert!(interval.contains(value),
            "FALSIFIER 1: {what} {id}, entry {entry}, value {value}, interval {interval:?}\nprogram:\n{}\nmodule: {:?}",
            self.program, self.module);
    }
}

impl Observe for Check<'_, '_> {
    fn value(&mut self, entry: usize, value: u32, bits: u64) {
        self.holds(self.ranges.entries[entry].values[value as usize], bits,
            self.module.ty(entry, value, false), "value", entry, value);
        self.values += 1;
    }
    fn set(&mut self, entry: usize, var: u32, bits: u64) {
        self.holds(self.ranges.entries[entry].vars[var as usize], bits,
            self.module.ty(entry, var, true), "variable", entry, var);
    }
    fn store(&mut self, region: u32, word: u32, bits: u32) {
        if region == REGION_STATE {
            self.holds(self.ranges.state[word as usize], u64::from(bits), TY_I32, "state", 0, word);
        }
    }
}

struct Inputs { phase: usize }
impl Ports for Inputs {
    fn call(&mut self, port: u16, _: u32, _: u64, _: &[u32], rets: &mut [u32]) -> PortCall {
        for (ret, bits) in rets.iter_mut().enumerate() {
            let (lo, hi) = ports::ret_interval(port, ret, super::PROFILE.analog.max());
            *bits = match self.phase % 4 { 0 => lo, 1 => hi, 2 => 0i32.clamp(lo, hi), _ => 1i32.clamp(lo, hi) } as u32;
        }
        self.phase += 1;
        PortCall::Done
    }
}

pub fn check(m: &Module<'_>, ranges: &Ranges, program: &str) {
    let mut state = m.state_init().to_vec();
    let mut scratch = vec![0; (0..m.entries().len()).map(|k| m.scratch_words(k)).max().unwrap_or(0)];
    let mut observer = Check::new(m, ranges, program);
    let mut inputs = Inputs { phase: 0 };
    // All entries, even ones the scheduler's particular input path never fires.
    for round in 0..4 {
        inputs.phase = round;
        for k in 0..m.entries().len() {
            assert!(matches!(exec::run_observed(m, k, &mut state, &mut scratch, &mut inputs, &mut observer), exec::Exit::Done { .. }));
        }
    }
}

pub fn print_census(program: &str, m: &Module<'_>, ranges: &Ranges) {
    let census = range::census(ranges);
    let counts = |c: Classes| format!("{}/{}/{}/{}/{}", c.u8, c.i8, c.u16, c.i16, c.i32);
    for (k, e) in census.entries.iter().enumerate() {
        println!("CENSUS {program} {} u8/i8/u16/i16/i32 values={} vars={} state={}",
            m.string(m.entries()[k].name), counts(e.values), counts(e.vars), counts(census.state));
    }
}
