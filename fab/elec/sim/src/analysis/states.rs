//! The other states a plan's cuts are tested in, from a fixed start (R-GEN step 4, the input
//! states): a function of what the board is set to, not of where the run is, so they are kept,
//! and a plan with the same settings takes them, and every test made in them, again.
//!
//! - The key (`key`): the circuit as lowered with its retunes (every value, so the constant
//!   sources a person's switches read), the values edited since (`Solver::edits`), each pad's mode
//!   by kind (driven, or an input or open drain with its pull: never its level, which firmware
//!   moves on every hit) and the plan's options. Not the run's `x`, not `t`.
//! - The base: a fresh solver of the circuit with those edits, each pad in its kind at its idle
//!   level (high where the circuit's own pad says so: driven high, or pulled up as an input or
//!   released open drain; else low), solved for its operating point from a fixed start at `t = 0`.
//! - The states, each stepped or settled from the base: the base itself; the circuit's own
//!   switching devices each in its other position a step after (bounded with the present state,
//!   `State::bound`); per pad that has levels, its other level a step after, that level settled,
//!   and its idle level again a step after that (an edge in, its rest solved from the base's solution,
//!   an edge out); every switch a
//!   person sets flipped (stability only) and each alone. A pad's state reaches the unknowns it
//!   moved (`moved`), a switch's those its switches touch.
//! - Beside them, no bar tested in it: per pad that has levels, its other level at its first
//!   instant (`State::first_instant`), every charge still the base's, for the plan-time test of
//!   the imports (`round::charge_hold`).
//! - Not tested: every pad at its other level at once (an all active-low board at rest). The
//!   report says so where a board has two or more.
//! - When the base cannot be made (a fresh solver on another layout, or no operating point from
//!   the fixed start), the states come from the run as before and are not kept (`run_states`).
//! - Kept (`StateCache`) a few keys deep, shared by a planner's forks: a key evicted is made again
//!   when it comes back, and counted.
//! - Beside them, each reader's import test (`round::charge_hold`) by everything it reads, its
//!   matrices in every state among them: kept while a key whose plan took it is, so a plan after a
//!   value edit makes again only the tests of the readers the edit reached.

use super::held::{touched, Others, State};
use super::round::{Carry, ReaderTable};
use super::op::{operating_point, operating_point_from, OpSeed};
use super::partition::{Land, PartOpts};
use crate::circuit::{PinMode, PinPull, SimCircuit};
use crate::newton::Solver;
use crate::rounds::{fan_out, flipped, stepped, Helpers, Unit};
use crate::stamp::UnkKind;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::fmt::Write;
use std::hash::Hasher;
use std::sync::{Arc, Mutex};

/// How many keys the cache keeps. One evicted is made again when it comes back (a miss), and
/// counted, so a run that thrashes it shows.
const KEYS: usize = 4;

/// A planner's kept states by key (`key`), shared by its forks and its run's snapshots: a
/// function of the key, so any copy may take an entry another made.
#[derive(Clone, Default)]
pub struct StateCache(Arc<Mutex<Kept>>);

#[derive(Default)]
struct Kept {
    /// The entries, the most recently used last.
    entries: VecDeque<(u64, Arc<Others>)>,
    count: CacheCount,
    /// The readers' import tests by everything each reads (`round::charge_hold`), with the keys
    /// whose plans took them: one stays while a key that took it is kept.
    tables: Vec<(u128, Arc<ReaderTable>, Vec<u64>)>,
    /// What runs refused and landed (`remember`), by `memory_key`. Refused nodes and landed
    /// prefixes stay for the cache's lifetime. Memory is process-local, so a process never holds
    /// name-only entries for landed chains.
    memory: Vec<(u64, Vec<String>, Vec<Land>)>,
    /// The newest cut of a plan made in its key's states, by `circuit_key` (`round::Carry`): what
    /// a start at other part values begins on. One per circuit, the most recently kept last, as
    /// many circuits as `KEYS`.
    carries: VecDeque<(u64, Arc<Carry>)>,
    /// The pads' modes by name as the newest run's start warmed in them (`keep_start_pads`).
    start_pads: Vec<(String, PinMode)>,
}

