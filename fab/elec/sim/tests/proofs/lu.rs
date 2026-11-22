//! T21: the sparse LU against the dense one on random circuit-patterned matrices.

use fab_elec_sim::num::{Scalar, C64};
use fab_elec_sim::sparse::{Layout, Lu};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() * n as f64) as usize % n
    }
}

/// An MNA-shaped matrix: a random conductance graph over `nodes`, voltage sources (zero
/// diagonals in their branch rows) and transconductances (unsymmetric entries).
fn mna(rng: &mut Lcg, nodes: usize, sources: usize, ctl: usize) -> (usize, Vec<(u32, u32, f64)>) {
    let n = nodes + sources;
    let mut e = Vec::new();
    let cond = |a: usize, b: Option<usize>, g: f64, e: &mut Vec<(u32, u32, f64)>| {
        e.push((a as u32, a as u32, g));
        if let Some(b) = b {
            e.push((b as u32, b as u32, g));
            e.push((a as u32, b as u32, -g));
            e.push((b as u32, a as u32, -g));
        }
    };
    for a in 1..nodes {
        // a spanning chain keeps it connected, plus random extra edges and some ground paths
        cond(a, Some(rng.pick(a)), 1e-3 + rng.next(), &mut e);
    }
    for _ in 0..nodes * 2 {
        let (a, b) = (rng.pick(nodes), rng.pick(nodes));
        if a != b {
            cond(a, Some(b), 10f64.powf(-6.0 + 9.0 * rng.next()), &mut e);
        }
    }
    for _ in 0..nodes / 4 {
        cond(rng.pick(nodes), None, 1e-2 + rng.next(), &mut e);
    }
    for s in 0..sources {
        // distinct nodes: two sources on one node would be a singular (parallel sources) circuit
        let (br, p) = ((nodes + s) as u32, (s * nodes / sources) as u32);
        e.push((p, br, 1.0));
        e.push((br, p, 1.0));
    }
    for _ in 0..ctl {
        let (a, b) = (rng.pick(nodes) as u32, rng.pick(nodes) as u32);
        e.push((a, b, 0.5 * rng.next()));
    }
    (n, e)
}

fn assemble<S: Scalar>(n: usize, e: &[(u32, u32, f64)], dense: bool, val: impl Fn(usize, f64) -> S) -> (Layout, Vec<S>) {
    let entries: Vec<(u32, u32)> = e.iter().map(|&(r, c, _)| (r, c)).collect();
    let (layout, map) = Layout::build(n, &entries, Some(dense));
    let mut v = vec![S::ZERO; layout.len()];
    for (k, &(_, _, x)) in e.iter().enumerate() {
        v[map[k] as usize] += val(k, x);
    }
    (layout, v)
}

/// The normwise backward error |b - A x| / (|A| |x| + |b|), infinity norms.
fn backward<S: Scalar>(n: usize, e: &[(u32, u32, f64)], val: &impl Fn(usize, f64) -> S, x: &[S], b: &[S], transposed: bool) -> f64 {
    let mut r: Vec<S> = b.to_vec();
    let mut row_sum = vec![0.0f64; n];
    for (k, &(row, col, v)) in e.iter().enumerate() {
        let (row, col) = if transposed { (col, row) } else { (row, col) };
        let a = val(k, v);
        r[row as usize] -= a * x[col as usize];
        row_sum[row as usize] += a.mag();
    }
    let norm = |v: &[S]| v.iter().map(|v| v.mag()).fold(0.0, f64::max);
    let an = row_sum.iter().cloned().fold(0.0, f64::max);
    norm(&r) / (an * norm(x) + norm(b))
}

fn solved<S: Scalar>(lu: &mut Lu<S>, b: &[S], transposed: bool) -> Vec<S> {
    let mut x = b.to_vec();
    if transposed {
        lu.solve_transposed(&mut x);
    } else {
        lu.solve(&mut x);
    }
    x
}

