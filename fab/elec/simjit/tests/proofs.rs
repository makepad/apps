use fab_elec_sim::sparse::compiled::{CompiledLu, LuPlan, PlanCompiler, PlanJit, JIT_AFTER};
use fab_elec_sim::sparse::{lu::SparseLu, order::order, pattern::Pattern, Singular};
use fab_elec_sim::wave::{WaveCompiler as _, WavePlan};
use fab_elec_simjit::{Compiler, WaveCompiler};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

struct Lcg(u64);

impl Lcg {
    fn word(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 32) as u32
    }

    fn below(&mut self, end: usize) -> usize {
        ((u64::from(self.word()) * end as u64) >> 32) as usize
    }

    fn unit(&mut self) -> f64 {
        f64::from(self.word()) / 4294967296.0
    }

    fn coefficient(&mut self, row: u32, col: u32) -> f64 {
        if row == col { 4.0 + self.unit() } else { -0.125 - 0.25 * self.unit() }
    }
}

fn entries(n: usize, block_size: usize, rng: &mut Lcg) -> Vec<(u32, u32)> {
    let mut entries = Vec::new();
    for col in 0..n {
        let first = col / block_size * block_size;
        // The cycle makes the whole diagonal block strongly connected for every seed.
        let mut rows = vec![col, first + (col - first + 1) % block_size];
        let count = 3 + rng.below(4);
        while rows.len() < count {
            let row = first + rng.below(block_size);
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
        entries.extend(rows.into_iter().map(|r| (r as u32, col as u32)));
        if col == first && first > 0 {
            entries.push(((first - 1) as u32, col as u32));
        }
    }
    entries
}

#[test]
fn refactor_and_solve_match_the_interpreted_plan() {
    let mut rng = Lcg(0x6c75_706c_616e);
    for (n, block_size) in [(8, 8), (17, 17), (27, 27), (120, 6)] {
        let entries = entries(n, block_size, &mut rng);
        let (pat, map) = Pattern::from_entries(n, &entries);
        let ord = order(&pat);
        let first_col = ord.q[0];
        let diag = pat.find(first_col, first_col).expect("first column needs its diagonal") as usize;
        let mut vals = vec![0.0; pat.nnz() + 1];
        vals[0] = f64::NAN; // Fill loads must ignore the trash cell.
        for (&(row, col), &i) in entries.iter().zip(&map) {
            vals[i as usize] = rng.coefficient(row, col);
        }
        let pat = Arc::new(pat);
        let mut a = SparseLu::<f64>::new(pat.clone(), Arc::new(ord));
        a.factor(&vals).expect("the initial diagonally dominant matrix must factor");
        let plan = a.plan().expect("a full factor must produce a plan");
        assert_eq!(plan.starts.len() - 1, n / block_size, "n={n}: diagonal block count");
        if n > block_size {
            assert!(
                plan.u_off.iter().zip(&plan.u_ptr[1..]).any(|(off, end)| off < end),
                "n={n}: the block solve must update earlier right-hand sides"
            );
        }
        // The first column has no preceding updates. Its first L slot is a direct
        // matrix entry, so changing this one input can force a specific kept pivot out.
        let challenger = pat.find(plan.l_row[0], first_col)
            .expect("the first L entry must come from the first matrix column") as usize;
        let mut b = a.clone();
        assert!(b.compile_now(&Compiler), "n={n}: build nodes have native code generation");
        let stats = b.compiled_stats().expect("the compiled plan must be active");
        assert_eq!(stats.ops as usize, plan.ops(), "n={n}: compiled plan operation count");
        assert!(stats.code_bytes > 0, "n={n}: the plan must contain native instructions");

        let mut x = vec![0.0; n];
        let mut y = x.clone();
        for round in 0..200 {
            for _ in 0..1 + rng.below(n) {
                let e = rng.below(entries.len());
                let (row, col) = entries[e];
                vals[map[e] as usize] = rng.coefficient(row, col);
            }
            match round % 100 {
                45 => vals[challenger] = 32768.0,
                46 => vals[challenger] = -0.0,
                _ => {}
            }
            let before = a.factors;
            let interpreted = a.refactor(&vals);
            let compiled = b.refactor(&vals);
            assert_eq!(compiled, interpreted, "n={n} round={round}: refactor result");
            assert!(interpreted.is_ok(), "n={n} round={round}: perturbed matrix must remain solvable");
            assert_eq!(b.factors, a.factors, "n={n} round={round}: kept-pivot fallback");
            if matches!(round % 100, 45 | 46) {
                assert_eq!(a.factors, before + 1, "n={n} round={round}: forced change must reject the kept pivot");
            }
            if b.compiled_stats().is_none() {
                assert!(b.compile_now(&Compiler), "n={n} round={round}: build nodes must compile the replacement pivots");
            }

            for (row, value) in x.iter_mut().enumerate() {
                *value = if round % 16 == 0 {
                    if row % 2 == 0 { -0.0 } else { 0.0 }
                } else if row % 5 == 0 {
                    -0.0
                } else if rng.below(4) == 0 {
                    0.0
                } else {
                    rng.unit() - 0.5
                };
            }
            if round == 80 {
                x[first_col as usize] = f64::from_bits(0x7ff8_0000_0000_0123);
            }
            y.copy_from_slice(&x);
            a.solve(&mut x);
            b.solve(&mut y);
            for (col, (got, want)) in y.iter().zip(&x).enumerate() {
                assert_eq!(got.to_bits(), want.to_bits(), "n={n} round={round} col={col}: solution bits");
            }
        }
        assert_eq!(b.compiled_refactors, 200, "n={n}: every round must enter the compiled refactor");
        assert_eq!(b.compiled_solves, 200, "n={n}: every round must enter the compiled solve");

        // With |d| = m = infinity the magnitude and threshold comparisons both pass.
        // Only the finiteness test rejects this pivot, and full pivot search reports its column.
        vals[diag] = f64::INFINITY;
        let singular = Err(Singular { col: first_col });
        assert_eq!(a.refactor(&vals), singular, "n={n}: interpreted singular column");
        assert_eq!(b.refactor(&vals), singular, "n={n}: compiled singular column");
    }
}

#[test]
fn wave_runs_match_the_interpreted_plan() {
    let samples = 1000;
    let mut rng = Lcg(0x7761_7665);
    for (name, plan) in [
        ("coupled ports", WavePlan {
            ports: 2, inputs: 2, outputs: 3,
            rows: vec![(0, 4), (4, 7), (7, 10), (10, 10), (10, 11)],
            terms: vec![3, 0, 2, 1, 0, 3, 1, 3, 1, 0, 2],
        }),
        ("no inputs", WavePlan {
            ports: 1, inputs: 0, outputs: 1,
            rows: vec![(0, 1), (1, 2)], terms: vec![0, 0],
        }),
        ("no outputs", WavePlan {
            ports: 2, inputs: 1, outputs: 0,
            rows: vec![(0, 0), (0, 3)], terms: vec![0, 2, 1],
        }),
    ] {
        let compiled = WaveCompiler.compile(&plan).unwrap_or_else(|| panic!("plan={name}: native compile"));
        let (ports, inputs, outputs) = (plan.ports as usize, plan.inputs as usize, plan.outputs as usize);
        let width = f64::from(plan.ports + plan.inputs);
        let consts: Vec<_> = (0..plan.ops()).map(|_| (0.5 + 0.5 * rng.unit()) / width).collect();
        let initial: Vec<_> = (0..ports).map(|_| rng.unit() - 0.5).collect();
        let mut u: Vec<_> = (0..samples * inputs).map(|_| rng.unit() - 0.5).collect();
        // The single-input output keeps the sign of its first product.
        u[..inputs].fill(-0.0);
        u[inputs..2 * inputs].fill(0.0);

        for chunk in [samples, 7] {
            let (mut want_a, mut got_a) = (initial.clone(), initial.clone());
            let mut want_b = vec![0.0; ports];
            let mut got_b = vec![f64::NAN; ports];
            let mut want_y = vec![0.0; samples * outputs];
            let mut got_y = vec![f64::NAN; samples * outputs];
            for start in (0..samples).step_by(chunk) {
                let end = (start + chunk).min(samples);
                let us = &u[start * inputs..end * inputs];
                let ys = start * outputs..end * outputs;
                plan.run(&consts, &mut want_a, us, &mut want_y[ys.clone()], &mut want_b, end - start);
                compiled.run(&consts, &mut got_a, us, &mut got_y[ys], &mut got_b, end - start);
                let last = end - 1;
                for k in 0..ports {
                    assert_eq!(got_a[k].to_bits(), want_a[k].to_bits(), "plan={name} sample={last} chunk={chunk}: a[{k}]");
                }
            }
            for s in 0..samples {
                for k in 0..outputs {
                    let i = s * outputs + k;
                    assert_eq!(got_y[i].to_bits(), want_y[i].to_bits(), "plan={name} sample={s} chunk={chunk}: y[{k}]");
                }
            }
        }
    }
}

/// Counts the plans it compiles.
struct Counting(Arc<AtomicU32>);

impl PlanCompiler for Counting {
    fn compile(&self, plan: &LuPlan) -> Option<Arc<dyn CompiledLu>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Compiler.compile(plan)
    }
}

