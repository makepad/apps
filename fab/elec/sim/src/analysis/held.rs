//! Held nodes (ESIMRT S4): which part of a circuit holds each node and which parts only load it,
//! measured from how a test current injected at the node divides among the instances on it; the
//! islands they give are the planner's (`round::plan_blocks`). Two facts come from outside the
//! measure: the control pads (`PartOpts::control`) and the supplies the design declares
//! (`PartOpts::supplies`, `NodeInfo::supply`). No other net name, kind or class enters it; the bank
//! kinds it reads are device types.
//!
//! - Shares: at the linearisation of a solver's state (its `x` and device states, each instance
//!   evaluated alone), for every voltage node `h` on its own row and every instance `d` that stamps
//!   that row, `s_h(d)` is the largest over a coarse grid (DC and one frequency an octave) of
//!   `|sum_k Y^d_hk Z_kh|`: the part of a unit current into `h` that flows into `d`. Over a node's
//!   instances the complex shares sum to one, which checks the stamps.
//! - The hub form, at the pads and the declared supplies only: consumers read the node in the same
//!   round and what they draw comes back a round late, as a current (the holder keeps no ghost); a
//!   cut may go through an instance that is not a resistor or a capacitor; the node's bar is
//!   `hub_tol`, over all its consumers together. What a consumer draws is its block's to give:
//!   every entry it puts in the node's row, in a column its block holds or reads in the same round
//!   (a switch's control, a transistor's base), is part of that return, never a same-round read of
//!   the consumer's block by the holder; in a column only a third island holds, the holder reads
//!   that island in the same round. Every other node is ordinary: bar `cut_tol`, cuts only through
//!   resistors and capacitors, each keeping its ghosts as in `partition`. A cut is read the way
//!   whose late read forces the smaller error: the stiff side first when `tau` (its other voltages'
//!   response to its load read a round late) is at most the soft side's `gain` (their response to
//!   the node read a round late), else the soft side first.
//! - Anchors: `d` is strong at `h` when its share is overwhelming (`ANCHOR_SHARE`, half or more),
//!   when it is the node's largest, when it drives a control pad, or when it is neither a resistor
//!   nor a capacitor and `h` is ordinary; a share under that only orders the candidates, and the
//!   attachment gets its test; an instance weak at every node anchors at an ordinary node before one
//!   in the hub form, and there where its share is largest (the lower node on a tie). An instance
//!   joins the nodes where it is strong and every unknown of its that is not a voltage on its own
//!   row: those unions are the islands. A weak attachment is a cut at `h` with `d` on its soft side;
//!   a node an instance reads without loading it is an exact edge. The control pads are one island.
//! - Forward is same-round: every exact read, and every consumer's read of a held node except at an
//!   ordinary cut whose consumers go first. Same-round cycles merge.
//! - Tests: each held node against the islands on its weak attachments (`partition::measure`), on
//!   the band and on the matrix a step solves (`G + (2 / h) C` at the step `h = 0.5 / fmax`); a
//!   cut passes both. Where the holder keeps no ghost (the hub form, or an attachment that is not a
//!   resistor or a capacitor) the consumers' own terms at the node come off its diagonal and count
//!   in the reflected admittance. A failed test merges the heaviest consumer into the holder; a
//!   declared supply that fails stays declared. A pass's tests are measured on the islands as the
//!   pass began, so a failure is judged on its consumers as grown, soft islands first: one with a
//!   consumer island that is the holder of another failure in the same pass is tested again next
//!   pass, on that island as its own failure grows it (all go in when every failure waits on
//!   another). Which merges go in depends on the pass's failures, never on the order the tests
//!   are taken in. A test reads of the holder only the part of it its matrix entries join to the
//!   node, and of a consumer island the parts its instances at the node attach to: the rest of an
//!   island changes nothing there, and a node is tested again only when what it reads changes.
//! - Two bars (`bars`, `judge`: the round map judges by the same). Stability: a loop through a
//!   late value cannot grow, `rho` under `STABILITY` at every point. Accuracy: what reading it a
//!   round late gets wrong, `rho w / (1 - rho)` with `w` the weight at that point for one round
//!   (`weights_at`, `PartOpts::round`), under the node's bar (`cut_tol`, `hub_tol`) through
//!   `PartOpts::band`. The late read is exact at DC. From half the round's rate up, its error
//!   bound is twice its swing.
//!   The loop's phase is unknown, so recirculation can amplify the error by up to `1 / (1 - rho)`.
//! - States (`held_states`): the search runs at the present state, where accuracy is judged; once
//!   a pass of it merges nothing, each other state (`Others`, made from a fixed start and kept by
//!   what the board is set to, `states`) in turn tests the cuts it reaches (the unknowns its
//!   switches touch or its pad moved, joined by matrix entries, within the islands as they are,
//!   to the node or its consumers' attachments), until one fails; a test made before on all it
//!   reads is taken from there. A state is a corner, stability only, or one the board's inputs or
//!   its own devices put it in, both bars (`State`). The circuit's own devices bound a cut's rho
//!   per consumer over their states (`State::bound`), and both bars hold on the bound. A failure
//!   there merges as at the present state, and the search goes on.
//! - After the search the control island merges as in `partition` and small islands join their
//!   single reader.
//! - Block 0 is the control island (every pad), with every island it reads in the same round
//!   merged into it: nothing an attached process reads is late. Where a declared supply's island
//!   runs is the plan's (`round::plan_blocks`): each island says whether it holds one and whether
//!   an instance of it raises an event.
//! - Reach, printed beside each consumer's rho and gating nothing: `max |Z_kh| |Y'_c|` over the
//!   band and the block's voltages `k`, the largest voltage a volt of the consumer's swing makes
//!   anywhere through the node (`Z_kh` from the share step's solves, on the coarse grid and the
//!   step's matrix, the larger of the two points around each band point).
//!
//! Which instance raises an event (a corner, a crossing, a pin write) is read through a proxy: a
//! pad's driver, a switch, a behavioural source while its bank reads time, and a source with a
//! waveform while its bank has a corner ahead.

use super::ac::AcSweep;
use super::partition::{components, condense, control_set, hz, line, measure, measure_each, measure_each_in, peak, sort_graph, structure, with_scratch, Edge, Entry, Groups, LocalMatrix, Measured, NodeTest, PartOpts, Sets, Structure};
use super::round::{RoundMap, KEEP};
use crate::newton::{SimDiag, Solver};
use crate::rounds::{fan_out, Helpers, Unit};
use crate::num::C64;
use crate::stamp::{EvalCx, Stamps, Tier, UnkKind};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::f64::consts::PI;
use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Ordinary nodes under this driving-point impedance are named in the report, as nodes a design
/// may mean as supplies. It only informs the report.
const STIFF_OHMS: f64 = 100.0;

/// The stability bar: a cut's `rho` stays under this at every point, in every state tested, so a
/// loop through its late value cannot grow. A margin chosen, not derived.
pub const STABILITY: f64 = 0.5;

/// A state the cuts must hold in beside the present one (`held_states`): the solver with switches
/// in their other positions or a pad at its other level, stepped or settled (`rounds::flipped`,
/// `states::fixed`).
pub struct State {
    pub s: Solver,
    /// The time it stands at, where it is linearised.
    pub t: f64,
    /// The unknowns it reaches (`touched` for its switches, `states::moved` for a pad): it tests
    /// the cuts whose holder or consumers hold one of them.
    pub touch: Vec<bool>,
    /// Only the stability bar holds in it: a corner no single edit reaches.
    pub stability_only: bool,
    /// A pad's first instant after its write: the pad at its new level, every charge still the
    /// base's (`states::fixed`). No bar is tested in it: only the plan-time test of the imports
    /// reads it (`round::charge_hold`), for a junction a jump turns on that a step later has
    /// already charged its capacitor and turned off again.
    pub first_instant: bool,
    /// Its devices' other states (each switching element flipped, a device's switches together):
    /// a cut's rho is bounded over them per consumer, `max |Z_S,ss|` over the present state and
    /// this one times the sum over its consumers of each one's larger `|Y'_j|`, so it holds
    /// whatever state each device is in, and the bars hold on that bound (both, unless
    /// `stability_only`). A present state at rest (an oscillator held still) says nothing of the
    /// states the circuit's own devices take it to; the bound does.
    pub bound: bool,
    /// What it is, for the report.
    pub what: String,
}

/// The unknowns the switches `which` read or write in `s` (`State::touch`).
pub fn touched(s: &Solver, which: &[String]) -> Vec<bool> {
    let mut touch = vec![false; s.n + 1];
    let mut unks = Vec::new();
    for w in which {
        if let Some((b, i)) = s.find_device(w) {
            unks.clear();
            s.banks[b].inst_unks(i, &mut unks);
            for &u in unks.iter().filter(|&&u| u != 0 && (u as usize) <= s.n) {
                touch[u as usize] = true;
            }
        }
    }
    touch
}

/// The other states made ready for the search (`held_states`): each one's linearisation and
/// reach, and what its node tests found, by everything a test reads, so a search on the same
/// states takes a test it has made before from here (`states::StateCache` keeps them by key).
/// The tests run on the present solver's layout, which they must share.
pub struct Others {
    list: Vec<Other>,
    /// The pads' first instants (`State::first_instant`), kept for the plan-time test of the
    /// imports alone.
    first: Vec<First>,
    len: usize,
    n: usize,
    /// The node tests made in these states by everything each reads (`TestKey`), with whether
    /// each fell back to the DC point: shared with the searches that take them, never copied.
    memo: Mutex<HashMap<TestKey, Arc<(Result<Measured, usize>, bool)>>>,
    /// With a fixed start, the node tests made at the base (the held step's primary state) by
    /// everything each reads besides the base's values (`BaseKey`), with whether each fell back.
    base_tests: Mutex<HashMap<BaseKey, Arc<(Result<Measured, usize>, bool)>>>,
    /// With a fixed start, the base (`states::fixed`'s first state): the held step's primary
    /// state, as its solver (behind a lock, as a solver's banks are not shared between threads),
    /// its time and its place in `list`.
    base: Option<(Mutex<Solver>, f64, usize)>,
    /// The round maps made in these states (`round::plan_blocks_kept`), by the plan mapped (its
    /// structure), the state (its place in `list`) and the accuracy bars' scale's bits: what each
    /// found and, failing, the blocks to merge, why, and the link's cuts.
    pub(crate) maps: Mutex<HashMap<(u64, usize, u64), (RoundMap, Option<super::round::Fail>)>>,
    /// Per plan structure as built, before its ports are fitted: the holder ports fitted at the
    /// base and the report's lines for them (`round::fit_ports_keyed`), so a plan made again on
    /// the key sets them without fitting them again, as it keeps its maps.
    pub(crate) ports: Mutex<HashMap<u64, (Vec<crate::rounds::HolderPort>, Vec<String>)>>,
    /// How they were made, for the report.
    pub what: String,
    /// Wall time making them (settling and linearising), ms.
    pub ms: f64,
    /// A plan was made in them (`round::plan_blocks_kept`): its tests and maps are kept, so the
    /// next with the same key pays no miss (`StateCache::planned`).
    pub(crate) planned: std::sync::atomic::AtomicBool,
    /// The circuit's own switching devices (`rounds::switches`): a holder with one moves its own
    /// node (`forward`). With none known, every switch counts as one.
    own: Option<Vec<String>>,
}

struct Other {
    lin: Arc<Kept>,
    seed: Seed,
    touch: Vec<bool>,
    stability_only: bool,
    bound: bool,
    what: String,
}

/// A pad's first instant as the others keep it: no linearisation, its seed only.
struct First {
    seed: Seed,
    touch: Vec<bool>,
    what: String,
}

/// A state as the round map seeds a plan's blocks from it (`round::plan_blocks_kept`): its
/// solution, its devices' states (`Bank::state`) and its time.
pub(crate) struct Seed {
    x: Vec<f64>,
    dev: Vec<(u32, u32, Vec<f64>)>,
    pub(crate) t: f64,
}

impl Seed {
    fn of(s: &Solver, t: f64) -> Seed {
        let mut dev = Vec::new();
        for (b, bank) in s.banks.iter().enumerate() {
            for i in 0..bank.len() {
                let state = bank.state(i);
                if !state.is_empty() {
                    dev.push((b as u32, i as u32, state));
                }
            }
        }
        Seed { x: s.x.clone(), dev, t }
    }

    /// A block's solver `to` put in this state: `unks` its unknowns (local, in the whole), `devs`
    /// its devices ((bank, instance) in the whole, in it), as `rounds::seed_from` does by name.
    pub(crate) fn put(&self, to: &mut Solver, unks: &[(usize, usize)], devs: &[((u32, u32), (usize, usize))]) {
        for &(l, g) in unks {
            to.x[l] = self.x[g];
        }
        for (at, (b, k)) in devs {
            if let Ok(i) = self.dev.binary_search_by(|d| (d.0, d.1).cmp(at)) {
                to.banks[*b].set_state(*k, &self.dev[i].2);
            }
        }
    }

    fn bytes(&self) -> usize {
        8 * self.x.len() + self.dev.iter().map(|d| 32 + 8 * d.2.len()).sum::<usize>()
    }
}

/// A state the round map is judged in besides the base (`Others::mapped`): its place (its seed is
/// `Others::seed`), the unknowns it reaches, whether only the stability bar holds in it, and what
/// it is.
pub(crate) struct Mapped<'a> {
    pub(crate) at: usize,
    pub(crate) touch: &'a [bool],
    pub(crate) stability_only: bool,
    pub(crate) what: &'a str,
}

/// Everything a node test reads besides a state's values: its node and diagonal, the members of
/// its holder's part and of each consumer's, and per consumer island the instances whose own
/// terms at the node it takes (`own_terms`), which a state's values make its terms. Made once a
/// pass per held node and shared by every state's test of it.
#[derive(PartialEq, Eq, Hash)]
struct TestShape {
    node: usize,
    diag: usize,
    stiff: Vec<usize>,
    soft: Vec<Vec<usize>>,
    own: Vec<Vec<usize>>,
}

