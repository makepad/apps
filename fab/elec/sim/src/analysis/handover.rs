//! A run handed from one lowering of a circuit to another at time `t`: a mode switch (Exact or
//! Fast to Logic and back) or an edit that changes the topology. The new solver takes, by name:
//!
//! - every unknown the old run has, and every device's state (`Bank::state`);
//! - what an old device stands for among the new run's parts (`Bank::stands_for`: a timing
//!   cell's input voltage and switch positions, going to the switch pair), before the old run's
//!   value of the same unknown: a cell's input node only follows its form, a rounding apart, and
//!   the form is the input's state;
//! - what a new device absorbs from the old run's parts (`Bank::adopt`: the pair's input and
//!   position, going to the cell).
//!
//! Voltage unknowns that only the new circuit has and that nothing stood for (a part's internal
//! node, a net a digital lowering left out) take a DC solve with every carried voltage held, so
//! the run goes on from where it was and nothing new starts at 0 V. Cost: O(unknowns + devices)
//! by hashed names, plus that DC solve when there is something to fill.

use crate::analysis::op::{operating_point_from, OpSeed};
use crate::newton::{SimDiag, Solver};
use crate::stamp::{Given, RunState, Unk, UnkKind};
use std::collections::{HashMap, HashSet};

impl RunState for Solver {
    fn value(&self, unknown: &str) -> Option<f64> {
        self.unknown(unknown).map(|u| self.x[u as usize])
    }
    fn state_of(&self, device: &str) -> Option<Vec<f64>> {
        self.find_device(device).map(|(b, i)| self.banks[b].state(i))
    }
}

/// What a hand-over did.
#[derive(Clone, Debug, Default)]
pub struct HandOver {
    /// The new run's unknowns that took their value from the old run (by name, or what an old
    /// device stood for).
    pub carried: usize,
    /// The voltage unknowns the DC fill gave, by name.
    pub filled: Vec<String>,
    /// The fill's failure: those unknowns keep the new solver's start.
    pub fill_failed: Option<SimDiag>,
    /// New devices that absorb parts of the old run but did not find them there (they keep their
    /// own start).
    pub unadopted: Vec<String>,
}

/// Hands the run of `old` at `t` to `new`, a solver of another lowering not yet stepped; then
/// `Stepper::resume(new, t, ..)` goes on from there.
pub fn hand_over(new: &mut Solver, old: &Solver, t: f64) -> HandOver {
    let n = new.unknowns.len();
    let old_unknowns: HashMap<&str, usize> = old.unknowns.iter().enumerate().skip(1).map(|(i, u)| (u.name.as_str(), i)).collect();
    let mut carried = vec![false; n];
    carried[0] = true;
    for i in 1..n {
        if let Some(&j) = old_unknowns.get(new.unknowns[i].name.as_str()) {
            new.x[i] = old.x[j];
            carried[i] = true;
        }
    }
    let names = |s: &Solver| -> HashMap<String, (usize, usize)> {
        s.banks.iter().enumerate().flat_map(|(b, bank)| (0..bank.len()).map(move |i| (bank.inst_name(i).to_string(), (b, i)))).collect()
    };
    let (old_at, new_at) = (names(old), names(new));
    // what the old run's devices that the new one lacks stand for there
    let mut given = Vec::new();
    for bank in &old.banks {
        for i in 0..bank.len() {
            if !new_at.contains_key(bank.inst_name(i)) {
                bank.stands_for(i, t, &mut given);
            }
        }
    }
    let new_unknowns: HashMap<&str, usize> = new.unknowns.iter().enumerate().skip(1).map(|(i, u)| (u.name.as_str(), i)).collect();
    // the first device to stand for an unknown gives it, over the old run's value of it
    let mut set_x = Vec::new();
    let mut given_to = vec![false; n];
    for g in &given {
        if let Given::Unknown(name, v) = g {
            if let Some(&u) = new_unknowns.get(name.as_str()) {
                if !given_to[u] {
                    set_x.push((u, *v));
                    given_to[u] = true;
                    carried[u] = true;
                }
            }
        }
    }
    drop(new_unknowns);
    for (u, v) in set_x {
        new.x[u] = v;
    }
    let old_names: HashSet<&str> = old_at.keys().map(String::as_str).collect();
    let mut report = HandOver::default();
    set_states(new, old, t, &given, &new_at, &old_names, &mut report.unadopted);
    let open: Vec<usize> = (1..n).filter(|&i| !carried[i] && new.unknowns[i].kind == UnkKind::Voltage).collect();
    if !open.is_empty() {
        let kept = new.x.clone();
        let held: Vec<(Unk, f64)> = (1..n).filter(|&i| carried[i] && new.unknowns[i].kind == UnkKind::Voltage).map(|i| (i as Unk, kept[i])).collect();
        let ics = std::mem::replace(&mut new.ics, held);
        let seed = OpSeed::of(new);
        let solved = operating_point_from(new, true, Some(&seed));
        new.ics = ics;
        match solved {
            Ok(_) => {
                for i in (0..n).filter(|&i| carried[i]) {
                    new.x[i] = kept[i];
                }
                report.filled = open.iter().map(|&i| new.unknowns[i].name.clone()).collect();
            }
            Err(d) => {
                new.x.copy_from_slice(&kept);
                report.fill_failed = Some(d);
            }
        }
        // the fill accepted its DC point into the banks: the carried states go in again
        report.unadopted.clear();
        set_states(new, old, t, &given, &new_at, &old_names, &mut report.unadopted);
    }
    report.carried = carried.iter().skip(1).filter(|&&c| c).count();
    report
}

/// The device states: by name, then what old devices stood for, then what new ones adopt.
fn set_states(new: &mut Solver, old: &Solver, t: f64, given: &[Given], new_at: &HashMap<String, (usize, usize)>, old_names: &HashSet<&str>, unadopted: &mut Vec<String>) {
    for bank in &old.banks {
        for i in 0..bank.len() {
            if let Some(&(nb, ni)) = new_at.get(bank.inst_name(i)) {
                let state = bank.state(i);
                if !state.is_empty() {
                    new.banks[nb].set_state(ni, &state);
                }
            }
        }
    }
    for g in given {
        if let Given::State(name, s) = g {
            if let Some(&(nb, ni)) = new_at.get(name.as_str()) {
                new.banks[nb].set_state(ni, s);
            }
        }
    }
    for bank in new.banks.iter_mut() {
        for i in 0..bank.len() {
            if !old_names.contains(bank.inst_name(i)) && !bank.adopt(i, t, old) {
                unadopted.push(bank.inst_name(i).to_string());
            }
        }
    }
}