fn compare<S: Scalar>(kept: &mut Lu<S>, fresh: &mut Lu<S>, b: &[S], bits: &impl Fn(S) -> [u64; 2], step: usize) {
    assert_eq!(kept.work(), fresh.work(), "factor work at step {step}");
    for transposed in [false, true] {
        let actual = solved(kept, b, transposed);
        let expected = solved(fresh, b, transposed);
        for (i, (&a, &e)) in actual.iter().zip(&expected).enumerate() {
            assert!(a.mag().is_finite() && e.mag().is_finite(), "nonfinite solution at {i}");
            assert_eq!(bits(a), bits(e), "n={} step={step} transposed={transposed} x[{i}]: {a:?} != {e:?}", b.len());
        }
    }
}

fn refactor_all<S: Scalar>(lu: &mut Lu<S>, vals: &[S]) {
    match lu {
        Lu::Sparse(s) => s.refactor_all(vals).expect("refactor every column"),
        Lu::Dense(_) => panic!("a forced sparse layout"),
    }
}

fn check<S: Scalar>(rng: &mut Lcg, nodes: usize, val: impl Fn(usize, f64) -> S, what: &str) {
    let (n, e) = mna(rng, nodes, nodes / 10 + 1, nodes / 8);
    let b: Vec<S> = (0..n).map(|i| S::from_f64(1.0 + (i % 7) as f64)).collect();
    let (ld, vd) = assemble(n, &e, true, &val);
    let (ls, vs) = assemble(n, &e, false, &val);
    let mut dlu: Lu<S> = ld.lu();
    let mut slu: Lu<S> = ls.lu();
    dlu.factor(&vd).expect("dense factor");
    slu.factor(&vs).expect("sparse factor");
    for transposed in [false, true] {
        let ws = backward(n, &e, &val, &solved(&mut slu, &b, transposed), &b, transposed);
        let wd = backward(n, &e, &val, &solved(&mut dlu, &b, transposed), &b, transposed);
        assert!(ws < 1e-12 && wd < 1e-12, "{what} n={n} transposed={transposed}: backward error sparse {ws:e}, dense {wd:e}");
    }
    // a refactor on the same values repeats the factor's arithmetic exactly
    let x1 = solved(&mut slu, &b, false);
    slu.refactor(&vs).expect("refactor");
    let x2 = solved(&mut slu, &b, false);
    assert!(x1 == x2, "{what} n={n}: refactor on unchanged values is not bit-identical to the factor");
    // new values on the same pattern: the kept pivots still give a backward-stable solve
    let val2 = |k: usize, x: f64| val(k, x) * S::from_f64(1.0 + 0.3 * ((k * 7919) % 13) as f64 / 13.0);
    let (_, vs2) = assemble(n, &e, false, &val2);
    slu.refactor(&vs2).expect("refactor");
    let wr = backward(n, &e, &val2, &solved(&mut slu, &b, false), &b, false);
    assert!(wr < 1e-12, "{what} n={n}: refactor backward error {wr:e}");
}

#[test]
fn t21_sparse_lu_against_dense() {
    let mut rng = Lcg(0x5eed);
    for nodes in [20, 60, 200] {
        for _ in 0..3 {
            check::<f64>(&mut rng, nodes, |_, x| x, "real");
        }
        // AC: G + jwC with a random capacitive part
        check::<C64>(&mut rng, nodes, |k, x| C64::new(x, 1e-3 * ((k % 5) as f64)), "complex");
    }
    // the ordering keeps a ladder's fill linear: 2000 unknowns, a few nonzeros per column
    let n = 2000;
    let mut e = Vec::new();
    for i in 0..n {
        e.push((i as u32, i as u32, 3.0));
        if i + 1 < n {
            e.push((i as u32, i as u32 + 1, -1.0));
            e.push((i as u32 + 1, i as u32, -1.0));
        }
    }
    let (layout, v) = assemble::<f64>(n, &e, false, |_, x| x);
    let mut lu = layout.lu::<f64>();
    lu.factor(&v).unwrap();
    if let Lu::Sparse(s) = &lu {
        assert!(s.fill() <= 3 * n, "ladder fill {} for {n} unknowns", s.fill());
    } else {
        panic!("a forced sparse layout");
    }
}