/// A `TestShape` shared, hashed once: a lookup hashes a number, and compares the shape only on a
/// hash that matches.
#[derive(Clone)]
struct Shaped(Arc<TestShape>, u64);

impl Shaped {
    fn new(shape: TestShape) -> Shaped {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        shape.hash(&mut h);
        Shaped(Arc::new(shape), h.finish())
    }
}

impl PartialEq for Shaped {
    fn eq(&self, other: &Shaped) -> bool {
        self.1 == other.1 && (Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0)
    }
}

impl Eq for Shaped {}

impl std::hash::Hash for Shaped {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        h.write_u64(self.1);
    }
}

/// A node test in a state, by everything it reads: the state (its place in the others, whose
/// values are fixed by the key) and its shape.
#[derive(Clone, PartialEq, Eq, Hash)]
struct TestKey {
    state: usize,
    shape: Shaped,
}

/// A node test at the base by everything it reads but the base's own values, bit for bit: its
/// node and the node's diagonal, the stiff side and the consumer islands (their members), the
/// consumers' own terms at each point and at the DC fallback, and the stiff side's imports. The
/// rest it reads is the base's: its linearisation (the band and the fallback's values), its
/// entries, rows and voltage unknowns, all fixed by the key whose others keep it (`states::key`:
/// the circuit with its values and edits, the pads' kinds and the plan's options). A part's value
/// or a pad's kind changed is another key, other others, and none of these.
#[derive(Clone, PartialEq, Eq, Hash)]
struct BaseKey(Vec<u64>);

impl BaseKey {
    fn of(t: &NodeTest, dc: &[C64]) -> BaseKey {
        let mut k = vec![t.node as u64, t.diag as u64, t.stiff.len() as u64];
        k.extend(t.stiff.iter().map(|&u| u as u64));
        k.push(t.soft.len() as u64);
        for part in &t.soft {
            k.push(part.len() as u64);
            k.extend(part.iter().map(|&u| u as u64));
        }
        for m in t.moved.iter().map(Vec::as_slice).chain([dc]) {
            k.push(m.len() as u64);
            k.extend(m.iter().flat_map(|z| [z.re.to_bits(), z.im.to_bits()]));
        }
        k.push(t.imports.len() as u64);
        k.extend(t.imports.iter().map(|&u| u as u64));
        BaseKey(k)
    }
}

impl Others {
    /// No other state.
    pub fn none() -> Others {
        Others { list: Vec::new(), first: Vec::new(), len: 0, n: 0, memo: Mutex::new(HashMap::new()), base_tests: Mutex::new(HashMap::new()), base: None, maps: Mutex::new(HashMap::new()), ports: Mutex::new(HashMap::new()), what: String::new(), ms: 0.0, planned: Default::default(), own: None }
    }

    /// `states` linearised at their own times on the points of `o` (`held_states`), on the layout
    /// of `whole`, which each must share, each on its own core of `pool` (`fan_out`). `ms` is what
    /// making them took before. With `fixed`, the first is the base (`states::fixed`). `own` are the
    /// circuit's own switching devices (`Others::own`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(states: Vec<State>, whole: &Solver, o: &PartOpts, what: String, ms: f64, pool: Option<&Helpers>, fixed: bool, own: Vec<String>) -> Result<Others, SimDiag> {
        let start = Instant::now();
        let g = grid(o)?;
        for x in &states {
            let same = x.s.n == whole.n && x.s.g.len() == whole.g.len() && x.s.slot_row == whole.slot_row && x.s.slot_col == whole.slot_col;
            if !same || x.touch.len() != whole.n + 1 {
                return Err(SimDiag::setup(format!("held: the state {} is not on the run's layout", x.what)));
            }
        }
        let (first, states): (Vec<State>, Vec<State>) = states.into_iter().partition(|x| x.first_instant);
        let first = first.into_iter().map(|x| First { seed: Seed::of(&x.s, x.t), touch: x.touch, what: x.what }).collect();
        let base = states.first().filter(|_| fixed).map(|x| (Mutex::new(x.s.clone()), x.t, 0));
        let units: Vec<Unit<Other>> = states.into_iter().map(|x| {
            let g = g.clone();
            Box::new(move || Other { lin: Arc::new(Kept::of(linearise(&x.s, x.t, &g.freqs, g.step, g.dc_alt), &g)), seed: Seed::of(&x.s, x.t), touch: x.touch,
                stability_only: x.stability_only, bound: x.bound, what: x.what }) as Unit<Other>
        }).collect();
        let list = fan_out(units, pool).map_err(|f| f.diag("held: a state's linearisation"))?;
        Ok(Others { list, first, len: whole.g.len(), n: whole.n, memo: Mutex::new(HashMap::new()), base_tests: Mutex::new(HashMap::new()), base, maps: Mutex::new(HashMap::new()), ports: Mutex::new(HashMap::new()), what, ms: ms + start.elapsed().as_secs_f64() * 1e3, planned: Default::default(), own: Some(own) })
    }

    /// The states, as `State::what`.
    pub fn whats(&self) -> Vec<String> {
        self.list.iter().map(|x| x.what.clone()).collect()
    }

    /// What keeping them costs, bytes: their linearisations and seeds.
    pub fn bytes(&self) -> usize {
        self.list.iter().map(|x| x.lin.bytes() + x.seed.bytes()).sum::<usize>() + self.first.iter().map(|x| x.seed.bytes()).sum::<usize>()
    }

    /// With a fixed start, the base: a copy of its solver, and its time (the held step's primary
    /// state).
    pub(crate) fn base(&self) -> Option<(Solver, f64)> {
        self.base.as_ref().map(|b| (b.0.lock().unwrap_or_else(|e| e.into_inner()).clone(), b.1))
    }

    /// The base's seed and what it is, with a fixed start.
    pub(crate) fn base_seed(&self) -> Option<(&Seed, &str)> {
        self.base.as_ref().map(|b| (&self.list[b.2].seed, self.list[b.2].what.as_str()))
    }

    /// The seed of the state at `at` (`Mapped::at`).
    pub(crate) fn seed(&self, at: usize) -> &Seed {
        &self.list[at].seed
    }

    /// The pads' first instants (`State::first_instant`): each one's seed, the unknowns it reaches
    /// and what it is.
    pub(crate) fn first_instants(&self) -> impl Iterator<Item = (&Seed, &[bool], &str)> {
        self.first.iter().map(|x| (&x.seed, x.touch.as_slice(), x.what.as_str()))
    }

    /// The states the round map is judged in besides the base, in order.
    pub(crate) fn mapped(&self) -> impl Iterator<Item = Mapped<'_>> {
        let base = self.base.as_ref().map(|b| b.2);
        self.list.iter().enumerate().filter(move |(i, _)| Some(*i) != base)
            .map(|(at, x)| Mapped { at, touch: &x.touch, stability_only: x.stability_only, what: &x.what })
    }
}

/// The search's work.
#[derive(Clone, Copy, Debug, Default)]
pub struct Searched {
    /// The exact blocks searched, and its passes over them.
    pub blocks: usize,
    pub passes: usize,
    /// Node tests at the present state (a node is tested again only when the parts it reads
    /// change), and those that failed.
    pub tests: usize,
    pub failed: usize,
    /// The other states (`State`), their node tests and those that failed.
    pub states: usize,
    pub state_tests: usize,
    pub state_failed: usize,
    /// With a fixed start, the present state's guard (stability only, after the base and the
    /// other states): its node tests and those that failed, which merge.
    pub guard_tests: usize,
    pub guard_failed: usize,
    /// Of the other states' tests, those taken from before (`Others`).
    pub memo: usize,
    /// Wall time in the tests, ms: all of them, and of that the other states' with their
    /// linearisation; and settling the other states (`round::plan_blocks`).
    pub ms: f64,
    pub state_ms: f64,
    pub settle_ms: f64,
}

/// A state's linearisation: its instances, their totals, the band's values then the step's
/// matrix (`values`, `values_step`), and the DC fallback's.
struct Lin {
    insts: Vec<Inst>,
    gt: Vec<f64>,
    ct: Vec<f64>,
    band: Vec<Vec<C64>>,
    fallback: Vec<C64>,
}

/// A state's linearisation as `Others` keeps it: its instances and their totals, and of the band
/// only what the banks' own AC terms make of a value (`Bank::ac_stamp`), where they change it. The
/// band rebuilt from these (`Kept::band`) is `linearise`'s, bit for bit, at a twentieth of its size.
struct Kept {
    insts: Vec<Inst>,
    gt: Vec<f64>,
    ct: Vec<f64>,
    /// Per band frequency, then at the DC fallback: (value, the banks' value) where they change it.
    ac: Vec<Vec<(u32, C64)>>,
}

impl Kept {
    fn of(lin: Lin, g: &Grid) -> Kept {
        let Lin { insts, gt, ct, band, fallback } = lin;
        let ws = g.freqs.iter().chain([&g.dc_alt]).map(|&f| 2.0 * PI * f);
        let ac = band.iter().take(g.freqs.len()).chain([&fallback]).zip(ws).map(|(vals, w)| {
            vals.iter().enumerate().filter_map(|(v, z)| {
                let plain = C64::new(gt[v], w * ct[v]);
                (plain.re.to_bits() != z.re.to_bits() || plain.im.to_bits() != z.im.to_bits()).then_some((v as u32, *z))
            }).collect()
        }).collect();
        Kept { insts, gt, ct, ac }
    }

    /// The band (`values` at each frequency, then `values_step`) and the DC fallback's values, made
    /// again in `band` and `fallback`.
    fn band_into(&self, g: &Grid, band: &mut Vec<Vec<C64>>, fallback: &mut Vec<C64>) {
        let at = |w: f64, ac: &[(u32, C64)], vals: &mut Vec<C64>| {
            vals.clear();
            vals.extend(self.gt.iter().zip(&self.ct).map(|(&g, &c)| C64::new(g, w * c)));
            for &(v, z) in ac {
                vals[v as usize] = z;
            }
        };
        band.resize_with(g.freqs.len() + 1, Vec::new);
        for ((&f, ac), vals) in g.freqs.iter().zip(&self.ac).zip(band.iter_mut()) {
            at(2.0 * PI * f, ac, vals);
        }
        values_step_into(&self.gt, &self.ct, 2.0 / g.step, &mut band[g.freqs.len()]);
        at(2.0 * PI * g.dc_alt, &self.ac[g.freqs.len()], fallback);
    }

    /// What keeping it costs, bytes.
    fn bytes(&self) -> usize {
        8 * (self.gt.len() + self.ct.len()) + std::mem::size_of::<(u32, C64)>() * self.ac.iter().map(Vec::len).sum::<usize>()
            + self.insts.iter().map(|i| 40 * i.slots.len() + 8 * i.unks.len() + i.name.len()).sum::<usize>()
    }
}

/// Bound on the error per unit swing when a read is up to `lag` seconds late.
/// The points sample the band and a round can end early, so the band weight covers every
/// frequency up to `|f|` and every delay up to `lag`. It equals `|1 - e^(-s lag)|` below
/// the first peak and stays at two above it. The step's real point keeps `|1 - e^(-2 lag / step)|`.
pub fn weight(s: C64, lag: f64) -> f64 {
    if s.re == 0.0 {
        let f = (s.im / (2.0 * PI)).abs();
        2.0 * (PI * (f * lag).min(0.5)).sin()
    } else {
        (C64::new(1.0, 0.0) - (s * C64::new(-lag, 0.0)).exp()).norm()
    }
}

/// At or above `fmax` every point counts, including an octave point rounded past the grid's end.
pub(super) fn accuracy_band(band: f64, fmax: f64) -> f64 {
    if band >= fmax { f64::INFINITY } else { band }
}

/// Whether the accuracy bars judge this point. Compare on the imaginary axis in radians so the
/// grid point made from the band's top is included exactly.
pub(super) fn in_band(s: C64, band: f64) -> bool {
    if s.re == 0.0 { s.im.abs() <= 2.0 * PI * band } else { s.re / (2.0 * PI) <= band }
}

fn test_point(freqs: &[f64], step: f64, k: usize) -> C64 {
    if k == freqs.len() { C64::new(2.0 / step, 0.0) } else { C64::new(0.0, 2.0 * PI * freqs[k]) }
}

/// Accuracy weights for a delay up to `lag` seconds, zero above `band`. The step's matrix comes last.
pub fn weights_at(freqs: &[f64], step: f64, lag: f64, band: f64) -> Vec<f64> {
    (0..=freqs.len()).map(|k| {
        let s = test_point(freqs, step, k);
        if in_band(s, band) { weight(s, lag) } else { 0.0 }
    }).collect()
}

/// A loop of gain `rho` has late-read error at most `rho w / (1 - rho)` (`recirculated`).
/// `w` bounds the error per unit swing at this point (`weight`).
/// Inside the loop that error goes round again. The tests and the round map give magnitudes, so
/// the loop's phase is unknown, and `1 / (1 - rho)` is the most the error can gather on its way
/// round. Nothing at DC: a late loop settles where the loop does.
pub fn late_wrong(rho: f64, w: f64) -> f64 {
    recirculated(rho * w, rho)
}

/// A late read's own error `own` (the loop gain times the weight)
/// gathered round a loop of gain `rho` whose phase is unknown: `own / (1 - rho)`. The held tests'
/// one value (`late_wrong`) and the round map's many, each at its own lateness, take it alike.
pub fn recirculated(own: f64, rho: f64) -> f64 {
    if own == 0.0 {
        0.0
    } else if rho < 1.0 {
        own / (1.0 - rho)
    } else {
        f64::INFINITY
    }
}

/// The bar a loop gain fails (`bars`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Stability,
    Accuracy,
}

