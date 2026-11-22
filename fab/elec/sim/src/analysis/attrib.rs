//! Where a profiled transient's work goes (ESIMRT row B0). It observes and changes nothing.
//!
//! - Every step attempt, by its role (a grid step, a step cut to a crossing, the first step after
//!   a restart, its confirming step, a step of the climb back after a restart) and outcome
//!   (accepted, or rejected by Newton, a crossing, the provisional test or the error estimate),
//!   and by the cause of the restart whose climb it belongs to: Newton iterations, loads and wall
//!   time. A restart's climb ends at an accepted step of at least half the step bound, or one that
//!   grew less than 1.5 times from the climb's last.
//! - The devices that fail Newton, by the step's role.
//! - A ghost bypass over named blocks (the partition's border view): at every load, whether each
//!   block's loaded values are bit-equal to the previous load's, and whether its terminals (its
//!   unknowns and the border unknowns beside it) moved more than a tenth of, or more than,
//!   Newton's own tolerance since the block was last counted as evaluated. The border is always
//!   active. Shares are weighted by unknowns.
//! - The largest move of named rails per accepted step, against Newton's tolerance.
//! - How often a binary step ladder's rung (`max_step / 2^k` at or below the step) and the order
//!   repeat from one attempt to the next; steps cut to land on a corner, an end or a crossing are
//!   off the ladder.

use crate::newton::Solver;
use crate::stamp::UnkKind;
use std::collections::BTreeMap;
use std::fmt::Write;

pub const ROLES: [&str; 5] = ["grid", "cut to a crossing", "first after a restart", "confirming", "climb"];
pub const OUTCOMES: [&str; 5] = ["accepted", "Newton failed", "crossing", "provisional", "error"];
pub const CAUSES: [&str; 5] = ["no restart", "corner", "crossing", "change", "rescue"];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tally {
    pub steps: u64,
    pub iterations: u64,
    pub loads: u64,
    pub ns: u64,
}

impl Tally {
    fn add(&mut self, iterations: u64, loads: u64, ns: u64) {
        self.steps += 1;
        self.iterations += iterations;
        self.loads += loads;
        self.ns += ns;
    }
}

const NONE: u32 = u32::MAX;
const FRACS: [f64; 2] = [0.1, 1.0];