/// A kept numeric plan must repeat a fresh factor bit for bit, including signed zeros and
/// the transposed solve. Skipping must also repeat a refactor of every column. Strong diagonals
/// fix the pivots; the final sequence forces a new plan.
#[test]
fn t21_flat_lu_refactor_is_bit_exact() {
    fn check<S: Scalar>(make: impl Fn(f64, f64) -> S, bits: impl Fn(S) -> [u64; 2]) {
        use fab_elec_sim::sparse::lu::PIVOT_TOL;

        let mut rng = Lcg(0xb17e5ac7);
        for n in [5, 12, 40, 80, 120] {
            for _ in 0..2 {
                let sources = (n / 10).max(1);
                let (_, mut entries) = mna(&mut rng, n - sources, sources, n / 8);
                // Regularise every diagonal, including the voltage-source rows. Domination by
                // far more than 1/PIVOT_TOL forces pivot row q[k], even if pref[k] differs.
                entries.extend((0..n).map(|i| (i as u32, i as u32, 0.0)));
                let (layout, base) = assemble::<f64>(n, &entries, false, |_, x| x);
                let (rows, cols) = layout.coords();
                let mut scale = vec![1.0; n];
                for i in 1..base.len() {
                    scale[cols[i] as usize] += base[i].abs();
                }
                let b: Vec<S> = (0..n).map(|i| match i % 5 {
                    0 => make(0.0, 0.0),
                    1 => make(-0.0, -0.0),
                    _ => make(rng.next() - 0.5, rng.next() - 0.5),
                }).collect();
                let zeros: Vec<S> = (0..n).map(|i| {
                    let z = if i % 2 == 0 { 0.0 } else { -0.0 };
                    make(z, z)
                }).collect();
                let mut kept = layout.lu::<S>();
                for step in 0..9 {
                    let mut vals = vec![S::ZERO; layout.len()];
                    // The trash is deliberately nonzero: fill loads must not read it.
                    vals[0] = make(17.0, -3.0);
                    for i in 1..vals.len() {
                        if rows[i] == cols[i] {
                            let sign = if rows[i] % 2 == 0 { 1.0 } else { -1.0 };
                            let re = sign * 1e6 * scale[cols[i] as usize] * (0.75 + 0.5 * rng.next());
                            vals[i] = make(re, re * 0.01 * rng.next());
                        } else if (i + step) % 7 == 0 {
                            // Assign after assembly so += onto ZERO cannot erase the sign.
                            vals[i] = make(-0.0, -0.0);
                        } else {
                            let re = base[i] * (0.75 + 0.5 * rng.next());
                            vals[i] = make(re, re * 0.1 * (rng.next() - 0.5));
                        }
                    }
                    if step == 0 {
                        kept.factor(&vals).expect("initial factor");
                    } else {
                        kept.refactor(&vals).expect("kept refactor");
                    }
                    assert_eq!(kept.counts(), (1, step as u64), "dominant pivots must stay usable");
                    let mut fresh = layout.lu::<S>();
                    fresh.factor(&vals).expect("fresh factor");
                    compare(&mut kept, &mut fresh, &b, &bits, step);
                    compare(&mut kept, &mut fresh, &zeros, &bits, step);
                }
            }
        }

        // Small changes to none, one or a random subset of the entries. Keep the diagonals
        // dominant so this sequence measures skipped work without changing the pivot plan.
        let (n, mut entries) = mna(&mut rng, 20, 2, 2);
        entries.extend((0..n).map(|i| (i as u32, i as u32, 0.0)));
        let (layout, base) = assemble::<f64>(n, &entries, false, |_, x| x);
        let (rows, cols) = layout.coords();
        let mut scale = vec![1.0; n];
        for i in 1..base.len() {
            scale[cols[i] as usize] += base[i].abs();
        }
        let mut vals: Vec<S> = base.iter().enumerate().map(|(i, &x)| {
            let re = if i > 0 && rows[i] == cols[i] { 1e6 * scale[cols[i] as usize] } else { x };
            make(re, re * 0.01)
        }).collect();
        let zero = (1..vals.len()).find(|&i| rows[i] != cols[i]).unwrap();
        vals[zero] = make(-0.0, -0.0);
        let entries: Vec<usize> = (1..vals.len()).filter(|&i| vals[i] != S::ZERO).collect();
        let b: Vec<S> = (0..n).map(|_| make(rng.next() - 0.5, rng.next() - 0.5)).collect();
        let mut kept = layout.lu::<S>();
        let mut all = layout.lu::<S>();
        kept.factor(&vals).expect("factor before skipping");
        all.factor(&vals).expect("reference factor before skipping");
        for step in 0..24 {
            let one = entries[rng.pick(entries.len())];
            for &i in &entries {
                let change = match step % 4 {
                    0 => false,
                    1 => i == one,
                    2 => rng.next() < 0.125,
                    _ => rng.next() < 0.75,
                };
                if change {
                    vals[i] *= S::from_f64(0.999 + 0.002 * rng.next());
                }
            }
            // Neither a signed-zero change nor a change to the trash is a changed load.
            let z = if (step / 4) % 2 == 0 { 0.0 } else { -0.0 };
            vals[zero] = make(z, z);
            vals[0] = make(step as f64 + 17.0, -3.0);
            let before = kept.column_counts();
            kept.refactor(&vals).expect("skipping refactor");
            refactor_all(&mut all, &vals);
            assert_eq!(kept.counts(), (1, step as u64 + 1), "small changes must keep the pivots");
            assert_eq!(kept.counts(), all.counts(), "same pivot history at step {step}");
            compare(&mut kept, &mut all, &b, &bits, step);
            if step > 0 && step % 4 == 0 {
                assert_eq!(kept.column_counts(), (before.0 + n as u64, before.1), "unchanged loads must skip every column");
            }
        }
        let (skipped, recomputed) = kept.column_counts();
        assert!(skipped > 0 && recomputed > 0, "the sequence must exercise both paths");
        assert_eq!(all.column_counts(), (0, 24 * n as u64), "the reference must never skip");
        all.refactor(&vals).expect("skip after the reference pass");
        assert_eq!(all.column_counts(), (n as u64, 24 * n as u64), "the reference must remember its loads");

        // After eliminating column 0, column 1 has diag = a11 - 1/4 and a candidate of 1.
        // Shrink that kept diagonal below PIVOT_TOL only after some slots have been updated;
        // the full factor must swap rows 1 and 2, then the next refactor must use its new plan.
        let entries = [(0, 0, 4.0), (1, 0, 1.0), (2, 0, -0.0),
                       (0, 1, 1.0), (1, 1, 4.0), (2, 1, 1.0),
                       (0, 2, -0.0), (1, 2, 1.0), (2, 2, 4.0)];
        let (layout, mut vals) = assemble::<S>(3, &entries, false, |_, x| make(x, 0.0));
        let (rows, cols) = layout.coords();
        let diag = (1..vals.len()).find(|&i| rows[i] == 1 && cols[i] == 1).unwrap();
        for i in 1..vals.len() {
            if rows[i].abs_diff(cols[i]) == 2 {
                vals[i] = make(-0.0, -0.0);
            }
        }
        let b = [make(1.0, 0.5), make(-0.0, -0.0), make(0.5, -0.25)];
        let zeros = [make(-0.0, -0.0), S::ZERO, make(-0.0, -0.0)];
        for dirty in [false, true] {
            vals[diag] = make(4.0, 0.0);
            let mut kept = layout.lu::<S>();
            let mut all = layout.lu::<S>();
            kept.factor(&vals).expect("factor before pivot shrink");
            all.factor(&vals).expect("reference factor before pivot shrink");
            kept.refactor(&vals).expect("remember before pivot shrink");
            refactor_all(&mut all, &vals);
            assert_eq!(kept.work(), Some((9, 5, 6)), "full 3x3 factor work");
            for (step, fraction) in [0.25, 0.25, 0.25, 0.125].into_iter().enumerate() {
                vals[diag] = make(0.25 + PIVOT_TOL * fraction, 0.0);
                let before = kept.column_counts();
                if dirty {
                    let changed: &[u32] = if step == 0 || step == 3 { &[1, 1] } else { &[] };
                    kept.refactor_dirty(&vals, changed).expect("dirty refactor through pivot shrink");
                } else {
                    kept.refactor(&vals).expect("refactor through pivot shrink");
                }
                refactor_all(&mut all, &vals);
                assert_eq!(kept.counts(), (2, step as u64 + 2), "one fallback, then reuse the rebuilt plan");
                assert_eq!(kept.counts(), all.counts(), "both paths must fall back once");
                let after = kept.column_counts();
                if step == 0 {
                    assert_eq!(after, (before.0 + 1, before.1 + 1), "count the skipped and failing columns before fallback");
                } else if step == 1 {
                    assert_eq!(after, (before.0, before.1 + 3), "a rebuilt plan must recompute every column once");
                } else if step == 2 {
                    assert_eq!(after, (before.0 + 3, before.1), "unchanged loads must skip after the rebuilt plan is remembered");
                }
                compare(&mut kept, &mut all, &b, &bits, step);
                compare(&mut kept, &mut all, &zeros, &bits, step);
                let mut fresh = layout.lu::<S>();
                fresh.factor(&vals).expect("fresh factor after pivot shrink");
                compare(&mut kept, &mut fresh, &b, &bits, step);
                compare(&mut kept, &mut fresh, &zeros, &bits, step);
            }
        }
    }

    check::<f64>(|re, _| re, |x| [x.to_bits(), 0]);
    check::<C64>(C64::new, |x| [x.re.to_bits(), x.im.to_bits()]);
}