/// What a cache has done: lookups that found their key and those that did not, keys evicted, and
/// what it keeps (keys, bytes).
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheCount {
    pub hits: u64,
    pub misses: u64,
    pub evicted: u64,
    pub keys: usize,
    pub bytes: usize,
}

impl std::fmt::Debug for StateCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StateCache").field(&self.count()).finish()
    }
}

impl StateCache {
    /// The entry kept for `key`, now the most recently used; counts a hit or a miss.
    pub fn get(&self, key: u64) -> Option<Arc<Others>> {
        let mut k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match k.entries.iter().position(|e| e.0 == key) {
            Some(i) => {
                let e = k.entries.remove(i).expect("a found entry");
                let got = e.1.clone();
                k.entries.push_back(e);
                k.count.hits += 1;
                Some(got)
            }
            None => {
                k.count.misses += 1;
                None
            }
        }
    }

    /// Whether an entry is kept for `key`, counting nothing and moving nothing.
    pub fn has(&self, key: u64) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).entries.iter().any(|e| e.0 == key)
    }

    /// Whether a plan was made in the entry kept for `key` (`Others::planned`): the next pays no
    /// miss. An entry another plan is still making answers no. Counting nothing, moving nothing.
    pub fn planned(&self, key: u64) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).entries.iter().any(|e| e.0 == key && e.1.planned.load(std::sync::atomic::Ordering::Acquire))
    }

    /// Keeps `others` for `key`, evicting the least recently used past `KEYS`.
    pub fn put(&self, key: u64, others: Arc<Others>) {
        let mut k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        k.entries.retain(|e| e.0 != key);
        k.entries.push_back((key, others));
        while k.entries.len() > KEYS {
            k.entries.pop_front();
            k.count.evicted += 1;
        }
        k.prune(None);
    }

    /// Keeps `carry` as the newest cut of the circuit `circuit` (`circuit_key`).
    pub(crate) fn keep_carry(&self, circuit: u64, carry: Arc<Carry>) {
        let mut k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        k.carries.retain(|c| c.0 != circuit);
        k.carries.push_back((circuit, carry));
        while k.carries.len() > KEYS {
            k.carries.pop_front();
        }
    }

    /// The newest cut kept for the circuit `circuit` (`circuit_key`).
    pub(crate) fn carry(&self, circuit: u64) -> Option<Arc<Carry>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).carries.iter().find(|c| c.0 == circuit).map(|c| c.1.clone())
    }

    /// The import test kept for a reader by everything it reads (`round::charge_hold`), now taken
    /// by a plan in the states of `states` too.
    pub(crate) fn table(&self, key: u128, states: u64) -> Option<Arc<ReaderTable>> {
        let mut k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let e = k.tables.iter_mut().find(|e| e.0 == key)?;
        if !e.2.contains(&states) {
            e.2.push(states);
        }
        Some(e.1.clone())
    }

    /// Keeps a reader's import test by everything it reads, made by a plan in the states of
    /// `states`; one no kept key took any more goes.
    pub(crate) fn put_table(&self, key: u128, states: u64, table: Arc<ReaderTable>) {
        let mut k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        k.tables.retain(|e| e.0 != key);
        k.prune(Some(states));
        k.tables.push((key, table, vec![states]));
    }

    /// Keeps each refused node and each landed prefix once across runs of `key` (`memory_key`,
    /// `LiveSim::set_memory`), in the order first met.
    pub fn remember(&self, key: u64, refused: &[String], landed: &[Land]) {
        let mut k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let i = match k.memory.iter().position(|m| m.0 == key) {
            Some(i) => i,
            None => {
                k.memory.push((key, Vec::new(), Vec::new()));
                k.memory.len() - 1
            }
        };
        let m = &mut k.memory[i];
        for node in refused {
            if !m.1.contains(node) {
                m.1.push(node.clone());
            }
        }
        for land in landed {
            if !m.2.contains(land) {
                m.2.push(land.clone());
            }
        }
    }

    /// Keeps the pads' modes by name as a run's start warmed in them (`LiveSim::set_memory`): what
    /// its process's set-up left them in. A host warming the next start ahead while its run is
    /// stopped puts that run's pads in them (`start_pads`). O(pads).
    pub fn keep_start_pads(&self, pads: Vec<(String, PinMode)>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).start_pads = pads;
    }

    /// The pads' modes by name as the newest run's start warmed in them (`keep_start_pads`); none
    /// before a run's start has warmed.
    pub fn start_pads(&self) -> Vec<(String, PinMode)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).start_pads.clone()
    }

    /// The refused nodes and landed prefixes for `key` in the order first met (`remember`).
    pub fn recall(&self, key: u64) -> (Vec<String>, Vec<Land>) {
        let k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        k.memory.iter().find(|m| m.0 == key).map(|m| (m.1.clone(), m.2.clone())).unwrap_or_default()
    }

    pub fn count(&self) -> CacheCount {
        let k = self.0.lock().unwrap_or_else(|e| e.into_inner());
        CacheCount { keys: k.entries.len(), bytes: k.entries.iter().map(|e| e.1.bytes()).sum(), ..k.count }
    }
}

