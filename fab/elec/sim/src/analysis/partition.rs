//! Structural islands and small-signal weak-cut measurements. This does not change a simulation.

use super::ac::AcSweep;
use super::op::operating_point;
use crate::circuit::SimCircuit;
use crate::newton::{DiagKind, SimDiag, Solver};
use crate::num::C64;
use crate::sparse::{Layout, Lu};
use crate::stamp::UnkKind;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

/// A chain a run landed (`Planner::land_chain`), by its born export's name and the coupling its
/// held member read on the tree's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Land {
    pub born: String,
    pub to: String,
}

#[derive(Clone, Debug)]
pub struct PartOpts {
    /// The bar a cut's coupling stays under, everywhere but block 0.
    pub cut_tol: f64,
    /// The bar for a return into a node of block 0 (`held`): a block's loading of a pad, a rail or
    /// a reference is seen there a round late, up to this part of the block's swing; the node's
    /// own level and steps are exact. A tolerance, not a limit.
    pub hub_tol: f64,
    pub fmin: f64,
    pub fmax: f64,
    /// The top of the band the accuracy bars judge, Hz: Fast's delivered band (half the rate the
    /// run's audio leaves at). A point above it is judged for stability alone; its accuracy figure
    /// is reported, gating nothing. At or above `fmax`: every point, as before.
    pub band: f64,
    pub per_octave: u32,
    pub min_island: usize,
    pub control: Vec<String>,
    /// Declared supplies (`NodeInfo::supply`): consumers there may return their current a round
    /// late, under `hub_tol` (`held`). `round::plan_blocks` adds the circuit's own.
    pub supplies: Vec<String>,
    /// Declared heard nets (`NodeInfo::heard`). `round::plan_blocks` adds the circuit's own.
    /// A run may add more later.
    pub heard: Vec<String>,
    /// The round's length, seconds: how late a cut's late value is read. What reading it late gets
    /// wrong grows with it (`held`'s accuracy bar).
    pub round: f64,
    /// Nodes never cut: a run refused them (a value read across one left what the circuit can
    /// hold: `LiveSim`'s growth rule). Each fails its held tests, so its sides merge.
    pub never_cut: Vec<String>,
    /// Chains the run landed after a member held a split past its bar. After the relay rule,
    /// each lands its born pairs and the same-round paths from them to its named coupling.
    pub land: Vec<Land>,
}

#[derive(Clone, Debug)]
pub struct Partition {
    /// Solver unknown indices, with unused ground at zero.
    pub names: Vec<String>,
    pub n: usize,
    pub sources: Vec<u32>,
    pub sinks: Vec<u32>,
    pub islands: Vec<Island>,
    pub cuts: Vec<Cut>,
    pub refused: Vec<Refused>,
    pub merges: Vec<String>,
    /// Every node with passive elements in a block that was searched for cuts, with its
    /// driving-point impedance in the whole block (ohms, the band's maximum), ascending by unknown.
    pub stiffness: Vec<(u32, f64)>,
    pub freqs: Vec<f64>,
    /// Candidates with singular DC matrices use this frequency for their DC sample.
    pub dc_at: Option<f64>,
    cut_tol: f64,
    fmax: f64,
    /// Each unknown's structural neighbours (either direction), for `border_view`.
    coupled: Vec<Vec<u32>>,
}

#[derive(Clone, Debug)]
pub struct Island {
    pub unknowns: Vec<u32>,
    pub stage: u32,
    pub control: bool,
}

#[derive(Clone, Debug)]
pub struct Cut {
    pub node: u32,
    pub stiff: usize,
    pub soft: Vec<usize>,
    pub rho: Vec<f64>,
    pub tau: f64,
    pub stiff_first: bool,
}

#[derive(Clone, Debug)]
pub struct Refused {
    pub node: u32,
    pub reason: String,
}

#[derive(Clone, Copy)]
pub(super) struct Entry {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) value: usize,
}

/// Diagonal seed, then augmenting paths with a look-ahead (MC21): each column on a path first takes
/// a free row of its own when it has one, so a path is as short as the structure allows. Without it
/// a source's branch current would reach a free row the long way round, through other nodes'
/// rows, and leave them matched off their own diagonals. The stack replaces recursive DFS.
fn matching(rows: &[Vec<usize>], names: &[String]) -> Result<Vec<usize>, SimDiag> {
    let n = rows.len() - 1;
    let mut row_of = vec![0; n + 1];
    let mut col_at = vec![0; n + 1];
    for c in 1..=n {
        if rows[c].binary_search(&c).is_ok() {
            row_of[c] = c;
            col_at[c] = c;
        }
    }
    let mut seen = vec![false; n + 1];
    for root in 1..=n {
        if row_of[root] != 0 {
            continue;
        }
        seen.fill(false);
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut free = 0;
        let mut c = root;
        'path: loop {
            stack.push((c, 0));
            // the matching does not change during a search, so a column's rows are all matched
            // once its look-ahead finds none free
            if let Some(&r) = rows[c].iter().find(|&&r| col_at[r] == 0) {
                free = r;
                break 'path;
            }
            loop {
                let Some((top, next)) = stack.last_mut() else { break 'path };
                if *next == rows[*top].len() {
                    stack.pop();
                    continue;
                }
                let r = rows[*top][*next];
                *next += 1;
                if !seen[r] {
                    seen[r] = true;
                    c = col_at[r];
                    continue 'path;
                }
            }
        }
        if free == 0 {
            let name = names[root].clone();
            return Err(SimDiag {
                kind: DiagKind::Singular,
                message: format!("partition: no structural matching for {name}"),
                at: vec![name], time: None, partial: None,
            });
        }
        for (c, _) in stack.into_iter().rev() {
            let old = row_of[c];
            row_of[c] = free;
            col_at[free] = c;
            free = old;
        }
    }
    Ok(row_of)
}