/// Dirty columns must visit precisely their dependency closure, keep the comparing path's
/// remembered loads current, and reproduce full refactors and fresh factors bit for bit.
#[test]
fn t21_dirty_refactor_is_bit_exact() {
    fn check<S: Scalar>(make: impl Fn(f64, f64) -> S, bits: impl Fn(S) -> [u64; 2]) {
        let mut rng = Lcg(0xd17c01);
        // Include a partially filled second bitset word and dependencies crossing word 0.
        for n in [5, 12, 40, 65, 120] {
            let sources = (n / 10).max(1);
            let (_, mut entries) = mna(&mut rng, n - sources, sources, n / 8);
            entries.extend((0..n).map(|i| (i as u32, i as u32, 0.0)));
            let (layout, base) = assemble::<f64>(n, &entries, false, |_, x| x);
            let q = match &layout {
                Layout::Sparse { ord, .. } => &ord.q,
                Layout::Dense { .. } => panic!("a forced sparse layout"),
            };
            let (rows, cols) = layout.coords();
            let mut scale = vec![1.0; n];
            for i in 1..base.len() {
                scale[cols[i] as usize] += base[i].abs();
            }
            let mut vals: Vec<S> = base.iter().enumerate().map(|(i, &x)| {
                if i > 0 && rows[i] == cols[i] {
                    let sign = if rows[i] % 2 == 0 { 1.0 } else { -1.0 };
                    let re = sign * 1e6 * scale[cols[i] as usize];
                    make(re, re * 0.01)
                } else if i % 7 == 0 {
                    make(-0.0, -0.0)
                } else {
                    make(x, x * 0.01)
                }
            }).collect();
            vals[0] = make(17.0, -3.0);
            let b: Vec<S> = (0..n).map(|_| make(rng.next() - 0.5, rng.next() - 0.5)).collect();
            let zeros: Vec<S> = (0..n).map(|i| {
                let z = if i % 2 == 0 { 0.0 } else { -0.0 };
                make(z, z)
            }).collect();
            let mut dirty = layout.lu::<S>();
            let mut mixed = layout.lu::<S>();
            let mut comparing = layout.lu::<S>();
            let mut all = layout.lu::<S>();
            dirty.refactor_dirty(&vals, &[]).expect("dirty call without a factor");
            assert_eq!(dirty.counts(), (1, 0), "a first dirty call must factor");
            assert_eq!(dirty.column_counts(), (0, 0), "full factors count no refactor columns");
            mixed.factor(&vals).expect("mixed factor");
            comparing.factor(&vals).expect("comparing factor");
            all.factor(&vals).expect("reference factor");
            dirty.refactor_dirty(&vals, &[]).expect("remember every column even with an empty list");
            mixed.refactor_dirty(&vals, &[]).expect("remember mixed loads");
            comparing.refactor(&vals).expect("remember comparing loads");
            refactor_all(&mut all, &vals);
            assert_eq!(dirty.column_counts(), (0, n as u64), "the first dirty refactor must remember all loads");
            compare(&mut dirty, &mut all, &b, &bits, 0);
            compare(&mut dirty, &mut all, &zeros, &bits, 0);

            for step in 0..24 {
                let mut changed = vec![false; n];
                match step % 6 {
                    1 => changed[q[n - 1] as usize] = true,
                    2 => changed[q[0] as usize] = true,
                    3 | 4 => {
                        let chance = if step % 6 == 3 { 0.125 } else { 0.75 };
                        for c in &mut changed {
                            *c = rng.next() < chance;
                        }
                    }
                    _ => {}
                }
                // Perturb every listed column's nonzero diagonal so the comparing refactor
                // is an independent oracle for the dirty closure (including numeric zeros).
                for i in 1..vals.len() {
                    if changed[cols[i] as usize] {
                        if rows[i] == cols[i] {
                            vals[i] *= S::from_f64(1.0005 + 0.001 * rng.next());
                        } else if (i + step) % 7 == 0 {
                            let z = if step % 2 == 0 { -0.0 } else { 0.0 };
                            vals[i] = make(z, z);
                        } else {
                            let re = base[i] * (0.75 + 0.5 * rng.next());
                            vals[i] = make(re, re * 0.1 * (rng.next() - 0.5));
                        }
                    }
                }
                vals[0] = make(step as f64 + 18.0, -3.0);
                let mut listed: Vec<u32> = (0..n).rev().filter(|&c| changed[c]).map(|c| c as u32).collect();
                if step % 6 == 5 {
                    // Extra unchanged columns are legal, including adjacent steps on either
                    // side of a bitset word boundary. They still require a recomputation.
                    listed.push(q[rng.pick(n)]);
                    if n > 64 {
                        listed.extend([q[63], q[64]]);
                    }
                }
                if let Some(&c) = listed.first() {
                    listed.push(c);
                }
                let before = dirty.column_counts();
                let before_comparing = comparing.column_counts();
                let before_mixed = mixed.column_counts();
                dirty.refactor_dirty(&vals, &listed).expect("dirty refactor");
                comparing.refactor(&vals).expect("closure oracle");
                refactor_all(&mut all, &vals);
                // A different cycle from the mutations covers every ordered pair of APIs,
                // with dirty -> comparing and comparing -> dirty both occurring directly.
                match step % 7 {
                    0 | 2 | 6 => mixed.refactor_dirty(&vals, &listed).expect("mixed dirty refactor"),
                    1 | 4 => mixed.refactor(&vals).expect("mixed comparing refactor"),
                    _ => refactor_all(&mut mixed, &vals),
                }
                let after = dirty.column_counts();
                let visited = after.1 - before.1;
                let oracle = comparing.column_counts().1 - before_comparing.1;
                assert_eq!(after.0 - before.0 + visited, n as u64, "every step is skipped or recomputed");
                if step % 6 != 5 {
                    assert_eq!(visited, oracle, "n={n} step={step}: dirty closure must match the comparing refactor");
                }
                match step % 6 {
                    0 => assert_eq!(visited, 0, "an empty list must skip every column"),
                    1 => assert_eq!(visited, 1, "the last step has no dependents, even when listed twice"),
                    2 => assert!(visited > 1, "the first step must exercise propagation to dependents"),
                    5 => {
                        assert!(visited > 0, "unchanged listed columns must still be visited");
                        assert_eq!(oracle, 0, "the comparing refactor must see unchanged loads");
                    }
                    _ => {}
                }
                let mixed_visited = match step % 7 { 0 | 2 | 6 => visited, 1 | 4 => oracle, _ => n as u64 };
                assert_eq!(mixed.column_counts(), (before_mixed.0 + n as u64 - mixed_visited, before_mixed.1 + mixed_visited), "mixed calls must remember loads and reset flags");
                for lu in [&dirty, &mixed, &comparing, &all] {
                    assert_eq!(lu.counts(), (1, step as u64 + 2), "dominant pivots must stay usable");
                }
                let mut fresh = layout.lu::<S>();
                fresh.factor(&vals).expect("fresh reference factor");
                for rhs in [&b, &zeros] {
                    compare(&mut dirty, &mut all, rhs, &bits, step);
                    compare(&mut dirty, &mut fresh, rhs, &bits, step);
                    compare(&mut mixed, &mut all, rhs, &bits, step);
                    compare(&mut comparing, &mut all, rhs, &bits, step);
                }
            }
        }

        // The dense dispatcher factors for a dirty list, or when it holds no factor; an empty list
        // keeps the factor it holds, of the same values, so its solves match a fresh factor's.
        let (layout, mut vals) = assemble::<S>(2, &[(0, 0, 4.0), (1, 1, 3.0), (0, 1, 1.0)], true, |_, x| make(x, 0.0));
        let mut kept = layout.lu::<S>();
        kept.refactor_dirty(&vals, &[]).expect("dense first dirty call");
        assert_eq!(kept.counts(), (1, 0), "a dense LU without a factor factors on an empty dirty list");
        vals[1] = make(5.0, 0.0);
        kept.refactor_dirty(&vals, &[0, 0]).expect("dense dirty dispatch");
        kept.refactor_dirty(&vals, &[]).expect("dense empty dispatch");
        assert_eq!(kept.counts(), (2, 0), "a dense LU factors for a dirty list and keeps its factor for an empty one");
        let mut fresh = layout.lu::<S>();
        fresh.factor(&vals).expect("dense reference factor");
        compare(&mut kept, &mut fresh, &[make(1.0, 0.5), make(-0.0, -0.0)], &bits, 0);
    }

    check::<f64>(|re, _| re, |x| [x.to_bits(), 0]);
    check::<C64>(C64::new, |x| [x.re.to_bits(), x.im.to_bits()]);
}