impl Kept {
    /// Drops the import tests no kept key took (nor `also`, the key a plan is in).
    fn prune(&mut self, also: Option<u64>) {
        let Kept { entries, tables, .. } = self;
        tables.retain(|e| e.2.iter().any(|s| Some(*s) == also || entries.iter().any(|x| x.0 == *s)));
    }
}

/// A hasher written to as text, so a value's `Debug` form is hashed as it is produced.
struct Hashing(DefaultHasher);

impl Write for Hashing {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

/// A pad's mode by kind, without its level (`key`).
fn kind(mode: PinMode) -> String {
    match mode {
        PinMode::OutHigh | PinMode::OutLow | PinMode::Pwm { .. } => "driven".into(),
        PinMode::Input(pull) => format!("input {pull:?}"),
        PinMode::OpenDrain { pull, .. } => format!("open drain {pull:?}"),
    }
}

/// Whether two modes of a pad are of one kind (`kind`, without allocating): a plan made at one is
/// made at the other, its states taking every level of the kind.
pub fn same_kind(a: PinMode, b: PinMode) -> bool {
    match (a, b) {
        (PinMode::OutHigh | PinMode::OutLow | PinMode::Pwm { .. }, PinMode::OutHigh | PinMode::OutLow | PinMode::Pwm { .. }) => true,
        (PinMode::Input(p), PinMode::Input(q)) | (PinMode::OpenDrain { pull: p, .. }, PinMode::OpenDrain { pull: q, .. }) => p == q,
        _ => false,
    }
}

/// The key the states are kept by (the module's): what the board is set to, never where the run
/// stands. O(the circuit's size): its `Debug` form is hashed as it is written.
pub fn key(ckt: &SimCircuit, whole: &Solver, o: &PartOpts) -> u64 {
    let mut h = Hashing(DefaultHasher::new());
    // the nodes a run refused and the chains it landed change the verdicts, not the states
    let o = PartOpts { never_cut: Vec::new(), land: Vec::new(), ..o.clone() };
    write_board(&mut h, ckt, whole);
    let _ = write!(h, "|{o:?}");
    write_pads(&mut h, whole);
    h.0.finish()
}

/// The key a plan's cut is carried by (`StateCache::carry`, `round::build_carried`): the circuit's
/// structure without its part values. Its unknowns by name and kind; each instance by kind and
/// name with the unknowns it reads and the matrix entries it stamps; its nodes by name and whether
/// each is a declared supply; its options, the plan's options as `key` takes them, and the pads'
/// kinds. Not the devices' values, their models or the temperature: a cut carried across those is
/// proved again where it is built. O(the circuit's size).
pub fn circuit_key(ckt: &SimCircuit, whole: &Solver, o: &PartOpts) -> u64 {
    let mut h = Hashing(DefaultHasher::new());
    let o = PartOpts { never_cut: Vec::new(), land: Vec::new(), ..o.clone() };
    for u in &whole.unknowns {
        let _ = write!(h, "{} {:?}|", u.name, u.kind);
    }
    let (mut unks, mut slots) = (Vec::new(), Vec::new());
    for bank in &whole.banks {
        for i in 0..bank.len() {
            unks.clear();
            slots.clear();
            bank.inst_unks(i, &mut unks);
            bank.inst_slots(i, &mut slots);
            let _ = write!(h, "{} {} {unks:?}", bank.kind(), bank.inst_name(i));
            for &s in &slots {
                let _ = write!(h, " {},{}", whole.slot_row[s as usize], whole.slot_col[s as usize]);
            }
            let _ = write!(h, "|");
        }
    }
    for n in &ckt.nodes {
        let _ = write!(h, "{} {}|", n.name, n.supply);
    }
    let _ = write!(h, "{:?}|{o:?}", ckt.opts);
    write_pads(&mut h, whole);
    h.0.finish()
}

/// Each pad's mode by its kind (`kind`), as the keys take them.
fn write_pads(h: &mut Hashing, whole: &Solver) {
    for (b, bank) in whole.banks.iter().enumerate() {
        for i in 0..bank.len() {
            if let Some(mode) = bank.pin_mode(i) {
                let _ = write!(h, "|{b} {i} {}", kind(mode));
            }
        }
    }
}

/// The circuit as the run has it (`key`, `memory_key`): its nodes, devices, models, options and
/// temperature, and its effective part values: the values the run has, not the edits that made
/// them (an edit back to the circuit's own value is none, a key pressed and released, and the rest
/// go in by device and key, in any order made).
fn write_board(h: &mut Hashing, ckt: &SimCircuit, whole: &Solver) {
    debug_assert_eq!(whole.edits.len(), whole.edits_own.len(), "each edit keeps its own value");
    let mut values: Vec<(&str, &str, u64)> = whole.edits.iter().zip(&whole.edits_own)
        .filter(|((_, _, v), own)| own.is_none_or(|o| o.to_bits() != v.to_bits()))
        .map(|((device, key, v), _)| (device.as_str(), key.as_str(), v.to_bits()))
        .collect();
    values.sort_unstable();
    let _ = write!(h, "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}", ckt.nodes, ckt.devices, ckt.models, ckt.opts, ckt.temp_c, values);
}

/// The key a run's refusals and landings are remembered by (`StateCache::remember`): the circuit
/// with its effective part values (`write_board`), how the run steps it (`run`: its round's
/// length, its step bound, its time resolution) and the judge's version (`rounds::JUDGE`), so a
/// changed judge forgets them; never the pads' kinds or where the run stands, as a refusal is the
/// run's from where it is found on, whatever its pads do. O(the circuit's size).
pub fn memory_key(ckt: &SimCircuit, whole: &Solver, run: &dyn std::fmt::Debug) -> u64 {
    let mut h = Hashing(DefaultHasher::new());
    write_board(&mut h, ckt, whole);
    let _ = write!(h, "|{run:?}|judge {}", crate::rounds::JUDGE);
    h.0.finish()
}

/// A pad's mode in its kind at a level: driven high or low, open drain released or asserted.
fn at_level(mode: PinMode, high: bool) -> PinMode {
    match mode {
        PinMode::OpenDrain { pull, .. } => PinMode::OpenDrain { low: !high, pull },
        _ if high => PinMode::OutHigh,
        _ => PinMode::OutLow,
    }
}

/// Whether a pad idles high, by the circuit's own pad (its mode as lowered, the reset): driven
/// high, or pulled up as an input or a released open drain. Low where nothing says.
fn idle_high(reset: PinMode) -> bool {
    matches!(reset, PinMode::OutHigh | PinMode::Input(PinPull::Up) | PinMode::OpenDrain { low: false, pull: PinPull::Up })
}

fn level(high: bool) -> &'static str {
    if high { "high" } else { "low" }
}