/// Iterative Tarjan; both roots and neighbours are visited in increasing index order.
pub(super) fn components(graph: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = graph.len();
    let mut number = vec![usize::MAX; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut pending = Vec::new();
    let mut out = Vec::new();
    let mut serial = 0;
    for root in 0..n {
        if number[root] != usize::MAX {
            continue;
        }
        number[root] = serial;
        low[root] = serial;
        serial += 1;
        pending.push(root);
        on_stack[root] = true;
        let mut dfs = vec![(root, 0)];
        while let Some((v, next)) = dfs.last_mut() {
            let v = *v;
            if *next < graph[v].len() {
                let w = graph[v][*next];
                *next += 1;
                if number[w] == usize::MAX {
                    number[w] = serial;
                    low[w] = serial;
                    serial += 1;
                    pending.push(w);
                    on_stack[w] = true;
                    dfs.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(number[w]);
                }
                continue;
            }
            dfs.pop();
            if let Some(&(parent, _)) = dfs.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == number[v] {
                let mut part = Vec::new();
                loop {
                    let w = pending.pop().unwrap();
                    on_stack[w] = false;
                    part.push(w);
                    if w == v { break; }
                }
                part.sort_unstable();
                out.push(part);
            }
        }
    }
    out.sort_unstable_by_key(|p| p[0]);
    out
}

pub(super) fn sort_graph(graph: &mut [Vec<usize>]) {
    for neighbours in graph {
        neighbours.sort_unstable();
        neighbours.dedup();
    }
}

fn ac_values(s: &Solver, f: f64) -> Vec<C64> {
    let w = 2.0 * std::f64::consts::PI * f;
    let mut vals: Vec<_> = s.g.iter().zip(&s.c).map(|(&g, &c)| C64::new(g, w * c)).collect();
    for bank in &s.banks {
        bank.ac_stamp(w, &mut vals);
    }
    vals
}

pub(super) struct LocalMatrix {
    lu: Lu<C64>,
    vals: Vec<C64>,
    copy: Vec<(usize, usize)>,
}

impl LocalMatrix {
    pub(super) fn new(nodes: &[usize], row_of: &[usize], entries: &[Entry]) -> Self {
        Self::new_in(nodes, row_of, entries, &mut IslandMaps::default())
    }

    /// `new`, finding the island's entries through `maps`.
    fn new_in(nodes: &[usize], row_of: &[usize], entries: &[Entry], maps: &mut IslandMaps) -> Self {
        maps.set(nodes, row_of);
        let mut coords = Vec::new();
        let mut values = Vec::new();
        for e in entries {
            if maps.rows[e.row] != usize::MAX && maps.cols[e.col] != usize::MAX {
                coords.push((maps.rows[e.row] as u32, maps.cols[e.col] as u32));
                values.push(e.value);
            }
        }
        maps.clear(nodes, row_of);
        let (layout, map) = Layout::build(nodes.len(), &coords, None);
        let copy = values.into_iter().zip(map.into_iter().map(|v| v as usize)).collect();
        Self { lu: layout.lu::<C64>(), vals: vec![C64::ZERO_C; layout.len()], copy }
    }

    /// Loads the island's values from the parent's and factors them; false when singular.
    pub(super) fn load(&mut self, parent: &[C64]) -> bool {
        self.load_less(parent, 0, C64::ZERO_C)
    }

    /// As `load`, with `less` taken off the parent's value `at` (a consumer's own terms at a
    /// held node, counted on the other side instead).
    fn load_less(&mut self, parent: &[C64], at: usize, less: C64) -> bool {
        self.vals.fill(C64::ZERO_C);
        for &(from, to) in &self.copy {
            self.vals[to] += parent[from];
            if from == at && at != 0 {
                self.vals[to] -= less;
            }
        }
        self.lu.factor(&self.vals).is_ok()
    }

    /// Solves with the loaded factors; false when the solution is not finite.
    pub(super) fn solve_loaded(&mut self, rhs: &mut [C64]) -> bool {
        self.lu.solve(rhs);
        rhs.iter().all(|z| z.re.is_finite() && z.im.is_finite())
    }

    /// As `solve_loaded`, with the transpose (`A^T x = b`): `b` by column, `x` by row. A unit at
    /// a node's column gives the row of `Z` at it: how far a current into each row moves the node.
    fn solve_loaded_transposed(&mut self, rhs: &mut [C64]) -> bool {
        self.lu.solve_transposed(rhs);
        rhs.iter().all(|z| z.re.is_finite() && z.im.is_finite())
    }

    fn solve(&mut self, parent: &[C64], rhs: &mut [C64]) -> bool {
        self.load(parent) && self.solve_loaded(rhs)
    }
}

/// What a weak-cut test measured: per frequency the coupling `rho`, over the band `tau`, the soft
/// island whose own share of `rho` is largest and each soft island's share (the band's maximum).
#[derive(Clone)]
pub(super) struct Measured {
    pub(super) rho: Vec<f64>,
    pub(super) tau: f64,
    /// The soft islands' largest voltage response to a volt at the node, over the band (V/V).
    pub(super) gain: f64,
    pub(super) worst: usize,
    pub(super) shares: Vec<f64>,
    /// Per soft island, per frequency: `|Y'_j|`, the admittance it reflects into the node alone.
    pub(super) ys: Vec<Vec<f64>>,
    /// Per frequency: `|Z_S,ss|`, the stiff island's driving-point impedance at the node, its
    /// consumers' own terms off; a soft island's part of `rho` is this times its `ys`.
    pub(super) zs: Vec<f64>,
    /// Per frequency, before the band's maximum: `gain`'s term.
    pub(super) gains: Vec<f64>,
    /// Per frequency: how far the stiff side moves the node by anything but this cut, per volt at
    /// its imports (`NodeTest::imports`), at most 1: the row of `Z_S` at the node through each
    /// import's entries in the stiff side's rows, summed. 0 with no imports.
    pub(super) owns: Vec<f64>,
    /// Per frequency: the stiff side's largest voltage response (the node's included) to the
    /// current the cut's far ends drive into the node, per volt at each of them:
    /// `max_k |Z_S,kh| sum |Y_h,far|`. What reading its load late costs the holder going forward, the
    /// consumers fully active.
    pub(super) drives: Vec<f64>,
}

/// Per import of the stiff side `stiff` (`NodeTest::imports`): its entries in the side's rows, as
/// (value, local row).
fn import_entries(stiff: &[usize], imports: &[usize], row_of: &[usize], entries: &[Entry], maps: &mut IslandMaps) -> Vec<Vec<(usize, usize)>> {
    let mut out = vec![Vec::new(); imports.len()];
    if imports.is_empty() {
        return out;
    }
    maps.set(stiff, row_of);
    for e in entries.iter().filter(|e| maps.rows[e.row] != usize::MAX) {
        if let Ok(i) = imports.binary_search(&e.col) {
            out[i].push((e.value, maps.rows[e.row]));
        }
    }
    maps.clear(stiff, row_of);
    out
}

/// Where each unknown of one island sits in it, by its row and by its column, over all the
/// circuit's unknowns (`usize::MAX` outside it). One pair serves island after island: `set` marks
/// an island's unknowns and `clear` unmarks just those, so the pair is filled whole only when it
/// grows or a use was cut short (a panic between the two leaves it `dirty`).
#[derive(Default)]
struct IslandMaps {
    rows: Vec<usize>,
    cols: Vec<usize>,
    dirty: bool,
}

impl IslandMaps {
    fn set(&mut self, nodes: &[usize], row_of: &[usize]) {
        if self.dirty {
            self.rows.fill(usize::MAX);
            self.cols.fill(usize::MAX);
        }
        if self.rows.len() < row_of.len() {
            self.rows.resize(row_of.len(), usize::MAX);
            self.cols.resize(row_of.len(), usize::MAX);
        }
        self.dirty = true;
        for (i, &u) in nodes.iter().enumerate() {
            self.rows[row_of[u]] = i;
            self.cols[u] = i;
        }
    }

    fn clear(&mut self, nodes: &[usize], row_of: &[usize]) {
        for &u in nodes {
            self.rows[row_of[u]] = usize::MAX;
            self.cols[u] = usize::MAX;
        }
        self.dirty = false;
    }
}

/// `own` at a point: from the factored stiff side, the row of `Z_S` at the node (`local_s`)
/// through each import's entries, summed over the imports, at most 1; 1 when not finite. `r` is
/// where it solves, cleared first.
fn own_point(matrix: &mut LocalMatrix, len: usize, local_s: usize, imports: &[Vec<(usize, usize)>], parent: &[C64], r: &mut Vec<C64>) -> f64 {
    if imports.is_empty() {
        return 0.0;
    }
    r.clear();
    r.resize(len, C64::ZERO_C);
    r[local_s] = C64::new(1.0, 0.0);
    if !matrix.solve_loaded_transposed(r) {
        return 1.0;
    }
    let sum: f64 = imports.iter().map(|list| list.iter().fold(C64::ZERO_C, |s, &(value, row)| s + r[row] * parent[value]).norm()).sum();
    if sum.is_finite() { sum.min(1.0) } else { 1.0 }
}

/// The weak-cut test at `node` (step I1): `stiff` is the island holding it (node included), `soft`
/// the islands across its cut elements. Each island is its own matrix, the cut elements' own terms
/// kept on both sides (the ghosts). Per frequency `rho = |Z_S,ss * Y'|`, `Y'` the admittance the
/// soft islands reflect back into the node; `tau` (over the band) is the largest response of the
/// stiff island's other voltages to that reflected current, `max |Z_S,ks| * |Y'|`: what reading its
/// load a round late costs the stiff side. `gain` (over the band) is the soft islands' largest
/// voltage response to a volt at the node: what reading the node a round late costs them. `moved` (per
/// soft island, per frequency; empty for none) is its instances' own terms at the node that have
/// no ghost on the stiff side: taken off the node's diagonal (value `diag`) and counted in that
/// island's part of `Y'`. `imports` (`NodeTest::imports`) give `Measured::owns`. `Err(k)`: a
/// matrix is singular at frequency `k`.
#[allow(clippy::too_many_arguments)]
pub(super) fn measure(
    node: usize, stiff: &[usize], soft: &[Vec<usize>], row_of: &[usize], entries: &[Entry],
    voltage: &[bool], band: &[Vec<C64>], dc: Option<&[C64]>, moved: &[Vec<C64>], diag: usize, imports: &[usize],
) -> Result<Measured, usize> {
    let at = |k: usize| if k == 0 { dc.unwrap_or(&band[k]) } else { &band[k] };
    let own = |j: usize, k: usize| moved.get(j).and_then(|m| m.get(k)).copied().unwrap_or(C64::ZERO_C);
    let less = |k: usize| (0..soft.len()).fold(C64::ZERO_C, |sum, j| sum + own(j, k));
    let local_s = stiff.binary_search(&node).expect("the stiff island holds its node");
    let mut matrix = LocalMatrix::new(stiff, row_of, entries);
    let imports = import_entries(stiff, imports, row_of, entries, &mut IslandMaps::default());
    let mut r = Vec::new();
    let mut zs = Vec::with_capacity(band.len());
    let mut peaks = Vec::with_capacity(band.len());
    let mut owns = Vec::with_capacity(band.len());
    let mut fars = vec![0.0f64; band.len()];
    let mut z = vec![C64::ZERO_C; stiff.len()];
    for k in 0..band.len() {
        z.fill(C64::ZERO_C);
        z[local_s] = C64::new(1.0, 0.0);
        if !(matrix.load_less(at(k), diag, less(k)) && matrix.solve_loaded(&mut z)) { return Err(k); }
        zs.push(z[local_s]);
        let peak = stiff.iter().zip(&z)
            .filter(|&(&u, _)| u != node && voltage[u])
            .map(|(_, z)| z.norm()).fold(0.0, f64::max);
        peaks.push(peak);
        owns.push(own_point(&mut matrix, stiff.len(), local_s, &imports, at(k), &mut r));
    }
    let mut yp = vec![C64::ZERO_C; band.len()];
    let (mut worst, mut worst_share) = (0, -1.0f64);
    let mut shares = Vec::with_capacity(soft.len());
    let mut ys = Vec::with_capacity(soft.len());
    let mut gain = 0.0f64;
    let mut gains = vec![0.0f64; band.len()];
    for (j, part) in soft.iter().enumerate() {
        let mut matrix = LocalMatrix::new(part, row_of, entries);
        let mut rows = vec![usize::MAX; row_of.len()];
        let mut cols = vec![usize::MAX; row_of.len()];
        for (j, &u) in part.iter().enumerate() {
            rows[row_of[u]] = j;
            cols[u] = j;
        }
        let input: Vec<_> = entries.iter().filter(|e| e.col == node && rows[e.row] != usize::MAX)
            .map(|e| (e.value, rows[e.row])).collect();
        let output: Vec<_> = entries.iter().filter(|e| e.row == node && cols[e.col] != usize::MAX)
            .map(|e| (e.value, cols[e.col])).collect();
        let mut v = vec![C64::ZERO_C; part.len()];
        let mut share = 0.0f64;
        let mut y_j = Vec::with_capacity(band.len());
        for k in 0..band.len() {
            let parent = at(k);
            v.fill(C64::ZERO_C);
            for &(value, row) in &input { v[row] -= parent[value]; }
            if !matrix.solve(parent, &mut v) { return Err(k); }
            for (&u, value) in part.iter().zip(&v) {
                if voltage[u] {
                    gain = gain.max(value.norm());
                    gains[k] = gains[k].max(value.norm());
                }
            }
            let mut y = own(j, k);
            for &(value, col) in &output { y += parent[value] * v[col]; }
            fars[k] += output.iter().map(|&(value, _)| parent[value].norm()).sum::<f64>();
            yp[k] += y;
            y_j.push(y.norm());
            share = share.max((zs[k] * y).norm());
        }
        ys.push(y_j);
        if share > worst_share {
            (worst, worst_share) = (j, share);
        }
        shares.push(share);
    }
    let mut rho = Vec::with_capacity(band.len());
    let mut tau = 0.0f64;
    for k in 0..band.len() {
        let r = (zs[k] * yp[k]).norm();
        let t = peaks[k] * yp[k].norm();
        if !r.is_finite() || !t.is_finite() { return Err(k); }
        rho.push(r);
        tau = tau.max(t);
    }
    let drives = (0..band.len()).map(|k| peaks[k].max(zs[k].norm()) * fars[k]).collect();
    Ok(Measured { rho, tau, gain, worst, shares, ys, zs: zs.iter().map(|z| z.norm()).collect(), gains, owns, drives })
}

/// One weak-cut test: `measure`'s arguments, the band's own DC point.
#[derive(Clone)]
pub(super) struct NodeTest {
    pub(super) node: usize,
    pub(super) stiff: Vec<usize>,
    pub(super) soft: Vec<Vec<usize>>,
    pub(super) moved: Vec<Vec<C64>>,
    pub(super) diag: usize,
    /// The stiff side's imports that can move the node: the voltages outside it its rows read,
    /// sorted, but for ground, the cut's own far ends (in `soft`), a declared supply and a node a
    /// fixed source holds (`Measured::owns`).
    pub(super) imports: Vec<usize>,
}

/// `measure` for each of `tests` on the band's own DC point, bit for bit, with the matrices they
/// share factored once at each point and solved for each test that has them: every soft island,
/// and a stiff island where nothing comes off the node (its consumers' own terms add to zero at
/// every point, so its values are the island's own). A matrix is factored at the points in their
/// order, as `measure` factors its own, and each test's sums are taken in `measure`'s order. It
/// works in this thread's `Scratch`.
pub(super) fn measure_each(tests: &[NodeTest], row_of: &[usize], entries: &[Entry], voltage: &[bool], band: &[Vec<C64>]) -> Vec<Result<Measured, usize>> {
    with_scratch(|s| measure_each_in(s, tests, row_of, entries, voltage, band))
}

/// What `measure_each` works in, kept by each thread that measures (a pool's worker, a helper or
/// the caller) and cleared for each call and each solve instead of made again, at the largest
/// sizes it has needed: the solves' right-hand sides, the island maps, the per-test terms and a
/// state's band (`held::measure_state`).
#[derive(Default)]
pub(super) struct Scratch {
    /// A solve's right-hand side and then its solution: a stiff side's column of `Z` at its node
    /// and then its row there (`own_point`), or a consumer island's response to the node.
    rhs: Vec<C64>,
    maps: IslandMaps,
    /// Per test and point (test `i`'s from `i * points`): the stiff side's response at the node,
    /// its largest at its other voltages, and the consumers' entries at the node summed.
    zs: Vec<C64>,
    peaks: Vec<f64>,
    fars: Vec<f64>,
    /// Per test, where its consumer islands start in `soft_err` (each one's first failure) and,
    /// times the points, in `ys` (each one's part of `Y'` at each point).
    islands: Vec<usize>,
    soft_err: Vec<Option<usize>>,
    ys: Vec<C64>,
    /// `Y'` at each point, of one test at a time.
    yp: Vec<C64>,
    /// A state's band and DC fallback values, made again in place for each state.
    pub(super) band: Vec<Vec<C64>>,
    pub(super) fallback: Vec<C64>,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default());
}