/// For a loop gain `rho` at one point, take the worst late-read error (`late_wrong`) across the
/// weights and report which bar failed (`bar_failed`). The held tests, the plan's late cuts and
/// the re-weigh use this function. The round map calls `bar_failed` on its own figure.
pub fn bars(rho: f64, weights: &[f64], bar: Option<f64>) -> (f64, Option<Bar>) {
    let wrong = weights.iter().map(|&w| late_wrong(rho, w)).fold(0.0, f64::max);
    (wrong, bar_failed(rho, wrong, bar))
}

/// The bar a loop of gain `rho` that gets `wrong` read late fails. Stability: `rho` under
/// `STABILITY`. Accuracy, where `bar` is given: `wrong` under `bar`.
pub fn bar_failed(rho: f64, wrong: f64, bar: Option<f64>) -> Option<Bar> {
    if !(rho < STABILITY) {
        Some(Bar::Stability)
    } else if bar.is_some_and(|b| !(wrong < b)) {
        Some(Bar::Accuracy)
    } else {
        None
    }
}

/// `bars` over a test's points, `rho` and `weights` point by point.
#[derive(Clone, Copy, Debug)]
pub struct Judged {
    /// The largest rho and its point.
    pub rho: (usize, f64),
    /// The largest of what the late read gets wrong, and its point.
    pub wrong: (usize, f64),
    /// The bar failed and its point (that bar's peak), stability before accuracy.
    pub fails: Option<(Bar, usize)>,
}

pub fn judge(rho: &[f64], weights: &[f64], bar: Option<f64>) -> Judged {
    let mut j = Judged { rho: (0, 0.0), wrong: (0, 0.0), fails: None };
    let (mut stab, mut acc) = (false, false);
    for (k, (&r, &w)) in rho.iter().zip(weights).enumerate() {
        let (wrong, fails) = bars(r, &[w], bar);
        if r > j.rho.1 || k == 0 {
            j.rho = (k, r);
        }
        if wrong > j.wrong.1 || k == 0 {
            j.wrong = (k, wrong);
        }
        stab |= fails == Some(Bar::Stability);
        acc |= fails == Some(Bar::Accuracy);
    }
    j.fails = if stab { Some((Bar::Stability, j.rho.0)) } else if acc { Some((Bar::Accuracy, j.wrong.0)) } else { None };
    j
}

/// Forward error in either direction per volt of swing on the other side. At point `k`,
/// `w_k` bounds the error per unit swing for delays up to one round (`weight`).
/// With the holder first, its load is read late. Fully active consumers give `max drive_k w_k`
/// (`Measured::drives`). The reflected part `tau` falls where a series capacitor passes the far end.
/// With consumers first, the node is read late. It moves by `own` of the holder's activity and
/// `rho` of the consumers', giving `max (gain_k own_k + rho_k) w_k`. When the holder moves the
/// node itself (`moving`), `own` is one.
fn forward(m: &Measured, weights: &[f64], moving: bool) -> (f64, f64) {
    let mut out = (0.0f64, 0.0f64);
    for (k, &w) in weights.iter().enumerate().take(m.rho.len()) {
        let own = if moving { 1.0 } else { m.owns[k] };
        out.0 = out.0.max(m.drives[k] * w);
        out.1 = out.1.max((m.gains[k] * own + m.rho[k]) * w);
    }
    out
}

/// A cut's direction (A-S6): consumers first where today's figures say so (`tau` over `gain`: the
/// holder first reads its load late at a larger response than the consumers reading the node), and
/// where the holder first's forward bound is over both the cut's bar and the consumers first's
/// (`forward`). A cut in the hub form keeps `tau`'s verdict: its consumers read in the same round
/// whichever it is.
fn holder_first(tau: f64, gain: f64, (holder, consumers): (f64, f64), bar: f64, hub: bool) -> bool {
    tau <= gain && (hub || holder <= bar || holder <= consumers)
}

fn linearise(s: &Solver, t: f64, freqs: &[f64], step: f64, dc_alt: f64) -> Lin {
    let insts = instances(s, t);
    let len = s.g.len();
    let (mut gt, mut ct) = (vec![0.0; len], vec![0.0; len]);
    for d in &insts {
        for &(v, _, _, g, c) in &d.slots {
            gt[v] += g;
            ct[v] += c;
        }
    }
    let mut band: Vec<Vec<C64>> = freqs.iter().map(|&f| values(s, &gt, &ct, f)).collect();
    band.push(values_step(&gt, &ct, 2.0 / step));
    let fallback = values(s, &gt, &ct, dc_alt);
    Lin { insts, gt, ct, band, fallback }
}

/// An instance anchors at a node by its share alone only where the share is overwhelming: half
/// or more of what holds the node. A share is not a bound on a cut's rho (a capacitor whose far
/// end follows its near end carries little and still cannot be read late), so below this it only
/// orders the candidates: the node's largest anchors there, and an instance weak everywhere
/// anchors where its share is largest (at an ordinary node before one in the hub form). Every
/// other attachment gets its test.
const ANCHOR_SHARE: f64 = 0.5;

/// One instance's stamp at the plan's state.
struct Inst {
    name: String,
    kind: &'static str,
    /// Its matrix entries: (value index, row, column, g, c).
    slots: Vec<(usize, usize, usize, f64, f64)>,
    /// The unknowns it reads or writes, ground left out.
    unks: Vec<usize>,
    /// A resistor or a capacitor: a cut through it keeps a ghost on the holder's side.
    rc: bool,
    /// It adds terms `G + jwC` cannot say (`Bank::ac_stamp`): strong at every node.
    wide: bool,
    /// It raises events: a corner, a crossing or a pin write (the proxy).
    events: bool,
    /// A GPIO pad's driver.
    pad: bool,
    /// Its stamps follow time (`Tier::Timed`): a source with a waveform or a pending switch.
    timed: bool,
}

/// Every instance evaluated alone at `s.x` and time `t` (limiting and bypass off) on a copy of
/// the banks, so the solver's own state and memories are untouched.
fn instances(s: &Solver, t: f64) -> Vec<Inst> {
    let cx = EvalCx { t, tran: true, init_junctions: false, limit: false, edge_hold: false, bypass: false, settle: false, ..s.base_cx() };
    let len = s.g.len();
    let mut banks = s.banks.clone();
    let (mut g, mut c) = (vec![0.0; len], vec![0.0; len]);
    let (mut f, mut q) = (vec![0.0; s.n + 1], vec![0.0; s.n + 1]);
    let (mut sl, mut un) = (Vec::new(), Vec::new());
    let mut out = Vec::new();
    for (bi, b) in banks.iter_mut().enumerate() {
        let mut ac = vec![C64::ZERO_C; len];
        s.banks[bi].ac_stamp(2.0 * PI * 1e3, &mut ac);
        let kind = b.kind();
        let breaks = b.next_break(t).is_finite();
        let reads_time = b.varies_after(t);
        for i in 0..b.len() {
            let mut st = Stamps { g: &mut g, c: &mut c, f: &mut f, q: &mut q, noncon: 0, noncon_at: 0 };
            b.eval_some(&s.x, &cx, &mut st, &[i as u32]);
            sl.clear();
            b.inst_slots(i, &mut sl);
            sl.sort_unstable();
            sl.dedup();
            let mut slots = Vec::with_capacity(sl.len());
            let mut wide = false;
            for &v in &sl {
                let v = v as usize;
                if v == 0 || s.slot_row[v] == 0 || s.slot_col[v] == 0 { continue; }
                slots.push((v, s.slot_row[v] as usize, s.slot_col[v] as usize, g[v], c[v]));
                wide |= ac[v] != C64::ZERO_C;
            }
            for &v in &sl {
                g[v as usize] = 0.0;
                c[v as usize] = 0.0;
            }
            g[0] = 0.0;
            c[0] = 0.0;
            f.fill(0.0);
            q.fill(0.0);
            un.clear();
            b.inst_unks(i, &mut un);
            let mut unks: Vec<usize> = un.iter().map(|&u| u as usize).filter(|&u| u != 0).collect();
            unks.sort_unstable();
            unks.dedup();
            let events = match kind {
                "GPIO" | "S" => true,
                "b" => reads_time,
                _ => breaks && b.tier(i) != Tier::Fixed,
            };
            out.push(Inst { name: b.inst_name(i).to_string(), kind, slots, unks, rc: matches!(kind, "R" | "C"), wide, events, pad: kind == "GPIO", timed: b.tier(i) == Tier::Timed });
        }
    }
    out
}

/// `G + jwC` from the instances' totals, with the banks' own AC terms.
fn values(s: &Solver, gt: &[f64], ct: &[f64], f: f64) -> Vec<C64> {
    let w = 2.0 * PI * f;
    let mut vals: Vec<C64> = gt.iter().zip(ct).map(|(&g, &c)| C64::new(g, w * c)).collect();
    for b in &s.banks {
        b.ac_stamp(w, &mut vals);
    }
    vals
}

/// The matrix a step of `h` solves, `G + (2 / h) C` (the trapezoid), as `values` gives the band:
/// `two_h` is `2 / h`. The banks' own AC terms have no such form and are left out.
fn values_step(gt: &[f64], ct: &[f64], two_h: f64) -> Vec<C64> {
    let mut vals = Vec::new();
    values_step_into(gt, ct, two_h, &mut vals);
    vals
}

/// `values_step` made again in `vals`.
fn values_step_into(gt: &[f64], ct: &[f64], two_h: f64, vals: &mut Vec<C64>) {
    vals.clear();
    vals.extend(gt.iter().zip(ct).map(|(&g, &c)| C64::new(g + two_h * c, 0.0)));
}

/// `g + s c` at a point `s` (`jw`, or `2 / h` for a step's matrix).
fn at_point(g: f64, c: f64, sp: C64) -> C64 {
    C64::new(g + sp.re * c, sp.im * c)
}

/// What the small-island rule reads of one island.
#[derive(Default)]
struct Facts {
    size: usize,
    control: bool,
    /// How many islands read it in the same round, and the one when there is one.
    readers: usize,
    reader: Option<usize>,
}

/// The small-island rule: each island under `min` unknowns with a single reader, and that reader.
/// The control island joins nothing.
fn joins(facts: &[Facts], min: usize) -> Vec<(usize, usize)> {
    facts.iter().enumerate().filter(|(_, f)| f.size < min && !f.control).filter_map(|(i, f)| f.reader.map(|r| (i, r))).collect()
}