/// The unknowns `b` moved from `a`: further apart than the solver's tolerances (`vntol` for a
/// voltage, `abstol` for a current, and `reltol` of the larger).
pub fn moved(a: &Solver, b: &Solver) -> Vec<bool> {
    let o = &a.opts;
    (0..=a.n).map(|u| {
        if u == 0 {
            return false;
        }
        let (x, y) = (a.x[u], b.x[u]);
        let abs = if a.unknowns[u].kind == UnkKind::Voltage { o.vntol } else { o.abstol };
        (x - y).abs() > abs + o.reltol * x.abs().max(y.abs())
    }).collect()
}

/// The states from a fixed start (the module's), with what the report should say of them; why
/// not when the base cannot be made. `user` and `own` are the switches (`rounds::switches`).
pub fn fixed(ckt: &SimCircuit, whole: &Solver, o: &PartOpts, user: &[String], own: &[String], pool: Option<&Helpers>) -> Result<(Vec<State>, Vec<String>), String> {
    let span = 0.5 / o.fmax;
    let mut base = Solver::new(ckt).map_err(|e| format!("the circuit's fresh solver fails: {e}"))?;
    let same = base.n == whole.n && base.g.len() == whole.g.len() && base.slot_row == whole.slot_row && base.slot_col == whole.slot_col
        && base.banks.len() == whole.banks.len() && base.banks.iter().zip(&whole.banks).all(|(a, b)| a.kind() == b.kind() && a.len() == b.len());
    if !same {
        return Err("the circuit's fresh solver is not on the run's layout".into());
    }
    for (device, key, value) in &whole.edits {
        base.set_param(device, key, *value);
    }
    // the pads in their kinds as the run has them; those with levels at their idle level
    let mut pads = Vec::new();
    for (b, bank) in whole.banks.iter().enumerate() {
        for i in 0..bank.len() {
            let Some(mode) = bank.pin_mode(i) else { continue };
            if matches!(mode, PinMode::Input(_)) {
                base.pin_write((b, i), -1.0, mode);
                continue;
            }
            let name = bank.inst_name(i).to_string();
            let reset = ckt.devices.gpio.iter().find(|g| g.name == name).map_or(mode, |g| g.mode);
            let idle = idle_high(reset);
            base.pin_write((b, i), -1.0, at_level(mode, idle));
            pads.push(((b, i), name, mode, idle));
        }
    }
    operating_point(&mut base, false).map_err(|e| format!("no operating point from a fixed start: {e}"))?;
    // a pad's rest at its other level is solved from the base's solution: a function of the key
    // still, and a few iterations where a fresh start takes the whole ladder
    let seed = OpSeed::of(&base);
    let mut notes = Vec::new();
    if !pads.is_empty() {
        let list = pads.iter().map(|(_, name, _, idle)| format!("{name} {}", level(*idle))).collect::<Vec<_>>().join(", ");
        notes.push(format!("pads at their idle levels in the base: {list}"));
    }
    if pads.len() > 1 {
        notes.push(format!("not tested: the {} pads at their other levels at once", pads.len()));
    }
    let all = vec![true; whole.n + 1];
    let mut states = vec![State { s: base.clone(), t: 0.0, touch: all, stability_only: false, first_instant: false, bound: false, what: "the board at rest from a fixed start".into() }];
    if let Some(s) = flipped(&base, own, 0.0, span) {
        states.push(State { s, t: span, touch: touched(&base, own), stability_only: false, first_instant: false, bound: true, what: "the circuit's own devices in either state".into() });
    }
    // each pad's three states are a unit on the pool's cores (`fan_out`), in the pads' order, then
    // each flip of the switches a person sets (`user_flips`), in theirs
    let mut units: Vec<Unit<(Vec<State>, Option<String>)>> = pads.into_iter().map(|(dev, name, mode, idle)| {
        let (base, seed) = (base.clone(), seed.clone());
        Box::new(move || pad_states(base, &seed, dev, &name, mode, idle, span)) as Unit<_>
    }).collect();
    units.extend(user_flips(user).into_iter().map(|flip| {
        let base = base.clone();
        Box::new(move || (user_state(&base, flip, 0.0, span, span).into_iter().collect(), None)) as Unit<_>
    }));
    for (got, note) in fan_out(units, pool).map_err(|f| f.diag("a pad's or a switch's states").message)? {
        states.extend(got);
        notes.extend(note);
    }
    Ok((states, notes))
}