/// Runs `f` on this thread's `Scratch`, or on a fresh one while that is in use.
pub(super) fn with_scratch<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    SCRATCH.with(|s| match s.try_borrow_mut() {
        Ok(mut s) => f(&mut s),
        Err(_) => f(&mut Scratch::default()),
    })
}

/// `measure_each` in `s`.
pub(super) fn measure_each_in(s: &mut Scratch, tests: &[NodeTest], row_of: &[usize], entries: &[Entry], voltage: &[bool], band: &[Vec<C64>]) -> Vec<Result<Measured, usize>> {
    let Scratch { rhs, maps, zs, peaks, fars, islands, soft_err, ys, yp, .. } = s;
    let points = band.len();
    let voltage = |u: usize| voltage[u];
    // the matrices: an island's own values (shared by its members' list), or a stiff island's with
    // what comes off its node at each point (that test's alone)
    let mut mats: Vec<(LocalMatrix, Option<(usize, Vec<C64>)>)> = Vec::new();
    let mut shared: HashMap<&[usize], usize> = HashMap::new();
    let mut stiff_of = Vec::with_capacity(tests.len());
    let mut soft_of: Vec<Vec<usize>> = Vec::with_capacity(tests.len());
    for t in tests {
        let own_at = |j: usize, k: usize| t.moved.get(j).and_then(|m| m.get(k)).copied().unwrap_or(C64::ZERO_C);
        let off = |k: usize| (0..t.soft.len()).fold(C64::ZERO_C, |sum, j| sum + own_at(j, k));
        if (0..points).all(|k| off(k).re.to_bits() == 0 && off(k).im.to_bits() == 0) {
            stiff_of.push(*shared.entry(&t.stiff[..]).or_insert_with(|| {
                mats.push((LocalMatrix::new_in(&t.stiff, row_of, entries, maps), None));
                mats.len() - 1
            }));
        } else {
            mats.push((LocalMatrix::new_in(&t.stiff, row_of, entries, maps), Some((t.diag, (0..points).map(off).collect()))));
            stiff_of.push(mats.len() - 1);
        }
        soft_of.push(t.soft.iter().map(|part| *shared.entry(&part[..]).or_insert_with(|| {
            mats.push((LocalMatrix::new_in(part, row_of, entries, maps), None));
            mats.len() - 1
        })).collect());
    }
    // who solves with each matrix: a test's stiff side (None) or its soft island `j`
    let mut users: Vec<Vec<(usize, Option<usize>)>> = vec![Vec::new(); mats.len()];
    for (i, (&stiff, soft)) in stiff_of.iter().zip(&soft_of).enumerate() {
        users[stiff].push((i, None));
        for (j, &m) in soft.iter().enumerate() {
            users[m].push((i, Some(j)));
        }
    }
    // per test: where its node is in its stiff island, each soft island's reads of the node and
    // what the node reads of it (`measure`'s `input` and `output`)
    let local_s: Vec<usize> = tests.iter().map(|t| t.stiff.binary_search(&t.node).expect("the stiff island holds its node")).collect();
    let imports: Vec<Vec<Vec<(usize, usize)>>> = tests.iter().map(|t| import_entries(&t.stiff, &t.imports, row_of, entries, maps)).collect();
    let mut io: Vec<Vec<(Vec<(usize, usize)>, Vec<(usize, usize)>)>> = Vec::with_capacity(tests.len());
    for t in tests {
        io.push(t.soft.iter().map(|part| {
            maps.set(part, row_of);
            let input = entries.iter().filter(|e| e.col == t.node && maps.rows[e.row] != usize::MAX).map(|e| (e.value, maps.rows[e.row])).collect();
            let output = entries.iter().filter(|e| e.row == t.node && maps.cols[e.col] != usize::MAX).map(|e| (e.value, maps.cols[e.col])).collect();
            maps.clear(part, row_of);
            (input, output)
        }).collect());
    }
    // per test: its stiff side's responses and first failure, each soft island's terms and first
    // failure, and the soft islands' largest response
    let n = tests.len() * points;
    zs.clear();
    zs.resize(n, C64::ZERO_C);
    peaks.clear();
    peaks.resize(n, 0.0);
    fars.clear();
    fars.resize(n, 0.0);
    islands.clear();
    islands.extend(tests.iter().scan(0, |at, t| {
        *at += t.soft.len();
        Some(*at - t.soft.len())
    }));
    let all = tests.iter().map(|t| t.soft.len()).sum::<usize>();
    soft_err.clear();
    soft_err.resize(all, None);
    ys.clear();
    ys.resize(all * points, C64::ZERO_C);
    let mut stiff_err: Vec<Option<usize>> = vec![None; tests.len()];
    let mut gain = vec![0.0f64; tests.len()];
    let mut gains: Vec<Vec<f64>> = tests.iter().map(|_| vec![0.0; points]).collect();
    let mut owns: Vec<Vec<f64>> = tests.iter().map(|_| vec![0.0; points]).collect();
    for k in 0..points {
        let parent = &band[k];
        for (m, (matrix, off)) in mats.iter_mut().enumerate() {
            let loaded = match off {
                None => matrix.load(parent),
                Some((diag, off)) => matrix.load_less(parent, *diag, off[k]),
            };
            for &(i, side) in &users[m] {
                let t = &tests[i];
                let at = i * points + k;
                match side {
                    None => {
                        if stiff_err[i].is_some() {
                            continue;
                        }
                        let z = &mut *rhs;
                        z.clear();
                        z.resize(t.stiff.len(), C64::ZERO_C);
                        z[local_s[i]] = C64::new(1.0, 0.0);
                        if !(loaded && matrix.solve_loaded(z)) {
                            stiff_err[i] = Some(k);
                            continue;
                        }
                        zs[at] = z[local_s[i]];
                        peaks[at] = t.stiff.iter().zip(z.iter()).filter(|&(&u, _)| u != t.node && voltage(u)).map(|(_, z)| z.norm()).fold(0.0, f64::max);
                        owns[i][k] = own_point(matrix, t.stiff.len(), local_s[i], &imports[i], parent, z);
                    }
                    Some(j) => {
                        let isl = islands[i] + j;
                        if soft_err[isl].is_some() {
                            continue;
                        }
                        let (input, output) = &io[i][j];
                        let v = &mut *rhs;
                        v.clear();
                        v.resize(t.soft[j].len(), C64::ZERO_C);
                        for &(value, row) in input {
                            v[row] -= parent[value];
                        }
                        if !(loaded && matrix.solve_loaded(v)) {
                            soft_err[isl] = Some(k);
                            continue;
                        }
                        for (&u, value) in t.soft[j].iter().zip(v.iter()) {
                            if voltage(u) {
                                gain[i] = gain[i].max(value.norm());
                                gains[i][k] = gains[i][k].max(value.norm());
                            }
                        }
                        let mut y = t.moved.get(j).and_then(|m| m.get(k)).copied().unwrap_or(C64::ZERO_C);
                        for &(value, col) in output {
                            y += parent[value] * v[col];
                        }
                        fars[at] += output.iter().map(|&(value, _)| parent[value].norm()).sum::<f64>();
                        ys[isl * points + k] = y;
                    }
                }
            }
        }
    }
    tests.iter().enumerate().map(|(i, t)| {
        if let Some(k) = stiff_err[i] {
            return Err(k);
        }
        let isl = islands[i];
        if let Some(k) = soft_err[isl..isl + t.soft.len()].iter().find_map(|e| *e) {
            return Err(k);
        }
        let (zs, peaks, fars) = (&zs[i * points..][..points], &peaks[i * points..][..points], &fars[i * points..][..points]);
        yp.clear();
        yp.resize(points, C64::ZERO_C);
        let (mut worst, mut worst_share) = (0, -1.0f64);
        let mut shares = Vec::with_capacity(t.soft.len());
        let mut norms = Vec::with_capacity(t.soft.len());
        for (j, y) in ys[isl * points..][..t.soft.len() * points].chunks_exact(points).enumerate() {
            let mut share = 0.0f64;
            for k in 0..points {
                yp[k] += y[k];
                share = share.max((zs[k] * y[k]).norm());
            }
            norms.push(y.iter().map(|y| y.norm()).collect());
            if share > worst_share {
                (worst, worst_share) = (j, share);
            }
            shares.push(share);
        }
        let mut rho = Vec::with_capacity(points);
        let mut tau = 0.0f64;
        for k in 0..points {
            let r = (zs[k] * yp[k]).norm();
            let a = peaks[k] * yp[k].norm();
            if !r.is_finite() || !a.is_finite() {
                return Err(k);
            }
            rho.push(r);
            tau = tau.max(a);
        }
        let drives = (0..points).map(|k| peaks[k].max(zs[k].norm()) * fars[k]).collect();
        Ok(Measured { rho, tau, gain: gain[i], worst, shares, ys: norms, zs: zs.iter().map(|z| z.norm()).collect(), gains: std::mem::take(&mut gains[i]), owns: std::mem::take(&mut owns[i]), drives })
    }).collect()
}