/// Code compiled for one LU serves every LU of the same plan (pattern, ordering and pivot
/// sequence) through the compile facility's `CodeCache`: a new LU takes it at its first factor,
/// with no compile of its own and the interpreted plan's bits, as a block cut again in a shape it
/// had does; an LU whose values pivot it elsewhere finds none.
#[test]
fn an_lu_of_a_compiled_plan_takes_its_code_at_its_first_factor() {
    let mut rng = Lcg(0x6361_6368_6564);
    let n = 27;
    let entries = entries(n, n, &mut rng);
    let (pat, map) = Pattern::from_entries(n, &entries);
    let ord = Arc::new(order(&pat));
    let first_col = ord.q[0];
    let mut vals = vec![0.0; pat.nnz() + 1];
    for (&(row, col), &i) in entries.iter().zip(&map) {
        vals[i as usize] = rng.coefficient(row, col);
    }
    let pat = Arc::new(pat);
    let compiles = Arc::new(AtomicU32::new(0));
    let jit = PlanJit { post: Arc::new(|job| job()), compiler: Arc::new(Counting(compiles.clone())), codes: Arc::default() };
    let lu = |jit: Option<&PlanJit>| {
        let mut lu = SparseLu::<f64>::new(pat.clone(), ord.clone());
        lu.set_jit(jit.cloned());
        lu
    };
    let mut a = lu(Some(&jit));
    a.factor(&vals).expect("the diagonally dominant matrix factors");
    for _ in 0..=JIT_AFTER {
        a.refactor(&vals).expect("it refactors");
        a.adopt();
    }
    a.adopt();
    assert!(a.compiled_stats().is_some(), "the first LU compiled its plan after {JIT_AFTER} refactors");
    assert_eq!((compiles.load(Ordering::Relaxed), jit.codes.len()), (1, 1), "one compile, kept by the facility");

    let mut b = lu(Some(&jit));
    b.factor(&vals).expect("the same matrix factors");
    assert!(b.compiled_stats().is_some() && b.compiles_asked == 0, "a new LU of the same plan runs its code from its first factor, asking no compile");
    let mut c = lu(None);
    c.factor(&vals).expect("the same matrix factors");
    for k in 0..4 {
        for _ in 0..1 + rng.below(n) {
            let e = rng.below(entries.len());
            let (row, col) = entries[e];
            vals[map[e] as usize] = rng.coefficient(row, col);
        }
        assert_eq!(b.refactor(&vals), c.refactor(&vals), "refactor {k}: the cached code and the interpreted plan agree");
        let x: Vec<f64> = (0..n).map(|_| rng.unit() - 0.5).collect();
        let (mut got, mut want) = (x.clone(), x);
        b.solve(&mut got);
        c.solve(&mut want);
        for (col, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g.to_bits(), w.to_bits(), "refactor {k} col {col}: the cached code's solution is the interpreted plan's");
        }
    }
    assert_eq!(b.compiled_refactors, 4, "every refactor ran the cached code");

    // the first column's first L entry far over its diagonal moves the first pivot
    let challenger = pat.find(b.plan().expect("factored").l_row[0], first_col).expect("the first L entry is a matrix entry") as usize;
    vals[challenger] = 32768.0;
    let mut d = lu(Some(&jit));
    d.factor(&vals).expect("the challenged matrix factors");
    assert!(d.compiled_stats().is_none(), "another pivot sequence finds no code");
    assert_eq!(compiles.load(Ordering::Relaxed), 1, "and none was compiled");
}