/// A pad's first instant is a step this share of the sample period long: its charges move by
/// about this share of what a step moves them, so a junction its write turns on still conducts.
const FIRST_INSTANT: f64 = 1e-3;

/// A pad's states from the `base` (its own copy): its other level a step after (and at its first
/// instant, `State::first_instant`), that level settled (from the base's solution, `seed`) and back a step after that rest; with a note when
/// the rest has no operating point.
fn pad_states(base: Solver, seed: &OpSeed, dev: (usize, usize), name: &str, mode: PinMode, idle: bool, span: f64) -> (Vec<State>, Option<String>) {
    let (other, back) = (at_level(mode, !idle), at_level(mode, idle));
    let mut states = Vec::new();
    let mut edge = base.clone();
    if edge.pin_write(dev, 0.0, other).is_some() {
        if let Some(s) = stepped(edge, 0.0, span) {
            states.push(State { touch: moved(&base, &s), s, t: span, stability_only: false, first_instant: false, bound: false, what: format!("{name} {} a step after", level(!idle)) });
        }
    }
    // its first instant: the write long done, every charge the base's (a step too short to move one)
    let mut first = base.clone();
    if first.pin_write(dev, -1.0, other).is_some() {
        if let Some(s) = stepped(first, 0.0, FIRST_INSTANT * span) {
            states.push(State { touch: moved(&base, &s), s, t: FIRST_INSTANT * span, stability_only: false, first_instant: true, bound: false, what: format!("{name} {} at its first instant", level(!idle)) });
        }
    }
    let mut rest = base.clone();
    rest.pin_write(dev, -1.0, other);
    if operating_point_from(&mut rest, false, Some(seed)).is_err() {
        return (states, Some(format!("{name} {}: no operating point from a fixed start, its rest and the edge back untested", level(!idle))));
    }
    let mut edge = rest.clone();
    if edge.pin_write(dev, 0.0, back).is_some() {
        if let Some(s) = stepped(edge, 0.0, span) {
            states.push(State { touch: moved(&rest, &s), s, t: span, stability_only: false, first_instant: false, bound: false, what: format!("{name} back {} a step after", level(idle)) });
        }
    }
    states.push(State { touch: moved(&base, &rest), s: rest, t: 0.0, stability_only: false, first_instant: false, bound: false, what: format!("{name} {} settled", level(!idle)) });
    (states, None)
}