struct PendingCut {
    node: usize,
    /// The far ends of its cut elements, in the soft islands.
    soft: Vec<usize>,
    rho: Vec<f64>,
    tau: f64,
}

/// Union-find over unknowns; the root of a set is its smallest member.
pub(super) struct Sets(pub(super) Vec<usize>);

impl Sets {
    pub(super) fn find(&self, mut i: usize) -> usize {
        while self.0[i] != i { i = self.0[i]; }
        i
    }

    pub(super) fn union(&mut self, a: usize, b: usize) -> bool {
        let (a, b) = (self.find(a), self.find(b));
        if a == b { return false; }
        let (lo, hi) = (a.min(b), a.max(b));
        self.0[hi] = lo;
        true
    }
}

/// How two unknowns of one exact block meet in the matrix.
#[derive(Clone, Copy, PartialEq)]
enum Pair {
    /// Both read each other only through resistors and capacitors, between two node voltages on
    /// their own KCL rows: a candidate cut element.
    Passive,
    /// Only `reader` reads `producer`: an exact same-round dependency.
    OneWay { producer: usize, reader: usize },
    /// Anything else (a device's own terminals, a nonlinear or controlled coupling): they stay
    /// together.
    Strong,
}

/// Nodes whose driving-point impedance stays under this many ohms over the band are listed as
/// stiff in the report (amplifier outputs, virtual grounds, stiff rails). It only sorts the report.
const STIFF_OHMS: f64 = 100.0;