/// The ghost bypass: blocks, their terminals and value slots, and what each saw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ghost {
    pub names: Vec<String>,
    pub size: Vec<u32>,
    pub border: u32,
    total: f64,
    terminals: Vec<Vec<u32>>,
    slots: Vec<Vec<u32>>,
    rows: Vec<Vec<u32>>,
    /// Per fraction, per block: the terminals' values when it was last counted as evaluated.
    refs: [Vec<Vec<f64>>; 2],
    prev: [Vec<f64>; 4],
    abs_tol: Vec<f64>,
    reltol: f64,
    /// Loads by class (0 grid, 1 restart work), and per class the unknown-weighted sums of the
    /// active share (per fraction) and of the bit-equal share.
    pub loads: [u64; 2],
    pub active: [[f64; 2]; 2],
    pub equal: [f64; 2],
    /// Per block: loads where it was active (per fraction) and where its values were bit-equal.
    pub block_active: [Vec<u64>; 2],
    pub block_equal: Vec<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rail {
    pub name: String,
    pub u: u32,
    pub max: f64,
    pub over_tenth: u64,
    pub over_one: u64,
    pub steps: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attribution {
    pub by_role: [[Tally; 5]; 5],
    pub by_cause: [Tally; 5],
    /// Newton failures by (bank, instance) and the step's role.
    pub failures: BTreeMap<(usize, u32), [u64; 5]>,
    pub failures_unnamed: [u64; 5],
    pub ghost: Option<Ghost>,
    pub rails: Vec<Rail>,
    /// Ladder: attempts off it, on it with the previous on-ladder attempt's rung and order, on it
    /// with a different one; attempts per (rung, order).
    pub off_ladder: u64,
    pub repeats: u64,
    pub changes: u64,
    pub rungs: BTreeMap<(u32, u8), u64>,
    last_rung: Option<(u32, u8)>,
    /// The class the next loads belong to (0 grid, 1 restart work), set by the stepper.
    pub class: u8,
    /// Time spent in this observer, which the attempts' wall excludes.
    pub overhead_ns: u64,
}

impl Attribution {
    /// `blocks` and `border` by unknown name (the partition's border view), `rails` by name.
    pub fn new(s: &Solver, blocks: &[Vec<String>], border: &[String], rails: &[&str]) -> Result<Attribution, String> {
        let index: BTreeMap<&str, u32> = s.unknowns.iter().enumerate().map(|(i, u)| (u.name.as_str(), i as u32)).collect();
        let find = |name: &str| index.get(name).copied().ok_or_else(|| format!("attribution: no unknown named {name}"));
        let mut block_of = vec![NONE; s.n + 1];
        let mut is_border = vec![false; s.n + 1];
        for name in border {
            is_border[find(name)? as usize] = true;
        }
        let mut ghost = Ghost { reltol: s.opts.reltol, ..Ghost::default() };
        for (b, names) in blocks.iter().enumerate() {
            let mut own = Vec::with_capacity(names.len());
            for name in names {
                let u = find(name)?;
                block_of[u as usize] = b as u32;
                own.push(u);
            }
            ghost.names.push(names.first().cloned().unwrap_or_default());
            ghost.size.push(own.len() as u32);
            ghost.rows.push(own);
        }
        // terminals: a block's unknowns, then the border unknowns its matrix entries reach
        let mut terminals: Vec<Vec<u32>> = ghost.rows.clone();
        let mut slots = vec![Vec::new(); blocks.len()];
        for v in 1..s.slot_col.len() {
            let (r, c) = (s.slot_row[v] as usize, s.slot_col[v] as usize);
            if r == 0 || c == 0 {
                continue;
            }
            let (br, bc) = (block_of[r], block_of[c]);
            if bc != NONE {
                slots[bc as usize].push(v as u32);
            } else if br != NONE {
                slots[br as usize].push(v as u32);
            }
            if br != NONE && is_border[c] {
                terminals[br as usize].push(c as u32);
            }
            if bc != NONE && is_border[r] {
                terminals[bc as usize].push(r as u32);
            }
        }
        for t in &mut terminals {
            t.sort_unstable();
            t.dedup();
        }
        ghost.border = is_border.iter().filter(|&&b| b).count() as u32;
        ghost.total = f64::from(ghost.size.iter().sum::<u32>() + ghost.border);
        ghost.refs = [terminals.iter().map(|t| vec![f64::NAN; t.len()]).collect(), terminals.iter().map(|t| vec![f64::NAN; t.len()]).collect()];
        ghost.terminals = terminals;
        ghost.slots = slots;
        ghost.abs_tol = s.unknowns.iter().map(|u| if u.kind == UnkKind::Voltage { s.opts.vntol } else { s.opts.abstol }).collect();
        ghost.block_active = [vec![0; blocks.len()], vec![0; blocks.len()]];
        ghost.block_equal = vec![0; blocks.len()];
        let mut out = Attribution { ghost: Some(ghost), ..Attribution::default() };
        for name in rails {
            out.rails.push(Rail { name: name.to_string(), u: find(name)?, ..Rail::default() });
        }
        Ok(out)
    }

    /// A load's values at the iterate `x` (the solver calls this after every load).
    pub fn on_load(&mut self, x: &[f64], g: &[f64], c: &[f64], f: &[f64], q: &[f64]) {
        let class = usize::from(self.class.min(1));
        let Some(gh) = self.ghost.as_mut() else { return };
        gh.loads[class] += 1;
        let first = gh.prev[0].len() != g.len();
        let mut equal = 0.0;
        for b in 0..gh.size.len() {
            let same = !first
                && gh.slots[b].iter().all(|&v| {
                    let v = v as usize;
                    g[v].to_bits() == gh.prev[0][v].to_bits() && c[v].to_bits() == gh.prev[1][v].to_bits()
                })
                && gh.rows[b].iter().all(|&u| {
                    let u = u as usize;
                    f[u].to_bits() == gh.prev[2][u].to_bits() && q[u].to_bits() == gh.prev[3][u].to_bits()
                });
            if same {
                equal += f64::from(gh.size[b]);
                gh.block_equal[b] += 1;
            }
        }
        gh.equal[class] += equal / gh.total;
        for (k, frac) in FRACS.iter().enumerate() {
            let mut active = f64::from(gh.border);
            for b in 0..gh.size.len() {
                let refs = &mut gh.refs[k][b];
                let moved = gh.terminals[b].iter().zip(refs.iter()).any(|(&u, &r)| {
                    let (u, v) = (u as usize, x[u as usize]);
                    !(r.is_finite() && (v - r).abs() <= frac * (gh.reltol * v.abs().max(r.abs()) + gh.abs_tol[u]))
                });
                if moved {
                    for (r, &u) in refs.iter_mut().zip(&gh.terminals[b]) {
                        *r = x[u as usize];
                    }
                    active += f64::from(gh.size[b]);
                    gh.block_active[k][b] += 1;
                }
            }
            gh.active[k][class] += active / gh.total;
        }
        for (dst, src) in gh.prev.iter_mut().zip([g, c, f, q]) {
            dst.clear();
            dst.extend_from_slice(src);
        }
    }

    /// One step attempt ended.
    pub fn attempt(&mut self, role: usize, cause: usize, outcome: usize, iterations: u64, loads: u64, ns: u64) {
        self.by_role[role][outcome].add(iterations, loads, ns);
        self.by_cause[cause].add(iterations, loads, ns);
    }

    /// The rung and order of an attempt, or `None` when its step was cut to land somewhere.
    pub fn ladder(&mut self, rung: Option<(u32, u8)>) {
        match rung {
            None => self.off_ladder += 1,
            Some(r) => {
                if self.last_rung == Some(r) {
                    self.repeats += 1;
                } else {
                    self.changes += 1;
                }
                self.last_rung = Some(r);
                *self.rungs.entry(r).or_default() += 1;
            }
        }
    }

    pub fn newton_failed(&mut self, at: Option<(usize, u32)>, role: usize) {
        match at {
            Some(k) => self.failures.entry(k).or_default()[role] += 1,
            None => self.failures_unnamed[role] += 1,
        }
    }

    /// An accepted step from `x_old` to `x_new`.
    pub fn accepted(&mut self, x_old: &[f64], x_new: &[f64], reltol: f64, vntol: f64) {
        for r in &mut self.rails {
            let u = r.u as usize;
            let d = (x_new[u] - x_old[u]).abs();
            let tol = reltol * x_new[u].abs().max(x_old[u].abs()) + vntol;
            r.max = r.max.max(d);
            r.steps += 1;
            if d > 0.1 * tol {
                r.over_tenth += 1;
            }
            if d > tol {
                r.over_one += 1;
            }
        }
    }

    /// The tables, per simulated second over `t` seconds; `wall_ns` is the analog wall time the
    /// attempts are measured against.
    pub fn report(&self, s: &Solver, t: f64, wall_ns: u64) -> String {
        let mut out = String::new();
        let per_s = |x: u64| x as f64 / t;
        let sum = |ts: &[Tally]| ts.iter().fold(Tally::default(), |a, b| Tally {
            steps: a.steps + b.steps,
            iterations: a.iterations + b.iterations,
            loads: a.loads + b.loads,
            ns: a.ns + b.ns,
        });
        let all = sum(&self.by_cause);
        let share = |x: u64, of: u64| 100.0 * x as f64 / of.max(1) as f64;
        let _ = writeln!(out, "  attribution: {:.0} attempts, {:.0} iterations, {:.0} loads a simulated second; attempts hold {:.0} % of the analog wall (observer time excluded)",
            per_s(all.steps), per_s(all.iterations), per_s(all.loads), share(all.ns, wall_ns));
        let _ = writeln!(out, "  by restart cause (a restart's climb runs until its step reaches half the bound or stops growing):");
        for (k, name) in CAUSES.iter().enumerate() {
            let x = self.by_cause[k];
            if x.steps == 0 {
                continue;
            }
            let _ = writeln!(out, "    {name:<12} {:>9.0} attempts {:>9.0} iterations ({:>4.1} %) {:>9.0} loads, wall {:>4.1} %, {:.2} iterations an attempt",
                per_s(x.steps), per_s(x.iterations), share(x.iterations, all.iterations), per_s(x.loads), share(x.ns, all.ns),
                x.iterations as f64 / x.steps.max(1) as f64);
        }
        let restart = all.iterations - self.by_cause[0].iterations;
        let _ = writeln!(out, "    restart work: {:.1} % of iterations, {:.1} % of the attempts' wall", share(restart, all.iterations), share(all.ns - self.by_cause[0].ns, all.ns));
        let _ = writeln!(out, "  by role and outcome (attempts / iterations a simulated second):");
        for (r, role) in ROLES.iter().enumerate() {
            let row = sum(&self.by_role[r]);
            if row.steps == 0 {
                continue;
            }
            let cells = OUTCOMES.iter().enumerate().filter(|(o, _)| self.by_role[r][*o].steps > 0)
                .map(|(o, name)| format!("{name} {:.0}/{:.0}", per_s(self.by_role[r][o].steps), per_s(self.by_role[r][o].iterations)))
                .collect::<Vec<_>>().join(", ");
            let _ = writeln!(out, "    {role:<22} {:>9.0} attempts {:>9.0} iterations ({:>4.1} %), wall {:>4.1} %: {cells}",
                per_s(row.steps), per_s(row.iterations), share(row.iterations, all.iterations), share(row.ns, all.ns));
        }
        let fails: u64 = self.failures.values().flatten().sum::<u64>() + self.failures_unnamed.iter().sum::<u64>();
        if fails > 0 {
            let mut list: Vec<_> = self.failures.iter().map(|(&(b, i), by)| (by.iter().sum::<u64>(), b, i, *by)).collect();
            list.sort_by(|a, b| b.0.cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2))));
            let _ = writeln!(out, "  Newton failures: {:.0} a simulated second; the last device not converged, by the step's role:", per_s(fails));
            for (n, b, i, by) in list.iter().take(12) {
                let roles = ROLES.iter().enumerate().filter(|(r, _)| by[*r] > 0).map(|(r, name)| format!("{name} {:.0}", per_s(by[r]))).collect::<Vec<_>>().join(", ");
                let _ = writeln!(out, "    {:<28} {:>8.0} ({roles})", s.banks[*b].inst_name(*i as usize), per_s(*n));
            }
            let unnamed: u64 = self.failures_unnamed.iter().sum();
            if unnamed > 0 {
                let _ = writeln!(out, "    (no device named: {:.0})", per_s(unnamed));
            }
        }
        if let Some(gh) = &self.ghost {
            let mean = |x: f64, n: u64| if n == 0 { f64::NAN } else { x / n as f64 };
            let _ = writeln!(out, "  ghost bypass over {} blocks and a border of {} ({} unknowns): unknown-weighted share per load, grid / restart work:",
                gh.size.len(), gh.border, gh.total);
            let _ = writeln!(out, "    loads {:.0} / {:.0} a simulated second; bit-equal values {:.3} / {:.3}; moved past 0.1 tol {:.3} / {:.3}; past 1.0 tol {:.3} / {:.3}",
                per_s(gh.loads[0]), per_s(gh.loads[1]), mean(gh.equal[0], gh.loads[0]), mean(gh.equal[1], gh.loads[1]),
                mean(gh.active[0][0], gh.loads[0]), mean(gh.active[0][1], gh.loads[1]), mean(gh.active[1][0], gh.loads[0]), mean(gh.active[1][1], gh.loads[1]));
            let loads = (gh.loads[0] + gh.loads[1]).max(1) as f64;
            let all_active = |k: usize| (gh.active[k][0] + gh.active[k][1]) / loads;
            let _ = writeln!(out, "    all loads: active past 0.1 tol {:.3}, past 1.0 tol {:.3}", all_active(0), all_active(1));
            let mut order: Vec<usize> = (0..gh.size.len()).collect();
            order.sort_by(|&a, &b| gh.size[b].cmp(&gh.size[a]).then(a.cmp(&b)));
            let _ = writeln!(out, "    per block (unknowns: share of loads active past 0.1 / 1.0 tol, bit-equal):");
            let line = order.iter().filter(|&&b| gh.size[b] > 3).map(|&b| format!("{} ({}): {:.2}/{:.2}, {:.2}", gh.names[b], gh.size[b],
                gh.block_active[0][b] as f64 / loads, gh.block_active[1][b] as f64 / loads, gh.block_equal[b] as f64 / loads)).collect::<Vec<_>>().join("; ");
            let _ = writeln!(out, "      {line}");
            let small: Vec<usize> = order.iter().copied().filter(|&b| gh.size[b] <= 3).collect();
            if !small.is_empty() {
                let n = small.len() as f64;
                let avg = |v: &Vec<u64>| small.iter().map(|&b| v[b] as f64).sum::<f64>() / n / loads;
                let _ = writeln!(out, "      and {} blocks of 3 or fewer, on average: {:.2}/{:.2}, {:.2}", small.len(), avg(&gh.block_active[0]), avg(&gh.block_active[1]), avg(&gh.block_equal));
            }
        }
        for r in &self.rails {
            let _ = writeln!(out, "  rail {}: largest move a step {:.2e} V; steps moving past 0.1 tol {:.3}, past 1.0 tol {:.3}",
                r.name, r.max, r.over_tenth as f64 / r.steps.max(1) as f64, r.over_one as f64 / r.steps.max(1) as f64);
        }
        let on = self.repeats + self.changes;
        let _ = writeln!(out, "  step ladder: {:.1} % of attempts off it (cut to land); on it, the rung and order repeat {:.1} %, over {} rungs and orders; the busiest: {}",
            share(self.off_ladder, on + self.off_ladder), share(self.repeats, on), self.rungs.len(),
            {
                let mut v: Vec<_> = self.rungs.iter().collect();
                v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                v.iter().take(6).map(|((k, o), n)| format!("2^-{k} order {o} {:.1} %", share(**n, on))).collect::<Vec<_>>().join(", ")
            });
        out
    }
}