/// The layout cutoff (`DENSE_MAX`): a diode-loaded RC ladder of 26 unknowns, a size the dense LU
/// took before the cutoff came down, runs on the sparse LU by default, and its transient matches
/// the dense run within ten times the solver's own band (`reltol * |v| + vntol`) at every
/// compared time, through the pulse's edges and the diodes' turn-on.
#[test]
fn t21_default_layout_transient_matches_dense() {
    use fab_elec_sim::*;
    let build = |dense: Option<bool>| {
        let mut c = SimCircuit::new();
        c.opts.force_dense = dense;
        c.model("dsig", ModelKind::Diode, &[("is", 2.5e-9), ("n", 1.75), ("rs", 0.6), ("cjo", 4e-12), ("tt", 6e-9)]);
        c.v("vin", "n0", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 0.5e-3, per: 1e-3 }));
        for k in 0..12 {
            let (a, b) = (format!("n{k}"), format!("n{}", k + 1));
            c.r(&format!("r{k}"), &a, &b, 1e3);
            c.c(&format!("c{k}"), &b, "0", 10e-9);
            c.d(&format!("d{k}"), &b, "0", "dsig");
        }
        c
    };
    let go = |c: &SimCircuit| {
        let mut r = Record::default();
        let s = run(c, &Analysis::Tran(TranSpec { tstop: 2e-3, max_step: Some(10e-6), uic: false }), &mut r, &Control::default()).unwrap_or_else(|d| panic!("{d}"));
        (r, s)
    };
    let dense = build(Some(true));
    let (rd, sd) = go(&dense);
    let (rs, ss) = go(&build(None));
    assert!(sd.dense && !ss.dense, "the default layout at {} unknowns is sparse (forced dense: {}, default dense: {})", ss.unknowns, sd.dense, ss.dense);
    assert!(ss.unknowns > 8 && ss.unknowns < 48, "a size between the old and the new cutoff: {}", ss.unknowns);
    let (reltol, vntol) = (dense.opts.reltol, dense.opts.vntol);
    for node in ["n1", "n6", "n12"] {
        for k in 1..=80 {
            let t = k as f64 * 2e-3 / 80.0;
            let (a, b) = (rd.at_time(node, t), rs.at_time(node, t));
            let band = 10.0 * (reltol * a.abs().max(b.abs()) + vntol);
            assert!((a - b).abs() <= band, "{node} at {t:e} s: dense {a} V, sparse {b} V, band {band} V");
        }
    }
}