pub(super) fn peak(values: &[f64]) -> (usize, f64) {
    let mut best = (0, 0.0);
    for (k, &v) in values.iter().enumerate() {
        if v > best.1 { best = (k, v); }
    }
    best
}

pub(super) fn hz(f: f64) -> String {
    if f >= 1e3 { format!("{:.1e} kHz", f / 1e3) } else { format!("{f:.1e} Hz") }
}

#[derive(Clone, Copy)]
pub(super) struct Edge {
    pub(super) from: usize,
    pub(super) to: usize,
    pub(super) exact: bool,
    pub(super) same: bool,
}

pub(super) struct Groups(pub(super) Vec<usize>);

impl Groups {
    pub(super) fn root(&self, mut i: usize) -> usize {
        while self.0[i] != i { i = self.0[i]; }
        i
    }

    pub(super) fn merge(&mut self, ids: &[usize], reason: &str, log: &mut Vec<String>) {
        if ids.len() < 2 { return; }
        let first = ids[0];
        for &i in &ids[1..] { self.0[i] = first; }
        let names = ids.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
        log.push(format!("islands {names} -> {first}: {reason}"));
    }

    fn graph(&self, edges: &[Edge]) -> Vec<Vec<usize>> {
        let mut graph = vec![Vec::new(); self.0.len()];
        for e in edges.iter().filter(|e| e.same) {
            let (a, b) = (self.root(e.from), self.root(e.to));
            if a != b { graph[a].push(b); }
        }
        sort_graph(&mut graph);
        graph
    }
}

/// Merges every same-round cycle among the groups into one, until none is left: a merge (the
/// control island's, a small island's) can close a new one.
pub(super) fn condense(groups: &mut Groups, edges: &[Edge], merges: &mut Vec<String>) {
    loop {
        let cycles: Vec<_> = components(&groups.graph(edges)).into_iter().filter(|c| c.len() > 1).collect();
        if cycles.is_empty() {
            return;
        }
        for cycle in cycles {
            groups.merge(&cycle, "same-round cycle", merges);
        }
    }
}

/// Each named control node, by unknown.
pub(super) fn control_set(s: &Solver, control: &[String]) -> Result<Vec<bool>, SimDiag> {
    let mut set = vec![false; s.n + 1];
    for name in control {
        let u = s.unknown(name).ok_or_else(|| SimDiag::setup(format!("partition: no control node named {name}")))?;
        set[u as usize] = true;
    }
    Ok(set)
}

/// The matrix's structure over the given entries: each row's columns, the matching of unknowns
/// to rows, who reads whom, the sources, the sinks and the exact blocks.
pub(super) struct Structure {
    pub(super) names: Vec<String>,
    /// Per row, the columns it reads.
    pub(super) cols: Vec<Vec<usize>>,
    pub(super) row_of: Vec<usize>,
    /// Per unknown, the unknowns whose matched rows read it.
    pub(super) graph: Vec<Vec<usize>>,
    pub(super) coupled: Vec<Vec<u32>>,
    pub(super) sources: Vec<u32>,
    pub(super) sinks: Vec<u32>,
    pub(super) exact: Vec<Vec<usize>>,
    pub(super) block_of: Vec<usize>,
}