/// Every switch a person sets flipped, stability only (where a device may saturate), then each
/// alone, both bars: the states one edit away, each stepped from `s` at `t` and linearised at `at`.
fn user_states(s: &Solver, user: &[String], t: f64, span: f64, at: f64) -> Vec<State> {
    user_flips(user).into_iter().filter_map(|flip| user_state(s, flip, t, span, at)).collect()
}

/// One of `user_states`' states: the switches it flips, whether it is stability only, and what it is.
struct UserFlip {
    switches: Vec<String>,
    stability_only: bool,
    what: String,
}

/// The flips of the switches a person sets, in `user_states`' order: all of them at once (with
/// more than one), then each alone.
fn user_flips(user: &[String]) -> Vec<UserFlip> {
    let all = (user.len() > 1).then(|| UserFlip { switches: user.to_vec(), stability_only: true, what: "every switch a person sets flipped".into() });
    all.into_iter().chain(user.iter().map(|w| UserFlip { switches: vec![w.clone()], stability_only: false, what: format!("{w} flipped") })).collect()
}

/// The state of `flip` stepped from `s` at `t` and linearised at `at`; none where the step fails.
fn user_state(s: &Solver, flip: UserFlip, t: f64, span: f64, at: f64) -> Option<State> {
    let f = flipped(s, &flip.switches, t, span)?;
    Some(State { s: f, t: at, touch: touched(s, &flip.switches), stability_only: flip.stability_only, first_instant: false, bound: false, what: flip.what })
}

/// The states from the run's own state at `t`, as before a fixed start (kept by nothing), each
/// linearised at `t`: the circuit's own devices each in its other position a step after, with the
/// bound, then the switches a person sets (`user_states`).
pub fn run_states(whole: &Solver, user: &[String], own: &[String], t: f64, span: f64) -> Vec<State> {
    let mut states = Vec::new();
    if let Some(s) = flipped(whole, own, t, span) {
        states.push(State { s, t, touch: touched(whole, own), stability_only: false, first_instant: false, bound: true, what: "the circuit's own devices in either state".into() });
    }
    states.extend(user_states(whole, user, t, span, t));
    states
}