/// The held step's result.
#[derive(Clone, Debug)]
pub struct Held {
    pub names: Vec<String>,
    pub n: usize,
    pub t: f64,
    pub islands: Vec<HeldIsland>,
    /// Every held node: an attachment across islands remains.
    pub nodes: Vec<HeldNode>,
    pub merges: Vec<String>,
    /// Per voltage node on its own row in a searched block: its attachments (instance, share),
    /// largest first.
    pub shares: Vec<(u32, Vec<(String, f64)>)>,
    /// The largest `|sum of a node's complex shares - 1|` over the coarse grid (nodes with an
    /// instance whose AC terms are not `G + jwC` left out): a check on the stamps.
    pub share_err: f64,
    /// Per bank kind: instances, and how many of them the proxy says raise events.
    pub kinds: Vec<(&'static str, usize, usize)>,
    pub coarse: usize,
    /// The band's frequencies; a test's points are these and then the step's matrix.
    pub freqs: Vec<f64>,
    /// The configured tops of the accuracy band and the test grid, Hz.
    pub band: f64,
    pub fmax: f64,
    /// The step whose matrix the tests also take, `0.5 / fmax`.
    pub step: f64,
    /// Tested nodes whose band and step's matrix disagree on the accuracy bar: (node, the band's
    /// largest accuracy figure a round late, the step's matrix's, the bar), and how many nodes
    /// were tested.
    pub disagree: Vec<(String, f64, f64, f64)>,
    pub tested: usize,
    pub dc_at: Option<f64>,
    pub cut_tol: f64,
    pub hub_tol: f64,
    pub min_island: usize,
    /// The island that is block 0, when there is one.
    pub zero: Option<usize>,
    /// The declared supplies among the solver's nodes, and the ordinary nodes stiffer than
    /// `STIFF_OHMS` (with their driving-point impedance).
    pub supplies: Vec<String>,
    pub stiff: Vec<(String, f64)>,
    /// Declared supplies whose consumers failed the total: (node, why), one per merge.
    pub failed_supplies: Vec<(String, String)>,
    /// What the rules promise, checked: the held nodes in the hub form, and cuts through an
    /// instance that is not a resistor or a capacitor at an ordinary node (none).
    pub hub_nodes: Vec<String>,
    pub device_outside: Vec<String>,
    /// Wall time: the instances and the band, the shares, the search and the rest; ms.
    pub ms: [f64; 3],
    pub searched: Searched,
    /// The round (`PartOpts::round`), seconds.
    pub round: f64,
    /// The other states tested, as `State::what`.
    pub states: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct HeldIsland {
    pub unknowns: Vec<u32>,
    pub stage: u32,
    pub control: bool,
    /// Holds a declared supply.
    pub declared: bool,
    /// One of its instances raises an event (the proxy).
    pub events: bool,
    /// Block 0.
    pub zero: bool,
}

#[derive(Clone, Debug)]
pub struct HeldNode {
    pub node: u32,
    pub holder: usize,
    /// The holder is block 0.
    pub zero: bool,
    /// The hub form: a pad or a declared supply.
    pub hub: bool,
    pub bar: f64,
    /// Per point: the band, then the step's matrix; at the present state.
    pub rho: Vec<f64>,
    pub tau: f64,
    pub stiff_first: bool,
    pub consumers: Vec<Consumer>,
    /// Its stability gain: the largest rho at the present state and every other state tested, and
    /// where (the point, and the state when another).
    pub stability: (f64, String),
    /// Per point, the largest rho in the states where the accuracy bar holds: what a plan judges at
    /// the lateness its stages give the cut's late value (`Held::weights`, `judge`).
    pub acc_rho: Vec<f64>,
    /// What `acc_rho` gets wrong a round late (`late_wrong`), at its largest, and where.
    pub accuracy: (f64, String),
    /// The bound over the circuit's own device states (`State::bound`): what it gets wrong a round
    /// late, over the bar, at its largest, and where.
    pub bound: Option<(f64, String)>,
}

#[derive(Clone, Debug)]
pub struct Consumer {
    pub island: usize,
    /// Its instances weak at the node.
    pub insts: Vec<String>,
    /// Every one of them a resistor or a capacitor.
    pub rc: bool,
    /// Its own share of the node's coupling, the band's maximum.
    pub rho: f64,
    /// The largest voltage a volt of its swing makes anywhere in the block through the node,
    /// `max |Z_kh| |Y'_c|` over the band, and where: the point and the node `k`.
    pub reach: f64,
    pub reach_at: String,
}

/// A held node's passed test, in one block's unknowns.
struct Tested {
    rho: Vec<f64>,
    tau: f64,
    first: bool,
    /// (consumer root, its share of rho).
    shares: Vec<(usize, f64)>,
    /// Per consumer, as `shares`: its reach and where it peaks.
    reach: Vec<(f64, String)>,
    stability: (f64, String),
    acc_rho: Vec<f64>,
    accuracy: (f64, String),
    /// At the present state: per point the stiff side's `|Z_S,ss|`, and per consumer (as
    /// `shares`) per point its `|Y'_j|` (`Measured`).
    zs: Vec<f64>,
    ys: Vec<Vec<f64>>,
    /// The bound over the circuit's own device states (`State::bound`): what it gets wrong a
    /// round late over the bar, and where.
    bound: Option<(f64, String)>,
    /// At the present state: `gain`.
    gain: f64,
    /// Over it and the key's states the accuracy bar holds in that reach the cut, each at its
    /// largest: `tau` and `forward`'s two bounds (the direction's figures, `holder_first`).
    over: (f64, f64, f64),
    /// The holder's instances at the node's part that move by themselves (`forward`'s `moving`).
    moving: Vec<String>,
}

/// A held node's failed test in a pass, before the pass decides whether its merge goes in.
struct Failure {
    node: usize,
    holder: usize,
    /// The consumer root that joins the holder.
    joins: usize,
    /// Every consumer root it was tested against.
    soft: Vec<usize>,
    /// The report's line: why it failed, and what joins.
    why: String,
    joined: String,
    /// A declared supply's failure, for the report.
    supply: Option<(String, String)>,
}

/// One exact block's search state.
struct BlockSearch {
    n: usize,
    /// Per instance in the block: its attachments (node, share).
    atts: BTreeMap<usize, Vec<(usize, f64)>>,
    /// Per instance: the unknowns of the block it always joins.
    fixed: BTreeMap<usize, Vec<usize>>,
    /// Per instance: the block's voltage nodes it reads without loading them.
    reads: BTreeMap<usize, Vec<usize>>,
    /// Instances writing the block with no matrix entries in it (an independent current
    /// source): their voltage nodes in the block. Each is at home at its softest node and weak at
    /// the others, where what it injects is a consumer's return.
    loose: BTreeMap<usize, Vec<usize>>,
    /// Each node's driving-point impedance in the block, the coarse grid's maximum.
    zdp: BTreeMap<usize, f64>,
    /// Per node, per point (the coarse grid, then the step's matrix): the largest `|Z_kh|` over
    /// the block's voltages, and that `k`.
    zmax: BTreeMap<usize, Vec<(f64, usize)>>,
    largest: BTreeMap<usize, usize>,
    pads: Vec<usize>,
}

struct Anchored {
    sets: Sets,
    /// Per instance: an unknown of its home island.
    home: BTreeMap<usize, usize>,
    /// (node, instance) weak attachments across islands.
    weak: Vec<(usize, usize)>,
}

impl BlockSearch {
    /// The islands at the bars `bar`, with `hub` the nodes in the hub form.
    fn anchor(&self, insts: &[Inst], hub: &[bool], forced: &[(usize, usize)]) -> Anchored {
        let mut sets = Sets((0..=self.n).collect());
        let mut home = BTreeMap::new();
        let mut weak_all = Vec::new();
        for (&d, atts) in &self.atts {
            let mut join = self.fixed.get(&d).cloned().unwrap_or_default();
            let mut strong = Vec::new();
            for &(h, share) in atts {
                let i = &insts[d];
                if i.wide || share >= ANCHOR_SHARE || self.largest.get(&h) == Some(&d) || (i.pad && self.pads.contains(&h)) || (!i.rc && !hub[h]) {
                    strong.push(h);
                }
            }
            if strong.is_empty() {
                // weak everywhere: at an ordinary node before one in the hub form (whose consumers
                // return what they draw a round late), and there where its share is largest
                let mut best = atts[0];
                for &a in &atts[1..] {
                    if (!hub[a.0], a.1) > (!hub[best.0], best.1) { best = a; }
                }
                strong.push(best.0);
            }
            join.extend_from_slice(&strong);
            for w in join.windows(2) {
                sets.union(w[0], w[1]);
            }
            home.insert(d, join[0]);
            weak_all.extend(atts.iter().filter(|a| !strong.contains(&a.0)).map(|a| (a.0, d)));
        }
        for (&d, fixed) in &self.fixed {
            if self.atts.contains_key(&d) { continue; }
            for w in fixed.windows(2) {
                sets.union(w[0], w[1]);
            }
            home.insert(d, fixed[0]);
        }
        for (&d, nodes) in &self.loose {
            let z = |u: usize| self.zdp.get(&u).copied().unwrap_or(0.0);
            let Some(&at) = nodes.iter().fold(None, |best: Option<&usize>, u| if best.is_none_or(|b| z(*u) > z(*b)) { Some(u) } else { best }) else { continue };
            home.insert(d, at);
            weak_all.extend(nodes.iter().filter(|&&u| u != at).map(|&u| (u, d)));
        }
        for w in self.pads.windows(2) {
            sets.union(w[0], w[1]);
        }
        for &(a, b) in forced {
            sets.union(a, b);
        }
        let weak = weak_all.into_iter().filter(|&(h, d)| sets.find(h) != sets.find(home[&d])).collect();
        Anchored { sets, home, weak }
    }

    /// The same-round reads between the islands, by root: `from -> to` when `to` reads `from` in
    /// the same round. Every exact read; a consumer reads its holder, except at an ordinary cut
    /// through resistors and capacitors whose consumers go first, where the holder reads them.
    fn same_round(&self, insts: &[Inst], a: &Anchored, hub: &[bool], first: &BTreeMap<usize, bool>) -> Vec<Vec<usize>> {
        let mut graph = vec![Vec::new(); self.n + 1];
        for (&d, reads) in &self.reads {
            let Some(&home) = a.home.get(&d) else { continue };
            let to = a.sets.find(home);
            for &u in reads {
                let from = a.sets.find(u);
                if from != to { graph[from].push(to); }
            }
        }
        for &(h, d) in &a.weak {
            let (holder, consumer) = (a.sets.find(h), a.sets.find(a.home[&d]));
            if hub[h] || !insts[d].rc || *first.get(&h).unwrap_or(&true) { graph[holder].push(consumer); } else { graph[consumer].push(holder); }
        }
        sort_graph(&mut graph);
        graph
    }
}

/// What every node test of a search reads besides its state's band: the present solver's rows,
/// matrix entries and voltage unknowns, shared with the units on the pool's cores.
struct Common {
    row_of: Vec<usize>,
    entries: Vec<Entry>,
    voltage: Vec<bool>,
}

/// The state `xi`'s node tests on its band (`measure_each`), one singular at DC measured again at
/// the DC fallback with its consumers' own terms there (`dc_moved`, per test per consumer
/// island): a unit of `fan_out`. Each result with whether it fell back. The band is made in the
/// thread's scratch (`partition::Scratch`), as the tests are measured.
fn measure_state(lin: &Kept, g: &Grid, mut tests: Vec<NodeTest>, dc_moved: Vec<Vec<C64>>, c: &Common) -> Vec<(Result<Measured, usize>, bool)> {
    with_scratch(|s| {
        let (mut band, mut fallback) = (std::mem::take(&mut s.band), std::mem::take(&mut s.fallback));
        lin.band_into(g, &mut band, &mut fallback);
        let measured = measure_each_in(s, &tests, &c.row_of, &c.entries, &c.voltage, &band);
        let out = tests.iter_mut().zip(dc_moved).zip(measured).map(|((test, dc), mut result)| {
            let fell = matches!(result, Err(0));
            if fell {
                for (m, z) in test.moved.iter_mut().zip(dc) {
                    m[0] = z;
                }
                result = measure(test.node, &test.stiff, &test.soft, &c.row_of, &c.entries, &c.voltage, &band, Some(&fallback), &test.moved, test.diag, &test.imports);
            }
            (result, fell)
        }).collect();
        (s.band, s.fallback) = (band, fallback);
        out
    })
}

/// A state a pass of `held_states` tests its cuts in after the primary one: its place in the
/// others (past them for the present state), its linearisation and reach, its bars, what it is,
/// and whether its tests are kept (`Others::memo`).
struct View<'a> {
    at: usize,
    lin: Arc<Kept>,
    touch: &'a [bool],
    stability_only: bool,
    bound: bool,
    what: &'a str,
    kept: bool,
}

/// The held step at the state `s` holds (its `x` and device states) at time `t`, at that state
/// alone (`held_states` with no other state).
pub fn held(s: &Solver, t: f64, o: &PartOpts) -> Result<Held, SimDiag> {
    held_states(s, &Arc::new(Others::none()), t, o, None, None, &[])
}

/// The points the held step measures on, from its options: the shares' coarse grid, the tests'
/// band, the step's matrix (`step`) and the DC fallback.
#[derive(Clone)]
struct Grid {
    coarse: Vec<f64>,
    freqs: Vec<f64>,
    band: f64,
    step: f64,
    dc_alt: f64,
}

fn grid(o: &PartOpts) -> Result<Grid, SimDiag> {
    let ok = |v: f64| v.is_finite() && v > 0.0;
    if !ok(o.cut_tol) || !ok(o.hub_tol) || !ok(o.fmin) || !o.fmax.is_finite() || o.fmax < o.fmin || o.band.is_nan() || o.band < o.fmin || o.per_octave == 0 || o.min_island == 0 || o.fmin / 100.0 == 0.0 || !ok(o.round) {
        return Err(SimDiag::setup("held: need positive finite cut_tol, hub_tol, fmin, fmax >= fmin, band >= fmin (infinity allowed), per_octave, min_island and round".into()));
    }
    let mut coarse = vec![0.0];
    coarse.extend(AcSweep::Oct { n: 1, fstart: o.fmin, fstop: o.fmax }.freqs());
    let mut freqs = vec![0.0];
    freqs.extend(AcSweep::Oct { n: o.per_octave, fstart: o.fmin, fstop: o.fmax }.freqs());
    // 1, 5 and 20 kHz, and the band's top itself: a loop's gain can rise all the way to `fmax`
    freqs.extend([1e3, 5e3, 2e4, o.fmax].into_iter().filter(|&f| f <= o.fmax));
    if o.band < o.fmax {
        freqs.push(o.band);
    }
    freqs.sort_by(f64::total_cmp);
    freqs.dedup();
    // the tests' points: the band, then the matrix a step at the band's top solves
    Ok(Grid { coarse, freqs, band: o.band, step: 0.5 / o.fmax, dc_alt: o.fmin / 100.0 })
}

/// The held search at the state of `s`, its cuts tested in each of the `others` too (the
/// module's States), their tests measured on `pool`'s cores beside the caller (`fan_out`). With
/// `present` (the run's state and time), `s` is the others' base (`Others::base`), whose place in
/// them it takes, and the present state only guards: after the others it tests every cut on the
/// stability bar alone, its tests never kept, and a failure there merges as any does, counted
/// (`Searched::guard_failed`). A node marked in `kept` (a cut in force, `round::plan_blocks_kept`)
/// has its accuracy bar `round::KEEP` times its own.
#[allow(clippy::too_many_arguments)]
pub fn held_states(s: &Solver, others: &Arc<Others>, t: f64, o: &PartOpts, pool: Option<&Helpers>, present: Option<(&Solver, f64)>, kept: &[bool]) -> Result<Held, SimDiag> {
    let g = Arc::new(grid(o)?);
    let Grid { coarse, freqs, step, dc_alt, .. } = (*g).clone();
    let start = Instant::now();
    let n = s.n;
    let len = s.g.len();
    if !others.list.is_empty() && (others.len != len || others.n != n) {
        return Err(SimDiag::setup("held: the other states are not on the run's layout".into()));
    }
    let nf = freqs.len();
    let Lin { insts, gt, ct, band, fallback } = linearise(s, t, &freqs, step, dc_alt);
    // the states a pass tests its cuts in after this one: the others (the base not again where it
    // is this one), then the present state as a guard
    let all = vec![true; n + 1];
    let base_at = present.and(others.base.as_ref()).map(|b| b.2);
    let mut views: Vec<View> = others.list.iter().enumerate().filter(|&(i, _)| Some(i) != base_at)
        .map(|(at, x)| View { at, lin: x.lin.clone(), touch: &x.touch, stability_only: x.stability_only, bound: x.bound, what: &x.what, kept: true }).collect();
    if let Some((p, pt)) = present {
        views.push(View { at: others.list.len(), lin: Arc::new(Kept::of(linearise(p, pt, &freqs, step, dc_alt), &g)), touch: &all, stability_only: true, bound: false,
            what: "the present state (a guard)", kept: false });
    }
    let mut used = vec![false; len];
    for d in &insts {
        for &(v, _, _, _, _) in &d.slots {
            used[v] = true;
        }
    }
    for u in 1..=n {
        if s.unknowns[u].kind == UnkKind::Voltage { used[s.diag[u] as usize] = true; }
    }
    let entries: Vec<Entry> = (1..len).filter(|&v| used[v]).filter_map(|value| {
        let (row, col) = (s.slot_row[value] as usize, s.slot_col[value] as usize);
        (row != 0 && col != 0).then_some(Entry { row, col, value })
    }).collect();
    let control = control_set(s, &o.control)?;
    // the declared supplies the solver has as voltage nodes (ground and merged nets have none)
    let mut declared = vec![false; n + 1];
    for name in &o.supplies {
        if let Some(u) = s.unknown(name).filter(|&u| s.unknowns[u as usize].kind == UnkKind::Voltage) {
            declared[u as usize] = true;
        }
    }
    let hub: Vec<bool> = (0..=n).map(|u| control[u] || declared[u]).collect();
    // the nodes a run refused (`PartOpts::never_cut`): each fails its tests in every state
    let mut never = vec![false; n + 1];
    for name in &o.never_cut {
        if let Some(u) = s.unknown(name) {
            never[u as usize] = true;
        }
    }
    let Structure { names, row_of, exact, block_of, .. } = structure(s, &entries)?;
    let common = Arc::new(Common { row_of: row_of.clone(), entries: entries.clone(), voltage: s.unknowns.iter().map(|u| u.kind == UnkKind::Voltage).collect() });
    let mut col_at = vec![0; n + 1];
    for u in 1..=n { col_at[row_of[u]] = u; }
    let own = |u: usize| s.unknowns[u].kind == UnkKind::Voltage && row_of[u] == u;
    // the nodes a fixed source holds (its row is the node's): constant between edits, so no import
    // that moves a node (`NodeTest::imports`)
    let mut pinned = vec![false; n + 1];
    for i in insts.iter().filter(|i| matches!(i.kind, "V" | "ctl") && !i.timed) {
        for &(_, r, c, _, _) in &i.slots {
            if row_of[c] == r && s.unknowns[c].kind == UnkKind::Voltage {
                pinned[c] = true;
            }
        }
    }
    // an instance that moves by itself (`forward`'s `moving`): one of the circuit's own switches, a
    // behavioural source that reads time, a source with a waveform, a pad's or a cell's driver, a
    // boundary's feed; not a switch a person sets (an edit, re-proved at its round)
    let moves = |i: &Inst| match i.kind {
        "S" => others.own.as_ref().is_none_or(|own| own.contains(&i.name)),
        "b" => i.events,
        "V" | "I" => i.timed,
        "GPIO" | "cell" | "boundary" => true,
        _ => false,
    };
    // the weight of a value read a round late, per point (`bars`)
    let weights = weights_at(&freqs, step, o.round, accuracy_band(g.band, o.fmax));
    let mut dc_at = None;
    let lin_ms = start.elapsed().as_secs_f64() * 1e3;
    // the other states come linearised, with the unknowns each reaches (`Others`)
    let mut state_ms = 0.0;

    // instances by the blocks whose rows they load (what they only read of another block is an
    // exact edge between blocks, taken after the search)
    let mut by_block: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (d, i) in insts.iter().enumerate() {
        for &(_, r, _, _, _) in &i.slots {
            let u = col_at[r];
            if block_of[u] != usize::MAX { by_block.entry(block_of[u]).or_default().insert(d); }
        }
        if i.slots.is_empty() {
            for &u in &i.unks {
                if block_of[u] != usize::MAX { by_block.entry(block_of[u]).or_default().insert(d); }
            }
        }
    }
    let mut share_ms = 0.0;
    let mut searched = Searched { states: views.iter().filter(|v| v.kept).count(), ..Searched::default() };
    let label = |k: usize| if k == 0 { "DC".to_string() } else if k == nf { "the step's matrix".to_string() } else { hz(freqs[k]) };
    let mut share_err = 0.0f64;
    let mut shares_out = Vec::new();
    let mut merges = Vec::new();
    let mut global = Sets((0..=n).collect());
    // per searched block: the held nodes' tests and attachments, in unknowns
    let mut tests: BTreeMap<usize, Tested> = BTreeMap::new();
    let mut weak_all: Vec<(usize, usize)> = Vec::new();
    let mut home_all: BTreeMap<usize, usize> = BTreeMap::new();
    let bars: Vec<f64> = hub.iter().enumerate().map(|(u, &h)| (if h { o.hub_tol } else { o.cut_tol }) * (if kept.get(u) == Some(&true) { KEEP } else { 1.0 })).collect();
    let mut stiff = Vec::new();
    let mut failed_supplies = Vec::new();
    let mut bandstep: BTreeMap<usize, (f64, f64, f64)> = BTreeMap::new();
    for (b, block) in exact.iter().enumerate() {
        if block.len() <= o.min_island {
            // one island
            for &u in block {
                global.union(block[0], u);
            }
            continue;
        }
        let in_block = |u: usize| block_of[u] == b;
        let shares_start = Instant::now();
        let mut search = BlockSearch { n, atts: BTreeMap::new(), fixed: BTreeMap::new(), reads: BTreeMap::new(),
            loose: BTreeMap::new(), zdp: BTreeMap::new(), zmax: BTreeMap::new(), largest: BTreeMap::new(), pads: block.iter().copied().filter(|&u| control[u]).collect() };
        // per node: (instance, its terms in the node's row: (local column, g, c))
        let mut rows: BTreeMap<usize, Vec<(usize, Vec<(usize, f64, f64)>)>> = BTreeMap::new();
        let local = |u: usize| block.binary_search(&u).ok();
        for &d in by_block.get(&b).into_iter().flatten() {
            let i = &insts[d];
            let mut loads = BTreeSet::new();
            let mut fixed = BTreeSet::new();
            let mut reads = BTreeSet::new();
            for &(_, r, c, g, cap) in &i.slots {
                let ru = col_at[r];
                if in_block(ru) {
                    if own(ru) {
                        loads.insert(ru);
                        if let Some(l) = local(c) {
                            let list = rows.entry(ru).or_default();
                            match list.iter_mut().find(|e| e.0 == d) {
                                Some(e) => e.1.push((l, g, cap)),
                                None => list.push((d, vec![(l, g, cap)])),
                            }
                        }
                    } else {
                        fixed.insert(ru);
                    }
                }
                if in_block(c) {
                    if own(c) { reads.insert(c); } else { fixed.insert(c); }
                }
            }
            for &u in &i.unks {
                if in_block(u) && !own(u) { fixed.insert(u); }
            }
            let reads: Vec<usize> = reads.difference(&loads).copied().collect();
            if !fixed.is_empty() { search.fixed.insert(d, fixed.into_iter().collect()); }
            if !reads.is_empty() { search.reads.insert(d, reads); }
            if i.slots.is_empty() {
                let nodes: Vec<usize> = i.unks.iter().copied().filter(|&u| in_block(u) && own(u)).collect();
                if !nodes.is_empty() { search.loose.insert(d, nodes); }
            }
        }
        // shares on the coarse grid; the largest response to a current at each node (the reach's
        // `Z_kh`) on the coarse grid and the step's matrix
        let mut matrix = LocalMatrix::new(block, &row_of, &entries);
        let mut best: BTreeMap<usize, Vec<f64>> = rows.iter().map(|(&h, list)| (h, vec![0.0; list.len()])).collect();
        let wide: BTreeSet<usize> = rows.iter().filter(|(_, list)| list.iter().any(|e| insts[e.0].wide)).map(|(&h, _)| h).collect();
        let mut z = vec![C64::ZERO_C; block.len()];
        for p in 0..=coarse.len() {
            let step_point = p == coarse.len();
            let mut at = if step_point { 0.0 } else { coarse[p] };
            let mut vals = if step_point { values_step(&gt, &ct, 2.0 / step) } else { values(s, &gt, &ct, at) };
            if !matrix.load(&vals) {
                if step_point || at != 0.0 { continue; }
                at = dc_alt;
                dc_at = Some(dc_alt);
                vals = fallback.clone();
                if !matrix.load(&vals) { continue; }
            }
            let w = 2.0 * PI * at;
            for (&h, list) in &rows {
                let lh = local(h).expect("a block node");
                z.fill(C64::ZERO_C);
                z[lh] = C64::new(1.0, 0.0);
                if !matrix.solve_loaded(&mut z) { continue; }
                let mut zm = (0.0f64, h);
                for (l, &u) in block.iter().enumerate() {
                    if s.unknowns[u].kind == UnkKind::Voltage && z[l].norm() > zm.0 { zm = (z[l].norm(), u); }
                }
                search.zmax.entry(h).or_insert_with(|| vec![(0.0, h); coarse.len() + 1])[p] = zm;
                if step_point { continue; }
                let zh = search.zdp.entry(h).or_default();
                *zh = zh.max(z[lh].norm());
                let mut sum = C64::ZERO_C;
                let best = best.get_mut(&h).expect("a row's shares");
                for (j, (_, terms)) in list.iter().enumerate() {
                    let mut y = C64::ZERO_C;
                    for &(l, g, c) in terms { y += C64::new(g, w * c) * z[l]; }
                    best[j] = best[j].max(y.norm());
                    sum += y;
                }
                if !wide.contains(&h) {
                    share_err = share_err.max((sum - C64::new(1.0, 0.0)).norm());
                }
            }
        }
        for (&h, list) in &rows {
            let shares = &best[&h];
            let mut top = 0;
            for j in 1..list.len() {
                if shares[j] > shares[top] { top = j; }
            }
            search.largest.insert(h, list[top].0);
            for (j, (d, _)) in list.iter().enumerate() {
                search.atts.entry(*d).or_default().push((h, shares[j]));
            }
            let mut table: Vec<(String, f64)> = list.iter().zip(shares).map(|((d, _), &v)| (insts[*d].name.clone(), v)).collect();
            table.sort_by(|a, b| b.1.total_cmp(&a.1));
            shares_out.push((h as u32, table));
        }
        for atts in search.atts.values_mut() { atts.sort_by_key(|a| a.0); }
        share_ms += shares_start.elapsed().as_secs_f64() * 1e3;

        // the search: cycles and tests, until nothing changes
        let bar = &bars;
        let mut forced: Vec<(usize, usize)> = Vec::new();
        let mut first: BTreeMap<usize, bool> = BTreeMap::new();
        let mut results: BTreeMap<usize, Tested> = BTreeMap::new();
        let mut signatures: BTreeMap<usize, Vec<(usize, usize, u64)>> = BTreeMap::new();
        // per (other state, node): the signature its test there passed on
        let mut state_sigs: BTreeMap<(usize, usize), Vec<(usize, usize, u64)>> = BTreeMap::new();
        for &u in block {
            if let Some(&z) = search.zdp.get(&u).filter(|&&z| z < STIFF_OHMS && !hub[u]) {
                stiff.push((names[u].clone(), z));
            }
        }
        searched.blocks += 1;
        let a = loop {
            searched.passes += 1;
            let a = search.anchor(&insts, &hub, &forced);
            let graph = search.same_round(&insts, &a, &hub, &first);
            let cycles: Vec<_> = components(&graph).into_iter().filter(|c| c.len() > 1).collect();
            if !cycles.is_empty() {
                for c in cycles {
                    let names = c.iter().take(4).map(|&u| names[u].as_str()).collect::<Vec<_>>().join(", ");
                    merges.push(format!("a same-round cycle merges {} islands (of {names}{})", c.len(), if c.len() > 4 { " ..." } else { "" }));
                    for &u in &c[1..] { forced.push((c[0], u)); }
                }
                continue;
            }
            // the tests
            let mut held: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            for &(h, d) in &a.weak { held.entry(h).or_default().push(d); }
            // the islands' structural parts: a test at h reads of the holder only the part its
            // matrix entries join to h, and of a consumer island only the parts its instances at h
            // attach to; the rest of an island changes nothing there (a pad in block 0 beside the
            // others, which it is not wired to)
            let mut part = Sets((0..=n).collect());
            for e in &entries {
                let (r, c) = (col_at[e.row], e.col);
                if in_block(r) && in_block(c) && a.sets.find(r) == a.sets.find(c) { part.union(r, c); }
            }
            let mut psize = vec![0usize; n + 1];
            for &u in block { psize[part.find(u)] += 1; }
            // each member's part, and the members of each part and of each island in the block's
            // order: what a test reads (`node_test`), found once a pass and not per test
            let mut part_of = vec![usize::MAX; n + 1];
            let mut by_part: HashMap<usize, Vec<usize>> = HashMap::new();
            let mut by_island: HashMap<usize, Vec<usize>> = HashMap::new();
            for &u in block {
                part_of[u] = part.find(u);
                by_part.entry(part_of[u]).or_default().push(u);
                by_island.entry(a.sets.find(u)).or_default().push(u);
            }
            // per consumer island: the parts the consumers attach to
            let reached_in = |h: usize, ds: &[usize], r: usize| -> BTreeSet<usize> {
                ds.iter().flat_map(|&d| insts[d].unks.iter().copied()).filter(|&v| v != h && in_block(v) && a.sets.find(v) == r).map(|v| part.find(v)).collect()
            };
            let mut failures: Vec<Failure> = Vec::new();
            let mut flipped = false;
            results.retain(|h, _| held.contains_key(h));
            // each consumer's own terms at h in a state's instances, where the holder keeps no ghost
            let own_terms = |insts: &[Inst], h: usize, ds: &[usize], root: usize, sp: C64| -> C64 {
                let mut y = C64::ZERO_C;
                for &d in ds.iter().filter(|&&d| a.sets.find(a.home[&d]) == root) {
                    if hub[h] || !insts[d].rc {
                        for &(_, r, c, g, cap) in &insts[d].slots {
                            if r == h && c == h { y += at_point(g, cap, sp); }
                        }
                    }
                }
                y
            };
            // a held node's holder, its consumer islands, what a test of it reads of them and the
            // signature it stands on: each part read, by its root and size
            let signature_of = |h: usize, ds: &[usize]| -> (usize, Vec<usize>, Vec<BTreeSet<usize>>, Vec<(usize, usize, u64)>) {
                let holder = a.sets.find(h);
                let soft: Vec<usize> = ds.iter().map(|d| a.sets.find(a.home[d])).collect::<BTreeSet<_>>().into_iter().collect();
                let reached: Vec<BTreeSet<usize>> = soft.iter().map(|&r| reached_in(h, ds, r)).collect();
                let ph = part.find(h);
                let mut signature = vec![(ph, psize[ph], bar[h].to_bits())];
                for (&r, ps) in soft.iter().zip(&reached) {
                    if ps.is_empty() {
                        signature.push((r, by_island.get(&r).map_or(0, Vec::len), 1));
                    } else {
                        signature.push((r, 0, 1));
                        signature.extend(ps.iter().map(|&p| (p, psize[p], 0)));
                    }
                }
                (holder, soft, reached, signature)
            };
            // the members a test reads: the holder's part at h, each consumer island's parts (all
            // of it where its consumers attach to none, only at h)
            let node_test = |insts: &[Inst], h: usize, ds: &[usize], soft_roots: &[usize], reached: &[BTreeSet<usize>]| -> NodeTest {
                let moved = soft_roots.iter().map(|&r| (0..band.len()).map(|k| own_terms(insts, h, ds, r, test_point(&freqs, step, k))).collect()).collect();
                let stiff = by_part.get(&part.find(h)).cloned().unwrap_or_default();
                let soft: Vec<Vec<usize>> = soft_roots.iter().zip(reached).map(|(&r, ps)| {
                    let members = by_island.get(&r).map_or(&[][..], Vec::as_slice);
                    members.iter().copied().filter(|&u| ps.is_empty() || ps.contains(&part_of[u])).collect()
                }).collect();
                // the voltages outside the holder's part its rows read that can move h: not the
                // cut's far ends, a declared supply or a node a fixed source holds
                let mut imports: Vec<usize> = entries.iter().filter(|e| stiff.binary_search(&col_at[e.row]).is_ok()).map(|e| e.col)
                    .filter(|&c| common.voltage[c] && !declared[c] && !pinned[c] && stiff.binary_search(&c).is_err() && soft.iter().all(|p| p.binary_search(&c).is_err()))
                    .collect();
                imports.sort_unstable();
                imports.dedup();
                NodeTest { node: h, stiff, soft, moved, diag: s.diag[h] as usize, imports }
            };
            // the holder's instances at h's part that move by themselves (`forward`'s `moving`)
            let moving_at = |h: usize, holder: usize| -> Vec<String> {
                let ph = part.find(h);
                by_block.get(&b).into_iter().flatten().copied()
                    .filter(|&d| moves(&insts[d]) && a.home.get(&d).is_some_and(|&u| a.sets.find(u) == holder) && insts[d].unks.iter().any(|&u| in_block(u) && part.find(u) == ph))
                    .map(|d| insts[d].name.clone()).collect()
            };
            // the bars (`judge`): stability always, accuracy where `accuracy`, a round late
            let judged = |result: &Result<Measured, usize>, h: usize, accuracy: bool| -> Result<Judged, String> {
                if never[h] {
                    return Err("the run refused it: a value read across it left what the circuit holds".into());
                }
                match result {
                    Ok(m) => {
                        let j = judge(&m.rho, &weights, accuracy.then_some(bar[h]));
                        match j.fails {
                            Some((Bar::Stability, k)) => Err(format!("stability: rho {:.1e} at {} over {STABILITY}", m.rho[k], label(k))),
                            Some((Bar::Accuracy, k)) => Err(format!("accuracy: rho {:.1e} at {}, wrong by {:.1e} a round late, over the bar {:.0e}", m.rho[k], label(k), j.wrong.1, bar[h])),
                            None => Ok(j),
                        }
                    }
                    Err(k) => Err(format!("singular at {}", if *k == 0 { hz(dc_alt) } else { label(*k) })),
                }
            };
            let fail = |h: usize, holder: usize, soft_roots: Vec<usize>, stiff_part: &[usize], soft_parts: &[Vec<usize>], result: &Result<Measured, usize>, why: String| -> Failure {
                let worst = match result { Ok(m) => m.worst, Err(_) => 0 };
                let joins = soft_roots[worst];
                let part = &soft_parts[worst];
                let list = part.iter().take(5).map(|&u| names[u].as_str()).collect::<Vec<_>>().join(", ");
                let supply = declared[h].then(|| (names[h].clone(), format!("{why}; a consumer of {} joins its island", part.len())));
                Failure { node: h, holder, joins, soft: soft_roots, why: format!("the cut at {} fails ({why})", names[h]),
                    joined: format!("its consumer of {} ({list}{}) joins the holder of {}", part.len(), if part.len() > 5 { " ..." } else { "" }, stiff_part.len()),
                    supply }
            };
            // the pass's tests (each held node whose holder, consumers or their sizes changed; one
            // that passed waits while the islands still grow), measured together (`measure_each`:
            // what they share is factored once a point), then taken in order
            let mut due: Vec<(usize, &Vec<usize>, usize, Vec<usize>)> = Vec::new();
            let mut node_tests: Vec<NodeTest> = Vec::new();
            for (&h, ds) in &held {
                let (holder, soft_roots, reached, signature) = signature_of(h, ds);
                if signatures.get(&h) == Some(&signature) { continue; }
                signatures.insert(h, signature);
                node_tests.push(node_test(&insts, h, ds, &soft_roots, &reached));
                due.push((h, ds, holder, soft_roots));
            }
            // from a fixed start `s` is the key's base: a test made there before on all it reads
            // is taken from there (`Others::base_tests`); the rest are measured together, one
            // singular at DC again at the DC fallback with its consumers' own terms there
            let test_start = Instant::now();
            let dc_point = C64::new(0.0, 2.0 * PI * dc_alt);
            let dcs: Vec<Vec<C64>> = due.iter().map(|(h, ds, _, soft_roots)| soft_roots.iter().map(|&r| own_terms(&insts, *h, ds, r, dc_point)).collect()).collect();
            let keys: Vec<Option<BaseKey>> = node_tests.iter().zip(&dcs).map(|(t, dc)| present.is_some().then(|| BaseKey::of(t, dc))).collect();
            let mut got: Vec<Option<Arc<(Result<Measured, usize>, bool)>>> = {
                let memo = others.base_tests.lock().unwrap_or_else(|e| e.into_inner());
                keys.iter().map(|k| k.as_ref().and_then(|k| memo.get(k).cloned())).collect()
            };
            let fresh: Vec<usize> = (0..got.len()).filter(|&i| got[i].is_none()).collect();
            if !fresh.is_empty() {
                let tests: Vec<NodeTest> = fresh.iter().map(|&i| node_tests[i].clone()).collect();
                let measured = measure_each(&tests, &row_of, &entries, &common.voltage, &band);
                let mut memo = others.base_tests.lock().unwrap_or_else(|e| e.into_inner());
                for ((&i, mut test), mut result) in fresh.iter().zip(tests).zip(measured) {
                    let fell = matches!(result, Err(0));
                    if fell {
                        for (m, &z) in test.moved.iter_mut().zip(&dcs[i]) {
                            m[0] = z;
                        }
                        result = measure(test.node, &test.stiff, &test.soft, &row_of, &entries, &common.voltage, &band, Some(&fallback), &test.moved, test.diag, &test.imports);
                    }
                    let g = Arc::new((result, fell));
                    if let Some(k) = &keys[i] {
                        memo.insert(k.clone(), g.clone());
                    }
                    got[i] = Some(g);
                }
            }
            searched.ms += test_start.elapsed().as_secs_f64() * 1e3;
            for ((h, _, holder, soft_roots), (test, got)) in due.into_iter().zip(node_tests.into_iter().zip(got)) {
                let NodeTest { stiff: stiff_part, soft: soft_parts, .. } = test;
                searched.tests += 1;
                let (result, fell) = &*got.expect("each test is measured or taken from before");
                if *fell {
                    dc_at = Some(dc_alt);
                }
                let result = result.clone();
                if let Ok(m) = &result {
                    let weighed: Vec<f64> = m.rho.iter().zip(&weights).map(|(&r, &w)| late_wrong(r, w)).collect();
                    bandstep.insert(h, (peak(&weighed[..nf]).1, weighed[nf], bar[h]));
                }
                let moving = moving_at(h, holder);
                let fwd = match &result { Ok(m) => forward(m, &weights, !moving.is_empty()), Err(_) => (0.0, 0.0) };
                match (judged(&result, h, true), result) {
                    (Err(why), result) => {
                        failures.push(fail(h, holder, soft_roots, &stiff_part, &soft_parts, &result, why));
                        results.remove(&h);
                        searched.failed += 1;
                    }
                    (Ok(j), Ok(Measured { rho, tau, gain, shares, ys, zs, .. })) => {
                        // the way whose late read forces the smaller error (`holder_first`), at
                        // this state; the key's states may turn it after
                        let stiff_first = holder_first(tau, gain, fwd, bar[h], hub[h]);
                        if first.insert(h, stiff_first).unwrap_or(true) != stiff_first { flipped = true; }
                        // each consumer's reach: the block's largest response to a current at h
                        // (the coarse point on either side of a band point, the larger) times
                        // the admittance the consumer reflects there
                        let none = vec![(0.0, h); coarse.len() + 1];
                        let zm = search.zmax.get(&h).unwrap_or(&none);
                        let zat = |k: usize| -> (f64, usize) {
                            if k == nf { return zm[coarse.len()]; }
                            match coarse.iter().position(|&c| c >= freqs[k]) {
                                Some(0) => zm[0],
                                Some(i) => if zm[i].0 > zm[i - 1].0 { zm[i] } else { zm[i - 1] },
                                None => zm[coarse.len() - 1],
                            }
                        };
                        let reach = ys.iter().map(|y| {
                            let (mut best, mut at) = (0.0f64, String::new());
                            for (k, &y) in y.iter().enumerate() {
                                let (z, node) = zat(k);
                                if z * y > best {
                                    best = z * y;
                                    at = format!("at {} through {}", label(k), names[node]);
                                }
                            }
                            (best, at)
                        }).collect();
                        results.insert(h, Tested { acc_rho: rho.clone(), rho, tau, first: stiff_first, shares: soft_roots.iter().copied().zip(shares).collect(), reach,
                            stability: (j.rho.1, label(j.rho.0)), accuracy: (j.wrong.1, label(j.wrong.0)), zs, ys, bound: None, gain, over: (tau, fwd.0, fwd.1), moving });
                    }
                    (Ok(_), Err(_)) => unreachable!("an error refuses"),
                }
            }
            // every test at the primary state stands on these islands: each other state tests the
            // cuts its switches reach, in the order given, until one fails (its merges make the rest
            // stale). A switch reaches a cut through the part of an island its matrix entries join it
            // to: the node's part of the holder, or a part of a consumer island its instances at the
            // node attach to. Elsewhere a state differs only by the step it settled for. The states
            // go in waves: a wave's tests not made before are measured on the pool's cores, a state
            // to a unit (`measure_state`), and kept; then its states are judged in order as one by
            // one, so a failure leaves at most the rest of its wave measured ahead, kept for later.
            // The present state comes last, as a guard: it reaches every cut, on the stability bar
            // alone, and nothing of it is kept.
            if failures.is_empty() && !flipped && !views.is_empty() {
                let state_start = Instant::now();
                // each held node's holder, consumer islands, signature and what its test reads
                // besides a state's values (`TestShape`): the same in every state
                let sigs: Vec<_> = held.iter().map(|(&h, ds)| {
                    let (holder, soft_roots, reached, signature) = signature_of(h, ds);
                    let shape = Shaped::new(TestShape {
                        node: h,
                        diag: s.diag[h] as usize,
                        stiff: by_part.get(&part.find(h)).cloned().unwrap_or_default(),
                        soft: soft_roots.iter().zip(&reached).map(|(&r, ps)| {
                            let members = by_island.get(&r).map_or(&[][..], Vec::as_slice);
                            members.iter().copied().filter(|&u| ps.is_empty() || ps.contains(&part_of[u])).collect()
                        }).collect(),
                        own: soft_roots.iter().map(|&r| ds.iter().copied().filter(|&d| a.sets.find(a.home[&d]) == r && (hub[h] || !insts[d].rc)).collect()).collect(),
                    });
                    (h, ds, holder, soft_roots, reached, signature, shape)
                }).collect();
                let dc_point = C64::new(0.0, 2.0 * PI * dc_alt);
                let wave = pool.map_or(1, |p| 4 * (p.count + 1));
                let mut next = 0;
                while next < views.len() && failures.is_empty() {
                    let end = (next + wave).min(views.len());
                    let mut ready = Vec::new();
                    let mut units: Vec<Unit<Vec<(Result<Measured, usize>, bool)>>> = Vec::new();
                    for vi in next..end {
                        let (xi, lin, touch) = (views[vi].at, &views[vi].lin, views[vi].touch);
                        let parts: BTreeSet<usize> = block.iter().filter(|&&u| touch[u]).map(|&u| part.find(u)).collect();
                        if parts.is_empty() { continue; }
                        let roots: BTreeSet<usize> = block.iter().filter(|&&u| touch[u]).map(|&u| a.sets.find(u)).collect();
                        let mut due = Vec::new();
                        for (h, ds, holder, soft_roots, reached, signature, shape) in &sigs {
                            let reaches = parts.contains(&part.find(*h)) || soft_roots.iter().zip(reached).any(|(r, ps)| if ps.is_empty() { roots.contains(r) } else { !ps.is_disjoint(&parts) });
                            if !reaches || state_sigs.get(&(xi, *h)) == Some(signature) { continue; }
                            due.push((*h, *ds, *holder, soft_roots, reached, signature, shape));
                        }
                        // a test made before on all this one reads is taken from there (`TestKey`);
                        // the rest are made (`node_test`) and go to a unit with their consumers' own
                        // terms at the DC fallback
                        let keys: Vec<TestKey> = if views[vi].kept { due.iter().map(|d| TestKey { state: xi, shape: d.6.clone() }).collect() } else { Vec::new() };
                        let got: Vec<Option<Arc<(Result<Measured, usize>, bool)>>> = if views[vi].kept {
                            let memo = others.memo.lock().unwrap_or_else(|e| e.into_inner());
                            keys.iter().map(|k| memo.get(k).cloned()).collect()
                        } else {
                            vec![None; due.len()]
                        };
                        let fresh: Vec<usize> = (0..got.len()).filter(|&i| got[i].is_none()).collect();
                        if !fresh.is_empty() {
                            let tests: Vec<NodeTest> = fresh.iter().map(|&i| {
                                let (h, ds, _, soft_roots, reached, _, _) = due[i];
                                node_test(&lin.insts, h, ds, soft_roots, reached)
                            }).collect();
                            let dc_moved: Vec<Vec<C64>> = fresh.iter().map(|&i| {
                                let (h, ds, _, soft_roots, _, _, _) = due[i];
                                soft_roots.iter().map(|&r| own_terms(&lin.insts, h, ds, r, dc_point)).collect()
                            }).collect();
                            let (lin, g, common) = (lin.clone(), g.clone(), common.clone());
                            units.push(Box::new(move || measure_state(&lin, &g, tests, dc_moved, &common)));
                        }
                        ready.push((vi, due, keys, got, fresh));
                    }
                    let mut measured = fan_out(units, pool).map_err(|f| f.diag("held: a state's tests"))?.into_iter();
                    {
                        let mut memo = others.memo.lock().unwrap_or_else(|e| e.into_inner());
                        for (_, _, keys, got, fresh) in ready.iter_mut().filter(|r| !r.4.is_empty()) {
                            let m = measured.next().expect("a unit for each state with tests to measure");
                            for (&i, g) in fresh.iter().zip(m) {
                                let g = Arc::new(g);
                                if let Some(k) = keys.get(i) {
                                    memo.insert(k.clone(), g.clone());
                                }
                                got[i] = Some(g);
                            }
                        }
                    }
                    next = end;
                    for (vi, due, _, got, fresh) in ready {
                        if !failures.is_empty() { break; }
                        let x = &views[vi];
                        let xi = x.at;
                        searched.memo += got.len() - fresh.len();
                        for ((h, _, holder, soft_roots, _, signature, shape), got) in due.into_iter().zip(got) {
                            let (stiff_part, soft_parts) = (&shape.0.stiff, &shape.0.soft);
                            let got = got.expect("each test is measured or taken from before");
                            if x.kept { searched.state_tests += 1; } else { searched.guard_tests += 1; }
                            if got.1 {
                                dc_at = Some(dc_alt);
                            }
                            // a bound over the devices' states: per point the larger `|Z_S,ss|` of this
                            // state and the present one, times the sum over the consumers of each
                            // one's larger `|Y'_j|` (`State::bound`), on a copy of the shared result
                            let bounded = match (x.bound, &got.0, results.get(&h)) {
                                (true, Ok(m), Some(p)) => {
                                    let same = p.shares.len() == soft_roots.len() && p.shares.iter().zip(soft_roots).all(|(a, &r)| a.0 == r);
                                    let rho = (0..m.rho.len()).map(|k| {
                                        let y: f64 = if same {
                                            m.ys.iter().zip(&p.ys).map(|(a, b)| a[k].max(b[k])).sum()
                                        } else {
                                            m.ys.iter().chain(&p.ys).map(|a| a[k]).sum()
                                        };
                                        m.zs[k].max(p.zs[k]) * y
                                    }).collect();
                                    Some(Ok(Measured { rho, ..m.clone() }))
                                }
                                _ => None,
                            };
                            let result = bounded.as_ref().unwrap_or(&got.0);
                            match judged(result, h, !x.stability_only) {
                                Err(why) => {
                                    failures.push(fail(h, holder, soft_roots.clone(), stiff_part, soft_parts, result, format!("{}: {why}", x.what)));
                                    results.remove(&h);
                                    if x.kept { searched.state_failed += 1; } else { searched.guard_failed += 1; }
                                }
                                Ok(j) => {
                                    state_sigs.insert((xi, h), signature.clone());
                                    if let (Some(r), Ok(m)) = (results.get_mut(&h), result) {
                                        if j.rho.1 > r.stability.0 { r.stability = (j.rho.1, format!("{}, {}", label(j.rho.0), x.what)); }
                                        if !x.stability_only {
                                            for (a, &b) in r.acc_rho.iter_mut().zip(&m.rho) { *a = a.max(b); }
                                            if j.wrong.1 > r.accuracy.0 { r.accuracy = (j.wrong.1, format!("{}, {}", label(j.wrong.0), x.what)); }
                                        }
                                        if x.bound {
                                            let b = judge(&m.rho, &weights, None);
                                            r.bound = Some((b.wrong.1 / bar[h], label(b.wrong.0)));
                                        }
                                        // the direction's figures over the key's states the
                                        // accuracy bar holds in (`Tested::over`)
                                        if x.kept && !x.stability_only {
                                            let f = forward(m, &weights, !r.moving.is_empty());
                                            r.over = (r.over.0.max(m.tau), r.over.1.max(f.0), r.over.2.max(f.1));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // each cut's direction over the key's states (`Tested::over`): one that turns
                // there runs the pass again, as one that turns at this state does
                if failures.is_empty() {
                    for (&h, r) in results.iter_mut() {
                        let dir = holder_first(r.over.0, r.gain, (r.over.1, r.over.2), bar[h], hub[h]);
                        if dir != r.first {
                            r.first = dir;
                            first.insert(h, dir);
                            flipped = true;
                        }
                    }
                }
                state_ms += state_start.elapsed().as_secs_f64() * 1e3;
            }
            // a failed test is judged on its consumers as grown, soft islands first: one with a
            // consumer island that is itself the holder of another failed test in the pass waits,
            // and is tested again next pass on that island as it has grown; when every failure
            // waits on another, all go in. Which go in depends only on the pass's failures, not on
            // the order the tests are taken in.
            let merged = !failures.is_empty();
            let waits: Vec<bool> = failures.iter()
                .map(|f| failures.iter().any(|g| g.node != f.node && g.holder != f.holder && f.soft.contains(&g.holder)))
                .collect();
            let all = waits.iter().all(|&w| w);
            for (f, wait) in failures.drain(..).zip(waits) {
                if wait && !all {
                    merges.push(format!("{} on a consumer that grows in the same pass: tested again on it as grown", f.why));
                    signatures.remove(&f.node);
                    continue;
                }
                merges.push(format!("{}: {}", f.why, f.joined));
                failed_supplies.extend(f.supply);
                forced.push((f.holder, f.joins));
            }
            if merged || flipped {
                continue;
            }
            break a;
        };
        for &u in block {
            global.union(u, a.sets.find(u));
        }
        tests.extend(results);
        weak_all.extend(a.weak.iter().copied());
        home_all.extend(a.home.iter().map(|(&d, &u)| (d, u)));
    }

    // islands over every exact block
    let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for block in &exact {
        for &u in block { by_root.entry(global.find(u)).or_default().push(u); }
    }
    let split: Vec<Vec<usize>> = by_root.into_values().map(|mut p| { p.sort_unstable(); p }).collect();
    let mut island_of = vec![usize::MAX; n + 1];
    for (i, nodes) in split.iter().enumerate() {
        for &u in nodes { island_of[u] = i; }
    }
    // each instance's home island: the searched blocks' homes, else its first entry's row
    let home_of = |d: usize| -> Option<usize> {
        if let Some(&u) = home_all.get(&d) { return Some(island_of[u]); }
        insts[d].slots.iter().map(|e| col_at[e.1]).chain(insts[d].unks.iter().copied()).find(|&u| island_of[u] != usize::MAX).map(|u| island_of[u])
    };
    let homes: Vec<Option<usize>> = (0..insts.len()).map(home_of).collect();
    let weak: BTreeSet<(usize, usize)> = weak_all.iter().copied().collect();
    // exact dependencies between islands: every entry across two that is not a cut's coupling
    let mut exact_pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (d, i) in insts.iter().enumerate() {
        let home = homes[d];
        // a column its home reads in the same round: one of its entries in its home's rows
        let home_reads = |cu: usize| i.slots.iter().any(|e| e.2 == cu && home == Some(island_of[col_at[e.1]]));
        for &(_, r, c, _, _) in &i.slots {
            let (ru, cu) = (col_at[r], c);
            let (ri, ci) = (island_of[ru], island_of[cu]);
            if ri == usize::MAX || ci == usize::MAX || ri == ci { continue; }
            // a cut's coupling: the holder's row reading its consumer (at a node in the hub form,
            // in any column the consumer's home reads in the same round: its return), or the
            // consumer reading it
            let returned = || Some(ci) == home || (hub[ru] && home_reads(cu));
            let cut = (ru == r && weak.contains(&(ru, d)) && returned()) || (own(cu) && weak.contains(&(cu, d)) && Some(ri) == home);
            if !cut { exact_pairs.insert((ci, ri)); }
        }
    }
    // edges: every exact read in the same round; in the hub form the consumers read in the same
    // round and return a round late; elsewhere a cut keeps the direction its tau gives
    let mut edges = Vec::new();
    // a consumers-first cut's same-round reads, consumer to holder: the small-island rule weighs a
    // consumer only once its own pieces have gathered
    let mut fan_in: BTreeSet<(usize, usize)> = BTreeSet::new();
    for &(from, to) in &exact_pairs {
        edges.push(Edge { from, to, exact: true, same: true });
    }
    for &(h, d) in &weak {
        let (Some(c), holder) = (homes[d], island_of[h]) else { continue };
        if c == holder { continue; }
        let consumers_first = !hub[h] && insts[d].rc && tests.get(&h).is_some_and(|t| !t.first);
        if consumers_first { fan_in.insert((c, holder)); }
        edges.push(Edge { from: holder, to: c, exact: false, same: !consumers_first });
        edges.push(Edge { from: c, to: holder, exact: false, same: consumers_first });
    }
    let mut groups = Groups((0..split.len()).collect());
    condense(&mut groups, &edges, &mut merges);
    let control_groups: BTreeSet<_> = (1..=n).filter(|&u| control[u] && island_of[u] != usize::MAX).map(|u| groups.root(island_of[u])).collect();
    groups.merge(&control_groups.into_iter().collect::<Vec<_>>(), "control island", &mut merges);
    condense(&mut groups, &edges, &mut merges);
    // the small-island rule's facts over the islands, and the root of each
    let facts_of = |groups: &Groups, gather: bool| -> (Vec<Facts>, Vec<usize>) {
        let mut index: BTreeMap<usize, usize> = BTreeMap::new();
        let mut reps = Vec::new();
        for i in 0..split.len() {
            let g = groups.root(i);
            if !index.contains_key(&g) {
                index.insert(g, reps.len());
                reps.push(g);
            }
        }
        let mut facts: Vec<Facts> = reps.iter().map(|_| Facts::default()).collect();
        for (i, nodes) in split.iter().enumerate() {
            let f = &mut facts[index[&groups.root(i)]];
            f.size += nodes.len();
            f.control |= nodes.iter().any(|&u| control[u]);
        }
        let mut readers: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); reps.len()];
        for e in edges.iter().filter(|e| e.same && !(gather && !e.exact && fan_in.contains(&(e.from, e.to)))) {
            let (gf, gt) = (groups.root(e.from), groups.root(e.to));
            if gf != gt { readers[index[&gf]].insert(index[&gt]); }
        }
        for (f, r) in facts.iter_mut().zip(&readers) {
            f.readers = r.len();
            f.reader = if r.len() == 1 { r.first().copied() } else { None };
        }
        (facts, reps)
    };
    // small islands join their single reader: first with a consumers-first cut's same-round reads
    // left out, so each consumer gathers its own pieces, then with them, so a consumer still small
    // whole joins its holder (with no such cut the second pass finds nothing more)
    for gather in [true, false] {
        loop {
            let (facts, reps) = facts_of(&groups, gather);
            let Some(&(i, r)) = joins(&facts, o.min_island).first() else { break };
            let (a, b) = (reps[i], reps[r]);
            groups.merge(&[a.min(b), a.max(b)], "small, joins its single reader", &mut merges);
            condense(&mut groups, &edges, &mut merges);
        }
    }
    // block 0: the control island, with every island it reads in the same round
    let ctl_island = (1..=n).find(|&u| control[u] && island_of[u] != usize::MAX).map(|u| island_of[u]);
    if let Some(c) = ctl_island {
        loop {
            let root = groups.root(c);
            let Some(e) = edges.iter().find(|e| e.same && groups.root(e.to) == root && groups.root(e.from) != root) else { break };
            let from = groups.root(e.from);
            let why = format!("block 0 reads {} in the same round", names[split[e.from][0]]);
            groups.merge(&[root.min(from), root.max(from)], &why, &mut merges);
            condense(&mut groups, &edges, &mut merges);
        }
    }
    let zero_root = ctl_island.map(|c| groups.root(c));
    let mut merged: Vec<Vec<usize>> = vec![Vec::new(); split.len()];
    for (i, nodes) in split.iter().enumerate() {
        merged[groups.root(i)].extend_from_slice(nodes);
    }
    let mut final_of = vec![usize::MAX; split.len()];
    let mut islands = Vec::new();
    for (r, nodes) in merged.iter_mut().enumerate().filter(|(_, v)| !v.is_empty()) {
        nodes.sort_unstable();
        final_of[r] = islands.len();
        islands.push(HeldIsland { unknowns: nodes.iter().map(|&u| u as u32).collect(), stage: 0, control: nodes.iter().any(|&u| control[u]),
            declared: nodes.iter().any(|&u| declared[u]), events: false, zero: Some(r) == zero_root });
    }
    let fin = |i: usize| final_of[groups.root(i)];
    for (d, home) in homes.iter().enumerate() {
        if let Some(i) = home.filter(|_| insts[d].events) {
            islands[fin(i)].events = true;
        }
    }
    let zero = zero_root.map(|r| final_of[r]);
    let mut same = vec![Vec::new(); islands.len()];
    for e in edges.iter().filter(|e| e.same) {
        let (a, b) = (fin(e.from), fin(e.to));
        if a != b { same[a].push(b); }
    }
    sort_graph(&mut same);
    let mut indegree = vec![0; islands.len()];
    for readers in &same { for &b in readers { indegree[b] += 1; } }
    let mut ready: BTreeSet<_> = (0..islands.len()).filter(|&i| indegree[i] == 0).collect();
    let mut visited = 0;
    while let Some(i) = ready.pop_first() {
        visited += 1;
        for &j in &same[i] {
            islands[j].stage = islands[j].stage.max(islands[i].stage + 1);
            indegree[j] -= 1;
            if indegree[j] == 0 { ready.insert(j); }
        }
    }
    if visited != islands.len() {
        return Err(SimDiag::setup("held: the same-round graph keeps a cycle after its merges".into()));
    }
    // the held nodes that remain across final islands
    let mut by_node: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(h, d) in &weak { by_node.entry(h).or_default().push(d); }
    let mut nodes = Vec::new();
    let mut device_outside = Vec::new();
    for (h, ds) in by_node {
        let holder = fin(island_of[h]);
        let mut consumers: BTreeMap<usize, Consumer> = BTreeMap::new();
        let test = tests.get(&h);
        for d in ds {
            let Some(c) = homes[d] else { continue };
            let island = fin(c);
            if island == holder { continue; }
            if !insts[d].rc && !insts[d].slots.is_empty() && !hub[h] {
                device_outside.push(format!("{} ({}) at {}", insts[d].name, insts[d].kind, names[h]));
            }
            let at = test.and_then(|t| t.shares.iter().position(|&(r, _)| island_of[r] != usize::MAX && fin(island_of[r]) == island));
            let share = at.map_or(0.0, |j| test.unwrap().shares[j].1);
            let (reach, reach_at) = at.map_or((0.0, String::new()), |j| test.unwrap().reach[j].clone());
            let e = consumers.entry(island).or_insert(Consumer { island, insts: Vec::new(), rc: true, rho: 0.0, reach: 0.0, reach_at: String::new() });
            if reach > e.reach {
                e.reach = reach;
                e.reach_at = reach_at;
            }
            e.insts.push(insts[d].name.clone());
            e.rc &= insts[d].rc;
            e.rho = e.rho.max(share);
        }
        if consumers.is_empty() { continue; }
        let (rho, tau, stiff_first) = test.map_or((Vec::new(), 0.0, true), |t| (t.rho.clone(), t.tau, t.first));
        let (stability, acc_rho, accuracy, bound) = test.map_or(((0.0, "untested".to_string()), Vec::new(), (0.0, "untested".to_string()), None),
            |t| (t.stability.clone(), t.acc_rho.clone(), t.accuracy.clone(), t.bound.clone()));
        nodes.push(HeldNode { node: h as u32, holder, zero: Some(holder) == zero, hub: hub[h], bar: bars[h], rho, tau, stiff_first, consumers: consumers.into_values().collect(),
            stability, acc_rho, accuracy, bound });
    }
    let hub_nodes = nodes.iter().filter(|h| h.hub).map(|h| names[h.node as usize].clone()).collect();
    let supplies = (1..=n).filter(|&u| declared[u]).map(|u| names[u].clone()).collect();
    let mut kinds: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    for i in &insts {
        let e = kinds.entry(i.kind).or_default();
        e.0 += 1;
        e.1 += usize::from(i.events);
    }
    let disagree = bandstep.iter().filter(|(_, &(band, step, bar))| (band >= bar) != (step >= bar))
        .map(|(&h, &(band, step, bar))| (names[h].clone(), band, step, bar)).collect();
    let total_ms = start.elapsed().as_secs_f64() * 1e3;
    Ok(Held {
        names, n, t, islands, nodes, merges, shares: shares_out, share_err,
        kinds: kinds.into_iter().map(|(k, (a, b))| (k, a, b)).collect(),
        coarse: coarse.len(), freqs, band: g.band, fmax: o.fmax, step, disagree, tested: bandstep.len(), dc_at, cut_tol: o.cut_tol, hub_tol: o.hub_tol, min_island: o.min_island,
        zero, supplies, stiff, failed_supplies, hub_nodes, device_outside,
        ms: [lin_ms, share_ms, total_ms - lin_ms - share_ms],
        searched: Searched { ms: searched.ms + state_ms, state_ms, ..searched },
        round: o.round,
        states: others.whats(),
    })
}

impl Held {
    pub fn total_ms(&self) -> f64 {
        self.ms.iter().sum()
    }

    /// The weight of a value read `lag` seconds late at each test point (`weights_at`).
    /// `judge` takes these weights with a cut's `acc_rho`.
    pub fn weights(&self, lag: f64) -> Vec<f64> {
        weights_at(&self.freqs, self.step, lag, accuracy_band(self.band, self.fmax))
    }

    /// Every point's accuracy weight, including those above the band, for the plan report.
    pub fn weights_full(&self, lag: f64) -> Vec<f64> {
        weights_at(&self.freqs, self.step, lag, f64::INFINITY)
    }

    pub(super) fn test_point(&self, k: usize) -> C64 {
        test_point(&self.freqs, self.step, k)
    }

    /// A test point's name: DC, a frequency, or the step's matrix after the band.
    pub fn point(&self, k: usize) -> String {
        if k == 0 { "DC".to_string() } else if k == self.freqs.len() { "the step's matrix".to_string() } else { hz(self.freqs[k]) }
    }

    pub fn report(&self, title: &str) -> String {
        let mut out = String::new();
        let total: usize = self.islands.iter().map(|i| i.unknowns.len()).sum();
        let mut sizes: Vec<usize> = self.islands.iter().map(|i| i.unknowns.len()).collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        let largest = sizes.first().copied().unwrap_or(0);
        let percent = |k: usize| if total == 0 { 0.0 } else { 100.0 * k as f64 / total as f64 };
        let over = sizes.iter().filter(|&&s| s > self.min_island).count();
        let stages = self.islands.iter().map(|i| i.stage + 1).max().unwrap_or(0);
        line(&mut out, &format!("{title} at t = {:.4} s: n={}; {total} unknowns in {} islands ({over} over {}); largest {largest} ({:.1}%); {stages} stages; sizes {:?}",
            self.t, self.n, self.islands.len(), self.min_island, percent(largest), sizes));
        let rest = self.islands.iter().enumerate().filter(|&(i, _)| Some(i) != self.zero).map(|(_, i)| i.unknowns.len()).max().unwrap_or(0);
        match self.zero {
            Some(z) => {
                let island = &self.islands[z];
                line(&mut out, &format!("block 0: island {z}, {} unknowns ({:.1}%), stage {}; largest outside block 0 {rest}",
                    island.unknowns.len(), percent(island.unknowns.len()), island.stage));
                let all = island.unknowns.iter().take(80).map(|&u| self.names[u as usize].as_str()).collect::<Vec<_>>().join(", ");
                line(&mut out, &format!("block 0 holds: {all}{}", if island.unknowns.len() > 80 { " ..." } else { "" }));
            }
            None => line(&mut out, &format!("no block 0; largest {rest}")),
        }
        let list = |v: &[String]| if v.is_empty() { String::new() } else { format!(": {}{}", v.iter().take(8).cloned().collect::<Vec<_>>().join(", "), if v.len() > 8 { " ..." } else { "" }) };
        let stiff = self.stiff.iter().take(8).map(|(n, z)| format!("{n} {z:.1e} ohm")).collect::<Vec<_>>().join(", ");
        let stiff = if self.stiff.is_empty() { String::new() } else { format!("; ordinary nodes under {STIFF_OHMS} ohm: {stiff}{}", if self.stiff.len() > 8 { " ..." } else { "" }) };
        if self.supplies.is_empty() {
            line(&mut out, &format!("no declared supply: only the pads take their consumers' currents a round late{stiff}"));
        } else {
            line(&mut out, &format!("declared supplies {}{}{stiff}", self.supplies.len(), list(&self.supplies)));
        }
        for (node, why) in &self.failed_supplies {
            line(&mut out, &format!("{node} is declared a supply, but its consumers fail its total ({why})"));
        }
        line(&mut out, &format!("checks: the hub form at {} held nodes{}; {} device cuts at ordinary nodes{}", self.hub_nodes.len(), list(&self.hub_nodes),
            self.device_outside.len(), list(&self.device_outside)));
        // reach, a diagnostic: the consumers at nodes in the hub form whose reach passes hub_tol
        let over: Vec<String> = self.nodes.iter().filter(|h| h.hub).flat_map(|h| h.consumers.iter().filter(|c| c.reach > self.hub_tol)
            .map(move |c| format!("{} island {} ({}) reach {:.1e} {}", self.names[h.node as usize], c.island, self.islands[c.island].unknowns.len(), c.reach, c.reach_at))).collect();
        let top = self.nodes.iter().filter(|h| h.hub).flat_map(|h| h.consumers.iter()).map(|c| c.reach).fold(0.0, f64::max);
        line(&mut out, &format!("reach: {} consumers over hub_tol at nodes in the hub form (largest {top:.1e}){}", over.len(), list(&over)));
        let zero_nodes = self.nodes.iter().filter(|h| h.zero).count();
        line(&mut out, &format!("held nodes {} ({zero_nodes} held by block 0); stability under {STABILITY}; accuracy a round ({:.2e} s) late under cut_tol {:.0e}, hub_tol {:.0e}; shares on {} frequencies, tests on {} and the step's matrix (step {:.2e} s); share sums within {:.1e} of 1{}",
            self.nodes.len(), self.round, self.cut_tol, self.hub_tol, self.coarse, self.freqs.len(), self.step, self.share_err,
            self.dc_at.map_or(String::new(), |f| format!("; DC singular, taken at {}", hz(f)))));
        let list = self.disagree.iter().take(6).map(|(n, band, step, bar)| format!("{n} (band {band:.1e}, step {step:.1e}, bar {bar:.0e})")).collect::<Vec<_>>().join(", ");
        line(&mut out, &format!("band and step's matrix disagree on the bar at {} of {} tested nodes{}{list}", self.disagree.len(), self.tested, if list.is_empty() { "" } else { ": " }));
        let w = &self.searched;
        line(&mut out, &format!("cost: {:.1} ms (instances and band {:.1}, shares {:.1}, search {:.1}: {} passes over {} exact blocks, {} node tests of which {} failed; {} in {} other states of which {} failed; {:.1} ms in the tests, {:.1} of it in the other states)",
            self.total_ms(), self.ms[0], self.ms[1], self.ms[2], w.passes, w.blocks, w.tests, w.failed, w.state_tests, w.states, w.state_failed, w.ms, w.state_ms));
        if !self.states.is_empty() {
            line(&mut out, &format!("other states: {}{}", self.states.iter().take(8).cloned().collect::<Vec<_>>().join("; "), if self.states.len() > 8 { " ..." } else { "" }));
        }
        let kinds = self.kinds.iter().map(|(k, a, b)| if *b > 0 { format!("{k} {a} ({b} raise events)") } else { format!("{k} {a}") }).collect::<Vec<_>>().join(", ");
        line(&mut out, &format!("instances by kind: {kinds}"));
        for (i, island) in self.islands.iter().enumerate() {
            let names = island.unknowns.iter().take(8).map(|&u| self.names[u as usize].as_str()).collect::<Vec<_>>().join(", ");
            let more = if island.unknowns.len() > 8 { format!(" +{} more", island.unknowns.len() - 8) } else { String::new() };
            line(&mut out, &format!("island {i}: stage {}, size {}{}{}{}; {names}{more}", island.stage, island.unknowns.len(),
                if island.zero { ", block 0" } else { "" }, if island.control { ", control" } else { "" }, if island.declared { ", a declared supply" } else { "" }));
        }
        for h in &self.nodes {
            let (k, max) = peak(&h.rho);
            let at = if h.rho.is_empty() { "untested".to_string() } else { self.point(k) };
            let (stab, acc) = (&h.stability, &h.accuracy);
            let form = if h.hub { "read in the same round, the return one round late" } else if h.consumers.iter().all(|c| c.rc) {
                if h.stiff_first { "holder first, ghosts" } else { "consumers first, ghosts" }
            } else { "a device cut at an ordinary node" };
            let mut text = format!("held {}: island {} ({}{}), bar {:.0e}; rho {max:.1e} at {at}; stability {:.1e} at {} (under {STABILITY}), wrong by {:.1e} a round late at {}; {}tau {:.1e}; {form}; consumers:", self.names[h.node as usize], h.holder,
                self.islands[h.holder].unknowns.len(), if h.zero { ", block 0" } else { "" }, h.bar, stab.0, stab.1, acc.0, acc.1,
                h.bound.as_ref().map_or(String::new(), |(r, at)| format!("its own devices in either state wrong by {r:.2} times the bar at {at}; ")), h.tau);
            for c in &h.consumers {
                let list = c.insts.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
                let more = if c.insts.len() > 3 { format!(" +{}", c.insts.len() - 3) } else { String::new() };
                let _ = write!(text, " island {} ({}) rho {:.1e} reach {:.1e} via {list}{more};", c.island, self.islands[c.island].unknowns.len(), c.rho, c.reach);
            }
            line(&mut out, &text);
        }
        line(&mut out, &format!("merges: {}", self.merges.len()));
        for m in &self.merges { line(&mut out, &format!("  {m}")); }
        out
    }

    /// Every searched node's attachments by share, the largest `top` of each.
    pub fn share_table(&self, top: usize) -> String {
        let mut out = String::new();
        for (h, list) in &self.shares {
            let text = list.iter().take(top).map(|(name, v)| format!("{name} {v:.2e}")).collect::<Vec<_>>().join(", ");
            let more = if list.len() > top { format!(" +{} more", list.len() - top) } else { String::new() };
            line(&mut out, &format!("shares {}: {text}{more}", self.names[*h as usize]));
        }
        out
    }

    /// The held node named `name`, if it is one.
    pub fn node(&self, name: &str) -> Option<&HeldNode> {
        self.nodes.iter().find(|h| self.names[h.node as usize] == name)
    }
}