pub(super) fn structure(s: &Solver, entries: &[Entry]) -> Result<Structure, SimDiag> {
    let names: Vec<_> = s.unknowns.iter().map(|u| u.name.clone()).collect();
    let mut rows = vec![Vec::new(); s.n + 1];
    let mut cols = vec![Vec::new(); s.n + 1];
    for e in entries {
        rows[e.col].push(e.row);
        cols[e.row].push(e.col);
    }
    sort_graph(&mut rows);
    sort_graph(&mut cols);
    let row_of = matching(&rows, &names)?;
    let mut graph = vec![Vec::new(); s.n + 1];
    for c in 1..=s.n {
        for &producer in &cols[row_of[c]] {
            if producer != c { graph[producer].push(c); }
        }
    }
    sort_graph(&mut graph);
    let mut coupled = vec![Vec::new(); s.n + 1];
    for (u, readers) in graph.iter().enumerate() {
        for &w in readers {
            coupled[u].push(w as u32);
            coupled[w].push(u as u32);
        }
    }
    for list in &mut coupled {
        list.sort_unstable();
        list.dedup();
    }
    let mut sources = Vec::new();
    let mut sinks = Vec::new();
    let mut exact = Vec::new();
    let mut block_of = vec![usize::MAX; s.n + 1];
    let blocks: Vec<_> = components(&graph).into_iter().filter(|b| b[0] != 0).collect();
    // A sink is a branch current that only other sinks read: a source's current is read by the
    // KCL row of its other terminal, and that row is matched to the current of the source holding
    // that node (a VCVS's current is read by the reference source's row).
    let mut sink = vec![false; s.n + 1];
    loop {
        let mut changed = false;
        for block in blocks.iter().filter(|b| b.len() == 1) {
            let u = block[0];
            if !sink[u] && s.unknowns[u].kind == UnkKind::Current && graph[u].iter().all(|&w| sink[w]) {
                sink[u] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for block in blocks {
        let u = block[0];
        if block.len() == 1 && cols[row_of[u]].len() == 1 {
            sources.push(u as u32);
        } else if sink[u] {
            sinks.push(u as u32);
        } else {
            for &u in &block { block_of[u] = exact.len(); }
            exact.push(block);
        }
    }
    Ok(Structure { names, cols, row_of, graph, coupled, sources, sinks, exact, block_of })
}

/// Measure a partition using the same operating-point linearisation and AC stamps as `ac()`.
pub fn partition(ckt: &SimCircuit, o: &PartOpts) -> Result<Partition, SimDiag> {
    if !o.cut_tol.is_finite() || o.cut_tol <= 0.0 || !o.fmin.is_finite() || o.fmin <= 0.0
        || !o.fmax.is_finite() || o.fmax < o.fmin || o.band.is_nan() || o.band < o.fmin || o.per_octave == 0 || o.min_island == 0
        || o.fmin / 100.0 == 0.0
    {
        return Err(SimDiag::setup("partition: need positive finite cut_tol, fmin, fmax >= fmin, band >= fmin (infinity allowed), per_octave and min_island".into()));
    }
    let mut ckt = ckt.clone();
    ckt.opts.force_dense = Some(false);
    let mut s = Solver::new(&ckt)?;
    operating_point(&mut s, false)?;
    let control = control_set(&s, &o.control)?;
    let entries: Vec<_> = (1..s.g.len()).filter_map(|value| {
        let (row, col) = (s.slot_row[value] as usize, s.slot_col[value] as usize);
        (row != 0 && col != 0).then_some(Entry { row, col, value })
    }).collect();
    let Structure { names, cols, row_of, graph, coupled, sources, sinks, exact, block_of } = structure(&s, &entries)?;
    let mut freqs = vec![0.0];
    freqs.extend(AcSweep::Oct { n: o.per_octave, fstart: o.fmin, fstop: o.fmax }.freqs());
    freqs.extend([1e3, 5e3, 2e4].into_iter().filter(|&f| f <= o.fmax));
    freqs.sort_by(f64::total_cmp);
    freqs.dedup();
    let band: Vec<_> = freqs.iter().map(|&f| ac_values(&s, f)).collect();
    let voltage: Vec<bool> = s.unknowns.iter().map(|u| u.kind == UnkKind::Voltage).collect();
    let mut fallback = None;
    let mut dc_at = None;
    let mut blocked = vec![None; s.n + 1];
    for sw in &ckt.devices.sw {
        for u in [sw.p, sw.n] {
            blocked[u as usize] = Some("a switch terminal: its stiffness depends on the switch's state");
        }
    }
    for pin in &ckt.devices.gpio {
        blocked[pin.pad as usize].get_or_insert("a GPIO pad");
    }
    for cell in &ckt.devices.cell {
        blocked[cell.out as usize].get_or_insert("a timing cell's output: its stiffness depends on the cell's state");
    }
    for (u, &is_control) in control.iter().enumerate() {
        if is_control { blocked[u].get_or_insert("in the control set"); }
    }
    let mut refused: Vec<Option<String>> = vec![None; s.n + 1];
    // only resistors and capacitors may carry a cut: their weakness at the operating point holds
    // at every point
    let mut passive_value = vec![true; s.g.len()];
    for b in s.banks.iter_mut() {
        if !matches!(b.kind(), "R" | "C") {
            for &v in b.slots().iter() { passive_value[v as usize] = false; }
        }
    }
    let value_at: HashMap<(usize, usize), usize> = entries.iter().map(|e| ((e.row, e.col), e.value)).collect();
    let own = |u: usize| s.unknowns[u].kind == UnkKind::Voltage && row_of[u] == u;
    let mut sets = Sets((0..=s.n).collect());
    let mut stiffness = Vec::new();
    let mut merges = Vec::new();
    let mut pending = Vec::new();
    let mut cut_pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    for block in &exact {
        if block.len() <= o.min_island {
            for &u in &block[1..] { sets.union(block[0], u); }
            continue;
        }
        // every pair of the block's unknowns that meet, by how they meet
        let mut pairs: BTreeMap<(usize, usize), Pair> = BTreeMap::new();
        for &i in block {
            for &j in &cols[row_of[i]] {
                if j == i || block_of[j] != block_of[i] { continue; }
                let key = (i.min(j), i.max(j));
                if pairs.contains_key(&key) { continue; }
                let (a, b) = (value_at.get(&(row_of[i], j)).copied(), value_at.get(&(row_of[j], i)).copied());
                let pair = match (a, b) {
                    (Some(a), Some(b)) if own(i) && own(j) && passive_value[a] && passive_value[b] => Pair::Passive,
                    (Some(_), Some(_)) => Pair::Strong,
                    (Some(_), None) => Pair::OneWay { producer: j, reader: i },
                    (None, Some(_)) => Pair::OneWay { producer: i, reader: j },
                    (None, None) => unreachable!("a pair comes from an entry"),
                };
                pairs.insert(key, pair);
            }
        }
        // each node with passive elements: its driving-point impedance in the whole block, the
        // band's maximum
        let mut passive_of: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (&(i, j), &pair) in &pairs {
            if pair == Pair::Passive {
                passive_of.entry(i).or_default().push(j);
                passive_of.entry(j).or_default().push(i);
            }
        }
        let mut whole = LocalMatrix::new(block, &row_of, &entries);
        let mut zmax: BTreeMap<usize, f64> = passive_of.keys().map(|&u| (u, 0.0)).collect();
        let mut z = vec![C64::ZERO_C; block.len()];
        for (k, values) in band.iter().enumerate() {
            let values = if k == 0 && !whole.load(values) {
                dc_at = Some(o.fmin / 100.0);
                fallback.get_or_insert_with(|| ac_values(&s, o.fmin / 100.0))
            } else {
                values
            };
            if !whole.load(values) { continue; }
            for (&u, r) in zmax.iter_mut() {
                let local = block.binary_search(&u).expect("a block member");
                z.fill(C64::ZERO_C);
                z[local] = C64::new(1.0, 0.0);
                if !whole.solve_loaded(&mut z) { continue; }
                *r = r.max(z[local].norm());
            }
        }
        stiffness.extend(zmax.iter().map(|(&u, &r)| (u as u32, r)));
        // Every node with passive elements may cut them: each element is decided at its stiffer
        // end (the smaller impedance), and the test there keeps the cut or merges.
        let mut candidate = BTreeSet::new();
        for &u in zmax.keys() {
            match blocked[u] {
                Some(reason) => { refused[u].get_or_insert_with(|| reason.to_string()); }
                None => { candidate.insert(u); }
            }
        }
        // islands: everything that meets strongly, and passive elements between blocked nodes
        let mut oneway = Vec::new();
        let mut cuts_here = Vec::new();
        for (&(i, j), &pair) in &pairs {
            match pair {
                Pair::Strong => { sets.union(i, j); }
                Pair::Passive if !candidate.contains(&i) && !candidate.contains(&j) => { sets.union(i, j); }
                Pair::Passive => {
                    let s_node = if !candidate.contains(&j) || (candidate.contains(&i) && zmax[&i] <= zmax[&j]) { i } else { j };
                    cuts_here.push((s_node, if s_node == i { j } else { i }));
                }
                Pair::OneWay { producer, reader } => oneway.push((producer, reader)),
            }
        }
        // Passes: merge same-round cycles, then test every cut node against the islands its cut
        // elements reach (a test whose islands did not change since the last pass is kept); every
        // failed test merges its node's island with those islands. Until a pass changes nothing.
        let mut first: BTreeMap<usize, bool> = BTreeMap::new();
        let mut results: BTreeMap<usize, (Vec<f64>, f64)> = BTreeMap::new();
        let mut tested: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
        let members = |root: usize, sets: &Sets| -> Vec<usize> { block.iter().copied().filter(|&u| sets.find(u) == root).collect() };
        loop {
            let mut changed = false;
            loop {
                let mut graph_i: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
                let add = |a: usize, b: usize, g: &mut BTreeMap<usize, BTreeSet<usize>>| {
                    let (ra, rb) = (sets.find(a), sets.find(b));
                    if ra != rb { g.entry(ra).or_default().insert(rb); }
                };
                for &(p, r) in &oneway { add(p, r, &mut graph_i); }
                for &(sn, a) in &cuts_here {
                    if *first.get(&sn).unwrap_or(&true) { add(sn, a, &mut graph_i); } else { add(a, sn, &mut graph_i); }
                }
                let mut dense = vec![Vec::new(); s.n + 1];
                for (a, bs) in graph_i { dense[a] = bs.into_iter().collect(); }
                let cycles: Vec<_> = components(&dense).into_iter().filter(|c| c.len() > 1).collect();
                if cycles.is_empty() { break; }
                for c in cycles { for &u in &c[1..] { sets.union(c[0], u); } }
                changed = true;
            }
            let nodes: BTreeSet<usize> = cuts_here.iter().filter(|&&(sn, a)| sets.find(sn) != sets.find(a)).map(|&(sn, _)| sn).collect();
            let sizes = |root: usize, sets: &Sets| block.iter().filter(|&&u| sets.find(u) == root).count();
            let mut refusals = Vec::new();
            for &sn in &nodes {
                let root = sets.find(sn);
                let soft_roots: BTreeSet<usize> = cuts_here.iter().filter(|&&(c, a)| c == sn && sets.find(a) != root).map(|&(_, a)| sets.find(a)).collect();
                if soft_roots.is_empty() { continue; }
                let signature: Vec<_> = std::iter::once((root, sizes(root, &sets))).chain(soft_roots.iter().map(|&r| (r, sizes(r, &sets)))).collect();
                if tested.get(&sn) == Some(&signature) { continue; }
                tested.insert(sn, signature);
                let stiff_part = members(root, &sets);
                let soft_parts: Vec<_> = soft_roots.iter().map(|&r| members(r, &sets)).collect();
                let mut result = measure(sn, &stiff_part, &soft_parts, &row_of, &entries, &voltage, &band, None, &[], 0, &[]);
                let mut dc = 0.0;
                if matches!(result, Err(0)) {
                    dc = o.fmin / 100.0;
                    dc_at = Some(dc);
                    let values = fallback.get_or_insert_with(|| ac_values(&s, dc));
                    result = measure(sn, &stiff_part, &soft_parts, &row_of, &entries, &voltage, &band, Some(values), &[], 0, &[]);
                }
                let refuse = match &result {
                    Ok(m) => {
                        let (k, max) = peak(&m.rho);
                        (max >= o.cut_tol).then(|| {
                            let at = if k == 0 && dc == 0.0 { "DC".to_string() } else { hz(if k == 0 { dc } else { freqs[k] }) };
                            let mut also = String::new();
                            for (f, label) in [(0.0, "DC"), (1e3, "1 kHz"), (5e3, "5 kHz"), (2e4, "20 kHz")] {
                                if let Ok(j) = freqs.binary_search_by(|v| v.total_cmp(&f)) {
                                    let _ = write!(also, "{}{label} {:.1e}", if also.is_empty() { "" } else { ", " }, m.rho[j]);
                                }
                            }
                            format!("rho {max:.1e} at {at}, the band's maximum ({also}): its heaviest soft island merged")
                        })
                    }
                    Err(k) => Some(format!("singular at {:.1e} Hz: a soft island merged", if *k == 0 { dc } else { freqs[*k] })),
                };
                if let Some(reason) = refuse {
                    // the soft island that weighs most joins the node's island; the test runs
                    // again on the next pass with the rest
                    let worst = match &result { Ok(m) => m.worst, Err(_) => 0 };
                    let joins = *soft_roots.iter().nth(worst).expect("a soft island");
                    let part = &soft_parts[worst];
                    let names = part.iter().take(6).map(|&u| names[u].as_str()).collect::<Vec<_>>().join(", ");
                    let what = format!("{} unknowns; the soft island's {}: {names}{}", stiff_part.len(), part.len(), if part.len() > 6 { " ..." } else { "" });
                    refusals.push((sn, reason, root, joins, what));
                    continue;
                }
                let Measured { rho, tau, .. } = result.expect("a passed test");
                let stiff_first = tau <= o.cut_tol;
                if first.insert(sn, stiff_first).unwrap_or(true) != stiff_first { changed = true; }
                results.insert(sn, (rho, tau));
            }
            for (sn, reason, root, joins, what) in refusals {
                merges.push(format!("the cut at {} fails ({reason}): the island of {} joins that of {} (stiff island of {what})", names[sn], names[joins], names[root]));
                refused[sn] = Some(reason);
                results.remove(&sn);
                sets.union(root, joins);
                changed = true;
            }
            if !changed { break; }
        }
        for &(sn, a) in &cuts_here {
            if sets.find(sn) != sets.find(a) { cut_pairs.insert((sn, a)); }
        }
        for (sn, (rho, tau)) in results {
            let soft: Vec<_> = cuts_here.iter().filter(|&&(c, a)| c == sn && sets.find(a) != sets.find(sn)).map(|&(_, a)| a).collect();
            if !soft.is_empty() {
                refused[sn] = None;
                pending.push(PendingCut { node: sn, soft, rho, tau });
            }
        }
    }
    let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for block in &exact {
        for &u in block { by_root.entry(sets.find(u)).or_default().push(u); }
    }
    let mut split: Vec<Vec<usize>> = by_root.into_values().collect();
    for part in &mut split { part.sort_unstable(); }
    split.sort_unstable_by_key(|p| p[0]);
    let mut island_of = vec![usize::MAX; s.n + 1];
    for (i, nodes) in split.iter().enumerate() {
        for &u in nodes { island_of[u] = i; }
    }
    // exact edges: every dependency between islands that is not a cut element
    let mut edges = Vec::new();
    for (a, readers) in graph.iter().enumerate().skip(1) {
        for &b in readers {
            if island_of[a] == usize::MAX || island_of[b] == usize::MAX || island_of[a] == island_of[b] { continue; }
            if cut_pairs.contains(&(a, b)) || cut_pairs.contains(&(b, a)) { continue; }
            edges.push(Edge { from: island_of[a], to: island_of[b], exact: true, same: true });
        }
    }
    for cut in &pending {
        for &u in &cut.soft {
            let (stiff, soft) = (island_of[cut.node], island_of[u]);
            let stiff_first = cut.tau <= o.cut_tol;
            edges.push(Edge { from: stiff, to: soft, exact: false, same: stiff_first });
            edges.push(Edge { from: soft, to: stiff, exact: false, same: !stiff_first });
        }
    }
    let mut groups = Groups((0..split.len()).collect());
    condense(&mut groups, &edges, &mut merges);
    let control_groups: BTreeSet<_> = (1..=s.n).filter(|&u| control[u] && island_of[u] != usize::MAX)
        .map(|u| groups.root(island_of[u])).collect();
    groups.merge(&control_groups.into_iter().collect::<Vec<_>>(), "control island", &mut merges);
    condense(&mut groups, &edges, &mut merges);
    loop {
        let mut sizes = vec![0; split.len()];
        for (i, nodes) in split.iter().enumerate() { sizes[groups.root(i)] += nodes.len(); }
        let mut changed = false;
        for (i, &size) in sizes.iter().enumerate() {
            if size == 0 || size >= o.min_island { continue; }
            let mut neighbours = BTreeSet::new();
            for e in edges.iter().filter(|e| e.exact) {
                let (a, b) = (groups.root(e.from), groups.root(e.to));
                if a != b {
                    if a == i { neighbours.insert(b); }
                    if b == i { neighbours.insert(a); }
                }
            }
            if neighbours.len() == 1 {
                let j = *neighbours.first().unwrap();
                groups.merge(&[i.min(j), i.max(j)], "small, merged across an exact cut", &mut merges);
                condense(&mut groups, &edges, &mut merges);
                changed = true;
                break;
            }
        }
        if !changed { break; }
    }
    let mut merged: Vec<Vec<usize>> = vec![Vec::new(); split.len()];
    for (i, nodes) in split.iter().enumerate() { merged[groups.root(i)].extend_from_slice(nodes); }
    let mut islands = Vec::new();
    for nodes in merged.iter_mut().filter(|v| !v.is_empty()) {
        nodes.sort_unstable();
        for &u in nodes.iter() { island_of[u] = islands.len(); }
        islands.push(Island {
            unknowns: nodes.iter().map(|&u| u as u32).collect(), stage: 0,
            control: nodes.iter().any(|&u| control[u]),
        });
    }
    // Each root is the smallest original island index, so this is already ordered by unknown.
    let mut same = vec![Vec::new(); islands.len()];
    for e in edges.iter().filter(|e| e.same) {
        let a = island_of[split[e.from][0]];
        let b = island_of[split[e.to][0]];
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
        return Err(SimDiag::setup("partition: the same-round graph keeps a cycle after its merges".into()));
    }
    pending.sort_unstable_by_key(|c| c.node);
    let cuts = pending.into_iter().map(|c| {
        let stiff = island_of[c.node];
        let mut soft: Vec<_> = c.soft.iter().map(|&u| island_of[u]).filter(|&i| i != stiff).collect();
        soft.sort_unstable();
        soft.dedup();
        Cut { node: c.node as u32, stiff, soft, rho: c.rho, tau: c.tau, stiff_first: c.tau <= o.cut_tol }
    }).filter(|c| !c.soft.is_empty()).collect();
    let refused = refused.into_iter().enumerate().filter_map(|(node, reason)| {
        reason.map(|reason| Refused { node: node as u32, reason })
    }).collect();
    stiffness.sort_unstable_by_key(|e| e.0);
    Ok(Partition { names, n: s.n, sources, sinks, islands, cuts, refused, merges, stiffness, freqs, dc_at, cut_tol: o.cut_tol, fmax: o.fmax, coupled })
}

/// Wrap even long node names, without depending on UTF-8 byte boundaries or terminal width.
pub(super) fn line(out: &mut String, text: &str) {
    let mut width = 0;
    for word in text.split_whitespace() {
        if width > 0 {
            if width + 1 + word.chars().count() > 160 {
                out.push_str("\n  ");
                width = 2;
            } else {
                out.push(' ');
                width += 1;
            }
        }
        for ch in word.chars() {
            if width == 160 {
                out.push_str("\n  ");
                width = 2;
            }
            out.push(ch);
            width += 1;
        }
    }
    out.push('\n');
}

impl Partition {
    /// The blocks left when the named nodes become an exact border (solved together each Newton
    /// iteration, as in a bordered block-diagonal solve): the structural components of every
    /// other unknown except sources and sinks. Only a view; the partition is unchanged.
    pub fn border_view(&self, border: &[&str]) -> Result<String, String> {
        let blocks = self.border_blocks(border)?;
        let total: usize = blocks.iter().map(Vec::len).sum::<usize>() + border.len();
        let largest = blocks.first().map_or(0, Vec::len);
        let mut out = String::new();
        line(&mut out, &format!("border {{{}}}: {} blocks; largest {largest} of {total} ({:.1}%); sizes {:?}", border.join(", "), blocks.len(),
            100.0 * largest as f64 / total.max(1) as f64, blocks.iter().map(Vec::len).collect::<Vec<_>>()));
        for block in &blocks {
            let names = block.iter().take(8).map(|&u| self.names[u].as_str()).collect::<Vec<_>>().join(", ");
            let more = if block.len() > 8 { format!(" +{} more", block.len() - 8) } else { String::new() };
            line(&mut out, &format!("  block of {}: {names}{more}", block.len()));
        }
        Ok(out)
    }

    /// `border_view`'s blocks: unknown indices, largest first.
    pub fn border_blocks(&self, border: &[&str]) -> Result<Vec<Vec<usize>>, String> {
        let mut cut = vec![false; self.n + 1];
        cut[0] = true;
        for &u in self.sources.iter().chain(&self.sinks) { cut[u as usize] = true; }
        for name in border {
            let u = self.names.iter().position(|n| n == name).ok_or_else(|| format!("no unknown named {name}"))?;
            cut[u] = true;
        }
        let mut sets = Sets((0..=self.n).collect());
        for u in 1..=self.n {
            if cut[u] { continue; }
            for &w in &self.coupled[u] {
                if !cut[w as usize] { sets.union(u, w as usize); }
            }
        }
        let mut blocks: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for u in (1..=self.n).filter(|&u| !cut[u]) { blocks.entry(sets.find(u)).or_default().push(u); }
        let mut blocks: Vec<_> = blocks.into_values().collect();
        blocks.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
        Ok(blocks)
    }

    pub fn report(&self, title: &str) -> String {
        let mut out = String::new();
        let total: usize = self.islands.iter().map(|i| i.unknowns.len()).sum();
        let largest = self.islands.iter().map(|i| i.unknowns.len()).max().unwrap_or(0);
        let percent = if total == 0 { 0.0 } else { 100.0 * largest as f64 / total as f64 };
        let stages = self.islands.iter().map(|i| i.stage + 1).max().unwrap_or(0);
        line(&mut out, &format!("{title}: n={}, {} sources, {} sinks; {total} unknowns in {} islands; largest {largest} ({percent:.1}%); {stages} stages",
            self.n, self.sources.len(), self.sinks.len(), self.islands.len()));
        line(&mut out, &format!("cut_tol={:.1e}; {} frequencies, fmax={}", self.cut_tol, self.freqs.len(), hz(self.fmax)));
        if let Some(f) = self.dc_at {
            line(&mut out, &format!("DC: candidates singular at 0 Hz were evaluated at {}", hz(f)));
        }
        for (i, island) in self.islands.iter().enumerate() {
            let names = island.unknowns.iter().take(10).map(|&u| self.names[u as usize].as_str()).collect::<Vec<_>>().join(", ");
            let more = if island.unknowns.len() > 10 { format!(" +{} more", island.unknowns.len() - 10) } else { String::new() };
            line(&mut out, &format!("island {i}: stage {}, size {}, control={}; {names}{more}", island.stage, island.unknowns.len(), island.control));
        }
        for cut in &self.cuts {
            let soft = cut.soft.iter().map(usize::to_string).collect::<Vec<_>>().join(",");
            let mut text = format!("cut {}: stiff {}, soft [{soft}]; rho", self.names[cut.node as usize], cut.stiff);
            for (f, label) in [(0.0, "DC"), (1e3, "1kHz"), (5e3, "5kHz"), (2e4, "20kHz")] {
                if let Ok(k) = self.freqs.binary_search_by(|v| v.total_cmp(&f)) {
                    let _ = write!(text, " {label}={:.1e}", cut.rho[k]);
                }
            }
            let (k, max) = peak(&cut.rho);
            let at = if k == 0 { "DC".to_string() } else { hz(self.freqs[k]) };
            let first = if cut.stiff_first { "stiff side first" } else { "soft side first" };
            let _ = write!(text, "; max={max:.1e} at {at}; tau={:.1e}; {first}", cut.tau);
            line(&mut out, &text);
        }
        // the refusals at stiff nodes one by one; softer nodes refusing their elements is the
        // normal case, so they are counted
        let z = |u: u32| self.stiffness.binary_search_by_key(&u, |e| e.0).map_or(f64::INFINITY, |k| self.stiffness[k].1);
        let soft = self.refused.iter().filter(|r| z(r.node) >= STIFF_OHMS).count();
        line(&mut out, &format!("refused: {} at stiff nodes (under {STIFF_OHMS} ohm), and {soft} softer nodes kept their elements",
            self.refused.len() - soft));
        for r in self.refused.iter().filter(|r| z(r.node) < STIFF_OHMS) {
            line(&mut out, &format!("  {} ({:.1e} ohm): {}", self.names[r.node as usize], z(r.node), r.reason));
        }
        let mut stiff: Vec<_> = self.stiffness.iter().filter(|e| e.1 < STIFF_OHMS).collect();
        stiff.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        let list = stiff.iter().map(|&&(u, r)| format!("{} {r:.1e}", self.names[u as usize])).collect::<Vec<_>>().join(", ");
        line(&mut out, &format!("stiff nodes (driving-point impedance in the whole block under {STIFF_OHMS} ohm, the band's maximum): {list}"));
        line(&mut out, &format!("merges: {} (island indices before renumbering)", self.merges.len()));
        for merge in &self.merges { line(&mut out, merge); }
        out
    }
}
